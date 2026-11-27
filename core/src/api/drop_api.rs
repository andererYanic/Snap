//! Snap Drop über die gemeinsame API (CLI, Desktop, Mobile). Nur lokal erlaubt.

use crate::api::error::{ApiError, ErrorCode};
use crate::daemon::DaemonInner;
use crate::drop::wire::ConnectInfo;
use crate::ipc::{Request, Response};
use anyhow::Result;
use data_encoding::BASE64;
use std::sync::Arc;

/// Größte Base64-Eingabe aus Bluetooth (Verbindungsdaten oder Kurzanfrage).
const MAX_B64: usize = 2048;

fn b64(data: &str) -> Result<Vec<u8>> {
    if data.len() > MAX_B64 {
        return Err(ApiError::new(ErrorCode::InvalidInput, "Daten sind zu lang").into());
    }
    BASE64
        .decode(data.trim().as_bytes())
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Daten sind kein Base64").into())
}

pub(crate) async fn handle(inner: &Arc<DaemonInner>, req: Request) -> Result<Response> {
    let d = &inner.drop;
    Ok(match req {
        Request::DropStatus => Response::DropStatus(d.status()?),
        Request::DropSetVisibility { mode } => Response::DropStatus(d.set_visibility(mode).await?),
        Request::DropBrowse { on } => {
            if on {
                d.browse_start()?;
            } else {
                d.browse_stop();
            }
            Response::DropNearby { peers: d.nearby() }
        }
        Request::DropNearby => Response::DropNearby { peers: d.nearby() },
        Request::DropAddCode { code } => Response::DropPeer(d.add_code(&code).await?),
        Request::DropAddPeer { info } => {
            let info = ConnectInfo::decode(&b64(&info)?)?;
            d.add_peer(&info, "bluetooth");
            Response::Success { message: "Gerät wird abgefragt".into() }
        }
        Request::DropConnectInfo => {
            Response::DropConnectInfo { info: d.connect_info().await.map(|i| BASE64.encode(&i.encode())) }
        }
        Request::DropRefresh => {
            Response::DropConnectInfo { info: d.refresh_session().await?.map(|i| BASE64.encode(&i.encode())) }
        }
        Request::DropSend { peer, paths, text } => Response::DropSent(d.send_with_text(&peer, paths, text).await?),
        Request::DropOffers => Response::DropOffers { offers: d.pending_offers() },
        Request::DropRespond { id, accept, block } => {
            d.respond(id, accept, block)?;
            Response::Success { message: if accept { "Angenommen".into() } else { "Abgelehnt".into() } }
        }
        Request::DropPreview { id } => Response::DropPreview { png: d.offer_preview(id).map(|p| BASE64.encode(&p)) },
        Request::DropContacts => Response::DropContacts { contacts: d.contacts()? },
        Request::DropSetRemote { enabled } => Response::DropStatus(d.set_remote(enabled).await?),
        Request::DropContactRemote { key, ok } => Response::DropContact(d.set_contact_remote(&key, ok)?),
        Request::DropSendContact { contact, paths, text } => {
            Response::DropSent(d.send_to_contact(&contact, paths, text).await?)
        }
        Request::DropWakeExport { enable } => {
            d.set_wake_export(enable)?;
            Response::Success { message: if enable { "Wecken eingerichtet".into() } else { "Wecken aus".into() } }
        }
        Request::DropBackground { secs, everyone } => Response::DropStatus(d.background(secs, everyone).await?),
        Request::DropSaveContact { id } => Response::DropContact(d.save_contact(id)?),
        Request::DropRemoveContact { key } => {
            if !d.remove_contact(&key)? {
                return Err(ApiError::new(ErrorCode::NotFound, "Kein solcher Kontakt").into());
            }
            Response::Success { message: "Kontakt entfernt".into() }
        }
        Request::DropRenameContact { key, alias } => Response::DropContact(d.rename_contact(&key, alias.as_deref())?),
        Request::DropSetName { name } => Response::DropStatus(d.set_name(name.as_deref())?),
        Request::DropBlocked => Response::DropBlocked { blocked: d.blocked()? },
        Request::DropUnblock { key } => {
            d.unblock(&key)?;
            Response::Success { message: "Sperre aufgehoben".into() }
        }
        Request::DropHotspotParse { data } => Response::DropHotspot(d.parse_hotspot(&b64(&data)?)?),
        Request::DropHotspotAccept { data } => {
            d.pre_accept(&b64(&data)?)?;
            Response::Success { message: "Angenommen".into() }
        }
        Request::DropHotspotRequest { ssid, passphrase, paths } => {
            Response::DropHotspotRequest { data: BASE64.encode(&d.hotspot_request(&ssid, &passphrase, &paths).await?) }
        }
        _ => unreachable!("nur Drop-Anfragen"),
    })
}
