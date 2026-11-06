//! Streaming über einen lokalen HTTP-Server (für VLC, mpv, Browser und die Snap-App).
//!
//! - Lauscht nur auf `127.0.0.1`; jede Datei bekommt eine eigene Adresse mit 256-Bit-Token
//!   (`/s/<token>/<name>`), die nach 12 h ohne Nutzung verfällt.
//! - Host-Prüfung gegen DNS-Rebinding: nur `127.0.0.1:<port>` und `localhost:<port>`.
//! - Nur `GET`/`HEAD`, Byte-Bereiche (`Range`) – Spulen lädt nur die nötigen Stücke.
//! - Daten werden in 1-MiB-Blöcken vom Gerät geholt (BLAKE3/bao-verifiziert) und bleiben im
//!   Streaming-Cache, bis die Grenze erreicht ist (älteste zuerst).
//! - Ob ein Gerät die Datei liefern darf, entscheidet weiterhin dessen Rechteprüfung.

use crate::daemon::DaemonInner;
use anyhow::Result;
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Method, Request, Response, StatusCode};
use iroh_blobs::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

const BLOCK: u64 = 1024 * 1024;
/// Größtes Vorausladefenster (bei durchgehendem Abspielen).
const MAX_WINDOW: u64 = 16 * 1024 * 1024;
const TOKEN_TTL: Duration = Duration::from_secs(12 * 3600);
const MAX_TOKENS: usize = 1_000;
const MAX_CONNECTIONS: usize = 64;

/// Eine ausgegebene Streaming-Adresse.
#[derive(Debug, Clone)]
struct Entry {
    hash: Hash,
    name: String,
    mime: String,
    /// Mögliche Quellen, die erste zuerst.
    sources: Vec<iroh::EndpointAddr>,
    last_used: Instant,
}

/// Antwort auf `StreamUrl`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StreamInfo {
    pub url: String,
    pub name: String,
    pub mime: String,
}

/// Ein Byte-Bereich `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// Ergebnis der `Range`-Auswertung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeReq {
    /// Kein (verwertbarer) Range-Kopf: ganze Datei.
    Full,
    Partial(ByteRange),
    /// Bereich liegt außerhalb der Datei.
    Unsatisfiable,
}

/// Wertet einen `Range`-Kopf aus (RFC 9110). Mehrere Bereiche → ganze Datei.
pub fn parse_range(header: Option<&str>, size: u64) -> RangeReq {
    let Some(h) = header else { return RangeReq::Full };
    let Some(spec) = h.trim().strip_prefix("bytes=") else { return RangeReq::Full };
    if spec.contains(',') {
        return RangeReq::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else { return RangeReq::Full };
    let num = |s: &str| -> Option<u64> {
        let s = s.trim();
        (!s.is_empty() && s.len() <= 20 && s.bytes().all(|c| c.is_ascii_digit())).then(|| s.parse().ok()).flatten()
    };
    let r = match (a.trim().is_empty(), b.trim().is_empty()) {
        // bytes=-N: die letzten N Bytes
        (true, false) => match num(b) {
            Some(0) | None => return RangeReq::Unsatisfiable,
            Some(n) => ByteRange { start: size.saturating_sub(n), end: size },
        },
        // bytes=A-
        (false, true) => match num(a) {
            Some(s) => ByteRange { start: s, end: size },
            None => return RangeReq::Full,
        },
        (false, false) => match (num(a), num(b)) {
            (Some(s), Some(e)) if e >= s => ByteRange { start: s, end: e.saturating_add(1).min(size) },
            (Some(_), Some(_)) => return RangeReq::Unsatisfiable,
            _ => return RangeReq::Full,
        },
        (true, true) => return RangeReq::Full,
    };
    if r.start >= size || r.start >= r.end {
        return RangeReq::Unsatisfiable;
    }
    RangeReq::Partial(r)
}

/// MIME-Typ aus der Dateiendung (für Player).
pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Lokaler Streaming-Server.
pub struct StreamServer {
    addr: SocketAddr,
    entries: Mutex<HashMap<String, Entry>>,
}

impl StreamServer {
    /// Bindet an `127.0.0.1:port` (0 = frei wählen).
    pub async fn bind(port: u16) -> Result<(Arc<Self>, tokio::net::TcpListener)> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let addr = listener.local_addr()?;
        Ok((Arc::new(Self { addr, entries: Mutex::new(HashMap::new()) }), listener))
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Gibt eine Adresse für einen Blob aus.
    pub fn issue(&self, hash: Hash, name: &str, sources: Vec<iroh::EndpointAddr>) -> StreamInfo {
        let mut token = [0u8; 32];
        getrandom::fill(&mut token).expect("Zufallszahlen des Betriebssystems");
        let token = data_encoding::HEXLOWER.encode(&token);
        let safe = crate::api::transfer::sanitize_file_name(name);
        let mime = mime_for(&safe).to_string();
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, e| e.last_used.elapsed() < TOKEN_TTL);
        if map.len() >= MAX_TOKENS {
            if let Some(oldest) = map.iter().min_by_key(|(_, e)| e.last_used).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
        map.insert(
            token.clone(),
            Entry { hash, name: safe.clone(), mime: mime.clone(), sources, last_used: Instant::now() },
        );
        let url = format!(
            "http://127.0.0.1:{}/s/{token}/{}",
            self.port(),
            crate::fileops::trash::percent_encode(safe.as_bytes())
        );
        StreamInfo { url, name: safe, mime }
    }

    fn lookup(&self, token: &str) -> Option<Entry> {
        let mut map = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let e = map.get_mut(token)?;
        if e.last_used.elapsed() >= TOKEN_TTL {
            map.remove(token);
            return None;
        }
        e.last_used = Instant::now();
        Some(e.clone())
    }
}

type Body = BoxBody<Bytes, std::io::Error>;

fn plain(status: StatusCode, msg: &'static str) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from_static(msg.as_bytes())).map_err(|e| match e {}).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert("content-type", "text/plain; charset=utf-8".parse().unwrap());
    r
}

/// Prüft den Host-Kopf gegen DNS-Rebinding.
pub fn host_ok(host: Option<&str>, port: u16) -> bool {
    matches!(host, Some(h) if h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"))
}

/// Startet die Annahmeschleife.
pub(crate) async fn serve(server: Arc<StreamServer>, listener: tokio::net::TcpListener, inner: Weak<DaemonInner>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let Ok((tcp, peer)) = listener.accept().await else { continue };
        // Nur lokale Verbindungen (zusätzlich zur Bindung an 127.0.0.1).
        if !peer.ip().is_loopback() {
            continue;
        }
        let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
        let (server, inner) = (server.clone(), inner.clone());
        tokio::spawn(async move {
            let _permit = permit;
            let svc = hyper::service::service_fn(move |req| {
                let (server, inner) = (server.clone(), inner.clone());
                async move { Ok::<_, Infallible>(handle(server, inner, req).await) }
            });
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder.timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(Duration::from_secs(10));
            let _ = builder.serve_connection(hyper_util::rt::TokioIo::new(tcp), svc).await;
        });
    }
}

async fn handle(
    server: Arc<StreamServer>,
    inner: Weak<DaemonInner>,
    req: Request<hyper::body::Incoming>,
) -> Response<Body> {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return plain(StatusCode::METHOD_NOT_ALLOWED, "Nur GET und HEAD.\n");
    }
    let host = req.headers().get("host").and_then(|h| h.to_str().ok());
    if !host_ok(host, server.port()) {
        return plain(StatusCode::FORBIDDEN, "Ungültiger Host.\n");
    }
    let path = req.uri().path();
    let token = path.strip_prefix("/s/").and_then(|r| r.split('/').next()).unwrap_or("");
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return plain(StatusCode::NOT_FOUND, "Nicht gefunden.\n");
    }
    let Some(entry) = server.lookup(token) else { return plain(StatusCode::NOT_FOUND, "Nicht gefunden.\n") };
    let Some(inner) = inner.upgrade() else { return plain(StatusCode::SERVICE_UNAVAILABLE, "Beendet.\n") };

    // Größe (verifiziert) von der ersten erreichbaren Quelle.
    let mut size_and_source = None;
    for addr in &entry.sources {
        if let Ok(size) = inner.network.stream_size(addr, entry.hash).await {
            size_and_source = Some((size, addr.clone()));
            break;
        }
    }
    let Some((size, _)) = size_and_source else {
        return plain(StatusCode::BAD_GATEWAY, "Kein Gerät mit dieser Datei erreichbar.\n");
    };
    let _ = inner.network.tag_stream(entry.hash).await;
    let _ = inner.db.touch_stream(&entry.hash.to_string());

    let range_header = req.headers().get("range").and_then(|h| h.to_str().ok());
    let (status, range) = match parse_range(range_header, size) {
        RangeReq::Full => (StatusCode::OK, ByteRange { start: 0, end: size }),
        RangeReq::Partial(r) => (StatusCode::PARTIAL_CONTENT, r),
        RangeReq::Unsatisfiable => {
            let mut r = plain(StatusCode::RANGE_NOT_SATISFIABLE, "Bereich außerhalb der Datei.\n");
            r.headers_mut().insert("content-range", format!("bytes */{size}").parse().unwrap());
            return r;
        }
    };
    let body: Body = if req.method() == Method::HEAD || range.start == range.end {
        Empty::new().map_err(|e| match e {}).boxed()
    } else {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, std::io::Error>>(4);
        let (inner2, entry2) = (inner.clone(), entry.clone());
        tokio::spawn(async move {
            let mut pos = range.start;
            // Vorausladen: kleines erstes Fenster (schneller Start beim Spulen), dann wachsend.
            let mut fetched_until = range.start;
            let mut window = BLOCK;
            while pos < range.end {
                let end = (pos + BLOCK).min(range.end);
                if end > fetched_until {
                    let until = (pos + window).min(range.end);
                    if let Err(e) = fetch_window(&inner2, &entry2, pos, until).await {
                        let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                        return;
                    }
                    fetched_until = until;
                    window = (window * 2).min(MAX_WINDOW);
                }
                match inner2.network.read_local(entry2.hash, pos, end).await {
                    Ok(data) => {
                        if tx.send(Ok(Frame::data(Bytes::from(data)))).await.is_err() {
                            return; // Player hat die Verbindung geschlossen (z. B. Spulen)
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
                        return;
                    }
                }
                pos = end;
            }
        });
        StreamBody::new(tokio_stream_from(rx)).boxed()
    };
    let mut resp = Response::new(body);
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert("accept-ranges", "bytes".parse().unwrap());
    h.insert("content-length", (range.end - range.start).to_string().parse().unwrap());
    h.insert("content-type", entry.mime.parse().unwrap_or_else(|_| "application/octet-stream".parse().unwrap()));
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    h.insert("cache-control", "no-store".parse().unwrap());
    h.insert("referrer-policy", "no-referrer".parse().unwrap());
    if let Ok(v) = format!(
        "inline; filename*=UTF-8''{}",
        crate::fileops::trash::percent_encode(entry.name.as_bytes()).replace('/', "%2F")
    )
    .parse()
    {
        h.insert("content-disposition", v);
    }
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert("content-range", format!("bytes {}-{}/{size}", range.start, range.end - 1).parse().unwrap());
    }
    resp
}

/// Stellt einen Bereich lokal bereit (fehlende Stücke von der ersten Quelle, die liefert).
async fn fetch_window(inner: &DaemonInner, entry: &Entry, start: u64, end: u64) -> Result<()> {
    let mut last_err = None;
    for addr in &entry.sources {
        match inner.network.ensure_range(addr, entry.hash, start, end).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    anyhow::bail!("Keine Quelle lieferte die Daten: {}", last_err.unwrap_or_default())
}

fn tokio_stream_from<T: Send + 'static>(
    mut rx: tokio::sync::mpsc::Receiver<T>,
) -> impl n0_future::Stream<Item = T> + Send + 'static {
    n0_future::stream::poll_fn(move |cx| rx.poll_recv(cx))
}

/// Hält den Streaming-Cache unter `max_bytes` (älteste zuerst freigeben).
pub(crate) async fn enforce_cache_limit(inner: &DaemonInner, max_bytes: u64) -> Result<u64> {
    let entries = inner.db.stream_entries()?; // älteste zuerst
    let mut sizes = Vec::with_capacity(entries.len());
    let mut total = 0u64;
    for hash in &entries {
        let Ok(h) = hash.parse::<Hash>() else { continue };
        let b = inner.network.local_bytes(h).await;
        total += b;
        sizes.push((h, hash.clone(), b));
    }
    let mut freed = 0;
    for (h, hex, b) in sizes {
        if total <= max_bytes {
            break;
        }
        inner.network.untag_stream(h).await?;
        inner.db.remove_stream(&hex)?;
        total = total.saturating_sub(b);
        freed += b;
    }
    Ok(freed)
}

/// Gibt eine Streaming-Adresse für ein Ziel aus (Ticket, Hash-Präfix, Name oder Freigabe/Pfad).
pub(crate) fn stream_url(inner: &DaemonInner, target: &str) -> Result<crate::ipc::Response> {
    let t = crate::api::transfer::resolve_download_target(inner, target)?;
    let hash = t.ticket.hash();
    let mut sources = vec![t.ticket.addr().clone()];
    // Weitere Geräte mit demselben Inhalt als Ersatzquellen.
    let self_id = inner.self_id();
    let mut others: Vec<String> = inner
        .db
        .find_remote_by_hash_prefix(&hash.to_string(), &self_id)
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.origin)
        .collect();
    others.sort();
    others.dedup();
    for o in others {
        let Ok(id) = o.parse::<iroh::EndpointId>() else { continue };
        if sources.iter().any(|s| s.id == id) {
            continue;
        }
        let hints = inner.db.addr_hints(&o).unwrap_or_default();
        sources.push(crate::network::endpoint_addr(id, &hints));
    }
    let server = inner.stream.as_ref().ok_or_else(|| {
        crate::api::error::ApiError::new(crate::api::error::ErrorCode::Unavailable, "Streaming-Server läuft nicht")
    })?;
    Ok(crate::ipc::Response::StreamUrl(server.issue(hash, &t.default_name, sources)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let p = |h: &str| parse_range(Some(h), 1000);
        let r = |s, e| RangeReq::Partial(ByteRange { start: s, end: e });
        assert_eq!(parse_range(None, 1000), RangeReq::Full);
        assert_eq!(p("bytes=0-99"), r(0, 100));
        assert_eq!(p("bytes=500-"), r(500, 1000));
        assert_eq!(p("bytes=-100"), r(900, 1000));
        assert_eq!(p("bytes=-5000"), r(0, 1000));
        assert_eq!(p("bytes=900-5000"), r(900, 1000));
        assert_eq!(p("bytes=1000-"), RangeReq::Unsatisfiable);
        assert_eq!(p("bytes=5-1"), RangeReq::Unsatisfiable);
        assert_eq!(p("bytes=-0"), RangeReq::Unsatisfiable);
        for full in
            ["bytes=0-1,5-9", "items=0-1", "bytes=a-b", "bytes=-", "bytes=99999999999999999999999-", "bytes=+1-2"]
        {
            assert_eq!(p(full), RangeReq::Full, "{full}");
        }
        assert_eq!(parse_range(Some("bytes=0-"), 0), RangeReq::Unsatisfiable);
    }

    #[test]
    fn host_header_must_be_local() {
        assert!(host_ok(Some("127.0.0.1:8080"), 8080));
        assert!(host_ok(Some("localhost:8080"), 8080));
        for bad in [None, Some("127.0.0.1:8081"), Some("evil.example:8080"), Some("127.0.0.1"), Some("LOCALHOST:8080 ")]
        {
            assert!(!host_ok(bad, 8080), "{bad:?}");
        }
    }

    #[test]
    fn mime_types() {
        assert_eq!(mime_for("Film.MKV"), "video/x-matroska");
        assert_eq!(mime_for("x"), "application/octet-stream");
    }
}
