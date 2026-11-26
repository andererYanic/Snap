//! Snap Drop: Sichtbarkeit, Empfangen, Senden, Kontakte.
//!
//! - Empfangen läuft nur, solange man sichtbar ist: eigener Iroh-Endpunkt mit Zufallsschlüssel je
//!   Sitzung (getrennt vom Geräte-Endpunkt), mDNS-Bekanntmachung, Protokoll `snap/drop/1`.
//! - Senden nutzt ebenfalls einen eigenen Zufalls-Endpunkt (alle 10 Minuten neu).
//! - Jede Verbindung: Probe → Karte → (Angebot → Antwort → Dateien → Abschluss).
//! - Grenzen gegen Spam und Überlastung siehe `docs/DROP.md`.

use super::discovery::{Advert, Browser, Found};
use super::identity::{self, DropIdentity};
use super::wire::{
    hex, Card, ConnectInfo, DeviceKind, Offer, OfferFile, Probe, Proof, Received, Reply, ValidOffer, DROP_ALPN,
    MAX_FRAME, MAX_TAGS, TOKEN_LEN, VERSION,
};
use crate::api::error::{ApiError, ErrorCode};
use crate::api_bail;
use crate::database::{DatabaseManager, DropContactRow};
use crate::events::Event;
use crate::ipc::{read_frame_max, write_frame_max};
use anyhow::{Context, Result};
use iroh::endpoint::{presets, Connection, RecvStream, SendStream, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, SecretKey, TransportAddr};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{oneshot, Semaphore};

const IO_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// So lange wartet ein Angebot auf die Antwort des Menschen.
pub const OFFER_TIMEOUT: Duration = Duration::from_secs(120);
/// Längste Pause ohne Daten während einer Übertragung.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const EVERYONE_SECS: i64 = 600;
const SESSION_ROTATE: Duration = Duration::from_secs(900);
const SENDER_TTL: Duration = Duration::from_secs(600);
const EXCHANGE_TTL: Duration = Duration::from_secs(900);
const NEARBY_TTL: Duration = Duration::from_secs(120);
const REPROBE: Duration = Duration::from_secs(30);
const MAX_CONNS: usize = 8;
const MAX_PENDING: usize = 3;
const PROBES_PER_MIN: u32 = 30;
const OFFERS_PER_MIN: u32 = 6;
const DECLINES_UNTIL_OFF: u32 = 3;
const MAX_NEARBY: usize = 64;
const CHUNK: usize = 256 * 1024;
const PART_SUFFIX: &str = ".snap-part";

const SETTING_BASE_MODE: &str = "drop_visibility";

/// Einstellungen für Snap Drop.
#[derive(Debug, Clone)]
pub struct DropOptions {
    /// Ablage für empfangene Dateien.
    pub dir: PathBuf,
    pub identity_path: PathBuf,
    pub name: String,
    pub kind: DeviceKind,
    pub use_relay: bool,
    pub relay_urls: Vec<String>,
    pub relay_fallback_n0: bool,
    /// Nur 127.0.0.1 (Tests).
    pub loopback_only: bool,
    /// mDNS bekanntmachen und suchen.
    pub mdns: bool,
    /// So viel Platz muss nach dem Empfang frei bleiben.
    pub reserve_bytes: u64,
    /// Kann einem Hotspot beitreten (Android, Desktop mit NetworkManager).
    pub can_join_hotspot: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Off,
    Contacts,
    Everyone,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Contacts => "contacts",
            Mode::Everyone => "everyone",
        }
    }
}

/// Aktueller Stand für die Oberfläche.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DropStatus {
    pub mode: Mode,
    /// Ende von „Alle“ (Unix-Sekunden).
    pub until: Option<i64>,
    pub name: String,
    /// Fingerabdruck des eigenen Drop-Schlüssels.
    pub fingerprint: String,
    pub dir: String,
    /// Code für QR/Abtippen, solange man sichtbar ist.
    pub code: Option<String>,
}

/// Ein Gerät in der Nähe.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NearbyPeer {
    /// Transport-Kennung (wechselt je Sitzung).
    pub id: String,
    pub name: Option<String>,
    pub kind: Option<DeviceKind>,
    /// Gespeicherter Kontakt (am Schlüssel erkannt).
    pub contact: Option<String>,
    /// `lan`, `bluetooth` oder `code`.
    pub via: String,
    /// Nur über Bluetooth gefunden und per Netz nicht erreichbar: Senden über den Offline-Weg
    /// (Hotspot). Name stammt dann aus Bluetooth und ist unbestätigt.
    #[serde(default)]
    pub offline: bool,
}

/// Eingehendes Angebot für die Oberfläche.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct IncomingOffer {
    pub id: u64,
    pub name: String,
    pub contact: Option<String>,
    pub code: String,
    pub files: Vec<OfferedFile>,
    pub total: u64,
    pub preview: bool,
    pub expires_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OfferedFile {
    pub name: String,
    pub size: u64,
    pub mime: String,
}

/// Gespeicherter Kontakt für die Oberfläche (ohne Geheimnis).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ContactInfo {
    pub key: String,
    pub name: String,
    pub fingerprint: String,
    pub added_at: i64,
    pub last_seen: i64,
}

/// Gesperrter Absender.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlockedSender {
    pub key: String,
    pub name: String,
    pub blocked_at: i64,
}

/// Kurzanfrage für den Offline-Weg (über Bluetooth empfangen, **unbestätigt**).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HotspotOffer {
    pub name: String,
    pub files: u32,
    pub total: u64,
    pub ssid: String,
    pub passphrase: String,
}

/// Ergebnis eines Sendevorgangs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SendResult {
    pub id: u64,
    pub ok: bool,
    pub error: Option<String>,
    /// Prüfcode dieser Verbindung (zum Vergleichen mit dem Empfänger).
    pub code: Option<String>,
}

#[derive(Clone)]
struct Target {
    endpoint: PublicKey,
    addrs: Vec<SocketAddr>,
}

impl Target {
    fn addr(&self) -> EndpointAddr {
        let mut a = EndpointAddr::from(self.endpoint);
        for s in &self.addrs {
            a.addrs.insert(TransportAddr::Ip(*s));
        }
        a
    }
}

struct NearbyEntry {
    target: Target,
    via: &'static str,
    /// Name aus Bluetooth (unbestätigt), falls das Gerät per Netz nicht erreichbar ist.
    ble_name: Option<String>,
    offline: bool,
    seen: Instant,
    card: Option<(String, DeviceKind, Option<String>)>,
    hidden: bool,
    probed: Option<Instant>,
    probing: bool,
}

struct Session {
    endpoint: Endpoint,
    router: Router,
    token: [u8; TOKEN_LEN],
    _advert: Option<Advert>,
    started: Instant,
}

struct Pending {
    view: IncomingOffer,
    sender_key: String,
    preview_png: Option<Vec<u8>>,
    respond: Option<oneshot::Sender<Answer>>,
}

#[derive(Debug, Clone, Copy)]
struct Answer {
    accept: bool,
    block: bool,
}

/// Über Bluetooth bereits angenommene Kurzanfrage: Das passende Angebot vom selben
/// Transport-Endpunkt wird ohne zweite Rückfrage angenommen.
struct PreAccept {
    endpoint: PublicKey,
    name: String,
    files: u32,
    total: u64,
    at: Instant,
}

const PRE_ACCEPT_TTL: Duration = Duration::from_secs(180);

struct Exchange {
    key: PublicKey,
    name: String,
    secret: [u8; 32],
    at: Instant,
}

#[derive(Default)]
struct Limiter {
    window: Option<Instant>,
    probes: u32,
    offers: u32,
}

impl Limiter {
    fn tick(&mut self) {
        if self.window.is_none_or(|w| w.elapsed() >= Duration::from_secs(60)) {
            *self = Limiter { window: Some(Instant::now()), ..Default::default() };
        }
    }
    fn probe(&mut self) -> bool {
        self.tick();
        self.probes += 1;
        self.probes <= PROBES_PER_MIN
    }
    fn offer(&mut self) -> bool {
        self.tick();
        self.offers += 1;
        self.offers <= OFFERS_PER_MIN
    }
}

struct VisState {
    mode: Mode,
    until: Option<i64>,
    /// Wird bei jeder Änderung erhöht (alte Ablauf-Aufgaben erkennen sich daran).
    generation: u64,
}

pub struct DropService {
    me: Weak<DropService>,
    opts: DropOptions,
    identity: DropIdentity,
    db: Arc<DatabaseManager>,
    vis: Mutex<VisState>,
    session: tokio::sync::Mutex<Option<Session>>,
    session_info: Mutex<Option<ConnectInfo>>,
    pending: Mutex<HashMap<u64, Pending>>,
    exchanges: Mutex<HashMap<u64, Exchange>>,
    limiter: Mutex<Limiter>,
    declined_strangers: AtomicU32,
    next_id: AtomicU64,
    active: Arc<AtomicUsize>,
    slots: Arc<Semaphore>,
    sender: tokio::sync::Mutex<Option<(Endpoint, Instant)>>,
    nearby: Mutex<HashMap<PublicKey, NearbyEntry>>,
    browser: Mutex<Option<Browser>>,
    pre_accept: Mutex<Option<PreAccept>>,
}

impl DropService {
    pub fn new(opts: DropOptions, db: Arc<DatabaseManager>) -> Result<Arc<Self>> {
        let identity = DropIdentity::load_or_create(&opts.identity_path)?;
        Ok(Self::with_identity(opts, db, identity))
    }

    pub fn with_identity(opts: DropOptions, db: Arc<DatabaseManager>, identity: DropIdentity) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            opts,
            identity,
            db,
            vis: Mutex::new(VisState { mode: Mode::Off, until: None, generation: 0 }),
            session: tokio::sync::Mutex::new(None),
            session_info: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            exchanges: Mutex::new(HashMap::new()),
            limiter: Mutex::new(Limiter::default()),
            declined_strangers: AtomicU32::new(0),
            next_id: AtomicU64::new(1),
            active: Arc::new(AtomicUsize::new(0)),
            slots: Arc::new(Semaphore::new(MAX_CONNS)),
            sender: tokio::sync::Mutex::new(None),
            nearby: Mutex::new(HashMap::new()),
            browser: Mutex::new(None),
            pre_accept: Mutex::new(None),
        })
    }

    pub fn drop_key(&self) -> PublicKey {
        self.identity.public()
    }

    /// Stellt die gespeicherte Sichtbarkeit nach dem Start wieder her („Kontakte“ bleibt an,
    /// „Alle“ nie – das gilt immer nur 10 Minuten).
    pub async fn restore(&self) -> Result<()> {
        let base = self.base_mode();
        if base == Mode::Contacts {
            self.set_visibility(Mode::Contacts).await?;
        }
        Ok(())
    }

    fn base_mode(&self) -> Mode {
        let v =
            self.db.settings().ok().and_then(|s| s.into_iter().find(|(k, _)| k == SETTING_BASE_MODE)).map(|(_, v)| v);
        if v == Some(1) {
            Mode::Contacts
        } else {
            Mode::Off
        }
    }

    fn now() -> i64 {
        jiff::Timestamp::now().as_second()
    }

    fn mode(&self) -> Mode {
        self.vis.lock().unwrap_or_else(|e| e.into_inner()).mode
    }

    // --- Sichtbarkeit ---

    /// Ändert die Sichtbarkeit. „Alle“ gilt 10 Minuten und fällt dann auf die gespeicherte
    /// Grundeinstellung zurück („Aus“ oder „Kontakte“).
    pub async fn set_visibility(&self, mode: Mode) -> Result<DropStatus> {
        if mode != Mode::Everyone {
            self.db.put_setting(SETTING_BASE_MODE, Some(i64::from(mode == Mode::Contacts)))?;
        }
        let generation = self.apply(mode).await?;
        if mode == Mode::Everyone {
            let me = self.me.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(EVERYONE_SECS as u64)).await;
                if let Some(svc) = me.upgrade() {
                    svc.everyone_expired(generation).await;
                }
            });
        }
        self.status()
    }

    /// Setzt den Modus und startet/stoppt die Sitzung (ohne Ablauf-Aufgabe).
    async fn apply(&self, mode: Mode) -> Result<u64> {
        let (until, generation) = {
            let mut v = self.vis.lock().unwrap_or_else(|e| e.into_inner());
            v.mode = mode;
            v.until = (mode == Mode::Everyone).then(|| Self::now() + EVERYONE_SECS);
            v.generation += 1;
            (v.until, v.generation)
        };
        self.declined_strangers.store(0, Ordering::Relaxed);
        if mode == Mode::Off {
            self.stop_session().await;
        } else {
            self.ensure_session().await?;
        }
        self.db.events().emit(Event::DropVisibilityChanged { mode: mode.as_str().into(), until });
        Ok(generation)
    }

    async fn everyone_expired(&self, generation: u64) {
        let still = {
            let v = self.vis.lock().unwrap_or_else(|e| e.into_inner());
            v.generation == generation && v.mode == Mode::Everyone
        };
        if still {
            let base = self.base_mode();
            if let Err(e) = self.apply(base).await {
                tracing::warn!("Drop: Zurückschalten der Sichtbarkeit fehlgeschlagen: {e:#}");
            }
        }
    }

    pub fn status(&self) -> Result<DropStatus> {
        let (mode, until) = {
            let v = self.vis.lock().unwrap_or_else(|e| e.into_inner());
            (v.mode, v.until)
        };
        let code = self.session_info.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(ConnectInfo::to_code);
        Ok(DropStatus {
            mode,
            until,
            name: super::wire::display_name(&self.opts.name),
            fingerprint: identity::fingerprint(&self.identity.public()),
            dir: self.opts.dir.display().to_string(),
            code: if mode == Mode::Off { None } else { code },
        })
    }

    /// Verbindungsdaten der laufenden Sitzung (für Bluetooth-GATT und QR), mit den aktuellen
    /// Adressen (z. B. nach dem Beitritt zu einem Hotspot).
    pub async fn connect_info(&self) -> Option<ConnectInfo> {
        if self.mode() == Mode::Off {
            return None;
        }
        let addrs = {
            let guard = self.session.lock().await;
            wait_for_addrs(&guard.as_ref()?.endpoint).await
        };
        let everyone = self.mode() == Mode::Everyone;
        let mut lock = self.session_info.lock().unwrap_or_else(|e| e.into_inner());
        let info = lock.as_mut()?;
        if !addrs.is_empty() {
            info.addrs = addrs;
        }
        let mut out = info.clone();
        out.name = everyone.then(|| super::wire::display_name(&self.opts.name));
        Some(out)
    }

    /// Startet die Sitzung neu (neuer Endpunkt, neue Adressen) – z. B. nach dem Beitritt zu einem
    /// Hotspot, damit neue Sockets das neue Netz nutzen. Die Vorab-Annahme bleibt erhalten.
    pub async fn refresh_session(&self) -> Result<Option<ConnectInfo>> {
        if self.mode() == Mode::Off {
            return Ok(None);
        }
        if let Some(old) = self.session.lock().await.take() {
            let _ = old.router.shutdown().await;
        }
        self.ensure_session().await?;
        Ok(self.connect_info().await)
    }

    fn endpoint_builder(&self) -> Result<iroh::endpoint::Builder> {
        let mut b = if self.opts.use_relay {
            let b = Endpoint::builder(presets::N0);
            match crate::network::relay_mode(&self.opts.relay_urls, self.opts.relay_fallback_n0)? {
                Some(mode) => b.relay_mode(mode),
                None => b,
            }
        } else {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        };
        b = b.secret_key(SecretKey::generate());
        if self.opts.loopback_only {
            b = b.bind_addr("127.0.0.1:0")?;
        }
        Ok(b)
    }

    async fn ensure_session(&self) -> Result<()> {
        let mut guard = self.session.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let endpoint = self
            .endpoint_builder()?
            .alpns(vec![DROP_ALPN.to_vec()])
            .bind()
            .await
            .context("Drop-Endpunkt konnte nicht gestartet werden")?;
        let router = Router::builder(endpoint.clone()).accept(DROP_ALPN, DropReceiver { svc: self.me.clone() }).spawn();
        let mut token = [0u8; TOKEN_LEN];
        getrandom::fill(&mut token).map_err(|e| anyhow::anyhow!("Zufall nicht verfügbar: {e}"))?;
        let addrs = wait_for_addrs(&endpoint).await;
        let advert = if self.opts.mdns {
            match addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()) {
                Some(a) => Advert::start(&token, &endpoint.id(), a.port())
                    .map_err(|e| tracing::warn!("Drop: mDNS-Bekanntmachung nicht möglich: {e:#}"))
                    .ok(),
                None => None,
            }
        } else {
            None
        };
        let info = ConnectInfo {
            endpoint: endpoint.id(),
            token,
            can_join_hotspot: self.opts.can_join_hotspot,
            relay: self.opts.use_relay,
            addrs,
            name: None,
        };
        *self.session_info.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
        *guard = Some(Session { endpoint, router, token, _advert: advert, started: Instant::now() });
        drop(guard);
        self.spawn_rotation();
        Ok(())
    }

    /// Erneuert Endpunkt und Token alle 15 Minuten, sobald gerade nichts übertragen wird.
    fn spawn_rotation(&self) {
        let me = self.me.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let Some(svc) = me.upgrade() else { return };
                let mut guard = svc.session.lock().await;
                let Some(s) = guard.as_ref() else { return };
                if s.started.elapsed() < SESSION_ROTATE || svc.active.load(Ordering::Relaxed) > 0 {
                    continue;
                }
                if let Some(old) = guard.take() {
                    let _ = old.router.shutdown().await;
                }
                drop(guard);
                if svc.mode() != Mode::Off {
                    if let Err(e) = svc.ensure_session().await {
                        tracing::warn!("Drop: Sitzung nicht erneuerbar: {e:#}");
                    }
                }
                return; // ensure_session startet eine neue Rotations-Aufgabe
            }
        });
    }

    async fn stop_session(&self) {
        *self.session_info.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let old = self.session.lock().await.take();
        if let Some(s) = old {
            let _ = s.router.shutdown().await;
            drop(s.endpoint);
            let _ = s.token;
        }
        // Offene Angebote verfallen.
        let ids: Vec<u64> = self.pending.lock().unwrap_or_else(|e| e.into_inner()).keys().copied().collect();
        for id in ids {
            self.close_pending(id, "cancelled");
        }
    }

    pub async fn shutdown(&self) {
        self.browse_stop();
        self.stop_session().await;
        if let Some((ep, _)) = self.sender.lock().await.take() {
            ep.close().await;
        }
    }

    // --- Empfangen ---

    async fn handle_incoming(&self, conn: &Connection) -> Result<()> {
        let Ok(_permit) = self.slots.clone().try_acquire_owned() else {
            conn.close(VarInt::from_u32(1), b"busy");
            return Ok(());
        };
        let mode = self.mode();
        if mode == Mode::Off || !self.limiter.lock().unwrap_or_else(|e| e.into_inner()).probe() {
            conn.close(VarInt::from_u32(1), b"busy");
            return Ok(());
        }
        let _active = ActiveGuard::new(self.active.clone());
        // Höchstens zwei gleichzeitige Datei-Ströme (begrenzt den Speicher bei Missbrauch).
        conn.set_max_concurrent_uni_streams(VarInt::from_u32(2));
        let session = identity::session_value(conn)?;
        let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await??;
        let probe: Probe = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
        let tags: HashSet<[u8; 8]> = probe.tag_bytes()?.into_iter().collect();
        let contact = self.match_contact(&tags, &session)?;
        if mode == Mode::Contacts && contact.is_none() {
            write_frame_max(&mut send, &Card::Hidden, MAX_FRAME).await?;
            let _ = send.finish();
            let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
            return Ok(());
        }
        let card = Card::Visible {
            name: super::wire::display_name(&self.opts.name),
            kind: self.opts.kind,
            proof: contact.as_ref().map(|_| Proof::new(&self.identity.public(), &self.identity.sign_session(&session))),
        };
        write_frame_max(&mut send, &card, MAX_FRAME).await?;

        // Angebot ist optional (Probe zum Auflisten endet hier).
        let offer: Offer = match tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await {
            Ok(Ok(o)) => o,
            _ => return Ok(()),
        };
        if !self.limiter.lock().unwrap_or_else(|e| e.into_inner()).offer() {
            return reply(&mut send, false, "busy").await;
        }
        let offer = match offer.validate(&session) {
            Ok(o) => o,
            Err(_) => return reply(&mut send, false, "invalid").await,
        };
        let sender_key = hex(offer.sender.as_bytes());
        if self.db.is_drop_blocked(&sender_key)? {
            // Gesperrte erfahren nicht, dass sie gesperrt sind.
            tokio::time::sleep(Duration::from_secs(2)).await;
            return reply(&mut send, false, "declined").await;
        }
        let free = crate::fileops::archive::free_space(&self.ensure_dir()?).unwrap_or(0);
        if offer.total.saturating_add(self.opts.reserve_bytes) > free {
            return reply(&mut send, false, "space").await;
        }
        let contact_name = self.contact_by_key(&sender_key)?.map(|c| c.name);
        let pre_accepted = self.take_pre_accept(conn.remote_id(), &offer);

        let (tx, rx) = oneshot::channel();
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let full = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let full = pending.len() >= MAX_PENDING || pending.values().any(|p| p.sender_key == sender_key);
            if !full {
                pending.insert(
                    id,
                    Pending {
                        view: incoming_view(id, &offer, contact_name.clone(), identity::check_code(&session)),
                        sender_key: sender_key.clone(),
                        preview_png: offer.preview_png.clone(),
                        respond: Some(tx),
                    },
                );
            }
            full
        };
        if full {
            return reply(&mut send, false, "busy").await;
        }
        if pre_accepted {
            let _ = self.respond(id, true, false);
        }
        if !pre_accepted {
            self.db.events().emit(Event::DropOffer {
                id,
                name: offer.name.clone(),
                contact: contact_name.clone(),
                code: identity::check_code(&session),
                files: offer.files.len() as u32,
                total: offer.total,
                kinds: kinds_of(&offer),
                preview: offer.preview_png.is_some(),
            });
        }

        let answer = tokio::select! {
            a = rx => a.ok(),
            _ = tokio::time::sleep(OFFER_TIMEOUT) => None,
            _ = conn.closed() => None,
        };
        let Some(answer) = answer else {
            let reason = if conn.close_reason().is_some() { "cancelled" } else { "timeout" };
            self.close_pending(id, reason);
            let _ = reply(&mut send, false, "timeout").await;
            return Ok(());
        };
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        if !answer.accept {
            if answer.block {
                self.db.block_drop(&sender_key, &offer.name, Self::now())?;
            }
            if contact_name.is_none() && self.note_stranger_declined() {
                let base = self.base_mode();
                let me = self.me.clone();
                tokio::spawn(async move {
                    if let Some(svc) = me.upgrade() {
                        let _ = svc.apply(base).await;
                    }
                });
            }
            return reply(&mut send, false, "declined").await;
        }
        reply(&mut send, true, "").await?;

        let result = self.receive_files(conn, id, &offer).await;
        let peer = contact_name.clone().unwrap_or_else(|| offer.name.clone());
        match result {
            Ok(saved) => {
                let fin = Received {
                    ok: true,
                    proof: Some(Proof::new(&self.identity.public(), &self.identity.sign_session(&session))),
                };
                let _ = write_frame_max(&mut send, &fin, MAX_FRAME).await;
                let _ = send.finish();
                let can_save = self.remember_exchange(id, offer.sender, &offer.name, &session)?;
                self.db.events().emit(Event::DropFinished {
                    id,
                    incoming: true,
                    ok: true,
                    peer,
                    files: saved.iter().map(|p| p.display().to_string()).collect(),
                    error: None,
                    can_save_contact: can_save,
                });
                let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
            }
            Err(e) => {
                let _ = write_frame_max(&mut send, &Received { ok: false, proof: None }, MAX_FRAME).await;
                self.db.events().emit(Event::DropFinished {
                    id,
                    incoming: true,
                    ok: false,
                    peer,
                    files: vec![],
                    error: Some(format!("{e:#}")),
                    can_save_contact: false,
                });
            }
        }
        Ok(())
    }

    /// Zählt abgelehnte Angebote Fremder; `true`, wenn „Alle“ deshalb enden soll.
    fn note_stranger_declined(&self) -> bool {
        self.mode() == Mode::Everyone
            && self.declined_strangers.fetch_add(1, Ordering::Relaxed) + 1 >= DECLINES_UNTIL_OFF
    }

    fn ensure_dir(&self) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.opts.dir)
            .with_context(|| format!("Ablage {} nicht anlegbar", self.opts.dir.display()))?;
        Ok(self.opts.dir.clone())
    }

    async fn receive_files(&self, conn: &Connection, id: u64, offer: &ValidOffer) -> Result<Vec<PathBuf>> {
        let dir = self.ensure_dir()?;
        let mut seen = vec![false; offer.files.len()];
        let mut saved = Vec::with_capacity(offer.files.len());
        let mut progress = Progress::new(self.db.clone(), id, true, offer.total);
        for _ in 0..offer.files.len() {
            let r: Result<PathBuf> = async {
                let mut uni = tokio::time::timeout(IDLE_TIMEOUT, conn.accept_uni()).await??;
                let idx = tokio::time::timeout(IO_TIMEOUT, uni.read_u32()).await?? as usize;
                if idx >= offer.files.len() || seen[idx] {
                    api_bail!(InvalidInput, "Gegenüber sendet eine unerwartete Datei");
                }
                seen[idx] = true;
                receive_one(&mut uni, &dir, &offer.files[idx], &mut progress).await
            }
            .await;
            match r {
                Ok(p) => saved.push(p),
                Err(e) => {
                    // Nichts Halbes zurücklassen.
                    for p in &saved {
                        let _ = std::fs::remove_file(p);
                    }
                    return Err(e);
                }
            }
        }
        Ok(saved)
    }

    fn match_contact(&self, tags: &HashSet<[u8; 8]>, session: &[u8; 32]) -> Result<Option<DropContactRow>> {
        if tags.is_empty() {
            return Ok(None);
        }
        for c in self.db.drop_contacts()? {
            let Ok(secret) = <[u8; 32]>::try_from(c.secret.as_slice()) else { continue };
            if tags.contains(&identity::contact_tag(&secret, session)) {
                return Ok(Some(c));
            }
        }
        Ok(None)
    }

    fn contact_by_key(&self, key: &str) -> Result<Option<DropContactRow>> {
        Ok(self.db.drop_contacts()?.into_iter().find(|c| c.key == key))
    }

    /// Merkt sich einen erfolgreichen Tausch zum späteren Speichern als Kontakt. Ist das Gegenüber
    /// schon Kontakt, wird sein Geheimnis gleich auf den neuesten Stand gebracht.
    fn remember_exchange(&self, id: u64, key: PublicKey, name: &str, session: &[u8; 32]) -> Result<bool> {
        let secret = identity::contact_secret(session);
        let key_hex = hex(key.as_bytes());
        if let Some(c) = self.contact_by_key(&key_hex)? {
            self.db.upsert_drop_contact(&DropContactRow {
                secret: secret.to_vec(),
                last_seen: Self::now(),
                name: super::wire::display_name(name),
                ..c
            })?;
            return Ok(false);
        }
        let mut ex = self.exchanges.lock().unwrap_or_else(|e| e.into_inner());
        ex.retain(|_, e| e.at.elapsed() < EXCHANGE_TTL);
        ex.insert(id, Exchange { key, name: super::wire::display_name(name), secret, at: Instant::now() });
        Ok(true)
    }

    fn close_pending(&self, id: u64, reason: &str) {
        if self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
            self.db.events().emit(Event::DropOfferClosed { id, reason: reason.into() });
        }
    }

    /// Offene Angebote (z. B. nach dem Öffnen der App).
    pub fn pending_offers(&self) -> Vec<IncomingOffer> {
        let mut v: Vec<IncomingOffer> =
            self.pending.lock().unwrap_or_else(|e| e.into_inner()).values().map(|p| p.view.clone()).collect();
        v.sort_by_key(|o| o.id);
        v
    }

    /// Vorschaubild eines Angebots (PNG, im Kern neu codiert).
    pub fn offer_preview(&self, id: u64) -> Option<Vec<u8>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).get(&id).and_then(|p| p.preview_png.clone())
    }

    /// Antwort auf ein Angebot. `block` sperrt den Absender zusätzlich (nur mit Ablehnen).
    pub fn respond(&self, id: u64, accept: bool, block: bool) -> Result<()> {
        let tx = self.pending.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id).and_then(|p| p.respond.take());
        match tx {
            Some(tx) => {
                let _ = tx.send(Answer { accept, block: block && !accept });
                Ok(())
            }
            None => api_bail!(NotFound, "Kein offenes Angebot {id}"),
        }
    }

    // --- Kontakte ---

    pub fn contacts(&self) -> Result<Vec<ContactInfo>> {
        Ok(self
            .db
            .drop_contacts()?
            .into_iter()
            .map(|c| ContactInfo {
                fingerprint: super::wire::hex_array::<32>(&c.key)
                    .ok()
                    .and_then(|k| PublicKey::from_bytes(&k).ok())
                    .map(|k| identity::fingerprint(&k))
                    .unwrap_or_default(),
                key: c.key,
                name: c.name,
                added_at: c.added_at,
                last_seen: c.last_seen,
            })
            .collect())
    }

    /// Speichert das Gegenüber eines eben beendeten Tauschs als Kontakt.
    pub fn save_contact(&self, exchange_id: u64) -> Result<ContactInfo> {
        let ex = self.exchanges.lock().unwrap_or_else(|e| e.into_inner()).remove(&exchange_id);
        let Some(ex) = ex.filter(|e| e.at.elapsed() < EXCHANGE_TTL) else {
            api_bail!(NotFound, "Dieser Tausch kann nicht mehr als Kontakt gespeichert werden");
        };
        if self.db.drop_contacts()?.len() >= MAX_TAGS {
            api_bail!(Rejected, "Höchstens {MAX_TAGS} Kontakte");
        }
        let now = Self::now();
        let row = DropContactRow {
            key: hex(ex.key.as_bytes()),
            name: ex.name,
            secret: ex.secret.to_vec(),
            added_at: now,
            last_seen: now,
        };
        self.db.upsert_drop_contact(&row)?;
        let _ = self.db.unblock_drop(&row.key);
        self.invalidate_cards();
        Ok(ContactInfo {
            fingerprint: identity::fingerprint(&ex.key),
            key: row.key,
            name: row.name,
            added_at: now,
            last_seen: now,
        })
    }

    pub fn remove_contact(&self, key: &str) -> Result<bool> {
        self.invalidate_cards();
        self.db.remove_drop_contact(key)
    }

    pub fn blocked(&self) -> Result<Vec<BlockedSender>> {
        Ok(self
            .db
            .drop_blocked()?
            .into_iter()
            .map(|(key, name, blocked_at)| BlockedSender { key, name, blocked_at })
            .collect())
    }

    pub fn unblock(&self, key: &str) -> Result<bool> {
        self.db.unblock_drop(key)
    }

    // --- Offline-Weg (Hotspot) ---

    /// Prüft eine Kurzanfrage aus Bluetooth (für die Rückfrage beim Menschen).
    pub fn parse_hotspot(&self, data: &[u8]) -> Result<HotspotOffer> {
        let r = super::wire::HotspotRequest::decode(data)?;
        Ok(HotspotOffer { name: r.name, files: r.files, total: r.total, ssid: r.ssid, passphrase: r.passphrase })
    }

    /// Der Mensch hat die Kurzanfrage angenommen: Das passende Angebot (gleicher Endpunkt, Name,
    /// Anzahl und Größe) wird ohne zweite Rückfrage angenommen. Gilt 3 Minuten, nur einmal.
    pub fn pre_accept(&self, data: &[u8]) -> Result<()> {
        let r = super::wire::HotspotRequest::decode(data)?;
        *self.pre_accept.lock().unwrap_or_else(|e| e.into_inner()) = Some(PreAccept {
            endpoint: r.sender.endpoint,
            name: r.name,
            files: r.files,
            total: r.total,
            at: Instant::now(),
        });
        Ok(())
    }

    fn take_pre_accept(&self, remote: PublicKey, offer: &ValidOffer) -> bool {
        let mut lock = self.pre_accept.lock().unwrap_or_else(|e| e.into_inner());
        let ok = lock.as_ref().is_some_and(|p| {
            p.at.elapsed() < PRE_ACCEPT_TTL
                && p.endpoint == remote
                && p.name == offer.name
                && p.files as usize == offer.files.len()
                && p.total == offer.total
        });
        if ok {
            *lock = None;
        }
        ok
    }

    /// Baut die Kurzanfrage für den Offline-Weg (Sender, nachdem er einen Hotspot geöffnet hat).
    pub async fn hotspot_request(&self, ssid: &str, passphrase: &str, paths: &[PathBuf]) -> Result<Vec<u8>> {
        let mut total = 0u64;
        for p in paths {
            total = total.saturating_add(tokio::fs::metadata(p).await?.len());
        }
        let ep = self.sender_endpoint().await?;
        let sender = ConnectInfo {
            endpoint: ep.id(),
            token: [0; TOKEN_LEN],
            can_join_hotspot: false,
            relay: false,
            addrs: wait_for_addrs(&ep).await,
            name: None,
        };
        let req = super::wire::HotspotRequest {
            name: super::wire::display_name(&self.opts.name),
            files: paths.len() as u32,
            total,
            ssid: ssid.into(),
            passphrase: passphrase.into(),
            sender,
        };
        let bytes = req.encode();
        // Selbstprüfung: nur senden, was der Empfänger auch annimmt.
        super::wire::HotspotRequest::decode(&bytes)?;
        Ok(bytes)
    }

    // --- Senden ---

    async fn sender_endpoint(&self) -> Result<Endpoint> {
        let mut guard = self.sender.lock().await;
        if let Some((ep, at)) = guard.as_ref() {
            if at.elapsed() < SENDER_TTL {
                return Ok(ep.clone());
            }
        }
        if let Some((old, _)) = guard.take() {
            old.close().await;
        }
        let ep = self.endpoint_builder()?.bind().await.context("Drop-Endpunkt konnte nicht gestartet werden")?;
        *guard = Some((ep.clone(), Instant::now()));
        Ok(ep)
    }

    fn own_endpoint(&self) -> Option<PublicKey> {
        self.session_info.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|i| i.endpoint)
    }

    async fn connect(&self, target: &Target) -> Result<Connection> {
        if Some(target.endpoint) == self.own_endpoint() {
            api_bail!(InvalidInput, "Das ist dieses Gerät");
        }
        let ep = self.sender_endpoint().await?;
        let conn = tokio::time::timeout(CONNECT_TIMEOUT, ep.connect(target.addr(), DROP_ALPN))
            .await
            .map_err(|_| ApiError::new(ErrorCode::Unavailable, "Gerät ist nicht erreichbar"))?
            .map_err(|_| ApiError::new(ErrorCode::Unavailable, "Gerät ist nicht erreichbar"))?;
        Ok(conn)
    }

    fn probe_for(&self, session: &[u8; 32]) -> Result<Probe> {
        let tags = self
            .db
            .drop_contacts()?
            .iter()
            .filter_map(|c| <[u8; 32]>::try_from(c.secret.as_slice()).ok())
            .take(MAX_TAGS)
            .map(|s| hex(&identity::contact_tag(&s, session)))
            .collect();
        Ok(Probe { v: VERSION, tags })
    }

    /// Fragt die Karte eines Geräts ab: `Some((Name, Typ, Kontaktname))` oder `None` (verborgen).
    async fn probe(&self, target: &Target) -> Result<Option<(String, DeviceKind, Option<String>)>> {
        let conn = self.connect(target).await?;
        let session = identity::session_value(&conn)?;
        let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.open_bi()).await??;
        write_frame_max(&mut send, &self.probe_for(&session)?, MAX_FRAME).await?;
        let card: Card = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
        let _ = send.finish();
        conn.close(VarInt::from_u32(0), b"probe");
        self.card_view(card, &session)
    }

    fn card_view(&self, card: Card, session: &[u8; 32]) -> Result<Option<(String, DeviceKind, Option<String>)>> {
        Ok(match card {
            Card::Hidden => None,
            Card::Visible { name, kind, proof } => {
                let contact = match proof {
                    Some(p) => {
                        let key = p.verify(session)?;
                        self.contact_by_key(&hex(key.as_bytes()))?.map(|c| c.name)
                    }
                    None => None,
                };
                Some((super::wire::display_name(&name), kind, contact))
            }
        })
    }

    /// Sendet Dateien an ein Gerät aus der Nähe-Liste (oder per Code). Wartet bis zum Ende.
    pub async fn send(&self, peer: &str, paths: Vec<PathBuf>) -> Result<SendResult> {
        let target = self.target_of(peer)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let peer_name = self.peer_label(&target);
        let r = self.send_to(id, &target, paths).await;
        let (ok, error, code, can_save) = match r {
            Ok((code, can_save)) => (true, None, Some(code), can_save),
            Err(e) => (false, Some(format!("{e:#}")), None, false),
        };
        self.db.events().emit(Event::DropFinished {
            id,
            incoming: false,
            ok,
            peer: peer_name,
            files: vec![],
            error: error.clone(),
            can_save_contact: can_save,
        });
        Ok(SendResult { id, ok, error, code })
    }

    fn peer_label(&self, target: &Target) -> String {
        let nearby = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
        nearby
            .get(&target.endpoint)
            .and_then(|e| e.card.as_ref())
            .map(|(n, _, c)| c.clone().unwrap_or_else(|| n.clone()))
            .unwrap_or_else(|| "Gerät".into())
    }

    fn target_of(&self, peer: &str) -> Result<Target> {
        if peer.starts_with("snapdrop1-") {
            let info = ConnectInfo::from_code(peer)?;
            return Ok(Target { endpoint: info.endpoint, addrs: info.addrs });
        }
        let key = super::wire::hex_array::<32>(peer)
            .ok()
            .and_then(|k| PublicKey::from_bytes(&k).ok())
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Unbekanntes Gerät"))?;
        let nearby = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
        nearby
            .get(&key)
            .map(|e| e.target.clone())
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "Gerät ist nicht mehr in der Nähe").into())
    }

    async fn send_to(&self, id: u64, target: &Target, paths: Vec<PathBuf>) -> Result<(String, bool)> {
        let files = prepare_files(paths).await?;
        let conn = self.connect(target).await?;
        let session = identity::session_value(&conn)?;
        let code = identity::check_code(&session);
        let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.open_bi()).await??;
        write_frame_max(&mut send, &self.probe_for(&session)?, MAX_FRAME).await?;
        let card: Card = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
        let Some((name, _, contact)) = self.card_view(card, &session)? else {
            api_bail!(PermissionDenied, "Das Gerät empfängt gerade nur von Kontakten");
        };
        let offer = Offer {
            name: super::wire::display_name(&self.opts.name),
            kind: self.opts.kind,
            proof: Proof::new(&self.identity.public(), &self.identity.sign_session(&session)),
            files: files.iter().map(|f| f.offer.clone()).collect(),
            preview: files.iter().find_map(|f| f.preview.clone()),
        };
        write_frame_max(&mut send, &offer, MAX_FRAME).await?;
        let reply: Reply =
            tokio::time::timeout(OFFER_TIMEOUT + IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
        if !reply.accepted {
            let why = match reply.reason.as_deref() {
                Some("space") => "Beim Empfänger ist nicht genug Platz",
                Some("busy") => "Der Empfänger ist gerade beschäftigt",
                Some("timeout") => "Keine Antwort",
                Some("invalid") => "Der Empfänger hat das Angebot nicht verstanden",
                _ => "Abgelehnt",
            };
            api_bail!(PermissionDenied, "{why}");
        }
        let total: u64 = files.iter().map(|f| f.offer.size).sum();
        let mut progress = Progress::new(self.db.clone(), id, false, total);
        for (i, f) in files.iter().enumerate() {
            let mut uni = tokio::time::timeout(IO_TIMEOUT, conn.open_uni()).await??;
            uni.write_u32(i as u32).await?;
            send_one(&mut uni, &f.path, f.offer.size, &mut progress).await?;
        }
        let fin: Received = tokio::time::timeout(IDLE_TIMEOUT * 4, read_frame_max(&mut recv, MAX_FRAME)).await??;
        conn.close(VarInt::from_u32(0), b"done");
        if !fin.ok {
            api_bail!(Internal, "Der Empfänger konnte die Dateien nicht speichern");
        }
        // Empfänger zeigt seinen Schlüssel erst nach dem Annehmen.
        let can_save = match fin.proof.map(|p| p.verify(&session)).transpose()? {
            Some(key) => self.remember_exchange(id, key, contact.as_deref().unwrap_or(&name), &session)?,
            None => false,
        };
        Ok((code, can_save))
    }

    // --- Geräte in der Nähe ---

    /// Startet die Suche (mDNS) und hält die Liste aktuell, bis `browse_stop`.
    pub fn browse_start(&self) -> Result<()> {
        self.invalidate_cards();
        if !self.opts.mdns {
            return Ok(());
        }
        let mut b = self.browser.lock().unwrap_or_else(|e| e.into_inner());
        if b.is_some() {
            return Ok(());
        }
        let me = self.me.clone();
        *b = Some(Browser::start(move |found: Found| {
            if let Some(svc) = me.upgrade() {
                svc.add_nearby(Target { endpoint: found.endpoint, addrs: found.addrs }, "lan");
            }
        })?);
        Ok(())
    }

    /// Erzwingt neue Abfragen (z. B. nach Änderungen an den Kontakten).
    fn invalidate_cards(&self) {
        let again: Vec<(Target, &'static str)> = {
            let mut n = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
            n.values_mut()
                .map(|e| {
                    e.probed = None;
                    (e.target.clone(), e.via)
                })
                .collect()
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            for (t, via) in again {
                self.add_nearby(t, via);
            }
        }
    }

    pub fn browse_stop(&self) {
        self.browser.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// Verbindungsdaten von außen (Bluetooth-GATT oder QR-Code) übernehmen.
    pub fn add_peer(&self, info: &ConnectInfo, via: &'static str) {
        if let Some(name) = &info.name {
            let mut n = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(e) = n.get_mut(&info.endpoint) {
                e.ble_name = Some(name.clone());
            } else if n.len() < MAX_NEARBY {
                n.insert(
                    info.endpoint,
                    NearbyEntry {
                        target: Target { endpoint: info.endpoint, addrs: info.addrs.clone() },
                        via,
                        ble_name: Some(name.clone()),
                        offline: false,
                        seen: Instant::now(),
                        card: None,
                        hidden: false,
                        probed: None,
                        probing: false,
                    },
                );
            }
        }
        self.add_nearby(Target { endpoint: info.endpoint, addrs: info.addrs.clone() }, via);
    }

    fn add_nearby(&self, target: Target, via: &'static str) {
        if Some(target.endpoint) == self.own_endpoint() {
            return;
        }
        let key = target.endpoint;
        {
            let mut n = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
            n.retain(|_, e| e.seen.elapsed() < NEARBY_TTL);
            if !n.contains_key(&key) && n.len() >= MAX_NEARBY {
                return;
            }
            let e = n.entry(key).or_insert(NearbyEntry {
                target: target.clone(),
                via,
                ble_name: None,
                offline: false,
                seen: Instant::now(),
                card: None,
                hidden: false,
                probed: None,
                probing: false,
            });
            e.seen = Instant::now();
            for a in target.addrs {
                if !e.target.addrs.contains(&a) && e.target.addrs.len() < 8 {
                    e.target.addrs.push(a);
                }
            }
            if e.probing || e.probed.is_some_and(|p| p.elapsed() < REPROBE) {
                return;
            }
            e.probing = true;
        }
        let me = self.me.clone();
        tokio::spawn(async move {
            let Some(svc) = me.upgrade() else { return };
            let target = svc.nearby.lock().unwrap_or_else(|e| e.into_inner()).get(&key).map(|e| e.target.clone());
            let Some(target) = target else { return };
            let card = svc.probe(&target).await;
            let mut n = svc.nearby.lock().unwrap_or_else(|e| e.into_inner());
            match card {
                Ok(c) => {
                    if let Some(e) = n.get_mut(&key) {
                        e.offline = false;
                        e.probing = false;
                        e.probed = Some(Instant::now());
                        e.hidden = c.is_none();
                        e.card = c;
                    }
                }
                Err(_) => {
                    // Über Bluetooth gefundene Geräte bleiben als „offline erreichbar“ stehen.
                    match n.get_mut(&key) {
                        Some(e) if e.ble_name.is_some() => {
                            e.probing = false;
                            e.probed = Some(Instant::now());
                            e.offline = true;
                        }
                        _ => {
                            n.remove(&key);
                        }
                    }
                }
            }
            drop(n);
            svc.db.events().emit(Event::DropNearbyChanged);
        });
    }

    /// Sichtbare Geräte in der Nähe (verborgene und noch nicht abgefragte fehlen).
    pub fn nearby(&self) -> Vec<NearbyPeer> {
        let n = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<NearbyPeer> = n
            .iter()
            .filter(|(_, e)| !e.hidden && e.seen.elapsed() < NEARBY_TTL)
            .filter_map(|(k, e)| {
                if e.offline {
                    return Some(NearbyPeer {
                        id: hex(k.as_bytes()),
                        name: e.ble_name.clone(),
                        kind: None,
                        contact: None,
                        via: e.via.into(),
                        offline: true,
                    });
                }
                let (name, kind, contact) = e.card.clone()?;
                Some(NearbyPeer {
                    id: hex(k.as_bytes()),
                    name: Some(name),
                    kind: Some(kind),
                    contact,
                    via: e.via.into(),
                    offline: false,
                })
            })
            .collect();
        v.sort_by(|a, b| (a.contact.is_none(), &a.name).cmp(&(b.contact.is_none(), &b.name)));
        v
    }

    /// Fügt ein Gerät per Code hinzu und fragt es sofort ab.
    pub async fn add_code(&self, code: &str) -> Result<NearbyPeer> {
        let info = ConnectInfo::from_code(code)?;
        let target = Target { endpoint: info.endpoint, addrs: info.addrs.clone() };
        let card = self.probe(&target).await?;
        let Some((name, kind, contact)) = card.clone() else {
            api_bail!(PermissionDenied, "Das Gerät empfängt gerade nur von Kontakten");
        };
        self.nearby.lock().unwrap_or_else(|e| e.into_inner()).insert(
            info.endpoint,
            NearbyEntry {
                target,
                via: "code",
                ble_name: None,
                offline: false,
                seen: Instant::now(),
                card,
                hidden: false,
                probed: Some(Instant::now()),
                probing: false,
            },
        );
        Ok(NearbyPeer {
            id: hex(info.endpoint.as_bytes()),
            name: Some(name),
            kind: Some(kind),
            contact,
            via: "code".into(),
            offline: false,
        })
    }
}

#[derive(Clone, Debug)]
struct DropReceiver {
    svc: Weak<DropService>,
}

impl ProtocolHandler for DropReceiver {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if let Some(svc) = self.svc.upgrade() {
            if let Err(e) = svc.handle_incoming(&conn).await {
                tracing::debug!("Drop: eingehende Verbindung beendet: {e:#}");
            }
            // Gegenseite die letzte Antwort lesen lassen, dann schließen.
            let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
        }
        Ok(())
    }
}

struct ActiveGuard(Arc<AtomicUsize>);
impl ActiveGuard {
    fn new(a: Arc<AtomicUsize>) -> Self {
        a.fetch_add(1, Ordering::Relaxed);
        Self(a)
    }
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn reply(send: &mut SendStream, accepted: bool, reason: &str) -> Result<()> {
    let r = Reply { accepted, reason: (!accepted).then(|| reason.to_string()) };
    write_frame_max(send, &r, MAX_FRAME).await?;
    if !accepted {
        let _ = send.finish();
    }
    Ok(())
}

/// Wartet kurz, bis der Endpunkt seine lokalen Adressen kennt.
async fn wait_for_addrs(ep: &Endpoint) -> Vec<SocketAddr> {
    for _ in 0..20 {
        let addrs: Vec<SocketAddr> = ep
            .addr()
            .addrs
            .iter()
            .filter_map(|a| match a {
                TransportAddr::Ip(sa) => Some(*sa),
                _ => None,
            })
            .collect();
        if !addrs.is_empty() {
            return addrs;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Vec::new()
}

fn incoming_view(id: u64, o: &ValidOffer, contact: Option<String>, code: String) -> IncomingOffer {
    IncomingOffer {
        id,
        name: o.name.clone(),
        contact,
        code,
        files: o
            .files
            .iter()
            .map(|f| OfferedFile { name: f.name.clone(), size: f.size, mime: f.mime.clone() })
            .collect(),
        total: o.total,
        preview: o.preview_png.is_some(),
        expires_at: DropService::now() + OFFER_TIMEOUT.as_secs() as i64,
    }
}

fn kinds_of(o: &ValidOffer) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for f in &o.files {
        let k = match f.mime.split('/').next().unwrap_or("") {
            "image" => "image",
            "video" => "video",
            "audio" => "audio",
            "text" => "text",
            _ if f.mime == "application/pdf" => "pdf",
            _ if f.mime.contains("zip") || f.mime.contains("tar") || f.mime.contains("7z") => "archive",
            _ => "other",
        };
        if !v.iter().any(|x| x == k) {
            v.push(k.into());
        }
    }
    v
}

/// Fortschritt melden (höchstens 4× pro Sekunde).
struct Progress {
    db: Arc<DatabaseManager>,
    id: u64,
    incoming: bool,
    total: u64,
    done: u64,
    last: Instant,
}

impl Progress {
    fn new(db: Arc<DatabaseManager>, id: u64, incoming: bool, total: u64) -> Self {
        Self { db, id, incoming, total, done: 0, last: Instant::now() - Duration::from_secs(1) }
    }
    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed() >= Duration::from_millis(250) || self.done == self.total {
            self.last = Instant::now();
            self.db.events().emit(Event::DropProgress {
                id: self.id,
                incoming: self.incoming,
                done_bytes: self.done,
                total_bytes: self.total,
            });
        }
    }
}

/// Empfängt eine Datei in eine Teil-Datei, prüft Größe und BLAKE3 und benennt erst dann um.
async fn receive_one(
    uni: &mut RecvStream,
    dir: &Path,
    f: &super::wire::ValidFile,
    progress: &mut Progress,
) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(|e| anyhow::anyhow!("Zufall nicht verfügbar: {e}"))?;
    let part = dir.join(format!(".snap-drop-{}{PART_SUFFIX}", hex(&rnd)));
    let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&part)?;
    let mut file = tokio::fs::File::from_std(file);
    let res: Result<()> = async {
        let mut hasher = blake3::Hasher::new();
        let mut got: u64 = 0;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match tokio::time::timeout(IDLE_TIMEOUT, uni.read(&mut buf)).await? {
                Ok(Some(n)) => n,
                Ok(None) => break,
                Err(e) => return Err(e.into()),
            };
            got += n as u64;
            if got > f.size {
                api_bail!(InvalidInput, "Gegenüber sendet mehr Daten als angekündigt");
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).await?;
            progress.add(n as u64);
        }
        if got != f.size {
            api_bail!(InvalidInput, "Datei unvollständig empfangen");
        }
        if hasher.finalize().as_bytes() != &f.hash {
            api_bail!(InvalidInput, "Prüfsumme stimmt nicht – Datei verworfen");
        }
        file.sync_all().await?;
        Ok(())
    }
    .await;
    drop(file);
    if let Err(e) = res {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    let target = reserve_name(dir, &f.name)?;
    if let Err(e) = std::fs::rename(&part, &target) {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&target);
        return Err(e.into());
    }
    Ok(target)
}

/// Legt eine leere Datei unter einem freien Namen an („foto (2).jpg“) und gibt den Pfad zurück.
/// Das anschließende Umbenennen ersetzt nur diese eigene, leere Datei.
fn reserve_name(dir: &Path, name: &str) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1..10_000u32 {
        let candidate = if n == 1 { name.to_string() } else { format!("{stem} ({n}){ext}") };
        let p = dir.join(&candidate);
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    api_bail!(Rejected, "Kein freier Dateiname für {name}")
}

struct Prepared {
    path: PathBuf,
    offer: OfferFile,
    preview: Option<String>,
}

/// Prüft und hasht die Dateien vor dem Angebot.
async fn prepare_files(paths: Vec<PathBuf>) -> Result<Vec<Prepared>> {
    if paths.is_empty() || paths.len() > super::wire::MAX_FILES {
        api_bail!(InvalidInput, "1 bis {} Dateien auswählen", super::wire::MAX_FILES);
    }
    tokio::task::spawn_blocking(move || {
        let mut out = Vec::with_capacity(paths.len());
        let mut preview_done = false;
        for p in paths {
            let meta = std::fs::metadata(&p).with_context(|| format!("{} nicht lesbar", p.display()))?;
            if !meta.is_file() {
                api_bail!(InvalidInput, "{} ist keine Datei", p.display());
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(super::wire::sanitize_file_name)
                .ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Dateiname"))?;
            let mut file = std::fs::File::open(&p)?;
            let mut hasher = blake3::Hasher::new();
            let mut head = Vec::new();
            let mut buf = vec![0u8; CHUNK];
            let mut size = 0u64;
            loop {
                let n = std::io::Read::read(&mut file, &mut buf)?;
                if n == 0 {
                    break;
                }
                if head.len() < 64 {
                    head.extend_from_slice(&buf[..n.min(64 - head.len())]);
                }
                hasher.update(&buf[..n]);
                size += n as u64;
            }
            let (kind, mime) = crate::media::detect(&head, &name);
            let preview = if !preview_done && kind == crate::media::MediaKind::Image {
                preview_done = true;
                super::wire::make_preview(&p)
            } else {
                None
            };
            out.push(Prepared {
                offer: OfferFile {
                    name,
                    size,
                    hash: hex(hasher.finalize().as_bytes()),
                    mime: mime.unwrap_or("application/octet-stream").into(),
                },
                path: p,
                preview,
            });
        }
        Ok(out)
    })
    .await?
}

async fn send_one(uni: &mut SendStream, path: &Path, size: u64, progress: &mut Progress) -> Result<()> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    while sent < size {
        let want = CHUNK.min((size - sent) as usize);
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            api_bail!(Internal, "{} wurde während des Sendens verändert", path.display());
        }
        tokio::time::timeout(IDLE_TIMEOUT, uni.write_all(&buf[..n])).await??;
        sent += n as u64;
        progress.add(n as u64);
    }
    uni.finish()?;
    Ok(())
}
