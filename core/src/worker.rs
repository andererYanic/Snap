//! Offline-Warteschlange: Hybrid-Worker (Notify + Backoff pro Peer).

use crate::api::transfer::finalize_output;
use crate::daemon::*;
use crate::database::{now_unix, PendingJob};
use crate::network::FetchError;
use anyhow::Result;
use iroh_blobs::ticket::BlobTicket;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

// ----------------------------------------------------------------------
// Offline-Warteschlange (Hybrid: Notify + Backoff pro Peer)
// ----------------------------------------------------------------------

/// Hybrid-Worker:
/// - Warteschlange leer → schläft unbegrenzt bis `notify_one()`.
/// - Aufträge vorhanden → schläft bis zum nächsten fälligen Versuch (Backoff des
///   Peers mit Jitter) oder bis `notify_one()`, je nachdem was zuerst eintritt.
///
/// Es werden keine Pings gesendet: Ein Versuch ist immer ein echter Download.
pub(crate) async fn run_pending_pin_worker(inner: Arc<DaemonInner>) {
    loop {
        let processed = process_due_jobs(&inner).await;

        let next = match inner.db.next_wakeup() {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("Warteschlange nicht lesbar: {e:#}");
                Some(now_unix() + 60)
            }
        };
        match next {
            None => inner.sync_notifier.notified().await,
            Some(at) => {
                let mut secs = (at - now_unix()).max(0) as u64;
                if secs == 0 && processed {
                    // Schutz vor Endlosschleifen bei unerwarteten Zuständen.
                    secs = 1;
                }
                tokio::select! {
                    _ = inner.sync_notifier.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
                }
            }
        }
    }
}

/// Arbeitet alle fälligen Aufträge ab. Ist ein Peer nicht erreichbar, werden
/// seine übrigen Aufträge in dieser Runde übersprungen.
/// Gleichzeitig bearbeitete Aufträge.
const PARALLEL_JOBS: usize = 8;

pub(crate) async fn process_due_jobs(inner: &Arc<DaemonInner>) -> bool {
    let jobs = match inner.db.due_jobs(now_unix()) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("Fällige Aufträge nicht lesbar: {e:#}");
            return false;
        }
    };
    let processed = !jobs.is_empty();
    let unreachable: Arc<std::sync::Mutex<HashSet<String>>> = Arc::default();
    let sem = Arc::new(tokio::sync::Semaphore::new(PARALLEL_JOBS));
    let mut set = tokio::task::JoinSet::new();
    for job in jobs {
        let Ok(permit) = sem.clone().acquire_owned().await else { break };
        if unreachable.lock().unwrap().contains(&job.peer_id) {
            continue;
        }
        let (inner, unreachable) = (inner.clone(), unreachable.clone());
        set.spawn(async move {
            let _permit = permit;
            if let Err(e) = process_job(&inner, &job, &unreachable).await {
                tracing::error!("Auftrag {} fehlgeschlagen: {e:#}", job.id);
            }
        });
    }
    while set.join_next().await.is_some() {}
    processed
}

pub(crate) async fn process_job(
    inner: &DaemonInner,
    job: &PendingJob,
    unreachable: &std::sync::Mutex<HashSet<String>>,
) -> Result<()> {
    let ticket: BlobTicket = match job.ticket.parse() {
        Ok(t) => t,
        Err(e) => {
            inner.db.job_failed(job.id, &format!("Ungültiges Ticket: {e}"))?;
            return Ok(());
        }
    };
    let (fetched, _source) = crate::api::transfer::fetch_with_fallback(inner, &ticket).await;
    match fetched {
        Ok(temp) => {
            let output = PathBuf::from(&job.output_path);
            let is_pin = job.action == "PIN";
            if is_pin && !crate::pins::may_write_pin(inner, &output) {
                // Seit dem Einreihen lokal verändert (oder durch einen Link ersetzt): nie überschreiben.
                inner.db.pinned_mark_conflict(&job.output_path)?;
                inner.db.job_failed(job.id, "Konflikt: Die lokale Kopie wurde verändert – nicht überschrieben")?;
                return Ok(());
            }
            let res = finalize_output(inner, ticket.hash(), &output, job.explicit_output, job.force).await;
            drop(temp);
            match res {
                Ok(path) => {
                    if is_pin {
                        crate::pins::record_written(inner, &path, &job.target_hash)?;
                    }
                    inner.db.complete_job(job.id)?;
                    tracing::info!("✅ Hintergrund-Download abgeschlossen: {}", path.display());
                }
                Err(e) => {
                    tracing::warn!("Auftrag {}: {e:#}", job.id);
                    inner.db.job_failed(job.id, &format!("{e:#}"))?;
                }
            }
        }
        Err(e) if e.is_retryable() => {
            let next = inner.db.mark_peer_unreachable(&job.peer_id, true)?;
            inner.db.job_attempt_failed(job.id, &e.to_string())?;
            unreachable.lock().unwrap().insert(job.peer_id.clone());
            tracing::info!(
                "Peer {} weiterhin nicht erreichbar – nächster Versuch in {} s",
                short(&job.peer_id),
                (next - now_unix()).max(0)
            );
        }
        Err(e) => {
            if matches!(e, FetchError::Rejected(_)) {
                inner.db.mark_peer_online(&job.peer_id)?;
            }
            tracing::warn!("Auftrag {} endgültig fehlgeschlagen: {e}", job.id);
            inner.db.job_failed(job.id, &e.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn short(s: &str) -> &str {
    &s[..s.len().min(10)]
}
