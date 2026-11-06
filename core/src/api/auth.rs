//! Rechte: eine einzige Prüfstelle für alle Operationen.
//!
//! - `Caller::Local` (gleiche Benutzer-ID über den Socket oder eingebettet) darf alles.
//! - `Caller::Device` (gekoppeltes Gerät, Node-ID aus TLS) darf nur, was ausdrücklich
//!   gewährt wurde. Standard beim Koppeln: `catalog.read`.
//! - Ordnergebundene Rechte gelten nur innerhalb des Ordners; Pfade werden kanonisiert,
//!   Symlinks aufgelöst. Geschützte Pfade (Snap-Daten, Systemverzeichnisse) sind immer gesperrt.
//! - `admin` ist vorgesehen, aber für Geräte noch nicht nutzbar (später nur mit Bestätigung).
//! - Entscheidungen über fremde Geräte werden im Audit-Log festgehalten.

use crate::api::error::{ApiError, ErrorCode};
use crate::api_bail;
use crate::database::DatabaseManager;
use crate::ipc::Request;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Dateiliste lesen.
    CatalogRead,
    /// Dateien in einem Ordner lesen/streamen.
    FilesRead,
    /// Dateien in einen Ordner schreiben (z. B. Kamera-Upload).
    FilesWrite,
    /// Gerät bitten, eine Datei dauerhaft vorzuhalten.
    PinRequest,
    /// Dateien in einem Ordner löschen (immer über den Papierkorb).
    FilesDelete,
    /// Dateien in einem Ordner endgültig löschen (ohne Papierkorb) – nie Standard.
    FilesPurge,
    /// Fernverwaltung (Einstellungen, Kopplungen) – für Geräte noch gesperrt.
    Admin,
}

impl Capability {
    pub const ALL: [Capability; 7] = [
        Capability::CatalogRead,
        Capability::FilesRead,
        Capability::FilesWrite,
        Capability::PinRequest,
        Capability::FilesDelete,
        Capability::FilesPurge,
        Capability::Admin,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Capability::CatalogRead => "catalog.read",
            Capability::FilesRead => "files.read",
            Capability::FilesWrite => "files.write",
            Capability::PinRequest => "pin.request",
            Capability::FilesDelete => "files.delete",
            Capability::FilesPurge => "files.purge",
            Capability::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s.trim()).ok_or_else(|| {
            let all: Vec<&str> = Self::ALL.iter().map(|c| c.as_str()).collect();
            ApiError::new(ErrorCode::InvalidInput, format!("Unbekanntes Recht '{s}'. Möglich: {}", all.join(", ")))
                .into()
        })
    }

    /// Rechte, die nur innerhalb eines Ordners gelten.
    pub fn needs_folder(&self) -> bool {
        matches!(
            self,
            Capability::FilesRead | Capability::FilesWrite | Capability::FilesDelete | Capability::FilesPurge
        )
    }
}

/// Wer eine Operation auslöst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// Gleicher Benutzer auf diesem Gerät (Socket mit SO_PEERCRED oder eingebettet).
    Local,
    /// Gekoppeltes Gerät; Node-ID stammt aus der TLS-Verbindung.
    Device(String),
}

/// Welches Recht eine Anfrage verlangt, wenn sie von einem fremden Gerät kommt.
/// Bewusst ohne Platzhalterzweig: neue Anfragen müssen hier eingeordnet werden.
pub fn required_capability(req: &Request) -> Capability {
    match req {
        Request::List | Request::Browse(_) | Request::Ticket { .. } | Request::Subscribe => Capability::CatalogRead,
        // Steuerung dieses Daemons: nur lokal (bzw. künftig admin mit Bestätigung).
        Request::Publish { .. }
        | Request::Download { .. }
        | Request::Delete { .. }
        | Request::Sync
        | Request::Queue
        | Request::QueueCancel { .. }
        | Request::QueueRetry { .. }
        | Request::Peers
        | Request::Info
        | Request::PairInvite
        | Request::PairInviteWait
        | Request::PairJoin { .. }
        | Request::PairList
        | Request::PairRemove { .. }
        | Request::PairGrant { .. }
        | Request::PairRevoke { .. }
        | Request::Audit { .. }
        | Request::ShareAdd { .. }
        | Request::ShareList
        | Request::ShareRemove { .. }
        | Request::ShareRescan { .. }
        | Request::PinAdd { .. }
        | Request::PinList
        | Request::PinRemove { .. }
        // Lokaler Index und Dateisystem bleiben auf dem Gerät (kein Zugriff für Geräte).
        | Request::Search { .. }
        | Request::ListDir { .. }
        | Request::IndexRoots
        | Request::IndexAdd { .. }
        | Request::IndexRemove { .. }
        | Request::IndexRescan
        // Lokale Dateioperationen (Geräte nutzen das eigene Protokoll snap/ops/1).
        | Request::FileOp { .. }
        | Request::FileOpCancel { .. }
        | Request::Undo
        | Request::TrashList
        | Request::TrashRestore { .. }
        | Request::TrashPurge { .. }
        | Request::TrashEmpty
        | Request::RemoteOp { .. }
        | Request::OpsPending
        | Request::OpsApprove { .. }
        | Request::OpsReject { .. }
        // Vorschaubilder/Metadaten beliebiger lokaler Dateien: nur lokal (für Geräte später
        // über Freigaben mit files.read).
        | Request::Thumbnail { .. }
        | Request::Metadata { .. }
        | Request::StreamUrl { .. }
        | Request::RemoteThumbnail { .. }
        // Einstellungen: nur lokal – ein Gerät darf sich keine größeren Grenzen geben.
        | Request::Settings
        | Request::SettingSet { .. }
        | Request::Upload { .. }
        | Request::UploadList
        | Request::UploadCancel { .. }
        | Request::UploadRetry
        | Request::UploadRuleAdd { .. }
        | Request::UploadRuleList
        | Request::UploadRuleRemove { .. } => Capability::Admin,
    }
}

/// Prüft ein Recht. `path` ist bei ordnergebundenen Rechten das Ziel der Operation,
/// `protected` die immer gesperrten Pfade.
pub fn authorize(
    db: &DatabaseManager,
    protected: &[PathBuf],
    caller: &Caller,
    cap: Capability,
    path: Option<&Path>,
    operation: &str,
) -> Result<()> {
    let node_id = match caller {
        Caller::Local => return Ok(()),
        Caller::Device(id) => id,
    };
    let target = path.map(|p| p.display().to_string());
    let decision = decide(db, protected, node_id, cap, path);
    // Häufige, erlaubte Leserechte nicht protokollieren (der Abgleich läuft dauernd).
    // Ebenso erlaubte Vorschaubild-Abrufe (eine Galerie fragt Hunderte auf einmal an).
    let noisy = decision.is_ok() && (cap == Capability::CatalogRead || operation == "thumb.get");
    if !noisy {
        let detail = decision.as_ref().err().map(|e| e.to_string());
        let _ = db.audit(node_id, operation, target.as_deref(), decision.is_ok(), detail.as_deref());
    }
    decision
}

fn decide(
    db: &DatabaseManager,
    protected: &[PathBuf],
    node_id: &str,
    cap: Capability,
    path: Option<&Path>,
) -> Result<()> {
    if cap == Capability::Admin {
        api_bail!(PermissionDenied, "Fernverwaltung ist für gekoppelte Geräte nicht freigegeben");
    }
    let folders = db.grant_folders(node_id, cap.as_str())?;
    if folders.is_empty() {
        api_bail!(PermissionDenied, "Recht '{}' wurde diesem Gerät nicht gewährt", cap.as_str());
    }
    if !cap.needs_folder() {
        return Ok(());
    }
    let Some(path) = path else {
        api_bail!(PermissionDenied, "Recht '{}' braucht einen Zielpfad", cap.as_str());
    };
    let target = resolve_for_check(path)?;
    if is_protected(&target, protected) {
        api_bail!(PermissionDenied, "Zugriff auf geschützten Pfad verweigert");
    }
    let inside = folders.iter().filter(|f| !f.is_empty()).any(|f| target.starts_with(Path::new(f)));
    if !inside {
        api_bail!(PermissionDenied, "Pfad liegt außerhalb der freigegebenen Ordner");
    }
    Ok(())
}

/// Kanonisiert einen Pfad zur Prüfung. `..` und `.` sind grundsätzlich verboten.
/// Für noch nicht existierende Ziele (Schreiben) wird der längste existierende
/// Vorfahre kanonisiert (löst Symlinks auf) und der Rest angehängt.
pub fn resolve_for_check(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        api_bail!(InvalidInput, "Pfad darf weder '..' noch '.' enthalten");
    }
    let mut existing = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(canon) = std::fs::canonicalize(&existing) {
            let mut out = canon;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return Ok(out);
        }
        match existing.file_name() {
            Some(name) => rest.push(name.to_os_string()),
            None => api_bail!(InvalidInput, "Pfad nicht auflösbar"),
        }
        existing.pop();
    }
}

/// Systempfade, die nie über Rechte erreichbar sind (zusätzlich zu den Snap-Daten).
const SYSTEM_PATHS: &[&str] = &["/proc", "/sys", "/dev", "/etc", "/boot", "/run"];

fn is_protected(target: &Path, protected: &[PathBuf]) -> bool {
    SYSTEM_PATHS.iter().any(|p| target.starts_with(p))
        || protected.iter().any(|p| {
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            target.starts_with(&canon)
        })
}

/// Prüft einen Ordner, bevor er in einem Recht freigegeben wird.
pub fn validate_grant_folder(folder: &Path, protected: &[PathBuf]) -> Result<PathBuf> {
    if !folder.is_absolute() {
        api_bail!(InvalidInput, "Ordner muss absolut sein");
    }
    let canon = std::fs::canonicalize(folder)
        .map_err(|_| ApiError::new(ErrorCode::NotFound, format!("Ordner nicht gefunden: {}", folder.display())))?;
    if !canon.is_dir() {
        api_bail!(InvalidInput, "Kein Ordner: {}", canon.display());
    }
    if canon == Path::new("/") {
        api_bail!(InvalidInput, "Das gesamte Dateisystem kann nicht freigegeben werden");
    }
    if is_protected(&canon, protected) {
        api_bail!(InvalidInput, "Geschützter Ordner kann nicht freigegeben werden: {}", canon.display());
    }
    Ok(canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::error::code_of;

    fn setup() -> (tempfile::TempDir, DatabaseManager, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("geteilt");
        let secret = dir.path().join("snapdaten");
        std::fs::create_dir_all(shared.join("unter")).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(shared.join("a.txt"), b"a").unwrap();
        std::fs::write(secret.join("secret.key"), b"k").unwrap();
        let db = DatabaseManager::open_in_memory().unwrap();
        db.add_device("dev", "Handy").unwrap();
        (dir, db, std::fs::canonicalize(shared).unwrap(), std::fs::canonicalize(secret).unwrap())
    }

    fn check(db: &DatabaseManager, prot: &[PathBuf], cap: Capability, path: Option<&Path>) -> Result<()> {
        authorize(db, prot, &Caller::Device("dev".into()), cap, path, "test")
    }

    #[test]
    fn local_may_do_everything() {
        let (_d, db, _s, _p) = setup();
        for cap in Capability::ALL {
            authorize(&db, &[], &Caller::Local, cap, None, "test").unwrap();
        }
    }

    #[test]
    fn pairing_grants_only_catalog_read() {
        let (_d, db, shared, _p) = setup();
        check(&db, &[], Capability::CatalogRead, None).unwrap();
        for cap in [
            Capability::FilesRead,
            Capability::FilesWrite,
            Capability::PinRequest,
            Capability::FilesDelete,
            Capability::FilesPurge,
        ] {
            let e = check(&db, &[], cap, Some(&shared.join("a.txt"))).unwrap_err();
            assert_eq!(code_of(&e), ErrorCode::PermissionDenied, "{cap:?}");
        }
    }

    #[test]
    fn admin_is_always_denied_for_devices() {
        let (_d, db, _s, _p) = setup();
        db.add_grant("dev", "admin", None).unwrap(); // selbst wenn eingetragen
        assert_eq!(code_of(&check(&db, &[], Capability::Admin, None).unwrap_err()), ErrorCode::PermissionDenied);
    }

    #[test]
    fn folder_scope_and_escapes() {
        let (dir, db, shared, secret) = setup();
        let prot = vec![secret.clone()];
        db.add_grant("dev", "files.read", Some(shared.to_str().unwrap())).unwrap();
        check(&db, &prot, Capability::FilesRead, Some(&shared.join("a.txt"))).unwrap();
        check(&db, &prot, Capability::FilesRead, Some(&shared.join("unter/neu.txt"))).unwrap();
        // Ausbruch per ..
        let esc = shared.join("../snapdaten/secret.key");
        assert!(check(&db, &prot, Capability::FilesRead, Some(&esc)).is_err());
        // Ausbruch per .. in nicht existierendem Teil
        let esc2 = shared.join("gibtsnicht/../../snapdaten/secret.key");
        assert!(check(&db, &prot, Capability::FilesRead, Some(&esc2)).is_err());
        // Ausbruch per Symlink im freigegebenen Ordner
        std::os::unix::fs::symlink(&secret, shared.join("link")).unwrap();
        assert!(check(&db, &prot, Capability::FilesRead, Some(&shared.join("link/secret.key"))).is_err());
        // Präfix-Trick: /…/geteilt-anders ist nicht /…/geteilt
        let sibling = dir.path().join("geteilt-anders");
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(check(&db, &prot, Capability::FilesRead, Some(&sibling.join("x"))).is_err());
        // Anderes Recht im selben Ordner bleibt verboten
        assert!(check(&db, &prot, Capability::FilesWrite, Some(&shared.join("a.txt"))).is_err());
        // Relative Pfade
        assert!(check(&db, &prot, Capability::FilesRead, Some(Path::new("a.txt"))).is_err());
    }

    #[test]
    fn protected_paths_stay_locked_even_inside_granted_folder() {
        let (dir, db, _shared, secret) = setup();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        db.add_grant("dev", "files.read", Some(root.to_str().unwrap())).unwrap();
        let prot = vec![secret.clone()];
        assert!(check(&db, &prot, Capability::FilesRead, Some(&secret.join("secret.key"))).is_err());
        assert!(check(&db, &prot, Capability::FilesRead, Some(Path::new("/etc/passwd"))).is_err());
    }

    #[test]
    fn grants_vanish_with_pairing_and_are_audited() {
        let (_d, db, shared, _p) = setup();
        db.add_grant("dev", "files.read", Some(shared.to_str().unwrap())).unwrap();
        assert!(check(&db, &[], Capability::FilesRead, Some(&shared.join("a.txt"))).is_ok());
        assert!(check(&db, &[], Capability::FilesWrite, Some(&shared.join("a.txt"))).is_err());
        // Wiederholung innerhalb von 60 s wird nicht erneut geschrieben
        assert!(check(&db, &[], Capability::FilesWrite, Some(&shared.join("a.txt"))).is_err());
        let log = db.audit_log(10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(!log[0].allowed && log[1].allowed);
        db.remove_device("dev").unwrap();
        assert!(check(&db, &[], Capability::CatalogRead, None).is_err(), "Entkoppelt = keine Rechte");
        assert!(db.grants_of("dev").unwrap().is_empty());
    }

    #[test]
    fn grant_folder_validation() {
        let (_d, _db, shared, secret) = setup();
        validate_grant_folder(&shared, &[]).unwrap();
        assert!(validate_grant_folder(Path::new("/"), &[]).is_err());
        assert!(validate_grant_folder(Path::new("/etc"), &[]).is_err());
        assert!(validate_grant_folder(&secret, std::slice::from_ref(&secret)).is_err());
        assert!(validate_grant_folder(&shared.join("a.txt"), &[]).is_err());
        assert!(validate_grant_folder(Path::new("relativ"), &[]).is_err());
    }

    #[test]
    fn capability_names_roundtrip() {
        for c in Capability::ALL {
            assert_eq!(Capability::parse(c.as_str()).unwrap(), c);
        }
        assert!(Capability::parse("root").is_err());
    }
}
