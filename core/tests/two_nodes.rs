//! Integrationstests mit zwei echten Daemons auf 127.0.0.1 (ohne Relais/Internet).

use iroh::{endpoint::presets, Endpoint, RelayMode};
use iroh_blobs::{protocol::ObserveRequest, store::mem::MemStore, ticket::BlobTicket, BlobFormat, Hash};
use n0_future::StreamExt;
use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn config(root: &Path, port: u16) -> Config {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(port);
    c.connect_timeout_secs = 2;
    c
}

async fn start(root: &Path, port: u16) -> Daemon {
    Daemon::start_loopback(config(root, port)).await.expect("Daemon startet")
}

async fn publish(d: &Daemon, path: &Path) -> (String, String) {
    match d.request(Request::Publish { path: path.to_path_buf() }).await {
        Response::Published { ticket, hash, .. } => (ticket, hash),
        other => panic!("Publish fehlgeschlagen: {other:?}"),
    }
}

async fn download(d: &Daemon, target: &str, out: Option<PathBuf>) -> Response {
    d.request(Request::Download { target: target.to_string(), output: out, force: false }).await
}

/// Freier UDP-Port auf 127.0.0.1.
fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// Ein fremder Client ohne eigenen Daemon, der versucht, Daten abzugreifen.
async fn attacker() -> (Endpoint, MemStore) {
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    (ep, MemStore::new())
}

/// Versucht, einen Hash direkt per iroh-blobs von `target` zu holen.
async fn raw_fetch(target: &Daemon, hash: Hash) -> bool {
    let (ep, store) = attacker().await;
    let addr = target.network().endpoint().addr();
    let conn = ep.connect(addr, iroh_blobs::ALPN).await.expect("Verbindung");
    let ok = store.remote().fetch(conn, hash).await.is_ok() && store.has(hash).await.unwrap_or(false);
    ep.close().await;
    ok
}

/// Fragt per Observe an, ob `target` einen Hash besitzt.
async fn raw_observe(target: &Daemon, hash: Hash) -> bool {
    let (ep, store) = attacker().await;
    let addr = target.network().endpoint().addr();
    let conn = ep.connect(addr, iroh_blobs::ALPN).await.expect("Verbindung");
    let mut stream = std::pin::pin!(store.remote().observe(conn, ObserveRequest::new(hash)));
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next()).await;
    ep.close().await;
    matches!(first, Ok(Some(Ok(_))))
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_download_and_access_control() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), free_port()).await;
    let b = start(&tmp.path().join("b"), free_port()).await;

    let file = tmp.path().join("geheim.txt");
    std::fs::write(&file, b"streng vertraulich").unwrap();
    let (ticket, hash_hex) = publish(&a, &file).await;
    let hash: Hash = hash_hex.parse().unwrap();

    // Normaler Download per Ticket in den Standardordner
    let path = match download(&b, &ticket, None).await {
        Response::Downloaded { path } => path,
        other => panic!("Download fehlgeschlagen: {other:?}"),
    };
    assert_eq!(std::fs::read(&path).unwrap(), b"streng vertraulich");
    assert!(path.starts_with(tmp.path().join("b/downloads")));

    // Freigegebene Datei: Observe erlaubt
    assert!(raw_observe(&a, hash).await);
    assert!(raw_fetch(&a, hash).await);

    // B hat die Datei heruntergeladen, aber NICHT freigegeben -> B liefert sie nicht aus
    assert!(!raw_fetch(&b, hash).await, "heruntergeladene Datei darf nicht ausgeliefert werden");
    assert!(!raw_observe(&b, hash).await, "Besitz darf nicht ausspähbar sein");

    // Ein ganz unbekannter Hash ist ebenfalls nicht abfragbar
    let unknown = Hash::new(b"gibt es nicht");
    assert!(!raw_observe(&a, unknown).await);

    // Nach dem Löschen liefert A die Datei sofort nicht mehr aus
    match a.request(Request::Delete { hash: Some(hash_hex[..8].to_string()), path: None }).await {
        Response::Success { .. } => {}
        other => panic!("Delete fehlgeschlagen: {other:?}"),
    }
    assert!(!raw_fetch(&a, hash).await, "gelöschte Datei darf nicht ausgeliefert werden");

    // Ein dritter Node bekommt eine klare Fehlermeldung statt eines Warteschlangen-Eintrags
    let c = start(&tmp.path().join("c"), free_port()).await;
    match download(&c, &ticket, None).await {
        Response::Error { message } => assert!(message.contains("nicht"), "{message}"),
        other => panic!("erwartet Fehler, bekam {other:?}"),
    }
    match c.request(Request::Queue).await {
        Response::Queue { jobs } => assert!(jobs.is_empty()),
        other => panic!("{other:?}"),
    }

    for d in [a, b, c] {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_dangerous_inputs() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), free_port()).await;
    let file = tmp.path().join("x.txt");
    std::fs::write(&file, b"x").unwrap();
    publish(&a, &file).await;

    for bad in ["%", "", "abc", "aaaa_%"] {
        let res = a.request(Request::Delete { hash: Some(bad.into()), path: None }).await;
        assert!(matches!(res, Response::Error { .. }), "'{bad}' darf nichts löschen");
    }
    match a.request(Request::List).await {
        Response::FileList { files } => assert_eq!(files.len(), 1),
        other => panic!("{other:?}"),
    }
    // Relative Pfade werden vom Daemon nicht akzeptiert
    let res = a.request(Request::Publish { path: PathBuf::from("x.txt") }).await;
    assert!(matches!(res, Response::Error { .. }));
    // Verzeichnisse können nicht freigegeben werden
    let res = a.request(Request::Publish { path: tmp.path().to_path_buf() }).await;
    assert!(matches!(res, Response::Error { .. }));

    // Zweiter Daemon auf demselben Socket wird abgelehnt
    assert!(Daemon::start_loopback(config(&tmp.path().join("a"), free_port())).await.is_err());
    a.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_peer_is_queued_and_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let port_a = free_port();
    let a = start(&tmp.path().join("a"), port_a).await;
    let b = start(&tmp.path().join("b"), free_port()).await;
    let node_a = a.node_id();

    let file = tmp.path().join("bild.bin");
    std::fs::write(&file, vec![42u8; 200_000]).unwrap();
    let (ticket, _) = publish(&a, &file).await;
    a.shutdown().await;
    drop(a);

    // A ist offline -> Auftrag landet in der Warteschlange
    let out = tmp.path().join("ziel.bin");
    let job_id = match download(&b, &ticket, Some(out.clone())).await {
        Response::Queued { job_id, peer, already_queued, .. } => {
            assert_eq!(peer, node_a);
            assert!(!already_queued);
            job_id
        }
        other => panic!("erwartet Queued, bekam {other:?}"),
    };
    // Doppelter Auftrag wird erkannt
    assert!(matches!(
        download(&b, &ticket, Some(out.clone())).await,
        Response::Queued { already_queued: true, .. }
    ));
    match b.request(Request::Peers).await {
        Response::Peers { peers } => {
            assert_eq!(peers.len(), 1);
            assert_eq!(peers[0].status, "offline");
            assert!(peers[0].next_retry_at.is_some());
        }
        other => panic!("{other:?}"),
    }

    // A kommt mit derselben Identität zurück
    let a = start(&tmp.path().join("a"), port_a).await;
    assert_eq!(a.node_id(), node_a, "Node-ID muss über Neustarts stabil sein");

    // Sofortiger Retry (statt 30 s Backoff abzuwarten)
    assert!(matches!(b.request(Request::QueueRetry { id: job_id }).await, Response::Success { .. }));
    let mut done = false;
    for _ in 0..100 {
        if out.exists() {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(done, "Hintergrund-Download wurde nicht ausgeführt");
    assert_eq!(std::fs::read(&out).unwrap(), vec![42u8; 200_000]);

    // Warteschlange leer, Peer online
    let mut empty = false;
    for _ in 0..50 {
        if let Response::Queue { jobs } = b.request(Request::Queue).await {
            if jobs.is_empty() {
                empty = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(empty);
    match b.request(Request::Peers).await {
        Response::Peers { peers } => assert_eq!(peers[0].status, "online"),
        other => panic!("{other:?}"),
    }

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn incoming_connection_wakes_worker() {
    // Kein Ping: Meldet sich ein bekannter Peer aus eigenem Anlass bei uns,
    // nutzen wir die Gelegenheit, um wartende Aufträge sofort zu erledigen.
    let tmp = tempfile::tempdir().unwrap();
    let port_a = free_port();
    let a = start(&tmp.path().join("a"), port_a).await;
    let b = start(&tmp.path().join("b"), free_port()).await;

    let fa = tmp.path().join("von_a.txt");
    std::fs::write(&fa, b"von a").unwrap();
    let (ticket_a, _) = publish(&a, &fa).await;
    let fb = tmp.path().join("von_b.txt");
    std::fs::write(&fb, b"von b").unwrap();
    let (ticket_b, _) = publish(&b, &fb).await;

    a.shutdown().await;
    drop(a);
    let out = tmp.path().join("ziel.txt");
    assert!(matches!(download(&b, &ticket_a, Some(out.clone())).await, Response::Queued { .. }));

    // A startet neu und lädt seinerseits etwas von B -> B erkennt A als online
    let a = start(&tmp.path().join("a"), port_a).await;
    let ticket_b: BlobTicket = ticket_b.parse().unwrap();
    assert_eq!(ticket_b.format(), BlobFormat::Raw);
    assert!(matches!(
        download(&a, &ticket_b.to_string(), Some(tmp.path().join("a_hat.txt"))).await,
        Response::Downloaded { .. }
    ));

    let mut done = false;
    for _ in 0..100 {
        if out.exists() {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(done, "eingehende Verbindung hat den Worker nicht geweckt");
    assert_eq!(std::fs::read(&out).unwrap(), b"von a");

    a.shutdown().await;
    b.shutdown().await;
}
