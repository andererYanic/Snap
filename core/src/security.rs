//! Sicherheitsübersicht: Wer darf was, was ist sichtbar, was ist offen – auf einen Blick, mit
//! verständlichen Hinweisen. Nur lesend und nur lokal.

use crate::daemon::DaemonInner;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Rechte, die über das Übliche hinausgehen (für Hinweise).
const STRONG: &[&str] = &["files.write", "files.delete", "files.purge", "clipboard.write", "device.ring", "admin"];

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DeviceRights {
    pub node_id: String,
    pub name: String,
    /// Rechte als lesbare Kürzel, z. B. `files.write: /home/ich/Bilder`.
    pub rights: Vec<String>,
    /// Hat Rechte über „Dateiliste sehen/lesen“ hinaus.
    pub strong: bool,
    pub online: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ShareView {
    pub name: String,
    pub root: String,
    /// Für alle (auch künftig) gekoppelten Geräte.
    pub all_devices: bool,
}

/// Ein Hinweis: `warn` (sollte man wissen/prüfen) oder `info`. `code` und die Werte erlauben
/// den Oberflächen eine eigene Übersetzung; `text` ist die deutsche Fassung.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub level: String,
    pub code: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SecurityReport {
    pub devices: Vec<DeviceRights>,
    pub shares: Vec<ShareView>,
    pub all_files: bool,
    pub links_active: u64,
    /// Snap Drop: `Off`, `Contacts`, `Everyone`.
    pub drop_mode: String,
    pub vault_exists: bool,
    pub vault_unlocked: bool,
    /// Abgewiesene Zugriffe fremder bzw. gekoppelter Geräte in den letzten 7 Tagen.
    pub denied_7d: u64,
    pub relay_public: bool,
    pub hints: Vec<Hint>,
}

fn hint(level: &str, code: &str, text: String, name: Option<&str>, count: Option<u64>) -> Hint {
    Hint { level: level.into(), code: code.into(), text, name: name.map(str::to_string), count }
}

pub(crate) fn report(inner: &DaemonInner) -> Result<SecurityReport> {
    let db = &inner.db;
    let mut hints = Vec::new();

    let mut devices = Vec::new();
    for d in db.list_devices()? {
        let grants = db.grants_of(&d.node_id)?;
        let rights: Vec<String> = grants
            .iter()
            .map(|g| match &g.folder {
                Some(f) if !f.is_empty() => format!("{}: {f}", g.capability),
                _ => g.capability.clone(),
            })
            .collect();
        let strong = grants.iter().any(|g| STRONG.contains(&g.capability.as_str()));
        if grants.iter().any(|g| g.capability == "files.purge") {
            hints.push(hint(
                "warn",
                "purge",
                format!("„{}“ darf Dateien endgültig löschen (ohne Papierkorb).", d.name),
                Some(&d.name),
                None,
            ));
        }
        devices.push(DeviceRights {
            online: inner.live.is_connected(&d.node_id),
            node_id: d.node_id,
            name: d.name,
            rights,
            strong,
        });
    }
    devices.sort_by_key(|d| (!d.strong, d.name.to_lowercase()));

    let shares: Vec<ShareView> = db
        .list_shares()?
        .into_iter()
        .map(|s| ShareView { all_devices: s.auto_grant, name: s.name, root: s.root })
        .collect();
    let all_files = shares.iter().any(|s| s.name == crate::shares::ALL_FILES_SHARE);
    if all_files {
        hints.push(hint(
            "warn",
            "all_files",
            "„Alle Dateien“ ist eingeschaltet: Deine gekoppelten Geräte sehen den ganzen Speicher.".into(),
            None,
            None,
        ));
    }

    let links_active = crate::links::list(inner)
        .map(|(items, _)| items.iter().filter(|l| l.state == "active").count() as u64)
        .unwrap_or(0);
    if links_active > 0 {
        hints.push(hint(
            "info",
            "links",
            format!("{links_active} Freigabelink(s) sind aktiv – wer den Link hat, kann die Datei laden."),
            None,
            Some(links_active),
        ));
    }

    let drop_mode = inner.drop.status().map(|s| format!("{:?}", s.mode)).unwrap_or_else(|_| "Off".into());
    if drop_mode == "Everyone" {
        hints.push(hint(
            "info",
            "drop_everyone",
            "Snap Drop ist gerade für alle in der Nähe sichtbar (läuft von selbst ab).".into(),
            None,
            None,
        ));
    }

    let vault = inner.vault.status();
    if vault.unlocked {
        hints.push(hint(
            "info",
            "vault_unlocked",
            "Der Tresor ist entsperrt (sperrt sich nach 5 Minuten ohne Benutzung).".into(),
            None,
            None,
        ));
    }

    let since = crate::database::now_unix() - 7 * 86_400;
    let denied_7d = db.audit_log(1000)?.iter().filter(|e| !e.allowed && e.ts >= since).count() as u64;
    if denied_7d > 0 {
        hints.push(hint(
            "warn",
            "denied",
            format!("{denied_7d} abgewiesene Zugriffe in den letzten 7 Tagen – Einzelheiten im Protokoll."),
            None,
            Some(denied_7d),
        ));
    }

    let relay_public = inner.config.use_relay && inner.config.relay_urls.is_empty();
    if devices.is_empty() {
        hints.push(hint("info", "no_devices", "Noch kein Gerät gekoppelt.".into(), None, None));
    }
    Ok(SecurityReport {
        devices,
        shares,
        all_files,
        links_active,
        drop_mode,
        vault_exists: vault.exists,
        vault_unlocked: vault.unlocked,
        denied_7d,
        relay_public,
        hints,
    })
}
