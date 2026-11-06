//! Lokaler Dateiindex für die Suche („wie Spotlight“).
//!
//! - Eigene SQLite-Datei (`index.db`): verwerfbar und neu aufbaubar, bremst die
//!   Haupt-Datenbank nicht aus.
//! - Platzsparend: jeder Eintrag speichert nur Namen + Verweis auf den Elternordner;
//!   Pfade werden bei Bedarf über die Elternkette rekonstruiert (keine vollen Pfade pro
//!   Eintrag – genau das kostet Spacedrive Gigabytes).
//! - Suche über einen FTS5-Trigramm-Index auf den Namen (externer Inhalt, keine zweite
//!   Kopie). Teilwörter funktionieren („laub“ findet „Urlaub“).
//! - RAM ist begrenzt: fester SQLite-Cache pro Verbindung, kein Index im Arbeitsspeicher.
//! - Bleibt lokal: Der Index wird nicht an gekoppelte Geräte weitergegeben.

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

/// SQLite-Seitencache je Verbindung (negativ = KiB). Hält den RAM-Bedarf fest begrenzt.
const CACHE_KIB: i64 = 8 * 1024;
const READERS: usize = 2;
/// Einträge pro Schreibtransaktion beim Indexieren.
const BATCH: usize = 20_000;

/// Art eines Eintrags.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
}

impl NodeKind {
    fn code(self) -> i64 {
        match self {
            NodeKind::File => 0,
            NodeKind::Dir => 1,
            NodeKind::Symlink => 2,
        }
    }
    fn from_code(c: i64) -> Self {
        match c {
            1 => NodeKind::Dir,
            2 => NodeKind::Symlink,
            _ => NodeKind::File,
        }
    }
}

/// Ein indexierter Ort.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct IndexRoot {
    pub id: i64,
    pub path: String,
    pub enabled: bool,
    pub last_scan: Option<i64>,
    pub entries: u64,
}

/// Ein Suchtreffer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
    pub mtime: i64,
}

/// Filter für die Suche.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SearchFilter {
    /// Nur diese Art (Datei/Ordner).
    pub kind: Option<NodeKind>,
    /// Nur diese Endungen (klein, ohne Punkt), z. B. ["jpg", "png"].
    #[serde(default)]
    pub extensions: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<i64>,
    /// Versteckte Dateien/Ordner (mit Punkt) einbeziehen.
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    pub excluded_dirs: u64,
}

/// Ordnernamen, die nie indexiert werden (Caches, Abhängigkeiten, Versionsverwaltung).
const EXCLUDED_NAMES: &[&str] = &[
    ".cache",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".venv",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".npm",
    ".yarn",
    ".pnpm-store",
    ".gradle",
    ".m2",
    ".rustup",
    ".Trash",
    ".snapshots",
    "lost+found",
];

/// Pfade relativ zum Home-Ordner, die nie indexiert werden.
const EXCLUDED_HOME_PATHS: &[&str] = &[
    ".local/share/Trash",
    ".cargo/registry",
    ".cargo/git",
    ".local/share/Steam",
    ".var/app",
    ".mozilla",
    ".thunderbird",
];

/// Ordner mit `CACHEDIR.TAG` (Cache-Standard, u. a. von Cargo und pip) werden übersprungen.
fn has_cachedir_tag(dir: &Path) -> bool {
    std::fs::read(dir.join("CACHEDIR.TAG"))
        .map(|b| b.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55"))
        .unwrap_or(false)
}

pub struct LocalIndex {
    writer: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    next: AtomicUsize,
    /// Zusätzlich ausgeschlossene absolute Pfade (z. B. Snap-Daten).
    excluded_paths: Vec<PathBuf>,
}

fn tune(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.set_prepared_statement_cache_capacity(32);
    Ok(())
}

impl LocalIndex {
    pub fn open(path: &Path, excluded_paths: Vec<PathBuf>) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("Index {} nicht zu öffnen", path.display()))?;
        tune(&conn)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS roots (
                id        INTEGER PRIMARY KEY,
                path      TEXT    NOT NULL UNIQUE,
                enabled   INTEGER NOT NULL DEFAULT 1,
                last_scan INTEGER
            );
            CREATE TABLE IF NOT EXISTS nodes (
                id      INTEGER PRIMARY KEY,
                parent  INTEGER,             -- NULL = Wurzel eines Ortes
                root_id INTEGER NOT NULL,
                name    TEXT    NOT NULL,
                kind    INTEGER NOT NULL,    -- 0 Datei, 1 Ordner, 2 Symlink
                size    INTEGER NOT NULL,
                mtime   INTEGER NOT NULL,
                ext     TEXT,                -- klein, ohne Punkt
                hidden  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_nodes_child ON nodes(parent, name);
            CREATE INDEX IF NOT EXISTS idx_nodes_root ON nodes(root_id);
            CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                name, content='nodes', content_rowid='id', tokenize='trigram'
            );
            ",
        )?;
        let mut readers = Vec::new();
        for _ in 0..READERS {
            let r =
                Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            tune(&r)?;
            readers.push(Mutex::new(r));
        }
        Ok(Self { writer: Mutex::new(conn), readers, next: AtomicUsize::new(0), excluded_paths })
    }

    fn w(&self) -> MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn r(&self) -> MutexGuard<'_, Connection> {
        let n = self.readers.len();
        let start = self.next.fetch_add(1, Ordering::Relaxed);
        for i in 0..n {
            if let Ok(g) = self.readers[(start + i) % n].try_lock() {
                return g;
            }
        }
        self.readers[start % n].lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Legt einen Ort an (oder gibt den bestehenden zurück).
    pub fn add_root(&self, path: &Path) -> Result<i64> {
        let p = path.to_str().context("Pfad ist kein gültiges UTF-8")?;
        let conn = self.w();
        conn.execute("INSERT OR IGNORE INTO roots (path) VALUES (?1)", params![p])?;
        Ok(conn.query_row("SELECT id FROM roots WHERE path = ?1", params![p], |r| r.get(0))?)
    }

    pub fn remove_root(&self, id: i64) -> Result<bool> {
        let mut conn = self.w();
        let tx = conn.transaction()?;
        Self::delete_root_nodes(&tx, id)?;
        let n = tx.execute("DELETE FROM roots WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    fn delete_root_nodes(tx: &Connection, root_id: i64) -> Result<()> {
        // Externer FTS-Inhalt: Einträge vor dem Löschen aus dem Suchindex nehmen.
        tx.execute(
            "INSERT INTO nodes_fts(nodes_fts, rowid, name) SELECT 'delete', id, name FROM nodes WHERE root_id = ?1",
            params![root_id],
        )?;
        tx.execute("DELETE FROM nodes WHERE root_id = ?1", params![root_id])?;
        Ok(())
    }

    pub fn roots(&self) -> Result<Vec<IndexRoot>> {
        let conn = self.r();
        let mut stmt = conn.prepare_cached(
            "SELECT r.id, r.path, r.enabled, r.last_scan, (SELECT COUNT(*) FROM nodes n WHERE n.root_id = r.id)
             FROM roots r ORDER BY r.path",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(IndexRoot {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    enabled: r.get(2)?,
                    last_scan: r.get(3)?,
                    entries: r.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn is_excluded(&self, path: &Path, name: &str, home: Option<&Path>) -> bool {
        if EXCLUDED_NAMES.contains(&name) || self.excluded_paths.iter().any(|p| path.starts_with(p)) {
            return true;
        }
        if let Some(home) = home {
            if let Ok(rel) = path.strip_prefix(home) {
                if EXCLUDED_HOME_PATHS.iter().any(|e| rel.starts_with(e)) {
                    return true;
                }
            }
        }
        has_cachedir_tag(path)
    }

    /// Indexiert einen Ort vollständig neu (blockierend – in `spawn_blocking` aufrufen).
    /// Symlinks werden erfasst, aber nie verfolgt.
    pub fn rebuild_root(&self, root_id: i64, root: &Path) -> Result<ScanStats> {
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        let mut stats = ScanStats::default();
        let mut conn = self.w();
        {
            let tx = conn.transaction()?;
            Self::delete_root_nodes(&tx, root_id)?;
            tx.commit()?;
        }
        // Eltern-IDs entlang der aktuellen Tiefe (Tiefensuche von walkdir).
        let mut stack: Vec<i64> = Vec::new();
        let mut tx = conn.transaction()?;
        let mut in_batch = 0usize;
        let walker = walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_entry(|e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !self.is_excluded(e.path(), &name, home.as_deref())
        });
        for entry in walker {
            let Ok(entry) = entry else {
                stats.skipped += 1;
                continue;
            };
            let depth = entry.depth();
            let ft = entry.file_type();
            let kind = if ft.is_symlink() {
                NodeKind::Symlink
            } else if ft.is_dir() {
                NodeKind::Dir
            } else if ft.is_file() {
                NodeKind::File
            } else {
                continue; // Geräte, Sockets, FIFOs
            };
            let name = if depth == 0 {
                root.to_string_lossy().into_owned()
            } else {
                match entry.file_name().to_str() {
                    Some(n) => n.to_string(),
                    None => {
                        stats.skipped += 1;
                        continue;
                    }
                }
            };
            let meta = entry.metadata().ok();
            let size = meta.as_ref().filter(|_| kind == NodeKind::File).map(|m| m.len()).unwrap_or(0);
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let ext = if kind == NodeKind::File {
                Path::new(&name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
            } else {
                None
            };
            stack.truncate(depth);
            let parent = if depth == 0 { None } else { stack.last().copied() };
            tx.prepare_cached(
                "INSERT INTO nodes (parent, root_id, name, kind, size, mtime, ext, hidden)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![
                parent,
                root_id,
                name,
                kind.code(),
                size as i64,
                mtime,
                ext,
                name.starts_with('.')
            ])?;
            let id = tx.last_insert_rowid();
            if kind == NodeKind::Dir {
                stack.push(id);
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
            in_batch += 1;
            if in_batch >= BATCH {
                tx.commit()?;
                tx = conn.transaction()?;
                in_batch = 0;
            }
        }
        tx.execute("UPDATE roots SET last_scan = ?2 WHERE id = ?1", params![root_id, crate::database::now_unix()])?;
        tx.commit()?;
        // Suchindex für diesen Ort in einem Rutsch aufbauen (schneller als pro Zeile).
        conn.execute(
            "INSERT INTO nodes_fts(rowid, name) SELECT id, name FROM nodes WHERE root_id = ?1",
            params![root_id],
        )?;
        Ok(stats)
    }

    /// Test-Hilfe: füllt einen Ort mit synthetischen Einträgen (`dirs` Ordner à `files_per_dir`
    /// Dateien, zwei Ebenen tief), ohne das Dateisystem zu berühren – für Benchmarks mit
    /// Millionen Einträgen.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn insert_synthetic(&self, root_id: i64, dirs: usize, files_per_dir: usize) -> Result<()> {
        const WORDS: &[&str] = &["Urlaub", "Rechnung", "Projekt", "Foto", "Bericht", "Notiz", "Musik", "Vertrag"];
        const EXTS: &[&str] = &["jpg", "pdf", "txt", "mp3", "rs", "docx", "png", "mkv"];
        let mut conn = self.w();
        let tx = conn.transaction()?;
        let ins = "INSERT INTO nodes (parent, root_id, name, kind, size, mtime, ext, hidden)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)";
        tx.prepare_cached(ins)?.execute(params![None::<i64>, root_id, "/synthetisch", 1, 0, 0, None::<String>])?;
        let top = tx.last_insert_rowid();
        let mut group = top;
        for d in 0..dirs {
            if d % 100 == 0 {
                tx.prepare_cached(ins)?.execute(params![
                    top,
                    root_id,
                    format!("Gruppe {}", d / 100),
                    1,
                    0,
                    0,
                    None::<String>
                ])?;
                group = tx.last_insert_rowid();
            }
            let dname = format!("{} {d}", WORDS[d % WORDS.len()]);
            tx.prepare_cached(ins)?.execute(params![group, root_id, dname, 1, 0, 0, None::<String>])?;
            let dir_id = tx.last_insert_rowid();
            for f in 0..files_per_dir {
                let i = d * files_per_dir + f;
                let ext = EXTS[i % EXTS.len()];
                let name = format!("{}-{i:07}.{ext}", WORDS[(i / 3) % WORDS.len()]);
                tx.prepare_cached(ins)?.execute(params![
                    dir_id,
                    root_id,
                    name,
                    0,
                    (i % 10_000_000) as i64,
                    1_600_000_000 + i as i64,
                    ext
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO nodes_fts(rowid, name) SELECT id, name FROM nodes WHERE root_id = ?1",
            params![root_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rekonstruiert den vollständigen Pfad eines Eintrags über die Elternkette.
    fn path_of(conn: &Connection, id: i64, cache: &mut HashMap<i64, String>) -> Result<String> {
        let mut chain: Vec<(i64, String)> = Vec::new();
        let mut cur = Some(id);
        let mut base: Option<String> = None;
        while let Some(i) = cur {
            if let Some(p) = cache.get(&i) {
                base = Some(p.clone());
                break;
            }
            let (parent, name): (Option<i64>, String) = conn
                .prepare_cached("SELECT parent, name FROM nodes WHERE id = ?1")?
                .query_row(params![i], |r| Ok((r.get(0)?, r.get(1)?)))?;
            chain.push((i, name));
            cur = parent;
        }
        let mut path = base.unwrap_or_default();
        for (i, name) in chain.into_iter().rev() {
            path = if path.is_empty() { name } else { format!("{path}/{name}") };
            cache.insert(i, path.clone());
        }
        Ok(path)
    }

    /// Sucht Einträge, deren Name `query` enthält (ohne Groß-/Kleinschreibung).
    /// Ab 3 Zeichen über den Trigramm-Index; kürzere Anfragen per Durchsuchen (begrenzt).
    pub fn search(&self, query: &str, filter: &SearchFilter, limit: usize) -> Result<Vec<SearchHit>> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 500);
        // Breite Anfragen („foto“ bei 100 000 Fotos) nicht komplett sortieren: höchstens `cap`
        // Kandidaten lesen und davon die besten zurückgeben. Hält die Suche im Millisekundenbereich.
        let cap = (limit * 20).max(2_000) as i64;
        let conn = self.r();
        let mut where_sql = String::from("1");
        if let Some(k) = filter.kind {
            where_sql.push_str(&format!(" AND n.kind = {}", k.code()));
        }
        if !filter.include_hidden {
            where_sql.push_str(" AND n.hidden = 0");
        }
        if let Some(v) = filter.min_size {
            where_sql.push_str(&format!(" AND n.size >= {}", v as i64));
        }
        if let Some(v) = filter.max_size {
            where_sql.push_str(&format!(" AND n.size <= {}", v as i64));
        }
        if let Some(v) = filter.modified_after {
            where_sql.push_str(&format!(" AND n.mtime >= {v}"));
        }
        let exts: Vec<String> = filter
            .extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .filter(|e| !e.is_empty() && e.len() <= 16 && e.chars().all(|c| c.is_ascii_alphanumeric()))
            .collect();
        if !exts.is_empty() {
            let list: Vec<String> = exts.iter().map(|e| format!("'{e}'")).collect();
            where_sql.push_str(&format!(" AND n.ext IN ({})", list.join(",")));
        }
        // Nur geprüfte Zahlen/Endungen werden eingesetzt; der Suchbegriff geht als Parameter.
        let rows: Vec<(i64, String, i64, i64, i64)> = if q.chars().count() >= 3 {
            // FTS5-Phrase: Anführungszeichen verdoppeln, damit Sonderzeichen wörtlich gelten.
            let phrase = format!("\"{}\"", q.replace('"', "\"\""));
            let sql = format!(
                "SELECT n.id, n.name, n.kind, n.size, n.mtime FROM nodes_fts f JOIN nodes n ON n.id = f.rowid
                 WHERE nodes_fts MATCH ?1 AND {where_sql} LIMIT ?2"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map(params![phrase, cap], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        } else {
            let pattern = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
            let sql = format!(
                "SELECT n.id, n.name, n.kind, n.size, n.mtime FROM nodes n
                 WHERE n.name LIKE ?1 ESCAPE '\\' AND n.parent IS NOT NULL AND {where_sql} LIMIT ?2"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt
                .query_map(params![pattern, cap], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        // Rangfolge: Namensanfang vor Treffer in der Mitte, dann kürzere Namen.
        let ql = q.to_lowercase();
        let mut rows = rows;
        rows.sort_by_cached_key(|(_, name, ..)| (!name.to_lowercase().starts_with(&ql), name.chars().count()));
        rows.truncate(limit);
        let mut cache = HashMap::new();
        rows.into_iter()
            .map(|(id, name, kind, size, mtime)| {
                Ok(SearchHit {
                    path: Self::path_of(&conn, id, &mut cache)?,
                    name,
                    kind: NodeKind::from_code(kind),
                    size: size as u64,
                    mtime,
                })
            })
            .collect()
    }

    /// Anzahl indexierter Einträge.
    pub fn count(&self) -> Result<u64> {
        Ok(self.r().query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Größe des Ortes mit dieser ID (für Tests/Statistik).
    pub fn root_id(&self, path: &Path) -> Result<Option<i64>> {
        let p = path.to_string_lossy();
        Ok(self.r().query_row("SELECT id FROM roots WHERE path = ?1", params![p], |r| r.get(0)).optional()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home");
        std::fs::create_dir_all(root.join("Bilder/Urlaub 2024")).unwrap();
        std::fs::create_dir_all(root.join("projekt/node_modules/paket")).unwrap();
        std::fs::create_dir_all(root.join("projekt/target/debug")).unwrap();
        std::fs::create_dir_all(root.join(".cache/x")).unwrap();
        std::fs::write(root.join("Bilder/Urlaub 2024/Strand.JPG"), b"jpg").unwrap();
        std::fs::write(root.join("Bilder/notiz.txt"), b"txt").unwrap();
        std::fs::write(root.join("projekt/node_modules/paket/index.js"), b"x").unwrap();
        std::fs::write(root.join("projekt/target/CACHEDIR.TAG"), b"Signature: 8a477f597d28d172789f06886806bc55\n")
            .unwrap();
        std::fs::write(root.join("projekt/target/debug/urlaub-build.bin"), b"x").unwrap();
        std::fs::write(root.join(".cache/x/urlaub-cache"), b"x").unwrap();
        std::fs::write(root.join(".versteckt-urlaub"), b"x").unwrap();
        std::os::unix::fs::symlink("/etc", root.join("link-nach-etc")).unwrap();
        (dir, root)
    }

    #[test]
    fn index_search_and_exclusions() {
        let (dir, root) = tree();
        let idx = LocalIndex::open(&dir.path().join("index.db"), vec![]).unwrap();
        let id = idx.add_root(&root).unwrap();
        let st = idx.rebuild_root(id, &root).unwrap();
        assert!(st.files >= 3);

        // Teilwort, ohne Groß-/Kleinschreibung, mit vollem Pfad
        let hits = idx.search("laub", &SearchFilter::default(), 50).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.iter().any(|p| p.ends_with("Bilder/Urlaub 2024")), "{paths:?}");
        // Ausschlüsse: node_modules, CACHEDIR.TAG (target), .cache
        assert!(!paths.iter().any(|p| p.contains("target") || p.contains(".cache")), "{paths:?}");
        assert!(idx.search("index.js", &SearchFilter::default(), 50).unwrap().is_empty());
        // Versteckte nur auf Wunsch
        assert!(!paths.iter().any(|p| p.contains(".versteckt")));
        let with_hidden = idx.search("laub", &SearchFilter { include_hidden: true, ..Default::default() }, 50).unwrap();
        assert!(with_hidden.iter().any(|h| h.name == ".versteckt-urlaub"));
        // Filter nach Endung (klein) und Art
        let jpg =
            idx.search("strand", &SearchFilter { extensions: vec!["jpg".into()], ..Default::default() }, 50).unwrap();
        assert_eq!(jpg.len(), 1);
        assert!(jpg[0].path.ends_with("Urlaub 2024/Strand.JPG"));
        let dirs = idx.search("urlaub", &SearchFilter { kind: Some(NodeKind::Dir), ..Default::default() }, 50).unwrap();
        assert!(dirs.iter().all(|h| h.kind == NodeKind::Dir));
        // Symlink erfasst, aber nicht verfolgt (kein /etc-Inhalt im Index)
        assert!(idx.search("passwd", &SearchFilter::default(), 50).unwrap().is_empty());
        assert_eq!(idx.search("link-nach", &SearchFilter::default(), 50).unwrap()[0].kind, NodeKind::Symlink);
        // Kurze Anfragen und Sonderzeichen
        assert!(!idx.search("no", &SearchFilter::default(), 50).unwrap().is_empty());
        assert!(idx.search("%", &SearchFilter::default(), 50).unwrap().is_empty());
        assert!(idx.search("\"; DROP TABLE nodes; --", &SearchFilter::default(), 50).unwrap().is_empty());
        assert!(idx.count().unwrap() > 0);

        // Neu indexieren ersetzt; Entfernen leert
        std::fs::remove_file(root.join("Bilder/notiz.txt")).unwrap();
        idx.rebuild_root(id, &root).unwrap();
        assert!(idx.search("notiz", &SearchFilter::default(), 50).unwrap().is_empty());
        assert!(idx.remove_root(id).unwrap());
        assert_eq!(idx.count().unwrap(), 0);
        assert!(idx.search("strand", &SearchFilter::default(), 50).unwrap().is_empty());
    }
}
