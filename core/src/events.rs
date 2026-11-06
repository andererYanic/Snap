//! Ereignis-Stream für Live-Updates der Oberflächen.
//!
//! Ereignisse entstehen in der Datenbankschicht direkt nach dem Commit jeder Änderung –
//! so kann keine Aufrufstelle ein Ereignis vergessen. Abonnenten (Desktop/Mobile über
//! IPC oder FFI) erhalten sie über einen Broadcast-Kanal. Langsame Abonnenten verlieren
//! alte Ereignisse und bekommen stattdessen `Lagged` – sie laden dann neu.

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Kapazität des Kanals. Ein Abonnent, der mehr als so viele Ereignisse zurückliegt,
/// erhält `Lagged`.
const CAPACITY: usize = 1024;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Event {
    /// Eigene Freigaben haben sich geändert (veröffentlicht, gelöscht, endgültig entfernt).
    LocalCatalogChanged,
    /// Dateilisten anderer Geräte haben sich geändert.
    RemoteCatalogChanged {
        new_entries: usize,
    },
    /// Ein Peer ist erreichbar geworden oder nicht mehr erreichbar.
    PeerStatusChanged {
        node_id: String,
        status: String,
    },
    JobQueued {
        job_id: i64,
    },
    JobCompleted {
        job_id: i64,
    },
    JobFailed {
        job_id: i64,
        error: String,
    },
    JobRemoved {
        job_id: i64,
    },
    DevicePaired {
        node_id: String,
        name: String,
    },
    DeviceRemoved {
        node_id: String,
    },
    /// Eine Ordner-Freigabe wurde (neu) gescannt.
    ShareScanned {
        name: String,
        changed: usize,
        deleted: usize,
        total: usize,
    },
    /// Pins wurden abgeglichen (neue Downloads, entfernte Kopien, Konflikte).
    PinsChanged {
        queued: usize,
        removed: usize,
        conflicts: usize,
    },
    /// Rechte eines Geräts wurden geändert.
    GrantsChanged {
        node_id: String,
    },
    /// Fortschritt einer Dateioperation (höchstens 4× pro Sekunde).
    FileOpProgress {
        job: u64,
        done_files: u64,
        total_files: u64,
        done_bytes: u64,
        total_bytes: u64,
    },
    /// Dateioperation beendet (erfolgreich, fehlgeschlagen oder abgebrochen).
    FileOpFinished {
        job: u64,
        ok: bool,
        message: String,
    },
    /// Ein Gerät hat mehr Änderungen angefragt als erlaubt – wartet auf Bestätigung.
    ApprovalRequested {
        id: i64,
        node_id: String,
        summary: String,
    },
    /// Ein Upload-Auftrag hat seinen Zustand geändert ('pending' | 'done' | 'failed').
    UploadChanged {
        id: i64,
        state: String,
        message: String,
    },
    /// Fortschritt der Duplikatsuche (höchstens 2× pro Sekunde).
    DuplicateScanProgress {
        done_bytes: u64,
        total_bytes: u64,
    },
    /// Eine Einstellung wurde zur Laufzeit geändert.
    SettingChanged {
        key: String,
        value: i64,
    },
    /// Der Abonnent hat Ereignisse verpasst und sollte seinen Zustand neu laden.
    Lagged {
        missed: u64,
    },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self { tx: broadcast::channel(CAPACITY).0 }
    }
}

impl EventBus {
    /// Sendet ein Ereignis. Ohne Abonnenten kostet das praktisch nichts.
    pub fn emit(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> Subscription {
        Subscription { rx: self.tx.subscribe() }
    }
}

pub struct Subscription {
    rx: broadcast::Receiver<Event>,
}

impl Subscription {
    /// Nächstes Ereignis; `None`, wenn der Bus geschlossen wurde.
    pub async fn next(&mut self) -> Option<Event> {
        match self.rx.recv().await {
            Ok(e) => Some(e),
            Err(broadcast::error::RecvError::Lagged(missed)) => Some(Event::Lagged { missed }),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lagging_subscriber_gets_lagged_event() {
        let bus = EventBus::default();
        let mut sub = bus.subscribe();
        for i in 0..(CAPACITY as i64 + 10) {
            bus.emit(Event::JobQueued { job_id: i });
        }
        assert!(matches!(sub.next().await, Some(Event::Lagged { missed: 10 })));
        assert!(matches!(sub.next().await, Some(Event::JobQueued { job_id: 10 })));
    }

    #[test]
    fn events_serialize_with_type_tag() {
        let json =
            serde_json::to_string(&Event::PeerStatusChanged { node_id: "a".into(), status: "online".into() }).unwrap();
        assert_eq!(json, r#"{"type":"PeerStatusChanged","node_id":"a","status":"online"}"#);
    }
}
