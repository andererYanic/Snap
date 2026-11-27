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
use crate::twoway::{Action, Wait};
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
    /// Zwei-Wege-Sync: zum Ursprung gesendete Änderungen bzw. angehaltene Löschungen.
    pub pushed: usize,
    pub held: usize,
    /// Dateien, die gerade noch geschrieben werden: gleich noch einmal abgleichen.
    pub retry_soon: usize,
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
    let (stats, actions) = tokio::task::spawn_blocking(move || reconcile_blocking(&inner2)).await??;
    if !actions.is_empty() {
        tokio::spawn(crate::twoway::run(inner.clone(), actions));
    }
    if stats.retry_soon > 0 {
        let notify = inner.pin_notify.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(4)).await;
            notify.notify_one();
        });
    }
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

fn reconcile_blocking(inner: &DaemonInner) -> Result<(ReconcileStats, Vec<Action>)> {
    let db = &inner.db;
    let root = inner.config.pin_dir.clone();
    std::fs::create_dir_all(&root)?;
    let max = inner.settings.pin_max_bytes();
    let mut used = db.pinned_total_size()?;
    let mut stats = ReconcileStats::default();
    let mut actions = Vec::new();

    for pin in db.list_pins()? {
        let targets = db.pin_targets(&pin)?;
        let mut wanted: HashSet<String> = HashSet::new();
        let first_action = actions.len();
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
            let mut rec = db.pinned_by_path(&path_str)?;
            let mut on_disk = std::fs::symlink_metadata(&path).is_ok();
            if pin.two_way {
                match two_way_file(inner, &pin, t, &path, &path_str, rec.as_ref(), on_disk, &mut actions)? {
                    TwoWay::Done => continue,
                    TwoWay::Settling => {
                        stats.retry_soon += 1;
                        continue;
                    }
                    TwoWay::Pushed => {
                        stats.pushed += 1;
                        continue;
                    }
                    // Lokale (ältere) Fassung liegt jetzt im Papierkorb → neu laden
                    TwoWay::Download => {
                        on_disk = false;
                        rec = db.pinned_by_path(&path_str)?;
                    }
                    TwoWay::Normal => {}
                }
            }
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
        if pin.two_way {
            guard_local_deletes(inner, &pin, &root, &mut actions, first_action, &mut stats);
            stats.pushed += two_way_rest(inner, &pin, &root, &mut wanted, &mut actions, &mut stats)?;
            continue;
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
    Ok((stats, actions))
}

enum TwoWay {
    /// Erledigt oder wartet (nichts weiter tun).
    Done,
    /// Wird gerade noch geschrieben: gleich erneut prüfen.
    Settling,
    /// Lokale Änderung wird gesendet.
    Pushed,
    /// Lokale Fassung ist in den Papierkorb gewandert: Fassung des Ursprungs laden.
    Download,
    /// Wie bei einem einfachen Spiegel weiter (laden, falls nötig).
    Normal,
}

/// Zwei-Wege-Sync für eine Datei, die der Ursprung kennt.
#[allow(clippy::too_many_arguments)]
fn two_way_file(
    inner: &DaemonInner,
    pin: &PinRow,
    t: &crate::database::PinTarget,
    path: &Path,
    path_str: &str,
    rec: Option<&PinnedFile>,
    on_disk: bool,
    actions: &mut Vec<Action>,
) -> Result<TwoWay> {
    let db = &inner.db;
    match inner.sync.check(path_str, &t.hash) {
        Wait::Pending => return Ok(TwoWay::Done),
        Wait::Adopt => {
            if let Ok(m) = std::fs::symlink_metadata(path) {
                if rec.is_none() {
                    db.pinned_set_pending(&pinned(pin, t, path_str))?;
                }
                db.pinned_mark_ok(path_str, &t.hash, m.len(), mtime_of(&m))?;
            }
            return Ok(TwoWay::Done);
        }
        Wait::No => {}
    }
    let push = |actions: &mut Vec<Action>, replace: bool| {
        let action = Action::Push {
            origin: pin.origin.clone(),
            share: pin.share.clone(),
            rel: t.rel_path.clone(),
            path: path.to_path_buf(),
            replace,
            expect: t.hash.clone(),
        };
        crate::twoway::queue(inner, actions, action, &t.hash);
    };
    let meta = std::fs::symlink_metadata(path).ok();
    if meta.as_ref().is_some_and(|m| !m.is_file()) {
        return Ok(TwoWay::Normal); // Ordner o. Ä. am Zielort: wie bisher (Konflikt)
    }
    let changed_locally = |r: &PinnedFile| on_disk && !unchanged_since_written(path, r);
    match rec {
        // Lokal geändert (oder früher als Konflikt markiert): Neuestes gewinnt
        Some(r) if (r.state == "ok" || r.state == "conflict") && changed_locally(r) => {
            let m = meta.expect("on_disk");
            if crate::twoway::settling(&m) {
                return Ok(TwoWay::Settling);
            }
            if r.hash == t.hash || mtime_of(&m) >= t.created_at {
                push(actions, true);
                Ok(TwoWay::Pushed)
            } else {
                inner.files.trash.trash(path)?;
                db.pinned_remove(path_str)?; // → wird unten wie eine neue Datei geladen
                Ok(TwoWay::Download)
            }
        }
        // Lokal gelöscht: beim Ursprung in den Papierkorb – außer, er hat sich inzwischen geändert
        Some(r) if r.state == "ok" && !on_disk => {
            if r.hash == t.hash {
                let action = Action::Delete {
                    origin: pin.origin.clone(),
                    share: pin.share.clone(),
                    rel: t.rel_path.clone(),
                    path: path_str.to_string(),
                    expect: t.hash.clone(),
                };
                crate::twoway::queue(inner, actions, action, &t.hash);
                Ok(TwoWay::Pushed)
            } else {
                Ok(TwoWay::Normal)
            }
        }
        // Datei liegt schon da, ohne dass wir sie geschrieben haben (z. B. gerade hochgeladen)
        None if on_disk => {
            let m = meta.expect("on_disk");
            if crate::twoway::settling(&m) {
                return Ok(TwoWay::Settling);
            }
            if crate::twoway::file_hash(path).as_deref() == Some(t.hash.as_str()) {
                db.pinned_set_pending(&pinned(pin, t, path_str))?;
                db.pinned_mark_ok(path_str, &t.hash, m.len(), mtime_of(&m))?;
                Ok(TwoWay::Done)
            } else if mtime_of(&m) >= t.created_at {
                push(actions, true);
                Ok(TwoWay::Pushed)
            } else {
                inner.files.trash.trash(path)?;
                db.pinned_remove(path_str)?; // → wird unten wie eine neue Datei geladen
                Ok(TwoWay::Download)
            }
        }
        _ => Ok(TwoWay::Normal),
    }
}

/// Viele lokale Löschungen auf einmal (oder ein verschwundener Spiegel-Ordner, z. B. ausgehängter
/// Speicher) nie ungefragt zum Ursprung schicken.
fn guard_local_deletes(
    inner: &DaemonInner,
    pin: &PinRow,
    root: &Path,
    actions: &mut Vec<Action>,
    first: usize,
    stats: &mut ReconcileStats,
) {
    let is_delete = |a: &Action| matches!(a, Action::Delete { .. });
    let count = actions[first..].iter().filter(|a| is_delete(a)).count();
    if count == 0 {
        return;
    }
    let mirror_gone = !root.join(&pin.dir_name).join(&pin.share).is_dir();
    if !mirror_gone && inner.sync.may_delete(-pin.id, count) {
        return;
    }
    let mut kept = actions.split_off(first);
    kept.retain(|a| {
        if let Action::Delete { path, .. } = a {
            inner.sync.forget(path);
            false
        } else {
            true
        }
    });
    actions.extend(kept);
    stats.pushed = stats.pushed.saturating_sub(count);
    if !mirror_gone {
        stats.held += count;
        crate::twoway::report_held(inner, pin, count);
    }
}

/// Zwei-Wege-Sync für alles, was der Ursprung (nicht mehr) kennt: seine Löschungen hierher
/// (in den Papierkorb, viele nur nach Bestätigung) und neue lokale Dateien zu ihm.
fn two_way_rest(
    inner: &DaemonInner,
    pin: &PinRow,
    root: &Path,
    wanted: &mut HashSet<String>,
    actions: &mut Vec<Action>,
    stats: &mut ReconcileStats,
) -> Result<usize> {
    let db = &inner.db;
    let mut pushed = 0;
    let mut deletes = Vec::new();
    for rec in db.pinned_of_pin(pin.id)? {
        if wanted.contains(&rec.path) || inner.sync.is_pending(&rec.path) {
            wanted.insert(rec.path.clone());
            continue;
        }
        let path = PathBuf::from(&rec.path);
        let meta = std::fs::symlink_metadata(&path).ok();
        match meta {
            None => {
                db.cancel_stale_pin_jobs(&rec.path, "")?;
                db.pinned_remove(&rec.path)?;
            }
            Some(_) if rec.state == "pending" => {
                db.cancel_stale_pin_jobs(&rec.path, "")?;
                db.pinned_remove(&rec.path)?;
            }
            // Beim Ursprung gelöscht, hier unverändert → hier in den Papierkorb
            Some(_) if rec.state == "ok" && unchanged_since_written(&path, &rec) => deletes.push(rec),
            // Hier inzwischen geändert → als neue Datei zurück zum Ursprung (Arbeit bleibt erhalten)
            Some(_) => {
                db.pinned_remove(&rec.path)?;
            }
        }
    }
    if !deletes.is_empty() {
        if inner.sync.may_delete(pin.id, deletes.len()) {
            let stop = root.join(&pin.dir_name);
            for rec in &deletes {
                let path = PathBuf::from(&rec.path);
                if path.starts_with(&stop) {
                    inner.files.trash.trash(&path)?;
                    remove_empty_parents(&path, &stop);
                }
                db.pinned_remove(&rec.path)?;
                stats.removed += 1;
            }
        } else {
            stats.held += deletes.len();
            for rec in &deletes {
                wanted.insert(rec.path.clone());
            }
            crate::twoway::report_held(inner, pin, deletes.len());
        }
    }
    // Neue lokale Dateien (noch nicht im Ursprung und ohne Eintrag)
    let share_root = root.join(&pin.dir_name).join(&pin.share);
    let known: HashSet<String> =
        db.pinned_of_pin(pin.id)?.into_iter().map(|r| r.path).chain(wanted.iter().cloned()).collect();
    let (new_files, busy) = crate::twoway::untracked(&share_root, &pin.prefix, &known);
    if busy {
        stats.retry_soon += 1;
    }
    for (rel, path) in new_files {
        if inner.sync.is_pending(&path.to_string_lossy()) || safe_target(root, &pin.dir_name, &pin.share, &rel).is_err()
        {
            continue;
        }
        let action = Action::Push {
            origin: pin.origin.clone(),
            share: pin.share.clone(),
            rel,
            path,
            replace: false,
            expect: String::new(),
        };
        crate::twoway::queue(inner, actions, action, "");
        pushed += 1;
    }
    Ok(pushed)
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
    // Zwei-Wege-Sync: lokale Änderungen im Pin-Ordner bemerken (nur, solange es solche Pins gibt)
    let mut watcher: Option<notify::RecommendedWatcher> = None;
    loop {
        let two_way = inner.db.list_pins().map(|p| p.iter().any(|p| p.two_way)).unwrap_or(false);
        if two_way && watcher.is_none() {
            watcher = watch_pin_dir(&inner);
        } else if !two_way {
            watcher = None;
        }
        tokio::select! {
            // Regelmäßig erneut versuchen (z. B. Ursprung war offline)
            _ = tokio::time::sleep(Duration::from_secs(300)), if two_way => {}
            ev = sub.next() => match ev {
                Some(Event::RemoteCatalogChanged { .. }) | Some(Event::Lagged { .. }) => {}
                Some(Event::DeviceRemoved { node_id }) => {
                    let inner2 = inner.clone();
                    match tokio::task::spawn_blocking(move || forget_device_files(&inner2, &node_id)).await {
                        Ok(Ok((removed, kept))) if removed + kept > 0 => {
                            tracing::info!("Entkoppelt: {removed} Offline-Kopien entfernt, {kept} lokal veränderte behalten");
                        }
                        Ok(Err(e)) => tracing::warn!("Aufräumen nach dem Entkoppeln fehlgeschlagen: {e:#}"),
                        _ => {}
                    }
                    continue;
                }
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

fn watch_pin_dir(inner: &Arc<DaemonInner>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    let notify = inner.pin_notify.clone();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if !matches!(ev.kind, notify::EventKind::Access(_)) {
                notify.notify_one();
            }
        }
    })
    .ok()?;
    w.watch(&inner.config.pin_dir, notify::RecursiveMode::Recursive).ok()?;
    Some(w)
}

/// Nach dem Entkoppeln: Offline-Kopien (Pins) und Vorschaubilder des Geräts entfernen.
/// Lokal veränderte Kopien bleiben als normale Dateien liegen (sie gehören jetzt dem Nutzer);
/// ausdrücklich heruntergeladene Dateien werden nie angefasst. Gibt (entfernt, behalten) zurück.
pub fn forget_device_files(inner: &DaemonInner, node_id: &str) -> Result<(usize, usize)> {
    let db = &inner.db;
    let root = inner.config.pin_dir.clone();
    let (mut removed, mut kept) = (0, 0);
    for pin in db.list_pins()?.into_iter().filter(|p| p.origin == node_id) {
        let stop = root.join(&pin.dir_name);
        for rec in db.pinned_of_pin(pin.id)? {
            let path = PathBuf::from(&rec.path);
            // Nur Dateien unter dem eigenen Pin-Ordner (nie durch Symlinks oder nach außen).
            if !path.starts_with(&stop) {
                continue;
            }
            if rec.state == "ok" && unchanged_since_written(&path, &rec) {
                std::fs::remove_file(&path)?;
                remove_empty_parents(&path, &stop);
                removed += 1;
            } else if std::fs::symlink_metadata(&path).is_ok() && rec.state != "pending" {
                kept += 1;
            }
        }
        db.remove_pin(pin.id)?;
        let _ = std::fs::remove_dir(&stop); // nur, wenn leer
    }
    let thumbs = crate::remote_thumbs::device_dir(&inner.config.thumb_dir(), node_id);
    if thumbs.is_dir() {
        std::fs::remove_dir_all(&thumbs)?;
    }
    Ok((removed, kept))
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
