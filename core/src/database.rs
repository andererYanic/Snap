use anyhow::Result;
use rusqlite::{Connection, params};
use std::sync::Mutex;
use serde::{Serialize, Deserialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRecord {
    pub blake3_hash: String,
    pub file_name: String,
    pub file_size: u64,
    pub mime_type: Option<String>,
    pub ticket: String,
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
}

pub struct DatabaseManager {
    conn: Mutex<Connection>,
}

impl DatabaseManager {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;

        // Performance-Optimierungen: pragma_update nutzen
        conn.pragma_update(None, "journal_mode", &"wal")?;
        conn.pragma_update(None, "synchronous", &"NORMAL")?;

        // Dateien-Ledger mit Soft-Delete-Spalte
        conn.execute(
            "CREATE TABLE IF NOT EXISTS files (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                hash TEXT NOT NULL,
                ticket TEXT NOT NULL,
                blake3_hash TEXT,
                file_name TEXT,
                file_size INTEGER,
                mime_type TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                deleted_at DATETIME DEFAULT NULL
            )",
            [],
        )?;

        // Performance-Indizes für blitzschnelle Lookups & den Garbage Collector
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_files_hash ON files(hash);",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_files_deleted_at ON files(deleted_at);",
            [],
        )?;

        // Settings-Tabelle für Konfigurationen wie Retention-Timer
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES ('retention_days', '0')",
            [],
        )?;

        // Pending Pins - Offline Warteschlange
        conn.execute(
            "CREATE TABLE IF NOT EXISTS pending_pins (
                id TEXT PRIMARY KEY,
                target_hash TEXT NOT NULL,
                action TEXT NOT NULL, -- z.B. 'PIN' oder 'SYNC'
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_hash ON pending_pins(target_hash);",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS peer_status (
                node_id TEXT PRIMARY KEY,
                status TEXT NOT NULL, -- 'online' oder 'offline'
                last_seen INTEGER NOT NULL
            )",
            [],
        )?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;
        if let Some(row) = rows.next()? {
            let val: String = row.get(0)?;
            Ok(Some(val))
        } else {
            Ok(None)
        }
    }

    /// Speichert eine Datei mitsamt echtem Ticket in der Datenbank
    pub fn add_file(
        &self,
        record: &FileRecord,
        _device_id: &str,
        abs_path: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let file_id = uuid::Uuid::new_v4().to_string();

        conn.execute(
            "INSERT OR REPLACE INTO files (id, path, hash, ticket, blake3_hash, file_name, file_size, mime_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                file_id,
                abs_path,
                record.blake3_hash,
                record.ticket,
                record.blake3_hash,
                record.file_name,
                record.file_size as i64,
                record.mime_type
            ],
        )?;
        Ok(())
    }

    /// Wird vom Daemon aufgerufen
    pub fn add_simple_file(
        &self,
        id: &str,
        path: &str,
        hash: &str,
        ticket: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO files (id, path, hash, ticket, blake3_hash, file_name, file_size, mime_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                path,
                hash,
                ticket,
                hash,
                path,
                0i64,
                Option::<String>::None
            ],
        )?;
        Ok(())
    }

    /// Listet alle aktiven (nicht gelöschten) Dateien im Katalog auf
    pub fn list_files(&self) -> Result<Vec<FileRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT blake3_hash, file_name, file_size, mime_type, ticket FROM files WHERE deleted_at IS NULL"
        )?;
        
        let file_iter = stmt.query_map([], |row| {
            Ok(FileRecord {
                blake3_hash: row.get(0)?,
                file_name: row.get(1)?,
                file_size: row.get(2)?,
                mime_type: row.get(3)?,
                ticket: row.get(4)?,
            })
        })?;

        let mut files = Vec::new();
        for file in file_iter {
            files.push(file?);
        }
        Ok(files)
    }

    pub fn get_hash_by_id(&self, id: &str) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT hash FROM files WHERE id = ?1")?;
        let hash: String = stmt.query_row(params![id], |row| row.get(0))?;
        Ok(hash)
    }

    pub fn mark_file_as_deleted(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE files SET deleted_at = CURRENT_TIMESTAMP WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn remove_file_permanently(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn remove_by_hash(&self, hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM files WHERE hash = ?1", params![hash])?;
        Ok(())
    }

    /// Findet Hashes von Dateien, deren Soft-Delete-Frist abgelaufen ist
    pub fn get_expired_files(&self, retention_days: u32) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let query = format!(
            "SELECT hash FROM files WHERE deleted_at IS NOT NULL AND deleted_at <= datetime('now', '-{} days')",
            retention_days
        );
        let mut stmt = conn.prepare(&query)?;
        let hashes = stmt.query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(hashes)
    }

    pub fn get_ticket_by_hash(&self, hash_prefix: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        // Wir suchen per LIKE nach allem, was mit dem eingegebenen Hash beginnt
        let query = "SELECT ticket FROM files WHERE blake3_hash LIKE ?1 OR ticket LIKE ?1 LIMIT 1";
        let mut stmt = conn.prepare(query)?;
    
        // Wir hängen ein '%' an den Suchbegriff an, damit es als Präfix-Suche funktioniert
        let search_pattern = format!("{}%", hash_prefix);
        let mut rows = stmt.query(params![search_pattern])?;
    
        if let Some(row) = rows.next()? {
            let ticket: String = row.get(0)?;
            Ok(Some(ticket))
        } else {
            Ok(None)
        }
    }

    /// Markiert eine Datei flexibel per ID, Hash-Präfix oder Dateipfad als gelöscht (Papierkorb)
    pub fn delete_file_flexible(&self, id: Option<&str>, hash: Option<&str>, path: Option<&str>) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut count = 0;

        if let Some(i) = id {
            let mut stmt = conn.prepare("UPDATE files SET deleted_at = CURRENT_TIMESTAMP WHERE id = ?1 AND deleted_at IS NULL")?;
            count += stmt.execute(params![i])?;
        }
        
        if let Some(h) = hash {
            let pattern = format!("{}%", h);
            let mut stmt = conn.prepare("UPDATE files SET deleted_at = CURRENT_TIMESTAMP WHERE (blake3_hash LIKE ?1 OR hash LIKE ?1) AND deleted_at IS NULL")?;
            count += stmt.execute(params![pattern])?;
        }

        if let Some(p) = path {
            let mut stmt = conn.prepare("UPDATE files SET deleted_at = CURRENT_TIMESTAMP WHERE path = ?1 AND deleted_at IS NULL")?;
            count += stmt.execute(params![p])?;
        }

        Ok(count)
    }

    /// Fügt einen Pending Pin hinzu
    pub fn add_pending_pin(&self, target_hash: &str, action: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT OR REPLACE INTO pending_pins (id, target_hash, action) VALUES (?1, ?2, ?3)",
            params![id, target_hash, action],
        )?;
        Ok(())
    }

    /// Holt alle offenen Pending Pins ab
    pub fn get_pending_pins(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, target_hash, action FROM pending_pins")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Löscht einen abgearbeiteten Pending Pin aus der Warteschlange
    pub fn remove_pending_pin(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM pending_pins WHERE id = ?1", params![id])?;
        Ok(())
    }

    // Status eines Peers aktualisieren
    pub fn update_peer_status(&self, node_id: &str, status: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO peer_status (node_id, status, last_seen) VALUES (?1, ?2, ?3)
             ON CONFLICT(node_id) DO UPDATE SET status = ?2, last_seen = ?3",
            rusqlite::params![node_id, status, now],
        )?;
        Ok(())
    }

    // Status abfragen
    pub fn get_peer_status(&self, node_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT status FROM peer_status WHERE node_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![node_id])?;
        
        if let Some(row) = rows.next()? {
            let status: String = row.get(0)?;
            Ok(Some(status))
        } else {
            Ok(None) // Unbekannt (weder explizit online noch offline)
        }
    }
}
