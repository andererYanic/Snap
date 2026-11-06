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
    /// Anzeigename dieses Geräts für gekoppelte Geräte. Standard: Hostname.
    pub device_name: Option<String>,
    /// Intervall für den automatischen Ledger-Abgleich in Stunden (±30 % Zufall). 0 = aus.
    pub sync_interval_hours: Option<f64>,
    /// Stehende Verbindungen zu gekoppelten Geräten („immer live“). Standard: true.
    pub live_connections: Option<bool>,
    /// Zielordner für gepinnte (offline verfügbare) Dateien. Standard: ~/Snap
    pub pin_dir: Option<PathBuf>,
    /// Kontingent für gepinnte Dateien in GB (0 = unbegrenzt).
    pub pin_max_gb: Option<f64>,
    /// Lokale Suche: Home-Ordner und angeschlossene Laufwerke indexieren. Standard: true.
    pub index_enabled: Option<bool>,
    /// Angeschlossene Laufwerke (/media, /run/media) automatisch indexieren. Standard: true.
    pub index_drives: Option<bool>,
    /// Höchstzahl beobachteter Ordner (inotify). Darüber hinaus gleicht der periodische
    /// Abgleich ab. Standard: 50 000.
    pub index_max_watches: Option<usize>,
    /// Gekoppelte Geräte: verändernde Operationen (Dateien) pro Stunde ohne Bestätigung.
    /// Standard: 100. 0 = jede Fernänderung muss bestätigt werden.
    pub ops_hourly_limit: Option<u32>,
    /// Höchstgröße des Vorschaubild-Caches in MB. Standard: 500.
    pub thumb_cache_mb: Option<u64>,
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
    pub device_name: String,
    /// 0.0 = periodischer Abgleich deaktiviert.
    pub sync_interval_hours: f64,
    pub live_connections: bool,
    pub pin_dir: PathBuf,
    /// 0 = unbegrenzt.
    pub pin_max_bytes: u64,
    pub index_enabled: bool,
    /// Home-Ordner beim ersten Start als Ort anlegen (in Tests aus).
    pub index_home: bool,
    pub index_drives: bool,
    pub index_max_watches: usize,
    /// Home-Papierkorb (freedesktop): `$XDG_DATA_HOME/Trash`.
    pub trash_dir: PathBuf,
    /// Verändernde Fernoperationen pro Gerät und Stunde, darüber wird Bestätigung verlangt.
    pub ops_hourly_limit: u32,
    pub thumb_cache_bytes: u64,
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

    pub fn thumb_dir(&self) -> PathBuf {
        self.data_dir.join("thumbs")
    }

    pub fn index_path(&self) -> PathBuf {
        self.data_dir.join("index.db")
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
            let contents =
                fs::read_to_string(&path).with_context(|| format!("Konfiguration {} nicht lesbar", path.display()))?;
            toml::from_str::<ConfigFile>(&contents)
                .with_context(|| format!("Konfiguration {} ist ungültig", path.display()))?
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Konfigurationsordner {} nicht anlegbar", parent.display()))?;
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

        let socket_path = env_path("SPACE_SOCKET").or(file.socket_path.map(expand_tilde)).unwrap_or_else(|| match dirs
            .runtime_dir()
        {
            Some(rt) => rt.join("snap.sock"),
            None => data_dir.join("snap.sock"),
        });

        let download_dir =
            env_path("SPACE_DOWNLOAD_DIR").or(file.download_dir.map(expand_tilde)).unwrap_or_else(|| {
                directories::UserDirs::new()
                    .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
                    .unwrap_or_else(|| data_dir.join("downloads"))
                    .join(APP_NAME)
            });

        let pin_dir = env_path("SPACE_PIN_DIR").or(file.pin_dir.map(expand_tilde)).unwrap_or_else(|| {
            directories::BaseDirs::new().map(|b| b.home_dir().join("Snap")).unwrap_or_else(|| data_dir.join("pinned"))
        });

        let trash_dir =
            directories::BaseDirs::new().map(|b| b.data_dir().join("Trash")).unwrap_or_else(|| data_dir.join("Trash"));
        Ok(Self {
            data_dir,
            socket_path,
            download_dir,
            retention_days: file.retention_days.unwrap_or(30),
            connect_timeout_secs: file.connect_timeout_secs.unwrap_or(10).max(1),
            bind_port: file.bind_port,
            use_relay: file.use_relay.unwrap_or(true),
            device_name: file
                .device_name
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(hostname),
            sync_interval_hours: file.sync_interval_hours.unwrap_or(6.0).max(0.0),
            live_connections: file.live_connections.unwrap_or(true),
            pin_dir,
            pin_max_bytes: (file.pin_max_gb.unwrap_or(0.0).max(0.0) * 1e9) as u64,
            index_enabled: file.index_enabled.unwrap_or(true),
            index_home: true,
            index_drives: file.index_drives.unwrap_or(true),
            index_max_watches: file.index_max_watches.unwrap_or(50_000),
            trash_dir,
            ops_hourly_limit: file.ops_hourly_limit.unwrap_or(100),
            thumb_cache_bytes: file.thumb_cache_mb.unwrap_or(500).max(10) * 1024 * 1024,
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
            device_name: "Testgerät".to_string(),
            sync_interval_hours: 0.0,
            live_connections: true,
            pin_dir: root.join("Snap"),
            pin_max_bytes: 0,
            index_enabled: true,
            index_home: false,
            index_drives: false,
            index_max_watches: 10_000,
            trash_dir: root.join("Trash"),
            ops_hourly_limit: 100,
            thumb_cache_bytes: 50 * 1024 * 1024,
        }
    }
}

/// Hostname des Systems als Standard-Gerätename.
fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|p| fs::read_to_string(p).ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "snap-gerät".to_string())
}

fn project_dirs() -> Result<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", APP_NAME)
        .context("Home-Verzeichnis konnte nicht ermittelt werden (ist $HOME gesetzt?)")
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(|v| expand_tilde(PathBuf::from(v)))
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

# Name dieses Geräts, wie er auf gekoppelten Geräten angezeigt wird (Standard: Hostname)
# device_name = "Arch-Desktop"

# Automatischer Abgleich der Dateilisten mit gekoppelten Geräten, in Stunden (±30 % Zufall).
# Zusätzlich wird bei Nutzung (list, sync) und bei Kontakt mit einem Gerät abgeglichen.
# 0 = nur bei Nutzung/Kontakt.
# sync_interval_hours = 6

# Stehende Verbindungen zu gekoppelten Geräten: Änderungen und Online-Status sofort.
# Hinweis: Relais/Netzbeobachter sehen dadurch, wann deine Geräte online sind.
# live_connections = true

# Gepinnte (offline verfügbare) Dateien anderer Geräte: ~/Snap/<Gerät>/<Freigabe>/…
# pin_dir = "~/Snap"
# Kontingent in GB (0 = unbegrenzt)
# pin_max_gb = 0

# Lokale Suche über Home-Ordner und angeschlossene Laufwerke (Index bleibt auf diesem Gerät).
# index_enabled = true
# index_drives = true
# index_max_watches = 50000

# Gekoppelte Geräte: höchstens so viele Dateien pro Stunde verändern (umbenennen, verschieben,
# löschen), darüber wird eine Bestätigung auf diesem Gerät verlangt (space-daemon ops pending).
# ops_hourly_limit = 100

# Größe des Caches für Vorschaubilder (MB).
# thumb_cache_mb = 500
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
