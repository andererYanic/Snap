//! Zentrale Konfiguration und Pfadauflösung.
//!
//! Reihenfolge (höchste Priorität zuerst):
//! 1. Umgebungsvariablen (`SPACE_CONFIG`, `SPACE_DATA_DIR`, `SPACE_SOCKET`, `SPACE_DOWNLOAD_DIR`)
//! 2. Werte aus `config.toml`
//! 3. XDG-Standardpfade
//!
//! Standardpfade unter Linux:
//! - Konfiguration: `~/.config/snap/config.toml`
//! - Daten (DB, Blobs, Schlüssel): `~/.local/share/snap/`
//! - IPC-Socket: `$XDG_RUNTIME_DIR/snap/snap.sock` (Fallback: `<data_dir>/snap.sock`)
//! - Hintergrund-Downloads: `~/Downloads/snap/`

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const APP_NAME: &str = "snap";

/// Inhalt der `config.toml`. Alle Felder sind optional, damit auch eine
/// unvollständige Datei gültig ist.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    /// Verzeichnis für Datenbank, Blob-Store und Node-Schlüssel.
    pub data_dir: Option<PathBuf>,
    /// Pfad zum IPC-Socket.
    pub socket_path: Option<PathBuf>,
    /// Zielordner für Downloads ohne expliziten Zielpfad (z. B. aus der Warteschlange).
    pub download_dir: Option<PathBuf>,
    /// Wie viele Tage gelöschte Dateien im Papierkorb bleiben, bevor sie endgültig entfernt werden.
    pub retention_days: Option<u32>,
    /// Timeout für den Verbindungsaufbau zu einem Peer (Sekunden).
    pub connect_timeout_secs: Option<u64>,
    /// Fester UDP-Port für Iroh (z. B. für Docker / Portfreigaben). Ohne Angabe: zufällig.
    pub bind_port: Option<u16>,
    /// `false` = keine öffentlichen n0-Relais und kein n0-DNS (reiner LAN-/Direktmodus).
    /// Tickets enthalten dann direkte IP-Adressen.
    pub use_relay: Option<bool>,
}

/// Vollständig aufgelöste Laufzeitkonfiguration.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    pub download_dir: PathBuf,
    pub retention_days: u32,
    pub connect_timeout_secs: u64,
    pub bind_port: Option<u16>,
    pub use_relay: bool,
}

impl Config {
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("snap.db")
    }

    pub fn blob_store_path(&self) -> PathBuf {
        self.data_dir.join("blobs")
    }

    pub fn secret_key_path(&self) -> PathBuf {
        self.data_dir.join("secret.key")
    }

    /// Standardpfad der Konfigurationsdatei (oder `SPACE_CONFIG`).
    pub fn default_config_path() -> Result<PathBuf> {
        if let Some(p) = env_path("SPACE_CONFIG") {
            return Ok(p);
        }
        let dirs = project_dirs()?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Lädt die Konfiguration. Existiert die Datei nicht, wird eine kommentierte
    /// Vorlage angelegt. Umgebungsvariablen überschreiben Dateiwerte.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_config_path()?,
        };

        let file = if path.exists() {
            let contents = fs::read_to_string(&path)
                .with_context(|| format!("Konfiguration {} nicht lesbar", path.display()))?;
            toml::from_str::<ConfigFile>(&contents)
                .with_context(|| format!("Konfiguration {} ist ungültig", path.display()))?
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("Konfigurationsordner {} nicht anlegbar", parent.display())
                })?;
            }
            fs::write(&path, CONFIG_TEMPLATE)
                .with_context(|| format!("Konfiguration {} nicht schreibbar", path.display()))?;
            ConfigFile::default()
        };

        Self::resolve(file)
    }

    /// Kombiniert Dateiwerte, Umgebungsvariablen und Standardpfade.
    pub fn resolve(file: ConfigFile) -> Result<Self> {
        let dirs = project_dirs()?;

        let data_dir = env_path("SPACE_DATA_DIR")
            .or(file.data_dir.map(expand_tilde))
            .unwrap_or_else(|| dirs.data_dir().to_path_buf());

        let socket_path = env_path("SPACE_SOCKET")
            .or(file.socket_path.map(expand_tilde))
            .unwrap_or_else(|| match dirs.runtime_dir() {
                Some(rt) => rt.join("snap.sock"),
                None => data_dir.join("snap.sock"),
            });

        let download_dir = env_path("SPACE_DOWNLOAD_DIR")
            .or(file.download_dir.map(expand_tilde))
            .unwrap_or_else(|| {
                directories::UserDirs::new()
                    .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| data_dir.join("downloads"))
                    .join(APP_NAME)
            });

        Ok(Self {
            data_dir,
            socket_path,
            download_dir,
            retention_days: file.retention_days.unwrap_or(30),
            connect_timeout_secs: file.connect_timeout_secs.unwrap_or(10).max(1),
            bind_port: file.bind_port,
            use_relay: file.use_relay.unwrap_or(true),
        })
    }

    /// Konfiguration mit allen Daten unterhalb eines Wurzelordners (für Tests / portable Setups).
    pub fn portable(root: &Path) -> Self {
        Self {
            data_dir: root.join("data"),
            socket_path: root.join("run").join("snap.sock"),
            download_dir: root.join("downloads"),
            retention_days: 30,
            connect_timeout_secs: 5,
            bind_port: None,
            use_relay: true,
        }
    }
}

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", APP_NAME)
        .context("Home-Verzeichnis konnte nicht ermittelt werden (ist $HOME gesetzt?)")
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(|v| expand_tilde(PathBuf::from(v)))
}

/// Ersetzt ein führendes `~/` durch das Home-Verzeichnis.
fn expand_tilde(path: PathBuf) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
            return home.join(rest);
        }
    }
    path
}

const CONFIG_TEMPLATE: &str = r#"# Snap / space-daemon Konfiguration
# Alle Werte sind optional. Auskommentierte Werte zeigen den Standard.

# Datenbank, Blob-Store und Node-Schlüssel (secret.key)
# data_dir = "~/.local/share/snap"

# IPC-Socket zwischen CLI und Daemon (Standard: $XDG_RUNTIME_DIR/snap/snap.sock)
# socket_path = "/run/user/1000/snap/snap.sock"

# Zielordner für Downloads ohne expliziten Zielpfad
# download_dir = "~/Downloads/snap"

# Tage, die gelöschte Dateien im Papierkorb bleiben (0 = beim nächsten Aufräumen entfernen)
# retention_days = 30

# Timeout für den Verbindungsaufbau zu einem Peer in Sekunden
# connect_timeout_secs = 10

# Fester UDP-Port für Iroh (z. B. für Docker/Portfreigabe). Standard: zufällig
# bind_port = 11204

# false = keine öffentlichen n0-Relais / kein n0-DNS (nur direkte Verbindungen, z. B. im LAN).
# Tickets enthalten dann direkte IP-Adressen.
# use_relay = true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses() {
        let parsed: ConfigFile = toml::from_str(CONFIG_TEMPLATE).unwrap();
        assert!(parsed.data_dir.is_none());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<ConfigFile>("db_path = \"x\"").is_err());
    }

    #[test]
    fn tilde_is_expanded() {
        let p = expand_tilde(PathBuf::from("~/foo"));
        assert!(!p.starts_with("~"));
        assert!(p.ends_with("foo"));
    }
}
