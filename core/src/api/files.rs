//! Operationen auf dem Dateikatalog: freigeben, löschen, Tickets, Katalog.

use crate::api_bail;
use crate::daemon::*;
use crate::database::{now_unix, DatabaseManager};
use crate::ipc::Response;
use crate::sync::spawn_sync_all;
use anyhow::{Context, Result};
use iroh_blobs::Hash;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ----------------------------------------------------------------------
// Publish / Ticket / Delete
// ----------------------------------------------------------------------

pub(crate) async fn publish(inner: &DaemonInner, path: &Path) -> Result<Response> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let canon =
        tokio::fs::canonicalize(path).await.with_context(|| format!("Datei nicht gefunden: {}", path.display()))?;
    let meta = tokio::fs::metadata(&canon).await?;
    if !meta.is_file() {
        api_bail!(InvalidInput, "Nur einzelne Dateien können freigegeben werden: {}", canon.display());
    }
    let path_str =
        canon.to_str().with_context(|| format!("Pfad ist kein gültiges UTF-8: {}", canon.display()))?.to_string();
    let file_name = canon.file_name().and_then(|n| n.to_str()).unwrap_or("unbenannt").to_string();

    let (hash, ticket) = inner
        .network
        .publish_file(&canon)
        .await
        .with_context(|| format!("Datei {} konnte nicht importiert werden", canon.display()))?;
    let hash_hex = hash.to_string();
    let (already_shared, file_id) = inner.db.upsert_file(&path_str, &hash_hex, &file_name, meta.len(), &ticket)?;
    if !already_shared {
        if let Some(record) = inner.db.file_by_id(file_id)? {
            inner.record_entry(file_id, &record, false)?;
        }
    }

    Ok(Response::Published { ticket, hash: hash_hex, file_name, already_shared })
}

/// Löst einen Hash-Präfix eindeutig im lokalen Katalog auf.
pub(crate) fn resolve_unique_hash(db: &DatabaseManager, prefix: &str) -> Result<crate::database::FileRecord> {
    let matches = db.find_by_hash_prefix(prefix)?;
    let distinct: HashSet<&str> = matches.iter().map(|f| f.blake3_hash.as_str()).collect();
    match distinct.len() {
        0 => api_bail!(NotFound, "Keine freigegebene Datei mit Hash '{prefix}' im lokalen Katalog."),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let list: Vec<String> =
                matches.iter().map(|f| format!("  {}  {}", &f.blake3_hash[..16], f.file_name)).collect();
            api_bail!(Ambiguous, "Hash '{prefix}' ist mehrdeutig – bitte mehr Zeichen angeben:\n{}", list.join("\n"))
        }
    }
}

pub(crate) fn ticket(inner: &DaemonInner, hash: &str) -> Result<Response> {
    let record = resolve_unique_hash(&inner.db, hash)?;
    let hash: Hash = record.blake3_hash.parse()?;
    Ok(Response::Ticket { ticket: inner.network.ticket_for(hash) })
}

pub(crate) async fn delete(inner: &DaemonInner, hash: Option<String>, path: Option<PathBuf>) -> Result<Response> {
    let ids = match (hash, path) {
        (Some(h), None) => {
            let record = resolve_unique_hash(&inner.db, &h)?;
            inner.db.soft_delete_by_hash(&record.blake3_hash)?
        }
        (None, Some(p)) => {
            if !p.is_absolute() {
                api_bail!(InvalidInput, "Pfad muss absolut sein: {}", p.display());
            }
            let p = tokio::fs::canonicalize(&p).await.unwrap_or(p);
            let p = p.to_str().context("Pfad ist kein gültiges UTF-8")?;
            inner.db.soft_delete_by_path(p)?
        }
        _ => api_bail!(InvalidInput, "Genau eines von --hash oder --path angeben."),
    };
    // Löschmarkierung (Tombstone) ins Ledger, damit gekoppelte Geräte sie übernehmen.
    for id in &ids {
        if let Some(record) = inner.db.file_by_id(*id)? {
            inner.record_entry(*id, &record, true)?;
        }
    }
    let count = ids.len();
    if count == 0 {
        api_bail!(NotFound, "Keine passende freigegebene Datei gefunden.");
    }
    let days = inner.config.retention_days;
    Ok(Response::Success {
        message: format!(
            "{count} Freigabe(n) entfernt. Die Datei wird ab sofort nicht mehr ausgeliefert \
             und nach {days} Tag(en) endgültig aus dem Blob-Store gelöscht."
        ),
    })
}

pub(crate) fn catalog(inner: &DaemonInner) -> Result<Response> {
    let devices = inner.db.list_devices()?;
    let now = now_unix();
    let stale = devices.iter().any(|d| d.last_sync_at.is_none_or(|t| now - t > CATALOG_MAX_AGE_SECS));
    let mut syncing = inner.sync_lock.try_lock().is_err();
    if stale && !syncing && !devices.is_empty() {
        spawn_sync_all(inner);
        syncing = true;
    }
    Ok(Response::Catalog {
        local: inner.db.list_files()?,
        remote: inner.db.remote_files(&inner.self_id())?,
        devices,
        peers: inner.db.list_peers()?,
        syncing,
    })
}

/// Ordner im Katalog eines gekoppelten Geräts (aus der lokalen Kopie, ohne Netzwerk).
pub(crate) fn remote_dir(
    inner: &DaemonInner,
    device: &str,
    share: Option<String>,
    dir: &str,
    after: Option<String>,
    limit: u32,
) -> Result<Response> {
    use crate::ipc::MAX_BROWSE_LIMIT;
    let found = inner.db.find_devices(device)?;
    let node = match found.as_slice() {
        [d] => d.node_id.clone(),
        [] => api_bail!(NotFound, "Kein gekoppeltes Gerät '{device}'"),
        _ => api_bail!(Ambiguous, "Gerät '{device}' ist mehrdeutig"),
    };
    let bad = |s: &str| s.len() > 4096 || s.contains('\0');
    if share.as_deref().is_some_and(bad) || bad(dir) || after.as_deref().is_some_and(|a| a.len() > 8192) {
        api_bail!(InvalidInput, "Ungültiger Pfad");
    }
    // Ordnerpfad normalisieren: ohne führende/abschließende '/', keine leeren Teile, kein "..".
    let dir = dir.trim_matches('/');
    if dir.split('/').any(|p| p.is_empty() || p == "." || p == "..") && !dir.is_empty() {
        api_bail!(InvalidInput, "Ungültiger Pfad");
    }
    if share.is_none() && !dir.is_empty() {
        api_bail!(InvalidInput, "Ordner nur innerhalb einer Freigabe");
    }
    let limit = match limit {
        0 => 200,
        n => n.min(MAX_BROWSE_LIMIT),
    } as usize;
    let page = inner.db.remote_dir(&node, share.as_deref(), dir, after.as_deref(), limit)?;
    Ok(Response::RemoteDir(page))
}

/// Seitenweise Katalogabfrage für Oberflächen.
pub(crate) fn browse(inner: &DaemonInner, q: crate::ipc::BrowseQuery) -> Result<Response> {
    use crate::ipc::MAX_BROWSE_LIMIT;
    let limit = match q.limit {
        0 => 100,
        n => n.min(MAX_BROWSE_LIMIT),
    };
    let device_id;
    let device = match q.device.as_deref() {
        None => None,
        Some("self") => Some(None),
        Some(other) => {
            let found = inner.db.find_devices(other)?;
            match found.as_slice() {
                [d] => {
                    device_id = d.node_id.clone();
                    Some(Some(device_id.as_str()))
                }
                [] => api_bail!(NotFound, "Kein gekoppeltes Gerät '{other}'"),
                _ => api_bail!(Ambiguous, "Gerät '{other}' ist mehrdeutig"),
            }
        }
    };
    if q.search.as_ref().is_some_and(|s| s.len() > 256) {
        api_bail!(InvalidInput, "Suchbegriff zu lang");
    }
    if q.offset > 100_000 {
        api_bail!(InvalidInput, "Offset zu groß – für große Kataloge `after` (Cursor) verwenden");
    }
    let (items, total, next) =
        inner.db.browse(&inner.self_id(), device, q.search.as_deref(), q.offset, limit, q.after.as_ref())?;
    Ok(Response::BrowsePage { items, total, next })
}
