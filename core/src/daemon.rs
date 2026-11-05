//! Der Daemon: Start/Stopp, IPC-Server und Hintergrund-Tasks.
//! Die Operationen selbst liegen in `api`, Warteschlange in `worker`, Abgleich in `sync`.

use crate::api::handle_request;
use crate::sync::run_periodic_sync;
use crate::worker::run_pending_pin_worker;

use crate::config::Config;
use crate::database::DatabaseManager;
use crate::api::error::ErrorCode;
use crate::ipc::{Envelope, API_VERSION, read_frame, write_frame, Request, Response};
use crate::ledger::{LedgerCtx, LedgerEntry, LedgerProtocol, LEDGER_ALPN};
use crate::network::{AccessCheck, ConnectHook, NetworkOptions, P2pNetwork};
use crate::pairing::{PairingCtx, PairingProtocol, PairingState, PAIR_ALPN};
use crate::secure_fs::{current_uid, ensure_private_dir, load_or_create_secret_key};
use anyhow::{bail, Context, Result};
use iroh_blobs::Hash;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use iroh::SecretKey;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Zeit, die ein Client maximal zum Senden seiner Anfrage hat.
pub(crate) const IPC_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Intervall für das Aufräumen abgelaufener Papierkorb-Einträge.
pub(crate) const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
/// Ab diesem Alter löst `list` einen Hintergrund-Abgleich aus.
pub(crate) const CATALOG_MAX_AGE_SECS: i64 = 300;

pub struct DaemonInner {
    pub config: Config,
    pub network: P2pNetwork,
    pub db: Arc<DatabaseManager>,
    /// Weckt den Warteschlangen-Worker (neuer Auftrag, Retry, Peer meldet sich).
    pub sync_notifier: Arc<Notify>,
    pub(crate) secret_key: SecretKey,
    pub(crate) ledger: Arc<LedgerCtx>,
    pub(crate) pairing: Arc<PairingCtx>,
    /// Verhindert parallele Ledger-Abgleiche.
    pub(crate) sync_lock: Arc<tokio::sync::Mutex<()>>,
}

impl DaemonInner {
    pub(crate) fn self_id(&self) -> String {
        self.network.node_id().to_string()
    }

    /// Schreibt einen eigenen, signierten Ledger-Eintrag für eine Datei.
    pub(crate) fn record_entry(&self, file_id: i64, record: &crate::database::FileRecord, deleted: bool) -> Result<LedgerEntry> {
        let key = self.secret_key.clone();
        self.db.record_own_entry(
            &self.self_id(),
            file_id,
            &record.blake3_hash,
            &record.file_name,
            record.file_size,
            record.created_at,
            deleted,
            &move |e: &LedgerEntry| e.sign(&key),
        )
    }
}

#[derive(Clone)]
pub struct Daemon {
    inner: Arc<DaemonInner>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Daemon {
    /// Startet Datenbank, P2P-Netzwerk, IPC-Server, Warteschlangen-Worker und Aufräum-Task.
    pub async fn start(config: Config) -> Result<Self> {
        Self::start_inner(config, false).await
    }

    /// Wie [`Daemon::start`], bindet das Netzwerk aber nur an 127.0.0.1 (für Tests).
    #[doc(hidden)]
    pub async fn start_loopback(config: Config) -> Result<Self> {
        Self::start_inner(config, true).await
    }

    async fn start_inner(config: Config, loopback_only: bool) -> Result<Self> {
        ensure_private_dir(&config.data_dir)?;
        let socket_dir = config
            .socket_path
            .parent()
            .context("Ungültiger Socket-Pfad")?
            .to_path_buf();
        ensure_private_dir(&socket_dir)?;
        prepare_socket_path(&config.socket_path).await?;

        let db = Arc::new(DatabaseManager::open(&config.db_path())?);
        let secret_key = load_or_create_secret_key(&config.secret_key_path())?;
        let sync_notifier = Arc::new(Notify::new());

        let access: AccessCheck = {
            let db = db.clone();
            Arc::new(move |hash: &Hash| db.is_shared(&hash.to_string()).unwrap_or(false))
        };
        let on_connect: ConnectHook = {
            let db = db.clone();
            let notifier = sync_notifier.clone();
            Arc::new(move |id| match db.mark_known_peer_seen(&id.to_string()) {
                Ok(true) => {
                    tracing::info!("Peer {} hat sich gemeldet – Warteschlange wird geprüft", id.fmt_short());
                    notifier.notify_one();
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("Peer-Status konnte nicht aktualisiert werden: {e}"),
            })
        };

        let pairing_state = Arc::new(PairingState::default());
        let mut ledger_slot: Option<Arc<LedgerCtx>> = None;
        let mut pairing_slot: Option<Arc<PairingCtx>> = None;
        let network = P2pNetwork::start(
            NetworkOptions {
                blob_store_path: config.blob_store_path(),
                secret_key: secret_key.clone(),
                connect_timeout: Duration::from_secs(config.connect_timeout_secs),
                bind_port: config.bind_port,
                use_relay: config.use_relay,
                loopback_only,
            },
            access,
            on_connect,
            |builder, endpoint| {
                let ledger = Arc::new(LedgerCtx {
                    db: db.clone(),
                    endpoint: endpoint.clone(),
                    device_name: config.device_name.clone(),
                    notifier: sync_notifier.clone(),
                });
                let pairing = Arc::new(PairingCtx {
                    db: db.clone(),
                    endpoint: endpoint.clone(),
                    device_name: config.device_name.clone(),
                    state: pairing_state.clone(),
                });
                ledger_slot = Some(ledger.clone());
                pairing_slot = Some(pairing.clone());
                builder
                    .accept(LEDGER_ALPN, LedgerProtocol { ctx: ledger })
                    .accept(PAIR_ALPN, PairingProtocol { ctx: pairing })
            },
        )
        .await?;
        let ledger = ledger_slot.context("Ledger-Protokoll nicht initialisiert")?;
        let pairing = pairing_slot.context("Pairing-Protokoll nicht initialisiert")?;

        let listener = UnixListener::bind(&config.socket_path)
            .with_context(|| format!("IPC-Socket {} nicht anlegbar", config.socket_path.display()))?;
        std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o600))?;

        let inner = Arc::new(DaemonInner {
            config,
            network,
            db,
            sync_notifier,
            secret_key,
            ledger,
            pairing,
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
        });
        backfill_ledger(&inner)?;
        let daemon = Self { inner: inner.clone(), tasks: Arc::new(Mutex::new(Vec::new())) };

        let ipc = tokio::spawn(run_ipc_server(inner.clone(), listener));
        let worker = tokio::spawn(run_pending_pin_worker(inner.clone()));
        let pruner = tokio::spawn(run_pruner(inner.clone()));
        let periodic = tokio::spawn(run_periodic_sync(inner.clone()));
        daemon.tasks.lock().unwrap().extend([ipc, worker, pruner, periodic]);

        Ok(daemon)
    }

    pub fn node_id(&self) -> String {
        self.inner.network.node_id().to_string()
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    /// Verarbeitet eine Anfrage direkt (ohne Socket) – z. B. für Tests oder eingebettete Nutzung.
    pub async fn request(&self, req: Request) -> Response {
        handle_request(&self.inner, req).await
    }

    /// Ereignisse abonnieren (eingebettete Nutzung, z. B. Mobile per FFI).
    pub fn subscribe(&self) -> crate::events::Subscription {
        self.inner.db.events().subscribe()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn network(&self) -> &P2pNetwork {
        &self.inner.network
    }

    pub async fn shutdown(&self) {
        let tasks: Vec<_> = std::mem::take(&mut *self.tasks.lock().unwrap());
        for t in tasks {
            t.abort();
        }
        self.inner.network.shutdown().await;
        let _ = std::fs::remove_file(&self.inner.config.socket_path);
    }
}

/// Entfernt einen verwaisten Socket von einem früheren Absturz – aber nur, wenn
/// dort kein Daemon mehr antwortet.
pub(crate) async fn prepare_socket_path(path: &Path) -> Result<()> {
    if tokio::fs::symlink_metadata(path).await.is_err() {
        return Ok(());
    }
    if UnixStream::connect(path).await.is_ok() {
        bail!("Es läuft bereits ein Daemon auf {}", path.display());
    }
    tokio::fs::remove_file(path)
        .await
        .with_context(|| format!("Verwaister Socket {} nicht entfernbar", path.display()))?;
    Ok(())
}

// ----------------------------------------------------------------------
// IPC
// ----------------------------------------------------------------------

pub(crate) async fn run_ipc_server(inner: Arc<DaemonInner>, listener: UnixListener) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                tracing::warn!("IPC accept fehlgeschlagen: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let inner = inner.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_ipc_connection(&inner, stream).await {
                tracing::debug!("IPC-Verbindung beendet: {e:#}");
            }
        });
    }
}

pub(crate) async fn handle_ipc_connection(inner: &DaemonInner, mut stream: UnixStream) -> Result<()> {
    // Nur Prozesse desselben Benutzers dürfen den Daemon steuern.
    let cred = stream.peer_cred().context("Peer-Credentials nicht lesbar")?;
    if cred.uid() != current_uid() {
        tracing::warn!("IPC-Verbindung von fremder uid {} abgelehnt", cred.uid());
        return Ok(());
    }
    let envelope: Envelope = tokio::time::timeout(IPC_READ_TIMEOUT, read_frame(&mut stream))
        .await
        .context("Zeitüberschreitung beim Lesen der Anfrage")??;
    if envelope.v != API_VERSION {
        let res = Response::error(
            ErrorCode::VersionMismatch,
            format!(
                "CLI/App spricht API-Version {}, der Daemon Version {API_VERSION}. Bitte beide auf denselben Stand bringen.",
                envelope.v
            ),
        );
        return write_frame(&mut stream, &res).await;
    }
    if let Request::Subscribe = envelope.req {
        return stream_events(inner, stream).await;
    }
    let res = handle_request(inner, envelope.req).await;
    write_frame(&mut stream, &res).await
}

/// Sendet Ereignisse, bis der Client die Verbindung schließt.
async fn stream_events(inner: &DaemonInner, mut stream: UnixStream) -> Result<()> {
    let mut sub = inner.db.events().subscribe();
    // Bestätigung, damit der Client weiß, dass das Abo aktiv ist.
    write_frame(&mut stream, &Response::Success { message: "Abonniert".into() }).await?;
    let mut probe = [0u8; 1];
    loop {
        tokio::select! {
            event = sub.next() => match event {
                Some(e) => write_frame(&mut stream, &Response::Event(e)).await?,
                None => return Ok(()),
            },
            // Client hat geschlossen (EOF) oder sendet unerwartet Daten -> beenden.
            _ = tokio::io::AsyncReadExt::read(&mut stream, &mut probe) => return Ok(()),
        }
    }
}


// ----------------------------------------------------------------------
// Katalog, Ledger-Abgleich & Pairing
// ----------------------------------------------------------------------

/// Trägt eigene Freigaben ohne Ledger-Eintrag nach (z. B. nach dem Update von Schema 1).
pub(crate) fn backfill_ledger(inner: &DaemonInner) -> Result<()> {
    let missing = inner.db.files_missing_entries(&inner.self_id())?;
    for (id, record) in &missing {
        inner.record_entry(*id, record, false)?;
    }
    if !missing.is_empty() {
        tracing::info!("Ledger: {} bestehende Freigabe(n) übernommen", missing.len());
    }
    Ok(())
}


// ----------------------------------------------------------------------
// Aufräumen (Papierkorb → Blob-Store)
// ----------------------------------------------------------------------

pub(crate) async fn run_pruner(inner: Arc<DaemonInner>) {
    let mut interval = tokio::time::interval(PRUNE_INTERVAL);
    loop {
        interval.tick().await;
        if let Err(e) = prune_once(&inner).await {
            tracing::error!("Aufräumen fehlgeschlagen: {e:#}");
        }
    }
}

pub(crate) async fn prune_once(inner: &DaemonInner) -> Result<()> {
    let orphaned = inner.db.purge_expired(inner.config.retention_days)?;
    for hash_hex in orphaned {
        let hash: Hash = match hash_hex.parse() {
            Ok(h) => h,
            Err(_) => continue,
        };
        inner.network.unpin_published(&hash).await?;
        tracing::info!("Endgültig gelöscht: {}", &hash_hex[..12]);
    }
    Ok(())
}

