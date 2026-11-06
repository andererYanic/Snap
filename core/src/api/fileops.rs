//! Lokale Dateioperationen als Aufträge (Fortschritt und Ende als Ereignisse).

use crate::api_bail;
use crate::daemon::*;
use crate::events::Event;
use crate::fileops::jobs::{FileOp, Outcome};
use crate::fileops::ops::Progress;
use crate::ipc::Response;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// Abstand der Fortschrittsereignisse.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

pub(crate) async fn file_op(inner: &Arc<DaemonInner>, op: FileOp, wait: bool) -> Result<Response> {
    let (job, progress) = inner.files.register();
    let handle = tokio::spawn(run_job(inner.clone(), job, progress, op));
    if wait {
        let outcome = handle.await??;
        Ok(Response::FileOpDone { job, outcome })
    } else {
        Ok(Response::FileOpStarted { job })
    }
}

/// Führt einen Auftrag aus, meldet Fortschritt und aktualisiert danach den Suchindex.
pub(crate) async fn run_job(inner: Arc<DaemonInner>, job: u64, p: Arc<Progress>, op: FileOp) -> Result<Outcome> {
    let files = inner.files.clone();
    let (p2, op2) = (p.clone(), op.clone());
    let mut task = tokio::task::spawn_blocking(move || files.run(&op2, &p2));
    let mut tick = tokio::time::interval(PROGRESS_EVERY);
    tick.tick().await;
    let res = loop {
        tokio::select! {
            r = &mut task => break r,
            _ = tick.tick() => inner.db.events().emit(Event::FileOpProgress {
                job,
                done_files: p.done_files.load(Ordering::Relaxed),
                total_files: p.total_files.load(Ordering::Relaxed),
                done_bytes: p.done_bytes.load(Ordering::Relaxed),
                total_bytes: p.total_bytes.load(Ordering::Relaxed),
            }),
        }
    };
    inner.files.unregister(job);
    let res = res.map_err(anyhow::Error::from).and_then(|r| r);
    match &res {
        Ok(out) => {
            inner.refresh_index(out.affected_dirs(&op)).await;
            inner.db.events().emit(Event::FileOpFinished { job, ok: true, message: op.label() });
        }
        Err(e) => {
            inner.db.events().emit(Event::FileOpFinished { job, ok: false, message: format!("{e:#}") });
        }
    }
    res
}

pub(crate) fn cancel(inner: &DaemonInner, job: u64) -> Result<Response> {
    if !inner.files.cancel(job) {
        api_bail!(NotFound, "Kein laufender Auftrag mit Nummer {job}.");
    }
    Ok(Response::Success { message: format!("Auftrag {job} wird abgebrochen.") })
}

pub(crate) async fn undo(inner: &Arc<DaemonInner>) -> Result<Response> {
    let files = inner.files.clone();
    let (label, dirs) = tokio::task::spawn_blocking(move || files.undo()).await??;
    inner.refresh_index(dirs).await;
    Ok(Response::Success { message: format!("Rückgängig gemacht: {label}") })
}

pub(crate) async fn trash_list(inner: &Arc<DaemonInner>) -> Result<Response> {
    let files = inner.files.clone();
    let entries = tokio::task::spawn_blocking(move || files.trash.list()).await?;
    Ok(Response::Trash { entries })
}

pub(crate) async fn trash_restore(inner: &Arc<DaemonInner>, id: String, keep_both: bool) -> Result<Response> {
    let files = inner.files.clone();
    let restored = tokio::task::spawn_blocking(move || files.trash.restore(&id, keep_both)).await??;
    inner.refresh_index(restored.parent().map(Path::to_path_buf).into_iter().collect::<Vec<PathBuf>>()).await;
    Ok(Response::Success { message: format!("Wiederhergestellt: {}", restored.display()) })
}

pub(crate) async fn trash_purge(inner: &Arc<DaemonInner>, id: String) -> Result<Response> {
    let files = inner.files.clone();
    tokio::task::spawn_blocking(move || files.trash.purge(&id)).await??;
    Ok(Response::Success { message: "Endgültig gelöscht.".into() })
}

pub(crate) async fn trash_empty(inner: &Arc<DaemonInner>) -> Result<Response> {
    let files = inner.files.clone();
    let n = tokio::task::spawn_blocking(move || files.trash.empty()).await??;
    Ok(Response::Success { message: format!("Papierkorb geleert ({n} Einträge).") })
}

pub(crate) fn ops_pending(inner: &DaemonInner) -> Result<Response> {
    let devices = inner.db.list_devices()?;
    let items = inner
        .db
        .pending_approvals()?
        .into_iter()
        .map(|(id, node_id, summary, created_at)| crate::ipc::ApprovalInfo {
            id,
            device_name: devices.iter().find(|d| d.node_id == node_id).map(|d| d.name.clone()),
            node_id,
            summary,
            created_at,
        })
        .collect();
    Ok(Response::Approvals { items })
}

pub(crate) fn upload_rule_add(
    inner: &DaemonInner,
    path: &Path,
    device: &str,
    share: &str,
    dest_dir: &str,
) -> Result<Response> {
    let real = match std::fs::canonicalize(path) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => api_bail!(InvalidInput, "{} ist kein Ordner.", path.display()),
        Err(_) => api_bail!(NotFound, "{} existiert nicht.", path.display()),
    };
    if inner.protected_paths().iter().any(|p| real.starts_with(p)) {
        api_bail!(InvalidInput, "Die Snap-Daten können nicht hochgeladen werden.");
    }
    let dest_dir = dest_dir.trim_matches('/');
    crate::remote_ops::RemoteOp::Mkdir { share: share.into(), parent: dest_dir.into(), name: "x".into() }.validate()?;
    let mut found = inner.db.find_devices(device)?;
    let dev = match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'."),
        1 => found.remove(0),
        _ => api_bail!(Ambiguous, "'{device}' passt auf mehrere Geräte."),
    };
    let local = real.to_str().ok_or_else(|| {
        crate::api::error::ApiError::new(crate::api::error::ErrorCode::InvalidInput, "Pfad ist kein UTF-8")
    })?;
    inner.db.add_upload_rule(local, &dev.node_id, share, dest_dir)?;
    inner.rules_notify.notify_one();
    Ok(Response::UploadRules { rules: inner.db.upload_rules()? })
}
