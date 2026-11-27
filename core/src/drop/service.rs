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
    hex, Card, ConnectInfo, DeviceKind, Offer, OfferFile, Probe, Proof, Received, RemoteAddr, Reply, ValidOffer,
    DROP_ALPN, MAX_FRAME, MAX_TAGS, TOKEN_LEN, VERSION,
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
/// Ende eines „Alle“-Fensters (Unix-Sekunden), damit es nach dem Wecken weiterläuft.
const SETTING_EVERYONE_UNTIL: &str = "drop_everyone_until";
/// Wecken im Hintergrund eingeschaltet (Weck-Schlüssel-Datei wird gepflegt).
const SETTING_WAKE_EXPORT: &str = "drop_wake_export";
/// Von überall empfangen (0 = aus; Standard: an, nur für Kontakte).
const SETTING_REMOTE: &str = "drop_remote";
/// Längstes Empfangsfenster im Hintergrund (der Dienst verlängert bei Bedarf).
const BACKGROUND_MAX_SECS: u32 = 3600;
const META_NAME: &str = "name";

/// Angezeigter Name eines Kontakts: eigener (lokaler) Name vor dem Namen des Geräts.
fn label(c: &DropContactRow) -> String {
    c.alias.clone().unwrap_or_else(|| c.name.clone())
}

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
    /// Name, den andere sehen.
    pub name: String,
    /// Gerätename (gilt, solange kein eigener Name gesetzt ist).
    #[serde(default)]
    pub default_name: String,
    /// Ein eigener Anzeigename ist gesetzt.
    #[serde(default)]
    pub custom_name: bool,
    /// Fingerabdruck des eigenen Drop-Schlüssels.
    pub fingerprint: String,
    pub dir: String,
    /// Code für QR/Abtippen, solange man sichtbar ist.
    pub code: Option<String>,
    /// Kontakte dürfen von überall senden.
    #[serde(default)]
    pub remote: bool,
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
    /// Schlüssel des Kontakts (zum Zuordnen in der Oberfläche).
    #[serde(default)]
    pub contact_key: Option<String>,
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
    /// Geteilter Text (bereinigt).
    #[serde(default)]
    pub text: Option<String>,
    /// Text ist genau ein Link (nur http/https).
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OfferedFile {
    pub name: String,
    pub size: u64,
    pub mime: String,
}

/// Gespeicherter Kontakt für die Oberfläche (ohne Geheimnis).
/// Weck-Schlüssel eines Kontakts (Hex) – nur für das lokale Wecken im Hintergrund.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WakeKey {
    pub contact: String,
    pub name: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ContactInfo {
    pub key: String,
    /// Angezeigter Name (lokaler Name, sonst der Name des Geräts).
    pub name: String,
    /// Name, den sich das Gerät selbst gibt.
    #[serde(default)]
    pub device_name: String,
    /// Lokaler Name gesetzt.
    #[serde(default)]
    pub renamed: bool,
    pub fingerprint: String,
    pub added_at: i64,
    pub last_seen: i64,
    /// Fern-Adresse bekannt (Senden von überall möglich).
    #[serde(default)]
    pub remote: bool,
    /// Darf von überall an dieses Gerät senden.
    #[serde(default)]
    pub remote_ok: bool,
}

/// Eine empfangene Sendung im Verlauf (ohne Textinhalt).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub id: i64,
    pub at: i64,
    pub peer: String,
    /// `files`, `text` oder `link`.
    pub kind: String,
    /// Zahl der Dateien.
    pub count: usize,
    /// Die ersten Pfade (höchstens 20).
    pub files: Vec<String>,
    /// Die erste Datei liegt noch dort.
    pub exists: bool,
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

/// Karte eines Geräts: Name, Art, Kontakt (angezeigter Name, Schlüssel).
type CardView = (String, DeviceKind, Option<(String, String)>);

struct NearbyEntry {
    target: Target,
    via: &'static str,
    /// Name aus Bluetooth (unbestätigt), falls das Gerät per Netz nicht erreichbar ist.
    ble_name: Option<String>,
    offline: bool,
    seen: Instant,
    card: Option<CardView>,
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
    /// Fern-Adresse des Gegenübers (JSON), falls mitgeschickt.
    remote: Option<String>,
}

/// Fern-Endpunkt: dauerhafte Kennung, nimmt nur Kontakte an.
struct RemoteNode {
    endpoint: Endpoint,
    router: Router,
    addrs: Vec<SocketAddr>,
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
    /// Generation der Sichtbarkeit, die `background` gesetzt hat (zum Zurücknehmen).
    background_gen: Mutex<Option<u64>>,
    remote: tokio::sync::Mutex<Option<RemoteNode>>,
    /// Laufende Übertragungen (zum Abbrechen).
    transfers: Mutex<HashMap<u64, Connection>>,
    /// Vom Nutzer abgebrochene Übertragungen (für eine klare Meldung).
    cancelled: Mutex<HashSet<u64>>,
}

/// Schließcode „vom Nutzer abgebrochen“.
const CLOSE_CANCELLED: u32 = 5;

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
            background_gen: Mutex::new(None),
            remote: tokio::sync::Mutex::new(None),
            transfers: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashSet::new()),
        })
    }

    pub fn drop_key(&self) -> PublicKey {
        self.identity.public()
    }

    /// Stellt die gespeicherte Sichtbarkeit nach dem Start wieder her („Kontakte“ bleibt an,
    /// „Alle“ nie – das gilt immer nur 10 Minuten).
    pub async fn restore(&self) -> Result<()> {
        self.wake_export_changed();
        if self.remote_enabled() && !self.db.drop_contacts()?.is_empty() {
            if let Err(e) = self.ensure_remote().await {
                tracing::warn!("Drop: Fern-Endpunkt nicht gestartet: {e:#}");
            }
        }
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
        let until = self.vis.lock().unwrap_or_else(|e| e.into_inner()).until;
        self.db.put_setting(SETTING_EVERYONE_UNTIL, until)?;
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
        self.apply_until(mode, (mode == Mode::Everyone).then(|| Self::now() + EVERYONE_SECS)).await
    }

    async fn apply_until(&self, mode: Mode, until: Option<i64>) -> Result<u64> {
        let (until, generation) = {
            let mut v = self.vis.lock().unwrap_or_else(|e| e.into_inner());
            v.mode = mode;
            v.until = until;
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

    /// Name, den andere sehen: eigener Anzeigename oder der Gerätename (bereinigt).
    pub fn my_name(&self) -> String {
        let custom = self.db.drop_meta(META_NAME).ok().flatten();
        super::wire::display_name(custom.as_deref().unwrap_or(&self.opts.name))
    }

    /// Eigenen Anzeigenamen setzen (`None` oder leer = wieder den Gerätenamen verwenden).
    pub fn set_name(&self, name: Option<&str>) -> Result<DropStatus> {
        let cleaned = name.map(str::trim).filter(|n| !n.is_empty()).map(super::wire::display_name);
        if let Some(n) = &cleaned {
            if n.chars().count() > 40 {
                api_bail!(InvalidInput, "Höchstens 40 Zeichen");
            }
        }
        self.db.set_drop_meta(META_NAME, cleaned.as_deref())?;
        self.status()
    }

    /// Weck-Schlüssel aller Kontakte (siehe `wake.rs`).
    fn wake_keys(&self) -> Result<Vec<WakeKey>> {
        Ok(self
            .db
            .drop_contacts()?
            .into_iter()
            .map(|c| WakeKey {
                name: c.alias.clone().unwrap_or_else(|| c.name.clone()),
                key: data_encoding::HEXLOWER.encode(&super::wake::wake_key(&c.secret)),
                contact: c.key,
            })
            .collect())
    }

    /// Datei mit den Weck-Schlüsseln (neben dem Drop-Schlüssel, 0600).
    pub fn wake_file(&self) -> PathBuf {
        self.opts.identity_path.with_file_name("drop-wake.json")
    }

    /// Wecken im Hintergrund ein- oder ausschalten (Datei schreiben bzw. löschen).
    pub fn set_wake_export(&self, enable: bool) -> Result<()> {
        self.db.put_setting(SETTING_WAKE_EXPORT, enable.then_some(1))?;
        if enable {
            self.refresh_wake_export()
        } else {
            match std::fs::remove_file(self.wake_file()) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            }
        }
    }

    fn wake_export_changed(&self) {
        if let Err(e) = self.refresh_wake_export() {
            tracing::warn!("Drop: Weck-Schlüssel nicht aktualisiert: {e:#}");
        }
    }

    /// Nach jeder Kontaktänderung: Datei aktualisieren, falls das Wecken eingeschaltet ist.
    fn refresh_wake_export(&self) -> Result<()> {
        let on = self.db.settings()?.into_iter().any(|(k, v)| k == SETTING_WAKE_EXPORT && v == 1);
        if !on {
            return Ok(());
        }
        let json = serde_json::to_vec(&self.wake_keys()?)?;
        crate::secure_fs::write_private_file(&self.wake_file(), &json)
    }

    /// Kurz empfangsbereit sein, ohne die gespeicherte Sichtbarkeit zu ändern (Wecken im
    /// Hintergrund). Ist das Gerät schon sichtbar, ändert sich nichts. `secs` = 0 nimmt ein
    /// Hintergrund-Fenster zurück. Mit `everyone` läuft ein noch gültiges „Alle“-Fenster weiter;
    /// sonst nur für Kontakte.
    pub async fn background(&self, secs: u32, everyone: bool) -> Result<DropStatus> {
        let secs = secs.min(BACKGROUND_MAX_SECS);
        if secs == 0 {
            let ours = *self.background_gen.lock().unwrap_or_else(|e| e.into_inner());
            let current = self.vis.lock().unwrap_or_else(|e| e.into_inner()).generation;
            if ours == Some(current) {
                self.apply(self.base_mode()).await?;
            }
            *self.background_gen.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return self.status();
        }
        let mode = self.mode();
        let mine = *self.background_gen.lock().unwrap_or_else(|e| e.into_inner())
            == Some(self.vis.lock().unwrap_or_else(|e| e.into_inner()).generation);
        if mode != Mode::Off && !mine {
            return self.status(); // vom Nutzer sichtbar geschaltet – nichts ändern
        }
        let saved_until = self
            .db
            .settings()
            .ok()
            .and_then(|s| s.into_iter().find(|(k, _)| k == SETTING_EVERYONE_UNTIL))
            .map(|(_, v)| v)
            .filter(|u| *u > Self::now());
        let (target, until) = match saved_until {
            Some(u) if everyone => (Mode::Everyone, Some(u)),
            _ => (Mode::Contacts, None),
        };
        let generation = self.apply_until(target, until).await?;
        *self.background_gen.lock().unwrap_or_else(|e| e.into_inner()) = Some(generation);
        let end = match until {
            Some(u) => (u - Self::now()).clamp(1, i64::from(secs)) as u64,
            None => u64::from(secs),
        };
        let me = self.me.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(end)).await;
            if let Some(svc) = me.upgrade() {
                let still = svc.vis.lock().unwrap_or_else(|e| e.into_inner()).generation == generation;
                if still {
                    if let Err(e) = svc.apply(svc.base_mode()).await {
                        tracing::warn!("Drop: Ende des Hintergrund-Fensters fehlgeschlagen: {e:#}");
                    }
                }
            }
        });
        self.status()
    }

    // --- Von überall (Kontakte) ---

    fn remote_enabled(&self) -> bool {
        !self.db.settings().ok().into_iter().flatten().any(|(k, v)| k == SETTING_REMOTE && v == 0)
    }

    /// Von überall empfangen ein- oder ausschalten.
    pub async fn set_remote(&self, enabled: bool) -> Result<DropStatus> {
        self.db.put_setting(SETTING_REMOTE, Some(i64::from(enabled)))?;
        if enabled {
            self.ensure_remote().await?;
        } else if let Some(r) = self.remote.lock().await.take() {
            let _ = r.router.shutdown().await;
        }
        self.status()
    }

    /// Darf ein Kontakt von überall senden?
    pub fn set_contact_remote(&self, key: &str, ok: bool) -> Result<ContactInfo> {
        if !self.db.set_drop_contact_remote_ok(key, ok)? {
            api_bail!(NotFound, "Kein solcher Kontakt");
        }
        match self.contact_by_key(key)? {
            Some(c) => Ok(contact_info(c)),
            None => api_bail!(NotFound, "Kein solcher Kontakt"),
        }
    }

    /// Startet den Fern-Endpunkt (dauerhafte Kennung aus einer eigenen Schlüsseldatei).
    async fn ensure_remote(&self) -> Result<()> {
        let mut guard = self.remote.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let key_path = self.opts.identity_path.with_file_name("drop-remote.key");
        if let Some(dir) = key_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let key = crate::secure_fs::load_or_create_secret_key(&key_path)?;
        let endpoint = self
            .endpoint_builder_with(key)?
            .alpns(vec![DROP_ALPN.to_vec()])
            .bind()
            .await
            .context("Fern-Endpunkt konnte nicht gestartet werden")?;
        let router = Router::builder(endpoint.clone())
            .accept(DROP_ALPN, DropReceiver { svc: self.me.clone(), remote: true })
            .spawn();
        let addrs = wait_for_addrs(&endpoint).await;
        *guard = Some(RemoteNode { endpoint, router, addrs });
        Ok(())
    }

    /// Eigene Fern-Adresse zum Mitschicken (startet den Endpunkt bei Bedarf).
    async fn remote_addr(&self) -> Option<RemoteAddr> {
        if !self.remote_enabled() {
            return None;
        }
        if let Err(e) = self.ensure_remote().await {
            tracing::warn!("Drop: Fern-Endpunkt nicht gestartet: {e:#}");
            return None;
        }
        let guard = self.remote.lock().await;
        guard.as_ref().map(|r| RemoteAddr::new(&r.endpoint.id(), &r.addrs))
    }

    /// Nur für Tests: gespeicherte Fern-Adresse eines Kontakts lesen bzw. setzen.
    #[doc(hidden)]
    pub fn contact_remote_for_tests(&self, key: &str) -> Option<RemoteAddr> {
        self.contact_by_key(key).ok().flatten()?.remote.and_then(|j| serde_json::from_str(&j).ok())
    }

    #[doc(hidden)]
    pub fn set_contact_remote_for_tests(&self, key: &str, remote: &RemoteAddr) {
        if let Ok(Some(c)) = self.contact_by_key(key) {
            let _ = self.db.upsert_drop_contact(&DropContactRow { remote: serde_json::to_string(remote).ok(), ..c });
        }
    }

    /// An einen gespeicherten Kontakt senden, egal wo er ist (über seinen Fern-Endpunkt; das
    /// Relais sieht nur verschlüsselte Daten). Die Gegenseite muss wie immer annehmen.
    pub async fn send_to_contact(&self, key: &str, paths: Vec<PathBuf>, text: Option<String>) -> Result<SendResult> {
        let Some(c) = self.contact_by_key(key)? else {
            api_bail!(NotFound, "Kein solcher Kontakt");
        };
        let Some((endpoint, addrs)) =
            c.remote.as_deref().and_then(|j| serde_json::from_str::<RemoteAddr>(j).ok()).and_then(|r| r.validate())
        else {
            api_bail!(
                NotFound,
                "Für „{}“ ist noch keine Adresse bekannt – einmal in der Nähe tauschen, dann geht es von überall",
                label(&c)
            );
        };
        let target = Target { endpoint, addrs };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let r = self.send_to(id, &target, paths, text, Some(&c.key)).await;
        let (ok, error, code, can_save) = match r {
            Ok((code, can_save)) => (true, None, Some(code), can_save),
            Err(e) => (false, Some(format!("{e:#}")), None, false),
        };
        self.db.events().emit(Event::DropFinished {
            id,
            incoming: false,
            ok,
            peer: label(&c),
            files: vec![],
            error: error.clone(),
            can_save_contact: can_save,
            text: None,
            link: None,
        });
        Ok(SendResult { id, ok, error, code })
    }

    /// Laufende Übertragung bzw. offenes Angebot abbrechen (beide Seiten erfahren es sofort;
    /// halb empfangene Dateien werden verworfen).
    pub fn cancel(&self, id: u64) -> Result<()> {
        if self.pending.lock().unwrap_or_else(|e| e.into_inner()).contains_key(&id) {
            return self.respond(id, false, false);
        }
        let conn = self.transfers.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
        let Some(conn) = conn else {
            api_bail!(NotFound, "Diese Übertragung läuft nicht mehr");
        };
        self.cancelled.lock().unwrap_or_else(|e| e.into_inner()).insert(id);
        conn.close(VarInt::from_u32(CLOSE_CANCELLED), b"cancelled");
        Ok(())
    }

    /// Klare Fehlermeldung für eine beendete Übertragung (abgebrochen hier oder drüben).
    fn transfer_error(&self, id: u64, conn: &Connection, e: &anyhow::Error) -> String {
        if self.cancelled.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            return "Abgebrochen".into();
        }
        if let Some(iroh::endpoint::ConnectionError::ApplicationClosed(c)) = conn.close_reason() {
            if c.error_code == VarInt::from_u32(CLOSE_CANCELLED) {
                return "Vom Gegenüber abgebrochen".into();
            }
        }
        format!("{e:#}")
    }

    /// Zuletzt empfangene Sendungen (neueste zuerst).
    pub fn history(&self, limit: usize) -> Result<Vec<HistoryItem>> {
        Ok(self
            .db
            .drop_history(limit.min(200))?
            .into_iter()
            .map(|(id, at, peer, kind, files)| HistoryItem {
                id,
                at,
                peer,
                kind,
                count: files.len(),
                exists: files.first().is_some_and(|f| Path::new(f).exists()),
                files: files.into_iter().take(20).collect(),
            })
            .collect())
    }

    pub fn clear_history(&self) -> Result<()> {
        self.db.clear_drop_history()
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
            name: self.my_name(),
            default_name: super::wire::display_name(&self.opts.name),
            custom_name: self.db.drop_meta(META_NAME).ok().flatten().is_some(),
            fingerprint: identity::fingerprint(&self.identity.public()),
            dir: self.opts.dir.display().to_string(),
            code: if mode == Mode::Off { None } else { code },
            remote: self.remote_enabled(),
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
        out.name = everyone.then(|| self.my_name());
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
        self.endpoint_builder_with(SecretKey::generate())
    }

    fn endpoint_builder_with(&self, key: SecretKey) -> Result<iroh::endpoint::Builder> {
        let mut b = if self.opts.use_relay {
            let b = Endpoint::builder(presets::N0);
            match crate::network::relay_mode(&self.opts.relay_urls, self.opts.relay_fallback_n0)? {
                Some(mode) => b.relay_mode(mode),
                None => b,
            }
        } else {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        };
        b = b.secret_key(key);
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
        let router = Router::builder(endpoint.clone())
            .accept(DROP_ALPN, DropReceiver { svc: self.me.clone(), remote: false })
            .spawn();
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
        if let Some(r) = self.remote.lock().await.take() {
            let _ = r.router.shutdown().await;
        }
        if let Some((ep, _)) = self.sender.lock().await.take() {
            ep.close().await;
        }
    }

    // --- Empfangen ---

    async fn handle_incoming(&self, conn: &Connection, remote: bool) -> Result<()> {
        let Ok(_permit) = self.slots.clone().try_acquire_owned() else {
            conn.close(VarInt::from_u32(1), b"busy");
            return Ok(());
        };
        // Von überall gilt immer „nur Kontakte“ – unabhängig von der Sichtbarkeit in der Nähe.
        let mode = if remote {
            if self.remote_enabled() {
                Mode::Contacts
            } else {
                Mode::Off
            }
        } else {
            self.mode()
        };
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
        // Von überall nur Kontakte, denen das erlaubt ist
        let contact = contact.filter(|c| !remote || c.remote_ok);
        if mode == Mode::Contacts && contact.is_none() {
            write_frame_max(&mut send, &Card::Hidden, MAX_FRAME).await?;
            let _ = send.finish();
            let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
            return Ok(());
        }
        let card = Card::Visible {
            name: self.my_name(),
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
        let contact_name = self.contact_by_key(&sender_key)?.map(|c| label(&c));
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
                self.wake_export_changed();
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

        self.transfers.lock().unwrap_or_else(|e| e.into_inner()).insert(id, conn.clone());
        let mut result = self.receive_files(conn, id, &offer).await;
        self.transfers.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        if let Ok(saved) = &mut result {
            unpack_folders(saved, &offer).await;
        }
        let peer = contact_name.clone().unwrap_or_else(|| offer.name.clone());
        match result {
            Ok(saved) => {
                let fin = Received {
                    ok: true,
                    proof: Some(Proof::new(&self.identity.public(), &self.identity.sign_session(&session))),
                    remote: self.remote_addr().await,
                };
                let _ = write_frame_max(&mut send, &fin, MAX_FRAME).await;
                let _ = send.finish();
                let remote = offer.remote.as_ref().map(|(k, a)| RemoteAddr::new(k, a));
                let can_save = self.remember_exchange(id, offer.sender, &offer.name, &session, remote)?;
                let files: Vec<String> = saved.iter().map(|p| p.display().to_string()).collect();
                let kind = if !files.is_empty() {
                    "files"
                } else if offer.link.is_some() {
                    "link"
                } else {
                    "text"
                };
                if let Err(e) = self.db.add_drop_history(&peer, kind, &files) {
                    tracing::warn!("Drop: Verlauf nicht gespeichert: {e:#}");
                }
                self.db.events().emit(Event::DropFinished {
                    id,
                    incoming: true,
                    ok: true,
                    peer,
                    files: saved.iter().map(|p| p.display().to_string()).collect(),
                    error: None,
                    can_save_contact: can_save,
                    text: offer.text.clone(),
                    link: offer.link.clone(),
                });
                let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
            }
            Err(e) => {
                let _ = write_frame_max(&mut send, &Received { ok: false, proof: None, remote: None }, MAX_FRAME).await;
                self.db.events().emit(Event::DropFinished {
                    id,
                    incoming: true,
                    ok: false,
                    peer,
                    files: vec![],
                    error: Some(self.transfer_error(id, conn, &e)),
                    can_save_contact: false,
                    text: None,
                    link: None,
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
    fn remember_exchange(
        &self,
        id: u64,
        key: PublicKey,
        name: &str,
        session: &[u8; 32],
        remote: Option<RemoteAddr>,
    ) -> Result<bool> {
        let secret = identity::contact_secret(session);
        let key_hex = hex(key.as_bytes());
        let remote = remote.and_then(|r| serde_json::to_string(&r).ok());
        if let Some(c) = self.contact_by_key(&key_hex)? {
            self.db.upsert_drop_contact(&DropContactRow {
                secret: secret.to_vec(),
                last_seen: Self::now(),
                name: super::wire::display_name(name),
                remote: remote.or(c.remote.clone()),
                ..c
            })?;
            self.wake_export_changed(); // neues Geheimnis → neuer Weck-Schlüssel
            return Ok(false);
        }
        let mut ex = self.exchanges.lock().unwrap_or_else(|e| e.into_inner());
        ex.retain(|_, e| e.at.elapsed() < EXCHANGE_TTL);
        ex.insert(id, Exchange { key, name: super::wire::display_name(name), secret, at: Instant::now(), remote });
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
        Ok(self.db.drop_contacts()?.into_iter().map(contact_info).collect())
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
            alias: None,
            secret: ex.secret.to_vec(),
            added_at: now,
            last_seen: now,
            remote: ex.remote,
            remote_ok: true,
        };
        self.db.upsert_drop_contact(&row)?;
        let _ = self.db.unblock_drop(&row.key);
        self.invalidate_cards();
        // Ein schon vorhandener lokaler Name bleibt erhalten
        let stored = self.contact_by_key(&row.key)?.unwrap_or(row);
        Ok(contact_info(stored))
    }

    /// Kontakt lokal umbenennen (`None` oder leer = wieder den Namen des Geräts zeigen).
    pub fn rename_contact(&self, key: &str, alias: Option<&str>) -> Result<ContactInfo> {
        let cleaned = alias.map(str::trim).filter(|a| !a.is_empty()).map(super::wire::display_name);
        if cleaned.as_ref().is_some_and(|a| a.chars().count() > 40) {
            api_bail!(InvalidInput, "Höchstens 40 Zeichen");
        }
        if !self.db.set_drop_contact_alias(key, cleaned.as_deref())? {
            api_bail!(NotFound, "Kein solcher Kontakt");
        }
        self.invalidate_cards();
        match self.contact_by_key(key)? {
            Some(c) => Ok(contact_info(c)),
            None => api_bail!(NotFound, "Kein solcher Kontakt"),
        }
    }

    pub fn remove_contact(&self, key: &str) -> Result<bool> {
        let removed = self.db.remove_drop_contact(key)?;
        self.invalidate_cards(); // danach: auch die Weck-Schlüssel-Datei ohne ihn
        Ok(removed)
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
            name: self.my_name(),
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
    async fn probe(&self, target: &Target) -> Result<Option<CardView>> {
        let conn = self.connect(target).await?;
        let session = identity::session_value(&conn)?;
        let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.open_bi()).await??;
        write_frame_max(&mut send, &self.probe_for(&session)?, MAX_FRAME).await?;
        let card: Card = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
        let _ = send.finish();
        conn.close(VarInt::from_u32(0), b"probe");
        self.card_view(card, &session)
    }

    fn card_view(&self, card: Card, session: &[u8; 32]) -> Result<Option<CardView>> {
        Ok(match card {
            Card::Hidden => None,
            Card::Visible { name, kind, proof } => {
                let contact = match proof {
                    Some(p) => {
                        let key = p.verify(session)?;
                        self.contact_by_key(&hex(key.as_bytes()))?.map(|c| (label(&c), c.key.clone()))
                    }
                    None => None,
                };
                Some((super::wire::display_name(&name), kind, contact))
            }
        })
    }

    /// Sendet Dateien an ein Gerät aus der Nähe-Liste (oder per Code). Wartet bis zum Ende.
    pub async fn send(&self, peer: &str, paths: Vec<PathBuf>) -> Result<SendResult> {
        self.send_with_text(peer, paths, None).await
    }

    /// Wie [`Self::send`], zusätzlich (oder nur) mit Text bzw. Link.
    pub async fn send_with_text(&self, peer: &str, paths: Vec<PathBuf>, text: Option<String>) -> Result<SendResult> {
        let target = self.target_of(peer)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let peer_name = self.peer_label(&target);
        let r = self.send_to(id, &target, paths, text, None).await;
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
            text: None,
            link: None,
        });
        Ok(SendResult { id, ok, error, code })
    }

    fn peer_label(&self, target: &Target) -> String {
        let nearby = self.nearby.lock().unwrap_or_else(|e| e.into_inner());
        nearby
            .get(&target.endpoint)
            .and_then(|e| e.card.as_ref())
            .map(|(n, _, c)| c.as_ref().map(|c| c.0.clone()).unwrap_or_else(|| n.clone()))
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

    async fn send_to(
        &self,
        id: u64,
        target: &Target,
        paths: Vec<PathBuf>,
        text: Option<String>,
        expect_contact: Option<&str>,
    ) -> Result<(String, bool)> {
        let text = text.filter(|t| !t.trim().is_empty());
        if text.as_ref().is_some_and(|t| t.len() > super::wire::MAX_TEXT) {
            api_bail!(InvalidInput, "Text ist zu lang (höchstens 64 KB)");
        }
        // Ordner werden für die Übertragung gepackt (privater Arbeitsordner, danach gelöscht)
        let (paths, packed) = self.pack_folders(paths).await?;
        let prepared = if paths.is_empty() && text.is_some() { Ok(Vec::new()) } else { prepare_files(paths).await };
        let mut files = match prepared {
            Ok(f) => f,
            Err(e) => {
                remove_all(&packed);
                return Err(e);
            }
        };
        for f in files.iter_mut().filter(|f| packed.contains(&f.path)) {
            f.offer.mime = super::wire::FOLDER_MIME.into();
        }
        let r = self.send_files(id, target, files, text, expect_contact).await;
        remove_all(&packed);
        r
    }

    /// Ordner in ZIPs packen (je Ordner eines, im privaten Arbeitsordner). Liefert die neue
    /// Pfadliste und die angelegten ZIPs.
    async fn pack_folders(&self, paths: Vec<PathBuf>) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
        if !paths.iter().any(|p| p.is_dir()) {
            return Ok((paths, Vec::new()));
        }
        let work = self.opts.identity_path.with_file_name("drop-pack");
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&work)?;
            let mut out = Vec::with_capacity(paths.len());
            let mut packed = Vec::new();
            for p in paths {
                if !p.is_dir() {
                    out.push(p);
                    continue;
                }
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("Ordner").to_string();
                // Inhalt ohne den Ordner selbst packen: Der Empfänger entpackt in einen Ordner
                // mit dem Namen des ZIPs (sonst stünde alles doppelt verschachtelt da).
                let mut children: Vec<PathBuf> =
                    std::fs::read_dir(&p)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
                children.sort();
                if children.is_empty() {
                    remove_all(&packed);
                    api_bail!(InvalidInput, "Der Ordner „{name}“ ist leer");
                }
                let progress = crate::fileops::ops::Progress::default();
                match crate::fileops::archive::compress_into(&children, &work, Some(&name), &progress) {
                    Ok(z) => {
                        packed.push(z.archive.clone());
                        out.push(z.archive);
                    }
                    Err(e) => {
                        remove_all(&packed);
                        return Err(e);
                    }
                }
            }
            Ok((out, packed))
        })
        .await
        .context("Packen abgebrochen")?
    }

    async fn send_files(
        &self,
        id: u64,
        target: &Target,
        files: Vec<Prepared>,
        text: Option<String>,
        expect_contact: Option<&str>,
    ) -> Result<(String, bool)> {
        let conn = self.connect(target).await?;
        let total: u64 = files.iter().map(|f| f.offer.size).sum();
        self.transfers.lock().unwrap_or_else(|e| e.into_inner()).insert(id, conn.clone());
        // Erstes Lebenszeichen: die Oberfläche kennt so die Kennung (zum Abbrechen)
        self.db.events().emit(Event::DropProgress { id, incoming: false, done_bytes: 0, total_bytes: total });
        let r = self.send_on(id, &conn, files, text, expect_contact).await;
        self.transfers.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        r.map_err(|e| anyhow::anyhow!(self.transfer_error(id, &conn, &e)))
    }

    async fn send_on(
        &self,
        id: u64,
        conn: &Connection,
        files: Vec<Prepared>,
        text: Option<String>,
        expect_contact: Option<&str>,
    ) -> Result<(String, bool)> {
        let session = identity::session_value(conn)?;
        let code = identity::check_code(&session);
        let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.open_bi()).await??;
        write_frame_max(&mut send, &self.probe_for(&session)?, MAX_FRAME).await?;
        let card: Card = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;
        let Some((name, _, contact)) = self.card_view(card, &session)? else {
            api_bail!(PermissionDenied, "Das Gerät empfängt gerade nur von Kontakten");
        };
        // Von überall: Am anderen Ende muss genau dieser Kontakt sitzen (Beweis mit seinem Schlüssel).
        if let Some(expected) = expect_contact {
            if contact.as_ref().map(|c| c.1.as_str()) != Some(expected) {
                api_bail!(PermissionDenied, "Das Gerät hat sich nicht als dieser Kontakt ausgewiesen");
            }
        }
        let offer = Offer {
            name: self.my_name(),
            kind: self.opts.kind,
            proof: Proof::new(&self.identity.public(), &self.identity.sign_session(&session)),
            files: files.iter().map(|f| f.offer.clone()).collect(),
            preview: files.iter().find_map(|f| f.preview.clone()),
            text,
            remote: self.remote_addr().await,
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
        let remote = fin.remote.as_ref().and_then(|r| r.validate()).map(|(k, a)| RemoteAddr::new(&k, &a));
        let can_save = match fin.proof.map(|p| p.verify(&session)).transpose()? {
            Some(key) => self.remember_exchange(id, key, &name, &session, remote)?,
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
        self.wake_export_changed();
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
                        contact_key: None,
                        via: e.via.into(),
                        offline: true,
                    });
                }
                let (name, kind, contact) = e.card.clone()?;
                Some(NearbyPeer {
                    id: hex(k.as_bytes()),
                    name: Some(name),
                    kind: Some(kind),
                    contact_key: contact.as_ref().map(|c| c.1.clone()),
                    contact: contact.map(|c| c.0),
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
            contact_key: contact.as_ref().map(|c| c.1.clone()),
            contact: contact.map(|c| c.0),
            via: "code".into(),
            offline: false,
        })
    }
}

fn contact_info(c: DropContactRow) -> ContactInfo {
    ContactInfo {
        fingerprint: super::wire::hex_array::<32>(&c.key)
            .ok()
            .and_then(|k| PublicKey::from_bytes(&k).ok())
            .map(|k| identity::fingerprint(&k))
            .unwrap_or_default(),
        name: label(&c),
        renamed: c.alias.is_some(),
        device_name: c.name,
        key: c.key,
        added_at: c.added_at,
        last_seen: c.last_seen,
        remote: c.remote.is_some(),
        remote_ok: c.remote_ok,
    }
}

#[derive(Clone, Debug)]
struct DropReceiver {
    svc: Weak<DropService>,
    /// Fern-Endpunkt: nur Kontakte, unabhängig von der Sichtbarkeit.
    remote: bool,
}

impl ProtocolHandler for DropReceiver {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if let Some(svc) = self.svc.upgrade() {
            if let Err(e) = svc.handle_incoming(&conn, self.remote).await {
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
        text: o.text.clone(),
        link: o.link.clone(),
    }
}

fn kinds_of(o: &ValidOffer) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if o.text.is_some() {
        v.push(if o.link.is_some() { "link".into() } else { "text".into() });
    }
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

fn remove_all(paths: &[PathBuf]) {
    for p in paths {
        let _ = std::fs::remove_file(p);
    }
}

/// Als Ordner gesendete ZIPs entpacken (gehärteter Entpacker mit Grenzen); das ZIP wird danach
/// gelöscht. Schlägt das Entpacken fehl, bleibt das ZIP liegen.
async fn unpack_folders(saved: &mut [PathBuf], offer: &ValidOffer) {
    for (path, f) in saved.iter_mut().zip(&offer.files) {
        if f.mime != super::wire::FOLDER_MIME || !f.name.to_ascii_lowercase().ends_with(".zip") {
            continue;
        }
        let zip = path.clone();
        let r = tokio::task::spawn_blocking(move || {
            let limits = zip.parent().map(crate::fileops::archive::Limits::for_target).transpose()?;
            crate::fileops::archive::extract(&zip, None, limits, &crate::fileops::ops::Progress::default())
        })
        .await;
        match r {
            Ok(Ok(x)) => {
                let _ = std::fs::remove_file(&*path);
                *path = x.dir;
            }
            Ok(Err(e)) => tracing::warn!("Drop: Ordner nicht entpackt (ZIP bleibt): {e:#}"),
            Err(e) => tracing::warn!("Drop: Entpacken abgebrochen: {e}"),
        }
    }
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
