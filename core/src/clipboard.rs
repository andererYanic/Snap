//! Zwischenablage zwischen eigenen Geräten (`snap/clip/1`).
//!
//! - Nur gekoppelte Geräte mit dem ausdrücklich vergebenen Recht `clipboard.write` dürfen etwas
//!   in die Zwischenablage dieses Geräts legen (nie Standard). Alle anderen werden vor dem ersten
//!   Lesen abgewiesen; entkoppelte erfahren es (Code 3).
//! - Eine Nachricht je Verbindung: `{"v":1,"text":"…"}`, höchstens [`MAX_TEXT`] Byte; der Text
//!   wird von Steuerzeichen bereinigt. Drosselung: [`PER_MINUTE`] je Gerät.
//! - Der Text wird nie protokolliert und nie auf die Platte geschrieben. Ist gerade keine
//!   Oberfläche verbunden (Desktop-App zu), hält der Dienst ihn bis zu [`PENDING_SECS`] im
//!   Speicher, damit die App ihn beim Öffnen übernehmen kann.

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

pub const CLIP_ALPN: &[u8] = b"snap/clip/1";
const VERSION: u32 = 1;
/// Größter Text (Byte).
pub const MAX_TEXT: usize = 64 * 1024;
const MAX_FRAME: u32 = MAX_TEXT as u32 + 256;
/// Höchstens so viele Nachrichten je Gerät und Minute.
pub const PER_MINUTE: usize = 30;
/// So lange hält der Dienst eine empfangene Zwischenablage für die App bereit.
pub const PENDING_SECS: u64 = 300;
const IO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClipRequest {
    v: u32,
    text: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ClipReply {
    ok: bool,
    #[serde(default)]
    message: Option<String>,
}

/// Zustand des Empfangs: Drosselung je Gerät und die zuletzt empfangene Zwischenablage.
#[derive(Default)]
pub struct ClipState {
    recent: Mutex<HashMap<String, Vec<Instant>>>,
    pending: Mutex<Option<(String, String, Instant)>>,
}

impl ClipState {
    fn allow(&self, peer: &str) -> bool {
        let mut m = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let v = m.entry(peer.to_string()).or_default();
        v.retain(|t| t.elapsed() < Duration::from_secs(60));
        if v.len() >= PER_MINUTE {
            return false;
        }
        v.push(Instant::now());
        true
    }

    /// Zuletzt empfangene Zwischenablage abholen (einmalig, höchstens 5 Minuten alt).
    pub fn take_pending(&self) -> Option<(String, String)> {
        let p = self.pending.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        (p.2.elapsed() < Duration::from_secs(PENDING_SECS)).then_some((p.0, p.1))
    }
}

pub struct ClipProtocol {
    /// Wird nach dem Start gesetzt (wie bei den Fernoperationen).
    pub inner: Arc<OnceLock<Weak<DaemonInner>>>,
}

impl std::fmt::Debug for ClipProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipProtocol")
    }
}

impl ProtocolHandler for ClipProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let Some(inner) = self.inner.get().and_then(Weak::upgrade) else {
            conn.close(2u32.into(), b"starting");
            return Ok(());
        };
        let peer = conn.remote_id().to_string();
        // Recht prüfen, bevor irgendetwas gelesen wird
        let caller = Caller::Device(peer.clone());
        if authorize(&inner.db, &[], &caller, Capability::Clipboard, None, "clipboard.write").is_err() {
            if inner.db.is_paired(&peer).unwrap_or(false) {
                conn.close(1u32.into(), b"not permitted");
            } else {
                crate::pairing::reject(&inner.db, &conn);
            }
            return Ok(());
        }
        if let Err(e) = serve(&inner, &peer, &conn).await {
            tracing::debug!("Zwischenablage von {}: {e:#}", &peer[..10.min(peer.len())]);
        }
        let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
        Ok(())
    }
}

async fn serve(inner: &Arc<DaemonInner>, peer: &str, conn: &Connection) -> Result<()> {
    let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await??;
    let reply = |ok: bool, msg: Option<&str>| ClipReply { ok, message: msg.map(str::to_string) };
    if !inner.clipboard.allow(peer) {
        write_frame_max(&mut send, &reply(false, Some("busy")), MAX_FRAME).await?;
        return Ok(());
    }
    let req: ClipRequest = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
    let Some(text) = accept_text(&req) else {
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
    // Protokoll ohne Inhalt
    let _ = inner.db.audit(peer, "clipboard.write", None, true, None);
    *inner.clipboard.pending.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((name.clone(), text.clone(), Instant::now()));
    inner.db.events().emit(Event::ClipboardReceived { node_id: peer.to_string(), name, text });
    write_frame_max(&mut send, &reply(true, None), MAX_FRAME).await?;
    let _ = send.finish();
    Ok(())
}

/// Prüft eine Anfrage: richtige Version, Größe, bereinigter und nicht leerer Text.
fn accept_text(req: &ClipRequest) -> Option<String> {
    if req.v != VERSION || req.text.len() > MAX_TEXT {
        return None;
    }
    let text = crate::drop::wire::clean_text(&req.text);
    (!text.trim().is_empty()).then_some(text)
}

/// Eine rohe Nachricht wie der Empfänger prüfen (für Fuzzing und Tests): bereinigter Text oder
/// `None`, wenn sie abgewiesen würde.
pub fn decode(bytes: &[u8]) -> Option<String> {
    serde_json::from_slice::<ClipRequest>(bytes).ok().as_ref().and_then(accept_text)
}

/// Text in die Zwischenablage eines eigenen Geräts legen.
pub(crate) async fn send(inner: &Arc<DaemonInner>, device: &str, text: String) -> Result<Response> {
    if text.trim().is_empty() {
        api_bail!(InvalidInput, "Die Zwischenablage ist leer.");
    }
    if text.len() > MAX_TEXT {
        api_bail!(InvalidInput, "Zu viel Text (höchstens 64 KB).");
    }
    let mut found = inner.db.find_devices(device)?;
    let dev = match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'."),
        1 => found.remove(0),
        _ => api_bail!(Ambiguous, "'{device}' passt auf mehrere Geräte – bitte genauer angeben."),
    };
    let id: iroh::EndpointId = dev.node_id.parse().context("Ungültige Node-ID")?;
    let hints = inner.db.addr_hints(&dev.node_id).unwrap_or_default();
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    let conn = tokio::time::timeout(timeout, inner.ledger.endpoint.connect(endpoint_addr(id, &hints), CLIP_ALPN))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar.", dev.name)))?
        .map_err(|e| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar: {e}", dev.name)))?;
    let result = async {
        let (mut s, mut r) = conn.open_bi().await?;
        write_frame_max(&mut s, &ClipRequest { v: VERSION, text }, MAX_FRAME).await?;
        s.finish()?;
        let reply: ClipReply = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut r, MAX_FRAME)).await??;
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
        Ok(ClipReply { ok: true, .. }) => Ok(Response::Success { message: format!("An '{}' gesendet.", dev.name) }),
        Ok(ClipReply { message, .. }) if message.as_deref() == Some("busy") => {
            api_bail!(Unavailable, "'{}' nimmt gerade nichts an – bitte gleich noch einmal.", dev.name)
        }
        Ok(_) => api_bail!(InvalidInput, "'{}' hat die Zwischenablage abgelehnt.", dev.name),
        Err(_) if not_permitted => api_bail!(
            PermissionDenied,
            "'{}' erlaubt dir das nicht. Auf dem Gerät unter Geräte „Zwischenablage füllen“ erlauben.",
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
        assert_eq!(decode(br#"{"v":1,"text":"hallo\u0007"}"#).as_deref(), Some("hallo"));
        assert_eq!(decode(br#"{"v":1,"text":"  "}"#), None);
        assert_eq!(decode(br#"{"v":2,"text":"x"}"#), None);
        assert_eq!(decode(br#"{"v":1,"text":"x","mehr":1}"#), None);
        let big = format!(r#"{{"v":1,"text":"{}"}}"#, "x".repeat(MAX_TEXT + 1));
        assert_eq!(decode(big.as_bytes()), None);
    }
}
