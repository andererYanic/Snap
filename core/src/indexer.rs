//! Hintergrund-Indexierung für die lokale Suche.
//!
//! - Erster Start: Home-Ordner wird als Ort angelegt; angeschlossene Laufwerke unter
//!   `/media/<user>` und `/run/media/<user>` werden automatisch erkannt.
//! - Watcher: jeder indexierte Ordner wird einzeln (nicht rekursiv) beobachtet – so werden
//!   ausgeschlossene Bäume wie `node_modules` gar nicht erst beobachtet. Obergrenze
//!   `index_max_watches`; darüber hinaus (oder bei erschöpftem inotify-Limit) greift der
//!   periodische Abgleich.
//! - Periodischer Abgleich: liest nur Ordner, deren mtime sich geändert hat.

use crate::daemon::DaemonInner;
use crate::index::LocalIndex;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

/// Abstand der periodischen Abgleiche.
const FULL_PASS_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// Watcher-Ereignisse bündeln: 1 s Ruhe, höchstens 5 s.
const DEBOUNCE_QUIET: Duration = Duration::from_secs(1);
const DEBOUNCE_MAX: Duration = Duration::from_secs(5);
/// Mehr geänderte Ordner auf einmal → lieber ein vollständiger Abgleich.
const MAX_DIRS_PER_BATCH: usize = 2_000;
/// Netzwerk-Dateisysteme werden nicht automatisch indexiert (langsam, ggf. getaktet).
const NETWORK_FS: &[&str] = &["nfs", "nfs4", "cifs", "smb3", "smbfs", "fuse.sshfs", "fuse.rclone", "9p", "afs"];

pub struct IndexService {
    pub index: Arc<LocalIndex>,
    rescan: Notify,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    watched: AtomicU64,
    max_watches: u64,
    limit_warned: AtomicBool,
    watched_roots: Mutex<HashSet<i64>>,
    /// Signal an die Foto-Zeitachse: Index hat sich geändert.
    pub(crate) media_dirty: Notify,
    pub(crate) stopping: AtomicBool,
    /// Monatsübersicht der ganzen Zeitachse (vom Aktualisierer vorberechnet).
    pub(crate) months_cache: Mutex<Option<Vec<crate::photos::MonthCount>>>,
}

impl IndexService {
    pub fn new(index: LocalIndex, max_watches: usize) -> Self {
        Self {
            index: Arc::new(index),
            rescan: Notify::new(),
            watcher: Mutex::new(None),
            watched: AtomicU64::new(0),
            max_watches: max_watches as u64,
            limit_warned: AtomicBool::new(false),
            watched_roots: Mutex::new(HashSet::new()),
            media_dirty: Notify::new(),
            stopping: AtomicBool::new(false),
            months_cache: Mutex::new(None),
        }
    }

    /// Startet sofort einen Abgleich aller Orte.
    pub fn rescan(&self) {
        self.rescan.notify_one();
    }

    pub fn watched_dirs(&self) -> u64 {
        self.watched.load(Ordering::Relaxed)
    }

    /// Beendet die Beobachtung (Daemon fährt herunter).
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.watcher.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// Beobachtet die Ordner (blockierend, bis zur Obergrenze).
    fn watch_dirs(&self, dirs: &[PathBuf]) {
        use notify::{RecursiveMode, Watcher};
        let mut guard = self.watcher.lock().unwrap_or_else(|e| e.into_inner());
        let Some(w) = guard.as_mut() else { return };
        for d in dirs {
            if self.watched.load(Ordering::Relaxed) >= self.max_watches {
                self.warn_limit(&format!("Obergrenze index_max_watches = {}", self.max_watches));
                return;
            }
            match w.watch(d, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.watched.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) if matches!(e.kind, notify::ErrorKind::MaxFilesWatch) => {
                    self.warn_limit("inotify-Limit des Systems (fs.inotify.max_user_watches)");
                    return;
                }
                Err(e) => tracing::debug!("{} nicht beobachtbar: {e}", d.display()),
            }
        }
    }

    /// Liest die Ordner sofort neu ein (blockierend) und beobachtet neu entstandene Ordner.
    /// Genutzt vom Watcher und nach eigenen Dateioperationen (Suche sofort aktuell).
    pub fn refresh_dirs(&self, dirs: impl IntoIterator<Item = PathBuf>) {
        let roots: Vec<(i64, PathBuf)> = match self.index.roots() {
            Ok(r) => r.into_iter().filter(|r| r.enabled).map(|r| (r.id, PathBuf::from(r.path))).collect(),
            Err(_) => return,
        };
        let mut new_dirs = Vec::new();
        for d in dirs {
            // Längster passender Ort (Orte können ineinander liegen).
            let Some((id, root)) =
                roots.iter().filter(|(_, r)| d.starts_with(r)).max_by_key(|(_, r)| r.as_os_str().len())
            else {
                continue;
            };
            match self.index.refresh(*id, root, &d, false) {
                Ok(st) => new_dirs.extend(st.new_dirs),
                Err(e) => tracing::debug!("Suchindex für {} nicht aktualisiert: {e:#}", d.display()),
            }
        }
        self.watch_dirs(&new_dirs);
    }

    fn warn_limit(&self, what: &str) {
        if !self.limit_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "Lokale Suche: {what} erreicht – weitere Ordner werden nur beim periodischen Abgleich \
                 aktualisiert ({} beobachtet).",
                self.watched.load(Ordering::Relaxed)
            );
        }
    }
}

/// Dekodiert die Oktal-Escapes in `/proc/self/mounts` (z. B. `\040` für Leerzeichen).
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            if v <= 255 {
                out.push(v as u8);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Angeschlossene Laufwerke des Nutzers aus einer Mount-Tabelle.
fn drives_from_mounts(mounts: &str, user: &str) -> Vec<PathBuf> {
    let prefixes = [format!("/media/{user}/"), format!("/run/media/{user}/")];
    let mut out = Vec::new();
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let (Some(_dev), Some(mp), Some(fstype)) = (f.next(), f.next(), f.next()) else { continue };
        let mp = unescape_mount(mp);
        if NETWORK_FS.contains(&fstype) || !prefixes.iter().any(|p| mp.starts_with(p.as_str())) {
            continue;
        }
        // Nur direkte Einhängepunkte (/media/<user>/<Laufwerk>), keine Pfad-Tricks.
        if mp.contains("/../") || mp.ends_with("/..") {
            continue;
        }
        out.push(PathBuf::from(mp));
    }
    out.sort();
    out.dedup();
    out
}

fn detect_drives() -> Vec<PathBuf> {
    let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) else { return Vec::new() };
    let Some(user) = home.file_name().and_then(|n| n.to_str()).map(str::to_string) else { return Vec::new() };
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    drives_from_mounts(&mounts, &user).into_iter().filter(|p| p.is_dir()).collect()
}

/// Hintergrund-Task: Erstbefüllung, Watcher, periodischer Abgleich.
pub(crate) async fn run_indexer(inner: Arc<DaemonInner>) {
    let Some(svc) = inner.index.clone() else { return };
    let config = inner.config.clone();
    // Erster Start: Home anlegen.
    match svc.index.seeded() {
        Ok(false) => {
            if config.index_home {
                if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
                    if let Err(e) = svc.index.add_root(&home) {
                        tracing::warn!("Home-Ordner nicht als Suchort anlegbar: {e:#}");
                    }
                }
            }
            let _ = svc.index.set_seeded();
        }
        Ok(true) => {}
        Err(e) => tracing::warn!("Lokaler Index nicht lesbar: {e:#}"),
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, notify::EventKind::Access(_)) {
                return;
            }
            for p in ev.paths {
                let _ = tx.send(p);
            }
        }
    }) {
        Ok(w) => {
            *svc.watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(w);
            tokio::spawn(run_event_loop(svc.clone(), rx));
        }
        Err(e) => tracing::warn!("Lokale Suche ohne Watcher (nur periodischer Abgleich): {e}"),
    }

    loop {
        full_pass(&svc, config.index_drives).await;
        tokio::select! {
            _ = tokio::time::sleep(FULL_PASS_INTERVAL) => {}
            _ = svc.rescan.notified() => {}
        }
    }
}

/// Gleicht alle aktiven Orte ab (neue Orte: vollständig) und beobachtet neue Orte.
async fn full_pass(svc: &Arc<IndexService>, drives: bool) {
    let s = svc.clone();
    let res = tokio::task::spawn_blocking(move || {
        if drives {
            for d in detect_drives() {
                if let Err(e) = s.index.add_root_auto(&d) {
                    tracing::debug!("Laufwerk {} nicht anlegbar: {e:#}", d.display());
                }
            }
        }
        for root in s.index.roots()?.into_iter().filter(|r| r.enabled) {
            let path = PathBuf::from(&root.path);
            if !path.is_dir() {
                continue; // z. B. Laufwerk nicht eingehängt: Index behalten
            }
            let t = std::time::Instant::now();
            let result = if root.last_scan.is_none() {
                s.index.rebuild_root(root.id, &path).map(|st| format!("{} Dateien, {} Ordner", st.files, st.dirs))
            } else {
                s.index.refresh(root.id, &path, &path, true).map(|st| {
                    format!("+{} −{} ~{} ({} Ordner gelesen)", st.added, st.removed, st.updated, st.dirs_read)
                })
            };
            match result {
                Ok(msg) => tracing::info!("Suchindex {}: {msg} in {:.1} s", root.path, t.elapsed().as_secs_f64()),
                Err(e) => tracing::warn!("Suchindex {} fehlgeschlagen: {e:#}", root.path),
            }
            let new_root = s.watched_roots.lock().unwrap_or_else(|e| e.into_inner()).insert(root.id);
            if new_root {
                let budget = s.max_watches.saturating_sub(s.watched_dirs()) as usize;
                s.watch_dirs(&s.index.dir_paths(root.id, budget)?);
            }
        }
        anyhow::Ok(())
    })
    .await;
    match res {
        Ok(Err(e)) => tracing::warn!("Abgleich des Suchindex fehlgeschlagen: {e:#}"),
        Err(e) => tracing::warn!("Abgleich des Suchindex abgebrochen: {e}"),
        Ok(Ok(())) => {}
    }
    svc.media_dirty.notify_one();
}

/// Verarbeitet Watcher-Ereignisse gebündelt: liest nur die betroffenen Ordner neu.
async fn run_event_loop(svc: Arc<IndexService>, mut rx: tokio::sync::mpsc::UnboundedReceiver<PathBuf>) {
    loop {
        let Some(first) = rx.recv().await else { return };
        let mut dirs: HashSet<PathBuf> = HashSet::new();
        let mut add = |p: PathBuf| {
            // Jede Änderung betrifft den Inhalt des Elternordners.
            if let Some(parent) = p.parent() {
                dirs.insert(parent.to_path_buf());
            }
        };
        add(first);
        let deadline = tokio::time::Instant::now() + DEBOUNCE_MAX;
        loop {
            let until = deadline.min(tokio::time::Instant::now() + DEBOUNCE_QUIET);
            match tokio::time::timeout_at(until, rx.recv()).await {
                Ok(Some(p)) => add(p),
                Ok(None) => return,
                Err(_) => break,
            }
        }
        if dirs.len() > MAX_DIRS_PER_BATCH {
            svc.rescan();
            continue;
        }
        let s = svc.clone();
        let _ = tokio::task::spawn_blocking(move || s.refresh_dirs(dirs)).await;
        svc.media_dirty.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_table_parsing() {
        let mounts = "\
/dev/sda1 / ext4 rw 0 0
/dev/sdb1 /media/yanic/USB\\040Stick vfat rw 0 0
/dev/sdc1 /run/media/yanic/Backup exfat rw 0 0
server:/x /media/yanic/nas nfs4 rw 0 0
/dev/sdd1 /media/anderer/Fremd vfat rw 0 0
/dev/sde1 /media/yanic/../../etc ext4 rw 0 0
kaputt
";
        let d = drives_from_mounts(mounts, "yanic");
        assert_eq!(d, [PathBuf::from("/media/yanic/USB Stick"), PathBuf::from("/run/media/yanic/Backup")]);
        assert_eq!(unescape_mount("a\\134b\\0"), "a\\b\\0");
        assert_eq!(unescape_mount("\\777x"), "\\777x");
    }
}
