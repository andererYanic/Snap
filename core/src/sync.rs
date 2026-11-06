//! Ledger-Abgleich mit gekoppelten Geräten: auf Anfrage, im Hintergrund und periodisch.

use crate::daemon::*;
use crate::database::{now_unix, DatabaseManager};
use crate::ipc::SyncResult;
use crate::ledger::{LedgerCtx, SyncError};
use iroh::EndpointId;
use std::sync::Arc;
use std::time::Duration;

/// Gleicht mit allen gekoppelten Geräten parallel ab.
pub(crate) async fn sync_all(inner: &DaemonInner) -> Vec<SyncResult> {
    let _guard = inner.sync_lock.lock().await;
    let devices = match inner.db.list_devices() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Geräteliste nicht lesbar: {e:#}");
            return Vec::new();
        }
    };
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    let mut set = tokio::task::JoinSet::new();
    for d in devices {
        let ctx = inner.ledger.clone();
        let db = inner.db.clone();
        set.spawn(async move { sync_one(&ctx, &db, d.node_id, d.name, timeout).await });
    }
    let mut results = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(r) = r {
            results.push(r);
        }
    }
    results.sort_by(|a, b| a.name.cmp(&b.name));
    results
}

pub(crate) async fn sync_one(
    ctx: &LedgerCtx,
    db: &DatabaseManager,
    node_id: String,
    name: String,
    timeout: Duration,
) -> SyncResult {
    let mut result = SyncResult { node_id: node_id.clone(), name, ok: false, new_entries: 0, error: None };
    let peer: EndpointId = match node_id.parse() {
        Ok(p) => p,
        Err(e) => {
            result.error = Some(format!("Ungültige Geräte-ID: {e}"));
            return result;
        }
    };
    match crate::ledger::sync_with(ctx, peer, timeout).await {
        Ok(stats) => {
            result.ok = true;
            result.new_entries = stats.new_entries;
            if let Some(n) = stats.device_name {
                result.name = n;
            }
        }
        Err(e) => {
            if matches!(e, SyncError::Unreachable(_)) {
                let _ = db.mark_peer_unreachable(&node_id, false);
            }
            result.error = Some(e.to_string());
        }
    }
    result
}

/// Startet einen Hintergrund-Abgleich, sofern keiner läuft. Geräte, die gerade im
/// Backoff sind (zuletzt nicht erreichbar), werden übersprungen – sonst würde jeder
/// `list`-Aufruf erneut bis zum Timeout auf sie warten.
pub(crate) fn spawn_sync_all(inner: &DaemonInner) {
    let Ok(guard) = inner.sync_lock.clone().try_lock_owned() else {
        return; // läuft bereits
    };
    let ctx = inner.ledger.clone();
    let db = inner.db.clone();
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    tokio::spawn(async move {
        let _guard = guard;
        let now = now_unix();
        let mut set = tokio::task::JoinSet::new();
        for d in db.list_devices().unwrap_or_default() {
            let in_backoff =
                db.get_peer(&d.node_id).ok().flatten().and_then(|p| p.next_retry_at).is_some_and(|t| t > now);
            if in_backoff {
                continue;
            }
            let (ctx, db) = (ctx.clone(), db.clone());
            set.spawn(async move { sync_one(&ctx, &db, d.node_id, d.name, timeout).await });
        }
        while let Some(Ok(r)) = set.join_next().await {
            if r.new_entries > 0 {
                tracing::info!("Ledger: {} neue Einträge von '{}'", r.new_entries, r.name);
            }
        }
    });
}

pub(crate) fn spawn_sync_with(inner: &DaemonInner, node_id: &str) {
    let ctx = inner.ledger.clone();
    let db = inner.db.clone();
    let node_id = node_id.to_string();
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    tokio::spawn(async move {
        let r = sync_one(&ctx, &db, node_id, String::new(), timeout).await;
        if let Some(e) = r.error {
            tracing::warn!("Erster Abgleich nach dem Koppeln fehlgeschlagen: {e}");
        }
    });
}

/// Periodischer Abgleich mit Zufallsstreuung (±30 %). Der erste Lauf erfolgt
/// nicht beim Start, sondern nach einer zufälligen Wartezeit – so entsteht kein
/// erkennbares Startmuster.
pub(crate) async fn run_periodic_sync(inner: Arc<DaemonInner>) {
    let hours = inner.config.sync_interval_hours;
    if hours <= 0.0 {
        return;
    }
    loop {
        let factor = 0.7 + fastrand::f64() * 0.6;
        tokio::time::sleep(Duration::from_secs_f64(hours * 3600.0 * factor)).await;
        if inner.db.list_devices().map(|d| d.is_empty()).unwrap_or(true) {
            continue;
        }
        let results = sync_all(&inner).await;
        let ok = results.iter().filter(|r| r.ok).count();
        tracing::info!("Periodischer Abgleich: {ok}/{} Gerät(e) erreicht", results.len());
    }
}
