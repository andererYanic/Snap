//! Explorer-Operationen: lokale Suche, Ordner live lesen, indexierte Orte verwalten.
//! Alles nur lokal – gekoppelte Geräte haben keinen Zugriff (siehe `auth`).

use crate::api::error::{ApiError, ErrorCode};
use crate::api_bail;
use crate::daemon::*;
use crate::index::{NodeKind, SearchFilter};
use crate::indexer::IndexService;
use crate::ipc::{DirEntryInfo, Response};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Höchstzahl Einträge pro Seite beim Lesen eines Ordners (passt sicher in ein IPC-Frame).
pub const LIST_DIR_MAX: u32 = 1_000;
const QUERY_MAX_LEN: usize = 256;
/// Höchstzahl Suchtreffer pro Anfrage.
pub const SEARCH_MAX: u32 = 200;

fn service(inner: &DaemonInner) -> Result<Arc<IndexService>> {
    match &inner.index {
        Some(s) => Ok(s.clone()),
        None => api_bail!(Unavailable, "Die lokale Suche ist deaktiviert (index_enabled = false)."),
    }
}

pub(crate) async fn search(
    inner: &Arc<DaemonInner>,
    query: String,
    filter: SearchFilter,
    limit: u32,
) -> Result<Response> {
    let svc = service(inner)?;
    if query.len() > QUERY_MAX_LEN {
        api_bail!(InvalidInput, "Suchbegriff ist zu lang (höchstens {QUERY_MAX_LEN} Bytes).");
    }
    let hits = tokio::task::spawn_blocking(move || svc.index.search(&query, &filter, limit.min(SEARCH_MAX) as usize))
        .await??;
    Ok(Response::SearchResults { hits })
}

/// Liest eine Seite eines Ordners direkt vom Dateisystem (immer aktuell, unabhängig vom
/// Index). Zum Sortieren genügen Name und Typ aus `readdir`; Größe und Datum (`stat`) werden
/// nur für die Einträge der angeforderten Seite gelesen. Gibt (Seite, Gesamtzahl) zurück.
pub fn read_dir_page(path: &Path, show_hidden: bool, offset: usize, limit: usize) -> Result<(Vec<DirEntryInfo>, u64)> {
    let (page, total, _) = read_dir_from(path, show_hidden, Start::Offset(offset), limit)?;
    Ok((page, total))
}

/// Wo eine Seite beginnt: nach Position oder hinter einem Eintrag (Cursor `next` der vorigen
/// Seite – stabil, auch wenn sich der Ordner zwischen zwei Seiten ändert).
pub enum Start<'a> {
    Offset(usize),
    After(&'a str),
}

/// Sortierschlüssel: Ordner zuerst, Name ohne Groß-/Kleinschreibung, dann Originalname.
type Key = (bool, String, String);

fn key_of(e: &DirEntryInfo) -> Key {
    (e.kind != NodeKind::Dir, e.name.to_lowercase(), e.name.clone())
}

/// Cursor-Text eines Eintrags: "0/<name>" für Ordner, sonst "1/<name>" ('/' kommt in Namen nie vor).
fn cursor_of(e: &DirEntryInfo) -> String {
    format!("{}/{}", if e.kind == NodeKind::Dir { 0 } else { 1 }, e.name)
}

fn parse_cursor(c: &str) -> Result<Key> {
    let (k, name) = c.split_once('/').ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Cursor"))?;
    if !(k == "0" || k == "1") || name.is_empty() || name.contains('/') || name.len() > 4096 {
        api_bail!(InvalidInput, "Ungültiger Cursor");
    }
    Ok((k == "1", name.to_lowercase(), name.to_string()))
}

/// Ab so vielen Einträgen wird die sortierte Liste eines Ordners kurz zwischengespeichert.
const CACHE_FROM: usize = 5_000;

type Listing = Arc<Vec<(Key, DirEntryInfo)>>;

/// Zuletzt gelesene große Ordner: (Pfad, versteckte?, Änderungszeit des Ordners) → Liste.
/// Ändert sich der Ordnerinhalt, ändert sich seine Änderungszeit – der Eintrag gilt dann nicht mehr.
type CacheKey = (PathBuf, bool, std::time::SystemTime);
static DIR_CACHE: std::sync::Mutex<Vec<(CacheKey, Listing)>> = std::sync::Mutex::new(Vec::new());

/// Vollständige, sortierte Liste eines Ordners (Name und Typ aus `readdir`).
fn listing(path: &Path, show_hidden: bool) -> Result<Listing> {
    let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some(t) = stamp {
        let cache = DIR_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, l)) = cache.iter().find(|(k, _)| k.0 == path && k.1 == show_hidden && k.2 == t) {
            return Ok(l.clone());
        }
    }
    let rd = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            api_bail!(NotFound, "{} existiert nicht.", path.display())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            api_bail!(PermissionDenied, "Keine Berechtigung für {}.", path.display())
        }
        Err(e) => api_bail!(InvalidInput, "{} ist nicht lesbar: {e}", path.display()),
    };
    let mut all = Vec::new();
    for e in rd.flatten() {
        // Namen, die kein gültiges UTF-8 sind, lassen sich über die API nicht eindeutig
        // ansprechen und werden ausgelassen.
        let Ok(name) = e.file_name().into_string() else { continue };
        let hidden = name.starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        // Typ kommt meist direkt aus readdir (d_type), ohne eigenen Systemaufruf.
        let Ok(ft) = e.file_type() else { continue };
        let kind = if ft.is_symlink() {
            NodeKind::Symlink
        } else if ft.is_dir() {
            NodeKind::Dir
        } else {
            NodeKind::File
        };
        let info = DirEntryInfo { name, kind, size: 0, mtime: 0, hidden };
        all.push((key_of(&info), info));
    }
    // Sortierung wie in Dateimanagern: Ordner zuerst, dann Name ohne Groß-/Kleinschreibung;
    // bei Gleichstand entscheidet der Originalname (eindeutige Reihenfolge).
    all.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let listing = Arc::new(all);
    // Nur wenn sich der Ordner während des Lesens nicht geändert hat
    let still = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    // Nicht bei gerade geänderten Ordnern (grobe Zeitstempel, z. B. FAT: 2 s) und nicht bei
    // riesigen (Speicher).
    let settled = stamp.and_then(|t| t.elapsed().ok()).is_some_and(|d| d.as_secs() >= 3);
    let size_ok = (CACHE_FROM..=200_000).contains(&listing.len());
    if let (Some(t), true) = (stamp, size_ok && settled && still == stamp) {
        let mut cache = DIR_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(k, _)| !(k.0 == path && k.1 == show_hidden));
        cache.push(((path.to_path_buf(), show_hidden, t), listing.clone()));
        if cache.len() > 4 {
            cache.remove(0);
        }
    }
    Ok(listing)
}

/// Wie [`read_dir_page`], mit Cursor. Große Ordner (ab 5 000 Einträgen) werden sortiert
/// zwischengespeichert, solange sich ihre Änderungszeit nicht ändert; Folgeseiten kosten dann
/// nur eine Binärsuche. Ergebnis: (Seite, Gesamtzahl, Cursor der nächsten Seite).
pub fn read_dir_from(
    path: &Path,
    show_hidden: bool,
    start: Start<'_>,
    limit: usize,
) -> Result<(Vec<DirEntryInfo>, u64, Option<String>)> {
    let after = match start {
        Start::After(c) => Some(parse_cursor(c)?),
        Start::Offset(_) => None,
    };
    let listing = listing(path, show_hidden)?;
    let total = listing.len() as u64;
    // Sortierte Liste: Beginn per Binärsuche hinter dem Cursor bzw. ab der Position.
    let from = match (&after, start) {
        (Some(a), _) => listing.partition_point(|(k, _)| k <= a),
        (None, Start::Offset(o)) => o.min(listing.len()),
        (None, Start::After(_)) => 0,
    };
    let end = from.saturating_add(limit).min(listing.len());
    let mut page: Vec<DirEntryInfo> = listing[from..end].iter().map(|(_, e)| e.clone()).collect();
    let next = if end < listing.len() { page.last().map(cursor_of) } else { None };
    for e in &mut page {
        // symlink_metadata folgt keinen Symlinks.
        if let Ok(m) = std::fs::symlink_metadata(path.join(&e.name)) {
            if e.kind == NodeKind::File {
                e.size = m.len();
            }
            e.mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
        }
    }
    Ok((page, total, next))
}

pub(crate) async fn list_dir(
    path: PathBuf,
    show_hidden: bool,
    offset: u32,
    limit: u32,
    after: Option<String>,
) -> Result<Response> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let limit = limit.clamp(1, LIST_DIR_MAX) as usize;
    let p = path.clone();
    let (entries, total, next) = tokio::task::spawn_blocking(move || {
        let start = match after.as_deref() {
            Some(c) => Start::After(c),
            None => Start::Offset(offset as usize),
        };
        read_dir_from(&p, show_hidden, start, limit)
    })
    .await??;
    Ok(Response::DirListing { path, entries, total, next })
}

pub(crate) fn index_roots(inner: &DaemonInner) -> Result<Response> {
    let svc = service(inner)?;
    Ok(Response::IndexRoots { roots: svc.index.roots()?, watched_dirs: svc.watched_dirs() })
}

pub(crate) async fn index_add(inner: &Arc<DaemonInner>, path: &Path) -> Result<Response> {
    let svc = service(inner)?;
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    // Symlinks auflösen: indexiert wird der echte Ort.
    let real = match std::fs::canonicalize(path) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => api_bail!(InvalidInput, "{} ist kein Ordner.", path.display()),
        Err(_) => api_bail!(NotFound, "{} existiert nicht.", path.display()),
    };
    if inner.protected_paths().iter().any(|p| real.starts_with(p)) {
        api_bail!(InvalidInput, "Die Snap-Daten werden nicht indexiert.");
    }
    if real.to_str().is_none() {
        api_bail!(InvalidInput, "Pfad ist kein gültiges UTF-8.");
    }
    svc.index.add_root(&real)?;
    svc.rescan();
    index_roots(inner)
}

pub(crate) async fn index_remove(inner: &Arc<DaemonInner>, id: i64) -> Result<Response> {
    let svc = service(inner)?;
    let s = svc.clone();
    if !tokio::task::spawn_blocking(move || s.index.remove_root(id)).await?? {
        api_bail!(NotFound, "Kein indexierter Ort mit ID {id}.");
    }
    *svc.months_cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    index_roots(inner)
}

pub(crate) fn index_rescan(inner: &DaemonInner) -> Result<Response> {
    service(inner)?.rescan();
    Ok(Response::Success { message: "Abgleich der indexierten Orte gestartet.".into() })
}

pub(crate) async fn space_usage(inner: &Arc<DaemonInner>, path: PathBuf, limit: u32) -> Result<Response> {
    let svc = service(inner)?;
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let limit = if limit == 0 { 30 } else { limit as usize };
    let usage = tokio::task::spawn_blocking(move || crate::analysis::space_usage(&svc.index, &path, limit)).await??;
    Ok(Response::SpaceUsage(usage))
}

pub(crate) async fn duplicates(
    inner: &Arc<DaemonInner>,
    path: Option<PathBuf>,
    min_size: Option<u64>,
    limit: u32,
) -> Result<Response> {
    let svc = service(inner)?;
    if let Some(p) = &path {
        if !p.is_absolute() {
            api_bail!(InvalidInput, "Pfad muss absolut sein: {}", p.display());
        }
    }
    let min = min_size.unwrap_or(crate::analysis::DEFAULT_MIN_DUP_SIZE);
    let limit = if limit == 0 { 200 } else { limit as usize };
    let events = inner.db.events().clone();
    let mut report = tokio::task::spawn_blocking(move || {
        let mut last = std::time::Instant::now();
        crate::analysis::duplicates(&svc.index, path.as_deref(), min, limit, |done, total| {
            if last.elapsed() >= std::time::Duration::from_millis(500) {
                last = std::time::Instant::now();
                events.emit(crate::events::Event::DuplicateScanProgress { done_bytes: done, total_bytes: total });
            }
        })
    })
    .await??;
    // Wo liegt derselbe Inhalt noch? (Katalog der gekoppelten Geräte)
    let devices = inner.db.list_devices()?;
    let me = inner.self_id();
    for g in &mut report.groups {
        let mut names: Vec<String> = inner
            .db
            .find_remote_by_hash_prefix(&g.hash, &me)
            .unwrap_or_default()
            .into_iter()
            .map(|f| devices.iter().find(|d| d.node_id == f.origin).map(|d| d.name.clone()).unwrap_or(f.origin))
            .collect();
        names.sort();
        names.dedup();
        g.other_devices = names;
    }
    Ok(Response::Duplicates(report))
}

fn check_scope(path: &Option<PathBuf>) -> Result<()> {
    if let Some(p) = path {
        if !p.is_absolute() {
            api_bail!(InvalidInput, "Pfad muss absolut sein: {}", p.display());
        }
    }
    Ok(())
}

pub(crate) async fn timeline(
    inner: &Arc<DaemonInner>,
    path: Option<PathBuf>,
    before: Option<String>,
    limit: u32,
) -> Result<Response> {
    let svc = service(inner)?;
    check_scope(&path)?;
    let limit = if limit == 0 { 200 } else { limit as usize };
    let page = tokio::task::spawn_blocking(move || {
        crate::photos::timeline(&svc.index, path.as_deref(), before.as_deref(), limit)
    })
    .await??;
    Ok(Response::Timeline(page))
}

pub(crate) async fn timeline_months(inner: &Arc<DaemonInner>, path: Option<PathBuf>) -> Result<Response> {
    let svc = service(inner)?;
    check_scope(&path)?;
    if path.is_none() {
        if let Some(m) = svc.months_cache.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Ok(Response::TimelineMonths { months: m });
        }
    }
    let months = tokio::task::spawn_blocking(move || crate::photos::months(&svc.index, path.as_deref())).await??;
    Ok(Response::TimelineMonths { months })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_is_sorted_and_hides_dotfiles() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["b.txt", "A.txt", ".geheim", "c"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        std::fs::create_dir(dir.path().join("zOrdner")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("link")).unwrap();
        let (page, total) = read_dir_page(dir.path(), false, 0, 100).unwrap();
        let names: Vec<String> = page.into_iter().map(|e| e.name).collect();
        assert_eq!(
            (names.as_slice(), total),
            (["zOrdner", "A.txt", "b.txt", "c", "link"].map(String::from).as_slice(), 5)
        );
        let (all, total) = read_dir_page(dir.path(), true, 0, 100).unwrap();
        assert_eq!((all.len(), total), (6, 6));
        assert_eq!(all.iter().find(|e| e.name == "b.txt").unwrap().size, 1);
        let (p2, _) = read_dir_page(dir.path(), false, 1, 2).unwrap();
        assert_eq!(p2.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["A.txt", "b.txt"]);
        let link = all.iter().find(|e| e.name == "link").unwrap();
        assert_eq!((link.kind, link.size), (NodeKind::Symlink, 0), "Symlink wird nicht verfolgt");
        let err = read_dir_page(&dir.path().join("fehlt"), false, 0, 10).unwrap_err();
        assert_eq!(crate::api::error::code_of(&err), crate::api::error::ErrorCode::NotFound);
    }

    #[test]
    fn cursor_pages_are_complete_ordered_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..57 {
            std::fs::write(dir.path().join(format!("Datei {i:02}.txt")), b"x").unwrap();
        }
        for i in 0..5 {
            std::fs::create_dir(dir.path().join(format!("ordner{i}"))).unwrap();
        }
        let (full, total) = read_dir_page(dir.path(), false, 0, 1000).unwrap();
        assert_eq!(total, 62);
        // Seiten über den Cursor ergeben genau die vollständige, sortierte Liste
        let mut got = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let start = cursor.as_deref().map(Start::After).unwrap_or(Start::Offset(0));
            let (page, t, next) = read_dir_from(dir.path(), false, start, 10).unwrap();
            assert_eq!(t, 62);
            got.extend(page.into_iter().map(|e| e.name));
            match next {
                Some(n) => cursor = Some(n),
                None => break,
            }
        }
        assert_eq!(got, full.iter().map(|e| e.name.clone()).collect::<Vec<_>>());
        assert!(got[..5].iter().all(|n| n.starts_with("ordner")), "Ordner zuerst");
        // Stabil: Entsteht vor dem Cursor eine neue Datei, verschiebt sich die nächste Seite nicht
        let (p1, _, next) = read_dir_from(dir.path(), false, Start::Offset(0), 10).unwrap();
        std::fs::write(dir.path().join("Datei 00a.txt"), b"x").unwrap();
        let (p2, _, _) = read_dir_from(dir.path(), false, Start::After(next.as_deref().unwrap()), 10).unwrap();
        assert!(p2[0].name > p1.last().unwrap().name || p2[0].kind != p1.last().unwrap().kind);
        assert!(!p2.iter().any(|e| p1.iter().any(|f| f.name == e.name)), "keine Doppelten");
        // Ungültige Cursor
        for bad in ["", "2/x", "1/", "0/a/b", "kein-cursor"] {
            let err = read_dir_from(dir.path(), false, Start::After(bad), 10).unwrap_err();
            assert_eq!(crate::api::error::code_of(&err), crate::api::error::ErrorCode::InvalidInput, "{bad}");
        }
    }
}
