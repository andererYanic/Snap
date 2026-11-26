//! Snap Drop: Dateien an Personen in der Nähe senden (Entwurf und Risiken: `docs/DROP.md`).
//!
//! Getrennt vom Koppeln: Drop gibt nie Rechte auf Freigaben, es überträgt nur ausdrücklich
//! angebotene und angenommene Dateien.

pub mod discovery;
pub mod identity;
pub mod service;
pub mod wire;

pub use service::{
    BlockedSender, ContactInfo, DropOptions, DropService, DropStatus, HotspotOffer, IncomingOffer, Mode, NearbyPeer,
    OfferedFile, SendResult,
};
