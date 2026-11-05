//! Ledger-Gossip: Austausch signierter Dateilisten zwischen gekoppelten Geräten.
//!
//! - Jedes Gerät ist alleiniger Schreiber seiner eigenen Einträge und signiert sie
//!   mit seinem Node-Schlüssel. Weitergereichte Einträge sind daher nicht fälschbar.
//! - Abgleich per Versionsvektor ("von Gerät X habe ich bis seq N"), seitenweise.
//! - Nur gekoppelte Geräte dürfen das Ledger abfragen.
//! - Über eine Verbindung wird in beide Richtungen abgeglichen: Der Initiator holt
//!   sich die Neuigkeiten, danach holt sich die Gegenseite über dieselbe Verbindung
//!   die des Initiators. Es entsteht keine zusätzliche Verbindung.

use crate::database::DatabaseManager;
use crate::ipc::{read_frame_max, write_frame_max};
use crate::network::{direct_addrs, endpoint_addr};
use anyhow::{bail, Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId, PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

pub const LEDGER_ALPN: &[u8] = b"snap/ledger/1";
const PROTOCOL_VERSION: u32 = 1;
/// Maximale Einträge pro Antwortseite.
pub const PAGE_SIZE: usize = 1000;
const MAX_REQUEST_FRAME: u32 = 1024 * 1024;
const MAX_RESPONSE_FRAME: u32 = 8 * 1024 * 1024;
/// Maximale Anzahl Ursprünge in einem Versionsvektor.
const MAX_ORIGINS: usize = 10_000;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// Wie lange der Initiator auf den Rück-Abgleich der Gegenseite wartet.
const REVERSE_WAIT: Duration = Duration::from_secs(5);
pub const MAX_NAME_LEN: usize = 255;

/// Ein signierter Katalogeintrag. Pfade werden bewusst nicht geteilt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LedgerEntry {
    /// Node-ID des Ursprungsgeräts (Hex) – zugleich der öffentliche Schlüssel.
    pub origin: String,
    /// Stabile ID des Eintrags beim Ursprungsgerät.
    pub entry_id: i64,
    /// Monoton steigende Sequenznummer des Ursprungsgeräts.
    pub seq: i64,
    pub hash: String,
    pub name: String,
    pub size: u64,
    pub created_at: i64,
    pub deleted: bool,
    /// Ed25519-Signatur (Hex) über [`LedgerEntry::signing_bytes`].
    pub signature: String,
}

impl LedgerEntry {
    /// Kanonische Bytes für die Signatur (Domänentrennung + Felder mit Längenpräfix).
    pub fn signing_bytes(&self) -> Vec<u8> {
        fn put(buf: &mut Vec<u8>, bytes: &[u8]) {
            buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(bytes);
        }
        let mut buf = Vec::with_capacity(256);
        buf.extend_from_slice(b"snap-ledger-entry-v1\0");
        put(&mut buf, self.origin.as_bytes());
        buf.extend_from_slice(&self.entry_id.to_be_bytes());
        buf.extend_from_slice(&self.seq.to_be_bytes());
        put(&mut buf, self.hash.as_bytes());
        put(&mut buf, self.name.as_bytes());
        buf.extend_from_slice(&self.size.to_be_bytes());
        buf.extend_from_slice(&self.created_at.to_be_bytes());
        buf.push(self.deleted as u8);
        buf
    }

    pub fn sign(&self, key: &SecretKey) -> String {
        data_encoding::HEXLOWER.encode(&key.sign(&self.signing_bytes()).to_bytes())
    }

    /// Prüft Felder und Signatur des Ursprungsgeräts.
    pub fn verify(&self) -> Result<()> {
        if self.hash.len() != 64 || !self.hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            bail!("ungültiger Hash");
        }
        if self.name.is_empty() || self.name.len() > MAX_NAME_LEN || self.name.chars().any(char::is_control) {
            bail!("ungültiger Dateiname");
        }
        if self.seq <= 0 {
            bail!("ungültige Sequenznummer");
        }
        let origin: PublicKey = self.origin.parse().context("ungültige Ursprungs-ID")?;
        let sig_bytes = data_encoding::HEXLOWER
            .decode(self.signature.as_bytes())
            .ok()
            .and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok())
            .context("ungültiges Signaturformat")?;
        origin
            .verify(&self.signing_bytes(), &Signature::from_bytes(&sig_bytes))
            .context("Signatur ungültig")?;
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct SyncRequest {
    version: u32,
    /// Höchste bekannte Sequenznummer je Ursprung.
    have: HashMap<String, i64>,
    /// Direkte Adressen des Anfragenden (Hinweis für spätere Verbindungen).
    addrs: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct SyncResponse {
    entries: Vec<LedgerEntry>,
    more: bool,
    addrs: Vec<String>,
    device_name: String,
}

/// Gemeinsamer Zustand für Ledger-Abgleiche.
pub struct LedgerCtx {
    pub db: Arc<DatabaseManager>,
    pub endpoint: Endpoint,
    pub device_name: String,
    /// Weckt den Warteschlangen-Worker, wenn ein Peer erreichbar ist.
    pub notifier: Arc<Notify>,
}

impl LedgerCtx {
    fn self_id(&self) -> String {
        self.endpoint.id().to_string()
    }

    /// Ein Peer hat nachweislich geantwortet: Status, Abgleichzeit, Warteschlange.
    fn peer_reached(&self, peer: &str) {
        if let Err(e) = self.db.mark_peer_online(peer) {
            tracing::warn!("Peer-Status nicht aktualisierbar: {e}");
        }
        if self.db.has_waiting_jobs(peer).unwrap_or(false) {
            self.notifier.notify_one();
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PullStats {
    pub new_entries: usize,
    pub device_name: Option<String>,
}

/// Fehler beim Abgleich – entscheidet über den Peer-Status.
#[derive(Debug)]
pub enum SyncError {
    Unreachable(String),
    Failed(anyhow::Error),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Unreachable(e) => write!(f, "nicht erreichbar: {e}"),
            SyncError::Failed(e) => write!(f, "{e:#}"),
        }
    }
}

/// Baut eine Verbindung zu einem gekoppelten Gerät auf und gleicht in beide Richtungen ab.
pub async fn sync_with(ctx: &LedgerCtx, peer: EndpointId, connect_timeout: Duration) -> Result<PullStats, SyncError> {
    let peer_str = peer.to_string();
    let hints = ctx.db.addr_hints(&peer_str).unwrap_or_default();
    let conn = match tokio::time::timeout(connect_timeout, ctx.endpoint.connect(endpoint_addr(peer, &hints), LEDGER_ALPN)).await
    {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(SyncError::Unreachable(e.to_string())),
        Err(_) => return Err(SyncError::Unreachable(format!("Zeitüberschreitung nach {} s", connect_timeout.as_secs()))),
    };

    let stats = pull_on(ctx, &conn, &peer_str).await.map_err(SyncError::Failed);
    if stats.is_ok() {
        // Rück-Abgleich der Gegenseite über dieselbe Verbindung bedienen.
        if let Ok(Ok((send, recv))) = tokio::time::timeout(REVERSE_WAIT, conn.accept_bi()).await {
            if let Err(e) = serve(ctx, send, recv, &peer_str).await {
                tracing::debug!("Rück-Abgleich mit {} fehlgeschlagen: {e:#}", peer.fmt_short());
            }
        }
    }
    conn.close(0u32.into(), b"done");
    stats
}

/// Holt alle Neuigkeiten der Gegenseite (Initiator-Rolle auf dieser Verbindung).
async fn pull_on(ctx: &LedgerCtx, conn: &Connection, peer: &str) -> Result<PullStats> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let self_id = ctx.self_id();
    let mut stats = PullStats::default();
    loop {
        let have = ctx.db.version_vector()?;
        let req = SyncRequest { version: PROTOCOL_VERSION, have, addrs: direct_addrs(&ctx.endpoint) };
        write_frame_max(&mut send, &req, MAX_REQUEST_FRAME).await?;
        let resp: SyncResponse = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_RESPONSE_FRAME))
            .await
            .context("Zeitüberschreitung beim Abgleich")??;
        if resp.entries.len() > PAGE_SIZE {
            bail!("Gegenseite hat zu viele Einträge auf einmal gesendet");
        }
        let SyncResponse { entries, more, addrs, device_name } = resp;
        let page_empty = entries.is_empty();
        let resp_entries = entries;
        // Erst alles prüfen, dann übernehmen: ein manipulierter Eintrag verwirft die ganze Seite.
        // Prüfung (CPU-lastig) und Übernahme (blockierendes SQLite) laufen außerhalb der
        // async-Runtime, damit andere Aufgaben (IPC, Transfers) nicht ausgebremst werden.
        let db = ctx.db.clone();
        let self_id_c = self_id.clone();
        let peer_short = peer[..10.min(peer.len())].to_string();
        let applied = tokio::task::spawn_blocking(move || -> Result<usize> {
            verify_all(&resp_entries).with_context(|| format!("Ungültiger Ledger-Eintrag von {peer_short}"))?;
            // Für eigene Einträge sind wir allein zuständig.
            let foreign: Vec<LedgerEntry> = resp_entries.into_iter().filter(|e| e.origin != self_id_c).collect();
            db.apply_remote_entries(&foreign)
        });
        stats.new_entries += applied.await.context("Abgleich-Task abgebrochen")??;
        let _ = ctx.db.set_addr_hints(peer, &addrs);
        stats.device_name = Some(sanitize_device_name(&device_name, peer));
        if !more {
            break;
        }
        if page_empty {
            bail!("Gegenseite meldet weitere Einträge, sendet aber keine");
        }
    }
    send.finish()?;

    ctx.db.set_last_sync(peer)?;
    if let Some(name) = &stats.device_name {
        ctx.db.add_device(peer, name)?; // Namen aktualisieren
    }
    ctx.peer_reached(peer);
    Ok(stats)
}

/// Prüft alle Einträge, bei großen Seiten parallel auf allen Kernen.
pub fn verify_all(entries: &[LedgerEntry]) -> Result<()> {
    const MIN_PER_THREAD: usize = 64;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    if entries.len() < MIN_PER_THREAD * 2 || threads < 2 {
        return entries.iter().try_for_each(LedgerEntry::verify);
    }
    let chunk = entries.len().div_ceil(threads).max(MIN_PER_THREAD);
    std::thread::scope(|scope| {
        let handles: Vec<_> = entries
            .chunks(chunk)
            .map(|c| scope.spawn(move || c.iter().try_for_each(LedgerEntry::verify)))
            .collect();
        handles
            .into_iter()
            .try_for_each(|h| h.join().unwrap_or_else(|_| bail!("Prüfung abgebrochen")))
    })
}

/// Beantwortet Abgleich-Anfragen auf einem Stream (Responder-Rolle).
async fn serve(
    ctx: &LedgerCtx,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    peer: &str,
) -> Result<()> {
    loop {
        let req: SyncRequest = match tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_REQUEST_FRAME)).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) | Err(_) => break, // Ende des Streams oder Zeitüberschreitung
        };
        if req.version != PROTOCOL_VERSION {
            bail!("Nicht unterstützte Protokollversion {}", req.version);
        }
        if req.have.len() > MAX_ORIGINS {
            bail!("Versionsvektor zu groß");
        }
        let _ = ctx.db.set_addr_hints(peer, &req.addrs);
        let db = ctx.db.clone();
        let have = req.have;
        let mut entries = tokio::task::spawn_blocking(move || db.entries_since(&have, PAGE_SIZE + 1))
            .await
            .context("Abgleich-Task abgebrochen")??;
        let more = entries.len() > PAGE_SIZE;
        entries.truncate(PAGE_SIZE);
        let resp = SyncResponse {
            entries,
            more,
            addrs: direct_addrs(&ctx.endpoint),
            device_name: ctx.device_name.clone(),
        };
        write_frame_max(&mut send, &resp, MAX_RESPONSE_FRAME).await?;
    }
    send.finish()?;
    Ok(())
}

/// Bereinigt einen Gerätenamen von der Gegenseite.
pub fn sanitize_device_name(name: &str, node_id: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        format!("Gerät-{}", &node_id[..6.min(node_id.len())])
    } else {
        cleaned.to_string()
    }
}

/// Protokoll-Handler für eingehende Ledger-Verbindungen.
#[derive(Clone)]
pub struct LedgerProtocol {
    pub ctx: Arc<LedgerCtx>,
}

impl std::fmt::Debug for LedgerProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LedgerProtocol")
    }
}

impl ProtocolHandler for LedgerProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let ctx = &self.ctx;
        let peer = conn.remote_id().to_string();
        if !ctx.db.is_paired(&peer).unwrap_or(false) {
            tracing::debug!("Ledger-Anfrage von nicht gekoppeltem Node abgelehnt");
            conn.close(1u32.into(), b"not paired");
            return Ok(());
        }
        ctx.peer_reached(&peer);

        // 1. Anfrage des Initiators beantworten
        let (send, recv) = match tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await {
            Ok(Ok(s)) => s,
            _ => return Ok(()),
        };
        if let Err(e) = serve(ctx, send, recv, &peer).await {
            tracing::debug!("Ledger-Abgleich (Antwort) fehlgeschlagen: {e:#}");
            conn.close(2u32.into(), b"error");
            return Ok(());
        }
        // 2. Über dieselbe Verbindung die Neuigkeiten des Initiators holen
        match pull_on(ctx, &conn, &peer).await {
            Ok(stats) if stats.new_entries > 0 => {
                tracing::info!("Ledger: {} neue Einträge von '{}'", stats.new_entries, stats.device_name.unwrap_or_default());
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("Rück-Abgleich fehlgeschlagen: {e:#}"),
        }
        conn.close(0u32.into(), b"done");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &SecretKey) -> LedgerEntry {
        let mut e = LedgerEntry {
            origin: key.public().to_string(),
            entry_id: 1,
            seq: 1,
            hash: "a".repeat(64),
            name: "foto.jpg".into(),
            size: 42,
            created_at: 1_700_000_000,
            deleted: false,
            signature: String::new(),
        };
        e.signature = e.sign(key);
        e
    }

    #[test]
    fn valid_signature_verifies() {
        let key = SecretKey::generate();
        entry(&key).verify().unwrap();
    }

    #[test]
    fn tampering_is_detected() {
        let key = SecretKey::generate();
        let good = entry(&key);
        for mutate in [
            |e: &mut LedgerEntry| e.name = "anders.jpg".into(),
            |e: &mut LedgerEntry| e.size = 43,
            |e: &mut LedgerEntry| e.seq = 2,
            |e: &mut LedgerEntry| e.deleted = true,
            |e: &mut LedgerEntry| e.hash = "b".repeat(64),
            |e: &mut LedgerEntry| e.entry_id = 7,
        ] {
            let mut e = good.clone();
            mutate(&mut e);
            assert!(e.verify().is_err());
        }
    }

    #[test]
    fn forged_origin_is_rejected() {
        // Ein anderes Gerät signiert einen Eintrag im Namen des Ursprungsgeräts.
        let victim = SecretKey::generate();
        let attacker = SecretKey::generate();
        let mut e = entry(&victim);
        e.signature = e.sign(&attacker);
        assert!(e.verify().is_err());
    }

    #[test]
    fn invalid_fields_are_rejected() {
        let key = SecretKey::generate();
        let mut e = entry(&key);
        e.name = "a\nb".into();
        e.signature = e.sign(&key);
        assert!(e.verify().is_err(), "Steuerzeichen im Namen");
        let mut e = entry(&key);
        e.name = "x".repeat(MAX_NAME_LEN + 1);
        e.signature = e.sign(&key);
        assert!(e.verify().is_err(), "Name zu lang");
        let mut e = entry(&key);
        e.hash = "Z".repeat(64);
        e.signature = e.sign(&key);
        assert!(e.verify().is_err(), "Hash kein Hex");
    }

    #[test]
    fn parallel_verification_finds_single_bad_entry() {
        let key = SecretKey::generate();
        let mut entries: Vec<LedgerEntry> = (0..200)
            .map(|i| {
                let mut e = entry(&key);
                e.entry_id = i;
                e.signature = e.sign(&key);
                e
            })
            .collect();
        verify_all(&entries).unwrap();
        entries[177].size += 1;
        assert!(verify_all(&entries).is_err());
    }

    #[test]
    fn device_names_are_sanitized() {
        assert_eq!(sanitize_device_name("Pixel\u{7}8", "abcdef123"), "Pixel8");
        assert_eq!(sanitize_device_name("   ", "abcdef123"), "Gerät-abcdef");
        assert_eq!(sanitize_device_name(&"x".repeat(100), "a").len(), 64);
    }
}
