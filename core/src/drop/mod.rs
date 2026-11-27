//! Snap Drop: Dateien an Personen in der Nähe senden (Entwurf und Risiken: `docs/DROP.md`).
//!
//! Getrennt vom Koppeln: Drop gibt nie Rechte auf Freigaben, es überträgt nur ausdrücklich
//! angebotene und angenommene Dateien.

pub mod discovery;
pub mod identity;
pub mod service;
pub mod wake;
pub mod wire;

pub use service::{
    BlockedSender, ContactInfo, DropOptions, DropService, DropStatus, HistoryItem, HotspotOffer, IncomingOffer, Mode,
    NearbyPeer, OfferedFile, SendResult, WakeKey,
};

/// Abgelaufene empfangene Dateien in den Papierkorb (nur normale Dateien, nie über Symlinks;
/// verschobene oder gelöschte Dateien werden übersprungen). Blockierend.
pub(crate) fn expire_due(inner: &crate::daemon::DaemonInner) {
    let now = crate::database::now_unix();
    let Ok(paths) = inner.db.take_expired_drops(now, 500) else { return };
    let mut n = 0;
    for p in paths {
        let path = std::path::Path::new(&p);
        let regular = std::fs::symlink_metadata(path).map(|m| m.is_file()).unwrap_or(false);
        if regular && inner.files.trash.trash(path).is_ok() {
            n += 1;
        }
    }
    if n > 0 {
        tracing::info!("Snap Drop: {n} abgelaufene Datei(en) in den Papierkorb gelegt");
    }
}
