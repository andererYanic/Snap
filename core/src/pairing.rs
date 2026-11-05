//! Pairing: einmaliges, ausdrückliches Koppeln zweier eigener Geräte.
//!
//! Ablauf:
//! 1. Gerät A erzeugt einen Einmal-Code (`pair invite`): Node-ID von A und ein
//!    zufälliges 256-Bit-Token. Gültig 10 Minuten, nur einmal, nur im Speicher.
//! 2. Gerät B verbindet sich mit dem Code (`pair join`). TLS beweist B, dass es mit
//!    der Node-ID aus dem Code spricht (kein Mittelsmann möglich). A vertraut B,
//!    weil B das Token kennt. A erfährt B's Node-ID authentisch aus TLS.
//! 3. Beide tragen sich gegenseitig in `devices` ein.

use crate::database::{DatabaseManager, DeviceInfo};
use crate::ipc::{read_frame_max, write_frame_max};
use crate::ledger::sanitize_device_name;
use crate::network::{direct_addrs, endpoint_addr};
use anyhow::{bail, Context, Result};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId, PublicKey};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

pub const PAIR_ALPN: &[u8] = b"snap/pair/1";
const PROTOCOL_VERSION: u32 = 1;
pub const INVITE_TTL_SECS: i64 = 600;
const CODE_PREFIX: &str = "snap1-";
const CODE_VERSION: u8 = 1;
const MAX_FRAME: u32 = 64 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ADDRS: usize = 16;

/// Inhalt eines Einladungscodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteCode {
    pub node_id: EndpointId,
    pub token: [u8; 32],
    pub expires_at: i64,
    /// Direkte Adressen (nur im Modus ohne Relais enthalten).
    pub addrs: Vec<String>,
}

impl InviteCode {
    pub fn encode(&self) -> String {
        let mut buf = Vec::with_capacity(80);
        buf.push(CODE_VERSION);
        buf.extend_from_slice(self.node_id.as_bytes());
        buf.extend_from_slice(&self.token);
        buf.extend_from_slice(&self.expires_at.to_be_bytes());
        let addrs: Vec<&String> = self.addrs.iter().filter(|a| a.len() <= 64).take(MAX_ADDRS).collect();
        buf.push(addrs.len() as u8);
        for a in addrs {
            buf.push(a.len() as u8);
            buf.extend_from_slice(a.as_bytes());
        }
        format!("{CODE_PREFIX}{}", data_encoding::BASE32_NOPAD.encode(&buf).to_ascii_lowercase())
    }

    pub fn decode(code: &str) -> Result<Self> {
        let body = code.trim().strip_prefix(CODE_PREFIX).context("Kein gültiger Snap-Einladungscode")?;
        let buf = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .ok()
            .context("Einladungscode ist beschädigt")?;
        let mut r = Reader(&buf);
        if r.u8()? != CODE_VERSION {
            bail!("Einladungscode stammt von einer anderen Version");
        }
        let node_id = PublicKey::from_bytes(&r.array::<32>()?).ok().context("Einladungscode ist beschädigt")?;
        let token = r.array::<32>()?;
        let expires_at = i64::from_be_bytes(r.array::<8>()?);
        let n = r.u8()? as usize;
        if n > MAX_ADDRS {
            bail!("Einladungscode ist beschädigt");
        }
        let mut addrs = Vec::with_capacity(n);
        for _ in 0..n {
            let len = r.u8()? as usize;
            let s = std::str::from_utf8(r.take(len)?).ok().context("Einladungscode ist beschädigt")?;
            addrs.push(s.to_string());
        }
        if !r.0.is_empty() {
            bail!("Einladungscode ist beschädigt");
        }
        Ok(Self { node_id, token, expires_at, addrs })
    }
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            bail!("Einladungscode ist unvollständig");
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.take(N)?.try_into().unwrap())
    }
}

/// Vergleich in konstanter Zeit (verhindert Timing-Angriffe auf das Token).
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn random_token() -> Result<[u8; 32]> {
    let mut t = [0u8; 32];
    getrandom::getrandom(&mut t).map_err(|e| anyhow::anyhow!("Zufallsgenerator nicht verfügbar: {e}"))?;
    Ok(t)
}

struct ActiveInvite {
    token: [u8; 32],
    expires_at: i64,
    done: Option<oneshot::Sender<DeviceInfo>>,
}

/// Laufzeitzustand des Pairings (nur im Speicher).
#[derive(Default)]
pub struct PairingState {
    invite: Mutex<Option<ActiveInvite>>,
    waiter: tokio::sync::Mutex<Option<(i64, oneshot::Receiver<DeviceInfo>)>>,
}

impl PairingState {
    /// Erzeugt eine neue Einladung (ersetzt eine bestehende).
    pub async fn create_invite(&self, node_id: EndpointId, addrs: Vec<String>) -> Result<InviteCode> {
        let token = random_token()?;
        let expires_at = crate::database::now_unix() + INVITE_TTL_SECS;
        let (tx, rx) = oneshot::channel();
        *self.invite.lock().unwrap() = Some(ActiveInvite { token, expires_at, done: Some(tx) });
        *self.waiter.lock().await = Some((expires_at, rx));
        Ok(InviteCode { node_id, token, expires_at, addrs })
    }

    /// Wartet, bis die aktuelle Einladung angenommen wurde oder abläuft.
    pub async fn wait_for_join(&self) -> Result<DeviceInfo> {
        let Some((expires_at, rx)) = self.waiter.lock().await.take() else {
            bail!("Keine aktive Einladung");
        };
        let remaining = (expires_at - crate::database::now_unix()).max(0) as u64;
        match tokio::time::timeout(Duration::from_secs(remaining), rx).await {
            Ok(Ok(device)) => Ok(device),
            Ok(Err(_)) => bail!("Einladung wurde durch eine neue ersetzt"),
            Err(_) => {
                self.cancel();
                bail!("Einladung abgelaufen – kein Gerät hat sich gekoppelt")
            }
        }
    }

    pub fn cancel(&self) {
        *self.invite.lock().unwrap() = None;
    }

    /// Prüft ein Token. Bei Erfolg wird die Einladung verbraucht.
    /// Falsche Tokens verbrauchen die Einladung nicht (sonst könnte jeder, der die
    /// Node-ID kennt, laufende Kopplungen sabotieren).
    fn redeem(&self, token: &[u8; 32]) -> Result<Option<oneshot::Sender<DeviceInfo>>, &'static str> {
        let mut guard = self.invite.lock().unwrap();
        let Some(inv) = guard.as_mut() else {
            return Err("Keine aktive Einladung");
        };
        if crate::database::now_unix() > inv.expires_at {
            *guard = None;
            return Err("Einladung abgelaufen");
        }
        if !ct_eq(&inv.token, token) {
            return Err("Ungültiger Einladungscode");
        }
        let done = inv.done.take();
        *guard = None;
        Ok(done)
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct PairRequest {
    version: u32,
    token: String,
    name: String,
    addrs: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct PairResponse {
    accepted: bool,
    name: String,
    reason: Option<String>,
}

pub struct PairingCtx {
    pub db: Arc<DatabaseManager>,
    pub endpoint: Endpoint,
    pub device_name: String,
    pub state: Arc<PairingState>,
}

/// Koppelt dieses Gerät mit dem Gerät aus dem Einladungscode.
pub async fn join(ctx: &PairingCtx, code: &str, connect_timeout: Duration) -> Result<DeviceInfo> {
    let invite = InviteCode::decode(code)?;
    if crate::database::now_unix() > invite.expires_at {
        bail!("Der Einladungscode ist abgelaufen – bitte auf dem anderen Gerät einen neuen erzeugen.");
    }
    if invite.node_id == ctx.endpoint.id() {
        bail!("Das ist der Einladungscode dieses Geräts.");
    }
    let addr = endpoint_addr(invite.node_id, &invite.addrs);
    let conn = tokio::time::timeout(connect_timeout, ctx.endpoint.connect(addr, PAIR_ALPN))
        .await
        .context("Gerät nicht erreichbar (Zeitüberschreitung)")?
        .context("Gerät nicht erreichbar")?;

    let result = async {
        let (mut send, mut recv) = conn.open_bi().await?;
        let req = PairRequest {
            version: PROTOCOL_VERSION,
            token: data_encoding::HEXLOWER.encode(&invite.token),
            name: ctx.device_name.clone(),
            addrs: direct_addrs(&ctx.endpoint),
        };
        write_frame_max(&mut send, &req, MAX_FRAME).await?;
        send.finish()?;
        let resp: PairResponse = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME))
            .await
            .context("Zeitüberschreitung beim Koppeln")??;
        anyhow::Ok(resp)
    }
    .await;
    conn.close(0u32.into(), b"done");
    let resp = result?;
    if !resp.accepted {
        bail!("Kopplung abgelehnt: {}", resp.reason.unwrap_or_default());
    }

    let node_id = invite.node_id.to_string();
    let name = sanitize_device_name(&resp.name, &node_id);
    ctx.db.add_device(&node_id, &name)?;
    ctx.db.set_addr_hints(&node_id, &invite.addrs)?;
    ctx.db.mark_peer_online(&node_id)?;
    tracing::info!("🔗 Gekoppelt mit '{name}' ({})", invite.node_id.fmt_short());
    device(&ctx.db, &node_id)
}

fn device(db: &DatabaseManager, node_id: &str) -> Result<DeviceInfo> {
    db.list_devices()?
        .into_iter()
        .find(|d| d.node_id == node_id)
        .context("Gerät nach dem Koppeln nicht gefunden")
}

/// Protokoll-Handler für eingehende Pairing-Anfragen.
#[derive(Clone)]
pub struct PairingProtocol {
    pub ctx: Arc<PairingCtx>,
}

impl std::fmt::Debug for PairingProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingProtocol")
    }
}

impl ProtocolHandler for PairingProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        if let Err(e) = self.handle(&conn).await {
            tracing::debug!("Pairing-Anfrage fehlgeschlagen: {e:#}");
        }
        // Gegenseite die Antwort lesen lassen, dann schließen.
        let _ = tokio::time::timeout(IO_TIMEOUT, conn.closed()).await;
        Ok(())
    }
}

impl PairingProtocol {
    async fn handle(&self, conn: &Connection) -> Result<()> {
        let ctx = &self.ctx;
        let remote = conn.remote_id();
        let (mut send, mut recv) = tokio::time::timeout(IO_TIMEOUT, conn.accept_bi()).await??;
        let req: PairRequest = tokio::time::timeout(IO_TIMEOUT, read_frame_max(&mut recv, MAX_FRAME)).await??;

        let reject = |reason: &str| PairResponse { accepted: false, name: String::new(), reason: Some(reason.into()) };
        let token: Option<[u8; 32]> = data_encoding::HEXLOWER
            .decode(req.token.as_bytes())
            .ok()
            .and_then(|b| b.try_into().ok());

        let response = if req.version != PROTOCOL_VERSION {
            reject("Nicht unterstützte Protokollversion")
        } else if remote == ctx.endpoint.id() {
            reject("Kopplung mit sich selbst")
        } else if let Some(token) = token {
            match ctx.state.redeem(&token) {
                Ok(done) => {
                    let node_id = remote.to_string();
                    let name = sanitize_device_name(&req.name, &node_id);
                    ctx.db.add_device(&node_id, &name)?;
                    ctx.db.set_addr_hints(&node_id, &req.addrs)?;
                    ctx.db.mark_peer_online(&node_id)?;
                    tracing::info!("🔗 Neues Gerät gekoppelt: '{name}' ({})", remote.fmt_short());
                    if let Some(done) = done {
                        let _ = done.send(device(&ctx.db, &node_id)?);
                    }
                    PairResponse { accepted: true, name: ctx.device_name.clone(), reason: None }
                }
                Err(reason) => {
                    tracing::warn!("Pairing-Versuch von {} abgelehnt: {reason}", remote.fmt_short());
                    reject(reason)
                }
            }
        } else {
            reject("Ungültiger Einladungscode")
        };
        write_frame_max(&mut send, &response, MAX_FRAME).await?;
        send.finish()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn code(addrs: Vec<String>) -> InviteCode {
        InviteCode {
            node_id: SecretKey::generate().public(),
            token: random_token().unwrap(),
            expires_at: 1_800_000_000,
            addrs,
        }
    }

    #[test]
    fn code_roundtrip() {
        for addrs in [vec![], vec!["192.168.1.5:41001".to_string(), "[::1]:4433".to_string()]] {
            let c = code(addrs);
            let s = c.encode();
            assert!(s.starts_with("snap1-"));
            assert_eq!(InviteCode::decode(&s).unwrap(), c);
            assert_eq!(InviteCode::decode(&format!("  {}\n", s.to_uppercase().replace("SNAP1-", "snap1-"))).unwrap(), c);
        }
    }

    #[test]
    fn corrupt_codes_are_rejected() {
        let s = code(vec![]).encode();
        assert!(InviteCode::decode("hallo").is_err());
        assert!(InviteCode::decode(&s[..s.len() - 4]).is_err());
        assert!(InviteCode::decode(&format!("{s}aaaa")).is_err());
        assert!(InviteCode::decode("snap1-!!!").is_err());
    }

    #[test]
    fn tokens_are_random() {
        assert_ne!(random_token().unwrap(), random_token().unwrap());
    }

    #[test]
    fn constant_time_compare() {
        let a = [1u8; 32];
        let mut b = a;
        assert!(ct_eq(&a, &b));
        b[31] = 2;
        assert!(!ct_eq(&a, &b));
    }

    #[tokio::test]
    async fn invite_is_single_use_and_wrong_tokens_do_not_burn_it() {
        let state = PairingState::default();
        let id = SecretKey::generate().public();
        let inv = state.create_invite(id, vec![]).await.unwrap();
        assert!(state.redeem(&[0u8; 32]).is_err());
        assert!(state.redeem(&inv.token).is_ok(), "falsches Token darf die Einladung nicht verbrauchen");
        assert!(state.redeem(&inv.token).is_err(), "Einladung ist nur einmal gültig");
    }

    #[tokio::test]
    async fn expired_invite_is_rejected() {
        let state = PairingState::default();
        let inv = state.create_invite(SecretKey::generate().public(), vec![]).await.unwrap();
        state.invite.lock().unwrap().as_mut().unwrap().expires_at = 0;
        assert!(state.redeem(&inv.token).is_err());
    }

    #[tokio::test]
    async fn new_invite_replaces_old() {
        let state = PairingState::default();
        let id = SecretKey::generate().public();
        let old = state.create_invite(id, vec![]).await.unwrap();
        let new = state.create_invite(id, vec![]).await.unwrap();
        assert!(state.redeem(&old.token).is_err());
        assert!(state.redeem(&new.token).is_ok());
    }
}
