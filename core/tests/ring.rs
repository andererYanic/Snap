//! Gerät finden (`snap/ring/1`) – Angriffstests nach CLAUDE.md: fremde, entkoppelte, nicht
//! berechtigte und manipulierte Gegenstellen, Flut.

use iroh::{endpoint::presets, Endpoint, RelayMode};
use space_core::events::Event;
use space_core::ipc::{Request, Response};
use space_core::ring::{RING_ALPN, RING_SECS};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::Duration;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn config(root: &Path, name: &str) -> Config {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(free_port());
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    c.live_connections = false;
    c
}

async fn start(root: &Path, name: &str) -> Daemon {
    Daemon::start_loopback(config(root, name)).await.unwrap()
}

async fn pair(host: &Daemon, guest: &Daemon) {
    let code = match host.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    host.confirm_next_pairing();
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

async fn ring(from: &Daemon, to: &str, stop: bool) -> Response {
    from.request(Request::Ring { device: to.into(), stop }).await
}

async fn grant(on: &Daemon, device: &str) {
    let r =
        on.request(Request::PairGrant { device: device.into(), capability: "device.ring".into(), folder: None }).await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
}

async fn next_ring(sub: &mut space_core::events::Subscription, wait: Duration) -> Option<(String, u32)> {
    tokio::time::timeout(wait, async {
        loop {
            if let Some(Event::Ring { name, secs, .. }) = sub.next().await {
                return (name, secs);
            }
        }
    })
    .await
    .ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn nur_mit_recht_und_gedrosselt() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A").await;
    let b = start(&tmp.path().join("b"), "B").await;
    pair(&a, &b).await;
    let mut sub = b.subscribe();

    // Ohne Recht (auch nicht beim Koppeln vergeben): abgewiesen, nichts klingelt
    let r = ring(&a, "B", false).await;
    assert!(matches!(&r, Response::Error { message, .. } if message.contains("erlaubt dir das nicht")), "{r:?}");
    assert!(next_ring(&mut sub, Duration::from_millis(300)).await.is_none());

    // Mit Recht: klingelt die vom Empfänger festgelegte Zeit, Stoppen geht ebenfalls
    grant(&b, "A").await;
    assert!(matches!(ring(&a, "B", false).await, Response::Success { .. }));
    assert_eq!(next_ring(&mut sub, Duration::from_secs(5)).await, Some(("A".into(), RING_SECS)));
    assert!(matches!(ring(&a, "B", true).await, Response::Success { .. }));
    assert_eq!(next_ring(&mut sub, Duration::from_secs(5)).await, Some(("A".into(), 0)));
    // Lokal beenden meldet allen Oberflächen „aufhören“
    assert!(matches!(b.request(Request::RingStop).await, Response::Success { .. }));
    assert_eq!(next_ring(&mut sub, Duration::from_secs(5)).await.map(|r| r.1), Some(0));

    // Das Recht wirkt nur in eine Richtung
    assert!(matches!(ring(&b, "A", false).await, Response::Error { .. }));

    // Flut: höchstens 6 je Minute (2 schon verbraucht)
    let mut busy = false;
    for _ in 0..10 {
        if let Response::Error { message, .. } = ring(&a, "B", false).await {
            assert!(message.contains("nimmt gerade nichts an"), "{message}");
            busy = true;
            break;
        }
    }
    assert!(busy, "Drosselung greift");

    // Entzug wirkt sofort; das Audit-Log hält die Versuche fest
    let r = b.request(Request::PairRevoke { device: "A".into(), capability: "device.ring".into(), folder: None }).await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
    assert!(matches!(ring(&a, "B", false).await, Response::Error { .. }));
    let audit = b.request(Request::Audit { limit: 50 }).await;
    assert!(format!("{audit:?}").contains("device.ring"), "{audit:?}");
    a.shutdown().await;
    b.shutdown().await;
}

async fn raw(key_path: &Path) -> Endpoint {
    let key = space_core::secure_fs::load_or_create_secret_key(key_path).unwrap();
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(key)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn frame(json: &str) -> Vec<u8> {
    let mut v = (json.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(json.as_bytes());
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn fremde_und_manipulierte_gegenstellen() {
    let tmp = tempfile::tempdir().unwrap();
    let b = start(&tmp.path().join("b"), "B").await;
    // C ist gekoppelt und berechtigt, spricht danach aber als roher (bösartiger) Client
    let c = start(&tmp.path().join("c"), "C").await;
    pair(&b, &c).await;
    grant(&b, "C").await;
    let c_key = c.config().secret_key_path();
    c.shutdown().await;
    drop(c);
    let mut sub = b.subscribe();

    // Fremd (nie gekoppelt): sofort geschlossen, nichts gelesen
    let stranger = raw(&tmp.path().join("fremd.key")).await;
    let conn = stranger.connect(b.network().endpoint().addr(), RING_ALPN).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    assert!(why.to_string().contains("not paired"), "{why}");

    // Manipuliert: eigene Dauer, unbekannte Felder, falsche Version, Übergröße → kein Klingeln
    let ep = raw(&c_key).await;
    for bad in [
        frame(r#"{"v":1,"stop":false,"secs":86400}"#),
        frame(r#"{"v":2,"stop":false}"#),
        frame(r#"{"v":1}"#),
        frame(&format!(r#"{{"v":1,"stop":false,"x":"{}"}}"#, "y".repeat(4096))),
        vec![0xff, 0xff, 0xff, 0xff],
    ] {
        let conn = ep.connect(b.network().endpoint().addr(), RING_ALPN).await.unwrap();
        if let Ok((mut s, _r)) = conn.open_bi().await {
            let _ = s.write_all(&bad).await;
            let _ = s.finish();
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), conn.closed()).await;
    }
    assert!(next_ring(&mut sub, Duration::from_millis(500)).await.is_none(), "nichts Manipuliertes klingelt");

    // Nach dem Entkoppeln: „unpaired“ statt Klingeln
    assert!(matches!(b.request(Request::PairRemove { device: "C".into() }).await, Response::Success { .. }));
    let conn = ep.connect(b.network().endpoint().addr(), RING_ALPN).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    assert!(why.to_string().contains("unpaired"), "{why}");
    b.shutdown().await;
}
