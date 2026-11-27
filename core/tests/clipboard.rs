//! Zwischenablage zwischen eigenen Geräten (`snap/clip/1`) – Angriffstests nach CLAUDE.md:
//! fremde, entkoppelte, nicht berechtigte und manipulierte Gegenstellen, Flut.

use iroh::{endpoint::presets, Endpoint, RelayMode};
use space_core::clipboard::CLIP_ALPN;
use space_core::events::Event;
use space_core::ipc::{Request, Response};
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

async fn send(from: &Daemon, to: &str, text: &str) -> Response {
    from.request(Request::ClipboardSend { device: to.into(), text: text.into() }).await
}

async fn grant(on: &Daemon, device: &str) {
    let r = on
        .request(Request::PairGrant { device: device.into(), capability: "clipboard.write".into(), folder: None })
        .await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
}

async fn next_clip(sub: &mut space_core::events::Subscription, wait: Duration) -> Option<(String, String)> {
    tokio::time::timeout(wait, async {
        loop {
            if let Some(Event::ClipboardReceived { name, text, .. }) = sub.next().await {
                return (name, text);
            }
        }
    })
    .await
    .ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn nur_mit_recht_und_bereinigt() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A").await;
    let b = start(&tmp.path().join("b"), "B").await;
    pair(&a, &b).await;
    let mut sub = b.subscribe();

    // Ohne Recht: abgewiesen, nichts kommt an
    let r = send(&a, "B", "geheim").await;
    assert!(matches!(&r, Response::Error { message, .. } if message.contains("erlaubt dir das nicht")), "{r:?}");
    assert!(next_clip(&mut sub, Duration::from_millis(300)).await.is_none());

    // Mit Recht: kommt an, Steuerzeichen entfernt, Zeilenumbruch bleibt
    grant(&b, "A").await;
    let r = send(&a, "B", "Zeile 1\nZeile\u{7} 2\u{202E}").await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
    let (name, text) = next_clip(&mut sub, Duration::from_secs(5)).await.expect("angekommen");
    assert_eq!((name.as_str(), text.as_str()), ("A", "Zeile 1\nZeile 2"));
    // Für die App bereitgehalten – genau einmal
    let p = b.request(Request::ClipboardPending).await;
    assert!(matches!(&p, Response::ClipboardPending { text: Some(t), .. } if t == "Zeile 1\nZeile 2"), "{p:?}");
    assert!(matches!(b.request(Request::ClipboardPending).await, Response::ClipboardPending { text: None, .. }));

    // Das Recht wirkt nur in eine Richtung
    let r = send(&b, "A", "zurück").await;
    assert!(matches!(r, Response::Error { .. }), "{r:?}");

    // Zu groß bzw. leer: schon lokal abgewiesen
    assert!(matches!(send(&a, "B", &"x".repeat(64 * 1024 + 1)).await, Response::Error { .. }));
    assert!(matches!(send(&a, "B", "  ").await, Response::Error { .. }));

    // Flut: höchstens 30 je Minute
    let mut busy = false;
    for i in 0..40 {
        if let Response::Error { message, .. } = send(&a, "B", &format!("n{i}")).await {
            assert!(message.contains("nimmt gerade nichts an"), "{message}");
            busy = true;
            break;
        }
    }
    assert!(busy, "Drosselung greift");

    // Entzug wirkt sofort
    let r =
        b.request(Request::PairRevoke { device: "A".into(), capability: "clipboard.write".into(), folder: None }).await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
    assert!(matches!(send(&a, "B", "nach Entzug").await, Response::Error { .. }));
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
    let conn = stranger.connect(b.network().endpoint().addr(), CLIP_ALPN).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    assert!(why.to_string().contains("not paired"), "{why}");

    // Manipuliert: unbekannte Felder, falsche Version, Übergröße → kein Ereignis
    let ep = raw(&c_key).await;
    for bad in [
        frame(r#"{"v":1,"text":"x","extra":1}"#),
        frame(r#"{"v":2,"text":"x"}"#),
        frame(&format!(r#"{{"v":1,"text":"{}"}}"#, "y".repeat(70 * 1024))),
        vec![0xff, 0xff, 0xff, 0xff],
    ] {
        let conn = ep.connect(b.network().endpoint().addr(), CLIP_ALPN).await.unwrap();
        if let Ok((mut s, _r)) = conn.open_bi().await {
            let _ = s.write_all(&bad).await;
            let _ = s.finish();
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), conn.closed()).await;
    }
    assert!(next_clip(&mut sub, Duration::from_millis(500)).await.is_none(), "nichts Manipuliertes angekommen");

    // Nach dem Entkoppeln: „unpaired“ statt Zugriff
    assert!(matches!(b.request(Request::PairRemove { device: "C".into() }).await, Response::Success { .. }));
    let conn = ep.connect(b.network().endpoint().addr(), CLIP_ALPN).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    assert!(why.to_string().contains("unpaired"), "{why}");
    b.shutdown().await;
}
