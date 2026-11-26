//! Snap Drop: Verbindungsdaten und Kurzanfragen aus Bluetooth/QR, mDNS-Einträge.
//! Invarianten:
//! - nie eine Panik;
//! - was dekodiert, kodiert wieder zu denselben Bytes (keine zweideutigen Formate);
//! - höchstens 8 Adressen, keine Port-0-, unspezifizierten oder Multicast-Adressen;
//! - Kurzanfrage: 1–1000 Dateien, SSID 1–32 Byte, Passphrase 8–63 druckbare Zeichen.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::drop::discovery::parse_entry;
use space_core::drop::wire::{ConnectInfo, HotspotRequest};

fn check_addrs(i: &ConnectInfo) {
    assert!(i.addrs.len() <= 8);
    for a in &i.addrs {
        assert!(a.port() != 0 && !a.ip().is_unspecified() && !a.ip().is_multicast());
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    match sel % 4 {
        0 => {
            if let Ok(i) = ConnectInfo::decode(rest) {
                check_addrs(&i);
                assert_eq!(i.encode(), rest, "eindeutige Kodierung");
                assert_eq!(ConnectInfo::from_code(&i.to_code()).unwrap(), i);
            }
        }
        1 => {
            if let Ok(r) = HotspotRequest::decode(rest) {
                assert!(r.files >= 1 && r.files <= 1000);
                assert!(!r.ssid.is_empty() && r.ssid.len() <= 32);
                assert!((8..=63).contains(&r.passphrase.len()));
                check_addrs(&r.sender);
                assert_eq!(HotspotRequest::decode(&r.encode()).unwrap(), r);
            }
        }
        2 => {
            if let Ok(s) = std::str::from_utf8(rest) {
                if let Ok(i) = ConnectInfo::from_code(s) {
                    check_addrs(&i);
                }
            }
        }
        _ => {
            // mDNS: Version, Kennung, Port und Adressen aus fremden Paketen.
            let s = String::from_utf8_lossy(rest);
            let mut parts = s.splitn(3, '|');
            let (v, id, port) = (parts.next(), parts.next(), parts.next().and_then(|p| p.parse::<u16>().ok()));
            let ips: Vec<std::net::IpAddr> = rest.chunks(4).filter(|c| c.len() == 4).map(|c| std::net::IpAddr::from([c[0], c[1], c[2], c[3]])).collect();
            if let Some(f) = parse_entry(v, id, port.unwrap_or(0), &ips) {
                assert!(!f.addrs.is_empty() && f.addrs.len() <= 8);
                assert!(f.addrs.iter().all(|a| a.port() != 0 && !a.ip().is_unspecified() && !a.ip().is_multicast()));
            }
        }
    }
});
