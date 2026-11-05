//! Ordner-Freigaben: Scannen, Beobachten, Importieren.
//!
//! - Symlinks werden nie verfolgt (weder Dateien noch Ordner).
//! - Unveränderte Dateien (gleiche Größe + Änderungszeit) werden nicht neu gehasht.
//! - Import per Referenz (keine Kopie), parallel auf allen Kernen, Datenbank-Übernahme
//!   stapelweise in einer Transaktion.
//! - Der Watcher bündelt Ereignisse (2 s Ruhe, höchstens 10 s) und scannt nur die
//!   betroffenen Unterordner neu.

use crate::daemon::DaemonInner;
use crate::database::{KnownShareFile, ScanChange, ShareInfo};
use crate::events::Event;
use crate::ledger::{validate_rel_path, LedgerEntry};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Übernahme in die Datenbank in Stapeln dieser Größe.
const BATCH: usize = 500;
const DEBOUNCE_QUIET: Duration = Duration::from_secs(2);
const DEBOUNCE_MAX: Duration = Duration::from_secs(10);
/// Ab so vielen betroffenen Unterordnern lieber alles scannen.
const MAX_PREFIXES: usize = 256;

/// Dateien, die nie geteilt werden (temporär, System, eigene Teil-Downloads).
pub fn is_ignored(name: &str) -> bool {
    const EXACT: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini", ".git", ".hg", ".svn"];
    const SUFFIX: &[&str] = &[".part", ".partial", ".crdownload", ".tmp", ".swp", ".swx", "~", ".snap-part"];
    EXACT.contains(&name)
        || SUFFIX.iter().any(|s| name.ends_with(s))
        || name.starts_with(".~lock.")
        || name.starts_with("~$")
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanStats {
    pub added_or_changed: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub skipped: usize,
}

/// Eine Datei, die (neu) gehasht werden muss.
struct Candidate {
    abs: PathBuf,
    rel: String,
    name: String,
    size: u64,
    mtime: i64,
}

fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Relativer Pfad mit `/` als Trenner; `None` bei ungültigen Namen (nicht UTF-8 usw.).
fn rel_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let rel = parts?.join("/");
    validate_rel_path(&rel).ok().map(|_| rel)
}

/// Durchläuft die Freigabe (oder nur `prefixes`) und ermittelt Änderungen.
/// Blockierend – läuft in `spawn_blocking`.
fn walk(
    root: &Path,
    prefixes: Option<&[String]>,
    known: &HashMap<String, KnownShareFile>,
) -> (Vec<Candidate>, Vec<i64>, ScanStats) {
    let mut stats = ScanStats::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates = Vec::new();
    let starts: Vec<PathBuf> = match prefixes {
        None => vec![root.to_path_buf()],
        Some(p) => p.iter().map(|r| if r.is_empty() { root.to_path_buf() } else { root.join(r) }).collect(),
    };
    for start in &starts {
        // Startpunkt selbst darf kein Symlink sein (könnte aus der Freigabe herausführen).
        match std::fs::symlink_metadata(start) {
            Ok(m) if m.file_type().is_symlink() => continue,
            Err(_) => continue, // gelöscht -> wird unten als Löschung erkannt
            _ => {}
        }
        let walker = walkdir::WalkDir::new(start).follow_links(false).into_iter().filter_entry(|e| {
            e.depth() == 0 || e.file_name().to_str().is_some_and(|n| !is_ignored(n))
        });
        for entry in walker {
            let Ok(entry) = entry else {
                stats.skipped += 1;
                continue;
            };
            if !entry.file_type().is_file() {
                continue; // Ordner, Symlinks, Geräte, Sockets
            }
            let Some(rel) = rel_of(root, entry.path()) else {
                stats.skipped += 1;
                continue;
            };
            let Ok(meta) = entry.metadata() else {
                stats.skipped += 1;
                continue;
            };
            let (size, mtime) = (meta.len(), mtime_of(&meta));
            seen.insert(rel.clone());
            if known.get(&rel).is_some_and(|k| k.size == size && k.mtime == mtime) {
                stats.unchanged += 1;
                continue;
            }
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            candidates.push(Candidate { abs: entry.into_path(), rel, name, size, mtime });
        }
    }
    // Gelöscht: bekannt, im gescannten Bereich, aber nicht mehr gesehen.
    let in_scope = |rel: &str| match prefixes {
        None => true,
        Some(p) => p.iter().any(|pre| pre.is_empty() || rel == pre || rel.starts_with(&format!("{pre}/"))),
    };
    let deleted: Vec<i64> = known
        .iter()
        .filter(|(rel, _)| in_scope(rel) && !seen.contains(*rel))
        .map(|(_, k)| k.id)
        .collect();
    stats.deleted = deleted.len();
    (candidates, deleted, stats)
}

/// Scannt eine Freigabe (ganz oder nur bestimmte Unterordner) und übernimmt Änderungen.
pub async fn scan_share(inner: &Arc<DaemonInner>, share: &ShareInfo, prefixes: Option<Vec<String>>) -> Result<ScanStats> {
    let root = PathBuf::from(&share.root);
    let db = inner.db.clone();
    let share_id = share.id;
    let (candidates, deleted, mut stats) = tokio::task::spawn_blocking(move || -> Result<_> {
        let known = db.known_share_files(share_id)?;
        Ok(walk(&root, prefixes.as_deref(), &known))
    })
    .await
    .context("Scan abgebrochen")??;

    // Hashen/Importieren parallel, begrenzt auf die Anzahl der Kerne.
    let parallel = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 16);
    let sem = Arc::new(tokio::sync::Semaphore::new(parallel));
    let mut set = tokio::task::JoinSet::new();
    for c in candidates {
        let permit = sem.clone().acquire_owned().await?;
        let inner = inner.clone();
        set.spawn(async move {
            let _permit = permit;
            let hash = inner.network.import_reference(&c.abs).await;
            // Während des Imports verändert? Dann beim nächsten Ereignis erneut.
            let stable = std::fs::symlink_metadata(&c.abs)
                .map(|m| m.is_file() && m.len() == c.size && mtime_of(&m) == c.mtime)
                .unwrap_or(false);
            (c, hash, stable)
        });
    }
    let mut changes: Vec<ScanChange> = deleted.into_iter().map(|file_id| ScanChange::Delete { file_id }).collect();
    let mut pending = 0usize;
    while let Some(res) = set.join_next().await {
        let Ok((c, hash, stable)) = res else { continue };
        match (hash, stable) {
            (Ok(hash), true) => {
                changes.push(ScanChange::Upsert {
                    rel_path: c.rel,
                    abs_path: c.abs.to_string_lossy().into_owned(),
                    name: c.name,
                    hash: hash.to_string(),
                    size: c.size,
                    mtime: c.mtime,
                });
                stats.added_or_changed += 1;
            }
            (Err(e), _) => {
                tracing::debug!("Import fehlgeschlagen ({}): {e:#}", c.abs.display());
                stats.skipped += 1;
            }
            (Ok(_), false) => stats.skipped += 1,
        }
        pending += 1;
        if changes.len() >= BATCH {
            apply(inner, share, std::mem::take(&mut changes)).await?;
        }
    }
    let _ = pending;
    apply(inner, share, changes).await?;

    if stats.added_or_changed > 0 || stats.deleted > 0 {
        let total = inner.db.known_share_files(share.id).map(|k| k.len()).unwrap_or(0);
        inner.db.events().emit(Event::ShareScanned {
            name: share.name.clone(),
            changed: stats.added_or_changed,
            deleted: stats.deleted,
            total,
        });
    }
    Ok(stats)
}

async fn apply(inner: &Arc<DaemonInner>, share: &ShareInfo, changes: Vec<ScanChange>) -> Result<()> {
    if changes.is_empty() {
        return Ok(());
    }
    let db = inner.db.clone();
    let key = inner.secret_key.clone();
    let origin = inner.self_id();
    let share = share.clone();
    tokio::task::spawn_blocking(move || db.apply_scan(&origin, &share, &changes, &move |e: &LedgerEntry| e.sign(&key)))
        .await
        .context("Übernahme abgebrochen")??;
    Ok(())
}

/// Beobachtet eine Freigabe. Wird der zurückgegebene Watcher fallen gelassen, endet
/// die Beobachtung.
pub fn watch_share(inner: Arc<DaemonInner>, share: ShareInfo) -> Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for p in event.paths {
                let _ = tx.send(p);
            }
        }
    })?;
    watcher.watch(Path::new(&share.root), RecursiveMode::Recursive)?;

    tokio::spawn(async move {
        let root = PathBuf::from(&share.root);
        loop {
            // Auf das erste Ereignis warten …
            let Some(first) = rx.recv().await else { return };
            let mut paths: HashSet<PathBuf> = HashSet::from([first]);
            // … dann bündeln, bis 2 s Ruhe herrschen (höchstens 10 s).
            let deadline = tokio::time::Instant::now() + DEBOUNCE_MAX;
            loop {
                match tokio::time::timeout_at(deadline.min(tokio::time::Instant::now() + DEBOUNCE_QUIET), rx.recv()).await {
                    Ok(Some(p)) => {
                        paths.insert(p);
                    }
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            let prefixes = affected_prefixes(&root, &paths);
            if let Err(e) = scan_share(&inner, &share, prefixes).await {
                tracing::warn!("Scan von '{}' fehlgeschlagen: {e:#}", share.name);
            }
        }
    });
    Ok(watcher)
}

/// Ermittelt die minimal nötigen Unterordner (relativ), die neu gescannt werden müssen.
/// `None` = ganze Freigabe.
fn affected_prefixes(root: &Path, paths: &HashSet<PathBuf>) -> Option<Vec<String>> {
    let mut dirs: Vec<String> = Vec::new();
    for p in paths {
        // Existierender Ordner: ihn selbst scannen; sonst (Datei/gelöscht) den Elternordner.
        let dir = if std::fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false) { p.as_path() } else { p.parent()? };
        if !dir.starts_with(root) {
            continue;
        }
        let rel = rel_of(root, dir).unwrap_or_default();
        if rel.is_empty() && dir != root {
            return None; // ungültiger Name -> sicherheitshalber alles
        }
        dirs.push(rel);
    }
    if dirs.iter().any(|d| d.is_empty()) || dirs.len() > MAX_PREFIXES {
        return None;
    }
    // Verschachtelte Präfixe zusammenfassen.
    dirs.sort();
    dirs.dedup();
    let mut minimal: Vec<String> = Vec::new();
    for d in dirs {
        if !minimal.iter().any(|m| d == *m || d.starts_with(&format!("{m}/"))) {
            minimal.push(d);
        }
    }
    Some(minimal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_list() {
        for n in [".DS_Store", "a.part", "b.tmp", "~$doc.docx", ".~lock.x#", "datei~", ".git", "x.snap-part"] {
            assert!(is_ignored(n), "{n}");
        }
        for n in ["foto.jpg", ".bashrc", "notiz.txt", "partner.pdf"] {
            assert!(!is_ignored(n), "{n}");
        }
    }

    #[test]
    fn walk_skips_symlinks_and_detects_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("share");
        let outside = dir.path().join("geheim");
        std::fs::create_dir_all(root.join("unter")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("id_rsa"), b"key").unwrap();
        std::fs::write(root.join("a.txt"), b"a").unwrap();
        std::fs::write(root.join("unter/b.txt"), b"b").unwrap();
        std::fs::write(root.join("x.part"), b"tmp").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link-ordner")).unwrap();
        std::os::unix::fs::symlink(outside.join("id_rsa"), root.join("link-datei")).unwrap();

        let (cands, deleted, stats) = walk(&root, None, &HashMap::new());
        let mut rels: Vec<&str> = cands.iter().map(|c| c.rel.as_str()).collect();
        rels.sort();
        assert_eq!(rels, vec!["a.txt", "unter/b.txt"], "keine Symlinks, keine Temp-Dateien");
        assert!(deleted.is_empty());
        assert_eq!(stats.unchanged, 0);

        // Bekannter, unveränderter Stand -> nichts zu tun; fehlende Datei -> Löschung
        let meta = std::fs::metadata(root.join("a.txt")).unwrap();
        let known = HashMap::from([
            ("a.txt".to_string(), KnownShareFile { id: 1, hash: "h".into(), size: meta.len(), mtime: mtime_of(&meta) }),
            ("weg.txt".to_string(), KnownShareFile { id: 2, hash: "h".into(), size: 1, mtime: 1 }),
            ("anders/x.txt".to_string(), KnownShareFile { id: 3, hash: "h".into(), size: 1, mtime: 1 }),
        ]);
        let (cands, deleted, stats) = walk(&root, None, &known);
        assert_eq!(cands.len(), 1);
        assert_eq!(stats.unchanged, 1);
        let mut d = deleted.clone();
        d.sort();
        assert_eq!(d, vec![2, 3]);
        // Teil-Scan: nur "unter" -> Löschungen außerhalb werden nicht gemeldet
        let (cands, deleted, _) = walk(&root, Some(&["unter".to_string()]), &known);
        assert_eq!(cands.len(), 1);
        assert!(deleted.is_empty());
    }

    #[test]
    fn prefixes_are_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let paths: HashSet<PathBuf> = [root.join("a/b/x.jpg"), root.join("a/y.jpg"), root.join("a/b")].into();
        assert_eq!(affected_prefixes(&root, &paths), Some(vec!["a".to_string()]));
        let paths: HashSet<PathBuf> = [root.join("top.txt")].into();
        assert_eq!(affected_prefixes(&root, &paths), None, "Datei im Wurzelordner -> ganze Freigabe");
    }
}
