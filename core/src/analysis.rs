//! Speicheranalyse und Duplikatsuche auf Basis des lokalen Index.
//!
//! - **Speicheranalyse:** Größe je Unterordner, Anteile nach Dateityp, größte Dateien –
//!   ein Durchlauf über den Teilbaum im Index (kein Dateisystem-Scan).
//! - **Duplikate:** Kandidaten gleicher Größe aus dem Index → Hash der ersten 64 KiB →
//!   vollständiger BLAKE3-Hash. Hashes werden zwischengespeichert (gültig, solange Größe und
//!   mtime passen). Zusätzlich: Kopien desselben Inhalts auf gekoppelten Geräten.
//! - Nur lesend; gelöscht wird ausschließlich über die normalen Dateioperationen (Papierkorb).

use crate::api_bail;
use crate::index::LocalIndex;
use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{BinaryHeap, HashMap};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Ab dieser Größe werden Dateien als Duplikat-Kandidaten betrachtet (Standard).
pub const DEFAULT_MIN_DUP_SIZE: u64 = 1024 * 1024;
const HEAD: usize = 64 * 1024;

const SUBTREE: &str = "WITH RECURSIVE sub(id, top) AS (
        SELECT id, id FROM nodes WHERE parent = ?1
        UNION ALL
        SELECT n.id, sub.top FROM nodes n JOIN sub ON n.parent = sub.id
    )";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UsageEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub files: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TypeUsage {
    pub category: String,
    pub size: u64,
    pub files: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LargeFile {
    pub path: String,
    pub size: u64,
}

/// Ergebnis der Speicheranalyse für einen Ordner.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SpaceUsage {
    pub path: String,
    pub total: u64,
    pub files: u64,
    /// Direkte Einträge, größte zuerst.
    pub children: Vec<UsageEntry>,
    pub by_type: Vec<TypeUsage>,
    pub largest: Vec<LargeFile>,
}

/// Kategorie einer Dateiendung (für die Übersicht).
pub fn category(ext: Option<&str>) -> &'static str {
    match ext.unwrap_or("") {
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "m4v" | "wmv" | "3gp" | "mpg" | "mpeg" => "Videos",
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng"
        | "tif" | "tiff" | "bmp" | "svg" => "Bilder",
        "mp3" | "flac" | "m4a" | "ogg" | "oga" | "opus" | "wav" | "aac" | "wma" | "aiff" => "Musik",
        "pdf" | "doc" | "docx" | "odt" | "txt" | "md" | "rtf" | "xls" | "xlsx" | "ods" | "csv" | "ppt" | "pptx"
        | "odp" | "epub" => "Dokumente",
        "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "xz" | "zst" | "bz2" | "iso" | "dmg" => "Archive",
        "rs" | "py" | "js" | "ts" | "c" | "h" | "cpp" | "go" | "java" | "kt" | "swift" | "sh" | "json" | "toml"
        | "yaml" | "yml" | "html" | "css" => "Programmcode",
        _ => "Sonstiges",
    }
}

/// Speicheranalyse eines indexierten Ordners (blockierend).
pub fn space_usage(index: &LocalIndex, path: &Path, limit: usize) -> Result<SpaceUsage> {
    let Some(dir) = index.find_dir(path)? else {
        api_bail!(NotFound, "{} ist kein indexierter Ordner (index add …).", path.display());
    };
    let limit = limit.clamp(1, 500);
    let conn = index.reader();
    // Direkte Kinder (Namen/Art)
    let mut children: HashMap<i64, UsageEntry> = HashMap::new();
    {
        let mut stmt = conn.prepare_cached("SELECT id, name, kind, size FROM nodes WHERE parent = ?1")?;
        let rows = stmt.query_map(params![dir], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?))
        })?;
        for row in rows {
            let (id, name, kind, _) = row?;
            children.insert(id, UsageEntry { name, is_dir: kind == 1, size: 0, files: 0 });
        }
    }
    // Erst nur die Ordner des Teilbaums (Teilindex über Ordner), dann die Dateien je Ordner.
    // Schneller als eine Rekursion über alle Einträge.
    let mut dirs: Vec<(i64, i64)> = vec![(dir, 0)]; // (Ordner, oberster Eintrag; 0 = der Ordner selbst)
    {
        let mut stmt = conn.prepare_cached(
            "WITH RECURSIVE d(id, top) AS (
                SELECT id, id FROM nodes WHERE parent = ?1 AND kind = 1
                UNION ALL
                SELECT n.id, d.top FROM nodes n JOIN d ON n.parent = d.id WHERE n.kind = 1
            ) SELECT id, top FROM d",
        )?;
        let rows = stmt.query_map(params![dir], |r| Ok((r.get(0)?, r.get(1)?)))?;
        for row in rows {
            dirs.push(row?);
        }
    }
    let mut by_type: HashMap<&'static str, (u64, u64)> = HashMap::new();
    let mut heap: BinaryHeap<std::cmp::Reverse<(u64, i64)>> = BinaryHeap::new();
    let (mut total, mut files) = (0u64, 0u64);
    {
        let mut stmt = conn.prepare_cached("SELECT id, size, ext FROM nodes WHERE parent = ?1 AND kind = 0")?;
        for &(d, top) in &dirs {
            let mut rows = stmt.query(params![d])?;
            while let Some(r) = rows.next()? {
                let (id, size): (i64, i64) = (r.get(0)?, r.get(1)?);
                let size = size.max(0) as u64;
                let ext: Option<String> = r.get(2)?;
                total += size;
                files += 1;
                // Dateien direkt im Ordner sind selbst „oberste Einträge“.
                let key = if top == 0 { id } else { top };
                if let Some(c) = children.get_mut(&key) {
                    c.size += size;
                    c.files += 1;
                }
                let e = by_type.entry(category(ext.as_deref())).or_default();
                e.0 += size;
                e.1 += 1;
                if heap.len() < 20 || heap.peek().is_some_and(|m| m.0 .0 < size) {
                    heap.push(std::cmp::Reverse((size, id)));
                    if heap.len() > 20 {
                        heap.pop();
                    }
                }
            }
        }
    }
    let mut cache = HashMap::new();
    let mut largest: Vec<LargeFile> = heap
        .into_iter()
        .filter_map(|std::cmp::Reverse((size, id))| {
            LocalIndex::path_of(&conn, id, &mut cache).ok().map(|path| LargeFile { path, size })
        })
        .collect();
    largest.sort_by_key(|f| std::cmp::Reverse(f.size));
    let mut children: Vec<UsageEntry> = children.into_values().collect();
    children.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    children.truncate(limit);
    let mut by_type: Vec<TypeUsage> =
        by_type.into_iter().map(|(c, (size, files))| TypeUsage { category: c.into(), size, files }).collect();
    by_type.sort_by_key(|t| std::cmp::Reverse(t.size));
    Ok(SpaceUsage { path: path.to_string_lossy().into_owned(), total, files, children, by_type, largest })
}

/// Eine Gruppe identischer Dateien.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub hash: String,
    pub size: u64,
    pub paths: Vec<String>,
    /// Platz, der durch Entfernen aller Kopien bis auf eine frei würde.
    pub wasted: u64,
    /// Geräte, die denselben Inhalt ebenfalls freigeben (aus dem Katalog).
    #[serde(default)]
    pub other_devices: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub wasted: u64,
    pub candidates: u64,
    /// Gelesene Bytes (ohne Cache-Treffer).
    pub hashed_bytes: u64,
}

fn read_hash(path: &Path, head_only: bool) -> Option<String> {
    let mut f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path).ok()?;
    let mut h = blake3::Hasher::new();
    if head_only {
        let mut buf = vec![0u8; HEAD];
        let mut n = 0;
        while n < HEAD {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => return None,
            }
        }
        h.update(&buf[..n]);
    } else {
        h.update_reader(&mut f).ok()?;
    }
    Some(h.finalize().to_hex().to_string())
}

/// Sucht doppelte Dateien (blockierend). `scope = None`: alle indexierten Orte.
/// `progress(gelesen, gesamt)` wird gelegentlich aufgerufen.
pub fn duplicates(
    index: &LocalIndex,
    scope: Option<&Path>,
    min_size: u64,
    limit: usize,
    mut progress: impl FnMut(u64, u64),
) -> Result<DuplicateReport> {
    let min_size = min_size.max(1);
    // Kandidaten: Dateien, deren Größe mehrfach vorkommt.
    let mut by_size: HashMap<u64, Vec<(i64, i64)>> = HashMap::new();
    {
        let conn = index.reader();
        let mut collect = |rows: &mut rusqlite::Rows<'_>| -> Result<()> {
            while let Some(r) = rows.next()? {
                let (id, size, mtime): (i64, i64, i64) = (r.get(0)?, r.get(1)?, r.get(2)?);
                by_size.entry(size as u64).or_default().push((id, mtime));
            }
            Ok(())
        };
        match scope {
            Some(p) => {
                let Some(dir) = index.find_dir(p)? else {
                    api_bail!(NotFound, "{} ist kein indexierter Ordner.", p.display());
                };
                let mut stmt = conn.prepare(&format!(
                    "{SUBTREE} SELECT n.id, n.size, n.mtime FROM sub JOIN nodes n ON n.id = sub.id
                     WHERE n.kind = 0 AND n.size >= ?2"
                ))?;
                collect(&mut stmt.query(params![dir, min_size as i64])?)?;
            }
            None => {
                let mut stmt = conn.prepare(
                    "SELECT id, size, mtime FROM nodes WHERE kind = 0 AND size >= ?1 AND size IN (
                        SELECT size FROM nodes WHERE kind = 0 AND size >= ?1 GROUP BY size HAVING COUNT(*) > 1)",
                )?;
                collect(&mut stmt.query(params![min_size as i64])?)?;
            }
        }
    }
    by_size.retain(|_, v| v.len() > 1);
    let candidates: u64 = by_size.values().map(|v| v.len() as u64).sum();
    let total_bytes: u64 = by_size.iter().map(|(s, v)| s * v.len() as u64).sum();
    let mut report = DuplicateReport { candidates, ..Default::default() };
    let mut done_bytes = 0u64;
    let mut path_cache = HashMap::new();

    // Hash (Kopf oder ganz) mit Cache.
    let mut hash_of =
        |id: i64, size: u64, mtime: i64, head: bool, report: &mut DuplicateReport| -> Option<(String, String)> {
            let conn = index.reader();
            let cached: Option<(i64, i64, Option<String>, Option<String>)> = conn
                .prepare_cached("SELECT size, mtime, head, full FROM hashes WHERE node_id = ?1")
                .ok()?
                .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .optional()
                .ok()?;
            let path = LocalIndex::path_of(&conn, id, &mut path_cache).ok()?;
            drop(conn);
            let valid = cached.as_ref().filter(|c| c.0 as u64 == size && c.1 == mtime);
            let have = valid.and_then(|c| if head { c.2.clone() } else { c.3.clone() });
            let value = match have {
                Some(v) => v,
                None => {
                    let v = read_hash(Path::new(&path), head)?;
                    report.hashed_bytes += if head { size.min(HEAD as u64) } else { size };
                    let w = index.writer();
                    let col = if head { "head" } else { "full" };
                    let _ = w.execute(
                        &format!(
                            "INSERT INTO hashes (node_id, size, mtime, {col}) VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(node_id) DO UPDATE SET {col} = excluded.{col},
                           head = CASE WHEN hashes.size = excluded.size AND hashes.mtime = excluded.mtime
                                       THEN COALESCE(excluded.head, hashes.head) ELSE excluded.head END,
                           full = CASE WHEN hashes.size = excluded.size AND hashes.mtime = excluded.mtime
                                       THEN COALESCE(excluded.full, hashes.full) ELSE excluded.full END,
                           size = excluded.size, mtime = excluded.mtime"
                        ),
                        params![id, size as i64, mtime, v],
                    );
                    v
                }
            };
            Some((value, path))
        };

    let mut sizes: Vec<u64> = by_size.keys().copied().collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    for size in sizes {
        let files = &by_size[&size];
        // Stufe 1: Kopf (bei kleinen Dateien = ganzer Inhalt)
        let mut by_head: HashMap<String, Vec<(i64, i64, String)>> = HashMap::new();
        for &(id, mtime) in files {
            if let Some((h, path)) = hash_of(id, size, mtime, true, &mut report) {
                by_head.entry(h).or_default().push((id, mtime, path));
            }
        }
        // Stufe 2: vollständiger Hash nur, wo der Kopf übereinstimmt
        for (head, group) in by_head.into_iter().filter(|(_, g)| g.len() > 1) {
            let mut by_full: HashMap<String, Vec<String>> = HashMap::new();
            if size <= HEAD as u64 {
                by_full.insert(head, group.into_iter().map(|g| g.2).collect());
            } else {
                for (id, mtime, _) in group {
                    if let Some((h, path)) = hash_of(id, size, mtime, false, &mut report) {
                        by_full.entry(h).or_default().push(path);
                    }
                }
            }
            for (hash, mut paths) in by_full.into_iter().filter(|(_, p)| p.len() > 1) {
                paths.sort();
                let wasted = size * (paths.len() as u64 - 1);
                report.wasted += wasted;
                report.groups.push(DuplicateGroup { hash, size, paths, wasted, other_devices: Vec::new() });
            }
        }
        done_bytes += size * files.len() as u64;
        progress(done_bytes, total_bytes);
    }
    report.groups.sort_by(|a, b| b.wasted.cmp(&a.wasted).then_with(|| a.hash.cmp(&b.hash)));
    report.groups.truncate(limit.clamp(1, 10_000));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn setup() -> (tempfile::TempDir, PathBuf, LocalIndex, i64) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home");
        std::fs::create_dir_all(root.join("Videos")).unwrap();
        std::fs::create_dir_all(root.join("Fotos/2024")).unwrap();
        std::fs::create_dir_all(root.join("Backup")).unwrap();
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(root.join("Videos/film.mp4"), &big).unwrap();
        std::fs::write(root.join("Backup/film-kopie.mp4"), &big).unwrap();
        // Gleiche Größe und gleicher Anfang, aber anderes Ende → kein Duplikat
        let mut near = big.clone();
        *near.last_mut().unwrap() ^= 1;
        std::fs::write(root.join("Backup/fast-gleich.mp4"), &near).unwrap();
        std::fs::write(root.join("Fotos/2024/a.jpg"), vec![1u8; 2000]).unwrap();
        std::fs::write(root.join("Fotos/a-kopie.jpg"), vec![1u8; 2000]).unwrap();
        std::fs::write(root.join("notiz.txt"), b"hallo").unwrap();
        let idx = LocalIndex::open(&dir.path().join("index.db"), vec![]).unwrap();
        let id = idx.add_root(&root).unwrap();
        idx.rebuild_root(id, &root).unwrap();
        (dir, root, idx, id)
    }

    #[test]
    fn usage_sums_folders_types_and_largest() {
        let (_d, root, idx, _) = setup();
        let u = space_usage(&idx, &root, 10).unwrap();
        assert_eq!(u.total, 3 * 300_000 + 2 * 2000 + 5);
        assert_eq!(u.files, 6);
        assert_eq!(u.children[0].name, "Backup");
        assert_eq!(u.children[0].size, 600_000);
        assert_eq!(u.children[0].files, 2);
        let fotos = u.children.iter().find(|c| c.name == "Fotos").unwrap();
        assert_eq!((fotos.size, fotos.files, fotos.is_dir), (4000, 2, true));
        assert_eq!(u.by_type[0].category, "Videos");
        assert_eq!(u.by_type[0].size, 900_000);
        assert_eq!(u.largest.len(), 6);
        assert!(u.largest[0].path.ends_with(".mp4"));
        // Unterordner
        let sub = space_usage(&idx, &root.join("Fotos"), 10).unwrap();
        assert_eq!(sub.total, 4000);
        assert!(space_usage(&idx, &root.join("fehlt"), 10).is_err());
        assert!(space_usage(&idx, Path::new("/nicht/indexiert"), 10).is_err());
    }

    #[test]
    fn duplicates_are_exact_and_cached() {
        let (_d, root, idx, _) = setup();
        let r = duplicates(&idx, None, 1000, 100, |_, _| {}).unwrap();
        assert_eq!(r.groups.len(), 2, "{r:?}");
        let film = &r.groups[0];
        assert_eq!(film.size, 300_000);
        assert_eq!(film.wasted, 300_000);
        assert_eq!(film.paths.len(), 2);
        assert!(film.paths.iter().all(|p| !p.contains("fast-gleich")), "gleicher Anfang reicht nicht");
        assert_eq!(film.hash, blake3::hash(&std::fs::read(root.join("Videos/film.mp4")).unwrap()).to_hex().to_string());
        assert_eq!(r.groups[1].size, 2000);
        assert_eq!(r.wasted, 302_000);
        let first = r.hashed_bytes;
        assert!(first > 0);
        // Zweiter Lauf: alles aus dem Cache
        let r2 = duplicates(&idx, None, 1000, 100, |_, _| {}).unwrap();
        assert_eq!(r2.groups, r.groups);
        assert_eq!(r2.hashed_bytes, 0, "Hashes aus dem Cache");
        // Mindestgröße und Bereich
        assert_eq!(duplicates(&idx, None, 10_000, 100, |_, _| {}).unwrap().groups.len(), 1);
        let only_fotos = duplicates(&idx, Some(&root.join("Fotos")), 1, 100, |_, _| {}).unwrap();
        assert_eq!(only_fotos.groups.len(), 1);
        assert_eq!(only_fotos.groups[0].size, 2000);
    }
}
