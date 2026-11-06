//! Operationen für Smart-Pinning.

use crate::api_bail;
use crate::daemon::*;
use crate::database::PinRow;
use crate::ipc::{PinInfo, Response};
use crate::ledger::{validate_rel_path, validate_share_name};
use crate::pins::{dir_name_for, reconcile};
use anyhow::Result;
use std::sync::Arc;

fn info(inner: &DaemonInner, pin: &PinRow) -> Result<PinInfo> {
    let files = inner.db.pinned_of_pin(pin.id)?;
    let origin_name = inner.db.list_devices()?.into_iter().find(|d| d.node_id == pin.origin).map(|d| d.name);
    let mut path = inner.config.pin_dir.join(&pin.dir_name).join(&pin.share);
    if !pin.prefix.is_empty() {
        path = path.join(&pin.prefix);
    }
    Ok(PinInfo {
        id: pin.id,
        origin: pin.origin.clone(),
        origin_name,
        share: pin.share.clone(),
        prefix: pin.prefix.clone(),
        path,
        files_ok: files.iter().filter(|f| f.state == "ok").count() as u64,
        files_pending: files.iter().filter(|f| f.state == "pending").count() as u64,
        conflicts: files.iter().filter(|f| f.state == "conflict").count() as u64,
        bytes: files.iter().filter(|f| f.state == "ok").map(|f| f.size).sum(),
    })
}

pub(crate) async fn pin_add(inner: &Arc<DaemonInner>, target: &str) -> Result<Response> {
    let target = target.trim();
    let (device, rest) = match target.split_once(':') {
        Some((d, r)) if !d.contains('/') => (Some(d), r),
        _ => (None, target),
    };
    let rest = rest.trim_matches('/');
    let (share, prefix) = rest.split_once('/').unwrap_or((rest, ""));
    if validate_share_name(share).is_err() {
        api_bail!(InvalidInput, "Ungültiger Freigabe-Name '{share}'");
    }
    if !prefix.is_empty() && validate_rel_path(prefix).is_err() {
        api_bail!(InvalidInput, "Ungültiger Unterordner '{prefix}'");
    }
    let origins = inner.db.share_origins(share, &inner.self_id())?;
    let (origin, name) = match device {
        Some(d) => {
            let found = inner.db.find_devices(d)?;
            let [dev] = found.as_slice() else {
                api_bail!(NotFound, "Kein eindeutiges gekoppeltes Gerät '{d}'");
            };
            if !origins.iter().any(|(o, _)| *o == dev.node_id) {
                api_bail!(NotFound, "'{}' bietet keine Freigabe '{share}' an (ggf. erst `sync`)", dev.name);
            }
            (dev.node_id.clone(), Some(dev.name.clone()))
        }
        None => match origins.as_slice() {
            [] => api_bail!(NotFound, "Keine Freigabe '{share}' bekannt (ggf. erst `space-daemon sync`)"),
            [(o, n)] => (o.clone(), n.clone()),
            many => {
                let list: Vec<String> = many
                    .iter()
                    .map(|(o, n)| format!("  {}:{rest}", n.clone().unwrap_or_else(|| o[..10].to_string())))
                    .collect();
                api_bail!(Ambiguous, "Mehrere Geräte bieten '{share}' an – bitte angeben:\n{}", list.join("\n"));
            }
        },
    };
    // Ordnername eindeutig halten, falls zwei Geräte gleich heißen.
    let mut dir_name = dir_name_for(name.as_deref(), &origin);
    if inner.db.list_pins()?.iter().any(|p| p.dir_name == dir_name && p.origin != origin) {
        dir_name = format!("{dir_name}-{}", &origin[..6]);
    }
    let id = inner.db.add_pin(&origin, share, prefix, &dir_name)?;
    let stats = reconcile(inner).await?;
    let pin = inner.db.list_pins()?.into_iter().find(|p| p.id == id).expect("gerade angelegt");
    Ok(Response::PinAdded { pin: info(inner, &pin)?, queued: stats.queued })
}

pub(crate) fn pin_list(inner: &Arc<DaemonInner>) -> Result<Response> {
    let pins = inner.db.list_pins()?.iter().map(|p| info(inner, p)).collect::<Result<Vec<_>>>()?;
    Ok(Response::Pins {
        pins,
        pin_dir: inner.config.pin_dir.clone(),
        used_bytes: inner.db.pinned_total_size()?,
        max_bytes: inner.settings.pin_max_bytes(),
    })
}

pub(crate) fn pin_remove(inner: &Arc<DaemonInner>, id: i64) -> Result<Response> {
    if !inner.db.remove_pin(id)? {
        api_bail!(NotFound, "Kein Pin mit ID {id}. Siehe `space-daemon pin list`.");
    }
    Ok(Response::Success {
        message: format!(
            "Pin {id} entfernt. Bereits gespiegelte Dateien bleiben unter {} liegen.",
            inner.config.pin_dir.display()
        ),
    })
}
