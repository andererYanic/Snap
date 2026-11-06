//! Smart-Pinning: Freigaben (oder Unterordner) anderer Geräte lokal spiegeln.
//!
//! - Ziel: `<pin_dir>/<Gerät>/<Freigabe>/<relativer Pfad>`.
//! - Downloads laufen über die Warteschlange (Aufträge vom Typ `PIN`): Offline-Phasen,
//!   Backoff, Live-Wecken und Mehrquellen-Fallback gelten automatisch.
//! - Sicherheit: Pfade stammen von anderen Geräten. Sie werden streng geprüft, es wird nie
//!   durch Symlinks geschrieben, und lokal veränderte Kopien werden nie überschrieben oder
//!   gelöscht (Konflikt).
//! - Kontingent: `pin_max_gb`.

use crate::daemon::DaemonInner;
use crate::database::{NewJob, PinRow, PinnedFile};
use crate::events::Event;
use crate::ledger::{validate_rel_path, validate_share_name};
use crate::network::endpoint_addr;
use anyhow::{bail, Result};
use iroh_blobs::{ticket::BlobTicket, BlobFormat, Hash};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileStats {
    pub queued: usize,
    pub removed: usize,
    pub conflicts: usize,
    pub quota_skipped: usize,
    pub unsafe_skipped: usize,
}

fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Baut den Zielpfad einer gespiegelten Datei und prüft ihn:
/// alle Bestandteile gültig, kein vorhandener Bestandteil ist ein Symlink, das Ziel selbst
/// ist (falls vorhanden) eine normale Datei. So kann ein bösartiger Eintrag oder ein
/// untergeschobener Link nie außerhalb des Pin-Ordners schreiben.
pub fn safe_target(root: &Path, dir_name: &str, share: &str, rel: &str) -> Result<PathBuf> {
    validate_share_name(dir_name)?;
    validate_share_name(share)?;
    validate_rel_path(rel)?;
    let mut path = root.to_path_buf();
    let parts: Vec<&str> = [dir_name, share].into_iter().chain(rel.split('/')).collect();
    let last = parts.len() - 1;
    for (i, part) in parts.iter().enumerate() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => bail!("Symlink im Pin-Ordner: {}", path.display()),
            Ok(m) if i < last && !m.is_dir() => bail!("Kein Ordner: {}", path.display()),
            Ok(m) if i == last && !m.is_file() => bail!("Ziel ist keine normale Datei: {}", path.display()),
            _ => {}
        }
    }
    Ok(path)
}

/// Datei liegt unverändert so vor, wie wir sie zuletzt geschrieben haben?
fn unchanged_since_written(path: &Path, rec: &PinnedFile) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.is_file() && m.len() == rec.size && mtime_of(&m) == rec.mtime)
        .unwrap_or(false)
}

/// Ordnername für ein Gerät unter dem Pin-Verzeichnis.
pub fn dir_name_for(name: Option<&str>, node_id: &str) -> String {
    let short = &node_id[..6.min(node_id.len())];
    let cleaned: String = name
        .unwrap_or("")
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .take(40)
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() || validate_share_name(&cleaned).is_err() {
        format!("Gerät-{short}")
    } else {
        cleaned
    }
}

/// Gleicht alle Pins mit den Ledger-Einträgen ab und stellt nötige Downloads in die Warteschlange.
pub async fn reconcile(inner: &Arc<DaemonInner>) -> Result<ReconcileStats> {
    let inner2 = inner.clone();
    let stats = tokio::task::spawn_blocking(move || reconcile_blocking(&inner2)).await??;
    if stats.queued > 0 {
        inner.sync_notifier.notify_one();
    }
    if stats != ReconcileStats::default() {
        inner.db.events().emit(Event::PinsChanged {
            queued: stats.queued,
            removed: stats.removed,
            conflicts: stats.conflicts,
        });
    }
    Ok(stats)
}

fn reconcile_blocking(inner: &DaemonInner) -> Result<ReconcileStats> {
    let db = &inner.db;
    let root = inner.config.pin_dir.clone();
    std::fs::create_dir_all(&root)?;
    let max = inner.settings.pin_max_bytes();
    let mut used = db.pinned_total_size()?;
    let mut stats = ReconcileStats::default();

    for pin in db.list_pins()? {
        let targets = db.pin_targets(&pin)?;
        let mut wanted: HashSet<String> = HashSet::new();
        for t in targets.iter().filter(|t| !t.deleted) {
            let path = match safe_target(&root, &pin.dir_name, &pin.share, &t.rel_path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("Pin '{}': Ziel übersprungen: {e:#}", pin.share);
                    stats.unsafe_skipped += 1;
                    continue;
                }
            };
            let path_str = path.to_string_lossy().into_owned();
            wanted.insert(path_str.clone());
            let rec = db.pinned_by_path(&path_str)?;
            let on_disk = std::fs::symlink_metadata(&path).is_ok();
            match &rec {
                Some(r) if r.state == "ok" && r.hash == t.hash && unchanged_since_written(&path, r) => continue,
                Some(r) if r.state == "pending" && r.hash == t.hash => continue, // läuft bereits
                Some(r) if r.state == "conflict" => {
                    stats.conflicts += 1;
                    continue;
                }
                Some(r) if r.state == "ok" && on_disk && !unchanged_since_written(&path, r) => {
                    // Lokal verändert -> nie überschreiben.
                    db.pinned_mark_conflict(&path_str)?;
                    stats.conflicts += 1;
                    continue;
                }
                None if on_disk => {
                    // Fremde Datei am Zielort, die wir nicht geschrieben haben.
                    db.pinned_set_pending(&pinned(&pin, t, &path_str))?;
                    db.pinned_mark_conflict(&path_str)?;
                    stats.conflicts += 1;
                    continue;
                }
                _ => {}
            }
            // Laden (neu oder aktualisiert), sofern das Kontingent reicht.
            let old = rec.as_ref().map(|r| r.size).unwrap_or(0);
            if max > 0 && used.saturating_sub(old) + t.size > max {
                stats.quota_skipped += 1;
                continue;
            }
            used = used.saturating_sub(old) + t.size;
            db.pinned_set_pending(&pinned(&pin, t, &path_str))?;
            db.cancel_stale_pin_jobs(&path_str, &t.hash)?;
            let Ok(origin) = pin.origin.parse() else { continue };
            let Ok(hash) = t.hash.parse::<Hash>() else { continue };
            let hints = db.addr_hints(&pin.origin).unwrap_or_default();
            let ticket = BlobTicket::new(endpoint_addr(origin, &hints), hash, BlobFormat::Raw).to_string();
            db.enqueue_download(&NewJob {
                action: "PIN",
                target_hash: &t.hash,
                ticket: &ticket,
                peer_id: &pin.origin,
                output_path: &path_str,
                explicit_output: true,
                force: true,
                last_error: "",
            })?;
            stats.queued += 1;
        }
        // Nicht mehr gewünscht (gelöscht oder außerhalb des Pins): nur entfernen, wenn unverändert.
        for rec in db.pinned_of_pin(pin.id)? {
            if wanted.contains(&rec.path) {
                continue;
            }
            let path = PathBuf::from(&rec.path);
            let exists = std::fs::symlink_metadata(&path).is_ok();
            if rec.state == "ok" && exists && unchanged_since_written(&path, &rec) {
                std::fs::remove_file(&path)?;
                remove_empty_parents(&path, &root.join(&pin.dir_name));
                db.pinned_remove(&rec.path)?;
                stats.removed += 1;
            } else if !exists || rec.state == "pending" {
                db.cancel_stale_pin_jobs(&rec.path, "")?;
                db.pinned_remove(&rec.path)?;
            } else {
                db.pinned_mark_conflict(&rec.path)?;
                stats.conflicts += 1;
            }
        }
    }
    Ok(stats)
}

fn pinned(pin: &PinRow, t: &crate::database::PinTarget, path: &str) -> PinnedFile {
    PinnedFile {
        path: path.to_string(),
        pin_id: pin.id,
        origin: pin.origin.clone(),
        entry_id: t.entry_id,
        hash: t.hash.clone(),
        size: t.size,
        mtime: 0,
        state: "pending".into(),
    }
}

/// Entfernt leere Ordner bis (ausschließlich) `stop`.
fn remove_empty_parents(path: &Path, stop: &Path) {
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == stop || !d.starts_with(stop) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// Vor dem Schreiben eines PIN-Auftrags: Darf die Zieldatei ersetzt werden?
/// (Sie könnte seit dem Einreihen lokal verändert oder durch einen Symlink ersetzt worden sein.)
pub fn may_write_pin(inner: &DaemonInner, output: &Path) -> bool {
    let path_str = output.to_string_lossy();
    let Ok(Some(rec)) = inner.db.pinned_by_path(&path_str) else { return false };
    match std::fs::symlink_metadata(output) {
        Err(_) => rec.state != "conflict",
        Ok(m) if !m.is_file() => false,
        Ok(_) => rec.mtime != 0 && unchanged_since_written(output, &rec),
    }
}

/// Nach erfolgreichem Schreiben: Stand merken.
pub fn record_written(inner: &DaemonInner, output: &Path, hash: &str) -> Result<()> {
    let m = std::fs::symlink_metadata(output)?;
    inner.db.pinned_mark_ok(&output.to_string_lossy(), hash, m.len(), mtime_of(&m))
}

/// Hintergrund-Task: gleicht Pins bei Änderungen ab (gebündelt).
pub async fn run_pin_loop(inner: Arc<DaemonInner>) {
    let mut sub = inner.db.events().subscribe();
    if let Err(e) = reconcile(&inner).await {
        tracing::warn!("Pin-Abgleich fehlgeschlagen: {e:#}");
    }
    loop {
        tokio::select! {
            ev = sub.next() => match ev {
                Some(Event::RemoteCatalogChanged { .. }) | Some(Event::Lagged { .. }) => {}
                Some(_) => continue,
                None => return,
            },
            _ = inner.pin_notify.notified() => {}
        }
        // Bündeln: weitere Änderungen in kurzer Folge zusammenfassen.
        tokio::time::sleep(DEBOUNCE).await;
        if let Err(e) = reconcile(&inner).await {
            tracing::warn!("Pin-Abgleich fehlgeschlagen: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_target_blocks_escapes_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Snap");
        let outside = dir.path().join("draussen");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let ok = safe_target(&root, "PC", "Fotos", "2024/a.jpg").unwrap();
        assert_eq!(ok, root.join("PC/Fotos/2024/a.jpg"));
        for (d, s, r) in [
            ("PC", "Fotos", "../../x"),
            ("PC", "Fotos", "/etc/passwd"),
            ("..", "Fotos", "a"),
            ("PC", "..", "a"),
            ("PC", "a/b", "c"),
            ("PC", "Fotos", "a/./b"),
        ] {
            assert!(safe_target(&root, d, s, r).is_err(), "{d}/{s}/{r}");
        }
        // Untergeschobener Symlink im Pin-Ordner
        std::fs::create_dir_all(root.join("PC")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("PC/Fotos")).unwrap();
        assert!(safe_target(&root, "PC", "Fotos", "a.jpg").is_err());
        // Ziel ist ein Symlink auf eine fremde Datei
        std::fs::create_dir_all(root.join("PC/Docs")).unwrap();
        std::fs::write(outside.join("geheim"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.join("geheim"), root.join("PC/Docs/a.txt")).unwrap();
        assert!(safe_target(&root, "PC", "Docs", "a.txt").is_err());
        // Ziel ist ein Ordner
        std::fs::create_dir_all(root.join("PC/Docs/ordner")).unwrap();
        assert!(safe_target(&root, "PC", "Docs", "ordner").is_err());
    }

    #[test]
    fn device_dir_names() {
        assert_eq!(dir_name_for(Some("Arch-Desktop"), "abcdef123"), "Arch-Desktop");
        assert_eq!(dir_name_for(Some("../böse/name"), "abcdef123"), "_böse_name");
        assert_eq!(dir_name_for(Some(".."), "abcdef123"), "Gerät-abcdef");
        assert_eq!(dir_name_for(None, "abcdef123"), "Gerät-abcdef");
    }
}
