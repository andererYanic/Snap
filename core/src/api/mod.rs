//! API-Schicht: alle Operationen, die CLI, Desktop- und Mobile-App aufrufen.
//!
//! Der Wire-Vertrag (`ipc::Request`/`ipc::Response`) ist für alle Oberflächen gleich:
//! Unix-Socket (CLI, Desktop), eingebettet per FFI (Mobile) oder direkt in Rust.


pub mod devices;
pub mod error;
pub mod files;
pub mod transfer;

use crate::ipc::{DaemonInfo, Request, Response};
use anyhow::Result;
use std::time::Duration;
use crate::daemon::*;
use crate::sync::{spawn_sync_with, sync_all};
use devices::{pair_invite, pair_remove};
use files::{catalog, delete, publish, ticket};
use transfer::download;

pub(crate) async fn handle_request(inner: &DaemonInner, req: Request) -> Response {
    let result = match req {
        Request::Publish { path } => publish(inner, &path).await,
        Request::Download { target, output, force } => download(inner, &target, output, force).await,
        Request::Delete { hash, path } => delete(inner, hash, path).await,
        Request::List => catalog(inner),
        Request::Sync => Ok(Response::SyncReport { results: sync_all(inner).await }),
        Request::PairInvite => pair_invite(inner).await,
        Request::PairInviteWait => inner
            .pairing
            .state
            .wait_for_join()
            .await
            .map(|device| {
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
        Request::PairList => inner.db.list_devices().map(|devices| Response::Devices { devices }),
        Request::PairRemove { device } => pair_remove(inner, &device),
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

