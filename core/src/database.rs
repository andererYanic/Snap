//! SQLite-Persistenz: Dateikatalog, Offline-Warteschlange und Peer-Status.

use crate::ledger::LedgerEntry;
use anyhow::{bail, Result};
use std::collections::HashMap;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const SCHEMA_VERSION: i64 = 2;

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
}

pub struct DatabaseManager {
    conn: Mutex<Connection>,
}

/// Prüft und normalisiert einen Hash bzw. Hash-Präfix: nur Hexzeichen,
/// 6 bis 64 Zeichen. Verhindert u. a. SQL-Wildcards (`%`, `_`) in Suchen.
pub fn normalize_hash_prefix(input: &str) -> Result<String> {
    let s = input.trim().to_ascii_lowercase();
    if s.len() < 6 {
        bail!("Hash zu kurz – bitte mindestens 6 Zeichen angeben.");
    }
    if s.len() > 64 {
        bail!("Hash zu lang – ein BLAKE3-Hash hat 64 Hexzeichen.");
    }
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Ungültiger Hash – nur Hexzeichen (0-9, a-f) erlaubt.");
    }
    Ok(s)
}

impl DatabaseManager {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        Self::init(conn)
    }

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

            -- Direkte Adress-Hinweise, nur von authentifizierten Gegenstellen über sich selbst.
            CREATE TABLE IF NOT EXISTS peer_addrs (
                node_id    TEXT PRIMARY KEY,
                addrs      TEXT    NOT NULL,
                updated_at INTEGER NOT NULL
            );
            ",
        )?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // Ein Panic in einem anderen Thread soll die DB nicht dauerhaft blockieren.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
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
        let conn = self.conn();
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
        Ok((already, id))
    }

    /// Daten einer Datei per ID (auch gelöschte).
    pub fn file_by_id(&self, id: i64) -> Result<Option<FileRecord>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT blake3_hash, file_name, file_size, path, ticket, created_at FROM files WHERE id = ?1",
                params![id],
                Self::map_file,
            )
            .optional()?)
    }

    /// Darf dieser Hash an andere Peers ausgeliefert werden?
    pub fn is_shared(&self, hash: &str) -> Result<bool> {
        let conn = self.conn();
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
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT blake3_hash, file_name, file_size, path, ticket, created_at
             FROM files WHERE deleted_at IS NULL ORDER BY file_name COLLATE NOCASE",
        )?;
        let rows = stmt
            .query_map([], Self::map_file)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
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
        })
    }

    /// Löst einen (validierten) Hash-Präfix eindeutig auf aktive Dateien auf.
    /// Gibt alle passenden Dateien zurück; der Aufrufer prüft auf Eindeutigkeit.
    pub fn find_by_hash_prefix(&self, prefix: &str) -> Result<Vec<FileRecord>> {
        let prefix = normalize_hash_prefix(prefix)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT blake3_hash, file_name, file_size, path, ticket, created_at
             FROM files WHERE deleted_at IS NULL AND substr(blake3_hash, 1, ?2) = ?1
             ORDER BY created_at",
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
            let mut stmt =
                tx.prepare(&format!("SELECT id FROM files WHERE {cond} AND deleted_at IS NULL"))?;
            let rows = stmt.query_map(params![value], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
            rows
        };
        tx.execute(
            &format!("UPDATE files SET deleted_at = ?2 WHERE {cond} AND deleted_at IS NULL"),
            params![value, now_unix()],
        )?;
        tx.commit()?;
        Ok(ids)
    }

    /// Entfernt Papierkorb-Einträge, deren Frist abgelaufen ist. Gibt die Hashes
    /// zurück, auf die danach kein Eintrag mehr verweist (deren Blobs gelöscht werden dürfen).
    pub fn purge_expired(&self, retention_days: u32) -> Result<Vec<String>> {
        let cutoff = now_unix() - i64::from(retention_days) * 86_400;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let candidates: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT blake3_hash FROM files WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
            )?;
            let rows = stmt
                .query_map(params![cutoff], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        tx.execute(
            "DELETE FROM files WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
            params![cutoff],
        )?;
        let mut orphaned = Vec::new();
        for hash in candidates {
            let still_used: bool = tx
                .query_row("SELECT 1 FROM files WHERE blake3_hash = ?1 LIMIT 1", params![hash], |_| {
                    Ok(true)
                })
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
        let conn = self.conn();
        Ok(conn.query_row("SELECT COUNT(*) FROM files WHERE deleted_at IS NULL", [], |r| {
            r.get::<_, i64>(0)
        })? as u64)
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
                "SELECT id FROM pending_pins WHERE target_hash = ?1 AND action = 'DOWNLOAD' AND output_path = ?2",
                params![job.target_hash, job.output_path],
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
            return Ok((id, true));
        }
        conn.execute(
            "INSERT INTO pending_pins
               (target_hash, action, ticket, peer_id, output_path, explicit_output, force,
                attempts, state, last_error, created_at)
             VALUES (?1, 'DOWNLOAD', ?2, ?3, ?4, ?5, ?6, 1, 'waiting', ?7, ?8)",
            params![
                job.target_hash,
                job.ticket,
                job.peer_id,
                job.output_path,
                job.explicit_output,
                job.force,
                job.last_error,
                now_unix()
            ],
        )?;
        Ok((conn.last_insert_rowid(), false))
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
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{} ORDER BY j.id", Self::JOB_SELECT))?;
        let rows = stmt
            .query_map([], Self::map_job)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Wartende Aufträge, deren Peer nicht (mehr) im Backoff ist.
    pub fn due_jobs(&self, now: i64) -> Result<Vec<PendingJob>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{} WHERE j.state = 'waiting' AND (p.next_retry_at IS NULL OR p.next_retry_at <= ?1)
             ORDER BY j.id",
            Self::JOB_SELECT
        ))?;
        let rows = stmt
            .query_map(params![now], Self::map_job)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Zeitpunkt, zu dem der nächste wartende Auftrag fällig wird.
    /// `None` = keine wartenden Aufträge (Worker darf unbegrenzt schlafen).
    pub fn next_wakeup(&self) -> Result<Option<i64>> {
        let conn = self.conn();
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
        let conn = self.conn();
        Ok(conn.query_row("SELECT COUNT(*) FROM pending_pins WHERE state = 'waiting'", [], |r| {
            r.get::<_, i64>(0)
        })? as u64)
    }

    pub fn complete_job(&self, id: i64) -> Result<()> {
        self.conn().execute("DELETE FROM pending_pins WHERE id = ?1", params![id])?;
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
        Ok(())
    }

    pub fn cancel_job(&self, id: i64) -> Result<bool> {
        Ok(self.conn().execute("DELETE FROM pending_pins WHERE id = ?1", params![id])? > 0)
    }

    /// Setzt einen Auftrag auf "wartend" und das Backoff seines Peers zurück.
    /// Gibt `false` zurück, wenn es den Auftrag nicht gibt.
    pub fn retry_job(&self, id: i64) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let peer: Option<String> = tx
            .query_row("SELECT peer_id FROM pending_pins WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?;
        let Some(peer) = peer else {
            return Ok(false);
        };
        tx.execute("UPDATE pending_pins SET state = 'waiting' WHERE id = ?1", params![id])?;
        tx.execute(
            "UPDATE peer_status SET next_retry_at = NULL WHERE node_id = ?1",
            params![peer],
        )?;
        tx.commit()?;
        Ok(true)
    }

    // ------------------------------------------------------------------
    // Peer-Status (rein reaktiv)
    // ------------------------------------------------------------------

    /// Erfolgreiche Verbindung: online, Backoff zurücksetzen.
    pub fn mark_peer_online(&self, node_id: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO peer_status (node_id, status, last_seen, failures, next_retry_at)
             VALUES (?1, 'online', ?2, 0, NULL)
             ON CONFLICT(node_id) DO UPDATE SET
                status = 'online', last_seen = ?2, failures = 0, next_retry_at = NULL",
            params![node_id, now_unix()],
        )?;
        Ok(())
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
        let existing: Option<(u32, Option<i64>)> = tx
            .query_row(
                "SELECT failures, next_retry_at FROM peer_status WHERE node_id = ?1",
                params![node_id],
                |r| Ok((r.get::<_, i64>(0)? as u32, r.get(1)?)),
            )
            .optional()?;
        if !escalate {
            if let Some((_, Some(next))) = existing {
                if next > now {
                    tx.execute(
                        "UPDATE peer_status SET status = 'offline' WHERE node_id = ?1",
                        params![node_id],
                    )?;
                    tx.commit()?;
                    return Ok(next);
                }
            }
        }
        let failures = existing.map(|(f, _)| f).unwrap_or(0).saturating_add(1);
        let next = now + crate::backoff::delay_random(failures).as_secs() as i64;
        tx.execute(
            "INSERT INTO peer_status (node_id, status, last_seen, failures, next_retry_at)
             VALUES (?1, 'offline', NULL, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET
                status = 'offline', failures = ?2, next_retry_at = ?3",
            params![node_id, failures, next],
        )?;
        tx.commit()?;
        Ok(next)
    }

    /// Ein bereits bekannter Peer hat sich bei uns gemeldet (eingehende Verbindung).
    /// Unbekannte Node-IDs werden bewusst nicht gespeichert, damit Fremde die
    /// Tabelle nicht füllen können. Gibt `true` zurück, wenn für diesen Peer
    /// wartende Aufträge existieren.
    pub fn mark_known_peer_seen(&self, node_id: &str) -> Result<bool> {
        let conn = self.conn();
        let updated = conn.execute(
            "UPDATE peer_status SET status = 'online', last_seen = ?2, failures = 0, next_retry_at = NULL
             WHERE node_id = ?1",
            params![node_id, now_unix()],
        )?;
        if updated == 0 {
            return Ok(false);
        }
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
        let conn = self.conn();
        let mut stmt = conn.prepare(
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
        Ok(self.list_peers()?.into_iter().find(|p| p.node_id == node_id))
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
        };
        entry.signature = sign(&entry);
        Self::upsert_entry(&tx, &entry)?;
        tx.commit()?;
        Ok(entry)
    }

    fn upsert_entry(conn: &Connection, e: &LedgerEntry) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO ledger_entries (origin, entry_id, seq, hash, name, size, created_at, deleted, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(origin, entry_id) DO UPDATE SET
                seq = excluded.seq, hash = excluded.hash, name = excluded.name, size = excluded.size,
                created_at = excluded.created_at, deleted = excluded.deleted, signature = excluded.signature
             WHERE excluded.seq > ledger_entries.seq",
            params![e.origin, e.entry_id, e.seq, e.hash, e.name, e.size as i64, e.created_at, e.deleted, e.signature],
        )
    }

    /// Übernimmt einen (bereits verifizierten) fremden Eintrag, falls er neuer ist.
    pub fn apply_remote_entry(&self, e: &LedgerEntry) -> Result<bool> {
        Ok(Self::upsert_entry(&self.conn(), e)? > 0)
    }

    /// Aktive eigene Dateien ohne Ledger-Eintrag (z. B. aus Schema 1).
    pub fn files_missing_entries(&self, origin: &str) -> Result<Vec<(i64, FileRecord)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.blake3_hash, f.file_name, f.file_size, f.path, f.ticket, f.created_at
             FROM files f
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
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Höchste bekannte Sequenznummer je Ursprungsgerät.
    pub fn version_vector(&self) -> Result<HashMap<String, i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT origin, MAX(seq) FROM ledger_entries GROUP BY origin")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?;
        Ok(rows)
    }

    /// Alle Einträge, die neuer sind als der übergebene Versionsvektor –
    /// je Ursprung aufsteigend nach `seq`, maximal `limit` Stück.
    pub fn entries_since(&self, have: &HashMap<String, i64>, limit: usize) -> Result<Vec<LedgerEntry>> {
        let conn = self.conn();
        let origins: Vec<String> = {
            let mut stmt = conn.prepare("SELECT DISTINCT origin FROM ledger_entries ORDER BY origin")?;
            let rows = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            rows
        };
        let mut stmt = conn.prepare(
            "SELECT origin, entry_id, seq, hash, name, size, created_at, deleted, signature
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
        })
    }

    const REMOTE_SELECT: &'static str = "
        SELECT l.origin, d.name, l.hash, l.name, l.size, l.created_at
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
        })
    }

    /// Dateien anderer Geräte (ohne gelöschte).
    pub fn remote_files(&self, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{} ORDER BY COALESCE(d.name, l.origin), l.name COLLATE NOCASE",
            Self::REMOTE_SELECT
        ))?;
        let rows = stmt.query_map(params![self_origin], Self::map_remote)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_remote_by_hash_prefix(&self, prefix: &str, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let prefix = normalize_hash_prefix(prefix)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "{} AND substr(l.hash, 1, ?3) = ?2 ORDER BY l.created_at",
            Self::REMOTE_SELECT
        ))?;
        let rows = stmt
            .query_map(params![self_origin, prefix, prefix.len() as i64], Self::map_remote)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn find_remote_by_name(&self, name: &str, self_origin: &str) -> Result<Vec<RemoteFile>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!("{} AND l.name = ?2 ORDER BY l.created_at", Self::REMOTE_SELECT))?;
        let rows = stmt.query_map(params![self_origin, name], Self::map_remote)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ------------------------------------------------------------------
    // Gekoppelte Geräte & Adress-Hinweise
    // ------------------------------------------------------------------

    /// Fügt ein Gerät hinzu oder aktualisiert seinen Namen.
    pub fn add_device(&self, node_id: &str, name: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO devices (node_id, name, paired_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET name = excluded.name",
            params![node_id, name, now_unix()],
        )?;
        Ok(())
    }

    pub fn is_paired(&self, node_id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row("SELECT 1 FROM devices WHERE node_id = ?1", params![node_id], |_| Ok(true))
            .optional()?
            .unwrap_or(false))
    }

    pub fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT node_id, name, paired_at, last_sync_at FROM devices ORDER BY name COLLATE NOCASE")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(DeviceInfo { node_id: r.get(0)?, name: r.get(1)?, paired_at: r.get(2)?, last_sync_at: r.get(3)? })
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

    /// Entkoppelt ein Gerät und entfernt seine Ledger-Einträge und Adress-Hinweise.
    pub fn remove_device(&self, node_id: &str) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let removed = tx.execute("DELETE FROM devices WHERE node_id = ?1", params![node_id])? > 0;
        tx.execute("DELETE FROM ledger_entries WHERE origin = ?1", params![node_id])?;
        tx.execute("DELETE FROM peer_addrs WHERE node_id = ?1", params![node_id])?;
        tx.commit()?;
        Ok(removed)
    }

    pub fn set_last_sync(&self, node_id: &str) -> Result<()> {
        self.conn()
            .execute("UPDATE devices SET last_sync_at = ?2 WHERE node_id = ?1", params![node_id, now_unix()])?;
        Ok(())
    }

    /// Speichert direkte Adress-Hinweise (max. 16, nur gültige Socket-Adressen).
    pub fn set_addr_hints(&self, node_id: &str, addrs: &[String]) -> Result<()> {
        let valid: Vec<&String> =
            addrs.iter().filter(|a| a.parse::<std::net::SocketAddr>().is_ok()).take(16).collect();
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
            .conn()
            .query_row("SELECT addrs FROM peer_addrs WHERE node_id = ?1", params![node_id], |r| r.get(0))
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default())
    }

    pub fn has_waiting_jobs(&self, peer_id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM pending_pins WHERE peer_id = ?1 AND state = 'waiting' LIMIT 1",
                params![peer_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "aaaaaa1111111111111111111111111111111111111111111111111111111111";
    const H2: &str = "aaaaaa2222222222222222222222222222222222222222222222222222222222";

    fn job<'a>(hash: &'a str, out: &'a str, peer: &'a str) -> NewJob<'a> {
        NewJob {
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

    #[test]
    fn legacy_database_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE files (id TEXT)", []).unwrap();
        assert!(DatabaseManager::init(conn).is_err());
    }
}
