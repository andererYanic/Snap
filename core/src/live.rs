//! Live-Verbindungen („immer live“): eine stehende Verbindung pro gekoppeltem Gerät.
//!
//! Über die Leitung gehen **nur Hinweise** („mein Ledger-Stand ist jetzt N“). Die Daten
//! selbst holt der Empfänger wie bisher per signiertem Pull (`ledger::sync_with`). Ein
//! manipuliertes Gerät kann so keine Daten hineindrücken, höchstens einen (ratenbegrenzten)
//! Abgleich auslösen.
//!
//! - Nur Geräte mit `catalog.read` dürfen sich verbinden (gleiche Prüfstelle wie überall).
//! - Doppelte Verbindungen werden deterministisch aufgelöst: Es bleibt die Verbindung,
//!   die von der kleineren Node-ID ausgeht. Die größere wählt erst nach einer Wartezeit.
//! - Lebenszeichen alle 20 s; 60 s Stille gelten als Verbindungsverlust.
//! - Schutz: Frames max. 4 KiB, max. 50 Nachrichten/s, unbekannte Nachrichten beenden
//!   die Verbindung.

use crate::api::auth::{authorize, Caller, Capability};
use crate::events::Event;
use crate::ipc::{read_frame_max, write_frame_max};
use crate::ledger::LedgerCtx;
use crate::network::endpoint_addr;
use anyhow::{bail, Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::EndpointId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub const LIVE_ALPN: &[u8] = b"snap/live/1";
const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: u32 = 4096;
const HEARTBEAT: Duration = Duration::from_secs(20);
const DEAD_AFTER: Duration = Duration::from_secs(60);
const MAX_MSGS_PER_SEC: u32 = 50;
/// Mindestabstand zwischen zwei Abgleichen mit demselben Gerät.
const MIN_SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// Die größere Node-ID wartet so lange, bevor sie selbst wählt.
const DEFER_GRACE: Duration = Duration::from_secs(10);
const RETRY_BASE: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// Bündelung lokaler Änderungen, bevor ein Hinweis verschickt wird.
const HINT_DEBOUNCE: Duration = Duration::from_millis(300);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "t", deny_unknown_fields)]
enum LiveMsg {
    Hello { v: u32, total: i64 },
    Version { total: i64 },
    Heartbeat,
}

struct PeerConn {
    stable_id: usize,
    outgoing: bool,
    conn: Connection,
    tx: mpsc::Sender<LiveMsg>,
}

#[derive(Default)]
struct SyncState {
    running: bool,
    dirty: bool,
    last_total: Option<i64>,
}

/// Verwaltet alle Live-Verbindungen.
pub struct LiveManager {
    ctx: Arc<LedgerCtx>,
    connect_timeout: Duration,
    self_id: String,
    peers: Mutex<HashMap<String, PeerConn>>,
    sync: Mutex<HashMap<String, SyncState>>,
    dialers: Mutex<HashMap<String, JoinHandle<()>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl LiveManager {
    pub fn new(ctx: Arc<LedgerCtx>, connect_timeout: Duration) -> Arc<Self> {
        let self_id = ctx.endpoint.id().to_string();
        Arc::new(Self {
            ctx,
            connect_timeout,
            self_id,
            peers: Mutex::new(HashMap::new()),
            sync: Mutex::new(HashMap::new()),
            dialers: Mutex::new(HashMap::new()),
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// Startet Wähler für alle gekoppelten Geräte und reagiert auf Änderungen.
    pub fn start(self: &Arc<Self>) {
        for d in self.ctx.db.list_devices().unwrap_or_default() {
            self.ensure_dialer(&d.node_id);
        }
        let me = self.clone();
        let task = tokio::spawn(async move { me.event_loop().await });
        self.tasks.lock().unwrap().push(task);
    }

    pub fn shutdown(&self) {
        for (_, h) in self.dialers.lock().unwrap().drain() {
            h.abort();
        }
        for h in self.tasks.lock().unwrap().drain(..) {
            h.abort();
        }
        for (_, p) in self.peers.lock().unwrap().drain() {
            p.conn.close(0u32.into(), b"shutdown");
        }
    }

    /// Ist das Gerät gerade live verbunden?
    pub fn is_connected(&self, node_id: &str) -> bool {
        self.peers.lock().unwrap().contains_key(node_id)
    }

    pub fn connected_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    fn ledger_total(&self) -> i64 {
        self.ctx.db.ledger_total().unwrap_or(0)
    }

    /// Reagiert auf lokale Ereignisse: Hinweise verschicken, Wähler starten/stoppen.
    async fn event_loop(self: Arc<Self>) {
        let mut sub = self.ctx.db.events().subscribe();
        let mut last_sent = self.ledger_total();
        let mut pending_hint = false;
        loop {
            let event = if pending_hint {
                match tokio::time::timeout(HINT_DEBOUNCE, sub.next()).await {
                    Ok(e) => e,
                    Err(_) => {
                        pending_hint = false;
                        let total = self.ledger_total();
                        if total != last_sent {
                            last_sent = total;
                            self.broadcast(LiveMsg::Version { total });
                        }
                        continue;
                    }
                }
            } else {
                sub.next().await
            };
            let Some(event) = event else { return };
            match event {
                Event::LocalCatalogChanged | Event::RemoteCatalogChanged { .. } | Event::Lagged { .. } => {
                    pending_hint = true
                }
                Event::DevicePaired { node_id, .. } => self.ensure_dialer(&node_id),
                Event::DeviceRemoved { node_id } => {
                    if let Some(h) = self.dialers.lock().unwrap().remove(&node_id) {
                        h.abort();
                    }
                    self.disconnect(&node_id, b"unpaired");
                }
                Event::GrantsChanged { node_id } => {
                    // Wurde catalog.read entzogen, endet die Live-Verbindung sofort.
                    if self.ctx.db.grant_folders(&node_id, Capability::CatalogRead.as_str()).unwrap_or_default().is_empty() {
                        self.disconnect(&node_id, b"revoked");
                    }
                }
                _ => {}
            }
        }
    }

    /// Darf dieses Gerät unseren Ledger-Stand sehen? Gilt auch für Verbindungen, die wir
    /// selbst aufbauen – sonst würden Hinweise an Geräte ohne Recht fließen.
    fn may_see_us(&self, peer: &str) -> bool {
        !self.ctx.db.grant_folders(peer, Capability::CatalogRead.as_str()).unwrap_or_default().is_empty()
    }

    fn broadcast(&self, msg: LiveMsg) {
        for p in self.peers.lock().unwrap().values() {
            // Volle Warteschlange: Hinweis verwerfen – der nächste trägt den neuen Stand.
            let _ = p.tx.try_send(msg.clone());
        }
    }

    fn disconnect(&self, node_id: &str, reason: &[u8]) {
        if let Some(p) = self.peers.lock().unwrap().remove(node_id) {
            p.conn.close(1u32.into(), reason);
        }
    }

    fn ensure_dialer(self: &Arc<Self>, node_id: &str) {
        let mut dialers = self.dialers.lock().unwrap();
        if dialers.get(node_id).is_some_and(|h| !h.is_finished()) {
            return;
        }
        let me = self.clone();
        let peer = node_id.to_string();
        dialers.insert(node_id.to_string(), tokio::spawn(async move { me.dial_loop(peer).await }));
    }

    /// Baut die Verbindung zu einem Gerät auf und hält sie (mit Backoff bei Fehlern).
    async fn dial_loop(self: Arc<Self>, peer: String) {
        let Ok(peer_id) = peer.parse::<EndpointId>() else { return };
        let mut delay = RETRY_BASE;
        let defer = self.self_id > peer; // die kleinere Node-ID wählt zuerst
        loop {
            if self.is_connected(&peer) || !self.may_see_us(&peer) {
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
            if defer {
                tokio::time::sleep(jitter(DEFER_GRACE)).await;
                if self.is_connected(&peer) {
                    continue;
                }
            }
            let hints = self.ctx.db.addr_hints(&peer).unwrap_or_default();
            let attempt =
                tokio::time::timeout(self.connect_timeout, self.ctx.endpoint.connect(endpoint_addr(peer_id, &hints), LIVE_ALPN))
                    .await;
            match attempt {
                Ok(Ok(conn)) => {
                    let started = Instant::now();
                    if let Err(e) = self.clone().session(peer.clone(), conn, true).await {
                        tracing::debug!("Live-Verbindung zu {} beendet: {e:#}", short(&peer));
                    }
                    // Nach einer längeren, stabilen Verbindung sofort neu versuchen.
                    if started.elapsed() > Duration::from_secs(30) {
                        delay = RETRY_BASE;
                        continue;
                    }
                }
                Ok(Err(e)) => tracing::trace!("Live: {} nicht erreichbar: {e}", short(&peer)),
                Err(_) => tracing::trace!("Live: {} Zeitüberschreitung", short(&peer)),
            }
            tokio::time::sleep(jitter(delay)).await;
            delay = (delay * 2).min(RETRY_MAX);
        }
    }

    /// Meldet eine Verbindung an. Gibt `false` zurück, wenn sie als Duplikat verworfen wird.
    fn register(&self, peer: &str, conn: &Connection, outgoing: bool, tx: mpsc::Sender<LiveMsg>) -> bool {
        let mut peers = self.peers.lock().unwrap();
        // Bevorzugt: die Verbindung, die von der kleineren Node-ID ausgeht.
        let preferred = |out: bool| if self.self_id.as_str() < peer { out } else { !out };
        if let Some(existing) = peers.get(peer) {
            if preferred(existing.outgoing) || !preferred(outgoing) {
                return false;
            }
            existing.conn.close(2u32.into(), b"duplicate");
        }
        peers.insert(peer.to_string(), PeerConn { stable_id: conn.stable_id(), outgoing, conn: conn.clone(), tx });
        true
    }

    fn unregister(&self, peer: &str, conn: &Connection) -> bool {
        let mut peers = self.peers.lock().unwrap();
        if peers.get(peer).is_some_and(|p| p.stable_id == conn.stable_id()) {
            peers.remove(peer);
            return true;
        }
        false
    }

    /// Eine Live-Sitzung: Hello austauschen, Hinweise und Lebenszeichen bis zum Ende.
    async fn session(self: Arc<Self>, peer: String, conn: Connection, outgoing: bool) -> Result<()> {
        let (mut send, mut recv) = if outgoing {
            conn.open_bi().await?
        } else {
            tokio::time::timeout(Duration::from_secs(10), conn.accept_bi()).await.context("kein Stream")??
        };
        let (tx, mut rx) = mpsc::channel::<LiveMsg>(16);
        if !self.may_see_us(&peer) {
            conn.close(1u32.into(), b"not permitted");
            return Ok(());
        }
        if !self.register(&peer, &conn, outgoing, tx.clone()) {
            conn.close(2u32.into(), b"duplicate");
            return Ok(());
        }
        self.ctx.peer_reached(&peer);

        // Lesen in eigener Task (Lesen ist nicht abbruchsicher und darf nicht in select! stehen).
        let (in_tx, mut inbox) = mpsc::channel::<Result<LiveMsg>>(32);
        let reader = tokio::spawn(async move {
            loop {
                let msg = read_frame_max::<_, LiveMsg>(&mut recv, MAX_FRAME).await;
                let stop = msg.is_err();
                if in_tx.send(msg).await.is_err() || stop {
                    return;
                }
            }
        });

        let result = async {
            write_frame_max(&mut send, &LiveMsg::Hello { v: PROTOCOL_VERSION, total: self.ledger_total() }, MAX_FRAME).await?;
            let mut heartbeat = tokio::time::interval(HEARTBEAT);
            heartbeat.tick().await;
            let mut last_rx = Instant::now();
            let mut window = (Instant::now(), 0u32);
            loop {
                tokio::select! {
                    incoming = inbox.recv() => {
                        let msg = incoming.context("Leser beendet")??;
                        last_rx = Instant::now();
                        if window.0.elapsed() >= Duration::from_secs(1) {
                            window = (Instant::now(), 0);
                        }
                        window.1 += 1;
                        if window.1 > MAX_MSGS_PER_SEC {
                            bail!("zu viele Nachrichten");
                        }
                        match msg {
                            LiveMsg::Hello { v, total } => {
                                if v != PROTOCOL_VERSION {
                                    bail!("Protokollversion {v} nicht unterstützt");
                                }
                                self.on_total(&peer, total);
                            }
                            LiveMsg::Version { total } => self.on_total(&peer, total),
                            LiveMsg::Heartbeat => {}
                        }
                    }
                    Some(out) = rx.recv() => write_frame_max(&mut send, &out, MAX_FRAME).await?,
                    _ = heartbeat.tick() => {
                        if last_rx.elapsed() > DEAD_AFTER {
                            bail!("keine Lebenszeichen");
                        }
                        write_frame_max(&mut send, &LiveMsg::Heartbeat, MAX_FRAME).await?;
                    }
                }
            }
        }
        .await;

        reader.abort();
        conn.close(0u32.into(), b"bye");
        if self.unregister(&peer, &conn) && !self.is_connected(&peer) {
            let _ = self.ctx.db.mark_peer_disconnected(&peer);
        }
        result
    }

    /// Stand der Gegenseite hat sich geändert -> (ratenbegrenzt) abgleichen.
    fn on_total(self: &Arc<Self>, peer: &str, total: i64) {
        let start = {
            let mut sync = self.sync.lock().unwrap();
            let st = sync.entry(peer.to_string()).or_default();
            if st.last_total == Some(total) {
                return;
            }
            st.last_total = Some(total);
            if st.running {
                st.dirty = true;
                false
            } else {
                st.running = true;
                true
            }
        };
        if !start {
            return;
        }
        let me = self.clone();
        let peer = peer.to_string();
        tokio::spawn(async move {
            let mut last: Option<Instant> = None;
            loop {
                if let Some(l) = last {
                    let wait = MIN_SYNC_INTERVAL.saturating_sub(l.elapsed());
                    tokio::time::sleep(wait).await;
                }
                let r = crate::sync::sync_one(&me.ctx, &me.ctx.db, peer.clone(), String::new(), me.connect_timeout).await;
                if let Some(e) = r.error {
                    tracing::debug!("Live-Abgleich mit {} fehlgeschlagen: {e}", short(&peer));
                }
                last = Some(Instant::now());
                let mut sync = me.sync.lock().unwrap();
                let st = sync.entry(peer.clone()).or_default();
                if st.dirty {
                    st.dirty = false;
                } else {
                    st.running = false;
                    return;
                }
            }
        });
    }
}

fn jitter(d: Duration) -> Duration {
    d.mul_f64(0.8 + fastrand::f64() * 0.4)
}

fn short(s: &str) -> &str {
    &s[..10.min(s.len())]
}

/// Protokoll-Handler für eingehende Live-Verbindungen.
#[derive(Clone)]
pub struct LiveProtocol {
    pub mgr: Arc<LiveManager>,
}

impl std::fmt::Debug for LiveProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LiveProtocol")
    }
}

impl ProtocolHandler for LiveProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id().to_string();
        let caller = Caller::Device(peer.clone());
        if authorize(&self.mgr.ctx.db, &[], &caller, Capability::CatalogRead, None, "live.connect").is_err() {
            conn.close(1u32.into(), b"not paired");
            return Ok(());
        }
        if let Err(e) = self.mgr.clone().session(peer.clone(), conn, false).await {
            tracing::debug!("Live-Verbindung von {} beendet: {e:#}", short(&peer));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_strict() {
        assert_eq!(serde_json::to_string(&LiveMsg::Heartbeat).unwrap(), r#"{"t":"Heartbeat"}"#);
        assert!(serde_json::from_str::<LiveMsg>(r#"{"t":"Version","total":3,"x":1}"#).is_err(), "unbekannte Felder");
        assert!(serde_json::from_str::<LiveMsg>(r#"{"t":"Push","data":"…"}"#).is_err(), "unbekannte Nachricht");
        assert_eq!(
            serde_json::from_str::<LiveMsg>(r#"{"t":"Hello","v":1,"total":7}"#).unwrap(),
            LiveMsg::Hello { v: 1, total: 7 }
        );
    }

    #[test]
    fn jitter_bounds() {
        for _ in 0..100 {
            let j = jitter(Duration::from_secs(10)).as_secs_f64();
            assert!((8.0..=12.0).contains(&j));
        }
    }
}
