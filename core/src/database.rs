//! SQLite-Persistenz: Dateikatalog, Offline-Warteschlange und Peer-Status.

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

const SCHEMA_VERSION: i64 = 1;

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
    ) -> Result<bool> {
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
        Ok(already)
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

    pub fn soft_delete_by_hash(&self, full_hash: &str) -> Result<usize> {
        let conn = self.conn();
        Ok(conn.execute(
            "UPDATE files SET deleted_at = ?2 WHERE blake3_hash = ?1 AND deleted_at IS NULL",
            params![full_hash, now_unix()],
        )?)
    }

    pub fn soft_delete_by_path(&self, path: &str) -> Result<usize> {
        let conn = self.conn();
        Ok(conn.execute(
            "UPDATE files SET deleted_at = ?2 WHERE path = ?1 AND deleted_at IS NULL",
            params![path, now_unix()],
        )?)
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
        assert!(!db.upsert_file("/a", H1, "a", 1, "t").unwrap());
        assert!(db.upsert_file("/a", H1, "a", 1, "t").unwrap());
        assert_eq!(db.list_files().unwrap().len(), 1);
        // Inhalt geändert -> gleicher Eintrag, neuer Hash
        assert!(!db.upsert_file("/a", H2, "a", 2, "t2").unwrap());
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
        assert_eq!(db.soft_delete_by_path("/a").unwrap(), 1);
        assert!(db.is_shared(H1).unwrap(), "zweite Kopie ist noch freigegeben");
        assert_eq!(db.purge_expired(0).unwrap(), Vec::<String>::new());
        assert_eq!(db.soft_delete_by_hash(H1).unwrap(), 1);
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
        assert!(!db.upsert_file("/a", H1, "a", 1, "t").unwrap());
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
