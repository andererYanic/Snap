//! Finden im gleichen Netz per mDNS (`_snapdrop._udp.local.`).
//!
//! Im Rundsenden stehen nur ein Zufallstoken (Instanzname) und die Transport-Kennung der
//! Sitzung – kein Name, kein Drop-Schlüssel. Eingehende Einträge werden streng geprüft und
//! begrenzt (fremde Geräte im WLAN können beliebige Einträge senden).

use super::wire::hex;
use anyhow::{Context, Result};
use iroh::PublicKey;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::SocketAddr;

pub const SERVICE_TYPE: &str = "_snapdrop._udp.local.";
/// Höchstens so viele Adressen je Eintrag übernehmen.
const MAX_ADDRS: usize = 8;

/// Laufende Bekanntmachung; Fallenlassen meldet sie ab.
pub struct Advert {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advert {
    pub fn start(token: &[u8; 8], endpoint: &PublicKey, port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("mDNS nicht verfügbar")?;
        let name = hex(token);
        let host = format!("snap-{name}.local.");
        let id = hex(endpoint.as_bytes());
        let info = ServiceInfo::new(SERVICE_TYPE, &name, &host, "", port, &[("v", "1"), ("id", id.as_str())][..])
            .context("mDNS-Eintrag ungültig")?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info).context("mDNS-Anmeldung fehlgeschlagen")?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advert {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Ein im Netz gefundenes Gerät.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub endpoint: PublicKey,
    pub addrs: Vec<SocketAddr>,
}

/// Wertet einen mDNS-Eintrag aus. `None` bei allem, was nicht exakt passt.
pub fn parse_entry(version: Option<&str>, id: Option<&str>, port: u16, ips: &[std::net::IpAddr]) -> Option<Found> {
    if version != Some("1") || port == 0 {
        return None;
    }
    let key = super::wire::hex_array::<32>(id?).ok()?;
    let endpoint = PublicKey::from_bytes(&key).ok()?;
    let addrs: Vec<SocketAddr> = ips
        .iter()
        .filter(|ip| !ip.is_unspecified() && !ip.is_multicast())
        .take(MAX_ADDRS)
        .map(|ip| SocketAddr::new(*ip, port))
        .collect();
    (!addrs.is_empty()).then_some(Found { endpoint, addrs })
}

/// Sucht im Netz; jedes gefundene Gerät geht an `on_found`. Läuft, bis der Rückgabewert
/// fallengelassen wird.
pub struct Browser {
    daemon: ServiceDaemon,
    task: tokio::task::JoinHandle<()>,
}

impl Browser {
    pub fn start(on_found: impl Fn(Found) + Send + 'static) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("mDNS nicht verfügbar")?;
        let rx = daemon.browse(SERVICE_TYPE).context("mDNS-Suche fehlgeschlagen")?;
        let task = tokio::spawn(async move {
            while let Ok(ev) = rx.recv_async().await {
                if let ServiceEvent::ServiceResolved(s) = ev {
                    let ips: Vec<std::net::IpAddr> = s.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
                    if let Some(found) =
                        parse_entry(s.get_property_val_str("v"), s.get_property_val_str("id"), s.get_port(), &ips)
                    {
                        on_found(found);
                    }
                }
            }
        });
        Ok(Self { daemon, task })
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eintraege_werden_streng_geprueft() {
        let key = iroh::SecretKey::generate().public();
        let id = hex(key.as_bytes());
        let ip: std::net::IpAddr = "192.168.0.7".parse().unwrap();
        let f = parse_entry(Some("1"), Some(&id), 5000, &[ip]).unwrap();
        assert_eq!(f.endpoint, key);
        assert_eq!(f.addrs, vec!["192.168.0.7:5000".parse().unwrap()]);
        assert!(parse_entry(Some("2"), Some(&id), 5000, &[ip]).is_none());
        assert!(parse_entry(Some("1"), Some("zz"), 5000, &[ip]).is_none());
        assert!(parse_entry(Some("1"), Some(&id), 0, &[ip]).is_none());
        assert!(parse_entry(Some("1"), Some(&id), 5000, &["0.0.0.0".parse().unwrap()]).is_none());
        let many: Vec<std::net::IpAddr> = (1..50).map(|i| format!("10.0.0.{i}").parse().unwrap()).collect();
        assert_eq!(parse_entry(Some("1"), Some(&id), 1, &many).unwrap().addrs.len(), MAX_ADDRS);
    }
}
