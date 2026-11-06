//! Downloads: Ziel auflösen, laden, atomar ablegen.

use crate::api::files::resolve_unique_hash;
use crate::api_bail;
use crate::daemon::*;
use crate::database::{NewJob, RemoteFile};
use crate::ipc::Response;
use crate::network::{endpoint_addr, FetchError};
use anyhow::{bail, Context, Result};
use iroh::EndpointId;
use iroh_blobs::{ticket::BlobTicket, BlobFormat, Hash};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ----------------------------------------------------------------------
// Download
// ----------------------------------------------------------------------

/// Was der Nutzer herunterladen will.
pub(crate) struct DownloadTarget {
    ticket: BlobTicket,
    /// Dateiname für den Standard-Zielordner.
    default_name: String,
}

/// Löst ein Download-Ziel auf: Ticket, Hash(-Präfix) oder Dateiname.
/// Gesucht wird erst im eigenen Katalog, dann in den Dateilisten gekoppelter Geräte.
pub(crate) fn resolve_download_target(inner: &DaemonInner, target: &str) -> Result<DownloadTarget> {
    let target = target.trim();
    let db = &inner.db;
    let self_id = inner.self_id();

    // 1. Ticket
    if let Ok(ticket) = target.parse::<BlobTicket>() {
        if ticket.format() != BlobFormat::Raw {
            api_bail!(InvalidInput, "Sammlungen (HashSeq) werden noch nicht unterstützt.");
        }
        let hash_hex = ticket.hash().to_string();
        let known_name =
            db.find_by_hash_prefix(&hash_hex).ok().and_then(|m| m.into_iter().next()).map(|f| f.file_name).or_else(
                || {
                    db.find_remote_by_hash_prefix(&hash_hex, &self_id)
                        .ok()
                        .and_then(|m| m.into_iter().next())
                        .map(|f| f.name)
                },
            );
        let default_name =
            known_name.map(|n| sanitize_file_name(&n)).unwrap_or_else(|| format!("snap-{}", &hash_hex[..12]));
        return Ok(DownloadTarget { ticket, default_name });
    }

    // 2. Hash(-Präfix)
    let looks_like_hash = target.len() >= 6 && target.len() <= 64 && target.bytes().all(|b| b.is_ascii_hexdigit());
    if looks_like_hash {
        let local = db.find_by_hash_prefix(target)?;
        if !local.is_empty() {
            let record = resolve_unique_hash(db, target)?;
            let ticket: BlobTicket = record.ticket.parse().context("Gespeichertes Ticket ist ungültig")?;
            return Ok(DownloadTarget { ticket, default_name: sanitize_file_name(&record.file_name) });
        }
        let remote = db.find_remote_by_hash_prefix(target, &self_id)?;
        if !remote.is_empty() {
            return remote_target(inner, remote, target);
        }
    }

    // 3. Pfad in einer Freigabe eines anderen Geräts: "Freigabe/unter/datei.jpg"
    if let Some((share, rel)) = target.split_once('/') {
        let remote = db.find_remote_by_share_path(share, rel, &self_id)?;
        if !remote.is_empty() {
            return remote_target(inner, remote, target);
        }
    }

    // 4. Dateiname aus den Listen gekoppelter Geräte
    let remote = db.find_remote_by_name(target, &self_id)?;
    if !remote.is_empty() {
        return remote_target(inner, remote, target);
    }
    if looks_like_hash {
        api_bail!(NotFound, "Keine Datei mit Hash '{target}' bekannt (weder eigene noch von gekoppelten Geräten).");
    }
    api_bail!(
        NotFound,
        "'{target}' ist weder ein Ticket noch ein bekannter Hash oder Dateiname.\n   \
         Tipp: `space-daemon list` zeigt alle Dateien, `space-daemon sync` aktualisiert die Listen."
    )
}

/// Wählt aus passenden Ledger-Einträgen eine Quelle. Mehrdeutige Inhalte führen zu
/// einer Fehlermeldung; derselbe Inhalt auf mehreren Geräten wird bevorzugt von
/// einem zuletzt erreichbaren Gerät geladen.
pub(crate) fn remote_target(inner: &DaemonInner, matches: Vec<RemoteFile>, query: &str) -> Result<DownloadTarget> {
    let distinct: HashSet<&str> = matches.iter().map(|f| f.hash.as_str()).collect();
    if distinct.len() > 1 {
        let list: Vec<String> =
            matches.iter().map(|f| format!("  {}  {}  ({})", &f.hash[..16], f.name, device_label(f))).collect();
        api_bail!(Ambiguous, "'{query}' ist mehrdeutig – bitte den Hash angeben:\n{}", list.join("\n"));
    }
    let peers = inner.db.list_peers().unwrap_or_default();
    let score = |f: &RemoteFile| {
        peers
            .iter()
            .find(|p| p.node_id == f.origin)
            .map(|p| (p.status == "online", p.last_seen.unwrap_or(0)))
            .unwrap_or((false, 0))
    };
    let best = matches.iter().max_by_key(|f| score(f)).expect("nicht leer");
    let origin: EndpointId = best.origin.parse().context("Ungültige Geräte-ID im Ledger")?;
    let hash: Hash = best.hash.parse().context("Ungültiger Hash im Ledger")?;
    let hints = inner.db.addr_hints(&best.origin).unwrap_or_default();
    let ticket = BlobTicket::new(endpoint_addr(origin, &hints), hash, BlobFormat::Raw);
    Ok(DownloadTarget { ticket, default_name: sanitize_file_name(&best.name) })
}

pub(crate) fn device_label(f: &RemoteFile) -> String {
    f.origin_name.clone().unwrap_or_else(|| format!("Gerät {}", &f.origin[..10.min(f.origin.len())]))
}

/// Entfernt Pfadbestandteile aus einem Dateinamen (Schutz vor `../`-Tricks).
pub fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("").trim_start_matches('.');
    if base.is_empty() {
        "unbenannt".to_string()
    } else {
        base.to_string()
    }
}

pub(crate) async fn download(
    inner: &DaemonInner,
    target: &str,
    output: Option<PathBuf>,
    force: bool,
) -> Result<Response> {
    let DownloadTarget { ticket, default_name } = resolve_download_target(inner, target)?;

    // Ziel vor jedem Netzwerkzugriff prüfen, damit Fehler sofort sichtbar sind.
    let (output_path, explicit) = match output {
        Some(out) => {
            if !out.is_absolute() {
                api_bail!(InvalidInput, "Zielpfad muss absolut sein: {}", out.display());
            }
            let out = if out.is_dir() { out.join(&default_name) } else { out };
            if out.exists() && !force {
                api_bail!(AlreadyExists, "Ziel existiert bereits: {} (mit --force überschreiben)", out.display());
            }
            if out.is_dir() {
                api_bail!(InvalidInput, "Ziel ist ein Verzeichnis: {}", out.display());
            }
            (out, true)
        }
        None => (absolute(&inner.config.download_dir)?.join(&default_name), false),
    };

    let peer = ticket.addr().id;
    let is_self = peer == inner.network.node_id();
    let (fetched, _source) = fetch_with_fallback(inner, &ticket).await;
    match fetched {
        Ok(temp) => {
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
                action: "DOWNLOAD",
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
            api_bail!(Rejected, "{e}")
        }
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

pub(crate) fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p).with_context(|| format!("Pfad {} nicht auflösbar", p.display()))
}

/// Exportiert den Blob atomar an den Zielpfad:
/// erst in eine temporäre Datei im Zielordner, dann ohne Überschreiben verlinken
/// (bzw. mit `force` atomar ersetzen). Ohne expliziten Zielpfad wird bei
/// Namenskonflikten " (1)", " (2)" … angehängt.
pub(crate) async fn finalize_output(
    inner: &DaemonInner,
    hash: Hash,
    output: &Path,
    explicit: bool,
    force: bool,
) -> Result<PathBuf> {
    let dir = output.parent().context("Zielpfad hat keinen Ordner")?.to_path_buf();
    tokio::fs::create_dir_all(&dir).await.with_context(|| format!("Zielordner {} nicht anlegbar", dir.display()))?;
    let file_name =
        output.file_name().and_then(|n| n.to_str()).context("Ungültiger Dateiname im Zielpfad")?.to_string();

    let tmp = dir.join(format!(".{file_name}.{:08x}.snap-part", fastrand::u32(..)));
    if let Err(e) = inner.network.export(hash, &tmp).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e).with_context(|| format!("Export nach {} fehlgeschlagen", dir.display()));
    }

    let result = place_file(&tmp, output, explicit, force).await;
    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

pub(crate) async fn place_file(tmp: &Path, output: &Path, explicit: bool, force: bool) -> Result<PathBuf> {
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
                    api_bail!(
                        AlreadyExists,
                        "Ziel existiert bereits: {} (mit --force überschreiben)",
                        output.display()
                    );
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
pub(crate) async fn link_no_overwrite(tmp: &Path, target: &Path) -> std::io::Result<()> {
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
pub(crate) fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("datei");
    let name = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem} ({n}).{ext}"),
        None => format!("{stem} ({n})"),
    };
    path.with_file_name(name)
}

/// Holt einen Blob beim Anbieter aus dem Ticket. Ist dieser nicht erreichbar, werden andere
/// Geräte versucht, die laut Ledger denselben Inhalt (gleicher Hash) anbieten – zuerst die
/// zuletzt erreichbaren. Ob ein Gerät liefert, entscheidet es selbst (Rechte).
/// Rückgabe: Ergebnis und Node-ID der tatsächlichen Quelle.
pub(crate) async fn fetch_with_fallback(
    inner: &DaemonInner,
    ticket: &BlobTicket,
) -> (Result<iroh_blobs::api::TempTag, FetchError>, String) {
    let primary = ticket.addr().id.to_string();
    let is_self = ticket.addr().id == inner.network.node_id();
    let first = inner.network.fetch(ticket).await;
    match &first {
        Ok(_) => {
            if !is_self {
                let _ = inner.db.mark_peer_online(&primary);
            }
            return (first, primary);
        }
        Err(e) if !e.is_retryable() => return (first, primary),
        Err(_) => {}
    }
    let self_id = inner.self_id();
    let hash_hex = ticket.hash().to_string();
    let mut origins: Vec<String> = inner
        .db
        .find_remote_by_hash_prefix(&hash_hex, &self_id)
        .unwrap_or_default()
        .into_iter()
        .map(|f| f.origin)
        .filter(|o| *o != primary)
        .collect();
    origins.sort();
    origins.dedup();
    let peers = inner.db.list_peers().unwrap_or_default();
    origins.sort_by_key(|o| {
        let p = peers.iter().find(|p| p.node_id == *o);
        std::cmp::Reverse((p.is_some_and(|p| p.status == "online"), p.and_then(|p| p.last_seen).unwrap_or(0)))
    });
    for origin in origins {
        let Ok(id) = origin.parse::<EndpointId>() else { continue };
        let hints = inner.db.addr_hints(&origin).unwrap_or_default();
        let alt = BlobTicket::new(endpoint_addr(id, &hints), ticket.hash(), BlobFormat::Raw);
        match inner.network.fetch(&alt).await {
            Ok(temp) => {
                let _ = inner.db.mark_peer_online(&origin);
                tracing::info!("Quelle {} nicht erreichbar – geladen von {}", &primary[..10], &origin[..10]);
                return (Ok(temp), origin);
            }
            Err(e) if e.is_retryable() => {
                let _ = inner.db.mark_peer_unreachable(&origin, false);
            }
            Err(_) => {} // kein Recht oder Inhalt nicht (mehr) vorhanden
        }
    }
    (first, primary)
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
