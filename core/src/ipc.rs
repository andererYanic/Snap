//! IPC-Protokoll zwischen CLI und Daemon über einen Unix-Socket.
//!
//! Jede Nachricht ist ein JSON-Dokument mit vorangestellter Länge (u32, Big Endian).
//! Nachrichten über [`MAX_FRAME_LEN`] werden abgelehnt.

use crate::database::{FileRecord, PeerInfo, PendingJob};
use anyhow::{bail, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximale Größe einer IPC-Nachricht (1 MiB).
pub const MAX_FRAME_LEN: u32 = 1024 * 1024;

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
    /// Freigegebene Dateien auflisten.
    List,
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
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Response {
    Success { message: String },
    Published { ticket: String, hash: String, file_name: String, already_shared: bool },
    Downloaded { path: PathBuf },
    /// Peer nicht erreichbar – Auftrag liegt in der Warteschlange.
    Queued { job_id: i64, peer: String, reason: String, already_queued: bool },
    Ticket { ticket: String },
    FileList { files: Vec<FileRecord> },
    Queue { jobs: Vec<PendingJob> },
    Peers { peers: Vec<PeerInfo> },
    Info(DaemonInfo),
    Error { message: String },
}

impl Response {
    pub fn error(message: impl Into<String>) -> Self {
        Response::Error { message: message.into() }
    }
}

/// Schreibt eine Nachricht mit Längenpräfix.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_LEN as usize {
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
    let len = reader.read_u32().await?;
    if len > MAX_FRAME_LEN {
        bail!("IPC-Nachricht zu groß ({len} Byte, maximal {MAX_FRAME_LEN})");
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
