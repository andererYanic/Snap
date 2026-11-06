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
    home: Option<PathBuf>,
}

/// Ergebnis eines inkrementellen Abgleichs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefreshStats {
    pub added: u64,
    pub removed: u64,
    pub updated: u64,
    /// Neu eingelesene Ordner (geänderte mtime oder erzwungen).
    pub dirs_read: u64,
    /// Ordner mit unveränderter mtime (nur `stat`, kein Einlesen).
    pub dirs_unchanged: u64,
    /// Neu hinzugekommene Ordner (für den Watcher), höchstens [`MAX_NEW_DIRS`].
    pub new_dirs: Vec<PathBuf>,
}

/// Obergrenze für gemeldete neue Ordner pro Abgleich (Speicher bei riesigen Kopien).
pub const MAX_NEW_DIRS: usize = 10_000;

const INSERT_NODE: &str = "INSERT INTO nodes (parent, root_id, name, kind, size, mtime, ext, hidden, stamp)
                           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";

fn tune(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", -CACHE_KIB)?;
    conn.pragma_update(None, "temp_store", "FILE")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.set_prepared_statement_cache_capacity(32);
    Ok(())
}

/// Art eines Verzeichniseintrags; Geräte, Sockets und FIFOs werden nicht erfasst.
fn classify(ft: &std::fs::FileType) -> Option<NodeKind> {
    if ft.is_symlink() {
        Some(NodeKind::Symlink)
    } else if ft.is_dir() {
        Some(NodeKind::Dir)
    } else if ft.is_file() {
        Some(NodeKind::File)
    } else {
        None
    }
}

/// (Größe, mtime in s, Stempel in ns). Der Stempel (nur Ordner) erkennt auch Änderungen
/// innerhalb derselben Sekunde.
fn meta_values(meta: Option<&std::fs::Metadata>, kind: NodeKind) -> (u64, i64, i64) {
    let Some(m) = meta else { return (0, 0, 0) };
    let size = if kind == NodeKind::File { m.len() } else { 0 };
    let d = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).unwrap_or_default();
    let stamp = if kind == NodeKind::Dir { d.as_nanos().min(i64::MAX as u128) as i64 } else { 0 };
    (size, d.as_secs() as i64, stamp)
}

fn ext_of(name: &str, kind: NodeKind) -> Option<String> {
    if kind != NodeKind::File {
        return None;
    }
    Path::new(name).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

/// Bricht eine offene Schreibtransaktion ab, wenn sie nicht ordentlich beendet wurde.
struct TxGuard<'a> {
    conn: &'a Connection,
    done: bool,
}

impl<'a> TxGuard<'a> {
    fn begin(conn: &'a Connection) -> Result<Self> {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        Ok(Self { conn, done: false })
    }
    /// Zwischenstand festschreiben (große Abgleiche halten das WAL klein).
    fn checkpoint(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT; BEGIN IMMEDIATE")?;
        Ok(())
    }
    fn commit(mut self) -> Result<()> {
        self.done = true;
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }
}

impl Drop for TxGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

/// Zähler für Einfügungen innerhalb einer Transaktion.
struct Writer<'a> {
    tx: &'a TxGuard<'a>,
    root_id: i64,
    /// Suchindex sofort pflegen (inkrementell) oder am Ende gesammelt (Neuaufbau).
    fts_inline: bool,
    in_batch: usize,
    new_dirs: Vec<PathBuf>,
}

impl Writer<'_> {
    fn conn(&self) -> &Connection {
        self.tx.conn
    }

    fn tick(&mut self) -> Result<()> {
        self.in_batch += 1;
        if self.in_batch >= BATCH {
            self.tx.checkpoint()?;
            self.in_batch = 0;
        }
        Ok(())
    }

    fn insert(
        &mut self,
        parent: Option<i64>,
        name: &str,
        kind: NodeKind,
        meta: Option<&std::fs::Metadata>,
    ) -> Result<i64> {
        let (size, mtime, stamp) = meta_values(meta, kind);
        let conn = self.conn();
        conn.prepare_cached(INSERT_NODE)?.execute(params![
            parent,
            self.root_id,
            name,
            kind.code(),
            size as i64,
            mtime,
            ext_of(name, kind),
            name.starts_with('.'),
            stamp
        ])?;
        let id = conn.last_insert_rowid();
        // Wurzeln (voller Pfad als Name) kommen nicht in den Suchindex.
        if self.fts_inline && parent.is_some() {
            conn.prepare_cached("INSERT INTO nodes_fts(rowid, name) VALUES (?1, ?2)")?.execute(params![id, name])?;
        }
        self.tick()?;
        Ok(id)
    }

    /// Entfernt einen Eintrag samt allem darunter (auch aus dem Suchindex).
    fn delete_subtree(&mut self, id: i64) -> Result<u64> {
        let conn = self.conn();
        const SUB: &str =
            "WITH RECURSIVE sub(id) AS (SELECT ?1 UNION ALL SELECT n.id FROM nodes n JOIN sub ON n.parent = sub.id)";
        // Externer FTS-Inhalt: nur Zeilen löschen, die auch eingetragen wurden (keine Wurzeln).
        conn.prepare_cached(&format!(
            "{SUB} INSERT INTO nodes_fts(nodes_fts, rowid, name)
             SELECT 'delete', n.id, n.name FROM nodes n WHERE n.id IN sub AND n.parent IS NOT NULL"
        ))?
        .execute(params![id])?;
        conn.prepare_cached(&format!("{SUB} DELETE FROM hashes WHERE node_id IN sub"))?.execute(params![id])?;
        conn.prepare_cached(&format!("{SUB} DELETE FROM media_meta WHERE node_id IN sub"))?.execute(params![id])?;
        let n = conn.prepare_cached(&format!("{SUB} DELETE FROM nodes WHERE id IN sub"))?.execute(params![id])?;
        self.tick()?;
        Ok(n as u64)
    }
}

impl LocalIndex {
    pub fn open(path: &Path, excluded_paths: Vec<PathBuf>) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("Index {} nicht zu öffnen", path.display()))?;
        tune(&conn)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS roots (
                id        INTEGER PRIMARY KEY,
                path      TEXT    NOT NULL UNIQUE,
                enabled   INTEGER NOT NULL DEFAULT 1,
                last_scan INTEGER
            );
            CREATE TABLE IF NOT EXISTS nodes (
                id      INTEGER PRIMARY KEY,
                parent  INTEGER,             -- NULL = Wurzel eines Ortes (Name = voller Pfad)
                root_id INTEGER NOT NULL,
                name    TEXT    NOT NULL,
                kind    INTEGER NOT NULL,    -- 0 Datei, 1 Ordner, 2 Symlink
                size    INTEGER NOT NULL,
                mtime   INTEGER NOT NULL,    -- Sekunden
                ext     TEXT,                -- klein, ohne Punkt
                hidden  INTEGER NOT NULL,
                stamp   INTEGER NOT NULL DEFAULT 0  -- Ordner: mtime in ns (Änderungserkennung)
            );
            CREATE INDEX IF NOT EXISTS idx_nodes_child ON nodes(parent, name);
            CREATE INDEX IF NOT EXISTS idx_nodes_root ON nodes(root_id, parent);
            -- Nur Ordner: Abstieg beim Abgleich ohne die (vielen) Dateien zu lesen.
            CREATE INDEX IF NOT EXISTS idx_nodes_dirs ON nodes(parent, stamp) WHERE kind = 1;
            -- Inhalts-Hashes für die Duplikatsuche (gültig, solange Größe und mtime passen).
            CREATE TABLE IF NOT EXISTS hashes (
                node_id INTEGER PRIMARY KEY,
                size    INTEGER NOT NULL,
                mtime   INTEGER NOT NULL,
                head    TEXT,
                full    TEXT
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                name, content='nodes', content_rowid='id', tokenize='trigram'
            );
            ",
        )?;
        conn.execute_batch(&crate::photos::schema())?;
        let mut readers = Vec::new();
        for _ in 0..READERS {
            let r =
                Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
            tune(&r)?;
            readers.push(Mutex::new(r));
        }
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        Ok(Self { writer: Mutex::new(conn), readers, next: AtomicUsize::new(0), excluded_paths, home })
    }

    /// Lesende Verbindung (für Auswertungen wie Speicheranalyse und Duplikate).
    pub(crate) fn reader(&self) -> MutexGuard<'_, Connection> {
        self.r()
    }

    pub(crate) fn writer(&self) -> MutexGuard<'_, Connection> {
        self.w()
    }

    /// Knoten eines indexierten Ordners (`None`, wenn der Pfad in keinem Ort liegt oder
    /// (noch) nicht indexiert ist).
    pub fn find_dir(&self, path: &Path) -> Result<Option<i64>> {
        let conn = self.r();
        // Nur Pfade der Orte – ohne die Zählung aus `roots()` (bei 1 Mio. Einträgen ~30 ms).
        let roots: Vec<(i64, String)> = conn
            .prepare_cached("SELECT id, path FROM roots WHERE enabled = 1")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let Some((root_id, root_path)) =
            roots.into_iter().filter(|(_, p)| path.starts_with(p)).max_by_key(|(_, p)| p.len())
        else {
            return Ok(None);
        };
        let mut node: Option<i64> = conn
            .prepare_cached("SELECT id FROM nodes WHERE root_id = ?1 AND parent IS NULL")?
            .query_row(params![root_id], |r| r.get(0))
            .optional()?;
        let rel = path.strip_prefix(&root_path).unwrap_or(Path::new(""));
        for comp in rel.components() {
            let (Some(n), Some(name)) = (node, comp.as_os_str().to_str()) else { return Ok(None) };
            node = conn
                .prepare_cached("SELECT id FROM nodes WHERE parent = ?1 AND name = ?2 AND kind = 1")?
                .query_row(params![n, name], |r| r.get(0))
                .optional()?;
        }
        Ok(node)
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

    /// Legt einen Ort an oder aktiviert ihn wieder (ausdrücklicher Wunsch des Nutzers).
    pub fn add_root(&self, path: &Path) -> Result<i64> {
        let p = path.to_str().context("Pfad ist kein gültiges UTF-8")?;
        let conn = self.w();
        conn.execute("INSERT INTO roots (path) VALUES (?1) ON CONFLICT(path) DO UPDATE SET enabled = 1", params![p])?;
        Ok(conn.query_row("SELECT id FROM roots WHERE path = ?1", params![p], |r| r.get(0))?)
    }

    /// Legt einen automatisch erkannten Ort an (z. B. USB-Laufwerk). Hat der Nutzer ihn
    /// früher entfernt, bleibt er aus (`None`).
    pub fn add_root_auto(&self, path: &Path) -> Result<Option<i64>> {
        let Some(p) = path.to_str() else { return Ok(None) };
        let conn = self.w();
        conn.execute("INSERT OR IGNORE INTO roots (path) VALUES (?1)", params![p])?;
        Ok(conn
            .query_row("SELECT id FROM roots WHERE path = ?1 AND enabled = 1", params![p], |r| r.get(0))
            .optional()?)
    }

    /// Entfernt einen Ort aus der Suche. Er bleibt als „deaktiviert“ vermerkt, damit
    /// automatisch erkannte Laufwerke nicht wiederkommen.
    pub fn remove_root(&self, id: i64) -> Result<bool> {
        let conn = self.w();
        let tx = TxGuard::begin(&conn)?;
        Self::delete_root_nodes(&conn, id)?;
        let n = conn.execute("UPDATE roots SET enabled = 0, last_scan = NULL WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    fn delete_root_nodes(conn: &Connection, root_id: i64) -> Result<()> {
        // Externer FTS-Inhalt: Einträge vor dem Löschen aus dem Suchindex nehmen.
        conn.execute(
            "INSERT INTO nodes_fts(nodes_fts, rowid, name)
             SELECT 'delete', id, name FROM nodes WHERE root_id = ?1 AND parent IS NOT NULL",
            params![root_id],
        )?;
        conn.execute(
            "DELETE FROM hashes WHERE node_id IN (SELECT id FROM nodes WHERE root_id = ?1)",
            params![root_id],
        )?;
        conn.execute(
            "DELETE FROM media_meta WHERE node_id IN (SELECT id FROM nodes WHERE root_id = ?1)",
            params![root_id],
        )?;
        conn.execute("DELETE FROM nodes WHERE root_id = ?1", params![root_id])?;
        Ok(())
    }

    /// Wurde der Index schon einmal mit Standard-Orten befüllt?
    pub fn seeded(&self) -> Result<bool> {
        Ok(self.r().query_row("SELECT 1 FROM meta WHERE key = 'seeded'", [], |_| Ok(())).optional()?.is_some())
    }

    pub fn set_seeded(&self) -> Result<()> {
        self.w().execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('seeded', '1')", [])?;
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

    /// Ordner, der nie indexiert wird (Caches, Abhängigkeiten, Snap-Daten …).
    pub fn is_excluded(&self, path: &Path, name: &str) -> bool {
        if EXCLUDED_NAMES.contains(&name) || self.excluded_paths.iter().any(|p| path.starts_with(p)) {
            return true;
        }
        if let Some(home) = &self.home {
            if let Ok(rel) = path.strip_prefix(home) {
                if EXCLUDED_HOME_PATHS.iter().any(|e| rel.starts_with(e)) {
                    return true;
                }
            }
        }
        has_cachedir_tag(path)
    }

    /// Fügt `path` (Datei oder ganzen Ordnerbaum) unter `parent` ein.
    /// Symlinks werden erfasst, aber nie verfolgt.
    fn insert_tree(
        &self,
        w: &mut Writer<'_>,
        parent: Option<i64>,
        path: &Path,
        top_name: &str,
        stats: &mut ScanStats,
    ) -> Result<()> {
        // Eltern-IDs entlang der aktuellen Tiefe (Tiefensuche von walkdir).
        let mut stack: Vec<i64> = Vec::new();
        let walker = walkdir::WalkDir::new(path).follow_links(false).into_iter().filter_entry(|e| {
            e.depth() == 0 || !e.file_type().is_dir() || !self.is_excluded(e.path(), &e.file_name().to_string_lossy())
        });
        for entry in walker {
            let Ok(entry) = entry else {
                stats.skipped += 1;
                continue;
            };
            let depth = entry.depth();
            let Some(kind) = classify(&entry.file_type()) else { continue };
            let name = if depth == 0 {
                top_name
            } else {
                match entry.file_name().to_str() {
                    Some(n) => n,
                    None => {
                        stats.skipped += 1;
                        continue;
                    }
                }
            };
            stack.truncate(depth);
            let p = if depth == 0 {
                parent
            } else {
                match stack.last() {
                    Some(p) => Some(*p),
                    None => continue, // Elternordner wurde übersprungen
                }
            };
            let meta = entry.metadata().ok();
            let id = w.insert(p, name, kind, meta.as_ref())?;
            if kind == NodeKind::Dir {
                stack.push(id);
                stats.dirs += 1;
                if w.new_dirs.len() < MAX_NEW_DIRS {
                    w.new_dirs.push(entry.path().to_path_buf());
                }
            } else {
                stats.files += 1;
            }
        }
        Ok(())
    }

    /// Indexiert einen Ort vollständig neu (blockierend – in `spawn_blocking` aufrufen).
    pub fn rebuild_root(&self, root_id: i64, root: &Path) -> Result<ScanStats> {
        let root_name = root.to_str().context("Pfad ist kein gültiges UTF-8")?;
        anyhow::ensure!(root.is_dir(), "{} ist kein erreichbarer Ordner", root.display());
        let mut stats = ScanStats::default();
        let conn = self.w();
        let tx = TxGuard::begin(&conn)?;
        Self::delete_root_nodes(&conn, root_id)?;
        let mut w = Writer { tx: &tx, root_id, fts_inline: false, in_batch: 0, new_dirs: Vec::new() };
        self.insert_tree(&mut w, None, root, root_name, &mut stats)?;
        // Suchindex für diesen Ort in einem Rutsch aufbauen (schneller als pro Zeile).
        conn.execute(
            "INSERT INTO nodes_fts(rowid, name) SELECT id, name FROM nodes WHERE root_id = ?1 AND parent IS NOT NULL",
            params![root_id],
        )?;
        conn.execute("UPDATE roots SET last_scan = ?2 WHERE id = ?1", params![root_id, crate::database::now_unix()])?;
        tx.commit()?;
        Ok(stats)
    }

    /// Gleicht `start` (ein Ordner innerhalb von `root`) mit dem Dateisystem ab.
    ///
    /// Ordner, deren mtime unverändert ist, werden nicht eingelesen (Namen darin können sich
    /// nicht geändert haben) – mit `recursive` wird nur in ihre Unterordner abgestiegen.
    /// `start` selbst wird immer eingelesen (Watcher-Ereignis: auch Größenänderungen).
    /// Blockierend – in `spawn_blocking` aufrufen.
    pub fn refresh(&self, root_id: i64, root: &Path, start: &Path, recursive: bool) -> Result<RefreshStats> {
        // Nicht eingehängtes Laufwerk o. Ä.: nichts löschen.
        anyhow::ensure!(root.is_dir(), "{} ist nicht erreichbar", root.display());
        let Ok(rel) = start.strip_prefix(root) else { anyhow::bail!("{} liegt nicht im Ort", start.display()) };
        let conn = self.w();
        let root_node: Option<i64> = conn
            .prepare_cached("SELECT id FROM nodes WHERE root_id = ?1 AND parent IS NULL")?
            .query_row(params![root_id], |r| r.get(0))
            .optional()?;
        let Some(root_node) = root_node else {
            drop(conn);
            let st = self.rebuild_root(root_id, root)?;
            return Ok(RefreshStats { added: st.files + st.dirs, dirs_read: st.dirs, ..Default::default() });
        };
        // Tiefsten bereits bekannten Ordner auf dem Weg zu `start` finden.
        let mut node = root_node;
        let mut node_path = root.to_path_buf();
        for comp in rel.components() {
            let Some(name) = comp.as_os_str().to_str() else { break };
            let child: Option<(i64, i64)> = conn
                .prepare_cached("SELECT id, kind FROM nodes WHERE parent = ?1 AND name = ?2")?
                .query_row(params![node, name], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            match child {
                Some((id, k)) if k == NodeKind::Dir.code() => {
                    node = id;
                    node_path.push(name);
                }
                _ => break,
            }
        }

        let tx = TxGuard::begin(&conn)?;
        let mut w = Writer { tx: &tx, root_id, fts_inline: true, in_batch: 0, new_dirs: Vec::new() };
        let mut st = RefreshStats::default();
        let mut scan = ScanStats::default();
        // (Ordner-ID, Pfad, gespeicherter Stempel, erzwingen)
        let mut stack: Vec<(i64, PathBuf, i64, bool)> = vec![(node, node_path, 0, true)];
        while let Some((id, path, stored, force)) = stack.pop() {
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => m,
                _ => {
                    if id != root_node {
                        st.removed += w.delete_subtree(id)?;
                    }
                    continue;
                }
            };
            let (_, mtime, stamp) = meta_values(Some(&meta), NodeKind::Dir);
            if !force && stored == stamp {
                st.dirs_unchanged += 1;
                if recursive {
                    let mut q =
                        conn.prepare_cached("SELECT id, name, stamp FROM nodes WHERE parent = ?1 AND kind = 1")?;
                    let subs = q
                        .query_map(params![id], |r| {
                            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    stack.extend(subs.into_iter().map(|(cid, name, cs)| (cid, path.join(name), cs, false)));
                }
                continue;
            }
            st.dirs_read += 1;
            // Zuerst `stat`, dann lesen: Ändert sich der Ordner währenddessen, fällt das beim
            // nächsten Abgleich an der neueren mtime auf.
            let mut on_disk: HashMap<String, (NodeKind, std::fs::Metadata)> = HashMap::new();
            match std::fs::read_dir(&path) {
                Ok(rd) => {
                    for e in rd.flatten() {
                        let Ok(name) = e.file_name().into_string() else { continue };
                        let Ok(ft) = e.file_type() else { continue };
                        let Some(kind) = classify(&ft) else { continue };
                        if kind == NodeKind::Dir && self.is_excluded(&path.join(&name), &name) {
                            continue;
                        }
                        // DirEntry::metadata folgt unter Unix keinen Symlinks.
                        let Ok(m) = e.metadata() else { continue };
                        on_disk.insert(name, (kind, m));
                    }
                }
                Err(e) => {
                    // Keine Berechtigung o. Ä.: bestehende Einträge behalten.
                    tracing::debug!("{} nicht lesbar: {e}", path.display());
                    continue;
                }
            }
            let known: Vec<(i64, String, i64, i64, i64, i64)> = conn
                .prepare_cached("SELECT id, name, kind, size, mtime, stamp FROM nodes WHERE parent = ?1")?
                .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (cid, name, kind, size, cmtime, cstamp) in known {
                match on_disk.remove(&name) {
                    None => st.removed += w.delete_subtree(cid)?,
                    Some((k, _)) if k.code() != kind => {
                        st.removed += w.delete_subtree(cid)?;
                        let before = scan.files + scan.dirs;
                        self.insert_tree(&mut w, Some(id), &path.join(&name), &name, &mut scan)?;
                        st.added += scan.files + scan.dirs - before;
                    }
                    Some((k, m)) => {
                        if k == NodeKind::Dir {
                            if recursive {
                                stack.push((cid, path.join(&name), cstamp, false));
                            }
                        } else {
                            let (nsize, nmtime, _) = meta_values(Some(&m), k);
                            if nsize as i64 != size || nmtime != cmtime {
                                conn.prepare_cached("UPDATE nodes SET size = ?2, mtime = ?3 WHERE id = ?1")?
                                    .execute(params![cid, nsize as i64, nmtime])?;
                                st.updated += 1;
                                w.tick()?;
                            }
                        }
                    }
                }
            }
            for (name, (kind, m)) in on_disk {
                let before = scan.files + scan.dirs;
                if kind == NodeKind::Dir {
                    self.insert_tree(&mut w, Some(id), &path.join(&name), &name, &mut scan)?;
                } else {
                    w.insert(Some(id), &name, kind, Some(&m))?;
                    scan.files += 1;
                }
                st.added += scan.files + scan.dirs - before;
            }
            conn.prepare_cached("UPDATE nodes SET stamp = ?2, mtime = ?3 WHERE id = ?1")?
                .execute(params![id, stamp, mtime])?;
        }
        if recursive && node == root_node {
            conn.execute(
                "UPDATE roots SET last_scan = ?2 WHERE id = ?1",
                params![root_id, crate::database::now_unix()],
            )?;
        }
        st.new_dirs = std::mem::take(&mut w.new_dirs);
        drop(w);
        tx.commit()?;
        Ok(st)
    }

    /// Alle indexierten Ordner eines Ortes, flache zuerst (für den Watcher).
    pub fn dir_paths(&self, root_id: i64, max: usize) -> Result<Vec<PathBuf>> {
        let conn = self.r();
        let mut stmt = conn.prepare_cached("SELECT id, parent, name FROM nodes WHERE root_id = ?1 AND kind = 1")?;
        let rows = stmt
            .query_map(params![root_id], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, String>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut children: HashMap<Option<i64>, Vec<(i64, String)>> = HashMap::new();
        for (id, parent, name) in rows {
            children.entry(parent).or_default().push((id, name));
        }
        // Breitensuche ab der Wurzel.
        let mut out = Vec::new();
        let mut queue: std::collections::VecDeque<(i64, PathBuf)> = children
            .get(&None)
            .map(|v| v.iter().map(|(id, name)| (*id, PathBuf::from(name))).collect())
            .unwrap_or_default();
        while let Some((id, path)) = queue.pop_front() {
            if out.len() >= max {
                break;
            }
            if let Some(subs) = children.get(&Some(id)) {
                queue.extend(subs.iter().map(|(cid, name)| (*cid, path.join(name))));
            }
            out.push(path);
        }
        Ok(out)
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
            "INSERT INTO nodes_fts(rowid, name) SELECT id, name FROM nodes WHERE root_id = ?1 AND parent IS NOT NULL",
            params![root_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Rekonstruiert den vollständigen Pfad eines Eintrags über die Elternkette.
    pub(crate) fn path_of(conn: &Connection, id: i64, cache: &mut HashMap<i64, String>) -> Result<String> {
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
        // Nullbytes kommen in Dateinamen nie vor – also kein Treffer. SQLite würde den
        // Suchbegriff dort sonst abschneiden (aus „\0“ würde „%“ = alles). Gefunden per Fuzzing.
        if q.is_empty() || q.contains('\0') {
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
        assert!(idx.search("\0", &SearchFilter::default(), 50).unwrap().is_empty());
        assert!(idx.search("\0\0\0x", &SearchFilter::default(), 50).unwrap().is_empty());
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

    #[test]
    fn incremental_refresh() {
        let (dir, root) = tree();
        let idx = LocalIndex::open(&dir.path().join("index.db"), vec![]).unwrap();
        let id = idx.add_root(&root).unwrap();
        idx.rebuild_root(id, &root).unwrap();
        let total = idx.count().unwrap();
        let f = SearchFilter::default();

        // Nichts geändert: nichts eingelesen, nur stat
        let st = idx.refresh(id, &root, &root, true).unwrap();
        assert_eq!((st.added, st.removed, st.updated), (0, 0, 0), "{st:?}");
        assert_eq!(st.dirs_read, 1, "nur der Startordner wird erzwungen gelesen: {st:?}");
        assert_eq!(idx.count().unwrap(), total);

        // Neue Datei, neuer Ordnerbaum, gelöschte Datei, geänderte Größe
        std::fs::write(root.join("Bilder/Urlaub 2024/Berg.png"), b"png").unwrap();
        std::fs::create_dir_all(root.join("Neu/tief/drin")).unwrap();
        std::fs::write(root.join("Neu/tief/drin/fund.txt"), b"x").unwrap();
        std::fs::remove_file(root.join("Bilder/notiz.txt")).unwrap();
        std::fs::write(root.join("Bilder/Urlaub 2024/Strand.JPG"), b"viel groesser").unwrap();
        let st = idx.refresh(id, &root, &root, true).unwrap();
        assert_eq!(st.removed, 1, "{st:?}");
        assert_eq!(st.added, 5, "Berg.png + Neu, tief, drin, fund.txt: {st:?}");
        assert!(st.new_dirs.iter().any(|p| p.ends_with("Neu/tief/drin")));
        assert!(idx.search("notiz", &f, 10).unwrap().is_empty());
        assert_eq!(idx.search("fund", &f, 10).unwrap()[0].path, format!("{}/Neu/tief/drin/fund.txt", root.display()));
        assert_eq!(idx.search("berg.png", &f, 10).unwrap().len(), 1);
        // Größenänderung ohne Ordner-mtime-Änderung: nur über das Watcher-Ereignis (erzwungen)
        let st = idx.refresh(id, &root, &root.join("Bilder/Urlaub 2024"), false).unwrap();
        assert_eq!(idx.search("strand", &f, 10).unwrap()[0].size, 13, "{st:?}");

        // Ordner gelöscht (mit Inhalt), Datei durch Ordner gleichen Namens ersetzt
        std::fs::remove_dir_all(root.join("Neu")).unwrap();
        std::fs::remove_file(root.join("Bilder/Urlaub 2024/Berg.png")).unwrap();
        std::fs::create_dir(root.join("Bilder/Urlaub 2024/Berg.png")).unwrap();
        std::fs::write(root.join("Bilder/Urlaub 2024/Berg.png/innen.txt"), b"x").unwrap();
        let st = idx.refresh(id, &root, &root, true).unwrap();
        assert_eq!(st.removed, 5, "Neu-Baum (4) + alte Datei: {st:?}");
        assert!(idx.search("fund", &f, 10).unwrap().is_empty());
        assert_eq!(idx.search("berg.png", &f, 10).unwrap()[0].kind, NodeKind::Dir);
        assert_eq!(idx.search("innen", &f, 10).unwrap().len(), 1);

        // Ergebnis entspricht einem kompletten Neuaufbau
        let after = idx.count().unwrap();
        idx.rebuild_root(id, &root).unwrap();
        assert_eq!(idx.count().unwrap(), after);

        // Startpunkt unterhalb unbekannter Ordner: tiefster bekannter Ordner wird gelesen
        std::fs::create_dir_all(root.join("x/y")).unwrap();
        std::fs::write(root.join("x/y/z.txt"), b"x").unwrap();
        idx.refresh(id, &root, &root.join("x/y"), false).unwrap();
        assert_eq!(idx.search("z.txt", &f, 10).unwrap().len(), 1);

        // Nicht erreichbarer Ort: Fehler, Index bleibt erhalten
        let n = idx.count().unwrap();
        assert!(idx.refresh(id, &dir.path().join("weg"), &dir.path().join("weg"), true).is_err());
        assert_eq!(idx.count().unwrap(), n);

        // Ordner fürs Beobachten, flache zuerst; ausgeschlossene fehlen
        let dirs = idx.dir_paths(id, 1000).unwrap();
        assert_eq!(dirs[0], root);
        assert!(!dirs.iter().any(|d| d.ends_with("node_modules") || d.ends_with(".cache")));
        assert_eq!(idx.dir_paths(id, 2).unwrap().len(), 2);

        // Entfernen deaktiviert dauerhaft für automatische Erkennung
        idx.remove_root(id).unwrap();
        assert_eq!(idx.add_root_auto(&root).unwrap(), None);
        assert_eq!(idx.add_root(&root).unwrap(), id);
        assert_eq!(idx.add_root_auto(&root).unwrap(), Some(id));
    }
}
