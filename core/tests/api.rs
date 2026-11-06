//! Tests der API-Schicht über den echten Unix-Socket.

use space_core::api::error::ErrorCode;
use space_core::events::Event;
use space_core::ipc::{read_frame, write_frame, BrowseQuery, Envelope, Request, Response, API_VERSION};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    // Port vom Betriebssystem (0): kein Wettlauf mit parallel laufenden Tests.
    c.bind_port = None;
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
        Response::BrowsePage { items, total, .. } => {
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
    // (Vorher kann „online“ durch die Live-Verbindung gekommen sein.)
    let a_id = a.node_id();
    let st =
        wait_event(&mut sub, |e| matches!(e, Event::PeerStatusChanged { status, .. } if status == "offline")).await;
    assert_eq!(st, Event::PeerStatusChanged { node_id: a_id, status: "offline".into() });
    wait_event(&mut sub, |e| matches!(e, Event::JobQueued { .. })).await;

    // Client schließt -> Daemon beendet das Abo sauber und bleibt bedienbar
    drop(sub);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(call(&b, Request::Info).await, Response::Info(_)));
    b.shutdown().await;
}

mod permissions {
    use super::*;
    use space_core::api::auth::Caller;
    use std::path::PathBuf;

    /// Ein Beispiel für jede Anfrageart (bei neuen Varianten hier ergänzen).
    fn every_request() -> Vec<Request> {
        vec![
            Request::Publish { path: PathBuf::from("/tmp/x") },
            Request::Download { target: "abcdef".into(), output: None, force: false },
            Request::Delete { hash: Some("abcdef".into()), path: None },
            Request::List,
            Request::Ticket { hash: "abcdef".into() },
            Request::Queue,
            Request::QueueCancel { id: 1 },
            Request::QueueRetry { id: 1 },
            Request::Peers,
            Request::Info,
            Request::Browse(BrowseQuery::default()),
            Request::Subscribe,
            Request::Sync,
            Request::PairInvite,
            Request::PairInviteWait,
            Request::PairJoin { code: "x".into() },
            Request::PairList,
            Request::PairRemove { device: "x".into() },
            Request::PairGrant { device: "x".into(), capability: "files.read".into(), folder: None },
            Request::PairRevoke { device: "x".into(), capability: "files.read".into(), folder: None },
            Request::Audit { limit: 10 },
            Request::Search { query: "passwort".into(), filter: Default::default(), limit: 10 },
            Request::ListDir { path: "/".into(), show_hidden: true, offset: 0, limit: 10 },
            Request::IndexRoots,
            Request::IndexAdd { path: "/".into() },
            Request::IndexRemove { id: 1 },
            Request::IndexRescan,
            Request::FileOp {
                op: space_core::fileops::jobs::FileOp::Trash { paths: vec!["/tmp/x".into()] },
                wait: true,
            },
            Request::FileOpCancel { job: 1 },
            Request::Undo,
            Request::TrashList,
            Request::TrashRestore { id: "x".into(), keep_both: true },
            Request::TrashPurge { id: "x".into() },
            Request::TrashEmpty,
            Request::Thumbnail { path: "/etc/passwd".into(), size: Default::default() },
            Request::Metadata { path: "/etc/passwd".into() },
            Request::StreamUrl { target: "abcdef".into() },
            Request::Settings,
            Request::Upload { device: "x".into(), share: "F".into(), dest_dir: "".into(), path: "/etc/passwd".into() },
            Request::UploadList,
            Request::UploadCancel { id: 1 },
            Request::UploadRetry,
            Request::UploadRuleAdd { path: "/etc".into(), device: "x".into(), share: "F".into(), dest_dir: "".into() },
            Request::UploadRuleList,
            Request::LinkList,
            Request::SpaceUsage { path: "/".into(), limit: 10 },
            Request::BackupList,
            Request::BackupRun { id: "abcd".into() },
            Request::BackupRestore { id: "abcd".into(), snapshot: "x".into(), path: "/".into(), dest: "/tmp".into() },
            Request::BackupCreate {
                target: space_core::backup::repo::BackupTarget::Dir { path: "/tmp".into() },
                sources: vec!["/".into()],
                password: "x".repeat(12),
            },
            Request::Duplicates { path: None, min_size: None, limit: 10 },
            Request::LinkRevoke { id: "abcd".into() },
            Request::LinkCreate { path: "/etc/passwd".into(), days: None, max_downloads: None, password: None },
            Request::UploadRuleRemove { id: 1 },
            Request::SettingSet { key: "upload_daily_gb".into(), value: Some(100_000) },
            Request::RemoteThumbnail {
                device: "x".into(),
                share: "F".into(),
                path: "a.jpg".into(),
                size: Default::default(),
            },
        ]
    }

    fn is_denied(r: &Response) -> bool {
        matches!(r, Response::Error { code: ErrorCode::PermissionDenied, .. })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn permission_matrix_for_remote_callers() {
        let tmp = tempfile::tempdir().unwrap();
        let a = start(&tmp.path().join("a"), "A").await;
        let b = start(&tmp.path().join("b"), "B").await;
        let code = match a.request(Request::PairInvite).await {
            Response::PairInvite { code, .. } => code,
            other => panic!("{other:?}"),
        };
        assert!(matches!(b.request(Request::PairJoin { code }).await, Response::Paired { .. }));
        let b_id = b.node_id();
        let paired = Caller::Device(b_id.clone());
        let stranger = Caller::Device("f".repeat(64));

        let read_only = ["List", "Ticket", "Browse", "Subscribe"];
        for req in every_request() {
            let name = format!("{req:?}");
            let kind = name.split([' ', '(', '{']).next().unwrap().to_string();
            // Fremde: alles verboten
            assert!(is_denied(&a.request_as(&stranger, req.clone()).await), "Fremder darf nicht: {kind}");
            // Gekoppelt (nur catalog.read): nur Lesen der Liste
            let res = a.request_as(&paired, req.clone()).await;
            if read_only.contains(&kind.as_str()) {
                assert!(!is_denied(&res), "gekoppeltes Gerät sollte dürfen: {kind} -> {res:?}");
            } else {
                assert!(is_denied(&res), "gekoppeltes Gerät darf nicht: {kind} -> {res:?}");
            }
        }

        // Verweigerte Versuche stehen im Audit-Log (lokal einsehbar)
        match a.request(Request::Audit { limit: 1000 }).await {
            Response::Audit { entries } => {
                assert!(entries.iter().any(|e| !e.allowed && e.node_id == b_id && e.operation == "pair.grant"));
                assert!(
                    !entries.iter().any(|e| e.allowed && e.operation == "list"),
                    "erlaubtes Lesen wird nicht protokolliert"
                );
                assert!(
                    entries.iter().any(|e| !e.allowed && e.operation == "list" && e.node_id != b_id),
                    "abgelehntes Lesen eines Fremden wird protokolliert"
                );
            }
            other => panic!("{other:?}"),
        }

        // catalog.read entziehen -> auch Lesen verboten, Ledger-Abgleich von B scheitert
        assert!(matches!(
            a.request(Request::PairRevoke { device: "B".into(), capability: "catalog.read".into(), folder: None })
                .await,
            Response::Success { .. }
        ));
        assert!(is_denied(&a.request_as(&paired, Request::List).await));
        match b.request(Request::Sync).await {
            Response::SyncReport { results } => assert!(results.iter().all(|r| !r.ok), "{results:?}"),
            other => panic!("{other:?}"),
        }
        a.shutdown().await;
        b.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn granting_validates_input() {
        let tmp = tempfile::tempdir().unwrap();
        let a = start(&tmp.path().join("a"), "A").await;
        let b = start(&tmp.path().join("b"), "B").await;
        let code = match a.request(Request::PairInvite).await {
            Response::PairInvite { code, .. } => code,
            other => panic!("{other:?}"),
        };
        assert!(matches!(b.request(Request::PairJoin { code }).await, Response::Paired { .. }));
        let shared = tmp.path().join("geteilt");
        std::fs::create_dir_all(&shared).unwrap();
        let grant = |cap: &str, folder: Option<PathBuf>| Request::PairGrant {
            device: "B".into(),
            capability: cap.into(),
            folder,
        };
        let code_of = |r: Response| match r {
            Response::Error { code, .. } => Some(code),
            _ => None,
        };

        assert_eq!(code_of(a.request(grant("admin", None)).await), Some(ErrorCode::PermissionDenied));
        assert_eq!(code_of(a.request(grant("root", None)).await), Some(ErrorCode::InvalidInput));
        assert_eq!(code_of(a.request(grant("files.read", None)).await), Some(ErrorCode::InvalidInput), "Ordner fehlt");
        assert_eq!(code_of(a.request(grant("pin.request", Some(shared.clone()))).await), Some(ErrorCode::InvalidInput));
        assert_eq!(code_of(a.request(grant("files.read", Some("/".into()))).await), Some(ErrorCode::InvalidInput));
        assert_eq!(code_of(a.request(grant("files.read", Some("/etc".into()))).await), Some(ErrorCode::InvalidInput));
        let data_dir = a.config().data_dir.clone();
        assert_eq!(
            code_of(a.request(grant("files.read", Some(data_dir))).await),
            Some(ErrorCode::InvalidInput),
            "Snap-Daten"
        );
        assert_eq!(
            code_of(
                a.request(Request::PairGrant { device: "X".into(), capability: "pin.request".into(), folder: None })
                    .await
            ),
            Some(ErrorCode::NotFound)
        );

        assert!(matches!(a.request(grant("files.read", Some(shared.clone()))).await, Response::Success { .. }));
        assert!(matches!(a.request(grant("pin.request", None)).await, Response::Success { .. }));
        match a.request(Request::PairList).await {
            Response::Devices { devices } => {
                let caps: Vec<String> = devices[0].grants.iter().map(|g| g.capability.clone()).collect();
                assert_eq!(caps, vec!["catalog.read", "files.read", "pin.request"]);
                let canon = std::fs::canonicalize(&shared).unwrap();
                assert_eq!(devices[0].grants[1].folder.as_deref(), Some(canon.to_str().unwrap()));
            }
            other => panic!("{other:?}"),
        }
        a.shutdown().await;
        b.shutdown().await;
    }
}
