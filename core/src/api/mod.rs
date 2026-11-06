//! API-Schicht: alle Operationen, die CLI, Desktop- und Mobile-App aufrufen.
//!
//! Der Wire-Vertrag (`ipc::Request`/`ipc::Response`) ist für alle Oberflächen gleich:
//! Unix-Socket (CLI, Desktop), eingebettet per FFI (Mobile) oder direkt in Rust.

pub mod auth;
pub mod devices;
pub mod error;
pub mod explorer;
pub mod fileops;
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
        Request::FileOp { op, wait } => fileops::file_op(inner, op, wait).await,
        Request::FileOpCancel { job } => fileops::cancel(inner, job),
        Request::Undo => fileops::undo(inner).await,
        Request::TrashList => fileops::trash_list(inner).await,
        Request::TrashRestore { id, keep_both } => fileops::trash_restore(inner, id, keep_both).await,
        Request::TrashPurge { id } => fileops::trash_purge(inner, id).await,
        Request::TrashEmpty => fileops::trash_empty(inner).await,
        Request::RemoteOp { device, op } => crate::remote_ops::send(inner, &device, op).await,
        Request::OpsPending => fileops::ops_pending(inner),
        Request::OpsApprove { id } => crate::remote_ops::approve(inner, id).await,
        Request::OpsReject { id } => crate::remote_ops::reject(inner, id),
        Request::StreamUrl { target } => crate::stream::stream_url(inner, &target),
        Request::Settings => Ok(Response::Settings { items: inner.settings.list() }),
        Request::Upload { device, share, dest_dir, path } => {
            crate::upload::enqueue(inner, &device, &share, dest_dir.trim_matches('/'), &path, None)
                .map(|id| Response::Success { message: format!("Upload {id} eingeplant.") })
        }
        Request::UploadList => inner.db.list_uploads(200).map(|items| Response::Uploads { items }),
        Request::UploadCancel { id } => match inner.db.cancel_upload(id) {
            Ok(true) => Ok(Response::Success { message: format!("Upload {id} entfernt.") }),
            Ok(false) => Ok(Response::error(error::ErrorCode::NotFound, format!("Kein offener Upload {id}."))),
            Err(e) => Err(e),
        },
        Request::UploadRetry => inner.db.retry_uploads(None).map(|n| {
            inner.upload_notify.notify_one();
            Response::Success { message: format!("{n} Upload(s) werden erneut versucht.") }
        }),
        Request::UploadRuleAdd { path, device, share, dest_dir } => {
            fileops::upload_rule_add(inner, &path, &device, &share, &dest_dir)
        }
        Request::LinkSetup(s) => {
            crate::links::setup(inner, s).map(|_| Response::Success { message: "Freigabelinks eingerichtet.".into() })
        }
        Request::LinkCreate { path, days, max_downloads, password } => {
            crate::links::create(inner, &path, days, max_downloads, password).await.map(Response::Link)
        }
        Request::SpaceUsage { path, limit } => explorer::space_usage(inner, path, limit).await,
        Request::Duplicates { path, min_size, limit } => explorer::duplicates(inner, path, min_size, limit).await,
        Request::Timeline { path, before, limit } => explorer::timeline(inner, path, before, limit).await,
        Request::TimelineMonths { path } => explorer::timeline_months(inner, path).await,
        Request::BackupCreate { target, sources, password } => {
            crate::backup::create(inner, target, sources, password).await.map(Response::BackupCreated)
        }
        Request::BackupConnect { target, id, secret } => crate::backup::connect(inner, target, &id, secret)
            .await
            .map(|id| Response::Success { message: format!("Sicherung {} verbunden.", &id[..8]) }),
        Request::BackupList => crate::backup::list(inner).map(|jobs| Response::Backups { jobs }),
        Request::BackupSources { id, sources } => crate::backup::set_sources(inner, &id, sources)
            .map(|_| Response::Success { message: "Quellordner gespeichert.".into() }),
        Request::BackupRun { id } => crate::backup::run(inner, &id).await.map(Response::Snapshot),
        Request::BackupSnapshots { id } => {
            crate::backup::snapshots(inner, &id).await.map(|items| Response::Snapshots { items })
        }
        Request::BackupBrowse { id, snapshot, path } => {
            crate::backup::browse(inner, &id, &snapshot, &path).await.map(|entries| Response::BackupEntries { entries })
        }
        Request::BackupRestore { id, snapshot, path, dest } => {
            crate::backup::restore(inner, &id, &snapshot, &path, &dest)
                .await
                .map(|(path, files)| Response::Restored { path, files })
        }
        Request::BackupCheck { id, sample } => crate::backup::check(inner, &id, sample as usize).await.map(|(s, c)| {
            Response::Success { message: format!("Sicherung in Ordnung: {s} Stände, {c} Stücke geprüft.") }
        }),
        Request::BackupRemove { id } => crate::backup::remove(inner, &id).map(|_| Response::Success {
            message: "Sicherung von diesem Gerät entfernt (Daten am Ziel bleiben).".into(),
        }),
        Request::LinkList => crate::links::list(inner).map(|(items, base_url)| Response::Links { items, base_url }),
        Request::LinkRevoke { id } => crate::links::revoke(inner, &id).await.map(Response::Link),
        Request::UploadRuleList => inner.db.upload_rules().map(|rules| Response::UploadRules { rules }),
        Request::UploadRuleRemove { id } => match inner.db.remove_upload_rule(id) {
            Ok(true) => {
                inner.rules_notify.notify_one();
                Ok(Response::Success { message: format!("Regel {id} entfernt.") })
            }
            Ok(false) => Ok(Response::error(error::ErrorCode::NotFound, format!("Keine Regel {id}."))),
            Err(e) => Err(e),
        },
        Request::SettingSet { key, value } => inner.settings.set(&key, value).map(|info| {
            if info.key == "thumb_cache_mb" {
                inner.thumbs.set_max(inner.settings.thumb_cache_bytes());
            }
            if info.key == "pin_max_gb" {
                inner.pin_notify.notify_one();
            }
            inner.db.events().emit(crate::events::Event::SettingChanged { key: info.key.clone(), value: info.value });
            Response::Settings { items: inner.settings.list() }
        }),
        Request::RemoteThumbnail { device, share, path, size } => {
            crate::remote_thumbs::fetch(inner, &device, &share, &path, size).await.map(Response::Thumbnail)
        }
        Request::Thumbnail { path, size } => inner.thumbs.get(&path, size).await.map(Response::Thumbnail),
        Request::Metadata { path } => tokio::task::spawn_blocking(move || crate::media::metadata(&path))
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r)
            .map(Response::Metadata),
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
        Request::FileOp { .. } => "file.op",
        Request::FileOpCancel { .. } => "file.cancel",
        Request::Undo => "file.undo",
        Request::TrashList => "trash.list",
        Request::TrashRestore { .. } => "trash.restore",
        Request::TrashPurge { .. } => "trash.purge",
        Request::TrashEmpty => "trash.empty",
        Request::RemoteOp { .. } => "remote.op",
        Request::OpsPending => "ops.pending",
        Request::OpsApprove { .. } => "ops.approve",
        Request::OpsReject { .. } => "ops.reject",
        Request::Thumbnail { .. } => "thumbnail",
        Request::Metadata { .. } => "metadata",
        Request::StreamUrl { .. } => "stream.url",
        Request::Settings => "settings",
        Request::SettingSet { .. } => "settings.set",
        Request::Upload { .. } => "upload",
        Request::UploadList => "upload.list",
        Request::UploadCancel { .. } => "upload.cancel",
        Request::UploadRetry => "upload.retry",
        Request::UploadRuleAdd { .. } => "upload.rule.add",
        Request::UploadRuleList => "upload.rule.list",
        Request::UploadRuleRemove { .. } => "upload.rule.remove",
        Request::LinkSetup(_) => "link.setup",
        Request::LinkCreate { .. } => "link.create",
        Request::LinkList => "link.list",
        Request::LinkRevoke { .. } => "link.revoke",
        Request::SpaceUsage { .. } => "space.usage",
        Request::Duplicates { .. } => "duplicates",
        Request::Timeline { .. } => "timeline",
        Request::TimelineMonths { .. } => "timeline.months",
        Request::BackupCreate { .. } => "backup.create",
        Request::BackupConnect { .. } => "backup.connect",
        Request::BackupList => "backup.list",
        Request::BackupSources { .. } => "backup.sources",
        Request::BackupRun { .. } => "backup.run",
        Request::BackupSnapshots { .. } => "backup.snapshots",
        Request::BackupBrowse { .. } => "backup.browse",
        Request::BackupRestore { .. } => "backup.restore",
        Request::BackupCheck { .. } => "backup.check",
        Request::BackupRemove { .. } => "backup.remove",
        Request::RemoteThumbnail { .. } => "thumb.remote",
    })
}
