//! Pairing-Operationen.

use crate::ipc::Response;
use anyhow::{bail, Result};
use crate::daemon::*;

pub(crate) async fn pair_invite(inner: &DaemonInner) -> Result<Response> {
    // Direkte Adressen nur ohne Relais in den Code aufnehmen (sonst kein IP-Leak nötig).
    let addrs = if inner.config.use_relay { Vec::new() } else { inner.network.direct_addrs() };
    let invite = inner.pairing.state.create_invite(inner.network.node_id(), addrs).await?;
    Ok(Response::PairInvite { code: invite.encode(), expires_at: invite.expires_at })
}

pub(crate) fn pair_remove(inner: &DaemonInner, query: &str) -> Result<Response> {
    let found = inner.db.find_devices(query)?;
    match found.as_slice() {
        [] => bail!("Kein gekoppeltes Gerät '{query}' gefunden. Siehe `space-daemon pair list`."),
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
        _ => bail!("'{query}' ist mehrdeutig – bitte mehr Zeichen der Node-ID angeben."),
    }
}

