//! Tests der API-Schicht über den echten Unix-Socket.

use space_core::api::error::ErrorCode;
use space_core::events::Event;
use space_core::ipc::{read_frame, write_frame, BrowseQuery, Envelope, Request, Response, API_VERSION};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(free_port());
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    Daemon::start_loopback(c).await.unwrap()
}

async fn call(d: &Daemon, req: Request) -> Response {
    let mut s = UnixStream::connect(&d.config().socket_path).await.unwrap();
    write_frame(&mut s, &Envelope::new(req)).await.unwrap();
    read_frame(&mut s).await.unwrap()
}

async fn next_event(s: &mut UnixStream) -> Event {
    match tokio::time::timeout(Duration::from_secs(5), read_frame::<_, Response>(s)).await {
        Ok(Ok(Response::Event(e))) => e,
        other => panic!("kein Ereignis: {other:?}"),
    }
}

/// Liest Ereignisse, bis eines `pred` erfüllt.
async fn wait_event(s: &mut UnixStream, pred: impl Fn(&Event) -> bool) -> Event {
    loop {
        let e = next_event(s).await;
        if pred(&e) {
            return e;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn version_mismatch_is_reported_with_code() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(tmp.path(), "A").await;
    let mut s = UnixStream::connect(&d.config().socket_path).await.unwrap();
    write_frame(&mut s, &Envelope { v: API_VERSION + 1, req: Request::Info }).await.unwrap();
    match read_frame::<_, Response>(&mut s).await.unwrap() {
        Response::Error { code, message } => {
            assert_eq!(code, ErrorCode::VersionMismatch);
            assert!(message.contains("API-Version"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // Fehlercodes für typische Fälle
    match call(&d, Request::Ticket { hash: "abcdef".into() }).await {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    match call(&d, Request::Delete { hash: Some("%%%%%%".into()), path: None }).await {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidInput),
        other => panic!("{other:?}"),
    }
    match call(&d, Request::PairJoin { code: "kaputt".into() }).await {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::InvalidInput),
        other => panic!("{other:?}"),
    }
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_streams_live_events_across_devices() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A").await;
    let b = start(&tmp.path().join("b"), "B").await;

    // B abonniert über den Socket
    let mut sub = UnixStream::connect(&b.config().socket_path).await.unwrap();
    write_frame(&mut sub, &Envelope::new(Request::Subscribe)).await.unwrap();
    assert!(matches!(read_frame::<_, Response>(&mut sub).await.unwrap(), Response::Success { .. }));

    // Eigene Freigabe -> LocalCatalogChanged
    let f = tmp.path().join("x.txt");
    std::fs::write(&f, b"x").unwrap();
    assert!(matches!(call(&b, Request::Publish { path: f }).await, Response::Published { .. }));
    assert_eq!(next_event(&mut sub).await, Event::LocalCatalogChanged);

    // Koppeln -> DevicePaired, danach Abgleich -> RemoteCatalogChanged + PeerStatusChanged
    let g = tmp.path().join("von_a.txt");
    std::fs::write(&g, b"a").unwrap();
    assert!(matches!(call(&a, Request::Publish { path: g }).await, Response::Published { .. }));
    let code = match call(&a, Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(call(&b, Request::PairJoin { code }).await, Response::Paired { .. }));
    let paired = wait_event(&mut sub, |e| matches!(e, Event::DevicePaired { .. })).await;
    assert_eq!(paired, Event::DevicePaired { node_id: a.node_id(), name: "A".into() });
    wait_event(&mut sub, |e| matches!(e, Event::RemoteCatalogChanged { new_entries } if *new_entries >= 1)).await;

    // Seitenweise Abfrage mit Suche findet die Datei von A
    match call(&b, Request::Browse(BrowseQuery { search: Some("von_".into()), ..Default::default() })).await {
        Response::BrowsePage { items, total } => {
            assert_eq!(total, 1);
            assert_eq!(items[0].origin.as_deref(), Some(a.node_id().as_str()));
            assert_eq!(items[0].origin_name.as_deref(), Some("A"));
        }
        other => panic!("{other:?}"),
    }

    // A geht offline, B versucht zu laden -> PeerStatusChanged(offline) + JobQueued
    a.shutdown().await;
    match call(&b, Request::Download { target: "von_a.txt".into(), output: None, force: false }).await {
        Response::Queued { .. } => {}
        other => panic!("{other:?}"),
    }
    let st = wait_event(&mut sub, |e| matches!(e, Event::PeerStatusChanged { .. })).await;
    assert_eq!(st, Event::PeerStatusChanged { node_id: a.node_id(), status: "offline".into() });
    wait_event(&mut sub, |e| matches!(e, Event::JobQueued { .. })).await;

    // Client schließt -> Daemon beendet das Abo sauber und bleibt bedienbar
    drop(sub);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(call(&b, Request::Info).await, Response::Info(_)));
    b.shutdown().await;
}
