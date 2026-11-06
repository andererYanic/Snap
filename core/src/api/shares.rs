//! Operationen für Ordner-Freigaben.

use crate::api::auth::validate_grant_folder;
use crate::api_bail;
use crate::daemon::*;
use crate::ipc::{Response, ScanResult};
use crate::ledger::validate_share_name;
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;

pub(crate) async fn share_add(
    inner: &Arc<DaemonInner>,
    path: &Path,
    name: Option<String>,
    devices: Option<Vec<String>>,
) -> Result<Response> {
    let root = validate_grant_folder(path, &inner.protected_paths())?;
    let root_str = root.to_str().map(str::to_string);
    let Some(root_str) = root_str else { api_bail!(InvalidInput, "Ordnerpfad ist kein gültiges UTF-8") };
    let name = name.unwrap_or_else(|| root.file_name().and_then(|n| n.to_str()).unwrap_or("Freigabe").to_string());
    if validate_share_name(&name).is_err() {
        api_bail!(InvalidInput, "Ungültiger Freigabe-Name '{name}' (max. 64 Zeichen, kein '/')");
    }
    // Ohne Angabe: alle gekoppelten Geräte, auch künftige (auto_grant).
    let (auto_grant, targets) = match devices {
        None => (true, inner.db.list_devices()?),
        Some(list) => {
            let mut out = Vec::new();
            for q in list {
                let found = inner.db.find_devices(&q)?;
                match found.len() {
                    1 => out.extend(found),
                    0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{q}'"),
                    _ => api_bail!(Ambiguous, "Gerät '{q}' ist mehrdeutig"),
                }
            }
            (false, out)
        }
    };
    let ids: Vec<String> = targets.iter().map(|d| d.node_id.clone()).collect();
    let share = inner.db.create_share(&name, &root_str, auto_grant, &ids)?;
    // Beobachten + erster Scan im Hintergrund (große Ordner dauern).
    let (inner2, share2) = (inner.clone(), share.clone());
    tokio::spawn(async move { start_share(&inner2, share2).await });
    Ok(Response::ShareAdded { share, granted: targets.into_iter().map(|d| d.name).collect() })
}

pub(crate) fn share_remove(inner: &Arc<DaemonInner>, name: &str) -> Result<Response> {
    let Some(share) = inner.db.list_shares()?.into_iter().find(|s| s.name == name) else {
        api_bail!(NotFound, "Keine Freigabe '{name}'. Siehe `space-daemon share list`.");
    };
    inner.watchers.lock().unwrap().remove(&share.id);
    // Dateidaten (inkl. Freigabe-Name) festhalten, bevor die Freigabe verschwindet –
    // die Löschmarkierungen im Ledger müssen Freigabe und Pfad tragen.
    let records: Vec<(i64, crate::database::FileRecord)> = inner
        .db
        .known_share_files(share.id)?
        .values()
        .filter_map(|k| inner.db.file_by_id(k.id).ok().flatten().map(|r| (k.id, r)))
        .collect();
    let ids = inner.db.remove_share(share.id)?;
    for (id, record) in &records {
        inner.record_entry(*id, record, true)?;
    }
    Ok(Response::Success {
        message: format!(
            "Freigabe '{name}' entfernt ({} Dateien). Gekoppelte Geräte sehen sie nach dem nächsten Abgleich nicht mehr; \
             die Dateien selbst bleiben unverändert auf der Platte.",
            ids.len()
        ),
    })
}

pub(crate) async fn share_rescan(inner: &Arc<DaemonInner>, name: Option<String>) -> Result<Response> {
    let shares: Vec<_> = inner.db.list_shares()?.into_iter().filter(|s| name.as_deref().is_none_or(|n| s.name == n)).collect();
    if shares.is_empty() {
        api_bail!(NotFound, "Keine passende Freigabe gefunden.");
    }
    let mut results = Vec::new();
    for share in shares {
        let st = crate::shares::scan_share(inner, &share, None).await?;
        results.push(ScanResult {
            name: share.name,
            added_or_changed: st.added_or_changed,
            deleted: st.deleted,
            unchanged: st.unchanged,
            skipped: st.skipped,
        });
    }
    Ok(Response::Scanned { results })
}
