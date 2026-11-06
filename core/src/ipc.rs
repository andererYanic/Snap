//! IPC-Protokoll zwischen CLI und Daemon über einen Unix-Socket.
//!
//! Jede Nachricht ist ein JSON-Dokument mit vorangestellter Länge (u32, Big Endian).
//! Nachrichten über [`MAX_FRAME_LEN`] werden abgelehnt.

use crate::api::error::ErrorCode;
use crate::database::{AuditEntry, DeviceInfo, FileRecord, PeerInfo, PendingJob, RemoteFile, ShareInfo};
use anyhow::{bail, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximale Größe einer IPC-Nachricht (1 MiB).
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

/// Version des API-Vertrags. Bei inkompatiblen Änderungen erhöhen.
pub const API_VERSION: u32 = 1;

/// Jede Anfrage über den Socket ist mit der API-Version umhüllt.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Envelope {
    pub v: u32,
    pub req: Request,
}

impl Envelope {
    pub fn new(req: Request) -> Self {
        Self { v: API_VERSION, req }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ScanResult {
    pub name: String,
    pub added_or_changed: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub skipped: usize,
}

/// Ein Pin mit Fortschritt.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PinInfo {
    pub id: i64,
    pub origin: String,
    pub origin_name: Option<String>,
    pub share: String,
    pub prefix: String,
    /// Lokaler Ordner der Spiegelung.
    pub path: PathBuf,
    pub files_ok: u64,
    pub files_pending: u64,
    pub conflicts: u64,
    pub bytes: u64,
}

/// Seitenweise Abfrage des Katalogs (für Oberflächen).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct BrowseQuery {
    /// `None` = alle Geräte, `Some("self")` = dieses Gerät, sonst Node-ID.
    pub device: Option<String>,
    /// Teilstring im Dateinamen (ohne Groß-/Kleinschreibung).
    pub search: Option<String>,
    /// Für einfache Fälle. Bei großen Katalogen besser `after` verwenden (konstant schnell).
    pub offset: u32,
    /// Maximal [`MAX_BROWSE_LIMIT`]; 0 = Standard (100).
    pub limit: u32,
    /// Fortsetzung hinter diesem Eintrag (Keyset-Pagination, `next` der vorigen Seite).
    #[serde(default)]
    pub after: Option<BrowseCursor>,
}

/// Position im Katalog: Sortierschlüssel des letzten Eintrags einer Seite.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BrowseCursor {
    pub name: String,
    /// Leer = dieses Gerät.
    pub origin: String,
    pub hash: String,
}

pub const MAX_BROWSE_LIMIT: u32 = 500;

/// Ein Eintrag im Katalog, egal ob lokal oder auf einem anderen Gerät.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CatalogItem {
    /// `None` = dieses Gerät.
    pub origin: Option<String>,
    pub origin_name: Option<String>,
    pub hash: String,
    pub name: String,
    pub size: u64,
    pub created_at: i64,
    /// Lokaler Pfad (nur bei eigenen Dateien).
    pub path: Option<String>,
    /// Ordner-Freigabe (`None` = einzeln veröffentlicht).
    #[serde(default)]
    pub share: Option<String>,
    /// Pfad relativ zur Freigabe.
    #[serde(default)]
    pub rel_path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Request {
    /// Datei freigeben. `path` muss absolut sein (die CLI löst relative Pfade auf).
    Publish {
        path: PathBuf,
    },
    /// Datei herunterladen. `target` ist ein Ticket oder ein Hash(-Präfix) aus dem Katalog.
    Download {
        target: String,
        output: Option<PathBuf>,
        force: bool,
    },
    /// Datei(en) in den Papierkorb verschieben (Soft-Delete). Wird sofort nicht mehr ausgeliefert.
    Delete {
        hash: Option<String>,
        path: Option<PathBuf>,
    },
    /// Katalog anzeigen: eigene Freigaben und Dateien gekoppelter Geräte.
    /// Ist der Stand älter als 5 Minuten, wird im Hintergrund abgeglichen.
    List,
    /// Sofort mit allen gekoppelten Geräten abgleichen.
    Sync,
    /// Einladungscode für ein neues Gerät erzeugen.
    PairInvite,
    /// Warten, bis die aktuelle Einladung angenommen wurde (oder abläuft).
    PairInviteWait,
    /// Mit einem Einladungscode koppeln.
    PairJoin {
        code: String,
    },
    /// Gekoppelte Geräte auflisten.
    PairList,
    /// Gerät entkoppeln (Name oder Node-ID-Präfix).
    PairRemove {
        device: String,
    },
    /// Einem gekoppelten Gerät ein Recht gewähren (z. B. "files.read" für einen Ordner).
    PairGrant {
        device: String,
        capability: String,
        folder: Option<PathBuf>,
    },
    /// Ein Recht entziehen (`folder = None`: in allen Ordnern).
    PairRevoke {
        device: String,
        capability: String,
        folder: Option<PathBuf>,
    },
    /// Audit-Log der Operationen fremder Geräte (neueste zuerst).
    Audit {
        limit: u32,
    },
    /// Ordner freigeben. `devices = None`: alle gekoppelten (auch künftige) Geräte.
    ShareAdd {
        path: PathBuf,
        name: Option<String>,
        devices: Option<Vec<String>>,
    },
    ShareList,
    ShareRemove {
        name: String,
    },
    /// Freigabe(n) sofort neu scannen (`None` = alle).
    ShareRescan {
        name: Option<String>,
    },
    /// Freigabe (oder Unterordner) eines anderen Geräts offline verfügbar machen:
    /// "Fotos", "Fotos/2024" oder eindeutig "PC:Fotos/2024".
    PinAdd {
        target: String,
    },
    PinList,
    /// Pin entfernen (die bereits gespiegelten Dateien bleiben liegen).
    PinRemove {
        id: i64,
    },
    /// Vollständiges Ticket einer freigegebenen Datei abfragen.
    Ticket {
        hash: String,
    },
    /// Offline-Warteschlange anzeigen.
    Queue,
    /// Job aus der Warteschlange entfernen.
    QueueCancel {
        id: i64,
    },
    /// Job sofort erneut versuchen (setzt auch das Backoff des Peers zurück).
    QueueRetry {
        id: i64,
    },
    /// Bekannte Peers mit Status anzeigen.
    Peers,
    /// Informationen über den laufenden Daemon.
    Info,
    /// Katalog seitenweise abfragen (für Oberflächen).
    Browse(BrowseQuery),
    /// Ereignisse abonnieren: Die Verbindung bleibt offen, der Daemon sendet
    /// fortlaufend `Response::Event`.
    Subscribe,
    /// Lokale Suche in den indexierten Orten (Name enthält `query`). Nur lokal.
    Search {
        query: String,
        #[serde(default)]
        filter: crate::index::SearchFilter,
        limit: u32,
    },
    /// Ordnerinhalt live lesen (Explorer), Ordner zuerst, seitenweise. Nur lokal.
    ListDir {
        path: PathBuf,
        #[serde(default)]
        show_hidden: bool,
        #[serde(default)]
        offset: u32,
        limit: u32,
    },
    /// Indexierte Orte anzeigen.
    IndexRoots,
    /// Ordner zur Suche hinzufügen (oder wieder aktivieren).
    IndexAdd {
        path: PathBuf,
    },
    /// Ort aus der Suche entfernen (bleibt deaktiviert, auch für erkannte Laufwerke).
    IndexRemove {
        id: i64,
    },
    /// Alle Orte sofort abgleichen.
    IndexRescan,
    /// Dateioperation ausführen. `wait = false`: sofort `FileOpStarted`, Fortschritt und Ende
    /// als Ereignisse; `wait = true`: Antwort erst nach Abschluss.
    FileOp {
        op: crate::fileops::jobs::FileOp,
        #[serde(default)]
        wait: bool,
    },
    /// Laufende Dateioperation abbrechen.
    FileOpCancel {
        job: u64,
    },
    /// Letzte Dateioperation rückgängig machen.
    Undo,
    TrashList,
    /// Eintrag wiederherstellen; `keep_both`: bei belegtem Ziel freien Namen wählen.
    TrashRestore {
        id: String,
        #[serde(default)]
        keep_both: bool,
    },
    /// Eintrag endgültig löschen.
    TrashPurge {
        id: String,
    },
    /// Papierkorb leeren (endgültig).
    TrashEmpty,
    /// Dateioperation auf einem gekoppelten Gerät ausführen (in dessen Freigaben).
    RemoteOp {
        device: String,
        op: crate::remote_ops::RemoteOp,
    },
    /// Angehaltene Fernoperationen anderer Geräte (Massenschutz).
    OpsPending,
    OpsApprove {
        id: i64,
    },
    OpsReject {
        id: i64,
    },
    /// Vorschaubild (JPEG im lokalen Cache) für eine Bilddatei.
    Thumbnail {
        path: PathBuf,
        #[serde(default)]
        size: crate::media::ThumbSize,
    },
    /// Metadaten (EXIF, Audio-/Video-Tags, erkannte Art).
    Metadata {
        path: PathBuf,
    },
    /// Lokale Streaming-Adresse (http://127.0.0.1:…) für eine Datei – auch von anderen Geräten.
    /// Ziel wie bei `Download`: Ticket, Hash-Präfix, Name oder Freigabe/Pfad.
    StreamUrl {
        target: String,
    },
    /// Vorschaubild einer Datei in der Freigabe eines gekoppelten Geräts (braucht dort files.read).
    RemoteThumbnail {
        device: String,
        share: String,
        path: String,
        #[serde(default)]
        size: crate::media::ThumbSize,
    },
    /// Einstellungen (Grenzen) mit aktuellen Werten und Standardwerten.
    Settings,
    /// Einstellung ändern (`value = None`: zurück zum Standard aus config.toml). Gilt sofort.
    SettingSet {
        key: String,
        value: Option<i64>,
    },
    /// Datei auf ein gekoppeltes Gerät hochladen (Warteschlange; wird nachgeholt, wenn es offline ist).
    Upload {
        device: String,
        share: String,
        #[serde(default)]
        dest_dir: String,
        path: PathBuf,
    },
    UploadList,
    UploadCancel {
        id: i64,
    },
    /// Wartende und fehlgeschlagene Uploads sofort erneut versuchen.
    UploadRetry,
    /// Ordner beobachten und neue Dateien automatisch hochladen (z. B. Kamera).
    UploadRuleAdd {
        path: PathBuf,
        device: String,
        share: String,
        #[serde(default)]
        dest_dir: String,
    },
    UploadRuleList,
    UploadRuleRemove {
        id: i64,
    },
    /// Freigabelinks einrichten (Cloudflare R2 oder anderer S3-Speicher + Worker-Adresse).
    LinkSetup(crate::links::LinkSettings),
    /// Link für eine Datei erstellen (verschlüsselt hochladen). `days`/`max_downloads`/`password` optional.
    LinkCreate {
        path: PathBuf,
        days: Option<u32>,
        max_downloads: Option<u32>,
        password: Option<String>,
    },
    LinkList,
    /// Link widerrufen (ID-Präfix); löscht die verschlüsselte Kopie.
    LinkRevoke {
        id: String,
    },
    /// Speicheranalyse eines indexierten Ordners (Unterordner, Dateitypen, größte Dateien).
    SpaceUsage {
        path: PathBuf,
        #[serde(default)]
        limit: u32,
    },
    /// Doppelte Dateien finden (`path = None`: alle indexierten Orte). Nur lesend.
    Duplicates {
        path: Option<PathBuf>,
        min_size: Option<u64>,
        #[serde(default)]
        limit: u32,
    },
}

/// Eine angehaltene Fernoperation, die auf Bestätigung wartet.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ApprovalInfo {
    pub id: i64,
    pub node_id: String,
    pub device_name: Option<String>,
    pub summary: String,
    pub created_at: i64,
}

/// Ein Eintrag beim Lesen eines Ordners.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DirEntryInfo {
    pub name: String,
    pub kind: crate::index::NodeKind,
    /// Bytes (nur Dateien).
    pub size: u64,
    pub mtime: i64,
    pub hidden: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DaemonInfo {
    pub node_id: String,
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    pub download_dir: PathBuf,
    pub shared_files: u64,
    pub pending_jobs: u64,
    pub use_relay: bool,
    pub device_name: String,
    pub paired_devices: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SyncResult {
    pub node_id: String,
    pub name: String,
    pub ok: bool,
    pub new_entries: usize,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Response {
    Success {
        message: String,
    },
    Published {
        ticket: String,
        hash: String,
        file_name: String,
        already_shared: bool,
    },
    Downloaded {
        path: PathBuf,
    },
    /// Peer nicht erreichbar – Auftrag liegt in der Warteschlange.
    Queued {
        job_id: i64,
        peer: String,
        reason: String,
        already_queued: bool,
    },
    Ticket {
        ticket: String,
    },
    Catalog {
        local: Vec<FileRecord>,
        remote: Vec<RemoteFile>,
        devices: Vec<DeviceInfo>,
        peers: Vec<PeerInfo>,
        /// Ein Abgleich läuft gerade im Hintergrund.
        syncing: bool,
    },
    SyncReport {
        results: Vec<SyncResult>,
    },
    PairInvite {
        code: String,
        expires_at: i64,
    },
    Paired {
        device: DeviceInfo,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    Audit {
        entries: Vec<AuditEntry>,
    },
    Shares {
        shares: Vec<ShareInfo>,
    },
    ShareAdded {
        share: ShareInfo,
        granted: Vec<String>,
    },
    Scanned {
        results: Vec<ScanResult>,
    },
    Pins {
        pins: Vec<PinInfo>,
        pin_dir: PathBuf,
        used_bytes: u64,
        max_bytes: u64,
    },
    PinAdded {
        pin: PinInfo,
        queued: usize,
    },
    BrowsePage {
        items: Vec<CatalogItem>,
        total: u64,
        /// Cursor für die nächste Seite (`None` = Ende erreicht).
        next: Option<BrowseCursor>,
    },
    Event(crate::events::Event),
    Queue {
        jobs: Vec<PendingJob>,
    },
    Peers {
        peers: Vec<PeerInfo>,
    },
    Info(DaemonInfo),
    SearchResults {
        hits: Vec<crate::index::SearchHit>,
    },
    DirListing {
        path: PathBuf,
        entries: Vec<DirEntryInfo>,
        /// Gesamtzahl (nach Filter „versteckt“), für die Seitenanzeige.
        total: u64,
    },
    FileOpStarted {
        job: u64,
    },
    Approvals {
        items: Vec<ApprovalInfo>,
    },
    Thumbnail(crate::media::Thumbnail),
    StreamUrl(crate::stream::StreamInfo),
    Settings {
        items: Vec<crate::settings::SettingInfo>,
    },
    Uploads {
        items: Vec<crate::database::UploadRow>,
    },
    Link(crate::links::LinkInfo),
    SpaceUsage(crate::analysis::SpaceUsage),
    Duplicates(crate::analysis::DuplicateReport),
    Links {
        items: Vec<crate::links::LinkInfo>,
        /// Worker-Adresse (`None` = noch nicht eingerichtet).
        base_url: Option<String>,
    },
    UploadRules {
        rules: Vec<crate::database::UploadRule>,
    },
    Metadata(crate::media::MediaInfo),
    FileOpDone {
        job: u64,
        outcome: crate::fileops::jobs::Outcome,
    },
    Trash {
        entries: Vec<crate::fileops::trash::TrashEntry>,
    },
    IndexRoots {
        roots: Vec<crate::index::IndexRoot>,
        /// Beobachtete Ordner (Änderungen sofort sichtbar); der Rest per periodischem Abgleich.
        watched_dirs: u64,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

impl Response {
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Response::Error { code, message: message.into() }
    }

    /// Wandelt einen Fehler einer Operation in eine Antwort mit Fehlercode um.
    pub fn from_error(e: &anyhow::Error) -> Self {
        Response::Error { code: crate::api::error::code_of(e), message: format!("{e:#}") }
    }
}

/// Schreibt eine Nachricht mit Längenpräfix (maximal [`MAX_FRAME_LEN`]).
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    write_frame_max(writer, value, MAX_FRAME_LEN).await
}

/// Schreibt eine Nachricht mit Längenpräfix und eigenem Größenlimit.
pub async fn write_frame_max<W, T>(writer: &mut W, value: &T, max: u32) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > max as usize {
        bail!("IPC-Nachricht zu groß ({} Byte)", bytes.len());
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Liest eine Nachricht mit Längenpräfix (maximal [`MAX_FRAME_LEN`]).
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    read_frame_max(reader, MAX_FRAME_LEN).await
}

/// Liest eine Nachricht mit Längenpräfix und eigenem Größenlimit.
pub async fn read_frame_max<R, T>(reader: &mut R, max: u32) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let len = reader.read_u32().await?;
    if len > max {
        bail!("Nachricht zu groß ({len} Byte, maximal {max})");
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(serde_json::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(64 * 1024);
        let req = Request::Download { target: "abc".into(), output: None, force: true };
        write_frame(&mut a, &req).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert!(matches!(got, Request::Download { force: true, .. }));
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_without_allocation() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_u32(MAX_FRAME_LEN + 1).await.unwrap();
        let res: Result<Request> = read_frame(&mut b).await;
        assert!(res.is_err());
    }
}
