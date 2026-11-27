//! Gerät finden: ein eigenes Gerät klingeln lassen (`snap/ring/1`).
//!
//! - Nur gekoppelte Geräte mit dem ausdrücklich vergebenen Recht `device.ring` dürfen klingeln
//!   lassen (nie Standard). Alle anderen werden vor dem ersten Lesen abgewiesen; entkoppelte
//!   erfahren es (Code 3).
//! - Eine Nachricht je Verbindung: `{"v":1,"stop":false}`. Wie lange und wie laut es klingelt,
//!   legt allein der Empfänger fest ([`RING_SECS`]); der Sender kann nur starten oder stoppen.
//! - Drosselung: [`PER_MINUTE`] Anfragen je Gerät. Jede angenommene Anfrage steht im Audit-Log.
//! - Den Ton spielt die Oberfläche (Android: Wecker-Lautstärke, mit „Stopp“ in der Mitteilung;
//!   Desktop: in der App bzw. als Systemmitteilung). Der Kern meldet nur [`Event::Ring`].

use crate::api::auth::{authorize, Caller, Capability};
use crate::api::error::{ApiError, ErrorCode};
use crate::api_bail;
use crate::daemon::DaemonInner;
use crate::events::Event;
use crate::ipc::{read_frame_max, write_frame_max, Response};
use crate::network::endpoint_addr;
use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

pub const RING_ALPN: &[u8] = b"snap/ring/1";
const VERSION: u32 = 1;
/// So lange klingelt das Gerät (Sekunden), wenn niemand stoppt.
pub const RING_SECS: u32 = 30;
/// Höchstens so viele Anfragen je Gerät und Minute.
pub const PER_MINUTE: usize = 6;
const MAX_FRAME: u32 = 256;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RingRequest {
    v: u32,
    stop: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RingReply {
    ok: bool,
    #[serde(default)]
    message: Option<String>,
}

/// Drosselung je Gerät.
#[derive(Default)]
pub struct RingState {
    recent: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RingState {
    fn allow(&self, peer: &str) -> bool {
        let mut m = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, v| {
            v.retain(|t| t.elapsed() < Duration::from_secs(60));
            !v.is_empty()
        });
        let v = m.entry(peer.to_string()).or_default();
        if v.len() >= PER_MINUTE {
            return false;
        }
        v.push(Instant::now());
        true
    }
}

pub struct RingProtocol {
    /// Wird nach dem Start gesetzt (wie bei den Fernoperationen).
    pub inner: Arc<OnceLock<Weak<DaemonInner>>>,
}

impl std::fmt::Debug for RingProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RingProtocol")
    }
}

impl ProtocolHandler for RingProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let Some(inner) = self.inner.get().and_then(Weak::upgrade) else {
            conn.close(2u32.into(), b"starting");
            return Ok(());
        };
        let peer = conn.remote_id().to_string();
        // Recht prüfen, bevor irgendetwas gelesen wird
        let caller = Caller::Device(peer.clone());
        if authorize(&inner.db, &[], &caller, Capability::Ring, None, "device.ring").is_err() {
            if inner.db.is_paired(&peer).unwrap_or(false) {
                conn.close(1u32.into(), b"not permitted");
            } else {
                crate::pairing::reject(&inner.db, &conn);
            }
            return Ok(());
        }
        if let Err(e) = serve(&inner, &peer, &conn).await {
            tracing::debug!("Klingeln von {}: {e:#}", &peer[..10.min(peer.len())]);
        }
        let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
        Ok(())
    }
}

async fn serve(inner: &Arc<DaemonInner>, peer: &str, conn: &Connection) -> Result<()> {
    let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await??;
    let reply = |ok: bool, msg: Option<&str>| RingReply { ok, message: msg.map(str::to_string) };
    if !inner.ring.allow(peer) {
        write_frame_max(&mut send, &reply(false, Some("busy")), MAX_FRAME).await?;
        return Ok(());
    }
    let req: RingRequest = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
    let Some(stop) = accept(&req) else {
        write_frame_max(&mut send, &reply(false, Some("invalid")), MAX_FRAME).await?;
        return Ok(());
    };
    let name = inner
        .db
        .find_devices(peer)?
        .into_iter()
        .find(|d| d.node_id == peer)
        .map(|d| d.name)
        .unwrap_or_else(|| "Gerät".into());
    let _ = inner.db.audit(peer, if stop { "device.ring.stop" } else { "device.ring" }, None, true, None);
    let secs = if stop { 0 } else { RING_SECS };
    inner.db.events().emit(Event::Ring { node_id: peer.to_string(), name, secs });
    write_frame_max(&mut send, &reply(true, None), MAX_FRAME).await?;
    let _ = send.finish();
    Ok(())
}

/// Prüft eine Anfrage: richtige Version. `Some(stop)` oder `None` (abweisen).
fn accept(req: &RingRequest) -> Option<bool> {
    (req.v == VERSION).then_some(req.stop)
}

/// Eine rohe Nachricht wie der Empfänger prüfen (für Fuzzing und Tests): `Some(stop)` oder
/// `None`, wenn sie abgewiesen würde.
pub fn decode(bytes: &[u8]) -> Option<bool> {
    serde_json::from_slice::<RingRequest>(bytes).ok().as_ref().and_then(accept)
}

/// Ein eigenes Gerät klingeln lassen (`stop = false`) oder das Klingeln beenden.
pub(crate) async fn send(inner: &Arc<DaemonInner>, device: &str, stop: bool) -> Result<Response> {
    let mut found = inner.db.find_devices(device)?;
    let dev = match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'."),
        1 => found.remove(0),
        _ => api_bail!(Ambiguous, "'{device}' passt auf mehrere Geräte – bitte genauer angeben."),
    };
    let id: iroh::EndpointId = dev.node_id.parse().context("Ungültige Node-ID")?;
    let hints = inner.db.addr_hints(&dev.node_id).unwrap_or_default();
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    let conn = tokio::time::timeout(timeout, inner.ledger.endpoint.connect(endpoint_addr(id, &hints), RING_ALPN))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar.", dev.name)))?
        .map_err(|e| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar: {e}", dev.name)))?;
    let result = async {
        let (mut s, mut r) = conn.open_bi().await?;
        write_frame_max(&mut s, &RingRequest { v: VERSION, stop }, MAX_FRAME).await?;
        s.finish()?;
        let reply: RingReply = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut r, MAX_FRAME)).await??;
        anyhow::Ok(reply)
    }
    .await;
    crate::pairing::handle_close(&inner.db, &conn);
    let not_permitted = matches!(
        conn.close_reason(),
        Some(iroh::endpoint::ConnectionError::ApplicationClosed(c)) if c.error_code == 1u32.into()
    );
    conn.close(0u32.into(), b"done");
    match result {
        Ok(RingReply { ok: true, .. }) if stop => {
            Ok(Response::Success { message: format!("'{}' klingelt nicht mehr.", dev.name) })
        }
        Ok(RingReply { ok: true, .. }) => {
            Ok(Response::Success { message: format!("'{}' klingelt {RING_SECS} Sekunden lang.", dev.name) })
        }
        Ok(RingReply { message, .. }) if message.as_deref() == Some("busy") => {
            api_bail!(Unavailable, "'{}' nimmt gerade nichts an – bitte gleich noch einmal.", dev.name)
        }
        Ok(_) => api_bail!(InvalidInput, "'{}' hat die Anfrage abgelehnt.", dev.name),
        Err(_) if not_permitted => api_bail!(
            PermissionDenied,
            "'{}' erlaubt dir das nicht. Auf dem Gerät unter Geräte „Klingeln lassen“ erlauben.",
            dev.name
        ),
        Err(e) => {
            Err(ApiError::new(ErrorCode::Unavailable, format!("Verbindung zu '{}' abgebrochen: {e:#}", dev.name))
                .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nachrichten_pruefen() {
        assert_eq!(decode(br#"{"v":1,"stop":false}"#), Some(false));
        assert_eq!(decode(br#"{"v":1,"stop":true}"#), Some(true));
        assert_eq!(decode(br#"{"v":2,"stop":false}"#), None);
        assert_eq!(decode(br#"{"v":1}"#), None);
        assert_eq!(decode(br#"{"v":1,"stop":false,"secs":3600}"#), None, "Dauer bestimmt nur der Empfänger");
    }

    #[test]
    fn drosselung_je_geraet() {
        let s = RingState::default();
        for _ in 0..PER_MINUTE {
            assert!(s.allow("a"));
        }
        assert!(!s.allow("a"));
        assert!(s.allow("b"), "andere Geräte sind nicht betroffen");
    }
}
