//! Hochladen auf gekoppelte Geräte und Kamera-Upload-Regeln.
//!
//! Ablauf: Der Auftraggeber nimmt die Datei in seinen Blob-Store auf und bietet ihren Hash
//! **genau dem Zielgerät** an. Über `snap/ops/1` (`Import`) bittet er das Ziel, sie zu holen.
//! Das Ziel prüft Rechte (`files.write`), Tagesgrenze und freien Platz, lädt den Inhalt
//! BLAKE3-verifiziert und legt ihn ab – nie überschreibend.
//!
//! Regeln beobachten lokale Ordner (z. B. die Kamera) und planen neue Dateien automatisch ein;
//! die Warteschlange holt Offline-Phasen nach und lädt keine Datei doppelt.

use crate::api::error::{code_of, ApiError, ErrorCode};
use crate::api_bail;
use crate::daemon::DaemonInner;
use crate::database::{NewUpload, UploadRow};
use crate::fileops::ops::{rename_noreplace, unique_path, Conflict};
use crate::remote_ops::RemoteOp;
use anyhow::{Context, Result};
use iroh::EndpointId;
use iroh_blobs::Hash;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// So lange gilt ein Angebot (wird bei jedem Versuch erneuert).
const OFFER_TTL: Duration = Duration::from_secs(2 * 3600);
/// Dateien, die gerade noch geschrieben werden, abwarten.
const SETTLE: Duration = Duration::from_secs(3);
const RULE_RESCAN: Duration = Duration::from_secs(600);

/// Angebote: Welcher Hash darf von welchem Gerät abgeholt werden (zusätzlich zur normalen
/// Rechteprüfung des Blob-Zugriffs).
#[derive(Default)]
pub struct UploadOffers {
    map: Mutex<HashMap<(Hash, EndpointId), Instant>>,
}

impl UploadOffers {
    pub fn offer(&self, hash: Hash, node: EndpointId) {
        let mut m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, t| t.elapsed() < OFFER_TTL);
        m.insert((hash, node), Instant::now());
    }

    pub fn withdraw(&self, hash: Hash, node: EndpointId) {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(&(hash, node));
    }

    pub fn allows(&self, hash: &Hash, node: Option<EndpointId>) -> bool {
        let Some(node) = node else { return false };
        let m = self.map.lock().unwrap_or_else(|e| e.into_inner());
        m.get(&(*hash, node)).is_some_and(|t| t.elapsed() < OFFER_TTL)
    }
}

/// Freier Platz (Bytes) auf dem Laufwerk von `path`.
fn free_space(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: statvfs ist ein reines C-Struct; Nullen sind ein gültiger Wert.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: gültiger, NUL-terminierter Pfad; `st` wird vollständig von statvfs beschrieben.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

/// Empfängt eine hochgeladene Datei (Zielseite). Rechte wurden bereits geprüft.
/// Gibt den endgültigen Pfad zurück.
pub(crate) async fn receive(
    inner: &Arc<DaemonInner>,
    peer: &str,
    dest: &Path,
    name: &str,
    hash: Hash,
    declared: u64,
    conflict: Conflict,
) -> Result<PathBuf> {
    let daily = inner.settings.upload_daily_bytes();
    if daily == 0 {
        api_bail!(PermissionDenied, "Hochladen ist auf diesem Gerät gesperrt (upload_daily_gb = 0).");
    }
    let peer_id: EndpointId = peer.parse().context("Ungültige Node-ID")?;
    let hints = inner.db.addr_hints(peer).unwrap_or_default();
    let addr = crate::network::endpoint_addr(peer_id, &hints);
    // Größe ist erst nach dem Nachweis (BLAKE3/bao) verlässlich.
    let size = inner
        .network
        .stream_size(&addr, hash)
        .await
        .map_err(|e| ApiError::new(ErrorCode::Unavailable, format!("Datei nicht abrufbar: {e}")))?;
    if size != declared {
        api_bail!(InvalidInput, "Angegebene Größe stimmt nicht mit dem Inhalt überein.");
    }
    // Zielordner (innerhalb der Freigabe) anlegen.
    tokio::fs::create_dir_all(dest).await.with_context(|| format!("{} nicht anlegbar", dest.display()))?;
    let reserve = inner.settings.upload_reserve_bytes();
    let free = free_space(dest)?;
    if free < size.saturating_add(reserve) {
        api_bail!(
            Unavailable,
            "Nicht genug Platz: {:.1} GB frei, {:.1} GB nötig (inkl. Reserve).",
            free as f64 / 1e9,
            (size + reserve) as f64 / 1e9
        );
    }
    if !inner.db.reserve_upload_bytes(peer, size, daily)? {
        api_bail!(Unavailable, "Tagesgrenze für Uploads dieses Geräts erreicht (upload_daily_gb).");
    }
    let result = async {
        let ticket = iroh_blobs::ticket::BlobTicket::new(addr, hash, iroh_blobs::BlobFormat::Raw);
        let temp = inner
            .network
            .fetch(&ticket)
            .await
            .map_err(|e| ApiError::new(ErrorCode::Unavailable, format!("Übertragung fehlgeschlagen: {e}")))?;
        let mut target = dest.join(name);
        if std::fs::symlink_metadata(&target).is_ok() {
            match conflict {
                Conflict::Fail => api_bail!(AlreadyExists, "{} existiert bereits.", target.display()),
                Conflict::Skip => return Ok(target),
                Conflict::KeepBoth => target = unique_path(&target),
                Conflict::Replace => {
                    inner.files.trash.trash(&target)?;
                }
            }
        }
        // Unter temporärem Namen ablegen, dann ohne Überschreiben umbenennen.
        let tmp = dest.join(format!(".{name}.snap-upload"));
        let _ = std::fs::remove_file(&tmp);
        inner.network.export(hash, &tmp).await?;
        drop(temp);
        // Hochgeladene Dateien sind nie ausführbar.
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))?;
        let mut placed = rename_noreplace(&tmp, &target);
        if matches!(&placed, Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists) {
            // Inzwischen belegt (Wettlauf): freien Namen wählen.
            target = unique_path(&target);
            placed = rename_noreplace(&tmp, &target);
        }
        if let Err(e) = placed {
            let _ = std::fs::remove_file(&tmp);
            return Err(e).context("Ablegen fehlgeschlagen");
        }
        anyhow::Ok(target)
    }
    .await;
    match result {
        Ok(target) => {
            inner.refresh_index(vec![dest.to_path_buf()]).await;
            let _ = inner.db.audit(
                peer,
                "ops.import",
                Some(&target.to_string_lossy()),
                true,
                Some(&format!("{size} Bytes")),
            );
            Ok(target)
        }
        Err(e) => {
            let _ = inner.db.release_upload_bytes(peer, size);
            Err(e)
        }
    }
}

/// Plant ein Hochladen ein (Auftraggeberseite).
pub(crate) fn enqueue(
    inner: &DaemonInner,
    device: &str,
    share: &str,
    dest_dir: &str,
    path: &Path,
    rule_id: Option<i64>,
) -> Result<i64> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => api_bail!(InvalidInput, "{} ist keine normale Datei.", path.display()),
        Err(_) => api_bail!(NotFound, "{} existiert nicht.", path.display()),
    }
    let mut found = inner.db.find_devices(device)?;
    let dev = match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'."),
        1 => found.remove(0),
        _ => api_bail!(Ambiguous, "'{device}' passt auf mehrere Geräte."),
    };
    // Früh prüfen, was das Ziel ohnehin prüfen wird.
    let name = path.file_name().and_then(|n| n.to_str()).context("Dateiname ist kein gültiges UTF-8")?;
    RemoteOp::Import {
        share: share.into(),
        dest_dir: dest_dir.into(),
        name: name.into(),
        hash: Hash::EMPTY.to_string(),
        size: 0,
        conflict: Conflict::KeepBoth,
    }
    .validate()?;
    let id = inner.db.add_upload(&NewUpload {
        device: dev.node_id,
        share: share.into(),
        dest_dir: dest_dir.into(),
        local_path: path.to_string_lossy().into_owned(),
        rule_id,
    })?;
    inner.upload_notify.notify_one();
    Ok(id)
}

/// Führt einen Auftrag aus. Ergebnis: Pfad auf dem Ziel.
async fn process(inner: &Arc<DaemonInner>, row: &UploadRow) -> Result<String> {
    let path = PathBuf::from(&row.local_path);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) if m.is_file() => m,
        _ => api_bail!(NotFound, "Lokale Datei {} gibt es nicht mehr.", path.display()),
    };
    let name = path.file_name().and_then(|n| n.to_str()).context("Dateiname")?.to_string();
    let node: EndpointId = row.device.parse().context("Ungültige Node-ID")?;
    let hash = inner.network.import_for_upload(&path).await?;
    inner.offers.offer(hash, node);
    let op = RemoteOp::Import {
        share: row.share.clone(),
        dest_dir: row.dest_dir.clone(),
        name,
        hash: hash.to_string(),
        size: meta.len(),
        conflict: Conflict::KeepBoth,
    };
    // Zeit fürs Übertragen: 2 min Grundzeit + 1 s je MB (mindestens ~8 Mbit/s).
    let timeout = Duration::from_secs(120 + meta.len() / 1_000_000);
    let res = crate::remote_ops::send_op(inner, &row.device, op, timeout).await;
    inner.offers.withdraw(hash, node);
    let _ = inner.network.untag_upload(hash).await;
    let (_, reply) = res?;
    if reply.ok {
        Ok(reply.result.unwrap_or_default())
    } else {
        Err(ApiError::new(reply.code.unwrap_or(ErrorCode::Internal), reply.message).into())
    }
}

/// Wartezeit bis zum nächsten Versuch: 30 s, 2 min, 10 min, dann stündlich.
fn backoff(attempts: i64) -> i64 {
    match attempts {
        0 => 30,
        1 => 120,
        2 => 600,
        _ => 3600,
    }
}

/// Gleichzeitige Uploads.
const PARALLEL: usize = 4;

/// Hintergrund: Warteschlange abarbeiten (bis zu `PARALLEL` gleichzeitig).
pub(crate) async fn run_worker(inner: Arc<DaemonInner>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(PARALLEL));
    let mut running: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel::<i64>();
    loop {
        while let Ok(id) = done_rx.try_recv() {
            running.remove(&id);
        }
        let due: Vec<UploadRow> = inner
            .db
            .due_uploads((PARALLEL * 4) as u32)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !running.contains(&r.id))
            .collect();
        let mut started = 0;
        for row in due {
            let Ok(permit) = slots.clone().try_acquire_owned() else { break };
            running.insert(row.id);
            started += 1;
            let (inner, done_tx) = (inner.clone(), done_tx.clone());
            tokio::spawn(async move {
                let _permit = permit;
                let outcome = process(&inner, &row).await;
                let (state, msg) = match outcome {
                    Ok(result) => {
                        let _ = inner.db.finish_upload(row.id, &result);
                        ("done", result)
                    }
                    Err(e) => {
                        let msg = format!("{e:#}");
                        // Vorübergehend (Gerät aus, Grenze, Platz) → später; sonst endgültig.
                        let retry =
                            matches!(code_of(&e), ErrorCode::Unavailable | ErrorCode::Timeout | ErrorCode::Internal);
                        let _ = inner.db.fail_upload(row.id, &msg, retry.then(|| backoff(row.attempts)));
                        (if retry { "pending" } else { "failed" }, msg)
                    }
                };
                inner.db.events().emit(crate::events::Event::UploadChanged {
                    id: row.id,
                    state: state.into(),
                    message: msg,
                });
                let _ = done_tx.send(row.id);
                inner.upload_notify.notify_one();
            });
        }
        if started > 0 && slots.available_permits() > 0 {
            continue; // Noch Plätze frei und eben etwas gestartet: sofort weitere holen.
        }
        tokio::select! {
            _ = inner.upload_notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
    }
}

/// Plant alle neuen oder geänderten Dateien eines Regel-Ordners ein (blockierend).
fn scan_rule(inner: &DaemonInner, rule: &crate::database::UploadRule) -> Result<usize> {
    let root = PathBuf::from(&rule.local_dir);
    let mut queued = 0;
    for entry in walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !crate::shares::is_ignored(&e.file_name().to_string_lossy()))
    {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.starts_with('.') || crate::shares::is_ignored(&name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        // Datei wird vielleicht noch geschrieben (Kamera) → beim nächsten Durchlauf.
        if meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < SETTLE) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(&root) else { continue };
        let Some(rel_str) = rel.to_str() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if !inner.db.mark_upload_seen(rule.id, rel_str, meta.len(), mtime)? {
            continue;
        }
        // Unterordner beibehalten: Kamera/2024/a.jpg → <dest_dir>/2024/a.jpg
        let sub = rel.parent().and_then(|p| p.to_str()).unwrap_or("");
        let dest = match (rule.dest_dir.as_str(), sub) {
            (d, "") => d.to_string(),
            ("", s) => s.to_string(),
            (d, s) => format!("{d}/{s}"),
        };
        match enqueue(inner, &rule.device, &rule.share, &dest, entry.path(), Some(rule.id)) {
            Ok(_) => queued += 1,
            Err(e) => tracing::debug!("{} nicht eingeplant: {e:#}", entry.path().display()),
        }
    }
    Ok(queued)
}

/// Startet Beobachtung und regelmäßigen Abgleich aller Regeln.
pub(crate) async fn run_rules(inner: Arc<DaemonInner>) {
    loop {
        let rules = inner.db.upload_rules().unwrap_or_default();
        // Watcher für Regeln ohne Watcher anlegen (Ereignisse wecken den nächsten Durchlauf).
        {
            let mut watchers = inner.upload_watchers.lock().unwrap_or_else(|e| e.into_inner());
            watchers.retain(|id, _| rules.iter().any(|r| r.id == *id));
            for rule in &rules {
                if watchers.contains_key(&rule.id) {
                    continue;
                }
                let notify = inner.rules_notify.clone();
                let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(ev) = res {
                        if !matches!(ev.kind, notify::EventKind::Access(_)) {
                            notify.notify_one();
                        }
                    }
                });
                if let Ok(mut w) = w {
                    use notify::Watcher;
                    if w.watch(Path::new(&rule.local_dir), notify::RecursiveMode::Recursive).is_ok() {
                        watchers.insert(rule.id, w);
                    }
                }
            }
        }
        let inner2 = inner.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for rule in rules {
                match scan_rule(&inner2, &rule) {
                    Ok(n) if n > 0 => tracing::info!("Upload-Regel {}: {n} neue Datei(en) eingeplant", rule.id),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("Upload-Regel {} fehlgeschlagen: {e:#}", rule.id),
                }
            }
        })
        .await;
        tokio::select! {
            _ = inner.rules_notify.notified() => {
                // Bündeln und Dateien „zur Ruhe kommen“ lassen.
                tokio::time::sleep(SETTLE + Duration::from_millis(500)).await;
            }
            _ = tokio::time::sleep(RULE_RESCAN) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_are_bound_to_one_device() {
        let offers = UploadOffers::default();
        let a = iroh::SecretKey::generate().public();
        let b = iroh::SecretKey::generate().public();
        let h = Hash::new(b"x");
        offers.offer(h, a);
        assert!(offers.allows(&h, Some(a)));
        assert!(!offers.allows(&h, Some(b)), "anderes Gerät");
        assert!(!offers.allows(&h, None), "unbekannte Gegenstelle");
        assert!(!offers.allows(&Hash::new(b"y"), Some(a)), "anderer Inhalt");
        offers.withdraw(h, a);
        assert!(!offers.allows(&h, Some(a)));
        assert_eq!(backoff(0), 30);
        assert_eq!(backoff(9), 3600);
    }
}
