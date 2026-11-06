//! Fernoperationen auf Dateien in Freigaben (Protokoll `snap/ops/1`).
//!
//! Ein gekoppeltes Gerät darf innerhalb einer Freigabe umbenennen, verschieben, kopieren,
//! Ordner anlegen, in den Papierkorb legen und – mit eigenem Recht – endgültig löschen.
//!
//! Sicherheit:
//! - Identität aus TLS; nicht gekoppelte Gegenstellen werden vor dem Lesen getrennt.
//! - Pfade sind relativ zur Freigabe (strenge Prüfung, kein `..`, keine absoluten Pfade);
//!   Symlinks auf dem Weg werden abgelehnt, das Ziel wird zusätzlich kanonisiert geprüft.
//! - Jede Operation läuft durch die zentrale Rechteprüfung (`files.write`, `files.read`,
//!   `files.delete`, `files.purge`, jeweils ordnergebunden) und landet im Audit-Log.
//! - Löschen landet im Papierkorb; endgültig nur mit `files.purge` (nie Standard).
//! - Massenschutz: mehr als `ops_hourly_limit` veränderte Dateien pro Gerät und Stunde werden
//!   angehalten, bis der Nutzer am Zielgerät bestätigt.
//! - Nachrichten ≤ 64 KiB, strikte Typen, Zeitlimits, höchstens 4 Operationen gleichzeitig.

use crate::api::auth::{authorize, Caller, Capability};
use crate::api::error::{code_of, ApiError, ErrorCode};
use crate::api_bail;
use crate::daemon::DaemonInner;
use crate::fileops::jobs::FileOp;
use crate::fileops::ops::{measure, validate_name, Conflict};
use crate::ipc::{read_frame_max, write_frame_max, Response};
use crate::ledger::{validate_rel_path, validate_share_name};
use crate::network::endpoint_addr;
use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

pub const OPS_ALPN: &[u8] = b"snap/ops/1";
pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: u32 = 64 * 1024;
/// Höchstzahl Pfade pro Fernoperation.
pub const MAX_PATHS: usize = 1_000;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// So lange wartet der Auftraggeber auf das Ergebnis (große Kopien).
const REPLY_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_CONCURRENT: usize = 4;
const WINDOW: Duration = Duration::from_secs(3600);

/// Eine Fernoperation. Pfade relativ zur Freigabe; `""` steht für die Wurzel (nur als Ziel).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum RemoteOp {
    Rename {
        share: String,
        path: String,
        new_name: String,
    },
    Move {
        share: String,
        paths: Vec<String>,
        dest_dir: String,
        #[serde(default)]
        conflict: Conflict,
    },
    Copy {
        share: String,
        paths: Vec<String>,
        dest_dir: String,
        #[serde(default)]
        conflict: Conflict,
    },
    Mkdir {
        share: String,
        parent: String,
        name: String,
    },
    /// In den Papierkorb des Zielgeräts.
    Trash {
        share: String,
        paths: Vec<String>,
    },
    /// Endgültig löschen (Recht `files.purge`).
    Purge {
        share: String,
        paths: Vec<String>,
    },
    /// Hochladen: Das Zielgerät holt den Inhalt `hash` (BLAKE3) vom Auftraggeber und legt ihn
    /// als `dest_dir/name` ab (Recht `files.write`). Fehlende Ordner werden angelegt.
    Import {
        share: String,
        dest_dir: String,
        name: String,
        hash: String,
        size: u64,
        #[serde(default = "keep_both")]
        conflict: Conflict,
    },
}

fn keep_both() -> Conflict {
    Conflict::KeepBoth
}

/// Hash aus fremder Eingabe: nur genau 64 Hex-Zeichen. `Hash::from_str` aus iroh-blobs stürzt
/// bei 64 Zeichen mit Mehrbyte-Unicode ab (Panic in data-encoding; gefunden per Fuzzing).
pub fn parse_hash_hex(s: &str) -> Option<iroh_blobs::Hash> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    s.parse().ok()
}

/// Größte Datei, die hochgeladen werden kann.
pub const MAX_IMPORT_BYTES: u64 = 1 << 40;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpsRequest {
    pub v: u32,
    pub op: RemoteOp,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpsReply {
    pub ok: bool,
    pub code: Option<ErrorCode>,
    pub message: String,
    #[serde(default)]
    pub approval_id: Option<i64>,
    /// Ergebnis (z. B. Pfad der hochgeladenen Datei relativ zur Freigabe).
    #[serde(default)]
    pub result: Option<String>,
}

impl OpsReply {
    fn error(e: &anyhow::Error) -> Self {
        Self { ok: false, code: Some(code_of(e)), message: format!("{e:#}"), approval_id: None, result: None }
    }
}

impl RemoteOp {
    pub fn share(&self) -> &str {
        match self {
            RemoteOp::Rename { share, .. }
            | RemoteOp::Move { share, .. }
            | RemoteOp::Copy { share, .. }
            | RemoteOp::Mkdir { share, .. }
            | RemoteOp::Trash { share, .. }
            | RemoteOp::Purge { share, .. }
            | RemoteOp::Import { share, .. } => share,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            RemoteOp::Rename { .. } => "ops.rename",
            RemoteOp::Move { .. } => "ops.move",
            RemoteOp::Copy { .. } => "ops.copy",
            RemoteOp::Mkdir { .. } => "ops.mkdir",
            RemoteOp::Trash { .. } => "ops.trash",
            RemoteOp::Purge { .. } => "ops.purge",
            RemoteOp::Import { .. } => "ops.import",
        }
    }

    /// Lesbare Beschreibung für Bestätigung und Audit-Log.
    pub fn summary(&self) -> String {
        let list = |paths: &[String]| {
            let shown: Vec<&str> = paths.iter().take(3).map(String::as_str).collect();
            let more = if paths.len() > 3 { format!(" und {} weitere", paths.len() - 3) } else { String::new() };
            format!("{}{more}", shown.join(", "))
        };
        match self {
            RemoteOp::Rename { share, path, new_name } => format!("{share}/{path} umbenennen in '{new_name}'"),
            RemoteOp::Move { share, paths, dest_dir, .. } => {
                format!("in '{share}' verschieben nach '/{dest_dir}': {}", list(paths))
            }
            RemoteOp::Copy { share, paths, dest_dir, .. } => {
                format!("in '{share}' kopieren nach '/{dest_dir}': {}", list(paths))
            }
            RemoteOp::Mkdir { share, parent, name } => format!("Ordner '{name}' in '{share}/{parent}' anlegen"),
            RemoteOp::Trash { share, paths } => format!("in '{share}' in den Papierkorb: {}", list(paths)),
            RemoteOp::Purge { share, paths } => format!("in '{share}' ENDGÜLTIG löschen: {}", list(paths)),
            RemoteOp::Import { share, dest_dir, name, size, .. } => {
                format!("'{name}' ({:.1} MB) nach '{share}/{dest_dir}' hochladen", *size as f64 / 1e6)
            }
        }
    }

    /// Statische Prüfung ohne Dateisystem (auch für das Fuzz-Ziel).
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| -> anyhow::Error { ApiError::new(ErrorCode::InvalidInput, what.to_string()).into() };
        validate_share_name(self.share()).map_err(|_| bad("Ungültiger Freigabe-Name"))?;
        let rel = |p: &str| validate_rel_path(p).map_err(|_| bad("Ungültiger Pfad"));
        let dir = |p: &str| if p.is_empty() { Ok(()) } else { rel(p) };
        let many = |paths: &[String]| -> Result<()> {
            if paths.is_empty() || paths.len() > MAX_PATHS {
                return Err(bad("Ungültige Anzahl Pfade"));
            }
            paths.iter().try_for_each(|p| rel(p))
        };
        match self {
            RemoteOp::Rename { path, new_name, .. } => {
                rel(path)?;
                validate_name(new_name)
            }
            RemoteOp::Move { paths, dest_dir, .. } | RemoteOp::Copy { paths, dest_dir, .. } => {
                many(paths)?;
                dir(dest_dir)
            }
            RemoteOp::Mkdir { parent, name, .. } => {
                dir(parent)?;
                validate_name(name)
            }
            RemoteOp::Trash { paths, .. } | RemoteOp::Purge { paths, .. } => many(paths),
            RemoteOp::Import { dest_dir, name, hash, size, .. } => {
                dir(dest_dir)?;
                validate_name(name)?;
                // Keine versteckten Dateien (.bashrc, .git …) über das Netz anlegen.
                if name.starts_with('.') {
                    return Err(bad("Versteckte Dateien können nicht hochgeladen werden"));
                }
                if parse_hash_hex(hash).is_none() {
                    return Err(bad("Ungültiger Hash"));
                }
                if *size > MAX_IMPORT_BYTES {
                    return Err(bad("Datei ist zu groß"));
                }
                Ok(())
            }
        }
    }
}

/// Dekodiert und prüft eine Anfrage (Fuzz-Ziel).
pub fn decode_request(bytes: &[u8]) -> Result<OpsRequest> {
    let req: OpsRequest = serde_json::from_slice(bytes)?;
    req.op.validate()?;
    Ok(req)
}

/// Absoluter Pfad in einer Freigabe. Symlinks auf dem Weg werden abgelehnt.
pub fn resolve_in_share(root: &Path, rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }
    if validate_rel_path(rel).is_err() {
        api_bail!(InvalidInput, "Ungültiger Pfad");
    }
    let parts: Vec<&str> = rel.split('/').collect();
    let mut cur = root.to_path_buf();
    for part in &parts[..parts.len() - 1] {
        cur.push(part);
        if std::fs::symlink_metadata(&cur).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
            api_bail!(PermissionDenied, "Pfad führt über einen Symlink");
        }
    }
    Ok(root.join(rel))
}

/// Vorbereitete Operation: absolute Pfade und die nötigen Rechte.
/// Was nach der Prüfung ausgeführt wird.
enum Action {
    File(FileOp),
    Import { dest: PathBuf, name: String, hash: iroh_blobs::Hash, size: u64, conflict: Conflict },
}

struct Prepared {
    op: Action,
    checks: Vec<(Capability, PathBuf)>,
    /// Pfade, deren Dateien für den Massenschutz zählen.
    weighted: Vec<PathBuf>,
}

fn prepare(inner: &DaemonInner, op: &RemoteOp) -> Result<Prepared> {
    op.validate()?;
    let shares = inner.db.list_shares()?;
    let Some(share) = shares.into_iter().find(|s| s.name == op.share()) else {
        api_bail!(NotFound, "Freigabe '{}' gibt es auf diesem Gerät nicht.", op.share());
    };
    let root = PathBuf::from(&share.root);
    let r = |rel: &str| resolve_in_share(&root, rel);
    let many = |paths: &[String]| paths.iter().map(|p| r(p)).collect::<Result<Vec<_>>>();
    Ok(match op {
        RemoteOp::Rename { path, new_name, .. } => {
            let p = r(path)?;
            Prepared {
                checks: vec![(Capability::FilesWrite, p.clone())],
                weighted: vec![],
                op: Action::File(FileOp::Rename { path: p, new_name: new_name.clone() }),
            }
        }
        RemoteOp::Move { paths, dest_dir, conflict, .. } => {
            let (sources, dest) = (many(paths)?, r(dest_dir)?);
            let mut checks: Vec<_> = sources.iter().map(|p| (Capability::FilesWrite, p.clone())).collect();
            checks.push((Capability::FilesWrite, dest.clone()));
            // „Ersetzen“ legt das alte Ziel in den Papierkorb → braucht auch files.delete.
            if *conflict == Conflict::Replace {
                checks.push((Capability::FilesDelete, dest.clone()));
            }
            Prepared {
                checks,
                weighted: sources.clone(),
                op: Action::File(FileOp::Move { sources, dest, conflict: *conflict }),
            }
        }
        RemoteOp::Copy { paths, dest_dir, conflict, .. } => {
            let (sources, dest) = (many(paths)?, r(dest_dir)?);
            let mut checks: Vec<_> = sources.iter().map(|p| (Capability::FilesRead, p.clone())).collect();
            checks.push((Capability::FilesWrite, dest.clone()));
            if *conflict == Conflict::Replace {
                checks.push((Capability::FilesDelete, dest.clone()));
            }
            Prepared { checks, weighted: vec![], op: Action::File(FileOp::Copy { sources, dest, conflict: *conflict }) }
        }
        RemoteOp::Mkdir { parent, name, .. } => {
            let p = r(parent)?;
            Prepared {
                checks: vec![(Capability::FilesWrite, p.clone())],
                weighted: vec![],
                op: Action::File(FileOp::Mkdir { parent: p, name: name.clone() }),
            }
        }
        RemoteOp::Trash { paths, .. } => {
            let paths = many(paths)?;
            Prepared {
                checks: paths.iter().map(|p| (Capability::FilesDelete, p.clone())).collect(),
                weighted: paths.clone(),
                op: Action::File(FileOp::Trash { paths }),
            }
        }
        RemoteOp::Purge { paths, .. } => {
            let paths = many(paths)?;
            Prepared {
                checks: paths.iter().map(|p| (Capability::FilesPurge, p.clone())).collect(),
                weighted: paths.clone(),
                op: Action::File(FileOp::Delete { paths }),
            }
        }
        RemoteOp::Import { dest_dir, name, hash, size, conflict, .. } => {
            let dest = r(dest_dir)?;
            let mut checks = vec![(Capability::FilesWrite, dest.join(name))];
            if *conflict == Conflict::Replace {
                checks.push((Capability::FilesDelete, dest.join(name)));
            }
            Prepared {
                checks,
                weighted: vec![],
                op: Action::Import {
                    dest,
                    name: name.clone(),
                    hash: parse_hash_hex(hash)
                        .ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Hash"))?,
                    size: *size,
                    conflict: *conflict,
                },
            }
        }
    })
}

/// Zustand für Fernoperationen: gleitendes Stundenfenster je Gerät und Parallelitätsgrenze.
pub struct RemoteOpsState {
    window: Mutex<HashMap<String, VecDeque<(Instant, u64)>>>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl Default for RemoteOpsState {
    fn default() -> Self {
        Self { window: Mutex::new(HashMap::new()), slots: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT)) }
    }
}

impl RemoteOpsState {
    /// Bucht `weight` Dateien für `peer`, wenn das Stundenlimit es erlaubt.
    fn try_take(&self, peer: &str, weight: u64, limit: u32) -> bool {
        if weight == 0 {
            return true;
        }
        let now = Instant::now();
        let mut map = self.window.lock().unwrap_or_else(|e| e.into_inner());
        let q = map.entry(peer.to_string()).or_default();
        while q.front().is_some_and(|(t, _)| now.duration_since(*t) > WINDOW) {
            q.pop_front();
        }
        let used: u64 = q.iter().map(|(_, w)| w).sum();
        if used + weight > limit as u64 {
            return false;
        }
        q.push_back((now, weight));
        true
    }
}

/// Prüft alle Rechte einer vorbereiteten Operation.
fn authorize_all(inner: &DaemonInner, peer: &str, op: &RemoteOp, prepared: &Prepared) -> Result<()> {
    let caller = Caller::Device(peer.to_string());
    let protected = inner.protected_paths();
    for (cap, path) in &prepared.checks {
        authorize(&inner.db, &protected, &caller, *cap, Some(path), op.kind())?;
    }
    Ok(())
}

async fn execute(inner: &Arc<DaemonInner>, peer: &str, op: &RemoteOp, prepared: Prepared) -> Result<OpsReply> {
    let result = match prepared.op {
        Action::File(file_op) => {
            let (job, p) = inner.files.register();
            crate::api::fileops::run_job(inner.clone(), job, p, file_op).await?;
            None
        }
        Action::Import { dest, name, hash, size, conflict } => {
            let root =
                inner.db.list_shares()?.into_iter().find(|s| s.name == op.share()).map(|s| PathBuf::from(s.root));
            let path = crate::upload::receive(inner, peer, &dest, &name, hash, size, conflict).await?;
            root.and_then(|r| path.strip_prefix(&r).ok().map(|p| p.to_string_lossy().into_owned()))
        }
    };
    Ok(OpsReply { ok: true, code: None, message: format!("Erledigt: {}", op.summary()), approval_id: None, result })
}

/// Bearbeitet eine Anfrage eines gekoppelten Geräts (Identität aus TLS).
pub(crate) async fn serve(inner: &Arc<DaemonInner>, peer: &str, req: OpsRequest) -> OpsReply {
    match serve_inner(inner, peer, req).await {
        Ok(r) => r,
        Err(e) => OpsReply::error(&e),
    }
}

async fn serve_inner(inner: &Arc<DaemonInner>, peer: &str, req: OpsRequest) -> Result<OpsReply> {
    if req.v != PROTOCOL_VERSION {
        api_bail!(VersionMismatch, "Protokollversion {} wird nicht unterstützt", req.v);
    }
    if !inner.db.is_paired(peer)? {
        let _ = inner.db.audit(peer, req.op.kind(), None, false, Some("nicht gekoppelt"));
        api_bail!(PermissionDenied, "Gerät ist nicht gekoppelt");
    }
    let op = req.op;
    let prepared = match prepare(inner, &op) {
        Ok(p) => p,
        Err(e) => {
            let _ = inner.db.audit(peer, op.kind(), Some(op.share()), false, Some(&format!("{e:#}")));
            return Err(e);
        }
    };
    authorize_all(inner, peer, &op, &prepared)?;
    // Massenschutz: betroffene Dateien zählen (erst nach der Rechteprüfung).
    let weighted = prepared.weighted.clone();
    let weight = match &op {
        RemoteOp::Rename { .. } => 1,
        // Hochladen verändert nichts Bestehendes (eigene Grenze: upload_daily_gb) – außer beim Ersetzen.
        RemoteOp::Import { conflict, .. } => u64::from(*conflict == Conflict::Replace),
        _ => tokio::task::spawn_blocking(move || weighted.iter().map(|p| measure(p).0.max(1)).sum::<u64>()).await?,
    };
    if !inner.remote_ops.try_take(peer, weight, inner.settings.ops_hourly_limit()) {
        let json = serde_json::to_string(&op)?;
        let id = inner.db.add_approval(peer, &json, &op.summary())?;
        let _ = inner.db.audit(peer, op.kind(), Some(op.share()), false, Some("Massenschutz: wartet auf Bestätigung"));
        tracing::warn!(
            "Gerät {} überschreitet das Stundenlimit – Anfrage {id} wartet auf Bestätigung",
            &peer[..10.min(peer.len())]
        );
        return Ok(OpsReply {
            ok: false,
            code: Some(ErrorCode::NeedsApproval),
            message: format!(
                "Zu viele Änderungen in kurzer Zeit – die Anfrage {id} muss auf dem Zielgerät bestätigt werden."
            ),
            approval_id: Some(id),
            result: None,
        });
    }
    execute(inner, peer, &op, prepared).await
}

/// Bestätigt eine angehaltene Fernoperation und führt sie aus (Rechte werden erneut geprüft).
pub(crate) async fn approve(inner: &Arc<DaemonInner>, id: i64) -> Result<Response> {
    let Some((peer, json)) = inner.db.decide_approval(id, true)? else {
        api_bail!(NotFound, "Keine offene Anfrage mit Nummer {id}.");
    };
    if !inner.db.is_paired(&peer)? {
        api_bail!(PermissionDenied, "Das Gerät ist nicht mehr gekoppelt.");
    }
    let op: RemoteOp = serde_json::from_str(&json).context("Gespeicherte Anfrage ist beschädigt")?;
    let prepared = prepare(inner, &op)?;
    authorize_all(inner, &peer, &op, &prepared)?;
    let reply = execute(inner, &peer, &op, prepared).await?;
    let _ = inner.db.audit(&peer, op.kind(), Some(op.share()), true, Some(&format!("bestätigt (Anfrage {id})")));
    Ok(Response::Success { message: reply.message })
}

pub(crate) fn reject(inner: &DaemonInner, id: i64) -> Result<Response> {
    let Some((peer, _)) = inner.db.decide_approval(id, false)? else {
        api_bail!(NotFound, "Keine offene Anfrage mit Nummer {id}.");
    };
    let _ = inner.db.audit(&peer, "ops.approval", None, false, Some(&format!("abgelehnt (Anfrage {id})")));
    Ok(Response::Success { message: format!("Anfrage {id} abgelehnt.") })
}

/// Sendet eine Fernoperation an ein gekoppeltes Gerät (Aufruf durch den lokalen Nutzer).
pub(crate) async fn send(inner: &Arc<DaemonInner>, device: &str, op: RemoteOp) -> Result<Response> {
    let (name, reply) = send_op(inner, device, op, REPLY_TIMEOUT).await?;
    if reply.ok {
        Ok(Response::Success { message: format!("{name}: {}", reply.message) })
    } else {
        Ok(Response::error(reply.code.unwrap_or(ErrorCode::Internal), format!("{name}: {}", reply.message)))
    }
}

/// Sendet eine Fernoperation und liefert (Gerätename, Antwort).
pub(crate) async fn send_op(
    inner: &Arc<DaemonInner>,
    device: &str,
    op: RemoteOp,
    reply_timeout: Duration,
) -> Result<(String, OpsReply)> {
    op.validate()?;
    let mut found = inner.db.find_devices(device)?;
    let dev = match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'."),
        1 => found.remove(0),
        _ => api_bail!(Ambiguous, "'{device}' passt auf mehrere Geräte – bitte genauer angeben."),
    };
    let id: iroh::EndpointId = dev.node_id.parse().context("Ungültige Node-ID")?;
    let hints = inner.db.addr_hints(&dev.node_id).unwrap_or_default();
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    let conn = tokio::time::timeout(timeout, inner.ledger.endpoint.connect(endpoint_addr(id, &hints), OPS_ALPN))
        .await
        .map_err(|_| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar.", dev.name)))?
        .map_err(|e| ApiError::new(ErrorCode::Unavailable, format!("'{}' ist nicht erreichbar: {e}", dev.name)))?;
    let result = async {
        let (mut s, mut r) = conn.open_bi().await?;
        write_frame_max(&mut s, &OpsRequest { v: PROTOCOL_VERSION, op }, MAX_FRAME).await?;
        s.finish()?;
        let reply: OpsReply = tokio::time::timeout(reply_timeout, read_frame_max(&mut r, MAX_FRAME))
            .await
            .map_err(|_| ApiError::new(ErrorCode::Timeout, "Zeitüberschreitung beim Warten auf das Ergebnis"))??;
        anyhow::Ok(reply)
    }
    .await;
    conn.close(0u32.into(), b"done");
    let reply = result.map_err(|e| {
        if crate::api::error::code_of(&e) == ErrorCode::Internal {
            ApiError::new(ErrorCode::Unavailable, format!("Verbindung zu '{}' abgebrochen: {e:#}", dev.name)).into()
        } else {
            e
        }
    })?;
    Ok((dev.name, reply))
}

/// Protokoll-Handler für eingehende Fernoperationen.
#[derive(Clone)]
pub struct OpsProtocol {
    /// Wird nach dem Start gesetzt (der Daemon entsteht nach dem Netzwerk). `Weak` vermeidet
    /// einen Referenzzyklus Daemon → Netzwerk → Handler → Daemon.
    pub inner: Arc<OnceLock<Weak<DaemonInner>>>,
}

impl std::fmt::Debug for OpsProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpsProtocol")
    }
}

impl ProtocolHandler for OpsProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = conn.remote_id().to_string();
        let Some(inner) = self.inner.get().and_then(Weak::upgrade) else {
            conn.close(2u32.into(), b"starting");
            return Ok(());
        };
        // Nicht gekoppelt: trennen, bevor irgendetwas gelesen wird.
        if !inner.db.is_paired(&peer).unwrap_or(false) {
            let _ = inner.db.audit(&peer, "ops.connect", None, false, Some("nicht gekoppelt"));
            conn.close(1u32.into(), b"not paired");
            return Ok(());
        }
        let result = async {
            let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await??;
            let reply = match inner.remote_ops.slots.clone().try_acquire_owned() {
                Err(_) => OpsReply::error(&ApiError::new(ErrorCode::Unavailable, "Gerät ist beschäftigt").into()),
                Ok(_permit) => {
                    match tokio::time::timeout(IO_TIMEOUT, read_frame_max::<_, serde_json::Value>(&mut recv, MAX_FRAME))
                        .await
                    {
                        Ok(Ok(v)) => match serde_json::from_value::<OpsRequest>(v) {
                            Ok(req) => serve(&inner, &peer, req).await,
                            Err(e) => {
                                let _ = inner.db.audit(&peer, "ops", None, false, Some("ungültige Anfrage"));
                                OpsReply::error(
                                    &ApiError::new(ErrorCode::InvalidInput, format!("Ungültige Anfrage: {e}")).into(),
                                )
                            }
                        },
                        _ => OpsReply::error(
                            &ApiError::new(ErrorCode::InvalidInput, "Ungültige oder zu große Anfrage").into(),
                        ),
                    }
                }
            };
            write_frame_max(&mut send, &reply, MAX_FRAME).await?;
            send.finish()?;
            // Warten, bis die Gegenseite gelesen und geschlossen hat (sonst ginge die Antwort verloren).
            let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::debug!("Fernoperation von {} abgebrochen: {e:#}", &peer[..10.min(peer.len())]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_strict() {
        let ok = br#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":["2024/a.jpg"]}}"#;
        assert!(decode_request(ok).is_ok());
        for bad in [
            &br#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":["../x"]}}"#[..],
            br#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":["/etc/passwd"]}}"#,
            br#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":[""]}}"#,
            br#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":[]}}"#,
            br#"{"v":1,"op":{"op":"Trash","share":"../x","paths":["a"]}}"#,
            br#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":["a"],"extra":1}}"#,
            br#"{"v":1,"op":{"op":"Format","share":"Fotos"}}"#,
            br#"{"v":1,"op":{"op":"Rename","share":"F","path":"a","new_name":"x/y"}}"#,
            br#"{"v":1,"op":{"op":"Rename","share":"F","path":"a","new_name":".."}}"#,
            br#"{"v":1,"op":{"op":"Move","share":"F","paths":["a"],"dest_dir":"../b"}}"#,
            br#"{"v":1,"op":{"op":"Mkdir","share":"F","parent":"a//b","name":"x"}}"#,
            br#"{"v":1,"op":{"op":"Trash","share":"F","paths":["a"]},"x":2}"#,
        ] {
            assert!(decode_request(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        // Fuzzing-Fund: 64 Zeichen mit Mehrbyte-Unicode ließen Hash::from_str abstürzen.
        let evil = "{\"v\":1,\"op\":{\"op\":\"Import\",\"share\":\"F\",\"dest_dir\":\"\",\"name\":\"a\",\"hash\":\"00000000000000000fff$fFf\\ufffffffffffffffwfff$000000000000000000\",\"size\":1}}";
        assert!(decode_request(evil.as_bytes()).is_err());
        assert!(parse_hash_hex(&"0".repeat(64)).is_some());
        assert!(parse_hash_hex(&"ä".repeat(32)).is_none());
        let too_many = RemoteOp::Trash { share: "F".into(), paths: vec!["a".into(); MAX_PATHS + 1] };
        assert!(too_many.validate().is_err());
    }

    #[test]
    fn symlinks_on_the_way_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("share");
        std::fs::create_dir_all(root.join("echt")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("link")).unwrap();
        assert_eq!(resolve_in_share(&root, "echt/x").unwrap(), root.join("echt/x"));
        assert_eq!(resolve_in_share(&root, "").unwrap(), root);
        let e = resolve_in_share(&root, "link/share").unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::PermissionDenied);
        assert!(resolve_in_share(&root, "../x").is_err());
        // Der Symlink selbst als Ziel ist erlaubt (Umbenennen/Löschen betrifft den Link)
        assert!(resolve_in_share(&root, "link").is_ok());
    }

    #[test]
    fn hourly_window() {
        let s = RemoteOpsState::default();
        assert!(s.try_take("a", 60, 100));
        assert!(s.try_take("a", 40, 100));
        assert!(!s.try_take("a", 1, 100));
        assert!(s.try_take("a", 0, 100), "nicht verändernde Operationen zählen nicht");
        assert!(s.try_take("b", 100, 100), "je Gerät getrennt");
        assert!(!s.try_take("c", 101, 100));
    }
}
#[cfg(test)]
mod ticket_parsing {
    #[test]
    fn ticket_parse_with_unicode_does_not_panic() {
        for s in [
            "blob".to_string() + &"ä".repeat(60),
            "blob".to_string() + &"\u{ffff}".repeat(40),
            "ä".repeat(32),
            "x".repeat(64),
        ] {
            let r = std::panic::catch_unwind(|| s.parse::<iroh_blobs::ticket::BlobTicket>().is_ok());
            assert!(r.is_ok(), "Panic beim Ticket-Parsen: {s:?}");
        }
    }
}
