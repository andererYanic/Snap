//! Der Daemon: Start/Stopp, IPC-Server und Hintergrund-Tasks.
//! Die Operationen selbst liegen in `api`, Warteschlange in `worker`, Abgleich in `sync`.

use crate::api::handle_request;
use crate::sync::run_periodic_sync;
use crate::worker::run_pending_pin_worker;

use crate::api::error::ErrorCode;
use crate::config::Config;
use crate::database::DatabaseManager;
use crate::ipc::{read_frame, write_frame, Envelope, Request, Response, API_VERSION};
use crate::ledger::{LedgerCtx, LedgerEntry, LedgerProtocol, LEDGER_ALPN};
use crate::network::{AccessCheck, ConnectHook, NetworkOptions, P2pNetwork};
use crate::pairing::{PairingCtx, PairingProtocol, PairingState, PAIR_ALPN};
use crate::secure_fs::{current_uid, ensure_private_dir, load_or_create_secret_key};
use anyhow::{bail, Context, Result};
use iroh::SecretKey;
use iroh_blobs::Hash;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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
    /// Aktive Watcher je Freigabe (Fallenlassen beendet die Beobachtung).
    pub(crate) watchers: Mutex<std::collections::HashMap<i64, notify::RecommendedWatcher>>,
    /// Pro Freigabe höchstens ein Scan gleichzeitig.
    pub(crate) scan_locks: Mutex<std::collections::HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) live: Arc<crate::live::LiveManager>,
    /// Löst einen Pin-Abgleich aus (z. B. nach `pin add`).
    pub(crate) pin_notify: Arc<Notify>,
    /// Lokale Suche (`None` = in der Konfiguration abgeschaltet).
    pub(crate) index: Option<Arc<crate::indexer::IndexService>>,
    /// Dateioperationen (Aufträge, Rückgängig, Papierkorb).
    pub(crate) files: Arc<crate::fileops::jobs::FileJobs>,
    /// Stundenfenster und Parallelität für Fernoperationen.
    pub(crate) remote_ops: crate::remote_ops::RemoteOpsState,
    /// Vorschaubilder (Cache im Datenordner).
    pub(crate) thumbs: crate::media::ThumbCache,
    /// Lokaler Streaming-Server (`None`, falls der Port nicht gebunden werden konnte).
    pub(crate) stream: Option<Arc<crate::stream::StreamServer>>,
    /// Zur Laufzeit änderbare Grenzen.
    pub(crate) settings: Arc<crate::settings::Settings>,
    /// Zum Hochladen angebotene Inhalte (je genau ein Zielgerät).
    pub(crate) offers: Arc<crate::upload::UploadOffers>,
    /// Weckt die Upload-Warteschlange.
    pub(crate) upload_notify: Arc<Notify>,
    /// Weckt die Upload-Regeln (Watcher-Ereignis).
    pub(crate) rules_notify: Arc<Notify>,
    pub(crate) upload_watchers: Mutex<std::collections::HashMap<i64, notify::RecommendedWatcher>>,
    pub(crate) backups: crate::backup::BackupService,
}

impl DaemonInner {
    /// Liest geänderte Ordner sofort in den Suchindex ein (ohne auf den Watcher zu warten).
    pub(crate) async fn refresh_index(&self, dirs: Vec<PathBuf>) {
        if let (Some(svc), false) = (self.index.clone(), dirs.is_empty()) {
            let _ = tokio::task::spawn_blocking(move || svc.refresh_dirs(dirs)).await;
        }
    }
}

impl DaemonInner {
    /// Pfade, die über Rechte nie erreichbar sind (Snap-Daten inkl. Schlüssel, Socket).
    pub(crate) fn protected_paths(&self) -> Vec<PathBuf> {
        let mut p = vec![self.config.data_dir.clone()];
        if let Some(dir) = self.config.socket_path.parent() {
            p.push(dir.to_path_buf());
        }
        p
    }

    pub(crate) fn self_id(&self) -> String {
        self.network.node_id().to_string()
    }

    /// Schreibt einen eigenen, signierten Ledger-Eintrag für eine Datei.
    pub(crate) fn record_entry(
        &self,
        file_id: i64,
        record: &crate::database::FileRecord,
        deleted: bool,
    ) -> Result<LedgerEntry> {
        let key = self.secret_key.clone();
        self.db.record_own_entry(
            &self.self_id(),
            file_id,
            &record.blake3_hash,
            &record.file_name,
            record.file_size,
            record.created_at,
            deleted,
            // Freigabe und Pfad nur gemeinsam (sonst wäre der Eintrag ungültig).
            record.share.as_deref().filter(|_| record.rel_path.is_some()).unwrap_or(""),
            record.rel_path.as_deref().filter(|_| record.share.is_some()).unwrap_or(""),
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
        let socket_dir = config.socket_path.parent().context("Ungültiger Socket-Pfad")?.to_path_buf();
        ensure_private_dir(&socket_dir)?;
        prepare_socket_path(&config.socket_path).await?;

        let db = Arc::new(DatabaseManager::open(&config.db_path())?);
        let secret_key = load_or_create_secret_key(&config.secret_key_path())?;
        let sync_notifier = Arc::new(Notify::new());

        // Blob-Zugriff: Einzeln veröffentlicht -> Ticket reicht. Share-Datei -> nur mit
        // files.read auf ihren Pfad (gleiche Prüfstelle wie alle Operationen).
        let offers = Arc::new(crate::upload::UploadOffers::default());
        let access: AccessCheck = {
            let db = db.clone();
            let offers = offers.clone();
            let protected = vec![config.data_dir.clone(), socket_dir.clone()];
            Arc::new(move |hash: &Hash, node: Option<iroh::EndpointId>| {
                // Zum Hochladen angeboten – nur genau diesem Gerät.
                if offers.allows(hash, node) {
                    return true;
                }
                let Ok(acc) = db.blob_access(&hash.to_string()) else { return false };
                if acc.published {
                    return true;
                }
                let Some(node) = node else { return false };
                let caller = crate::api::auth::Caller::Device(node.to_string());
                acc.share_paths.iter().any(|p| {
                    crate::api::auth::authorize(
                        &db,
                        &protected,
                        &caller,
                        crate::api::auth::Capability::FilesRead,
                        Some(Path::new(p)),
                        "blob.get",
                    )
                    .is_ok()
                })
            })
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
        let mut live_slot: Option<Arc<crate::live::LiveManager>> = None;
        let ops_slot: Arc<std::sync::OnceLock<std::sync::Weak<DaemonInner>>> = Arc::default();
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
                let live =
                    crate::live::LiveManager::new(ledger.clone(), Duration::from_secs(config.connect_timeout_secs));
                ledger_slot = Some(ledger.clone());
                pairing_slot = Some(pairing.clone());
                live_slot = Some(live.clone());
                builder
                    .accept(LEDGER_ALPN, LedgerProtocol { ctx: ledger })
                    .accept(PAIR_ALPN, PairingProtocol { ctx: pairing })
                    .accept(crate::live::LIVE_ALPN, crate::live::LiveProtocol { mgr: live })
                    .accept(crate::remote_ops::OPS_ALPN, crate::remote_ops::OpsProtocol { inner: ops_slot.clone() })
                    .accept(
                        crate::remote_thumbs::THUMB_ALPN,
                        crate::remote_thumbs::ThumbProtocol {
                            inner: ops_slot.clone(),
                            slots: Arc::new(tokio::sync::Semaphore::new(4)),
                        },
                    )
            },
        )
        .await?;
        let ledger = ledger_slot.context("Ledger-Protokoll nicht initialisiert")?;
        let pairing = pairing_slot.context("Pairing-Protokoll nicht initialisiert")?;
        let live = live_slot.context("Live-Protokoll nicht initialisiert")?;

        let listener = UnixListener::bind(&config.socket_path)
            .with_context(|| format!("IPC-Socket {} nicht anlegbar", config.socket_path.display()))?;
        std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o600))?;

        let index = if config.index_enabled {
            let excluded = vec![config.data_dir.clone(), socket_dir.clone()];
            let idx = crate::index::LocalIndex::open(&config.index_path(), excluded)?;
            Some(Arc::new(crate::indexer::IndexService::new(idx, config.index_max_watches)))
        } else {
            None
        };

        let config_trash_dir = config.trash_dir.clone();
        let (stream, stream_listener) = match crate::stream::StreamServer::bind(config.stream_port).await {
            Ok((s, l)) => (Some(s), Some(l)),
            Err(e) => {
                tracing::warn!("Streaming-Server nicht gestartet (Port {}): {e}", config.stream_port);
                (None, None)
            }
        };
        let settings = Arc::new(crate::settings::Settings::load(db.clone(), &config)?);
        let thumbs = crate::media::ThumbCache::new(config.thumb_dir(), settings.thumb_cache_bytes());
        let inner = Arc::new(DaemonInner {
            config,
            network,
            db,
            sync_notifier,
            secret_key,
            ledger,
            pairing,
            sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            watchers: Mutex::new(std::collections::HashMap::new()),
            scan_locks: Mutex::new(std::collections::HashMap::new()),
            live,
            pin_notify: Arc::new(Notify::new()),
            files: Arc::new(crate::fileops::jobs::FileJobs::new(crate::fileops::trash::Trash::new(config_trash_dir))),
            index,
            remote_ops: Default::default(),
            thumbs,
            stream: stream.clone(),
            settings,
            offers,
            upload_notify: Arc::new(Notify::new()),
            rules_notify: Arc::new(Notify::new()),
            upload_watchers: Mutex::new(std::collections::HashMap::new()),
            backups: Default::default(),
        });
        let _ = ops_slot.set(Arc::downgrade(&inner));
        if inner.config.live_connections {
            inner.live.start();
        }
        backfill_ledger(&inner)?;
        let daemon = Self { inner: inner.clone(), tasks: Arc::new(Mutex::new(Vec::new())) };

        let ipc = tokio::spawn(run_ipc_server(inner.clone(), listener));
        let worker = tokio::spawn(run_pending_pin_worker(inner.clone()));
        let pruner = tokio::spawn(run_pruner(inner.clone()));
        let periodic = tokio::spawn(run_periodic_sync(inner.clone()));
        let shares = tokio::spawn(start_shares(inner.clone()));
        let pins = tokio::spawn(crate::pins::run_pin_loop(inner.clone()));
        let indexer = tokio::spawn(crate::indexer::run_indexer(inner.clone()));
        let uploads = tokio::spawn(crate::upload::run_worker(inner.clone()));
        let links = tokio::spawn(crate::links::run_cleanup(inner.clone()));
        let upload_rules = tokio::spawn(crate::upload::run_rules(inner.clone()));
        let streamer = match (stream, stream_listener) {
            (Some(s), Some(l)) => tokio::spawn(crate::stream::serve(s, l, Arc::downgrade(&inner))),
            _ => tokio::spawn(async {}),
        };
        let stream_cache = tokio::spawn({
            let inner = inner.clone();
            async move {
                loop {
                    if let Err(e) =
                        crate::stream::enforce_cache_limit(&inner, inner.settings.stream_cache_bytes()).await
                    {
                        tracing::debug!("Streaming-Cache nicht aufgeräumt: {e:#}");
                    }
                    tokio::time::sleep(Duration::from_secs(600)).await;
                }
            }
        });
        {
            // Vorschaubild-Cache beim Start auf die eingestellte Größe bringen.
            let (dir, max) = (inner.config.thumb_dir(), inner.settings.thumb_cache_bytes());
            tokio::task::spawn_blocking(move || crate::media::prune(&dir, max));
        }
        daemon.tasks.lock().unwrap().extend([
            ipc,
            worker,
            pruner,
            periodic,
            shares,
            pins,
            indexer,
            streamer,
            stream_cache,
            uploads,
            upload_rules,
            links,
        ]);

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
        handle_request(&self.inner, &crate::api::auth::Caller::Local, req).await
    }

    /// Verarbeitet eine Anfrage im Namen eines bestimmten Aufrufers (z. B. eines
    /// gekoppelten Geräts). Alle Rechte werden geprüft.
    pub async fn request_as(&self, caller: &crate::api::auth::Caller, req: Request) -> Response {
        handle_request(&self.inner, caller, req).await
    }

    /// Anzahl aktuell live verbundener Geräte.
    pub fn live_peers(&self) -> usize {
        self.inner.live.connected_count()
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
        self.inner.live.shutdown();
        self.inner.watchers.lock().unwrap().clear();
        self.inner.upload_watchers.lock().unwrap().clear();
        if let Some(index) = &self.inner.index {
            index.stop();
        }
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

pub(crate) async fn handle_ipc_connection(inner: &Arc<DaemonInner>, mut stream: UnixStream) -> Result<()> {
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
    // Über den Socket sprechen nur Prozesse desselben Benutzers (SO_PEERCRED oben).
    let res = handle_request(inner, &crate::api::auth::Caller::Local, envelope.req).await;
    let size = serde_json::to_vec(&res).map(|b| b.len()).unwrap_or(usize::MAX);
    if size > crate::ipc::MAX_FRAME_LEN as usize {
        // Z. B. sehr lange Pfade: lieber ein klarer Fehler als eine abgebrochene Verbindung.
        let err = Response::error(ErrorCode::InvalidInput, "Antwort zu groß – bitte eine kleinere Seite anfordern.");
        return write_frame(&mut stream, &err).await;
    }
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

/// Startet Beobachtung und Abgleich aller Freigaben (Änderungen während der Daemon
/// aus war, werden so nachgeholt).
pub(crate) async fn start_shares(inner: Arc<DaemonInner>) {
    let shares = match inner.db.list_shares() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Freigaben nicht lesbar: {e:#}");
            return;
        }
    };
    for share in shares {
        start_share(&inner, share).await;
    }
}

/// Beobachtet eine Freigabe und scannt sie einmal vollständig.
pub(crate) async fn start_share(inner: &Arc<DaemonInner>, share: crate::database::ShareInfo) {
    match crate::shares::watch_share(inner.clone(), share.clone()) {
        Ok(w) => {
            inner.watchers.lock().unwrap().insert(share.id, w);
        }
        Err(e) => tracing::warn!("Freigabe '{}' kann nicht beobachtet werden: {e:#} – nur manuelle Scans", share.name),
    }
    match crate::shares::scan_share(inner, &share, None).await {
        Ok(st) if st.added_or_changed + st.deleted > 0 => {
            tracing::info!("Freigabe '{}': {} neu/geändert, {} gelöscht", share.name, st.added_or_changed, st.deleted)
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("Scan von '{}' fehlgeschlagen: {e:#}", share.name),
    }
}
