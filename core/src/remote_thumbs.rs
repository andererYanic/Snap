//! Vorschaubilder von Dateien in Freigaben anderer Geräte (Protokoll `snap/thumb/1`).
//!
//! Galerie-Ansicht fremder Freigaben, ohne die Originale zu laden: Das Gerät mit der Datei
//! erzeugt das Vorschaubild und schickt nur das kleine JPEG.
//!
//! Sicherheit:
//! - Nur gekoppelte Geräte (sonst wird vor dem Lesen getrennt), nur mit `files.read` auf den
//!   Ordner (zentrale Rechteprüfung), nur innerhalb von Freigaben, keine Symlinks auf dem Weg.
//! - Anfrage ≤ 4 KiB, Antwort ≤ 1 MiB, Zeitlimits, höchstens 4 gleichzeitig.
//! - Empfangene Bilder werden nie ungeprüft weitergegeben: Sie werden mit den begrenzten
//!   Decodern neu codiert, bevor sie im Cache landen.

use crate::api::auth::{authorize, Caller, Capability};
use crate::api::error::{code_of, ApiError, ErrorCode};
use crate::api_bail;
use crate::daemon::DaemonInner;
use crate::media::{ThumbSize, Thumbnail};
use crate::network::endpoint_addr;
use crate::remote_ops::resolve_in_share;
use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

pub const THUMB_ALPN: &[u8] = b"snap/thumb/1";
const MAX_REQUEST: u32 = 4 * 1024;
/// Größtes akzeptiertes Vorschaubild (1024 px JPEG liegt meist bei 100–300 KB).
pub const MAX_IMAGE: u32 = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ThumbRequest {
    pub v: u32,
    pub share: String,
    pub path: String,
    #[serde(default)]
    pub size: ThumbSize,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ThumbReplyHead {
    ok: bool,
    code: Option<ErrorCode>,
    message: String,
    /// Länge des folgenden JPEG.
    len: u32,
}

/// Erzeugt das Vorschaubild für eine Anfrage eines gekoppelten Geräts.
async fn serve(inner: &Arc<DaemonInner>, peer: &str, req: ThumbRequest) -> Result<Vec<u8>> {
    if req.v != 1 {
        api_bail!(VersionMismatch, "Protokollversion {} wird nicht unterstützt", req.v);
    }
    if crate::ledger::validate_share_name(&req.share).is_err() || req.path.is_empty() {
        api_bail!(InvalidInput, "Ungültige Anfrage");
    }
    let shares = inner.db.list_shares()?;
    let Some(share) = shares.into_iter().find(|s| s.name == req.share) else {
        api_bail!(NotFound, "Freigabe '{}' gibt es nicht.", req.share);
    };
    let path = resolve_in_share(&PathBuf::from(&share.root), &req.path)?;
    authorize(
        &inner.db,
        &inner.protected_paths(),
        &Caller::Device(peer.to_string()),
        Capability::FilesRead,
        Some(&path),
        "thumb.get",
    )?;
    let thumb = inner.thumbs.get(&path, req.size).await?;
    let bytes = tokio::fs::read(&thumb.path).await?;
    if bytes.len() > MAX_IMAGE as usize {
        api_bail!(Internal, "Vorschaubild zu groß");
    }
    Ok(bytes)
}

/// Holt ein Vorschaubild von einem gekoppelten Gerät und legt es (neu codiert) im Cache ab.
pub(crate) async fn fetch(
    inner: &Arc<DaemonInner>,
    device: &str,
    share: &str,
    path: &str,
    size: ThumbSize,
) -> Result<Thumbnail> {
    crate::ledger::validate_share_name(share)
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Ungültige Freigabe"))?;
    crate::ledger::validate_rel_path(path).map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Pfad"))?;
    let mut found = inner.db.find_devices(device)?;
    let dev = match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'."),
        1 => found.remove(0),
        _ => api_bail!(Ambiguous, "'{device}' passt auf mehrere Geräte."),
    };
    // Cache-Schlüssel mit dem Inhalts-Hash aus dem Katalog: ändert sich die Datei, gibt es ein
    // neues Vorschaubild.
    let content = inner
        .db
        .find_remote_by_share_path(share, path, &inner.self_id())?
        .into_iter()
        .find(|f| f.origin == dev.node_id)
        .map(|f| f.hash)
        .unwrap_or_default();
    let key = {
        let mut h = blake3::Hasher::new();
        h.update(b"snap-remote-thumb-v1\0");
        for part in [dev.node_id.as_str(), share, path, content.as_str()] {
            h.update(&(part.len() as u32).to_le_bytes());
            h.update(part.as_bytes());
        }
        h.update(&size.edge().to_le_bytes());
        h.finalize().to_hex().to_string()
    };
    let target = inner.config.thumb_dir().join(&key[..2]).join(format!("{key}.jpg"));
    if let Ok((w, h)) = image::image_dimensions(&target) {
        return Ok(Thumbnail { path: target, width: w, height: h, cached: true });
    }

    let id: iroh::EndpointId = dev.node_id.parse().context("Ungültige Node-ID")?;
    let hints = inner.db.addr_hints(&dev.node_id).unwrap_or_default();
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    let conn = tokio::time::timeout(timeout, inner.ledger.endpoint.connect(endpoint_addr(id, &hints), THUMB_ALPN))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar.", dev.name)))?
        .map_err(|e| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar: {e}", dev.name)))?;
    let result = async {
        let (mut s, mut r) = conn.open_bi().await?;
        let req = ThumbRequest { v: 1, share: share.into(), path: path.into(), size };
        crate::ipc::write_frame_max(&mut s, &req, MAX_REQUEST).await?;
        s.finish()?;
        let head: ThumbReplyHead =
            tokio::time::timeout(IO_TIMEOUT, crate::ipc::read_frame_max(&mut r, MAX_REQUEST)).await??;
        if !head.ok {
            return Err(ApiError::new(head.code.unwrap_or(ErrorCode::Internal), head.message).into());
        }
        if head.len == 0 || head.len > MAX_IMAGE {
            anyhow::bail!("Ungültige Antwortgröße");
        }
        let mut buf = vec![0u8; head.len as usize];
        tokio::time::timeout(IO_TIMEOUT, r.read_exact(&mut buf)).await??;
        anyhow::Ok(buf)
    }
    .await;
    conn.close(0u32.into(), b"done");
    let bytes = result?;

    // Fremde Bytes neu codieren (begrenzte Decoder), erst dann in den Cache.
    let t2 = target.clone();
    let dir = inner.config.thumb_dir();
    let (w, h) = tokio::task::spawn_blocking(move || -> Result<(u32, u32)> {
        let img = std::panic::catch_unwind(|| {
            crate::media::render_thumbnail_limited(std::io::Cursor::new(&bytes), size.edge(), 64 * 1024 * 1024)
        })
        .map_err(|_| anyhow::anyhow!("Decoder abgestürzt"))?
        .map_err(|e| ApiError::new(ErrorCode::InvalidInput, format!("Ungültiges Vorschaubild: {e}")))?;
        crate::secure_fs::ensure_private_dir(&dir)?;
        let parent = t2.parent().context("Cache-Pfad")?;
        std::fs::create_dir_all(parent)?;
        let tmp = parent.join(format!(".{}.tmp", fastrand::u64(..)));
        {
            let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img)?;
        }
        std::fs::rename(&tmp, &t2)?;
        Ok((img.width(), img.height()))
    })
    .await??;
    Ok(Thumbnail { path: target, width: w, height: h, cached: false })
}

/// Protokoll-Handler für eingehende Vorschaubild-Anfragen.
#[derive(Clone)]
pub struct ThumbProtocol {
    pub inner: Arc<OnceLock<Weak<DaemonInner>>>,
    pub slots: Arc<tokio::sync::Semaphore>,
}

impl std::fmt::Debug for ThumbProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ThumbProtocol")
    }
}

impl ProtocolHandler for ThumbProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id().to_string();
        let Some(inner) = self.inner.get().and_then(Weak::upgrade) else {
            conn.close(2u32.into(), b"starting");
            return Ok(());
        };
        if !inner.db.is_paired(&peer).unwrap_or(false) {
            let _ = inner.db.audit(&peer, "thumb.connect", None, false, Some("nicht gekoppelt"));
            conn.close(1u32.into(), b"not paired");
            return Ok(());
        }
        let result = async {
            let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await??;
            let outcome: Result<Vec<u8>> = match self.slots.clone().try_acquire_owned() {
                Err(_) => Err(ApiError::new(ErrorCode::Unavailable, "Gerät ist beschäftigt").into()),
                Ok(_permit) => {
                    match tokio::time::timeout(
                        IO_TIMEOUT,
                        crate::ipc::read_frame_max::<_, ThumbRequest>(&mut recv, MAX_REQUEST),
                    )
                    .await
                    {
                        Ok(Ok(req)) => serve(&inner, &peer, req).await,
                        _ => Err(ApiError::new(ErrorCode::InvalidInput, "Ungültige Anfrage").into()),
                    }
                }
            };
            match outcome {
                Ok(bytes) => {
                    let head = ThumbReplyHead { ok: true, code: None, message: String::new(), len: bytes.len() as u32 };
                    crate::ipc::write_frame_max(&mut send, &head, MAX_REQUEST).await?;
                    send.write_all(&bytes).await?;
                }
                Err(e) => {
                    let head = ThumbReplyHead { ok: false, code: Some(code_of(&e)), message: format!("{e:#}"), len: 0 };
                    crate::ipc::write_frame_max(&mut send, &head, MAX_REQUEST).await?;
                }
            }
            send.finish()?;
            let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::debug!("Vorschaubild-Anfrage abgebrochen: {e:#}");
        }
        Ok(())
    }
}
