//! Zur Laufzeit änderbare Grenzen („Einstellungen“).
//!
//! Standardwerte kommen aus `config.toml`; Änderungen über die API (`settings set`) werden in
//! der Datenbank gespeichert und gelten sofort – ohne Neustart. Nur lokal änderbar
//! (gekoppelte Geräte haben dafür kein Recht).
//!
//! Rechte gekoppelter Geräte sind davon getrennt und ebenfalls jederzeit änderbar
//! (`pair grant` / `pair revoke`).

use crate::api_bail;
use crate::config::Config;
use crate::database::DatabaseManager;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Beschreibung einer Einstellung.
struct Spec {
    key: &'static str,
    unit: &'static str,
    min: i64,
    max: i64,
    help: &'static str,
    default: fn(&Config) -> i64,
}

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

const SPECS: &[Spec] = &[
    Spec {
        key: "ops_hourly_limit",
        unit: "Dateien/Stunde",
        min: 0,
        max: 1_000_000,
        help: "Massenschutz: so viele Dateien darf ein Gerät pro Stunde umbenennen, verschieben oder löschen, \
               bevor Bestätigung nötig ist (0 = jede Fernänderung bestätigen)",
        default: |c| c.ops_hourly_limit as i64,
    },
    Spec {
        key: "upload_daily_gb",
        unit: "GB/Tag",
        min: 0,
        max: 100_000,
        help: "So viel darf jedes gekoppelte Gerät pro Tag hierher hochladen (0 = Hochladen gesperrt)",
        default: |c| c.upload_daily_gb as i64,
    },
    Spec {
        key: "upload_reserve_gb",
        unit: "GB",
        min: 0,
        max: 100_000,
        help: "So viel Platz muss nach einem empfangenen Upload frei bleiben",
        default: |c| c.upload_reserve_gb as i64,
    },
    Spec {
        key: "link_default_days",
        unit: "Tage",
        min: 1,
        max: 3650,
        help: "So lange gelten neue Freigabelinks (je Link änderbar)",
        default: |c| c.link_default_days as i64,
    },
    Spec {
        key: "pin_max_gb",
        unit: "GB",
        min: 0,
        max: 1_000_000,
        help: "Kontingent für offline verfügbare (gepinnte) Dateien (0 = unbegrenzt)",
        default: |c| (c.pin_max_bytes / GB) as i64,
    },
    Spec {
        key: "stream_cache_mb",
        unit: "MB",
        min: 0,
        max: 10_000_000,
        help: "Größe des Streaming-Caches (angesehene Teile von Videos usw.)",
        default: |c| (c.stream_cache_bytes / MB) as i64,
    },
    Spec {
        key: "thumb_cache_mb",
        unit: "MB",
        min: 10,
        max: 1_000_000,
        help: "Größe des Vorschaubild-Caches",
        default: |c| (c.thumb_cache_bytes / MB) as i64,
    },
];

/// Eine Einstellung mit aktuellem Wert (für Anzeige und API).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingInfo {
    pub key: String,
    pub value: i64,
    pub default: i64,
    /// Vom Standard abweichend (zur Laufzeit gesetzt).
    pub overridden: bool,
    pub unit: String,
    pub min: i64,
    pub max: i64,
    pub help: String,
}

pub struct Settings {
    db: Arc<DatabaseManager>,
    defaults: HashMap<&'static str, i64>,
    current: RwLock<HashMap<&'static str, i64>>,
    /// Genaue Werte aus der Konfiguration (Bytes), solange nichts überschrieben wurde –
    /// die Einstellungen arbeiten in ganzen GB/MB.
    exact: HashMap<&'static str, u64>,
    /// Zur Laufzeit gesetzte Schlüssel.
    overridden: RwLock<std::collections::HashSet<&'static str>>,
}

impl Settings {
    pub fn load(db: Arc<DatabaseManager>, config: &Config) -> Result<Self> {
        let defaults: HashMap<&'static str, i64> = SPECS.iter().map(|s| (s.key, (s.default)(config))).collect();
        let mut current = defaults.clone();
        let mut overridden = std::collections::HashSet::new();
        for (k, v) in db.settings()? {
            // Unbekannte oder ungültige gespeicherte Werte werden ignoriert (sicherer Standard).
            if let Some(spec) = SPECS.iter().find(|s| s.key == k) {
                if (spec.min..=spec.max).contains(&v) {
                    current.insert(spec.key, v);
                    overridden.insert(spec.key);
                }
            }
        }
        let exact = HashMap::from([
            ("pin_max_gb", config.pin_max_bytes),
            ("stream_cache_mb", config.stream_cache_bytes),
            ("thumb_cache_mb", config.thumb_cache_bytes),
        ]);
        Ok(Self { db, defaults, current: RwLock::new(current), exact, overridden: RwLock::new(overridden) })
    }

    /// Wert in Bytes: überschriebener Wert × Einheit, sonst der genaue Wert aus der Konfiguration.
    fn bytes(&self, key: &str, unit: u64) -> u64 {
        let set = self.overridden.read().unwrap_or_else(|e| e.into_inner()).contains(key);
        match self.exact.get(key) {
            Some(exact) if !set => *exact,
            _ => self.get(key) as u64 * unit,
        }
    }

    fn get(&self, key: &str) -> i64 {
        let map = self.current.read().unwrap_or_else(|e| e.into_inner());
        map.get(key).copied().unwrap_or(0)
    }

    pub fn ops_hourly_limit(&self) -> u32 {
        self.get("ops_hourly_limit").clamp(0, u32::MAX as i64) as u32
    }
    pub fn upload_daily_bytes(&self) -> u64 {
        self.get("upload_daily_gb") as u64 * GB
    }
    pub fn upload_reserve_bytes(&self) -> u64 {
        self.get("upload_reserve_gb") as u64 * GB
    }
    pub fn link_default_days(&self) -> u32 {
        self.get("link_default_days").clamp(1, 3650) as u32
    }
    /// 0 = unbegrenzt.
    pub fn pin_max_bytes(&self) -> u64 {
        self.bytes("pin_max_gb", GB)
    }
    pub fn stream_cache_bytes(&self) -> u64 {
        self.bytes("stream_cache_mb", MB)
    }
    pub fn thumb_cache_bytes(&self) -> u64 {
        self.bytes("thumb_cache_mb", MB)
    }

    pub fn list(&self) -> Vec<SettingInfo> {
        SPECS
            .iter()
            .map(|s| {
                let value = self.get(s.key);
                let default = self.defaults[s.key];
                SettingInfo {
                    key: s.key.into(),
                    value,
                    default,
                    overridden: self.overridden.read().unwrap_or_else(|e| e.into_inner()).contains(s.key),
                    unit: s.unit.into(),
                    min: s.min,
                    max: s.max,
                    help: s.help.split_whitespace().collect::<Vec<_>>().join(" "),
                }
            })
            .collect()
    }

    /// Setzt einen Wert (`None` = zurück zum Standard aus `config.toml`). Gilt sofort.
    pub fn set(&self, key: &str, value: Option<i64>) -> Result<SettingInfo> {
        let Some(spec) = SPECS.iter().find(|s| s.key == key) else {
            let all: Vec<&str> = SPECS.iter().map(|s| s.key).collect();
            api_bail!(InvalidInput, "Unbekannte Einstellung '{key}'. Möglich: {}", all.join(", "));
        };
        if let Some(v) = value {
            if !(spec.min..=spec.max).contains(&v) {
                api_bail!(InvalidInput, "{key} muss zwischen {} und {} liegen ({}).", spec.min, spec.max, spec.unit);
            }
        }
        self.db.put_setting(spec.key, value)?;
        let new = value.unwrap_or(self.defaults[spec.key]);
        self.current.write().unwrap_or_else(|e| e.into_inner()).insert(spec.key, new);
        let mut o = self.overridden.write().unwrap_or_else(|e| e.into_inner());
        if value.is_some() {
            o.insert(spec.key);
        } else {
            o.remove(spec.key);
        }
        drop(o);
        Ok(self.list().into_iter().find(|s| s.key == key).expect("bekannte Einstellung"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_override_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::portable(dir.path());
        config.upload_daily_gb = 20;
        let db = Arc::new(DatabaseManager::open(&dir.path().join("db")).unwrap());
        let s = Settings::load(db.clone(), &config).unwrap();
        assert_eq!(s.upload_daily_bytes(), 20 * GB);
        let info = s.set("upload_daily_gb", Some(50)).unwrap();
        assert!(info.overridden);
        assert_eq!(s.upload_daily_bytes(), 50 * GB, "sofort wirksam");
        // Überlebt einen Neustart
        let s2 = Settings::load(db.clone(), &config).unwrap();
        assert_eq!(s2.upload_daily_bytes(), 50 * GB);
        // Zurück zum Standard
        s2.set("upload_daily_gb", None).unwrap();
        assert_eq!(s2.upload_daily_bytes(), 20 * GB);
        // Ungültig
        assert!(s.set("upload_daily_gb", Some(-1)).is_err());
        assert!(s.set("thumb_cache_mb", Some(1)).is_err());
        assert!(s.set("root", Some(1)).is_err());
        // Genaue Werte aus der Konfiguration gehen nicht durch Rundung verloren (10 Byte ≠ unbegrenzt)
        let mut tiny = config.clone();
        tiny.pin_max_bytes = 10;
        let t = Settings::load(db.clone(), &tiny).unwrap();
        assert_eq!(t.pin_max_bytes(), 10);
        t.set("pin_max_gb", Some(3)).unwrap();
        assert_eq!(t.pin_max_bytes(), 3 * GB);
        t.set("pin_max_gb", Some(0)).unwrap();
        assert_eq!(t.pin_max_bytes(), 0, "ausdrücklich 0 = unbegrenzt");
        t.set("pin_max_gb", None).unwrap();
        assert_eq!(t.pin_max_bytes(), 10);
        // Manipulierter gespeicherter Wert wird ignoriert
        db.put_setting("ops_hourly_limit", Some(-5)).unwrap();
        assert_eq!(Settings::load(db, &config).unwrap().ops_hourly_limit(), config.ops_hourly_limit);
    }
}
