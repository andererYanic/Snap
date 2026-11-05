//! IPC-Protokoll zwischen CLI und Daemon über einen Unix-Socket.
//!
//! Jede Nachricht ist ein JSON-Dokument mit vorangestellter Länge (u32, Big Endian).
//! Nachrichten über [`MAX_FRAME_LEN`] werden abgelehnt.

use crate::api::error::ErrorCode;
use crate::database::{AuditEntry, DeviceInfo, FileRecord, PeerInfo, PendingJob, RemoteFile};
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
    Publish { path: PathBuf },
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
    PairJoin { code: String },
    /// Gekoppelte Geräte auflisten.
    PairList,
    /// Gerät entkoppeln (Name oder Node-ID-Präfix).
    PairRemove { device: String },
    /// Einem gekoppelten Gerät ein Recht gewähren (z. B. "files.read" für einen Ordner).
    PairGrant { device: String, capability: String, folder: Option<PathBuf> },
    /// Ein Recht entziehen (`folder = None`: in allen Ordnern).
    PairRevoke { device: String, capability: String, folder: Option<PathBuf> },
    /// Audit-Log der Operationen fremder Geräte (neueste zuerst).
    Audit { limit: u32 },
    /// Vollständiges Ticket einer freigegebenen Datei abfragen.
    Ticket { hash: String },
    /// Offline-Warteschlange anzeigen.
    Queue,
    /// Job aus der Warteschlange entfernen.
    QueueCancel { id: i64 },
    /// Job sofort erneut versuchen (setzt auch das Backoff des Peers zurück).
    QueueRetry { id: i64 },
    /// Bekannte Peers mit Status anzeigen.
    Peers,
    /// Informationen über den laufenden Daemon.
    Info,
    /// Katalog seitenweise abfragen (für Oberflächen).
    Browse(BrowseQuery),
    /// Ereignisse abonnieren: Die Verbindung bleibt offen, der Daemon sendet
    /// fortlaufend `Response::Event`.
    Subscribe,
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
    Success { message: String },
    Published { ticket: String, hash: String, file_name: String, already_shared: bool },
    Downloaded { path: PathBuf },
    /// Peer nicht erreichbar – Auftrag liegt in der Warteschlange.
    Queued { job_id: i64, peer: String, reason: String, already_queued: bool },
    Ticket { ticket: String },
    Catalog {
        local: Vec<FileRecord>,
        remote: Vec<RemoteFile>,
        devices: Vec<DeviceInfo>,
        peers: Vec<PeerInfo>,
        /// Ein Abgleich läuft gerade im Hintergrund.
        syncing: bool,
    },
    SyncReport { results: Vec<SyncResult> },
    PairInvite { code: String, expires_at: i64 },
    Paired { device: DeviceInfo },
    Devices { devices: Vec<DeviceInfo> },
    Audit { entries: Vec<AuditEntry> },
    BrowsePage {
        items: Vec<CatalogItem>,
        total: u64,
        /// Cursor für die nächste Seite (`None` = Ende erreicht).
        next: Option<BrowseCursor>,
    },
    Event(crate::events::Event),
    Queue { jobs: Vec<PendingJob> },
    Peers { peers: Vec<PeerInfo> },
    Info(DaemonInfo),
    Error { code: ErrorCode, message: String },
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
