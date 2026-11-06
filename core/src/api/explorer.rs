//! Explorer-Operationen: lokale Suche, Ordner live lesen, indexierte Orte verwalten.
//! Alles nur lokal – gekoppelte Geräte haben keinen Zugriff (siehe `auth`).

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

/// Sortierung wie in Dateimanagern: Ordner zuerst, dann Name ohne Groß-/Kleinschreibung;
/// bei Gleichstand entscheidet der Originalname (eindeutige Reihenfolge).
fn sort_entries(entries: Vec<DirEntryInfo>) -> Vec<DirEntryInfo> {
    let mut keyed: Vec<(bool, String, DirEntryInfo)> =
        entries.into_iter().map(|e| (e.kind != NodeKind::Dir, e.name.to_lowercase(), e)).collect();
    keyed.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.name.cmp(&b.2.name)));
    keyed.into_iter().map(|k| k.2).collect()
}

/// Liest eine Seite eines Ordners direkt vom Dateisystem (immer aktuell, unabhängig vom
/// Index). Zum Sortieren genügen Name und Typ aus `readdir`; Größe und Datum (`stat`) werden
/// nur für die Einträge der angeforderten Seite gelesen. Gibt (Seite, Gesamtzahl) zurück.
pub fn read_dir_page(path: &Path, show_hidden: bool, offset: usize, limit: usize) -> Result<(Vec<DirEntryInfo>, u64)> {
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
        all.push(DirEntryInfo { name, kind, size: 0, mtime: 0, hidden });
    }
    let total = all.len() as u64;
    let mut page: Vec<DirEntryInfo> = sort_entries(all).into_iter().skip(offset).take(limit).collect();
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
    Ok((page, total))
}

pub(crate) async fn list_dir(path: PathBuf, show_hidden: bool, offset: u32, limit: u32) -> Result<Response> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let limit = limit.clamp(1, LIST_DIR_MAX) as usize;
    let p = path.clone();
    let (entries, total) =
        tokio::task::spawn_blocking(move || read_dir_page(&p, show_hidden, offset as usize, limit)).await??;
    Ok(Response::DirListing { path, entries, total })
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
}
