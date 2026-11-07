//! SQLite-Persistenz: Dateikatalog, Offline-Warteschlange und Peer-Status.

use crate::events::{Event, EventBus};
use crate::ledger::LedgerEntry;
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

const SCHEMA_VERSION: i64 = 11;

pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Eine freigegebene Datei im lokalen Katalog.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
    pub blake3_hash: String,
    pub file_name: String,
    pub file_size: u64,
    pub path: String,
    pub ticket: String,
    pub created_at: i64,
    /// Name der Ordner-Freigabe (`None` = einzeln veröffentlicht).
    #[serde(default)]
    pub share: Option<String>,
    /// Pfad relativ zur Freigabe.
    #[serde(default)]
    pub rel_path: Option<String>,
}

/// Eine eingerichtete Sicherung.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupJobRow {
    pub id: String,
    pub target: String,
    pub sources: String,
    pub created_at: i64,
    pub last_run: Option<i64>,
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
    pub last_snapshot: Option<String>,
}

/// Ein Freigabelink (lokal gespeichert; `secret` ist der Schlüssel aus dem Link).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRow {
    pub id: String,
    pub secret: String,
    pub name: String,
    pub local_path: String,
    pub size: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub max_downloads: Option<u32>,
    pub password: bool,
    pub state: String,
}

/// Neuer Upload-Auftrag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewUpload {
    pub device: String,
    pub share: String,
    pub dest_dir: String,
    pub local_path: String,
    pub rule_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UploadRow {
    pub id: i64,
    pub device: String,
    pub share: String,
    pub dest_dir: String,
    pub local_path: String,
    pub rule_id: Option<i64>,
    pub state: String,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub result: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UploadRule {
    pub id: i64,
    pub local_dir: String,
    pub device: String,
    pub share: String,
    pub dest_dir: String,
    pub created_at: i64,
}

/// Eine Ordner-Freigabe.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShareInfo {
    pub id: i64,
    pub name: String,
    pub root: String,
    /// Neue gekoppelte Geräte erhalten automatisch `files.read`.
    pub auto_grant: bool,
    pub created_at: i64,
    /// Anzahl aktiver Dateien (nur bei `share list` befüllt).
    #[serde(default)]
    pub file_count: u64,
    #[serde(default)]
    pub total_size: u64,
}

/// Bekannter Zustand einer Share-Datei (für den Abgleich beim Scannen).
#[derive(Clone, Debug)]
pub struct KnownShareFile {
    pub id: i64,
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
}

/// Änderung aus einem Scan.
#[derive(Clone, Debug)]
pub enum ScanChange {
    Upsert {
        rel_path: String,
        abs_path: String,
        name: String,
        hash: String,
        size: u64,
        mtime: i64,
    },
    Delete {
        file_id: i64,
    },
    /// Inhalt unverändert (gleicher Hash), nur die Änderungszeit ist neu –
    /// kein neuer Ledger-Eintrag, kein Abgleichverkehr.
    Touch {
        file_id: i64,
        mtime: i64,
    },
}

/// Wer einen Blob abrufen darf.
#[derive(Clone, Debug, Default)]
pub struct BlobAccess {
    /// Einzeln veröffentlicht: Ticket reicht.
    pub published: bool,
    /// Absolute Pfade in Ordner-Freigaben: nur mit `files.read`.
    pub share_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum JobState {
    /// Wartet auf den nächsten Versuch.
    Waiting,
    /// Endgültig fehlgeschlagen (z. B. Peer lehnt ab, lokaler Schreibfehler).
    /// Wird nicht automatisch wiederholt – `queue retry` setzt ihn zurück.
    Failed,
}

impl JobState {
    fn as_str(&self) -> &'static str {
        match self {
            JobState::Waiting => "waiting",
            JobState::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "failed" => JobState::Failed,
            _ => JobState::Waiting,
        }
    }
}

/// Ein Auftrag in der Offline-Warteschlange (`pending_pins`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingJob {
    pub id: i64,
    pub target_hash: String,
    pub action: String,
    pub ticket: String,
    pub peer_id: String,
    pub output_path: String,
    /// `true`, wenn der Nutzer den Zielpfad selbst angegeben hat.
    pub explicit_output: bool,
    pub force: bool,
    pub attempts: u32,
    pub state: JobState,
    pub last_error: Option<String>,
    pub created_at: i64,
    /// Nächster geplanter Versuch (Backoff des Peers). `None` = sofort fällig.
    pub next_attempt_at: Option<i64>,
}

/// Neuer Download-Auftrag für die Warteschlange.
pub struct NewJob<'a> {
    /// "DOWNLOAD" (vom Nutzer angefordert) oder "PIN" (Spiegelung eines Pins).
    pub action: &'a str,
    pub target_hash: &'a str,
    pub ticket: &'a str,
    pub peer_id: &'a str,
    pub output_path: &'a str,
    pub explicit_output: bool,
    pub force: bool,
    pub last_error: &'a str,
}

/// Zuletzt bekannter Status eines Peers. Wird ausschließlich durch echte
/// Verbindungen aktualisiert – es gibt keine Pings oder Heartbeats.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node_id: String,
    /// "online" oder "offline"
    pub status: String,
    /// Letzte erfolgreiche Verbindung (Unix-Sekunden).
    pub last_seen: Option<i64>,
    pub failures: u32,
    pub next_retry_at: Option<i64>,
}

/// Ein gekoppeltes Gerät.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub node_id: String,
    pub name: String,
    pub paired_at: i64,
    pub last_sync_at: Option<i64>,
    /// Gewährte Rechte (nur bei `pair list` befüllt).
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Gerade live verbunden (nur bei `pair list` befüllt).
    #[serde(default)]
    pub online: bool,
    /// Zuletzt gemeldeter Speicher seiner Freigaben (nur bei `pair list`, falls gemeldet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<DeviceStorage>,
}

/// Speicher, den ein gekoppeltes Gerät meldet: Summen über die Laufwerke seiner Freigaben.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceStorage {
    pub total: u64,
    pub free: u64,
    /// Zeitpunkt der Meldung (Unix-Sekunden).
    pub updated_at: i64,
}

/// Ein gewährtes Recht, optional auf einen Ordner begrenzt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub capability: String,
    pub folder: Option<String>,
    pub granted_at: i64,
}

/// Ein Eintrag im Audit-Log (Operationen fremder Geräte).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    pub ts: i64,
    pub node_id: String,
    pub operation: String,
    pub target: Option<String>,
    pub allowed: bool,
    pub detail: Option<String>,
}

/// Eine Datei auf einem anderen Gerät (aus dem Ledger).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RemoteFile {
    pub origin: String,
    /// Name des Geräts, falls direkt gekoppelt (sonst über ein anderes Gerät erfahren).
    pub origin_name: Option<String>,
    pub hash: String,
    pub name: String,
    pub size: u64,
    pub created_at: i64,
    /// Ordner-Freigabe des Ursprungsgeräts (`None` = einzeln veröffentlicht).
    #[serde(default)]
    pub share: Option<String>,
    #[serde(default)]
    pub rel_path: Option<String>,
}

/// SQLite mit einem Schreiber und einem Pool aus Leseverbindungen.
///
/// Im WAL-Modus blockieren Leser weder sich gegenseitig noch den Schreiber. So
/// bleiben Abfragen der Oberfläche auch während großer Abgleiche flüssig.
pub struct DatabaseManager {
    conn: Mutex<Connection>,
    readers: Vec<Mutex<Connection>>,
    next_reader: AtomicUsize,
    events: EventBus,
    /// Zwischengespeicherte Gesamtzahlen für `browse`; wird bei jeder Katalogänderung geleert.
    count_cache: Mutex<HashMap<String, u64>>,
    /// Zuletzt geschriebene Audit-Einträge (Schlüssel → Zeitpunkt) zum Entdoppeln.
    audit_recent: Mutex<HashMap<String, i64>>,
}

/// Gleiche Audit-Einträge werden innerhalb dieses Fensters nur einmal geschrieben
/// (Streaming erzeugt viele gleiche Zugriffe; Angreifer könnten sonst das Log fluten).
const AUDIT_DEDUP_SECS: i64 = 60;

/// Anzahl der Leseverbindungen.
const READER_POOL_SIZE: usize = 4;
/// Gecachte vorbereitete Statements pro Verbindung.
const STATEMENT_CACHE: usize = 64;

/// Prüft und normalisiert einen Hash bzw. Hash-Präfix: nur Hexzeichen,
/// 6 bis 64 Zeichen. Verhindert u. a. SQL-Wildcards (`%`, `_`) in Suchen.
pub fn normalize_hash_prefix(input: &str) -> Result<String> {
    let s = input.trim().to_ascii_lowercase();
    if s.len() < 6 {
        crate::api_bail!(InvalidInput, "Hash zu kurz – bitte mindestens 6 Zeichen angeben.");
    }
    if s.len() > 64 {
        crate::api_bail!(InvalidInput, "Hash zu lang – ein BLAKE3-Hash hat 64 Hexzeichen.");
    }
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        crate::api_bail!(InvalidInput, "Ungültiger Hash – nur Hexzeichen (0-9, a-f) erlaubt.");
    }
    Ok(s)
}

impl DatabaseManager {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        let mut db = Self::init(conn)?;
        for _ in 0..READER_POOL_SIZE {
            let r = Connection::open_with_flags(
                db_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
            )?;
            r.busy_timeout(std::time::Duration::from_secs(5))?;
            r.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
            db.readers.push(Mutex::new(r));
        }
        Ok(db)
    }

    /// In-Memory-Datenbank (Tests). Ohne Leser-Pool: alle Zugriffe über eine Verbindung.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version == 0 {
            let legacy: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='files')",
                [],
                |r| r.get(0),
            )?;
            if legacy {
                bail!(
                    "Die Datenbank stammt von einer älteren Version ohne Schema-Version. \
                     Bitte einen neuen data_dir verwenden (die alte Datei wird nicht verändert)."
                );
            }
        } else if version > SCHEMA_VERSION {
            bail!("Die Datenbank stammt von einer neueren Version (Schema {version}).");
        }

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS files (
                id          INTEGER PRIMARY KEY,
                blake3_hash TEXT    NOT NULL,
                path        TEXT    NOT NULL UNIQUE,
                file_name   TEXT    NOT NULL,
                file_size   INTEGER NOT NULL,
                ticket      TEXT    NOT NULL,
                created_at  INTEGER NOT NULL,
                deleted_at  INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_files_hash ON files(blake3_hash);
            CREATE INDEX IF NOT EXISTS idx_files_deleted_at ON files(deleted_at);

            CREATE TABLE IF NOT EXISTS peer_status (
                node_id       TEXT PRIMARY KEY,
                status        TEXT    NOT NULL,      -- 'online' | 'offline'
                last_seen     INTEGER,               -- letzte erfolgreiche Verbindung
                failures      INTEGER NOT NULL DEFAULT 0,
                next_retry_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS pending_pins (
                id              INTEGER PRIMARY KEY,
                target_hash     TEXT    NOT NULL,
                action          TEXT    NOT NULL,    -- aktuell nur 'DOWNLOAD'
                ticket          TEXT    NOT NULL,
                peer_id         TEXT    NOT NULL,
                output_path     TEXT    NOT NULL,
                explicit_output INTEGER NOT NULL,
                force           INTEGER NOT NULL DEFAULT 0,
                attempts        INTEGER NOT NULL DEFAULT 0,
                state           TEXT    NOT NULL DEFAULT 'waiting',
                last_error      TEXT,
                created_at      INTEGER NOT NULL,
                UNIQUE(target_hash, action, output_path)
            );
            CREATE INDEX IF NOT EXISTS idx_pending_peer ON pending_pins(peer_id);

            -- Schema 2: Pairing & Ledger
            CREATE TABLE IF NOT EXISTS devices (
                node_id      TEXT PRIMARY KEY,
                name         TEXT    NOT NULL,
                paired_at    INTEGER NOT NULL,
                last_sync_at INTEGER
            );

            -- Signierte Katalogeinträge aller Geräte (eigene und empfangene).
            -- Pro (origin, entry_id) wird nur die neueste Version gespeichert.
            CREATE TABLE IF NOT EXISTS ledger_entries (
                origin     TEXT    NOT NULL,
                entry_id   INTEGER NOT NULL,
                seq        INTEGER NOT NULL,
                hash       TEXT    NOT NULL,
                name       TEXT    NOT NULL,
                size       INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                deleted    INTEGER NOT NULL,
                signature  TEXT    NOT NULL,
                PRIMARY KEY (origin, entry_id)
            );
            CREATE INDEX IF NOT EXISTS idx_ledger_origin_seq ON ledger_entries(origin, seq);
            CREATE INDEX IF NOT EXISTS idx_ledger_hash ON ledger_entries(hash);
            CREATE INDEX IF NOT EXISTS idx_ledger_name ON ledger_entries(name COLLATE NOCASE, origin, hash);
            CREATE INDEX IF NOT EXISTS idx_ledger_live ON ledger_entries(deleted, origin);
            CREATE INDEX IF NOT EXISTS idx_files_name ON files(file_name COLLATE NOCASE, blake3_hash);

            -- Direkte Adress-Hinweise, nur von authentifizierten Gegenstellen über sich selbst.
            CREATE TABLE IF NOT EXISTS peer_addrs (
                node_id    TEXT PRIMARY KEY,
                addrs      TEXT    NOT NULL,
                updated_at INTEGER NOT NULL
            );

            -- Schema 3: Rechte pro Gerät und Audit-Log
            -- folder = '' bedeutet: ohne Ordnerbeschränkung (nur für Rechte ohne Pfadbezug).
            CREATE TABLE IF NOT EXISTS device_grants (
                node_id    TEXT    NOT NULL,
                capability TEXT    NOT NULL,
                folder     TEXT    NOT NULL DEFAULT '',
                granted_at INTEGER NOT NULL,
                PRIMARY KEY (node_id, capability, folder)
            );
            CREATE TABLE IF NOT EXISTS audit_log (
                id        INTEGER PRIMARY KEY,
                ts        INTEGER NOT NULL,
                node_id   TEXT    NOT NULL,
                operation TEXT    NOT NULL,
                target    TEXT,
                allowed   INTEGER NOT NULL,
                detail    TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);
            ",
        )?;
        if version < 4 {
            // Migration -> 4: Ordner-Freigaben, relative Pfade im Ledger.
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS shares (
                    id         INTEGER PRIMARY KEY,
                    name       TEXT    NOT NULL UNIQUE,
                    root       TEXT    NOT NULL UNIQUE,
                    auto_grant INTEGER NOT NULL,
                    created_at INTEGER NOT NULL
                );
                ALTER TABLE files ADD COLUMN share_id INTEGER;
                ALTER TABLE files ADD COLUMN rel_path TEXT;
                ALTER TABLE files ADD COLUMN mtime INTEGER;
                CREATE INDEX IF NOT EXISTS idx_files_share ON files(share_id, rel_path);
                ALTER TABLE ledger_entries ADD COLUMN share TEXT NOT NULL DEFAULT '';
                ALTER TABLE ledger_entries ADD COLUMN rel_path TEXT NOT NULL DEFAULT '';
                ",
            )?;
        }
        if version < 5 {
            // Migration -> 5: Smart-Pinning.
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS pins (
                    id         INTEGER PRIMARY KEY,
                    origin     TEXT    NOT NULL,
                    share      TEXT    NOT NULL,
                    prefix     TEXT    NOT NULL,
                    dir_name   TEXT    NOT NULL,
                    created_at INTEGER NOT NULL,
                    UNIQUE (origin, share, prefix)
                );
                -- Gespiegelte Dateien: was wir wohin geschrieben haben (für Updates, Löschungen
                -- und um lokal veränderte Kopien nie zu überschreiben).
                CREATE TABLE IF NOT EXISTS pinned_files (
                    path     TEXT    PRIMARY KEY,
                    pin_id   INTEGER NOT NULL,
                    origin   TEXT    NOT NULL,
                    entry_id INTEGER NOT NULL,
                    hash     TEXT    NOT NULL,
                    size     INTEGER NOT NULL,
                    mtime    INTEGER NOT NULL DEFAULT 0,
                    state    TEXT    NOT NULL  -- 'pending' | 'ok' | 'conflict'
                );
                CREATE INDEX IF NOT EXISTS idx_pinned_pin ON pinned_files(pin_id);
                ",
            )?;
        }
        if version < 6 {
            // Migration -> 6: Bestätigungen für Fernoperationen (Massenschutz).
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS op_approvals (
                    id         INTEGER PRIMARY KEY,
                    node_id    TEXT    NOT NULL,
                    op         TEXT    NOT NULL,  -- JSON der Fernoperation
                    summary    TEXT    NOT NULL,
                    created_at INTEGER NOT NULL,
                    state      TEXT    NOT NULL   -- 'pending' | 'approved' | 'rejected'
                );
                CREATE INDEX IF NOT EXISTS idx_approvals_state ON op_approvals(state, created_at);
                ",
            )?;
        }
        if version < 7 {
            // Migration -> 7: Streaming-Cache (welche Blobs wann zuletzt gestreamt wurden).
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS stream_cache (
                    hash        TEXT    PRIMARY KEY,
                    last_access INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_stream_access ON stream_cache(last_access);
                ",
            )?;
        }
        if version < 8 {
            // Migration -> 8: Hochladen (Warteschlange, Ordner-Regeln, Tagesgrenzen).
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS uploads (
                    id         INTEGER PRIMARY KEY,
                    device     TEXT    NOT NULL,   -- Node-ID des Ziels
                    share      TEXT    NOT NULL,
                    dest_dir   TEXT    NOT NULL,   -- relativ zur Freigabe ('' = Wurzel)
                    local_path TEXT    NOT NULL,
                    rule_id    INTEGER,
                    state      TEXT    NOT NULL,   -- 'pending' | 'done' | 'failed'
                    attempts   INTEGER NOT NULL DEFAULT 0,
                    next_try   INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    result     TEXT,               -- Pfad auf dem Ziel
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_uploads_state ON uploads(state, next_try);
                CREATE TABLE IF NOT EXISTS upload_rules (
                    id         INTEGER PRIMARY KEY,
                    local_dir  TEXT    NOT NULL,
                    device     TEXT    NOT NULL,
                    share      TEXT    NOT NULL,
                    dest_dir   TEXT    NOT NULL,
                    created_at INTEGER NOT NULL,
                    UNIQUE (local_dir, device, share, dest_dir)
                );
                -- Schon eingeplante Dateien je Regel (kein doppeltes Hochladen).
                CREATE TABLE IF NOT EXISTS upload_seen (
                    rule_id  INTEGER NOT NULL,
                    rel_path TEXT    NOT NULL,
                    size     INTEGER NOT NULL,
                    mtime    INTEGER NOT NULL,
                    PRIMARY KEY (rule_id, rel_path)
                );
                -- Zur Laufzeit geänderte Einstellungen (überschreiben config.toml).
                CREATE TABLE IF NOT EXISTS settings (
                    key   TEXT    PRIMARY KEY,
                    value INTEGER NOT NULL
                );
                -- Empfangene Bytes je Gerät und Tag (Grenze gegen volle Platten).
                CREATE TABLE IF NOT EXISTS upload_usage (
                    node_id TEXT    NOT NULL,
                    day     INTEGER NOT NULL,
                    bytes   INTEGER NOT NULL,
                    PRIMARY KEY (node_id, day)
                );
                ",
            )?;
        }
        if version < 9 {
            // Migration -> 9: Freigabelinks (Geheimnis bleibt im privaten Datenordner).
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS links (
                    id            TEXT    PRIMARY KEY,
                    secret        TEXT    NOT NULL,
                    name          TEXT    NOT NULL,
                    local_path    TEXT    NOT NULL,
                    size          INTEGER NOT NULL,
                    created_at    INTEGER NOT NULL,
                    expires_at    INTEGER NOT NULL,
                    max_downloads INTEGER,
                    password      INTEGER NOT NULL,
                    state         TEXT    NOT NULL   -- 'active' | 'revoked' | 'expired'
                );
                ",
            )?;
        }
        if version < 10 {
            // Migration -> 10: Sicherungen.
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS backup_jobs (
                    id            TEXT    PRIMARY KEY,
                    target        TEXT    NOT NULL,   -- JSON (BackupTarget)
                    sources       TEXT    NOT NULL,   -- JSON-Liste absoluter Pfade
                    created_at    INTEGER NOT NULL,
                    last_run      INTEGER,
                    last_ok       INTEGER,
                    last_error    TEXT,
                    last_snapshot TEXT
                );
                ",
            )?;
        }
        if version < 11 {
            // Migration -> 11: Ordneransicht fremder Geräte (Sprünge im Pfadindex statt Scan).
            conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_ledger_tree ON ledger_entries(origin, share, rel_path) WHERE deleted = 0;",
            )?;
        }
        if version > 0 && version < 3 {
            // Migration 2 -> 3: bestehende Kopplungen behalten ihr bisheriges Recht.
            conn.execute(
                "INSERT OR IGNORE INTO device_grants (node_id, capability, folder, granted_at)
                 SELECT node_id, 'catalog.read', '', paired_at FROM devices",
                [],
            )?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);

        Ok(Self {
            conn: Mutex::new(conn),
            readers: Vec::new(),
            next_reader: AtomicUsize::new(0),
            events: EventBus::default(),
            count_cache: Mutex::new(HashMap::new()),
            audit_recent: Mutex::new(HashMap::new()),
        })
    }

    /// Ereignis-Bus: meldet jede Änderung nach dem Commit.
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Sendet ein Ereignis; Katalogänderungen leeren den Zähler-Cache.
    fn emit(&self, event: Event) {
        if matches!(event, Event::LocalCatalogChanged | Event::RemoteCatalogChanged { .. }) {
            self.count_cache.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
        self.events.emit(event);
    }

    fn cached_count(&self, key: &str) -> Option<u64> {
        self.count_cache.lock().unwrap_or_else(|e| e.into_inner()).get(key).copied()
    }

    fn store_count(&self, key: String, total: u64) {
        let mut cache = self.count_cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 64 {
            cache.clear(); // einfache Begrenzung; Suchbegriffe wechseln häufig
        }
        cache.insert(key, total);
    }

    /// Schreibverbindung (auch für Lese-nach-Schreib innerhalb einer Methode).
    fn conn(&self) -> MutexGuard<'_, Connection> {
        // Ein Panic in einem anderen Thread soll die DB nicht dauerhaft blockieren.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Eine freie Leseverbindung aus dem Pool (fällt ohne Pool auf den Schreiber zurück).
    fn read(&self) -> MutexGuard<'_, Connection> {
        let n = self.readers.len();
        if n == 0 {
            return self.conn();
        }
        let start = self.next_reader.fetch_add(1, Ordering::Relaxed);
        for i in 0..n {
            if let Ok(guard) = self.readers[(start + i) % n].try_lock() {
                return guard;
            }
        }
        self.readers[start % n].lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------------
    // Dateikatalog
    // ------------------------------------------------------------------

    /// Trägt eine freigegebene Datei ein oder aktualisiert den Eintrag für denselben Pfad.
    /// Gibt `true` zurück, wenn genau diese Datei bereits freigegeben war.
    pub fn upsert_file(
        &self,
        path: &str,
        hash: &str,
        file_name: &str,
        file_size: u64,
        ticket: &str,
    ) -> Result<(bool, i64)> {
        let mut conn = self.conn();
        let conn = conn.transaction()?;
        let already: bool = conn
            .query_row(
                "SELECT 1 FROM files WHERE path = ?1 AND blake3_hash = ?2 AND deleted_at IS NULL",
                params![path, hash],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        conn.execute(
            "INSERT INTO files (blake3_hash, path, file_name, file_size, ticket, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                blake3_hash = excluded.blake3_hash,
                file_name   = excluded.file_name,
                file_size   = excluded.file_size,
                ticket      = excluded.ticket,
                created_at  = CASE WHEN files.blake3_hash = excluded.blake3_hash
                                    AND files.deleted_at IS NULL
                                   THEN files.created_at ELSE excluded.created_at END,
                deleted_at  = NULL",
            params![hash, path, file_name, file_size as i64, ticket, now_unix()],
        )?;
        let id: i64 = conn.query_row("SELECT id FROM files WHERE path = ?1", params![path], |r| r.get(0))?;
        conn.commit()?;
        if !already {
            self.emit(Event::LocalCatalogChanged);
        }
        Ok((already, id))
    }

    /// Daten einer Datei per ID (auch gelöschte).
    pub fn file_by_id(&self, id: i64) -> Result<Option<FileRecord>> {
        let conn = self.read();
        Ok(conn
            .query_row(
                "SELECT f.blake3_hash, f.file_name, f.file_size, f.path, f.ticket, f.created_at, s.name, f.rel_path
                 FROM files f LEFT JOIN shares s ON s.id = f.share_id WHERE f.id = ?1",
                params![id],
                Self::map_file,
            )
            .optional()?)
    }

    /// Wer diesen Hash abrufen darf: einzeln veröffentlicht (Ticket reicht) und/oder
    /// in Ordner-Freigaben (nur mit `files.read` auf den Pfad).
    pub fn blob_access(&self, hash: &str) -> Result<BlobAccess> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT share_id IS NULL, path FROM files WHERE blake3_hash = ?1 AND deleted_at IS NULL LIMIT 64",
        )?;
        let mut access = BlobAccess::default();
        for row in stmt.query_map(params![hash], |r| Ok((r.get::<_, bool>(0)?, r.get::<_, String>(1)?)))? {
            let (published, path) = row?;
            if published {
                access.published = true;
            } else {
                access.share_paths.push(path);
            }
        }
        Ok(access)
    }

    /// Gibt es überhaupt eine aktive Datei mit diesem Hash?
    pub fn is_shared(&self, hash: &str) -> Result<bool> {
        let conn = self.read();
        Ok(conn
            .query_row(
                "SELECT 1 FROM files WHERE blake3_hash = ?1 AND deleted_at IS NULL LIMIT 1",
                params![hash],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn list_files(&self) -> Result<Vec<FileRecord>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
"SELECT f.blake3_hash, f.file_name, f.file_size, f.path, f.ticket, f.created_at, s.name, f.rel_path
             FROM files f LEFT JOIN shares s ON s.id = f.share_id WHERE f.deleted_at IS NULL ORDER BY f.file_name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], Self::map_file)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn map_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            blake3_hash: row.get(0)?,
            file_name: row.get(1)?,
            file_size: row.get::<_, i64>(2)? as u64,
            path: row.get(3)?,
            ticket: row.get(4)?,
            created_at: row.get(5)?,
            share: row.get(6)?,
            rel_path: row.get(7)?,
        })
    }

    /// Löst einen (validierten) Hash-Präfix eindeutig auf aktive Dateien auf.
    /// Gibt alle passenden Dateien zurück; der Aufrufer prüft auf Eindeutigkeit.
    pub fn find_by_hash_prefix(&self, prefix: &str) -> Result<Vec<FileRecord>> {
        let prefix = normalize_hash_prefix(prefix)?;
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT f.blake3_hash, f.file_name, f.file_size, f.path, f.ticket, f.created_at, s.name, f.rel_path
             FROM files f LEFT JOIN shares s ON s.id = f.share_id
             WHERE f.deleted_at IS NULL AND substr(f.blake3_hash, 1, ?2) = ?1 ORDER BY f.created_at",
        )?;
        let rows = stmt
            .query_map(params![prefix, prefix.len() as i64], Self::map_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Verschiebt alle aktiven Einträge mit diesem Hash in den Papierkorb. Gibt die IDs zurück.
    pub fn soft_delete_by_hash(&self, full_hash: &str) -> Result<Vec<i64>> {
        self.soft_delete_where("blake3_hash = ?1", full_hash)
    }

    /// Verschiebt den aktiven Eintrag mit diesem Pfad in den Papierkorb. Gibt die IDs zurück.
    pub fn soft_delete_by_path(&self, path: &str) -> Result<Vec<i64>> {
        self.soft_delete_where("path = ?1", path)
    }

    fn soft_delete_where(&self, cond: &str, value: &str) -> Result<Vec<i64>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare_cached(&format!("SELECT id FROM files WHERE {cond} AND deleted_at IS NULL"))?;
            let rows = stmt.query_map(params![value], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
            rows
        };
        tx.execute(
            &format!("UPDATE files SET deleted_at = ?2 WHERE {cond} AND deleted_at IS NULL"),
            params![value, now_unix()],
        )?;
        tx.commit()?;
        if !ids.is_empty() {
            self.emit(Event::LocalCatalogChanged);
        }
        Ok(ids)
    }

    /// Entfernt Papierkorb-Einträge, deren Frist abgelaufen ist. Gibt die Hashes
    /// zurück, auf die danach kein Eintrag mehr verweist (deren Blobs gelöscht werden dürfen).
    pub fn purge_expired(&self, retention_days: u32) -> Result<Vec<String>> {
        let cutoff = now_unix() - i64::from(retention_days) * 86_400;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let candidates: Vec<String> = {
            let mut stmt = tx.prepare_cached(
                "SELECT DISTINCT blake3_hash FROM files WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
            )?;
            let rows = stmt.query_map(params![cutoff], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        tx.execute("DELETE FROM files WHERE deleted_at IS NOT NULL AND deleted_at <= ?1", params![cutoff])?;
        let mut orphaned = Vec::new();
        for hash in candidates {
            let still_used: bool = tx
                .query_row("SELECT 1 FROM files WHERE blake3_hash = ?1 LIMIT 1", params![hash], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if !still_used {
                orphaned.push(hash);
            }
        }
        tx.commit()?;
        Ok(orphaned)
    }

    pub fn count_shared(&self) -> Result<u64> {
        let conn = self.read();
        Ok(conn.query_row("SELECT COUNT(*) FROM files WHERE deleted_at IS NULL", [], |r| r.get::<_, i64>(0))? as u64)
    }

    // ------------------------------------------------------------------
    // Offline-Warteschlange
    // ------------------------------------------------------------------

    /// Legt einen Download-Auftrag an. Existiert bereits ein identischer Auftrag
    /// (gleicher Hash und Zielpfad), wird dieser wiederverwendet.
    /// Rückgabe: (Job-ID, war bereits vorhanden)
    pub fn enqueue_download(&self, job: &NewJob<'_>) -> Result<(i64, bool)> {
        let conn = self.conn();
        if let Some(id) = conn
            .query_row(
                "SELECT id FROM pending_pins WHERE target_hash = ?1 AND action = ?3 AND output_path = ?2",
                params![job.target_hash, job.output_path, job.action],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            conn.execute(
                "UPDATE pending_pins SET state = 'waiting', last_error = ?2, ticket = ?3,
                        peer_id = ?4, force = ?5
                 WHERE id = ?1",
                params![id, job.last_error, job.ticket, job.peer_id, job.force],
            )?;
            drop(conn);
            self.emit(Event::JobQueued { job_id: id });
            return Ok((id, true));
        }
        conn.execute(
            "INSERT INTO pending_pins
               (target_hash, action, ticket, peer_id, output_path, explicit_output, force,
                attempts, state, last_error, created_at)
             VALUES (?1, ?9, ?2, ?3, ?4, ?5, ?6, ?10, 'waiting', ?7, ?8)",
            params![
                job.target_hash,
                job.ticket,
                job.peer_id,
                job.output_path,
                job.explicit_output,
                job.force,
                job.last_error,
                now_unix(),
                job.action,
                if job.action == "PIN" { 0 } else { 1 }
            ],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.emit(Event::JobQueued { job_id: id });
        Ok((id, false))
    }

    const JOB_SELECT: &'static str = "
        SELECT j.id, j.target_hash, j.action, j.ticket, j.peer_id, j.output_path,
               j.explicit_output, j.force, j.attempts, j.state, j.last_error, j.created_at,
               p.next_retry_at
        FROM pending_pins j LEFT JOIN peer_status p ON p.node_id = j.peer_id";

    fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingJob> {
        Ok(PendingJob {
            id: row.get(0)?,
            target_hash: row.get(1)?,
            action: row.get(2)?,
            ticket: row.get(3)?,
            peer_id: row.get(4)?,
            output_path: row.get(5)?,
            explicit_output: row.get(6)?,
            force: row.get(7)?,
            attempts: row.get::<_, i64>(8)? as u32,
            state: JobState::parse(&row.get::<_, String>(9)?),
            last_error: row.get(10)?,
            created_at: row.get(11)?,
            next_attempt_at: row.get(12)?,
        })
    }

    pub fn list_jobs(&self) -> Result<Vec<PendingJob>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(&format!("{} ORDER BY j.id", Self::JOB_SELECT))?;
        let rows = stmt.query_map([], Self::map_job)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Wartende Aufträge, deren Peer nicht (mehr) im Backoff ist.
    pub fn due_jobs(&self, now: i64) -> Result<Vec<PendingJob>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(&format!(
            "{} WHERE j.state = 'waiting' AND (p.next_retry_at IS NULL OR p.next_retry_at <= ?1)
             ORDER BY j.id",
            Self::JOB_SELECT
        ))?;
        let rows = stmt.query_map(params![now], Self::map_job)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Zeitpunkt, zu dem der nächste wartende Auftrag fällig wird.
    /// `None` = keine wartenden Aufträge (Worker darf unbegrenzt schlafen).
    pub fn next_wakeup(&self) -> Result<Option<i64>> {
        let conn = self.read();
        let (count, min_retry, any_due): (i64, Option<i64>, i64) = conn.query_row(
            "SELECT COUNT(*),
                    MIN(p.next_retry_at),
                    COALESCE(SUM(CASE WHEN p.next_retry_at IS NULL THEN 1 ELSE 0 END), 0)
             FROM pending_pins j LEFT JOIN peer_status p ON p.node_id = j.peer_id
             WHERE j.state = 'waiting'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if count == 0 {
            return Ok(None);
        }
        if any_due > 0 {
            return Ok(Some(now_unix()));
        }
        Ok(min_retry)
    }

    pub fn count_waiting_jobs(&self) -> Result<u64> {
        let conn = self.read();
        Ok(conn.query_row("SELECT COUNT(*) FROM pending_pins WHERE state = 'waiting'", [], |r| r.get::<_, i64>(0))?
            as u64)
    }

    pub fn complete_job(&self, id: i64) -> Result<()> {
        self.conn().execute("DELETE FROM pending_pins WHERE id = ?1", params![id])?;
        self.emit(Event::JobCompleted { job_id: id });
        Ok(())
    }

    /// Versuch fehlgeschlagen, Auftrag bleibt wartend.
    pub fn job_attempt_failed(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE pending_pins SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(())
    }

    /// Auftrag endgültig fehlgeschlagen (keine automatischen Wiederholungen mehr).
    pub fn job_failed(&self, id: i64, error: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE pending_pins SET attempts = attempts + 1, state = ?3, last_error = ?2 WHERE id = ?1",
            params![id, error, JobState::Failed.as_str()],
        )?;
        self.emit(Event::JobFailed { job_id: id, error: error.to_string() });
        Ok(())
    }

    pub fn cancel_job(&self, id: i64) -> Result<bool> {
        let removed = self.conn().execute("DELETE FROM pending_pins WHERE id = ?1", params![id])? > 0;
        if removed {
            self.emit(Event::JobRemoved { job_id: id });
        }
        Ok(removed)
    }

    /// Setzt einen Auftrag auf "wartend" und das Backoff seines Peers zurück.
    /// Gibt `false` zurück, wenn es den Auftrag nicht gibt.
    pub fn retry_job(&self, id: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let peer: Option<String> =
            tx.query_row("SELECT peer_id FROM pending_pins WHERE id = ?1", params![id], |r| r.get(0)).optional()?;
        let Some(peer) = peer else {
            return Ok(false);
        };
        tx.execute("UPDATE pending_pins SET state = 'waiting' WHERE id = ?1", params![id])?;
        tx.execute("UPDATE peer_status SET next_retry_at = NULL WHERE node_id = ?1", params![peer])?;
        tx.commit()?;
        Ok(true)
    }

    // ------------------------------------------------------------------
    // Peer-Status (rein reaktiv)
    // ------------------------------------------------------------------

    /// Erfolgreiche Verbindung: online, Backoff zurücksetzen.
    pub fn mark_peer_online(&self, node_id: &str) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let prev = Self::peer_status_of(&tx, node_id)?;
        tx.execute(
            "INSERT INTO peer_status (node_id, status, last_seen, failures, next_retry_at)
             VALUES (?1, 'online', ?2, 0, NULL)
             ON CONFLICT(node_id) DO UPDATE SET
                status = 'online', last_seen = ?2, failures = 0, next_retry_at = NULL",
            params![node_id, now_unix()],
        )?;
        tx.commit()?;
        self.emit_status_change(node_id, prev.as_deref(), "online");
        Ok(())
    }

    fn peer_status_of(conn: &Connection, node_id: &str) -> rusqlite::Result<Option<String>> {
        conn.prepare_cached("SELECT status FROM peer_status WHERE node_id = ?1")?
            .query_row(params![node_id], |r| r.get(0))
            .optional()
    }

    fn emit_status_change(&self, node_id: &str, prev: Option<&str>, now: &str) {
        if prev != Some(now) {
            self.emit(Event::PeerStatusChanged { node_id: node_id.to_string(), status: now.to_string() });
        }
    }

    /// Verbindung fehlgeschlagen: offline und Backoff planen. Gibt den Zeitpunkt
    /// des nächsten automatischen Versuchs zurück. `last_seen` bleibt unverändert.
    ///
    /// `escalate = true` (Worker): Backoff-Stufe erhöhen.
    /// `escalate = false` (manueller Versuch): Läuft bereits ein Backoff, bleibt der
    /// Zeitplan unverändert – erneutes Probieren durch den Nutzer verlängert die Wartezeit nicht.
    pub fn mark_peer_unreachable(&self, node_id: &str, escalate: bool) -> Result<i64> {
        let now = now_unix();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let existing: Option<(u32, Option<i64>, String)> = tx
            .query_row(
                "SELECT failures, next_retry_at, status FROM peer_status WHERE node_id = ?1",
                params![node_id],
                |r| Ok((r.get::<_, i64>(0)? as u32, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let prev = existing.as_ref().map(|e| e.2.clone());
        if !escalate {
            if let Some((_, Some(next), _)) = existing {
                if next > now {
                    tx.execute("UPDATE peer_status SET status = 'offline' WHERE node_id = ?1", params![node_id])?;
                    tx.commit()?;
                    self.emit_status_change(node_id, prev.as_deref(), "offline");
                    return Ok(next);
                }
            }
        }
        let failures = existing.map(|(f, _, _)| f).unwrap_or(0).saturating_add(1);
        let next = now + crate::backoff::delay_random(failures).as_secs() as i64;
        tx.execute(
            "INSERT INTO peer_status (node_id, status, last_seen, failures, next_retry_at)
             VALUES (?1, 'offline', NULL, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET
                status = 'offline', failures = ?2, next_retry_at = ?3",
            params![node_id, failures, next],
        )?;
        tx.commit()?;
        self.emit_status_change(node_id, prev.as_deref(), "offline");
        Ok(next)
    }

    /// Live-Verbindung beendet: offline, ohne das Download-Backoff zu verändern.
    pub fn mark_peer_disconnected(&self, node_id: &str) -> Result<()> {
        let conn = self.conn();
        let prev = Self::peer_status_of(&conn, node_id)?;
        conn.prepare_cached("UPDATE peer_status SET status = 'offline' WHERE node_id = ?1")?
            .execute(params![node_id])?;
        drop(conn);
        if prev.is_some() {
            self.emit_status_change(node_id, prev.as_deref(), "offline");
        }
        Ok(())
    }

    /// Ein bereits bekannter Peer hat sich bei uns gemeldet (eingehende Verbindung).
    /// Unbekannte Node-IDs werden bewusst nicht gespeichert, damit Fremde die
    /// Tabelle nicht füllen können. Gibt `true` zurück, wenn für diesen Peer
    /// wartende Aufträge existieren.
    pub fn mark_known_peer_seen(&self, node_id: &str) -> Result<bool> {
        let conn = self.conn();
        let prev = Self::peer_status_of(&conn, node_id)?;
        let updated = conn.execute(
            "UPDATE peer_status SET status = 'online', last_seen = ?2, failures = 0, next_retry_at = NULL
             WHERE node_id = ?1",
            params![node_id, now_unix()],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        self.emit_status_change(node_id, prev.as_deref(), "online");
        Ok(conn
            .query_row(
                "SELECT 1 FROM pending_pins WHERE peer_id = ?1 AND state = 'waiting' LIMIT 1",
                params![node_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }

    pub fn list_peers(&self) -> Result<Vec<PeerInfo>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT node_id, status, last_seen, failures, next_retry_at FROM peer_status
             ORDER BY last_seen DESC NULLS LAST, node_id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PeerInfo {
                    node_id: r.get(0)?,
                    status: r.get(1)?,
                    last_seen: r.get(2)?,
                    failures: r.get::<_, i64>(3)? as u32,
                    next_retry_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn get_peer(&self, node_id: &str) -> Result<Option<PeerInfo>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT node_id, status, last_seen, failures, next_retry_at FROM peer_status WHERE node_id = ?1",
        )?;
        Ok(stmt
            .query_row(params![node_id], |r| {
                Ok(PeerInfo {
                    node_id: r.get(0)?,
                    status: r.get(1)?,
                    last_seen: r.get(2)?,
                    failures: r.get::<_, i64>(3)? as u32,
                    next_retry_at: r.get(4)?,
                })
            })
            .optional()?)
    }
}

impl DatabaseManager {
    // ------------------------------------------------------------------
    // Ledger
    // ------------------------------------------------------------------

    /// Schreibt (bzw. aktualisiert) einen eigenen Katalogeintrag mit neuer
    /// Sequenznummer. `sign` erzeugt die Signatur über den fertigen Eintrag.
    #[allow(clippy::too_many_arguments)]
    pub fn record_own_entry(
        &self,
        origin: &str,
        entry_id: i64,
        hash: &str,
        name: &str,
        size: u64,
        created_at: i64,
        deleted: bool,
        share: &str,
        rel_path: &str,
        sign: &dyn Fn(&LedgerEntry) -> String,
    ) -> Result<LedgerEntry> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM ledger_entries WHERE origin = ?1",
            params![origin],
            |r| r.get(0),
        )?;
        let mut entry = LedgerEntry {
            origin: origin.to_string(),
            entry_id,
            seq,
            hash: hash.to_string(),
            name: name.to_string(),
            size,
            created_at,
            deleted,
            signature: String::new(),
            share: share.to_string(),
            rel_path: rel_path.to_string(),
        };
        entry.signature = sign(&entry);
        Self::upsert_entry(&tx, &entry)?;
        tx.commit()?;
        Ok(entry)
    }

    fn upsert_entry(conn: &Connection, e: &LedgerEntry) -> rusqlite::Result<usize> {
        conn.prepare_cached(
            "INSERT INTO ledger_entries (origin, entry_id, seq, hash, name, size, created_at, deleted, signature, share, rel_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(origin, entry_id) DO UPDATE SET
                seq = excluded.seq, hash = excluded.hash, name = excluded.name, size = excluded.size,
                created_at = excluded.created_at, deleted = excluded.deleted, signature = excluded.signature,
                share = excluded.share, rel_path = excluded.rel_path
             WHERE excluded.seq > ledger_entries.seq",
        )?
        .execute(params![
            e.origin,
            e.entry_id,
            e.seq,
            e.hash,
            e.name,
            e.size as i64,
            e.created_at,
            e.deleted,
            e.signature,
            e.share,
            e.rel_path
        ])
    }

    /// Übernimmt einen (bereits verifizierten) fremden Eintrag, falls er neuer ist.
    pub fn apply_remote_entry(&self, e: &LedgerEntry) -> Result<bool> {
        Ok(Self::upsert_entry(&self.conn(), e)? > 0)
    }

    /// Übernimmt mehrere (bereits verifizierte) Einträge. Gibt die Anzahl übernommener zurück.
    pub fn apply_remote_entries(&self, entries: &[LedgerEntry]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut n = 0;
        for e in entries {
            n += Self::upsert_entry(&tx, e)?;
        }
        tx.commit()?;
        if n > 0 {
            self.emit(Event::RemoteCatalogChanged { new_entries: n });
        }
        Ok(n)
    }

    /// Aktive eigene Dateien ohne Ledger-Eintrag (z. B. aus Schema 1).
    pub fn files_missing_entries(&self, origin: &str) -> Result<Vec<(i64, FileRecord)>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT f.id, f.blake3_hash, f.file_name, f.file_size, f.path, f.ticket, f.created_at, s.name, f.rel_path
             FROM files f LEFT JOIN shares s ON s.id = f.share_id
             WHERE f.deleted_at IS NULL
               AND NOT EXISTS (SELECT 1 FROM ledger_entries l WHERE l.origin = ?1 AND l.entry_id = f.id)",
        )?;
        let rows = stmt
            .query_map(params![origin], |r| {
                Ok((
                    r.get(0)?,
                    FileRecord {
                        blake3_hash: r.get(1)?,
                        file_name: r.get(2)?,
                        file_size: r.get::<_, i64>(3)? as u64,
                        path: r.get(4)?,
                        ticket: r.get(5)?,
                        created_at: r.get(6)?,
                        share: r.get(7)?,
                        rel_path: r.get(8)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Summe der höchsten Sequenznummern aller Ursprünge – ändert sich bei jeder
    /// Ledger-Änderung (Hinweis für Live-Verbindungen).
    pub fn ledger_total(&self) -> Result<i64> {
        Ok(self.read().query_row(
            "SELECT COALESCE(SUM(m), 0) FROM (SELECT MAX(seq) AS m FROM ledger_entries GROUP BY origin)",
            [],
            |r| r.get(0),
        )?)
    }

    /// Höchste bekannte Sequenznummer je Ursprungsgerät.
    pub fn version_vector(&self) -> Result<HashMap<String, i64>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached("SELECT origin, MAX(seq) FROM ledger_entries GROUP BY origin")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        Ok(rows)
    }

    /// Alle Einträge, die neuer sind als der übergebene Versionsvektor –
    /// je Ursprung aufsteigend nach `seq`, maximal `limit` Stück.
    pub fn entries_since(&self, have: &HashMap<String, i64>, limit: usize) -> Result<Vec<LedgerEntry>> {
        let conn = self.read();
        let origins: Vec<String> = {
            let mut stmt = conn.prepare_cached("SELECT DISTINCT origin FROM ledger_entries ORDER BY origin")?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        let mut stmt = conn.prepare_cached(
            "SELECT origin, entry_id, seq, hash, name, size, created_at, deleted, signature, share, rel_path
             FROM ledger_entries WHERE origin = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let mut out = Vec::new();
        for origin in origins {
            if out.len() >= limit {
                break;
            }
            let since = have.get(&origin).copied().unwrap_or(0);
            let remaining = (limit - out.len()) as i64;
            let rows = stmt
                .query_map(params![origin, since, remaining], Self::map_entry)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out.extend(rows);
        }
        Ok(out)
    }

    fn map_entry(r: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerEntry> {
        Ok(LedgerEntry {
            origin: r.get(0)?,
            entry_id: r.get(1)?,
            seq: r.get(2)?,
            hash: r.get(3)?,
            name: r.get(4)?,
            size: r.get::<_, i64>(5)? as u64,
            created_at: r.get(6)?,
            deleted: r.get(7)?,
            signature: r.get(8)?,
            share: r.get(9)?,
            rel_path: r.get(10)?,
        })
    }

    const REMOTE_SELECT: &'static str = "
        SELECT l.origin, d.name, l.hash, l.name, l.size, l.created_at, NULLIF(l.share, ''), NULLIF(l.rel_path, '')
        FROM ledger_entries l LEFT JOIN devices d ON d.node_id = l.origin
        WHERE l.deleted = 0 AND l.origin != ?1";

    fn map_remote(r: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteFile> {
        Ok(RemoteFile {
            origin: r.get(0)?,
            origin_name: r.get(1)?,
            hash: r.get(2)?,
            name: r.get(3)?,
            size: r.get::<_, i64>(4)? as u64,
            created_at: r.get(5)?,
            share: r.get(6)?,
            rel_path: r.get(7)?,
        })
    }

    /// Dateien anderer Geräte (ohne gelöschte).
    pub fn remote_files(&self, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(&format!(
            "{} ORDER BY COALESCE(d.name, l.origin), l.name COLLATE NOCASE",
            Self::REMOTE_SELECT
        ))?;
        let rows = stmt.query_map(params![self_origin], Self::map_remote)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_remote_by_hash_prefix(&self, prefix: &str, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let prefix = normalize_hash_prefix(prefix)?;
        let conn = self.read();
        let mut stmt = conn
            .prepare_cached(&format!("{} AND substr(l.hash, 1, ?3) = ?2 ORDER BY l.created_at", Self::REMOTE_SELECT))?;
        let rows = stmt
            .query_map(params![self_origin, prefix, prefix.len() as i64], Self::map_remote)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_remote_by_name(&self, name: &str, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let conn = self.read();
        let mut stmt =
            conn.prepare_cached(&format!("{} AND l.name = ?2 ORDER BY l.created_at", Self::REMOTE_SELECT))?;
        let rows =
            stmt.query_map(params![self_origin, name], Self::map_remote)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Dateien anderer Geräte per Freigabe und relativem Pfad.
    pub fn find_remote_by_share_path(&self, share: &str, rel_path: &str, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(&format!(
            "{} AND l.share = ?2 AND l.rel_path = ?3 ORDER BY l.created_at",
            Self::REMOTE_SELECT
        ))?;
        let rows = stmt
            .query_map(params![self_origin, share, rel_path], Self::map_remote)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Ein Ordner im Katalog eines anderen Geräts: direkte Unterordner und Dateien, sortiert nach
    /// Pfad (Byte-Reihenfolge). `share = None`: Wurzel – Freigaben als Ordner, dazu einzeln
    /// geteilte Dateien. `after`: Fortsetzung (`next` der vorigen Seite).
    ///
    /// Statt alle Einträge unter dem Ordner zu lesen, springt die Abfrage im Index
    /// `idx_ledger_tree` von Kind zu Kind: ein Unterordner kostet einen Indexzugriff, egal wie
    /// viele Dateien er enthält.
    pub fn remote_dir(
        &self,
        origin: &str,
        share: Option<&str>,
        dir: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<crate::ipc::RemoteDirPage> {
        let conn = self.read();
        let mut page = crate::ipc::RemoteDirPage::default();
        let limit = limit.max(1);
        let Some(share) = share else {
            // Wurzel: erst die Freigaben (Sprung von Name zu Name), dann einzelne Dateien.
            // Fortsetzung: "s:<freigabe>" bzw. "f:<rel_path>\0".
            let mut cursor = match after {
                None => Some(String::from("\0")),
                Some(a) => a.strip_prefix("s:").map(str::to_owned),
            };
            if let Some(c) = cursor.as_mut() {
                let mut stmt = conn.prepare_cached(
                    "SELECT share FROM ledger_entries WHERE deleted = 0 AND origin = ?1 AND share >= ?2
                     ORDER BY share LIMIT 1",
                )?;
                loop {
                    let next: Option<String> = stmt.query_row(params![origin, c.as_str()], |r| r.get(0)).optional()?;
                    let Some(name) = next else { break };
                    if page.dirs.len() + page.files.len() >= limit {
                        page.next = Some(format!("s:{name}"));
                        return Ok(page);
                    }
                    *c = format!("{name}\0");
                    page.dirs.push(name);
                }
            }
            let start = match after.and_then(|a| a.strip_prefix("f:")) {
                Some(f) => f.to_owned(),
                None => String::new(),
            };
            Self::single_files_from(&conn, origin, &start, limit, &mut page)?;
            return Ok(page);
        };
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        // Obergrenze: alles, was mit `prefix` beginnt, liegt vor `dir + "0"` ('0' folgt auf '/').
        let upper = if dir.is_empty() { None } else { Some(format!("{dir}0")) };
        let mut cursor = after.map(str::to_owned).unwrap_or_else(|| prefix.clone());
        // Obergrenze fest in der Abfrage (nicht "?4 IS NULL OR …"), sonst nutzt SQLite sie nicht
        // für den Indexbereich und liest nach dem letzten Kind alle folgenden Einträge.
        // Ohne Ordner (Freigabe-Wurzel) gilt: Freigabe-Name ist im Index die obere Grenze.
        let upper = upper.unwrap_or_else(|| String::from("\u{10FFFF}"));
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {} FROM ledger_entries l LEFT JOIN devices d ON d.node_id = l.origin
             WHERE l.deleted = 0 AND l.origin = ?1 AND l.share = ?2 AND l.rel_path >= ?3 AND l.rel_path < ?4
             ORDER BY l.rel_path LIMIT 1",
            Self::TREE_COLUMNS
        ))?;
        loop {
            let row =
                stmt.query_row(params![origin, share, cursor.as_str(), upper.as_str()], Self::map_tree).optional()?;
            let Some(item) = row else { break };
            let rel = item.rel_path.clone().unwrap_or_default();
            let Some(rest) = rel.strip_prefix(&prefix) else { break };
            if page.dirs.len() + page.files.len() >= limit {
                page.next = Some(rel);
                break;
            }
            match rest.find('/') {
                Some(i) => {
                    let seg = &rest[..i];
                    // Alles in diesem Unterordner überspringen (liegt vor "<seg>0").
                    cursor = format!("{prefix}{seg}0");
                    if !seg.is_empty() {
                        page.dirs.push(seg.to_owned());
                    }
                }
                None => {
                    cursor = format!("{rel}\0");
                    page.files.push(item);
                }
            }
        }
        Ok(page)
    }

    const TREE_COLUMNS: &'static str =
        "l.origin, d.name, l.hash, l.name, l.size, l.created_at, NULLIF(l.share, ''), NULLIF(l.rel_path, '')";

    fn map_tree(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::ipc::CatalogItem> {
        Ok(crate::ipc::CatalogItem {
            origin: r.get(0)?,
            origin_name: r.get(1)?,
            hash: r.get(2)?,
            name: r.get(3)?,
            size: r.get::<_, i64>(4)? as u64,
            created_at: r.get(5)?,
            path: None,
            share: r.get(6)?,
            rel_path: r.get(7)?,
        })
    }

    /// Einzeln geteilte Dateien (ohne Freigabe) eines Geräts ab `start`, sortiert nach Hash.
    fn single_files_from(
        conn: &rusqlite::Connection,
        origin: &str,
        start: &str,
        limit: usize,
        page: &mut crate::ipc::RemoteDirPage,
    ) -> Result<()> {
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {} FROM ledger_entries l LEFT JOIN devices d ON d.node_id = l.origin
             WHERE l.deleted = 0 AND l.origin = ?1 AND l.share = '' AND l.hash >= ?2
             ORDER BY l.hash LIMIT ?3",
            Self::TREE_COLUMNS
        ))?;
        let room = limit.saturating_sub(page.dirs.len() + page.files.len());
        let rows = stmt
            .query_map(params![origin, start, (room + 1) as i64], Self::map_tree)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for item in rows {
            if page.dirs.len() + page.files.len() >= limit {
                page.next = Some(format!("f:{}", item.hash));
                break;
            }
            page.files.push(item);
        }
        Ok(())
    }

    /// Seitenweise Katalogabfrage über eigene und fremde Dateien, sortiert nach Name
    /// (ohne Groß-/Kleinschreibung), dann Gerät, dann Hash.
    ///
    /// `device`: `None` = alle, `Some(None)` = nur eigene, `Some(Some(id))` = ein Gerät.
    /// Beide Tabellen werden getrennt über ihren Namensindex gelesen und in Rust
    /// zusammengeführt; mit `after` (Keyset) bleibt jede Seite unabhängig von ihrer
    /// Position gleich schnell.
    #[allow(clippy::too_many_arguments)]
    pub fn browse(
        &self,
        self_origin: &str,
        device: Option<Option<&str>>,
        search: Option<&str>,
        offset: u32,
        limit: u32,
        after: Option<&crate::ipc::BrowseCursor>,
    ) -> Result<(Vec<crate::ipc::CatalogItem>, u64, Option<crate::ipc::BrowseCursor>)> {
        use crate::ipc::{BrowseCursor, CatalogItem};
        // LIKE-Muster mit maskierten Platzhaltern: "%" und "_" werden wörtlich gesucht.
        let pattern = search.filter(|s| !s.is_empty()).map(|s| {
            let escaped = s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            format!("%{escaped}%")
        });
        let (include_local, include_remote, origin) = match device {
            None => (true, true, None),
            Some(None) => (true, false, None),
            Some(Some(id)) => (false, true, Some(id)),
        };
        let want = offset as i64 + limit as i64 + 1; // +1: gibt es eine weitere Seite?
        let (c_name, c_origin, c_hash) = match after {
            Some(c) => (Some(c.name.as_str()), c.origin.as_str(), c.hash.as_str()),
            None => (None, "", ""),
        };
        // Keyset-Bedingung in Sortierreihenfolge (Name NOCASE, Gerät, Hash). Der erste Teil
        // (`>=`) erlaubt SQLite den direkten Sprung im Namensindex; ohne Cursor entfällt sie.
        let after_cond = if after.is_some() {
            "AND {n} COLLATE NOCASE >= ?2 AND ({n} COLLATE NOCASE > ?2 OR ({o}, {h}) > (?3, ?4))"
        } else {
            ""
        };

        // Hinweis: `+deleted…` in den Seitenabfragen verhindert, dass SQLite den
        // Lösch-Index wählt und danach alles sortiert; so liefert der Namensindex die
        // Reihenfolge direkt (100k Einträge: 0,2 ms statt 32 ms).
        let count_key = format!("{include_local}|{include_remote}|{origin:?}|{pattern:?}");
        let cached_total = self.cached_count(&count_key);
        let conn = self.read();
        let mut items: Vec<CatalogItem> = Vec::new();
        let mut total = 0u64;
        if include_local {
            let cond = after_cond.replace("{n}", "file_name").replace("{o}", "''").replace("{h}", "blake3_hash");
            let sql = format!(
                "SELECT f.blake3_hash, f.file_name, f.file_size, f.created_at, f.path, s.name, f.rel_path
                 FROM files f LEFT JOIN shares s ON s.id = f.share_id
                 WHERE +f.deleted_at IS NULL AND (?1 IS NULL OR file_name LIKE ?1 ESCAPE '\\') {cond}
                 ORDER BY file_name COLLATE NOCASE, blake3_hash LIMIT ?5"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(params![pattern, c_name, c_origin, c_hash, want], |r| {
                Ok(CatalogItem {
                    origin: None,
                    origin_name: None,
                    hash: r.get(0)?,
                    name: r.get(1)?,
                    size: r.get::<_, i64>(2)? as u64,
                    created_at: r.get(3)?,
                    path: r.get(4)?,
                    share: r.get(5)?,
                    rel_path: r.get(6)?,
                })
            })?;
            for row in rows {
                items.push(row?);
            }
            if cached_total.is_none() {
                total += conn
                    .prepare_cached(
                        "SELECT COUNT(*) FROM files WHERE deleted_at IS NULL AND (?1 IS NULL OR file_name LIKE ?1 ESCAPE '\\')",
                    )?
                    .query_row(params![pattern], |r| r.get::<_, i64>(0))? as u64;
            }
        }
        if include_remote {
            let cond = after_cond.replace("{n}", "l.name").replace("{o}", "l.origin").replace("{h}", "l.hash");
            let sql = format!(
                "SELECT l.origin, d.name, l.hash, l.name, l.size, l.created_at, NULLIF(l.share, ''), NULLIF(l.rel_path, '')
                 FROM ledger_entries l LEFT JOIN devices d ON d.node_id = l.origin
                 WHERE +l.deleted = 0 AND l.origin != ?6 AND (?7 IS NULL OR l.origin = ?7)
                   AND (?1 IS NULL OR l.name LIKE ?1 ESCAPE '\\') {cond}
                 ORDER BY l.name COLLATE NOCASE, l.origin, l.hash LIMIT ?5"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let rows = stmt.query_map(params![pattern, c_name, c_origin, c_hash, want, self_origin, origin], |r| {
                Ok(CatalogItem {
                    origin: r.get(0)?,
                    origin_name: r.get(1)?,
                    hash: r.get(2)?,
                    name: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    created_at: r.get(5)?,
                    path: None,
                    share: r.get(6)?,
                    rel_path: r.get(7)?,
                })
            })?;
            for row in rows {
                items.push(row?);
            }
            if cached_total.is_none() {
                total += conn
                    .prepare_cached(
                        "SELECT COUNT(*) FROM ledger_entries
                         WHERE deleted = 0 AND origin != ?2 AND (?3 IS NULL OR origin = ?3)
                           AND (?1 IS NULL OR name LIKE ?1 ESCAPE '\\')",
                    )?
                    .query_row(params![pattern, self_origin, origin], |r| r.get::<_, i64>(0))?
                    as u64;
            }
        }
        drop(conn);
        let total = match cached_total {
            Some(t) => t,
            None => {
                self.store_count(count_key, total);
                total
            }
        };

        // Zusammenführen in derselben Ordnung wie SQLite (NOCASE = ASCII-Kleinschreibung).
        items.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
                .then_with(|| a.origin.as_deref().unwrap_or("").cmp(b.origin.as_deref().unwrap_or("")))
                .then_with(|| a.hash.cmp(&b.hash))
        });
        let mut page: Vec<CatalogItem> = items.into_iter().skip(offset as usize).collect();
        let has_more = page.len() > limit as usize;
        page.truncate(limit as usize);
        let next = if has_more {
            page.last().map(|i| BrowseCursor {
                name: i.name.clone(),
                origin: i.origin.clone().unwrap_or_default(),
                hash: i.hash.clone(),
            })
        } else {
            None
        };
        Ok((page, total, next))
    }

    // ------------------------------------------------------------------
    // Gekoppelte Geräte & Adress-Hinweise
    // ------------------------------------------------------------------

    /// Fügt ein Gerät hinzu oder aktualisiert seinen Namen.
    /// Wird auch bei jedem Abgleich zur Namensaktualisierung aufgerufen; das Ereignis
    /// `DevicePaired` entsteht nur beim ersten Eintragen.
    pub fn add_device(&self, node_id: &str, name: &str) -> Result<()> {
        let conn = self.conn();
        let inserted = conn
            .prepare_cached(
                "INSERT INTO devices (node_id, name, paired_at) VALUES (?1, ?2, ?3) ON CONFLICT(node_id) DO NOTHING",
            )?
            .execute(params![node_id, name, now_unix()])?
            > 0;
        if inserted {
            // Standardrecht jeder Kopplung: nur die Dateiliste lesen.
            conn.prepare_cached(
                "INSERT OR IGNORE INTO device_grants (node_id, capability, folder, granted_at) VALUES (?1, 'catalog.read', '', ?2)",
            )?
            .execute(params![node_id, now_unix()])?;
            // Freigaben mit auto_grant gelten auch für später gekoppelte Geräte.
            conn.prepare_cached(
                "INSERT OR IGNORE INTO device_grants (node_id, capability, folder, granted_at)
                 SELECT ?1, 'files.read', root, ?2 FROM shares WHERE auto_grant = 1",
            )?
            .execute(params![node_id, now_unix()])?;
            drop(conn);
            self.emit(Event::DevicePaired { node_id: node_id.to_string(), name: name.to_string() });
        } else {
            conn.prepare_cached("UPDATE devices SET name = ?2 WHERE node_id = ?1 AND name != ?2")?
                .execute(params![node_id, name])?;
        }
        Ok(())
    }

    pub fn is_paired(&self, node_id: &str) -> Result<bool> {
        Ok(self
            .read()
            .query_row("SELECT 1 FROM devices WHERE node_id = ?1", params![node_id], |_| Ok(true))
            .optional()?
            .unwrap_or(false))
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT node_id, name, paired_at, last_sync_at FROM devices ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DeviceInfo {
                    node_id: r.get(0)?,
                    name: r.get(1)?,
                    paired_at: r.get(2)?,
                    last_sync_at: r.get(3)?,
                    grants: Vec::new(),
                    online: false,
                    storage: None,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Sucht ein Gerät per exaktem Namen oder Node-ID-Präfix (mind. 6 Zeichen).
    pub fn find_devices(&self, query: &str) -> Result<Vec<DeviceInfo>> {
        let q = query.trim();
        let by_id = normalize_hash_prefix(q).ok();
        Ok(self
            .list_devices()?
            .into_iter()
            .filter(|d| d.name == q || by_id.as_deref().is_some_and(|p| d.node_id.starts_with(p)))
            .collect())
    }

    pub fn add_backup_job(&self, id: &str, target: &str, sources: &str) -> Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO backup_jobs (id, target, sources, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![id, target, sources, now_unix()],
        )?;
        Ok(())
    }

    pub fn backup_jobs(&self) -> Result<Vec<BackupJobRow>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT id, target, sources, created_at, last_run, last_ok, last_error, last_snapshot FROM backup_jobs ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BackupJobRow {
                    id: r.get(0)?,
                    target: r.get(1)?,
                    sources: r.get(2)?,
                    created_at: r.get(3)?,
                    last_run: r.get(4)?,
                    last_ok: r.get(5)?,
                    last_error: r.get(6)?,
                    last_snapshot: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_backup_sources(&self, id: &str, sources: &str) -> Result<bool> {
        Ok(self.conn().execute("UPDATE backup_jobs SET sources = ?2 WHERE id = ?1", params![id, sources])? > 0)
    }

    pub fn finish_backup_run(&self, id: &str, ok: bool, error: Option<&str>, snapshot: Option<&str>) -> Result<()> {
        self.conn().execute(
            "UPDATE backup_jobs SET last_run = ?2, last_ok = CASE WHEN ?3 THEN ?2 ELSE last_ok END,
             last_error = ?4, last_snapshot = COALESCE(?5, last_snapshot) WHERE id = ?1",
            params![id, now_unix(), ok, error, snapshot],
        )?;
        Ok(())
    }

    pub fn remove_backup_job(&self, id: &str) -> Result<bool> {
        Ok(self.conn().execute("DELETE FROM backup_jobs WHERE id = ?1", params![id])? > 0)
    }

    pub fn add_link(&self, l: &LinkRow) -> Result<()> {
        self.conn().execute(
            "INSERT INTO links (id, secret, name, local_path, size, created_at, expires_at, max_downloads, password, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                l.id,
                l.secret,
                l.name,
                l.local_path,
                l.size as i64,
                l.created_at,
                l.expires_at,
                l.max_downloads,
                l.password,
                l.state
            ],
        )?;
        Ok(())
    }

    /// Alle Links, neueste zuerst.
    pub fn links(&self) -> Result<Vec<LinkRow>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT id, secret, name, local_path, size, created_at, expires_at, max_downloads, password, state
             FROM links ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(LinkRow {
                    id: r.get(0)?,
                    secret: r.get(1)?,
                    name: r.get(2)?,
                    local_path: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    created_at: r.get(5)?,
                    expires_at: r.get(6)?,
                    max_downloads: r.get(7)?,
                    password: r.get(8)?,
                    state: r.get(9)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_link_state(&self, id: &str, state: &str) -> Result<()> {
        self.conn().execute("UPDATE links SET state = ?2 WHERE id = ?1", params![id, state])?;
        Ok(())
    }

    /// Zur Laufzeit gesetzte Einstellungen.
    pub fn settings(&self) -> Result<Vec<(String, i64)>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Setzt (`Some`) oder entfernt (`None`, zurück zum Standard) eine Einstellung.
    pub fn put_setting(&self, key: &str, value: Option<i64>) -> Result<()> {
        match value {
            Some(v) => self.conn().execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, v],
            )?,
            None => self.conn().execute("DELETE FROM settings WHERE key = ?1", params![key])?,
        };
        Ok(())
    }

    // ------------------------------------------------------------------
    // Hochladen
    // ------------------------------------------------------------------

    /// Plant ein Hochladen ein. Gibt die Nummer zurück.
    pub fn add_upload(&self, u: &NewUpload) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO uploads (device, share, dest_dir, local_path, rule_id, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
            params![u.device, u.share, u.dest_dir, u.local_path, u.rule_id, now_unix()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Fällige Aufträge (ältester zuerst).
    pub fn due_uploads(&self, limit: u32) -> Result<Vec<UploadRow>> {
        self.uploads_where("state = 'pending' AND next_try <= ?1 ORDER BY id LIMIT ?2", params![now_unix(), limit])
    }

    pub fn list_uploads(&self, limit: u32) -> Result<Vec<UploadRow>> {
        self.uploads_where("1 = 1 OR ?1 = 0 ORDER BY id DESC LIMIT ?2", params![0, limit])
    }

    fn uploads_where(&self, cond: &str, p: impl rusqlite::Params) -> Result<Vec<UploadRow>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT id, device, share, dest_dir, local_path, rule_id, state, attempts, last_error, result, created_at
             FROM uploads WHERE {cond}"
        ))?;
        let rows = stmt
            .query_map(p, |r| {
                Ok(UploadRow {
                    id: r.get(0)?,
                    device: r.get(1)?,
                    share: r.get(2)?,
                    dest_dir: r.get(3)?,
                    local_path: r.get(4)?,
                    rule_id: r.get(5)?,
                    state: r.get(6)?,
                    attempts: r.get(7)?,
                    last_error: r.get(8)?,
                    result: r.get(9)?,
                    created_at: r.get(10)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn finish_upload(&self, id: i64, result: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE uploads SET state = 'done', result = ?2, last_error = NULL WHERE id = ?1",
            params![id, result],
        )?;
        Ok(())
    }

    /// Fehlschlag: erneut versuchen (`retry_in` Sekunden) oder endgültig aufgeben.
    pub fn fail_upload(&self, id: i64, error: &str, retry_in: Option<i64>) -> Result<()> {
        match retry_in {
            Some(secs) => self.conn().execute(
                "UPDATE uploads SET attempts = attempts + 1, last_error = ?2, next_try = ?3 WHERE id = ?1",
                params![id, error, now_unix() + secs],
            )?,
            None => self.conn().execute(
                "UPDATE uploads SET attempts = attempts + 1, last_error = ?2, state = 'failed' WHERE id = ?1",
                params![id, error],
            )?,
        };
        Ok(())
    }

    /// Fehlgeschlagene/wartende Aufträge sofort erneut versuchen (z. B. Gerät wieder online).
    pub fn retry_uploads(&self, device: Option<&str>) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE uploads SET next_try = 0, state = 'pending'
             WHERE state IN ('pending', 'failed') AND (?1 IS NULL OR device = ?1)",
            params![device],
        )?)
    }

    pub fn cancel_upload(&self, id: i64) -> Result<bool> {
        Ok(self.conn().execute("DELETE FROM uploads WHERE id = ?1 AND state != 'done'", params![id])? > 0)
    }

    pub fn add_upload_rule(&self, local_dir: &str, device: &str, share: &str, dest_dir: &str) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO upload_rules (local_dir, device, share, dest_dir, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![local_dir, device, share, dest_dir, now_unix()],
        )?;
        Ok(conn.query_row(
            "SELECT id FROM upload_rules WHERE local_dir = ?1 AND device = ?2 AND share = ?3 AND dest_dir = ?4",
            params![local_dir, device, share, dest_dir],
            |r| r.get(0),
        )?)
    }

    pub fn upload_rules(&self) -> Result<Vec<UploadRule>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT id, local_dir, device, share, dest_dir, created_at FROM upload_rules ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(UploadRule {
                    id: r.get(0)?,
                    local_dir: r.get(1)?,
                    device: r.get(2)?,
                    share: r.get(3)?,
                    dest_dir: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn remove_upload_rule(&self, id: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let n = tx.execute("DELETE FROM upload_rules WHERE id = ?1", params![id])?;
        tx.execute("DELETE FROM upload_seen WHERE rule_id = ?1", params![id])?;
        tx.execute("DELETE FROM uploads WHERE rule_id = ?1 AND state = 'pending'", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Merkt sich eine Datei für eine Regel. `true`, wenn sie neu oder geändert ist.
    pub fn mark_upload_seen(&self, rule_id: i64, rel_path: &str, size: u64, mtime: i64) -> Result<bool> {
        let n = self.conn().execute(
            "INSERT INTO upload_seen (rule_id, rel_path, size, mtime) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(rule_id, rel_path) DO UPDATE SET size = excluded.size, mtime = excluded.mtime
             WHERE upload_seen.size != excluded.size OR upload_seen.mtime != excluded.mtime",
            params![rule_id, rel_path, size as i64, mtime],
        )?;
        Ok(n > 0)
    }

    /// Bucht empfangene Bytes für ein Gerät, wenn die Tagesgrenze es erlaubt.
    pub fn reserve_upload_bytes(&self, node_id: &str, bytes: u64, daily_limit: u64) -> Result<bool> {
        let day = now_unix() / 86_400;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let used: i64 = tx
            .query_row("SELECT bytes FROM upload_usage WHERE node_id = ?1 AND day = ?2", params![node_id, day], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        if used as u64 + bytes > daily_limit {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO upload_usage (node_id, day, bytes) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id, day) DO UPDATE SET bytes = bytes + excluded.bytes",
            params![node_id, day, bytes as i64],
        )?;
        tx.execute("DELETE FROM upload_usage WHERE day < ?1", params![day - 7])?;
        tx.commit()?;
        Ok(true)
    }

    /// Gibt gebuchte Bytes zurück (Hochladen fehlgeschlagen).
    pub fn release_upload_bytes(&self, node_id: &str, bytes: u64) -> Result<()> {
        self.conn().execute(
            "UPDATE upload_usage SET bytes = MAX(0, bytes - ?3) WHERE node_id = ?1 AND day = ?2",
            params![node_id, now_unix() / 86_400, bytes as i64],
        )?;
        Ok(())
    }

    /// Merkt sich, dass ein Blob gerade gestreamt wird (für die Cache-Grenze).
    pub fn touch_stream(&self, hash: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO stream_cache (hash, last_access) VALUES (?1, ?2)
             ON CONFLICT(hash) DO UPDATE SET last_access = excluded.last_access",
            params![hash, now_unix()],
        )?;
        Ok(())
    }

    /// Gestreamte Blobs, älteste zuerst.
    pub fn stream_entries(&self) -> Result<Vec<String>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached("SELECT hash FROM stream_cache ORDER BY last_access")?;
        let rows = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn remove_stream(&self, hash: &str) -> Result<()> {
        self.conn().execute("DELETE FROM stream_cache WHERE hash = ?1", params![hash])?;
        Ok(())
    }

    /// Legt eine wartende Fernoperation an (Massenschutz). Gibt die Nummer zurück.
    pub fn add_approval(&self, node_id: &str, op_json: &str, summary: &str) -> Result<i64> {
        let conn = self.conn();
        // Höchstens 100 offene Anfragen je Gerät (ein Angreifer soll die Tabelle nicht füllen).
        let open: i64 = conn.query_row(
            "SELECT COUNT(*) FROM op_approvals WHERE node_id = ?1 AND state = 'pending'",
            params![node_id],
            |r| r.get(0),
        )?;
        if open >= 100 {
            bail!("Zu viele unbestätigte Anfragen dieses Geräts");
        }
        conn.execute(
            "INSERT INTO op_approvals (node_id, op, summary, created_at, state) VALUES (?1, ?2, ?3, ?4, 'pending')",
            params![node_id, op_json, summary, now_unix()],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        self.emit(Event::ApprovalRequested { id, node_id: node_id.to_string(), summary: summary.to_string() });
        Ok(id)
    }

    /// Offene Bestätigungen (neueste zuerst): (id, node_id, summary, created_at).
    pub fn pending_approvals(&self) -> Result<Vec<(i64, String, String, i64)>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT id, node_id, summary, created_at FROM op_approvals WHERE state = 'pending' ORDER BY id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Entscheidet über eine offene Bestätigung. Gibt (node_id, op_json) zurück, wenn sie offen war.
    pub fn decide_approval(&self, id: i64, approve: bool) -> Result<Option<(String, String)>> {
        let conn = self.conn();
        let row: Option<(String, String)> = conn
            .query_row("SELECT node_id, op FROM op_approvals WHERE id = ?1 AND state = 'pending'", params![id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if row.is_some() {
            conn.execute(
                "UPDATE op_approvals SET state = ?2 WHERE id = ?1",
                params![id, if approve { "approved" } else { "rejected" }],
            )?;
        }
        Ok(row)
    }

    /// Entkoppelt ein Gerät und entfernt seine Ledger-Einträge und Adress-Hinweise.
    pub fn remove_device(&self, node_id: &str) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let removed = tx.execute("DELETE FROM devices WHERE node_id = ?1", params![node_id])? > 0;
        tx.execute("DELETE FROM ledger_entries WHERE origin = ?1", params![node_id])?;
        tx.execute("DELETE FROM peer_addrs WHERE node_id = ?1", params![node_id])?;
        tx.execute("DELETE FROM device_grants WHERE node_id = ?1", params![node_id])?;
        tx.execute(
            "UPDATE op_approvals SET state = 'rejected' WHERE node_id = ?1 AND state = 'pending'",
            params![node_id],
        )?;
        tx.commit()?;
        if removed {
            self.emit(Event::DeviceRemoved { node_id: node_id.to_string() });
            self.emit(Event::RemoteCatalogChanged { new_entries: 0 });
        }
        Ok(removed)
    }

    pub fn set_last_sync(&self, node_id: &str) -> Result<()> {
        self.conn().execute("UPDATE devices SET last_sync_at = ?2 WHERE node_id = ?1", params![node_id, now_unix()])?;
        Ok(())
    }

    /// Speichert direkte Adress-Hinweise (max. 16, nur gültige Socket-Adressen).
    pub fn set_addr_hints(&self, node_id: &str, addrs: &[String]) -> Result<()> {
        let valid: Vec<&String> = addrs.iter().filter(|a| a.parse::<std::net::SocketAddr>().is_ok()).take(16).collect();
        if valid.is_empty() {
            return Ok(());
        }
        self.conn().execute(
            "INSERT INTO peer_addrs (node_id, addrs, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET addrs = excluded.addrs, updated_at = excluded.updated_at",
            params![node_id, serde_json::to_string(&valid)?, now_unix()],
        )?;
        Ok(())
    }

    pub fn addr_hints(&self, node_id: &str) -> Result<Vec<String>> {
        let json: Option<String> = self
            .read()
            .query_row("SELECT addrs FROM peer_addrs WHERE node_id = ?1", params![node_id], |r| r.get(0))
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
    }

    pub fn has_waiting_jobs(&self, peer_id: &str) -> Result<bool> {
        Ok(self
            .read()
            .query_row(
                "SELECT 1 FROM pending_pins WHERE peer_id = ?1 AND state = 'waiting' LIMIT 1",
                params![peer_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }
}

impl DatabaseManager {
    // ------------------------------------------------------------------
    // Rechte & Audit
    // ------------------------------------------------------------------

    /// Ordner, in denen ein Gerät ein Recht hat (`""` = ohne Ordnerbeschränkung).
    /// Leere Liste = Recht nicht gewährt. Gilt nur für gekoppelte Geräte.
    pub fn grant_folders(&self, node_id: &str, capability: &str) -> Result<Vec<String>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT g.folder FROM device_grants g JOIN devices d ON d.node_id = g.node_id
             WHERE g.node_id = ?1 AND g.capability = ?2",
        )?;
        let rows =
            stmt.query_map(params![node_id, capability], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(rows)
    }

    pub fn grants_of(&self, node_id: &str) -> Result<Vec<Grant>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT capability, folder, granted_at FROM device_grants WHERE node_id = ?1 ORDER BY capability, folder",
        )?;
        let rows = stmt
            .query_map(params![node_id], |r| {
                let folder: String = r.get(1)?;
                Ok(Grant {
                    capability: r.get(0)?,
                    folder: (!folder.is_empty()).then_some(folder),
                    granted_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Gewährt ein Recht. Gibt `false` zurück, wenn es schon bestand.
    pub fn add_grant(&self, node_id: &str, capability: &str, folder: Option<&str>) -> Result<bool> {
        let added = self
            .conn()
            .prepare_cached(
                "INSERT OR IGNORE INTO device_grants (node_id, capability, folder, granted_at)
                 SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM devices WHERE node_id = ?1)",
            )?
            .execute(params![node_id, capability, folder.unwrap_or(""), now_unix()])?
            > 0;
        if added {
            self.emit(Event::GrantsChanged { node_id: node_id.to_string() });
        }
        Ok(added)
    }

    /// Entzieht ein Recht (`folder = None`: in allen Ordnern). Gibt die Anzahl entfernter zurück.
    pub fn remove_grant(&self, node_id: &str, capability: &str, folder: Option<&str>) -> Result<usize> {
        let n = self
            .conn()
            .prepare_cached(
                "DELETE FROM device_grants WHERE node_id = ?1 AND capability = ?2 AND (?3 IS NULL OR folder = ?3)",
            )?
            .execute(params![node_id, capability, folder])?;
        if n > 0 {
            self.emit(Event::GrantsChanged { node_id: node_id.to_string() });
        }
        Ok(n)
    }

    /// Schreibt einen Audit-Eintrag; hält das Log bei höchstens 10 000 Einträgen.
    pub fn audit(
        &self,
        node_id: &str,
        operation: &str,
        target: Option<&str>,
        allowed: bool,
        detail: Option<&str>,
    ) -> Result<()> {
        let now = now_unix();
        {
            let key = format!("{node_id}|{operation}|{}|{allowed}", target.unwrap_or(""));
            let mut recent = self.audit_recent.lock().unwrap_or_else(|e| e.into_inner());
            if recent.get(&key).is_some_and(|t| now - t < AUDIT_DEDUP_SECS) {
                return Ok(());
            }
            if recent.len() >= 4096 {
                recent.retain(|_, t| now - *t < AUDIT_DEDUP_SECS);
                if recent.len() >= 4096 {
                    recent.clear();
                }
            }
            recent.insert(key, now);
        }
        let conn = self.conn();
        conn.prepare_cached(
            "INSERT INTO audit_log (ts, node_id, operation, target, allowed, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?
        .execute(params![now_unix(), node_id, operation, target, allowed, detail])?;
        let id = conn.last_insert_rowid();
        if id % 500 == 0 {
            conn.prepare_cached("DELETE FROM audit_log WHERE id <= ?1")?.execute(params![id - 10_000])?;
        }
        Ok(())
    }

    pub fn audit_log(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT ts, node_id, operation, target, allowed, detail FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(AuditEntry {
                    ts: r.get(0)?,
                    node_id: r.get(1)?,
                    operation: r.get(2)?,
                    target: r.get(3)?,
                    allowed: r.get(4)?,
                    detail: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

impl DatabaseManager {
    // ------------------------------------------------------------------
    // Ordner-Freigaben
    // ------------------------------------------------------------------

    /// Legt eine Freigabe an und vergibt `files.read` an die angegebenen Geräte.
    pub fn create_share(&self, name: &str, root: &str, auto_grant: bool, grant_to: &[String]) -> Result<ShareInfo> {
        let now = now_unix();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: Option<String> = tx
            .query_row(
                "SELECT name FROM shares WHERE name = ?1 OR root = ?2
                    OR substr(?2, 1, length(root) + 1) = root || '/'
                    OR substr(root, 1, length(?2) + 1) = ?2 || '/'",
                params![name, root],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = exists {
            crate::api_bail!(
                AlreadyExists,
                "Überschneidet sich mit der bestehenden Freigabe '{other}' (gleicher Name oder Ordner ineinander)"
            );
        }
        tx.execute(
            "INSERT INTO shares (name, root, auto_grant, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, root, auto_grant, now],
        )?;
        let id = tx.last_insert_rowid();
        for node in grant_to {
            tx.execute(
                "INSERT OR IGNORE INTO device_grants (node_id, capability, folder, granted_at)
                 SELECT ?1, 'files.read', ?2, ?3 WHERE EXISTS (SELECT 1 FROM devices WHERE node_id = ?1)",
                params![node, root, now],
            )?;
        }
        tx.commit()?;
        for node in grant_to {
            self.emit(Event::GrantsChanged { node_id: node.clone() });
        }
        Ok(ShareInfo {
            id,
            name: name.into(),
            root: root.into(),
            auto_grant,
            created_at: now,
            file_count: 0,
            total_size: 0,
        })
    }

    pub fn list_shares(&self) -> Result<Vec<ShareInfo>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT s.id, s.name, s.root, s.auto_grant, s.created_at,
                    (SELECT COUNT(*) FROM files f WHERE f.share_id = s.id AND f.deleted_at IS NULL),
                    (SELECT COALESCE(SUM(file_size), 0) FROM files f WHERE f.share_id = s.id AND f.deleted_at IS NULL)
             FROM shares s ORDER BY s.name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ShareInfo {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    root: r.get(2)?,
                    auto_grant: r.get(3)?,
                    created_at: r.get(4)?,
                    file_count: r.get::<_, i64>(5)? as u64,
                    total_size: r.get::<_, i64>(6)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Bekannte aktive Dateien einer Freigabe, nach relativem Pfad.
    pub fn known_share_files(&self, share_id: i64) -> Result<HashMap<String, KnownShareFile>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT id, rel_path, blake3_hash, file_size, COALESCE(mtime, 0) FROM files
             WHERE share_id = ?1 AND deleted_at IS NULL",
        )?;
        let rows = stmt
            .query_map(params![share_id], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    KnownShareFile {
                        id: r.get(0)?,
                        hash: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                        mtime: r.get(4)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        Ok(rows)
    }

    /// Übernimmt Scan-Ergebnisse einer Freigabe in **einer** Transaktion: Dateien
    /// anlegen/aktualisieren/löschen und die passenden signierten Ledger-Einträge schreiben.
    pub fn apply_scan(
        &self,
        origin: &str,
        share: &ShareInfo,
        changes: &[ScanChange],
        sign: &dyn Fn(&LedgerEntry) -> String,
    ) -> Result<usize> {
        if changes.is_empty() {
            return Ok(0);
        }
        let now = now_unix();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut seq: i64 =
            tx.query_row("SELECT COALESCE(MAX(seq), 0) FROM ledger_entries WHERE origin = ?1", params![origin], |r| {
                r.get(0)
            })?;
        let mut applied = 0;
        for change in changes {
            let (id, hash, name, size, created_at, rel_path, deleted) = match change {
                ScanChange::Upsert { rel_path, abs_path, name, hash, size, mtime } => {
                    tx.prepare_cached(
                        "INSERT INTO files (blake3_hash, path, file_name, file_size, ticket, created_at, share_id, rel_path, mtime)
                         VALUES (?1, ?2, ?3, ?4, '', ?5, ?6, ?7, ?8)
                         ON CONFLICT(path) DO UPDATE SET
                            blake3_hash = excluded.blake3_hash, file_name = excluded.file_name,
                            file_size = excluded.file_size, created_at = excluded.created_at,
                            share_id = excluded.share_id, rel_path = excluded.rel_path,
                            mtime = excluded.mtime, deleted_at = NULL",
                    )?
                    .execute(params![hash, abs_path, name, *size as i64, now, share.id, rel_path, mtime])?;
                    let id: i64 = tx
                        .prepare_cached("SELECT id FROM files WHERE path = ?1")?
                        .query_row(params![abs_path], |r| r.get(0))?;
                    (id, hash.clone(), name.clone(), *size, now, rel_path.clone(), false)
                }
                ScanChange::Touch { file_id, mtime } => {
                    tx.prepare_cached("UPDATE files SET mtime = ?2 WHERE id = ?1")?.execute(params![file_id, mtime])?;
                    continue;
                }
                ScanChange::Delete { file_id } => {
                    let row: Option<(String, String, i64, i64, String)> = tx
                        .prepare_cached(
                            "SELECT blake3_hash, file_name, file_size, created_at, COALESCE(rel_path, '') FROM files
                             WHERE id = ?1 AND deleted_at IS NULL",
                        )?
                        .query_row(params![file_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
                        .optional()?;
                    let Some((hash, name, size, created_at, rel_path)) = row else { continue };
                    tx.prepare_cached("UPDATE files SET deleted_at = ?2 WHERE id = ?1")?
                        .execute(params![file_id, now])?;
                    (*file_id, hash, name, size as u64, created_at, rel_path, true)
                }
            };
            seq += 1;
            let mut entry = LedgerEntry {
                origin: origin.to_string(),
                entry_id: id,
                seq,
                hash,
                name,
                size,
                created_at,
                deleted,
                signature: String::new(),
                share: share.name.clone(),
                rel_path,
            };
            entry.signature = sign(&entry);
            Self::upsert_entry(&tx, &entry)?;
            applied += 1;
        }
        tx.commit()?;
        if applied > 0 {
            self.emit(Event::LocalCatalogChanged);
        }
        Ok(applied)
    }

    /// Entfernt eine Freigabe: Dateien in den Papierkorb (IDs zurück), automatische
    /// Leserechte auf den Ordner entziehen.
    pub fn remove_share(&self, share_id: i64) -> Result<Vec<i64>> {
        let now = now_unix();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let root: String = tx.query_row("SELECT root FROM shares WHERE id = ?1", params![share_id], |r| r.get(0))?;
        let ids: Vec<i64> = {
            let mut stmt = tx.prepare_cached("SELECT id FROM files WHERE share_id = ?1 AND deleted_at IS NULL")?;
            let rows = stmt.query_map(params![share_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
            rows
        };
        tx.execute(
            "UPDATE files SET deleted_at = ?2, share_id = NULL WHERE share_id = ?1 AND deleted_at IS NULL",
            params![share_id, now],
        )?;
        tx.execute("DELETE FROM device_grants WHERE capability = 'files.read' AND folder = ?1", params![root])?;
        tx.execute("DELETE FROM shares WHERE id = ?1", params![share_id])?;
        tx.commit()?;
        self.emit(Event::LocalCatalogChanged);
        Ok(ids)
    }
}

/// Ein Pin: Freigabe (oder Unterordner) eines anderen Geräts, die lokal gespiegelt wird.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinRow {
    pub id: i64,
    pub origin: String,
    pub share: String,
    /// Unterordner relativ zur Freigabe ("" = ganze Freigabe).
    pub prefix: String,
    /// Ordnername unter dem Pin-Verzeichnis (stabil, auch wenn das Gerät umbenannt wird).
    pub dir_name: String,
    pub created_at: i64,
}

/// Ein Ledger-Eintrag, der zu einem Pin gehört.
#[derive(Clone, Debug)]
pub struct PinTarget {
    pub entry_id: i64,
    pub hash: String,
    pub rel_path: String,
    pub size: u64,
    pub deleted: bool,
}

/// Eine gespiegelte Datei.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedFile {
    pub path: String,
    pub pin_id: i64,
    pub origin: String,
    pub entry_id: i64,
    pub hash: String,
    pub size: u64,
    pub mtime: i64,
    pub state: String,
}

impl DatabaseManager {
    // ------------------------------------------------------------------
    // Smart-Pinning
    // ------------------------------------------------------------------

    /// Geräte (außer uns), die eine Freigabe dieses Namens mit aktiven Dateien anbieten.
    pub fn share_origins(&self, share: &str, self_origin: &str) -> Result<Vec<(String, Option<String>)>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT DISTINCT l.origin, d.name FROM ledger_entries l LEFT JOIN devices d ON d.node_id = l.origin
             WHERE l.share = ?1 AND l.deleted = 0 AND l.origin != ?2 ORDER BY d.name",
        )?;
        let rows = stmt
            .query_map(params![share, self_origin], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn add_pin(&self, origin: &str, share: &str, prefix: &str, dir_name: &str) -> Result<i64> {
        let conn = self.conn();
        let n = conn.execute(
            "INSERT OR IGNORE INTO pins (origin, share, prefix, dir_name, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![origin, share, prefix, dir_name, now_unix()],
        )?;
        if n == 0 {
            crate::api_bail!(AlreadyExists, "Dieser Pin besteht bereits");
        }
        Ok(conn.last_insert_rowid())
    }

    pub fn list_pins(&self) -> Result<Vec<PinRow>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT id, origin, share, prefix, dir_name, created_at FROM pins ORDER BY share, prefix",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PinRow {
                    id: r.get(0)?,
                    origin: r.get(1)?,
                    share: r.get(2)?,
                    prefix: r.get(3)?,
                    dir_name: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Entfernt einen Pin und vergisst seine gespiegelten Dateien (die Dateien bleiben liegen).
    pub fn remove_pin(&self, id: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        // Reihenfolge: erst die wartenden Aufträge (brauchen pinned_files), dann den Rest.
        tx.execute(
            "DELETE FROM pending_pins WHERE action = 'PIN' AND output_path IN (SELECT path FROM pinned_files WHERE pin_id = ?1)",
            params![id],
        )?;
        tx.execute("DELETE FROM pinned_files WHERE pin_id = ?1", params![id])?;
        let n = tx.execute("DELETE FROM pins WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(n > 0)
    }

    /// Alle Ledger-Einträge (inkl. gelöschter) im Bereich eines Pins.
    pub fn pin_targets(&self, pin: &PinRow) -> Result<Vec<PinTarget>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT entry_id, hash, rel_path, size, deleted FROM ledger_entries
             WHERE origin = ?1 AND share = ?2 AND rel_path != ''
               AND (?3 = '' OR rel_path = ?3 OR substr(rel_path, 1, length(?3) + 1) = ?3 || '/')",
        )?;
        let rows = stmt
            .query_map(params![pin.origin, pin.share, pin.prefix], |r| {
                Ok(PinTarget {
                    entry_id: r.get(0)?,
                    hash: r.get(1)?,
                    rel_path: r.get(2)?,
                    size: r.get::<_, i64>(3)? as u64,
                    deleted: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn map_pinned(r: &rusqlite::Row<'_>) -> rusqlite::Result<PinnedFile> {
        Ok(PinnedFile {
            path: r.get(0)?,
            pin_id: r.get(1)?,
            origin: r.get(2)?,
            entry_id: r.get(3)?,
            hash: r.get(4)?,
            size: r.get::<_, i64>(5)? as u64,
            mtime: r.get(6)?,
            state: r.get(7)?,
        })
    }

    pub fn pinned_by_path(&self, path: &str) -> Result<Option<PinnedFile>> {
        Ok(self
            .read()
            .prepare_cached(
                "SELECT path, pin_id, origin, entry_id, hash, size, mtime, state FROM pinned_files WHERE path = ?1",
            )?
            .query_row(params![path], Self::map_pinned)
            .optional()?)
    }

    pub fn pinned_of_pin(&self, pin_id: i64) -> Result<Vec<PinnedFile>> {
        let conn = self.read();
        let mut stmt = conn.prepare_cached(
            "SELECT path, pin_id, origin, entry_id, hash, size, mtime, state FROM pinned_files WHERE pin_id = ?1",
        )?;
        let rows = stmt.query_map(params![pin_id], Self::map_pinned)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Merkt eine (noch zu ladende) Spiegelung vor. Neue Einträge haben `mtime = 0`
    /// (noch nie geschrieben).
    pub fn pinned_set_pending(&self, f: &PinnedFile) -> Result<()> {
        self.conn()
            .prepare_cached(
                "INSERT INTO pinned_files (path, pin_id, origin, entry_id, hash, size, mtime, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending')
             ON CONFLICT(path) DO UPDATE SET pin_id = excluded.pin_id, origin = excluded.origin,
                entry_id = excluded.entry_id, hash = excluded.hash, state = 'pending'",
                // size/mtime bleiben bei Aktualisierungen die der zuletzt geschriebenen Fassung –
                // daran wird erkannt, ob die Kopie inzwischen lokal verändert wurde.
            )?
            .execute(params![f.path, f.pin_id, f.origin, f.entry_id, f.hash, f.size as i64, f.mtime])?;
        Ok(())
    }

    /// Spiegelung erfolgreich geschrieben: Größe und Änderungszeit der geschriebenen Datei merken.
    pub fn pinned_mark_ok(&self, path: &str, hash: &str, size: u64, mtime: i64) -> Result<()> {
        self.conn()
            .prepare_cached("UPDATE pinned_files SET state = 'ok', hash = ?2, size = ?3, mtime = ?4 WHERE path = ?1")?
            .execute(params![path, hash, size as i64, mtime])?;
        Ok(())
    }

    pub fn pinned_mark_conflict(&self, path: &str) -> Result<()> {
        self.conn()
            .prepare_cached("UPDATE pinned_files SET state = 'conflict' WHERE path = ?1")?
            .execute(params![path])?;
        Ok(())
    }

    pub fn pinned_remove(&self, path: &str) -> Result<()> {
        self.conn().prepare_cached("DELETE FROM pinned_files WHERE path = ?1")?.execute(params![path])?;
        Ok(())
    }

    /// Belegter bzw. vorgemerkter Platz aller Spiegelungen (für das Kontingent).
    pub fn pinned_total_size(&self) -> Result<u64> {
        Ok(self.read().query_row(
            "SELECT COALESCE(SUM(size), 0) FROM pinned_files WHERE state IN ('ok', 'pending')",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Entfernt wartende Pin-Aufträge für einen Pfad mit veraltetem Inhalt.
    pub fn cancel_stale_pin_jobs(&self, path: &str, current_hash: &str) -> Result<()> {
        self.conn()
            .prepare_cached("DELETE FROM pending_pins WHERE action = 'PIN' AND output_path = ?1 AND target_hash != ?2")?
            .execute(params![path, current_hash])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "aaaaaa1111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "aaaaaa2222222222222222222222222222222222222222222222222222222222";

    fn job<'a>(hash: &'a str, out: &'a str, peer: &'a str) -> NewJob<'a> {
        NewJob {
            action: "DOWNLOAD",
            target_hash: hash,
            ticket: "ticket",
            peer_id: peer,
            output_path: out,
            explicit_output: false,
            force: false,
            last_error: "offline",
        }
    }

    #[test]
    fn hash_validation() {
        assert!(normalize_hash_prefix("%").is_err());
        assert!(normalize_hash_prefix("").is_err());
        assert!(normalize_hash_prefix("abc").is_err());
        assert!(normalize_hash_prefix("abcde_").is_err());
        assert!(normalize_hash_prefix(&"a".repeat(65)).is_err());
        assert_eq!(normalize_hash_prefix(" ABCDEF ").unwrap(), "abcdef");
    }

    #[test]
    fn upsert_does_not_duplicate() {
        let db = DatabaseManager::open_in_memory().unwrap();
        assert!(!db.upsert_file("/a", H1, "a", 1, "t").unwrap().0);
        assert!(db.upsert_file("/a", H1, "a", 1, "t").unwrap().0);
        assert_eq!(db.list_files().unwrap().len(), 1);
        // Inhalt geändert -> gleicher Eintrag, neuer Hash
        assert!(!db.upsert_file("/a", H2, "a", 2, "t2").unwrap().0);
        let files = db.list_files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].blake3_hash, H2);
    }

    #[test]
    fn soft_delete_stops_sharing_and_purge_reports_orphans() {
        let db = DatabaseManager::open_in_memory().unwrap();
        db.upsert_file("/a", H1, "a", 1, "t").unwrap();
        db.upsert_file("/b", H1, "b", 1, "t").unwrap();
        assert!(db.is_shared(H1).unwrap());
        assert_eq!(db.soft_delete_by_path("/a").unwrap().len(), 1);
        assert!(db.is_shared(H1).unwrap(), "zweite Kopie ist noch freigegeben");
        assert_eq!(db.purge_expired(0).unwrap(), Vec::<String>::new());
        assert_eq!(db.soft_delete_by_hash(H1).unwrap().len(), 1);
        assert!(!db.is_shared(H1).unwrap());
        assert_eq!(db.purge_expired(30).unwrap(), Vec::<String>::new(), "Frist läuft noch");
        assert_eq!(db.purge_expired(0).unwrap(), vec![H1.to_string()]);
    }

    #[test]
    fn republish_restores_deleted_file() {
        let db = DatabaseManager::open_in_memory().unwrap();
        db.upsert_file("/a", H1, "a", 1, "t").unwrap();
        db.soft_delete_by_path("/a").unwrap();
        assert!(!db.is_shared(H1).unwrap());
        assert!(!db.upsert_file("/a", H1, "a", 1, "t").unwrap().0);
        assert!(db.is_shared(H1).unwrap());
    }

    #[test]
    fn prefix_lookup() {
        let db = DatabaseManager::open_in_memory().unwrap();
        db.upsert_file("/a", H1, "a", 1, "t").unwrap();
        db.upsert_file("/b", H2, "b", 1, "t").unwrap();
        assert_eq!(db.find_by_hash_prefix("aaaaaa").unwrap().len(), 2);
        assert_eq!(db.find_by_hash_prefix("aaaaaa1").unwrap().len(), 1);
        assert!(db.find_by_hash_prefix("aaaa%").is_err());
    }

    #[test]
    fn queue_dedup_and_backoff_scheduling() {
        let db = DatabaseManager::open_in_memory().unwrap();
        assert_eq!(db.next_wakeup().unwrap(), None);

        let (id, existed) = db.enqueue_download(&job(H1, "/out/a", "peer1")).unwrap();
        assert!(!existed);
        let (id2, existed) = db.enqueue_download(&job(H1, "/out/a", "peer1")).unwrap();
        assert!(existed);
        assert_eq!(id, id2);

        // Peer unbekannt -> sofort fällig
        assert_eq!(db.due_jobs(now_unix()).unwrap().len(), 1);

        let next = db.mark_peer_unreachable("peer1", true).unwrap();
        assert!(next >= now_unix() + 24);
        assert!(db.due_jobs(now_unix()).unwrap().is_empty());
        assert_eq!(db.next_wakeup().unwrap(), Some(next));
        assert_eq!(db.due_jobs(next).unwrap().len(), 1);

        // Zweiter Fehlschlag -> längeres Backoff
        let next2 = db.mark_peer_unreachable("peer1", true).unwrap();
        assert!(next2 >= now_unix() + 96);
        assert_eq!(db.get_peer("peer1").unwrap().unwrap().failures, 2);

        // Manueller Fehlschlag verlängert ein laufendes Backoff nicht
        assert_eq!(db.mark_peer_unreachable("peer1", false).unwrap(), next2);
        assert_eq!(db.get_peer("peer1").unwrap().unwrap().failures, 2);

        // Retry setzt Backoff zurück
        assert!(db.retry_job(id).unwrap());
        assert_eq!(db.due_jobs(now_unix()).unwrap().len(), 1);

        // Endgültiger Fehler -> nicht mehr fällig, kein Wakeup
        db.job_failed(id, "abgelehnt").unwrap();
        assert!(db.due_jobs(now_unix()).unwrap().is_empty());
        assert_eq!(db.next_wakeup().unwrap(), None);
        assert_eq!(db.list_jobs().unwrap()[0].state, JobState::Failed);

        assert!(db.cancel_job(id).unwrap());
        assert!(!db.cancel_job(id).unwrap());
    }

    #[test]
    fn online_resets_backoff_and_unknown_peers_are_ignored() {
        let db = DatabaseManager::open_in_memory().unwrap();
        assert!(!db.mark_known_peer_seen("stranger").unwrap());
        assert!(db.list_peers().unwrap().is_empty());

        db.mark_peer_unreachable("p", true).unwrap();
        let p = db.get_peer("p").unwrap().unwrap();
        assert_eq!(p.status, "offline");
        assert_eq!(p.last_seen, None);

        db.enqueue_download(&job(H1, "/x", "p")).unwrap();
        assert!(db.mark_known_peer_seen("p").unwrap());
        let p = db.get_peer("p").unwrap().unwrap();
        assert_eq!(p.status, "online");
        assert_eq!(p.failures, 0);
        assert!(p.last_seen.is_some());

        db.mark_peer_unreachable("p", true).unwrap();
        db.mark_peer_online("p").unwrap();
        assert_eq!(db.get_peer("p").unwrap().unwrap().next_retry_at, None);
    }

    #[tokio::test]
    async fn mutations_emit_events() {
        use crate::events::Event;
        let db = DatabaseManager::open_in_memory().unwrap();
        let mut sub = db.events().subscribe();
        db.upsert_file("/a", H1, "a", 1, "t").unwrap();
        db.upsert_file("/a", H1, "a", 1, "t").unwrap(); // unverändert -> kein Ereignis
        db.mark_peer_online("p").unwrap();
        db.mark_peer_online("p").unwrap(); // kein Statuswechsel -> kein Ereignis
        db.mark_peer_unreachable("p", true).unwrap();
        let (job, _) = db.enqueue_download(&job(H1, "/x", "p")).unwrap();
        db.complete_job(job).unwrap();
        db.add_device("d", "Handy").unwrap();
        db.add_device("d", "Handy").unwrap(); // Namensabgleich -> kein Ereignis
        db.remove_device("d").unwrap();
        let expected = vec![
            Event::LocalCatalogChanged,
            Event::PeerStatusChanged { node_id: "p".into(), status: "online".into() },
            Event::PeerStatusChanged { node_id: "p".into(), status: "offline".into() },
            Event::JobQueued { job_id: job },
            Event::JobCompleted { job_id: job },
            Event::DevicePaired { node_id: "d".into(), name: "Handy".into() },
            Event::DeviceRemoved { node_id: "d".into() },
            Event::RemoteCatalogChanged { new_entries: 0 },
        ];
        for e in expected {
            assert_eq!(sub.next().await.unwrap(), e);
        }
    }

    #[test]
    fn remote_dir_lists_children_and_pages() {
        let db = DatabaseManager::open_in_memory().unwrap();
        let key = iroh::SecretKey::generate();
        let origin = key.public().to_string();
        let files: &[(&str, &str)] = &[
            ("Fotos", "a.jpg"),
            ("Fotos", "2024/x.jpg"),
            ("Fotos", "2024/tief/y.jpg"),
            ("Fotos", "2024.txt"),
            ("Fotos", "2024-alt/z.jpg"),
            ("Fotos", "b.jpg"),
            ("Fotos", "/leer.jpg"),
            ("Musik", "lied.mp3"),
            ("", ""),
        ];
        let entries: Vec<LedgerEntry> = files
            .iter()
            .enumerate()
            .map(|(i, (share, rel))| {
                let i = i as i64;
                let mut e = LedgerEntry {
                    origin: origin.clone(),
                    entry_id: i + 1,
                    seq: i + 1,
                    hash: format!("{:064x}", i),
                    name: rel.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("einzeln.pdf").to_owned(),
                    size: 1,
                    created_at: 0,
                    deleted: false,
                    signature: String::new(),
                    share: share.to_string(),
                    rel_path: rel.to_string(),
                };
                e.signature = e.sign(&key);
                e
            })
            .collect();
        db.apply_remote_entries(&entries).unwrap();

        // Wurzel: Freigaben als Ordner, dazu die einzeln geteilte Datei
        let root = db.remote_dir(&origin, None, "", None, 100).unwrap();
        assert_eq!(root.dirs, vec!["Fotos", "Musik"]);
        assert_eq!(root.files.len(), 1);
        assert_eq!(root.files[0].name, "einzeln.pdf");
        assert!(root.next.is_none());
        // Seitenweise in der Wurzel: 1 + 1 + 1
        let p1 = db.remote_dir(&origin, None, "", None, 1).unwrap();
        assert_eq!((p1.dirs.clone(), p1.files.len()), (vec!["Fotos".to_owned()], 0));
        let p2 = db.remote_dir(&origin, None, "", p1.next.as_deref(), 1).unwrap();
        assert_eq!(p2.dirs, vec!["Musik"]);
        let p3 = db.remote_dir(&origin, None, "", p2.next.as_deref(), 1).unwrap();
        assert_eq!((p3.dirs.len(), p3.files.len(), p3.next.clone()), (0, 1, None));

        // Freigabe: Unterordner einmal, Dateien direkt darin; leere Teile werden übergangen
        let top = db.remote_dir(&origin, Some("Fotos"), "", None, 100).unwrap();
        // Reihenfolge nach Bytes ('-' < '.' < '/'); die Oberfläche sortiert selbst
        assert_eq!(top.dirs, vec!["2024-alt", "2024"]);
        let names: Vec<_> = top.files.iter().map(|f| f.rel_path.clone().unwrap()).collect();
        assert_eq!(names, vec!["2024.txt", "a.jpg", "b.jpg"]);
        let sub = db.remote_dir(&origin, Some("Fotos"), "2024", None, 100).unwrap();
        assert_eq!(sub.dirs, vec!["tief"]);
        assert_eq!(sub.files.len(), 1);
        assert_eq!(sub.files[0].name, "x.jpg");

        // Seitenweise: jede Seite setzt genau dort fort, nichts doppelt oder verloren
        let mut seen = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let p = db.remote_dir(&origin, Some("Fotos"), "", after.as_deref(), 2).unwrap();
            seen.extend(p.dirs.iter().cloned());
            seen.extend(p.files.iter().map(|f| f.rel_path.clone().unwrap()));
            match p.next {
                Some(n) => after = Some(n),
                None => break,
            }
        }
        seen.sort();
        assert_eq!(seen, vec!["2024", "2024-alt", "2024.txt", "a.jpg", "b.jpg"]);
        // Fremdes Gerät: leer
        assert_eq!(db.remote_dir("unbekannt", None, "", None, 10).unwrap(), Default::default());
    }

    #[test]
    fn browse_pages_filters_and_escapes() {
        let db = DatabaseManager::open_in_memory().unwrap();
        let key = iroh::SecretKey::generate();
        let origin = key.public().to_string();
        let entries: Vec<LedgerEntry> = (0..25)
            .map(|i| {
                let mut e = LedgerEntry {
                    origin: origin.clone(),
                    entry_id: i + 1,
                    seq: i + 1,
                    hash: format!("{:064x}", i),
                    name: format!("bild_{i:02}.jpg"),
                    size: 1,
                    created_at: 0,
                    deleted: false,
                    signature: String::new(),
                    ..Default::default()
                };
                e.signature = e.sign(&key);
                e
            })
            .collect();
        db.apply_remote_entries(&entries).unwrap();
        db.upsert_file("/home/notiz%.txt", H1, "notiz%.txt", 1, "t").unwrap();
        db.add_device(&origin, "Handy").unwrap();

        let (page, total, _) = db.browse("self", None, None, 0, 10, None).unwrap();
        assert_eq!((page.len(), total), (10, 26));
        assert_eq!(page[0].name, "bild_00.jpg");
        assert_eq!(page[0].origin_name.as_deref(), Some("Handy"));
        let (page, total, _) = db.browse("self", None, None, 20, 10, None).unwrap();
        assert_eq!((page.len(), total), (6, 26));
        assert_eq!(page.last().unwrap().path.as_deref(), Some("/home/notiz%.txt"));
        let (page, total, _) = db.browse("self", None, None, 100, 10, None).unwrap();
        assert_eq!((page.len(), total), (0, 26));

        // "%" wird wörtlich gesucht, "_" ebenso
        let (page, _, _) = db.browse("self", None, Some("%"), 0, 50, None).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].name, "notiz%.txt");
        assert_eq!(db.browse("self", None, Some("_1"), 0, 50, None).unwrap().1, 10);
        assert_eq!(db.browse("self", None, Some("BILD_2"), 0, 50, None).unwrap().1, 5, "ohne Groß-/Kleinschreibung");

        // Gerätefilter
        assert_eq!(db.browse("self", Some(None), None, 0, 50, None).unwrap().1, 1);
        assert_eq!(db.browse("self", Some(Some(&origin)), None, 0, 50, None).unwrap().1, 25);

        // Keyset: alle Einträge seitenweise per Cursor, lückenlos und ohne Dopplungen
        let mut seen = Vec::new();
        let mut after = None;
        loop {
            let (page, total, next) = db.browse("self", None, None, 0, 7, after.as_ref()).unwrap();
            assert_eq!(total, 26);
            seen.extend(page.into_iter().map(|i| i.name));
            match next {
                Some(c) => after = Some(c),
                None => break,
            }
        }
        let (all, _, none) = db.browse("self", None, None, 0, 100, None).unwrap();
        assert!(none.is_none());
        assert_eq!(seen, all.into_iter().map(|i| i.name).collect::<Vec<_>>());
    }

    #[test]
    fn shares_scan_batch_and_access() {
        let db = DatabaseManager::open_in_memory().unwrap();
        db.add_device("dev", "Handy").unwrap();
        let share = db.create_share("Fotos", "/home/u/Fotos", true, &["dev".into()]).unwrap();
        // Überschneidende Freigaben werden abgelehnt
        assert!(db.create_share("Fotos", "/x", true, &[]).is_err());
        assert!(db.create_share("Andere", "/home/u/Fotos/2024", true, &[]).is_err());
        assert!(db.create_share("Andere", "/home/u", true, &[]).is_err());
        assert!(db.create_share("Andere", "/home/u/Fotos2", true, &[]).is_ok(), "Präfix ohne / ist kein Unterordner");
        assert!(db.create_share("Unter_strich", "/home/u/F_tos", true, &[]).is_ok(), "_ ist kein Platzhalter");

        let sign = |e: &LedgerEntry| format!("sig{}", e.seq);
        let n = db
            .apply_scan(
                "me",
                &share,
                &[
                    ScanChange::Upsert {
                        rel_path: "2024/a.jpg".into(),
                        abs_path: "/home/u/Fotos/2024/a.jpg".into(),
                        name: "a.jpg".into(),
                        hash: H1.into(),
                        size: 3,
                        mtime: 10,
                    },
                    ScanChange::Upsert {
                        rel_path: "b.jpg".into(),
                        abs_path: "/home/u/Fotos/b.jpg".into(),
                        name: "b.jpg".into(),
                        hash: H2.into(),
                        size: 4,
                        mtime: 11,
                    },
                ],
                &sign,
            )
            .unwrap();
        assert_eq!(n, 2);
        let known = db.known_share_files(share.id).unwrap();
        assert_eq!(known["2024/a.jpg"].mtime, 10);
        // Ledger-Einträge tragen Freigabe + relativen Pfad
        let entries = db.entries_since(&HashMap::new(), 10).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!((entries[0].share.as_str(), entries[0].rel_path.as_str()), ("Fotos", "2024/a.jpg"));
        assert_eq!(entries[1].seq, entries[0].seq + 1);
        // Zugriff: Share-Datei ist nicht "veröffentlicht"
        let acc = db.blob_access(H1).unwrap();
        assert!(!acc.published);
        assert_eq!(acc.share_paths, vec!["/home/u/Fotos/2024/a.jpg".to_string()]);
        // Gerät hat files.read auf den Ordner bekommen, später gekoppelte auch (auto_grant)
        assert_eq!(db.grant_folders("dev", "files.read").unwrap(), vec!["/home/u/Fotos".to_string()]);
        db.add_device("neu", "Laptop").unwrap();
        assert!(db.grant_folders("neu", "files.read").unwrap().contains(&"/home/u/Fotos".to_string()));
        // Löschen per Scan -> Tombstone
        db.apply_scan("me", &share, &[ScanChange::Delete { file_id: known["b.jpg"].id }], &sign).unwrap();
        assert!(!db.is_shared(H2).unwrap());
        let last = db.entries_since(&HashMap::from([("me".to_string(), 2)]), 10).unwrap();
        assert!(last[0].deleted && last[0].rel_path == "b.jpg");
        // Freigabe entfernen: Dateien weg, Rechte weg
        let removed = db.remove_share(share.id).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(db.grant_folders("dev", "files.read").unwrap().iter().all(|f| f != "/home/u/Fotos"));
        assert!(db.blob_access(H1).unwrap().share_paths.is_empty());
    }

    #[test]
    fn legacy_database_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE files (id TEXT)", []).unwrap();
        assert!(DatabaseManager::init(conn).is_err());
    }
}
