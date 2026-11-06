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
    /// Rechte eines Geräts wurden geändert.
    GrantsChanged {
        node_id: String,
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
