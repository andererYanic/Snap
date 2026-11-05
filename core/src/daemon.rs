//! Der Daemon: IPC-Server, Request-Verarbeitung, Offline-Warteschlange und Aufräumen.

use crate::config::Config;
use crate::database::{now_unix, DatabaseManager, NewJob, PendingJob};
use crate::ipc::{read_frame, write_frame, DaemonInfo, Request, Response};
use crate::network::{AccessCheck, ConnectHook, FetchError, NetworkOptions, P2pNetwork};
use crate::secure_fs::{current_uid, ensure_private_dir, load_or_create_secret_key};
use anyhow::{bail, Context, Result};
use iroh_blobs::{ticket::BlobTicket, BlobFormat, Hash};
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

/// Zeit, die ein Client maximal zum Senden seiner Anfrage hat.
const IPC_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Intervall für das Aufräumen abgelaufener Papierkorb-Einträge.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

pub struct DaemonInner {
    pub config: Config,
    pub network: P2pNetwork,
    pub db: Arc<DatabaseManager>,
    /// Weckt den Warteschlangen-Worker (neuer Auftrag, Retry, Peer meldet sich).
    pub sync_notifier: Arc<Notify>,
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

        let network = P2pNetwork::start(
            NetworkOptions {
                blob_store_path: config.blob_store_path(),
                secret_key,
                connect_timeout: Duration::from_secs(config.connect_timeout_secs),
                bind_port: config.bind_port,
                use_relay: config.use_relay,
                loopback_only,
            },
            access,
            on_connect,
        )
        .await?;

        let listener = UnixListener::bind(&config.socket_path)
            .with_context(|| format!("IPC-Socket {} nicht anlegbar", config.socket_path.display()))?;
        std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o600))?;

        let inner = Arc::new(DaemonInner { config, network, db, sync_notifier });
        let daemon = Self { inner: inner.clone(), tasks: Arc::new(Mutex::new(Vec::new())) };

        let ipc = tokio::spawn(run_ipc_server(inner.clone(), listener));
        let worker = tokio::spawn(run_pending_pin_worker(inner.clone()));
        let pruner = tokio::spawn(run_pruner(inner.clone()));
        daemon.tasks.lock().unwrap().extend([ipc, worker, pruner]);

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
async fn prepare_socket_path(path: &Path) -> Result<()> {
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

async fn run_ipc_server(inner: Arc<DaemonInner>, listener: UnixListener) {
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

async fn handle_ipc_connection(inner: &DaemonInner, mut stream: UnixStream) -> Result<()> {
    // Nur Prozesse desselben Benutzers dürfen den Daemon steuern.
    let cred = stream.peer_cred().context("Peer-Credentials nicht lesbar")?;
    if cred.uid() != current_uid() {
        tracing::warn!("IPC-Verbindung von fremder uid {} abgelehnt", cred.uid());
        return Ok(());
    }
    let req: Request = tokio::time::timeout(IPC_READ_TIMEOUT, read_frame(&mut stream))
        .await
        .context("Zeitüberschreitung beim Lesen der Anfrage")??;
    let res = handle_request(inner, req).await;
    write_frame(&mut stream, &res).await
}

async fn handle_request(inner: &DaemonInner, req: Request) -> Response {
    let result = match req {
        Request::Publish { path } => publish(inner, &path).await,
        Request::Download { target, output, force } => download(inner, &target, output, force).await,
        Request::Delete { hash, path } => delete(inner, hash, path).await,
        Request::List => inner.db.list_files().map(|files| Response::FileList { files }),
        Request::Ticket { hash } => ticket(inner, &hash),
        Request::Queue => inner.db.list_jobs().map(|jobs| Response::Queue { jobs }),
        Request::QueueCancel { id } => match inner.db.cancel_job(id) {
            Ok(true) => Ok(Response::Success { message: format!("Auftrag {id} entfernt.") }),
            Ok(false) => Ok(Response::error(format!("Kein Auftrag mit ID {id}."))),
            Err(e) => Err(e),
        },
        Request::QueueRetry { id } => match inner.db.retry_job(id) {
            Ok(true) => {
                inner.sync_notifier.notify_one();
                Ok(Response::Success { message: format!("Auftrag {id} wird jetzt erneut versucht.") })
            }
            Ok(false) => Ok(Response::error(format!("Kein Auftrag mit ID {id}."))),
            Err(e) => Err(e),
        },
        Request::Peers => inner.db.list_peers().map(|peers| Response::Peers { peers }),
        Request::Info => info(inner),
    };
    result.unwrap_or_else(|e| Response::error(format!("{e:#}")))
}

fn info(inner: &DaemonInner) -> Result<Response> {
    let c = &inner.config;
    Ok(Response::Info(DaemonInfo {
        node_id: inner.network.node_id().to_string(),
        data_dir: c.data_dir.clone(),
        socket_path: c.socket_path.clone(),
        download_dir: c.download_dir.clone(),
        shared_files: inner.db.count_shared()?,
        pending_jobs: inner.db.count_waiting_jobs()?,
        use_relay: c.use_relay,
    }))
}

// ----------------------------------------------------------------------
// Publish / Ticket / Delete
// ----------------------------------------------------------------------

async fn publish(inner: &DaemonInner, path: &Path) -> Result<Response> {
    if !path.is_absolute() {
        bail!("Pfad muss absolut sein: {}", path.display());
    }
    let canon = tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("Datei nicht gefunden: {}", path.display()))?;
    let meta = tokio::fs::metadata(&canon).await?;
    if !meta.is_file() {
        bail!("Nur einzelne Dateien können freigegeben werden: {}", canon.display());
    }
    let path_str = canon
        .to_str()
        .with_context(|| format!("Pfad ist kein gültiges UTF-8: {}", canon.display()))?
        .to_string();
    let file_name = canon
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unbenannt")
        .to_string();

    let (hash, ticket) = inner
        .network
        .publish_file(&canon)
        .await
        .with_context(|| format!("Datei {} konnte nicht importiert werden", canon.display()))?;
    let hash_hex = hash.to_string();
    let already_shared = inner.db.upsert_file(&path_str, &hash_hex, &file_name, meta.len(), &ticket)?;

    Ok(Response::Published { ticket, hash: hash_hex, file_name, already_shared })
}

/// Löst einen Hash-Präfix eindeutig im lokalen Katalog auf.
fn resolve_unique_hash(db: &DatabaseManager, prefix: &str) -> Result<crate::database::FileRecord> {
    let matches = db.find_by_hash_prefix(prefix)?;
    let distinct: HashSet<&str> = matches.iter().map(|f| f.blake3_hash.as_str()).collect();
    match distinct.len() {
        0 => bail!("Keine freigegebene Datei mit Hash '{prefix}' im lokalen Katalog."),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let list: Vec<String> = matches
                .iter()
                .map(|f| format!("  {}  {}", &f.blake3_hash[..16], f.file_name))
                .collect();
            bail!("Hash '{prefix}' ist mehrdeutig – bitte mehr Zeichen angeben:\n{}", list.join("\n"))
        }
    }
}

fn ticket(inner: &DaemonInner, hash: &str) -> Result<Response> {
    let record = resolve_unique_hash(&inner.db, hash)?;
    let hash: Hash = record.blake3_hash.parse()?;
    Ok(Response::Ticket { ticket: inner.network.ticket_for(hash) })
}

async fn delete(inner: &DaemonInner, hash: Option<String>, path: Option<PathBuf>) -> Result<Response> {
    let count = match (hash, path) {
        (Some(h), None) => {
            let record = resolve_unique_hash(&inner.db, &h)?;
            inner.db.soft_delete_by_hash(&record.blake3_hash)?
        }
        (None, Some(p)) => {
            if !p.is_absolute() {
                bail!("Pfad muss absolut sein: {}", p.display());
            }
            let p = tokio::fs::canonicalize(&p).await.unwrap_or(p);
            let p = p.to_str().context("Pfad ist kein gültiges UTF-8")?;
            inner.db.soft_delete_by_path(p)?
        }
        _ => bail!("Genau eines von --hash oder --path angeben."),
    };
    if count == 0 {
        bail!("Keine passende freigegebene Datei gefunden.");
    }
    let days = inner.config.retention_days;
    Ok(Response::Success {
        message: format!(
            "{count} Freigabe(n) entfernt. Die Datei wird ab sofort nicht mehr ausgeliefert \
             und nach {days} Tag(en) endgültig aus dem Blob-Store gelöscht."
        ),
    })
}

// ----------------------------------------------------------------------
// Download
// ----------------------------------------------------------------------

/// Was der Nutzer herunterladen will.
struct DownloadTarget {
    ticket: BlobTicket,
    /// Dateiname für den Standard-Zielordner.
    default_name: String,
}

fn resolve_download_target(db: &DatabaseManager, target: &str) -> Result<DownloadTarget> {
    let target = target.trim();
    let looks_like_hash = target.len() <= 64 && target.bytes().all(|b| b.is_ascii_hexdigit());
    if looks_like_hash {
        let record = resolve_unique_hash(db, target)?;
        let ticket: BlobTicket = record.ticket.parse().context("Gespeichertes Ticket ist ungültig")?;
        return Ok(DownloadTarget { ticket, default_name: sanitize_file_name(&record.file_name) });
    }
    let ticket: BlobTicket = target
        .parse()
        .map_err(|_| anyhow::anyhow!("Weder ein gültiges Ticket noch ein Hash: '{target}'"))?;
    if ticket.format() != BlobFormat::Raw {
        bail!("Sammlungen (HashSeq) werden noch nicht unterstützt.");
    }
    let hash_hex = ticket.hash().to_string();
    // Kennen wir den Namen aus dem eigenen Katalog?
    let known_name = db
        .find_by_hash_prefix(&hash_hex)
        .ok()
        .and_then(|m| m.into_iter().next())
        .map(|f| sanitize_file_name(&f.file_name));
    let default_name = known_name.unwrap_or_else(|| format!("snap-{}", &hash_hex[..12]));
    Ok(DownloadTarget { ticket, default_name })
}

/// Entfernt Pfadbestandteile aus einem Dateinamen (Schutz vor `../`-Tricks).
fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .trim_start_matches('.');
    if base.is_empty() {
        "unbenannt".to_string()
    } else {
        base.to_string()
    }
}

async fn download(
    inner: &DaemonInner,
    target: &str,
    output: Option<PathBuf>,
    force: bool,
) -> Result<Response> {
    let DownloadTarget { ticket, default_name } = resolve_download_target(&inner.db, target)?;

    // Ziel vor jedem Netzwerkzugriff prüfen, damit Fehler sofort sichtbar sind.
    let (output_path, explicit) = match output {
        Some(out) => {
            if !out.is_absolute() {
                bail!("Zielpfad muss absolut sein: {}", out.display());
            }
            let out = if out.is_dir() { out.join(&default_name) } else { out };
            if out.exists() && !force {
                bail!("Ziel existiert bereits: {} (mit --force überschreiben)", out.display());
            }
            if out.is_dir() {
                bail!("Ziel ist ein Verzeichnis: {}", out.display());
            }
            (out, true)
        }
        None => (absolute(&inner.config.download_dir)?.join(&default_name), false),
    };

    let peer = ticket.addr().id;
    let is_self = peer == inner.network.node_id();
    match inner.network.fetch(&ticket).await {
        Ok(temp) => {
            if !is_self {
                inner.db.mark_peer_online(&peer.to_string())?;
            }
            let final_path = finalize_output(inner, ticket.hash(), &output_path, explicit, force).await;
            drop(temp);
            Ok(Response::Downloaded { path: final_path? })
        }
        Err(e) if e.is_retryable() => {
            let peer_str = peer.to_string();
            inner.db.mark_peer_unreachable(&peer_str, false)?;
            let output_str = output_path.to_str().context("Zielpfad ist kein gültiges UTF-8")?;
            let ticket_str = ticket.to_string();
            let hash_hex = ticket.hash().to_string();
            let reason = e.to_string();
            let (job_id, already_queued) = inner.db.enqueue_download(&NewJob {
                target_hash: &hash_hex,
                ticket: &ticket_str,
                peer_id: &peer_str,
                output_path: output_str,
                explicit_output: explicit,
                force,
                last_error: &reason,
            })?;
            inner.sync_notifier.notify_one();
            Ok(Response::Queued { job_id, peer: peer_str, reason, already_queued })
        }
        Err(e @ FetchError::Rejected(_)) => {
            if !is_self {
                inner.db.mark_peer_online(&peer.to_string())?;
            }
            bail!("{e}")
        }
        Err(e) => bail!("{e}"),
    }
}

fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p).with_context(|| format!("Pfad {} nicht auflösbar", p.display()))
}

/// Exportiert den Blob atomar an den Zielpfad:
/// erst in eine temporäre Datei im Zielordner, dann ohne Überschreiben verlinken
/// (bzw. mit `force` atomar ersetzen). Ohne expliziten Zielpfad wird bei
/// Namenskonflikten " (1)", " (2)" … angehängt.
async fn finalize_output(
    inner: &DaemonInner,
    hash: Hash,
    output: &Path,
    explicit: bool,
    force: bool,
) -> Result<PathBuf> {
    let dir = output.parent().context("Zielpfad hat keinen Ordner")?.to_path_buf();
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("Zielordner {} nicht anlegbar", dir.display()))?;
    let file_name = output
        .file_name()
        .and_then(|n| n.to_str())
        .context("Ungültiger Dateiname im Zielpfad")?
        .to_string();

    let tmp = dir.join(format!(".{file_name}.{:08x}.snap-part", fastrand::u32(..)));
    if let Err(e) = inner.network.export(hash, &tmp).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("Export nach {} fehlgeschlagen", dir.display()));
    }

    let result = place_file(&tmp, output, explicit, force).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

async fn place_file(tmp: &Path, output: &Path, explicit: bool, force: bool) -> Result<PathBuf> {
    if explicit && force {
        tokio::fs::rename(tmp, output)
            .await
            .with_context(|| format!("{} konnte nicht geschrieben werden", output.display()))?;
        return Ok(output.to_path_buf());
    }
    for n in 0..1000u32 {
        let candidate = if n == 0 { output.to_path_buf() } else { numbered(output, n) };
        match link_no_overwrite(tmp, &candidate).await {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if explicit {
                    bail!("Ziel existiert bereits: {} (mit --force überschreiben)", output.display());
                }
            }
            Err(e) => {
                return Err(e).with_context(|| format!("{} konnte nicht geschrieben werden", candidate.display()))
            }
        }
    }
    bail!("Kein freier Dateiname für {} gefunden", output.display())
}

/// Legt `target` an, ohne eine bestehende Datei zu überschreiben.
async fn link_no_overwrite(tmp: &Path, target: &Path) -> std::io::Result<()> {
    match tokio::fs::hard_link(tmp, target).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) => {
            // Dateisysteme ohne Hardlinks: bestmöglich ohne Überschreiben.
            if tokio::fs::symlink_metadata(target).await.is_ok() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            tokio::fs::copy(tmp, target).await.map(|_| ())
        }
    }
}

/// `datei.txt` → `datei (n).txt`
fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("datei");
    let name = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem} ({n}).{ext}"),
        None => format!("{stem} ({n})"),
    };
    path.with_file_name(name)
}

// ----------------------------------------------------------------------
// Offline-Warteschlange (Hybrid: Notify + Backoff pro Peer)
// ----------------------------------------------------------------------

/// Hybrid-Worker:
/// - Warteschlange leer → schläft unbegrenzt bis `notify_one()`.
/// - Aufträge vorhanden → schläft bis zum nächsten fälligen Versuch (Backoff des
///   Peers mit Jitter) oder bis `notify_one()`, je nachdem was zuerst eintritt.
///
/// Es werden keine Pings gesendet: Ein Versuch ist immer ein echter Download.
async fn run_pending_pin_worker(inner: Arc<DaemonInner>) {
    loop {
        let processed = process_due_jobs(&inner).await;

        let next = match inner.db.next_wakeup() {
            Ok(n) => n,
            Err(e) => {
                tracing::error!("Warteschlange nicht lesbar: {e:#}");
                Some(now_unix() + 60)
            }
        };
        match next {
            None => inner.sync_notifier.notified().await,
            Some(at) => {
                let mut secs = (at - now_unix()).max(0) as u64;
                if secs == 0 && processed {
                    // Schutz vor Endlosschleifen bei unerwarteten Zuständen.
                    secs = 1;
                }
                tokio::select! {
                    _ = inner.sync_notifier.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(secs)) => {}
                }
            }
        }
    }
}

/// Arbeitet alle fälligen Aufträge ab. Ist ein Peer nicht erreichbar, werden
/// seine übrigen Aufträge in dieser Runde übersprungen.
async fn process_due_jobs(inner: &DaemonInner) -> bool {
    let jobs = match inner.db.due_jobs(now_unix()) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("Fällige Aufträge nicht lesbar: {e:#}");
            return false;
        }
    };
    let processed = !jobs.is_empty();
    let mut unreachable: HashSet<String> = HashSet::new();
    for job in jobs {
        if unreachable.contains(&job.peer_id) {
            continue;
        }
        if let Err(e) = process_job(inner, &job, &mut unreachable).await {
            tracing::error!("Auftrag {} fehlgeschlagen: {e:#}", job.id);
        }
    }
    processed
}

async fn process_job(inner: &DaemonInner, job: &PendingJob, unreachable: &mut HashSet<String>) -> Result<()> {
    let ticket: BlobTicket = match job.ticket.parse() {
        Ok(t) => t,
        Err(e) => {
            inner.db.job_failed(job.id, &format!("Ungültiges Ticket: {e}"))?;
            return Ok(());
        }
    };
    match inner.network.fetch(&ticket).await {
        Ok(temp) => {
            inner.db.mark_peer_online(&job.peer_id)?;
            let output = PathBuf::from(&job.output_path);
            let res = finalize_output(inner, ticket.hash(), &output, job.explicit_output, job.force).await;
            drop(temp);
            match res {
                Ok(path) => {
                    inner.db.complete_job(job.id)?;
                    tracing::info!("✅ Hintergrund-Download abgeschlossen: {}", path.display());
                }
                Err(e) => {
                    tracing::warn!("Auftrag {}: {e:#}", job.id);
                    inner.db.job_failed(job.id, &format!("{e:#}"))?;
                }
            }
        }
        Err(e) if e.is_retryable() => {
            let next = inner.db.mark_peer_unreachable(&job.peer_id, true)?;
            inner.db.job_attempt_failed(job.id, &e.to_string())?;
            unreachable.insert(job.peer_id.clone());
            tracing::info!(
                "Peer {} weiterhin nicht erreichbar – nächster Versuch in {} s",
                short(&job.peer_id),
                (next - now_unix()).max(0)
            );
        }
        Err(e) => {
            if matches!(e, FetchError::Rejected(_)) {
                inner.db.mark_peer_online(&job.peer_id)?;
            }
            tracing::warn!("Auftrag {} endgültig fehlgeschlagen: {e}", job.id);
            inner.db.job_failed(job.id, &e.to_string())?;
        }
    }
    Ok(())
}

fn short(s: &str) -> &str {
    &s[..s.len().min(10)]
}

// ----------------------------------------------------------------------
// Aufräumen (Papierkorb → Blob-Store)
// ----------------------------------------------------------------------

async fn run_pruner(inner: Arc<DaemonInner>) {
    let mut interval = tokio::time::interval(PRUNE_INTERVAL);
    loop {
        interval.tick().await;
        if let Err(e) = prune_once(&inner).await {
            tracing::error!("Aufräumen fehlgeschlagen: {e:#}");
        }
    }
}

async fn prune_once(inner: &DaemonInner) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbering() {
        assert_eq!(numbered(Path::new("/d/a.txt"), 1), PathBuf::from("/d/a (1).txt"));
        assert_eq!(numbered(Path::new("/d/a"), 2), PathBuf::from("/d/a (2)"));
        assert_eq!(numbered(Path::new("/d/a.tar.gz"), 1), PathBuf::from("/d/a.tar (1).gz"));
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name(".bashrc"), "bashrc");
        assert_eq!(sanitize_file_name(".."), "unbenannt");
        assert_eq!(sanitize_file_name(""), "unbenannt");
        assert_eq!(sanitize_file_name("foto.jpg"), "foto.jpg");
    }

    #[tokio::test]
    async fn place_file_never_overwrites_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("tmp");
        std::fs::write(&tmp, b"neu").unwrap();
        let target = dir.path().join("a.txt");
        std::fs::write(&target, b"alt").unwrap();

        // Implizit: neuer Name
        let p = place_file(&tmp, &target, false, false).await.unwrap();
        assert_eq!(p, dir.path().join("a (1).txt"));
        assert_eq!(std::fs::read(&target).unwrap(), b"alt");

        // Explizit ohne force: Fehler
        assert!(place_file(&tmp, &target, true, false).await.is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"alt");

        // Explizit mit force: ersetzt
        place_file(&tmp, &target, true, true).await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"neu");
    }
}
