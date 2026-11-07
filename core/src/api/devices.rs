//! Pairing-Operationen.

use crate::api_bail;
use crate::daemon::*;
use crate::ipc::Response;
use anyhow::Result;

pub(crate) async fn pair_invite(inner: &DaemonInner) -> Result<Response> {
    // Direkte Adressen nur ohne Relais in den Code aufnehmen (sonst kein IP-Leak nötig).
    let addrs = if inner.config.use_relay { Vec::new() } else { inner.network.direct_addrs() };
    let invite = inner.pairing.state.create_invite(inner.network.node_id(), addrs).await?;
    Ok(Response::PairInvite { code: invite.encode(), expires_at: invite.expires_at })
}

pub(crate) fn pair_remove(inner: &DaemonInner, query: &str) -> Result<Response> {
    let found = inner.db.find_devices(query)?;
    match found.as_slice() {
        [] => api_bail!(NotFound, "Kein gekoppeltes Gerät '{query}' gefunden. Siehe `space-daemon pair list`."),
        [d] => {
            inner.db.remove_device(&d.node_id)?;
            Ok(Response::Success {
                message: format!(
                    "'{}' entkoppelt. Das Gerät kann deine Dateiliste nicht mehr abrufen; \
                     seine Einträge wurden hier entfernt.",
                    d.name
                ),
            })
        }
        _ => api_bail!(Ambiguous, "'{query}' ist mehrdeutig – bitte mehr Zeichen der Node-ID angeben."),
    }
}

fn find_one(inner: &DaemonInner, query: &str) -> Result<crate::database::DeviceInfo> {
    let found = inner.db.find_devices(query)?;
    match found.len() {
        0 => api_bail!(NotFound, "Kein gekoppeltes Gerät '{query}' gefunden. Siehe `space-daemon pair list`."),
        1 => Ok(found.into_iter().next().unwrap()),
        _ => api_bail!(Ambiguous, "'{query}' ist mehrdeutig – bitte mehr Zeichen der Node-ID angeben."),
    }
}

/// Gekoppelte Geräte mit ihren Rechten.
pub(crate) fn pair_list(inner: &DaemonInner) -> Result<Response> {
    let mut devices = inner.db.list_devices()?;
    for d in &mut devices {
        d.grants = inner.db.grants_of(&d.node_id)?;
        d.online = inner.live.is_connected(&d.node_id);
        d.storage = inner.live.storage_of(&d.node_id);
    }
    Ok(Response::Devices { devices })
}

pub(crate) fn pair_grant(
    inner: &DaemonInner,
    device: &str,
    capability: &str,
    folder: Option<std::path::PathBuf>,
) -> Result<Response> {
    use super::auth::{validate_grant_folder, Capability};
    let cap = Capability::parse(capability)?;
    if cap == Capability::Admin {
        api_bail!(PermissionDenied, "Fernverwaltung (admin) ist noch nicht verfügbar – sie wird später nur mit Bestätigung am Gerät möglich sein.");
    }
    let d = find_one(inner, device)?;
    let folder = match (cap.needs_folder(), folder) {
        (true, Some(f)) => Some(validate_grant_folder(&f, &inner.protected_paths())?),
        (true, None) => api_bail!(InvalidInput, "Recht '{}' braucht einen Ordner (--folder)", cap.as_str()),
        (false, Some(_)) => api_bail!(InvalidInput, "Recht '{}' gilt ohne Ordner", cap.as_str()),
        (false, None) => None,
    };
    let folder_str = folder.as_ref().map(|f| f.to_string_lossy().into_owned());
    let added = inner.db.add_grant(&d.node_id, cap.as_str(), folder_str.as_deref())?;
    let wo = folder_str.map(|f| format!(" für {f}")).unwrap_or_default();
    Ok(Response::Success {
        message: if added {
            format!("'{}' hat jetzt das Recht '{}'{wo}.", d.name, cap.as_str())
        } else {
            format!("'{}' hatte das Recht '{}'{wo} bereits.", d.name, cap.as_str())
        },
    })
}

pub(crate) fn pair_revoke(
    inner: &DaemonInner,
    device: &str,
    capability: &str,
    folder: Option<std::path::PathBuf>,
) -> Result<Response> {
    let cap = super::auth::Capability::parse(capability)?;
    let d = find_one(inner, device)?;
    let folder = folder.map(|f| std::fs::canonicalize(&f).unwrap_or(f).to_string_lossy().into_owned());
    let n = inner.db.remove_grant(&d.node_id, cap.as_str(), folder.as_deref())?;
    if n == 0 {
        api_bail!(NotFound, "'{}' hat dieses Recht nicht.", d.name);
    }
    Ok(Response::Success { message: format!("Recht '{}' von '{}' entzogen.", cap.as_str(), d.name) })
}
