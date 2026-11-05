use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use anyhow::Result;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    /// Pfad zur SQLite-Datenbank
    pub db_path: String,
    /// Pfad zum Iroh-Blob-Store
    pub blob_store_path: String,
    /// Standard-Retention-Tage für gelöschte Dateien
    pub default_retention_days: i64,
    /// Pfad zum lokalen IPC-Socket für die Kommunikation zwischen CLI und Daemon
    pub socket_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            db_path: "space.db".to_string(),
            blob_store_path: ".space_blobs".to_string(),
            default_retention_days: 0,
            socket_path: "space.sock".to_string(),
        }
    }
}

impl Config {
    /// Lädt die Konfiguration aus einer Datei oder erstellt eine Standard-config.toml, falls sie nicht existiert.
    pub fn load_or_create<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            let config = Config::default();
            let contents = toml::to_string_pretty(&config)?;
            fs::write(path, contents)?;
            println!("⚙️ Neue Standard-Konfiguration erstellt unter: {}", path.display());
            return Ok(config);
        }

        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}
