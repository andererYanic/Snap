//! Iroh-Netzwerkschicht: Endpoint, Blob-Store, Zugriffskontrolle und Transfers.

use anyhow::{Context, Result};
use iroh::protocol::RouterBuilder;
use iroh::{
    endpoint::{presets, BindOpts},
    Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey,
};
use iroh_blobs::{
    provider::events::{AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage, RequestMode},
    store::{
        fs::{options::Options, FsStore},
        GcConfig,
    },
    ticket::BlobTicket,
    BlobFormat, BlobsProtocol, Hash, HashAndFormat,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Entscheidet, ob ein Hash an andere Peers ausgeliefert werden darf.
/// Erhält den Hash und – falls bekannt – die per TLS bestätigte Node-ID der Gegenseite.
pub type AccessCheck = Arc<dyn Fn(&Hash, Option<EndpointId>) -> bool + Send + Sync>;
/// Wird aufgerufen, wenn sich ein Peer bei uns verbindet (für den reaktiven Peer-Status).
pub type ConnectHook = Arc<dyn Fn(EndpointId) + Send + Sync>;

pub struct NetworkOptions {
    pub blob_store_path: PathBuf,
    pub secret_key: SecretKey,
    pub connect_timeout: Duration,
    pub bind_port: Option<u16>,
    pub use_relay: bool,
    /// Nur für Tests: an 127.0.0.1 binden.
    pub loopback_only: bool,
}

/// Fehlerarten beim Herunterladen – bestimmen, ob ein Auftrag in die
/// Warteschlange kommt oder sofort als Fehler gemeldet wird.
#[derive(Debug)]
pub enum FetchError {
    /// Verbindung zum Peer konnte nicht aufgebaut werden → Peer offline, später erneut versuchen.
    Unreachable(String),
    /// Verbindung brach während der Übertragung ab → später fortsetzen.
    Interrupted(String),
    /// Peer ist erreichbar, liefert die Datei aber nicht aus (nicht/nicht mehr freigegeben).
    Rejected(String),
    /// Lokaler Fehler (Store, Dateisystem).
    Local(anyhow::Error),
}

impl FetchError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, FetchError::Unreachable(_) | FetchError::Interrupted(_))
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Unreachable(e) => write!(f, "Peer nicht erreichbar: {e}"),
            FetchError::Interrupted(e) => write!(f, "Übertragung unterbrochen: {e}"),
            FetchError::Rejected(e) => {
                write!(f, "Peer liefert die Datei nicht aus (nicht oder nicht mehr freigegeben): {e}")
            }
            FetchError::Local(e) => write!(f, "Lokaler Fehler: {e:#}"),
        }
    }
}

pub struct P2pNetwork {
    endpoint: Endpoint,
    store: iroh_blobs::api::Store,
    router: iroh::protocol::Router,
    connect_timeout: Duration,
    use_relay: bool,
    /// Offene Blob-Verbindungen je Gerät – viele Dateien teilen sich eine Verbindung
    /// (über das Internet spart das pro Datei einen Verbindungsaufbau).
    conns: std::sync::Mutex<std::collections::HashMap<EndpointId, iroh::endpoint::Connection>>,
}

/// Tag-Präfix für freigegebene Dateien. Getaggte Blobs überleben die Garbage Collection.
const PUBLISH_TAG_PREFIX: &str = "snap/pub/";
/// Tags für den Streaming-Cache (auch unvollständige Blobs).
const STREAM_TAG_PREFIX: &str = "snap/stream/";

impl P2pNetwork {
    /// `extra_protocols` registriert zusätzliche Protokolle (Ledger, Pairing) am Router.
    pub async fn start(
        opts: NetworkOptions,
        access: AccessCheck,
        on_connect: ConnectHook,
        extra_protocols: impl FnOnce(RouterBuilder, &Endpoint) -> RouterBuilder,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(&opts.blob_store_path).await?;

        // Garbage Collection: entfernt Blobs ohne Tag (gelöschte Freigaben,
        // abgeschlossene/abgebrochene Downloads). Laufende Vorgänge sind durch Temp-Tags geschützt.
        let mut store_opts = Options::new(&opts.blob_store_path);
        store_opts.gc = Some(GcConfig { interval: Duration::from_secs(600), add_protected: None });
        let fs_store = FsStore::load_with_opts(opts.blob_store_path.join("blobs.db"), store_opts)
            .await
            .context("Blob-Store konnte nicht geöffnet werden")?;
        let store: iroh_blobs::api::Store = fs_store.into();

        let mut builder = if opts.use_relay {
            Endpoint::builder(presets::N0)
        } else {
            Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled)
        };
        builder = builder.secret_key(opts.secret_key);
        if opts.loopback_only {
            builder = builder.bind_addr(format!("127.0.0.1:{}", opts.bind_port.unwrap_or(0)).as_str())?;
        } else if let Some(port) = opts.bind_port {
            // IPv6 ist optional: viele Container/Hosts haben kein IPv6.
            builder = builder
                .bind_addr_with_opts(
                    format!("0.0.0.0:{port}").as_str(),
                    BindOpts::default().set_is_default_route(true),
                )?
                .bind_addr_with_opts(
                    format!("[::]:{port}").as_str(),
                    BindOpts::default().set_is_required(false).set_is_default_route(true),
                )?;
        }
        let endpoint = builder.bind().await.context("Iroh-Endpoint konnte nicht gestartet werden")?;

        let events = access_control(access, on_connect);
        let blobs_protocol = BlobsProtocol::new(&store, Some(events));
        let builder = iroh::protocol::Router::builder(endpoint.clone()).accept(iroh_blobs::ALPN, blobs_protocol);
        let router = extra_protocols(builder, &endpoint).spawn();

        Ok(Self {
            endpoint,
            store,
            router,
            connect_timeout: opts.connect_timeout,
            use_relay: opts.use_relay,
            conns: std::sync::Mutex::new(std::collections::HashMap::new()),
        })
    }

    pub fn node_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Eigene Adresse für Tickets. Mit Relais genügt die Node-ID (Adressauflösung
    /// über n0-DNS, keine IP-Adressen im Ticket). Ohne Relais müssen die direkten
    /// Adressen im Ticket stehen.
    fn ticket_addr(&self) -> EndpointAddr {
        if self.use_relay {
            EndpointAddr::from(self.endpoint.id())
        } else {
            self.endpoint.addr()
        }
    }

    /// Direkte Adressen dieses Nodes (als Hinweis für gekoppelte Geräte).
    pub fn direct_addrs(&self) -> Vec<String> {
        direct_addrs(&self.endpoint)
    }

    pub fn ticket_for(&self, hash: Hash) -> String {
        BlobTicket::new(self.ticket_addr(), hash, BlobFormat::Raw).to_string()
    }

    /// Importiert eine Datei in den Blob-Store und markiert sie als freigegeben.
    /// Gibt (Hash, Ticket) zurück. Die Datei wird gestreamt, nicht komplett in den RAM geladen.
    pub async fn publish_file(&self, abs_path: &Path) -> Result<(Hash, String)> {
        let temp = self.store.add_path(abs_path).temp_tag().await?;
        let hash = temp.hash();
        self.store.tags().set(format!("{PUBLISH_TAG_PREFIX}{hash}"), HashAndFormat::raw(hash)).await?;
        drop(temp);
        Ok((hash, self.ticket_for(hash)))
    }

    /// Importiert eine Datei **per Referenz** (keine Kopie im Blob-Store) und schützt
    /// sie vor der GC. Ändert sich die Datei später, erkennt das der Watcher; ein
    /// Empfänger würde veraltete Daten ohnehin an der BLAKE3-Prüfung ablehnen.
    pub async fn import_reference(&self, abs_path: &Path) -> Result<Hash> {
        use iroh_blobs::api::blobs::{AddPathOptions, ImportMode};
        let opts =
            AddPathOptions { path: abs_path.to_path_buf(), format: BlobFormat::Raw, mode: ImportMode::TryReference };
        let temp = self.store.add_path_with_opts(opts).temp_tag().await?;
        let hash = temp.hash();
        self.store.tags().set(format!("{PUBLISH_TAG_PREFIX}{hash}"), HashAndFormat::raw(hash)).await?;
        drop(temp);
        Ok(hash)
    }

    /// Hebt den Schutz eines freigegebenen Blobs auf; die GC entfernt ihn danach.
    pub async fn unpin_published(&self, hash: &Hash) -> Result<()> {
        self.store.tags().delete(format!("{PUBLISH_TAG_PREFIX}{hash}")).await?;
        Ok(())
    }

    /// Lädt einen Blob in den lokalen Store. Ist er schon (vollständig) vorhanden,
    /// wird nichts übertragen. Gibt ein Temp-Tag zurück, das den Blob bis zum Export schützt.
    pub async fn fetch(&self, ticket: &BlobTicket) -> Result<iroh_blobs::api::TempTag, FetchError> {
        let hash = ticket.hash();
        let content = HashAndFormat::raw(hash);
        // Schutz vor der GC schon während des Downloads (auch für Teilstücke).
        let temp = self.store.tags().temp_tag(content).await.map_err(|e| FetchError::Local(e.into()))?;

        if ticket.addr().id == self.endpoint.id() {
            // Eigene Datei: liegt bereits lokal vor.
            return if self.has_complete(hash).await {
                Ok(temp)
            } else {
                Err(FetchError::Rejected("Ticket zeigt auf diesen Node, Datei liegt aber nicht vor".into()))
            };
        }
        if self.has_complete(hash).await {
            return Ok(temp);
        }

        let peer = ticket.addr().id;
        let conn = self.connection(ticket.addr()).await?;
        let result = self.store.remote().fetch(conn.clone(), content).await;
        match result {
            Ok(_) => {
                if self.has_complete(hash).await {
                    Ok(temp)
                } else {
                    Err(FetchError::Rejected("Peer hat die Datei nicht vollständig geliefert".into()))
                }
            }
            Err(e) => {
                // Ist die Verbindung noch offen, hat der Peer die Anfrage abgelehnt.
                // Ist sie geschlossen, ist der Peer während der Übertragung verschwunden.
                let closed = conn.close_reason().is_some();
                if closed {
                    self.conns.lock().unwrap().remove(&peer);
                    Err(FetchError::Interrupted(e.to_string()))
                } else {
                    Err(FetchError::Rejected(e.to_string()))
                }
            }
        }
    }

    /// Wiederverwendbare Verbindung zu einem Gerät (neu aufgebaut, falls keine offen ist).
    async fn connection(&self, addr: &EndpointAddr) -> Result<iroh::endpoint::Connection, FetchError> {
        if let Some(c) = self.conns.lock().unwrap().get(&addr.id) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        let conn =
            match tokio::time::timeout(self.connect_timeout, self.endpoint.connect(addr.clone(), iroh_blobs::ALPN))
                .await
            {
                Ok(Ok(conn)) => conn,
                Ok(Err(e)) => return Err(FetchError::Unreachable(e.to_string())),
                Err(_) => {
                    return Err(FetchError::Unreachable(format!(
                        "Zeitüberschreitung nach {} s",
                        self.connect_timeout.as_secs()
                    )))
                }
            };
        let mut conns = self.conns.lock().unwrap();
        conns.retain(|_, c| c.close_reason().is_none());
        conns.insert(addr.id, conn.clone());
        Ok(conn)
    }

    /// Verifizierte Größe eines Blobs; lädt bei Bedarf nur das letzte Stück vom Gerät
    /// (BLAKE3/bao beweist die Größe).
    pub async fn stream_size(&self, addr: &EndpointAddr, hash: Hash) -> Result<u64, FetchError> {
        if let Some(s) = self.validated_size(hash).await {
            return Ok(s);
        }
        use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt};
        self.ensure_chunks(addr, hash, ChunkRanges::last_chunk()).await?;
        self.validated_size(hash)
            .await
            .ok_or_else(|| FetchError::Rejected("Gerät hat die Größe nicht nachgewiesen".into()))
    }

    async fn validated_size(&self, hash: Hash) -> Option<u64> {
        self.store.observe(hash).await.ok().and_then(|bf| bf.validated_size())
    }

    /// Stellt sicher, dass die Bytes `[start, end)` lokal (verifiziert) vorliegen; fehlende
    /// Stücke werden vom Gerät geholt. Bereits vorhandene Teile werden nicht erneut übertragen.
    pub async fn ensure_range(&self, addr: &EndpointAddr, hash: Hash, start: u64, end: u64) -> Result<(), FetchError> {
        use iroh_blobs::protocol::{ChunkRanges, ChunkRangesExt};
        if end <= start {
            return Ok(());
        }
        self.ensure_chunks(addr, hash, ChunkRanges::bytes(start..end)).await
    }

    async fn ensure_chunks(
        &self,
        addr: &EndpointAddr,
        hash: Hash,
        ranges: iroh_blobs::protocol::ChunkRanges,
    ) -> Result<(), FetchError> {
        use iroh_blobs::protocol::{ChunkRangesSeq, GetRequest};
        let request = GetRequest::new(hash, ChunkRangesSeq::from_ranges([ranges]));
        let local = self.store.remote().local_for_request(request).await.map_err(|e| FetchError::Local(e.into()))?;
        if local.is_complete() {
            return Ok(());
        }
        if addr.id == self.endpoint.id() {
            return Err(FetchError::Rejected("Daten liegen auf diesem Gerät nicht vor".into()));
        }
        let conn = self.connection(addr).await?;
        match self.store.remote().execute_get(conn.clone(), local.missing()).complete().await {
            Ok(_) => Ok(()),
            Err(e) => {
                if conn.close_reason().is_some() {
                    self.conns.lock().unwrap().remove(&addr.id);
                    Err(FetchError::Interrupted(e.to_string()))
                } else {
                    Err(FetchError::Rejected(e.to_string()))
                }
            }
        }
    }

    /// Liest lokal vorliegende, verifizierte Bytes `[start, end)`.
    pub async fn read_local(&self, hash: Hash, start: u64, end: u64) -> Result<Vec<u8>> {
        use iroh_blobs::api::proto::ExportRangesItem;
        use n0_future::StreamExt;
        let len = usize::try_from(end.saturating_sub(start))?;
        let mut out = vec![0u8; len];
        let mut filled = 0usize;
        let mut stream = std::pin::pin!(self.store.export_ranges(hash, start..end).stream());
        while let Some(item) = stream.next().await {
            match item {
                ExportRangesItem::Data(leaf) => {
                    let (ls, le) = (leaf.offset, leaf.offset + leaf.data.len() as u64);
                    let (a, b) = (ls.max(start), le.min(end));
                    if a < b {
                        out[(a - start) as usize..(b - start) as usize]
                            .copy_from_slice(&leaf.data[(a - ls) as usize..(b - ls) as usize]);
                        filled += (b - a) as usize;
                    }
                }
                ExportRangesItem::Size(_) => {}
                ExportRangesItem::Error(e) => anyhow::bail!("Lesen fehlgeschlagen: {e}"),
            }
        }
        anyhow::ensure!(filled == len, "Bereich liegt nicht vollständig vor ({filled}/{len} Bytes)");
        Ok(out)
    }

    /// Hält gestreamte (auch unvollständige) Blobs über die GC hinaus (Streaming-Cache).
    pub async fn tag_stream(&self, hash: Hash) -> Result<()> {
        self.store.tags().set(format!("{STREAM_TAG_PREFIX}{hash}"), HashAndFormat::raw(hash)).await?;
        Ok(())
    }

    /// Gibt einen gestreamten Blob für die GC frei.
    pub async fn untag_stream(&self, hash: Hash) -> Result<()> {
        self.store.tags().delete(format!("{STREAM_TAG_PREFIX}{hash}")).await?;
        Ok(())
    }

    /// Lokal vorliegende Bytes eines Blobs (für die Cache-Größe).
    pub async fn local_bytes(&self, hash: Hash) -> u64 {
        self.store.observe(hash).await.map(|bf| bf.total_bytes()).unwrap_or(0)
    }

    async fn has_complete(&self, hash: Hash) -> bool {
        self.store.has(hash).await.unwrap_or(false)
    }

    /// Exportiert einen vollständig vorliegenden Blob in eine Datei.
    pub async fn export(&self, hash: Hash, target: &Path) -> Result<()> {
        self.store.export(hash, target).await?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        if let Err(e) = self.router.shutdown().await {
            tracing::warn!("Fehler beim Beenden des Routers: {e}");
        }
        self.endpoint.close().await;
        // Der Router schließt den Store i. d. R. bereits mit; ein Fehler hier ist harmlos.
        if let Err(e) = self.store.shutdown().await {
            tracing::debug!("Blob-Store bereits geschlossen: {e}");
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

/// Direkte IP-Adressen eines Endpoints als Strings.
pub fn direct_addrs(endpoint: &Endpoint) -> Vec<String> {
    endpoint
        .addr()
        .addrs
        .iter()
        .filter_map(|a| match a {
            iroh::TransportAddr::Ip(sa) => Some(sa.to_string()),
            _ => None,
        })
        .collect()
}

/// Baut eine Endpoint-Adresse aus Node-ID und (optionalen) direkten Adress-Hinweisen.
pub fn endpoint_addr(id: EndpointId, hints: &[String]) -> EndpointAddr {
    let mut addr = EndpointAddr::from(id);
    for h in hints {
        if let Ok(sa) = h.parse::<std::net::SocketAddr>() {
            addr.addrs.insert(iroh::TransportAddr::Ip(sa));
        }
    }
    addr
}

/// Zugriffskontrolle für eingehende Blob-Anfragen:
/// - `get`: nur freigegebene, nicht gelöschte Hashes, nur einzelne Blobs (keine HashSeqs)
/// - `observe`: ebenso (verhindert das Ausspähen, welche Dateien wir besitzen)
/// - `get_many` und `push`: komplett deaktiviert
/// - eingehende Verbindungen: nur zur Status-Aktualisierung bekannter Peers
fn access_control(access: AccessCheck, on_connect: ConnectHook) -> EventSender {
    let mask = EventMask {
        // Intercept statt Notify: Die Zuordnung Verbindung -> Node-ID steht garantiert,
        // bevor die erste Anfrage dieser Verbindung geprüft wird.
        connected: ConnectMode::Intercept,
        get: RequestMode::Intercept,
        get_many: RequestMode::Disabled,
        push: RequestMode::Disabled,
        observe: ObserveMode::Intercept,
        ..EventMask::DEFAULT
    };
    let (tx, mut rx) = EventSender::channel(64, mask);
    tokio::spawn(async move {
        // connection_id -> Node-ID der Gegenseite (aus TLS)
        let mut peers: std::collections::HashMap<u64, EndpointId> = std::collections::HashMap::new();
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::ClientConnected(msg) => {
                    if let Some(id) = msg.endpoint_id {
                        peers.insert(msg.connection_id, id);
                        on_connect(id);
                    }
                    msg.tx.send(Ok(())).await.ok();
                }
                ProviderMessage::ConnectionClosed(msg) => {
                    peers.remove(&msg.connection_id);
                }
                ProviderMessage::GetRequestReceived(msg) => {
                    let node = peers.get(&msg.connection_id).copied();
                    let allowed = msg.request.ranges.is_blob() && access(&msg.request.hash, node);
                    if !allowed {
                        tracing::debug!("Get-Anfrage abgelehnt: {}", msg.request.hash.fmt_short());
                    }
                    let res = if allowed { Ok(()) } else { Err(AbortReason::Permission) };
                    msg.tx.send(res).await.ok();
                }
                ProviderMessage::ObserveRequestReceived(msg) => {
                    let node = peers.get(&msg.connection_id).copied();
                    let res = if access(&msg.request.hash, node) { Ok(()) } else { Err(AbortReason::Permission) };
                    msg.tx.send(res).await.ok();
                }
                _ => {}
            }
        }
    });
    tx
}
