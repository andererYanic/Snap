//! API-Schicht: alle Operationen, die CLI, Desktop- und Mobile-App aufrufen.
//!
//! Der Wire-Vertrag (`ipc::Request`/`ipc::Response`) ist für alle Oberflächen gleich:
//! Unix-Socket (CLI, Desktop), eingebettet per FFI (Mobile) oder direkt in Rust.

pub mod auth;
pub mod devices;
pub mod error;
pub mod explorer;
pub mod files;
pub mod pins;
pub mod shares;
pub mod transfer;

use crate::daemon::*;
use crate::ipc::{DaemonInfo, Request, Response};
use crate::sync::{spawn_sync_with, sync_all};
use anyhow::Result;
use devices::{pair_invite, pair_remove};
use files::{catalog, delete, publish, ticket};
use std::time::Duration;
use transfer::download;

pub(crate) async fn handle_request(
    inner: &std::sync::Arc<DaemonInner>,
    caller: &auth::Caller,
    req: Request,
) -> Response {
    // Einzige Prüfstelle: jede Anfrage braucht das passende Recht (lokal: alle).
    let cap = auth::required_capability(&req);
    let op = request_name(&req).unwrap_or("unbekannt");
    // Geschützte Pfade braucht nur die Prüfung fremder Geräte.
    let protected = match caller {
        auth::Caller::Local => Vec::new(),
        auth::Caller::Device(_) => inner.protected_paths(),
    };
    if let Err(e) = auth::authorize(&inner.db, &protected, caller, cap, None, op) {
        return Response::from_error(&e);
    }
    let result = match req {
        Request::Publish { path } => publish(inner, &path).await,
        Request::Download { target, output, force } => download(inner, &target, output, force).await,
        Request::Delete { hash, path } => delete(inner, hash, path).await,
        Request::List => catalog(inner),
        Request::Sync => Ok(Response::SyncReport { results: sync_all(inner).await }),
        Request::PairInvite => pair_invite(inner).await,
        Request::PairInviteWait => inner.pairing.state.wait_for_join().await.map(|device| {
            spawn_sync_with(inner, &device.node_id);
            Response::Paired { device }
        }),
        Request::PairJoin { code } => {
            let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
            match crate::pairing::join(&inner.pairing, &code, timeout).await {
                Ok(device) => {
                    spawn_sync_with(inner, &device.node_id);
                    Ok(Response::Paired { device })
                }
                Err(e) => Err(e),
            }
        }
        Request::PairRemove { device } => pair_remove(inner, &device),
        Request::PairList => devices::pair_list(inner),
        Request::PairGrant { device, capability, folder } => devices::pair_grant(inner, &device, &capability, folder),
        Request::PairRevoke { device, capability, folder } => devices::pair_revoke(inner, &device, &capability, folder),
        Request::ShareAdd { path, name, devices } => shares::share_add(inner, &path, name, devices).await,
        Request::ShareList => inner.db.list_shares().map(|shares| Response::Shares { shares }),
        Request::ShareRemove { name } => shares::share_remove(inner, &name),
        Request::ShareRescan { name } => shares::share_rescan(inner, name).await,
        Request::PinAdd { target } => pins::pin_add(inner, &target).await,
        Request::PinList => pins::pin_list(inner),
        Request::PinRemove { id } => pins::pin_remove(inner, id),
        Request::Audit { limit } => inner.db.audit_log(limit.clamp(1, 1000)).map(|entries| Response::Audit { entries }),
        Request::Ticket { hash } => ticket(inner, &hash),
        Request::Queue => inner.db.list_jobs().map(|jobs| Response::Queue { jobs }),
        Request::QueueCancel { id } => match inner.db.cancel_job(id) {
            Ok(true) => Ok(Response::Success { message: format!("Auftrag {id} entfernt.") }),
            Ok(false) => Ok(Response::error(error::ErrorCode::NotFound, format!("Kein Auftrag mit ID {id}."))),
            Err(e) => Err(e),
        },
        Request::QueueRetry { id } => match inner.db.retry_job(id) {
            Ok(true) => {
                inner.sync_notifier.notify_one();
                Ok(Response::Success { message: format!("Auftrag {id} wird jetzt erneut versucht.") })
            }
            Ok(false) => Ok(Response::error(error::ErrorCode::NotFound, format!("Kein Auftrag mit ID {id}."))),
            Err(e) => Err(e),
        },
        Request::Peers => inner.db.list_peers().map(|peers| Response::Peers { peers }),
        Request::Info => info(inner),
        Request::Browse(q) => files::browse(inner, q),
        Request::Search { query, filter, limit } => explorer::search(inner, query, filter, limit).await,
        Request::ListDir { path, show_hidden, offset, limit } => {
            explorer::list_dir(path, show_hidden, offset, limit).await
        }
        Request::IndexRoots => explorer::index_roots(inner),
        Request::IndexAdd { path } => explorer::index_add(inner, &path).await,
        Request::IndexRemove { id } => explorer::index_remove(inner, id).await,
        Request::IndexRescan => explorer::index_rescan(inner),
        Request::Subscribe => Ok(Response::error(
            error::ErrorCode::InvalidInput,
            "Ereignisse gibt es nur über eine Socket-Verbindung oder Daemon::subscribe()",
        )),
    };
    result.unwrap_or_else(|e| Response::from_error(&e))
}

pub(crate) fn info(inner: &DaemonInner) -> Result<Response> {
    let c = &inner.config;
    Ok(Response::Info(DaemonInfo {
        node_id: inner.network.node_id().to_string(),
        data_dir: c.data_dir.clone(),
        socket_path: c.socket_path.clone(),
        download_dir: c.download_dir.clone(),
        shared_files: inner.db.count_shared()?,
        pending_jobs: inner.db.count_waiting_jobs()?,
        use_relay: c.use_relay,
        device_name: c.device_name.clone(),
        paired_devices: inner.db.list_devices()?.len() as u64,
    }))
}

/// Kurzer Name einer Anfrage für das Audit-Log.
fn request_name(req: &Request) -> Option<&'static str> {
    Some(match req {
        Request::Publish { .. } => "publish",
        Request::Download { .. } => "download",
        Request::Delete { .. } => "delete",
        Request::List => "list",
        Request::Ticket { .. } => "ticket",
        Request::Queue => "queue",
        Request::QueueCancel { .. } => "queue.cancel",
        Request::QueueRetry { .. } => "queue.retry",
        Request::Peers => "peers",
        Request::Info => "info",
        Request::Browse(_) => "browse",
        Request::Subscribe => "subscribe",
        Request::Sync => "sync",
        Request::PairInvite => "pair.invite",
        Request::PairInviteWait => "pair.invite.wait",
        Request::PairJoin { .. } => "pair.join",
        Request::PairList => "pair.list",
        Request::PairRemove { .. } => "pair.remove",
        Request::PairGrant { .. } => "pair.grant",
        Request::PairRevoke { .. } => "pair.revoke",
        Request::Audit { .. } => "audit",
        Request::ShareAdd { .. } => "share.add",
        Request::ShareList => "share.list",
        Request::ShareRemove { .. } => "share.remove",
        Request::ShareRescan { .. } => "share.rescan",
        Request::PinAdd { .. } => "pin.add",
        Request::PinList => "pin.list",
        Request::PinRemove { .. } => "pin.remove",
        Request::Search { .. } => "search",
        Request::ListDir { .. } => "listdir",
        Request::IndexRoots => "index.roots",
        Request::IndexAdd { .. } => "index.add",
        Request::IndexRemove { .. } => "index.remove",
        Request::IndexRescan => "index.rescan",
    })
}
