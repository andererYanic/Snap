//! Entkoppeln in beide Richtungen (Angriffstests nach CLAUDE.md): Die Gegenseite erfährt es
//! über einen authentifizierten Schließcode – sofort, oder beim nächsten Verbindungsversuch.
//! Nur der Absender selbst wird entkoppelt; eine Neuinstallation löst nichts aus.

use iroh::{endpoint::presets, Endpoint, RelayMode};
use space_core::events::Event;
use space_core::ipc::{Request, Response};
use space_core::live::LIVE_ALPN;
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn config(root: &Path, name: &str, port: u16) -> Config {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(port);
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    c
}

async fn start(root: &Path, name: &str, port: u16) -> Daemon {
    Daemon::start_loopback(config(root, name, port)).await.unwrap()
}

async fn pair(host: &Daemon, guest: &Daemon) {
    let code = match host.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    host.confirm_next_pairing();
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

async fn devices(d: &Daemon) -> Vec<String> {
    match d.request(Request::PairList).await {
        Response::Devices { devices } => devices.into_iter().map(|d| d.name).collect(),
        other => panic!("{other:?}"),
    }
}

async fn remote_names(d: &Daemon) -> Vec<String> {
    match d.request(Request::List).await {
        Response::Catalog { remote, .. } => remote.into_iter().map(|f| f.name).collect(),
        other => panic!("{other:?}"),
    }
}

async fn eventually<F, Fut>(what: &str, secs: u64, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let start = Instant::now();
    while !cond().await {
        assert!(start.elapsed() < Duration::from_secs(secs), "Zeitüberschreitung: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn publish(d: &Daemon, dir: &Path, name: &str) {
    let f = dir.join(name);
    std::fs::write(&f, name.as_bytes()).unwrap();
    assert!(matches!(d.request(Request::Publish { path: f }).await, Response::Published { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn entkoppeln_erreicht_die_gegenseite_sofort() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    let b = start(&tmp.path().join("b"), "B", free_port()).await;
    pair(&a, &b).await;
    eventually("Live-Verbindung", 20, || async { a.live_peers() == 1 && b.live_peers() == 1 }).await;
    publish(&a, tmp.path(), "von-a.txt").await;
    publish(&b, tmp.path(), "von-b.txt").await;
    eventually("Kataloge", 15, || async {
        remote_names(&b).await.contains(&"von-a.txt".to_string())
            && remote_names(&a).await.contains(&"von-b.txt".to_string())
    })
    .await;

    let mut sub = b.subscribe();
    let t0 = Instant::now();
    assert!(matches!(a.request(Request::PairRemove { device: "B".into() }).await, Response::Success { .. }));
    // B erfährt es über die Live-Verbindung und entkoppelt ebenfalls
    loop {
        let left = Duration::from_secs(5).checked_sub(t0.elapsed()).expect("B wurde nicht benachrichtigt");
        match tokio::time::timeout(left, sub.next()).await {
            Ok(Some(Event::DeviceUnpairedByPeer { name, .. })) => {
                assert_eq!(name, "A");
                break;
            }
            Ok(Some(_)) => {}
            _ => panic!("B wurde nicht benachrichtigt"),
        }
    }
    println!("Entkoppeln → Gegenseite: {:?}", t0.elapsed());
    assert!(devices(&b).await.is_empty());
    assert!(!remote_names(&b).await.contains(&"von-a.txt".to_string()), "A's Dateien sind bei B weg");
    assert!(!remote_names(&a).await.contains(&"von-b.txt".to_string()), "B's Dateien sind bei A weg");
    assert_eq!((a.live_peers(), b.live_peers()), (0, 0));

    // Niemand baut die Verbindung wieder auf, und ein Abgleich bringt nichts zurück
    tokio::time::sleep(Duration::from_secs(3)).await;
    b.request(Request::Sync).await;
    assert_eq!((a.live_peers(), b.live_peers()), (0, 0));
    assert!(remote_names(&b).await.is_empty());

    // Erneut koppeln funktioniert wie beim ersten Mal
    pair(&a, &b).await;
    eventually("neue Live-Verbindung", 20, || async { a.live_peers() == 1 && b.live_peers() == 1 }).await;
    eventually("Katalog nach neuer Kopplung", 15, || async {
        remote_names(&b).await.contains(&"von-a.txt".to_string())
    })
    .await;
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn entkoppeln_waehrend_die_gegenseite_offline_ist() {
    let tmp = tempfile::tempdir().unwrap();
    let port_b = free_port();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    let b = start(&tmp.path().join("b"), "B", port_b).await;
    pair(&a, &b).await;
    publish(&a, tmp.path(), "von-a.txt").await;
    eventually("Katalog", 15, || async { remote_names(&b).await.contains(&"von-a.txt".to_string()) }).await;

    b.shutdown().await;
    drop(b);
    assert!(matches!(a.request(Request::PairRemove { device: "B".into() }).await, Response::Success { .. }));

    // B kommt zurück: beim ersten Verbindungsversuch erfährt es die Entkopplung
    let b = start(&tmp.path().join("b"), "B", port_b).await;
    let t0 = Instant::now();
    eventually("B entkoppelt nach Wiederkehr", 30, || async { devices(&b).await.is_empty() }).await;
    println!("Offline entkoppelt → beim Wiederkommen übernommen nach {:?}", t0.elapsed());
    assert!(remote_names(&b).await.is_empty());
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn neuinstallation_entkoppelt_nicht() {
    // A verliert seine Datenbank (behält aber den Schlüssel): A kennt B nicht mehr und weist es
    // mit „nicht gekoppelt“ ab – das darf bei B nichts löschen (kein ausdrückliches Entkoppeln).
    let tmp = tempfile::tempdir().unwrap();
    let port_a = free_port();
    let a = start(&tmp.path().join("a"), "A", port_a).await;
    let b = start(&tmp.path().join("b"), "B", free_port()).await;
    pair(&a, &b).await;
    eventually("Live-Verbindung", 20, || async { b.live_peers() == 1 }).await;
    let db = a.config().db_path();
    a.shutdown().await;
    drop(a);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let a = start(&tmp.path().join("a"), "A", port_a).await;
    for _ in 0..3 {
        b.request(Request::Sync).await;
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(devices(&b).await, vec!["A".to_string()], "B bleibt mit A gekoppelt");
    a.shutdown().await;
    b.shutdown().await;
}

/// Roher Client mit dem Schlüssel eines Geräts.
async fn raw_client(key_path: &Path) -> Endpoint {
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

#[tokio::test(flavor = "multi_thread")]
async fn nur_der_absender_selbst_wird_entkoppelt() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    let b = start(&tmp.path().join("b"), "B", free_port()).await;
    pair(&a, &b).await;
    // C ist ebenfalls mit B gekoppelt und benutzt danach seinen Schlüssel als roher Client
    let mut cfg_c = config(&tmp.path().join("c"), "C", free_port());
    cfg_c.live_connections = false;
    let c = Daemon::start_loopback(cfg_c).await.unwrap();
    pair(&b, &c).await;
    let c_key = c.config().secret_key_path();
    c.shutdown().await;
    drop(c);
    let ep = raw_client(&c_key).await;

    // C schließt eine Live-Verbindung mit „entkoppelt“: B entkoppelt C – aber nicht A
    let conn = ep.connect(b.network().endpoint().addr(), LIVE_ALPN).await.unwrap();
    let _ = conn.open_bi().await;
    conn.close(3u32.into(), b"unpaired");
    eventually("C entkoppelt", 10, || async { !devices(&b).await.contains(&"C".to_string()) }).await;
    assert!(devices(&b).await.contains(&"A".to_string()), "A bleibt gekoppelt");

    // Ein fremdes Gerät (nie gekoppelt) mit demselben Code bewirkt nichts
    let stranger = raw_client(&tmp.path().join("fremd.key")).await;
    let conn = stranger.connect(b.network().endpoint().addr(), LIVE_ALPN).await.unwrap();
    conn.close(3u32.into(), b"unpaired");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(devices(&b).await, vec!["A".to_string()]);

    // C (jetzt entkoppelt) versucht es erneut: B antwortet mit „entkoppelt“, nicht „unbekannt“
    let conn = ep.connect(b.network().endpoint().addr(), LIVE_ALPN).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    assert!(why.to_string().contains("unpaired"), "{why}");
    // Der fremde Client bekommt nur „nicht gekoppelt“ – B verrät nicht, wen es kennt
    let conn = stranger.connect(b.network().endpoint().addr(), LIVE_ALPN).await.unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
    assert!(why.to_string().contains("not paired"), "{why}");
    a.shutdown().await;
    b.shutdown().await;
}
