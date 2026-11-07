//! Integrationstests für Live-Verbindungen (Phase 1b).

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
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

async fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "Zeitüberschreitung: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_event(
    sub: &mut space_core::events::Subscription,
    timeout: Duration,
    pred: impl Fn(&Event) -> bool,
) -> Duration {
    let start = Instant::now();
    loop {
        let left = timeout.checked_sub(start.elapsed()).expect("Ereignis kam nicht");
        match tokio::time::timeout(left, sub.next()).await {
            Ok(Some(e)) if pred(&e) => return start.elapsed(),
            Ok(Some(_)) => {}
            _ => panic!("Ereignis kam nicht rechtzeitig"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_and_status_arrive_live() {
    let tmp = tempfile::tempdir().unwrap();
    let port_a = free_port();
    let a = start(&tmp.path().join("a"), "A", port_a).await;
    let b = start(&tmp.path().join("b"), "B", free_port()).await;
    pair(&a, &b).await;
    wait_until("Live-Verbindung", Duration::from_secs(20), || a.live_peers() == 1 && b.live_peers() == 1).await;

    // Änderung auf A erscheint bei B ohne manuellen Abgleich
    let mut sub = b.subscribe();
    let f = tmp.path().join("live.txt");
    std::fs::write(&f, b"live").unwrap();
    assert!(matches!(a.request(Request::Publish { path: f }).await, Response::Published { .. }));
    // Gemessen wird, wann die Datei wirklich in B's Katalog steht (kein manueller Abgleich).
    let t0 = Instant::now();
    loop {
        let visible = match b.request(Request::List).await {
            Response::Catalog { remote, .. } => remote.iter().any(|f| f.name == "live.txt"),
            _ => false,
        };
        if visible {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(5), "Änderung kam nicht live an");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    println!("Änderung -> anderes Gerät: {:?}", t0.elapsed());

    // Genau eine Verbindung pro Paar (Duplikate aufgelöst)
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!((a.live_peers(), b.live_peers()), (1, 1));

    // A fährt herunter -> B sieht sofort "offline"
    let a_id = a.node_id();
    a.shutdown().await;
    drop(a);
    let t = wait_event(
        &mut sub,
        Duration::from_secs(10),
        |e| matches!(e, Event::PeerStatusChanged { node_id, status } if *node_id == a_id && status == "offline"),
    )
    .await;
    println!("Offline erkannt nach: {t:?}");

    // Download landet in der Warteschlange; A kommt zurück -> Auftrag läuft von selbst
    let out = tmp.path().join("ziel.txt");
    let res = b.request(Request::Download { target: "live.txt".into(), output: Some(out.clone()), force: false }).await;
    assert!(matches!(res, Response::Queued { .. }), "{res:?}");
    let a = start(&tmp.path().join("a"), "A", port_a).await;
    let start = Instant::now();
    wait_until("Auftrag nach Wiederverbindung", Duration::from_secs(30), || out.exists()).await;
    println!("Wieder da -> Download fertig: {:?}", start.elapsed());
    assert_eq!(std::fs::read(&out).unwrap(), b"live");

    a.shutdown().await;
    b.shutdown().await;
}

/// Roher Client mit dem Schlüssel eines (gekoppelten) Geräts.
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

/// Verbindet sich live, sendet `payload` und liefert den Schließgrund.
async fn send_raw(ep: &Endpoint, target: &Daemon, frames: Vec<Vec<u8>>) -> String {
    let conn = ep.connect(target.network().endpoint().addr(), LIVE_ALPN).await.unwrap();
    if let Ok((mut send, _recv)) = conn.open_bi().await {
        for f in frames {
            if send.write_all(&f).await.is_err() {
                break;
            }
        }
    }
    tokio::time::timeout(Duration::from_secs(10), conn.closed())
        .await
        .map(|e| e.to_string())
        .unwrap_or_else(|_| "offen".into())
}

fn frame(json: &str) -> Vec<u8> {
    let mut v = (json.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(json.as_bytes());
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_peers_are_disconnected() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    // C wird gekoppelt und danach als roher (bösartiger) Client benutzt
    let mut cfg_c = config(&tmp.path().join("c"), "C", free_port());
    cfg_c.live_connections = false;
    let c = Daemon::start_loopback(cfg_c.clone()).await.unwrap();
    pair(&a, &c).await;
    c.shutdown().await;
    drop(c);
    let key = cfg_c.secret_key_path();

    // Fremder ohne Kopplung
    let stranger = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let reason = send_raw(&stranger, &a, vec![frame(r#"{"t":"Hello","v":1,"total":0}"#)]).await;
    assert!(reason.contains("not paired"), "{reason}");

    let ep = raw_client(&key).await;
    // Flut
    let flood: Vec<Vec<u8>> = (0..500).map(|_| frame(r#"{"t":"Heartbeat"}"#)).collect();
    let reason = send_raw(&ep, &a, flood).await;
    assert_ne!(reason, "offen", "Flut muss zur Trennung führen");
    // Übergroßer Frame
    let mut big = (1_000_000u32).to_be_bytes().to_vec();
    big.extend(vec![b'x'; 10]);
    assert_ne!(send_raw(&ep, &a, vec![big]).await, "offen");
    // Unbekannte Nachricht (z. B. Versuch, Daten zu pushen)
    assert_ne!(send_raw(&ep, &a, vec![frame(r#"{"t":"Push","data":"boese"}"#)]).await, "offen");
    // Falsche Protokollversion
    assert_ne!(send_raw(&ep, &a, vec![frame(r#"{"t":"Hello","v":99,"total":0}"#)]).await, "offen");

    // Recht entzogen -> neue Verbindungen werden abgewiesen
    assert!(matches!(
        a.request(Request::PairRevoke { device: "C".into(), capability: "catalog.read".into(), folder: None }).await,
        Response::Success { .. }
    ));
    let reason = send_raw(&ep, &a, vec![frame(r#"{"t":"Hello","v":1,"total":0}"#)]).await;
    assert!(reason.contains("not paired"), "{reason}");
    // A ist nach all dem weiter bedienbar
    assert!(matches!(a.request(Request::Info).await, Response::Info(_)));
    ep.close().await;
    stranger.close().await;
    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_catalog_read_drops_live_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    let b = start(&tmp.path().join("b"), "B", free_port()).await;
    pair(&a, &b).await;
    wait_until("Live-Verbindung", Duration::from_secs(20), || a.live_peers() == 1).await;
    assert!(matches!(
        a.request(Request::PairRevoke { device: "B".into(), capability: "catalog.read".into(), folder: None }).await,
        Response::Success { .. }
    ));
    wait_until("Trennung nach Entzug", Duration::from_secs(5), || a.live_peers() == 0).await;
    // Keine Seite baut die Verbindung wieder auf: B wird abgewiesen, und A wählt B
    // nicht mehr an (A's Ledger-Stand darf nicht mehr zu B fließen).
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert_eq!((a.live_peers(), b.live_peers()), (0, 0));
    // Wieder gewähren -> Verbindung kommt zurück
    assert!(matches!(
        a.request(Request::PairGrant { device: "B".into(), capability: "catalog.read".into(), folder: None }).await,
        Response::Success { .. }
    ));
    wait_until("Wiederverbindung nach Gewähren", Duration::from_secs(30), || a.live_peers() == 1).await;
    a.shutdown().await;
    b.shutdown().await;
}

/// Geräteliste von `d` → (online, Speicher) des Geräts `name`.
async fn device_storage(d: &Daemon, name: &str) -> (bool, Option<space_core::database::DeviceStorage>) {
    match d.request(Request::PairList).await {
        Response::Devices { devices } => {
            let dev = devices.into_iter().find(|x| x.name == name).expect("Gerät fehlt");
            (dev.online, dev.storage)
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_is_reported_to_paired_devices() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    let b = start(&tmp.path().join("b"), "B", free_port()).await;
    let share = tmp.path().join("freigabe");
    std::fs::create_dir_all(&share).unwrap();
    assert!(matches!(
        a.request(Request::ShareAdd { path: share.clone(), name: Some("F".into()), devices: None }).await,
        Response::ShareAdded { .. }
    ));
    pair(&a, &b).await;
    wait_until("Live-Verbindung", Duration::from_secs(20), || a.live_peers() == 1 && b.live_peers() == 1).await;
    // B kennt A's Speicher (Summe der Freigaben), A kennt von B keinen (B gibt nichts frei)
    let start_t = Instant::now();
    let st = loop {
        if let (true, Some(s)) = device_storage(&b, "A").await {
            break s;
        }
        assert!(start_t.elapsed() < Duration::from_secs(10), "Speicher kam nicht an");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (expect_total, _) = space_core::live::shared_storage(std::slice::from_ref(&share)).unwrap();
    assert_eq!(st.total, expect_total);
    assert!(st.free <= st.total && st.updated_at > 0);
    assert_eq!(device_storage(&a, "B").await, (true, None));
    // Doppelt freigegebenes Laufwerk wird nur einmal gezählt
    let second = tmp.path().join("zweite");
    std::fs::create_dir_all(&second).unwrap();
    assert_eq!(space_core::live::shared_storage(&[share.clone(), second]).unwrap().0, expect_total);
    assert_eq!(space_core::live::shared_storage(&[]), None);
    a.shutdown().await;
    b.shutdown().await;
}

/// Liest bis zu `wait` lang Frames vom Daemon (nach dem Senden von `frames`).
async fn exchange(ep: &Endpoint, target: &Daemon, frames: Vec<Vec<u8>>, wait: Duration) -> (Vec<String>, bool) {
    let conn = ep.connect(target.network().endpoint().addr(), LIVE_ALPN).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    for f in frames {
        send.write_all(&f).await.unwrap();
    }
    let mut got = Vec::new();
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut len = [0u8; 4];
        match tokio::time::timeout(left, recv.read_exact(&mut len)).await {
            Ok(Ok(())) => {
                let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
                if recv.read_exact(&mut body).await.is_err() {
                    break;
                }
                got.push(String::from_utf8_lossy(&body).into_owned());
            }
            _ => break,
        }
    }
    let open = conn.close_reason().is_none();
    conn.close(0u32.into(), b"ende");
    (got, open)
}

#[tokio::test(flavor = "multi_thread")]
async fn hostile_storage_reports_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A", free_port()).await;
    let share = tmp.path().join("freigabe");
    std::fs::create_dir_all(&share).unwrap();
    assert!(matches!(
        a.request(Request::ShareAdd { path: share, name: Some("F".into()), devices: None }).await,
        Response::ShareAdded { .. }
    ));
    let mut cfg_c = config(&tmp.path().join("c"), "C", free_port());
    cfg_c.live_connections = false;
    let c = Daemon::start_loopback(cfg_c.clone()).await.unwrap();
    pair(&a, &c).await;
    c.shutdown().await;
    drop(c);
    let ep = raw_client(&cfg_c.secret_key_path()).await;

    // Fremder ohne Kopplung bekommt weder Verbindung noch Speicher
    let stranger = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let reason = send_raw(&stranger, &a, vec![frame(r#"{"t":"Hello","v":2,"total":0}"#)]).await;
    assert!(reason.contains("not paired"), "{reason}");

    // Alte Gegenstelle (Protokoll 1): Verbindung bleibt, aber keine Speichermeldung
    let (got, open) = exchange(&ep, &a, vec![frame(r#"{"t":"Hello","v":1,"total":0}"#)], Duration::from_secs(2)).await;
    assert!(open && got.iter().any(|m| m.contains("Hello")), "{got:?}");
    assert!(!got.iter().any(|m| m.contains("Storage")), "kein Speicher an Protokoll 1: {got:?}");
    // Protokoll 2 mit catalog.read: Speicher wird gemeldet
    let (got, _) = exchange(&ep, &a, vec![frame(r#"{"t":"Hello","v":2,"total":0}"#)], Duration::from_secs(2)).await;
    assert!(got.iter().any(|m| m.contains("\"Storage\"")), "{got:?}");

    // Manipulierte Werte beenden die Verbindung und werden nicht übernommen
    let hello = || frame(r#"{"t":"Hello","v":2,"total":0}"#);
    for bad in [
        r#"{"t":"Storage","total":100,"free":200}"#,
        r#"{"t":"Storage","total":0,"free":0}"#,
        r#"{"t":"Storage","total":2305843009213693952,"free":1}"#,
        r#"{"t":"Storage","total":-5,"free":1}"#,
        r#"{"t":"Storage","total":100,"free":1,"pfad":"/home"}"#,
    ] {
        let (_, open) = exchange(&ep, &a, vec![hello(), frame(bad)], Duration::from_secs(2)).await;
        assert!(!open, "{bad} muss zur Trennung führen");
        assert_eq!(device_storage(&a, "C").await.1, None, "{bad} darf nicht übernommen werden");
    }
    // Gültige Meldung wird übernommen, eine zweite sofort danach verworfen (Ratenbegrenzung)
    let (_, open) = exchange(
        &ep,
        &a,
        vec![
            hello(),
            frame(r#"{"t":"Storage","total":1000000,"free":250000}"#),
            frame(r#"{"t":"Storage","total":999,"free":1}"#),
        ],
        Duration::from_secs(1),
    )
    .await;
    assert!(open);
    let s = device_storage(&a, "C").await.1.expect("gültige Meldung übernommen");
    assert_eq!((s.total, s.free), (1_000_000, 250_000));

    // Recht entzogen: keine Verbindung mehr, also auch kein Speicher mehr
    assert!(matches!(
        a.request(Request::PairRevoke { device: "C".into(), capability: "catalog.read".into(), folder: None }).await,
        Response::Success { .. }
    ));
    let reason = send_raw(&ep, &a, vec![hello()]).await;
    assert!(reason.contains("not paired"), "{reason}");
    // Entkoppeln vergisst den gemeldeten Speicher
    assert!(matches!(a.request(Request::PairRemove { device: "C".into() }).await, Response::Success { .. }));
    match a.request(Request::PairList).await {
        Response::Devices { devices } => assert!(devices.iter().all(|d| d.name != "C")),
        other => panic!("{other:?}"),
    }
    assert!(matches!(a.request(Request::Info).await, Response::Info(_)));
    ep.close().await;
    stranger.close().await;
    a.shutdown().await;
}
