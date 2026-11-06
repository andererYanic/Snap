//! Fernoperationen (snap/ops/1) mit echten Daemons – inklusive Angriffstests:
//! fremde, entkoppelte und manipulierte Gegenstellen, Pfadtricks, Symlinks, Massenschutz.

use iroh::{endpoint::presets, Endpoint, RelayMode};
use space_core::api::error::ErrorCode;
use space_core::fileops::ops::Conflict;
use space_core::ipc::{read_frame_max, write_frame_max, Request, Response};
use space_core::remote_ops::{OpsReply, RemoteOp, OPS_ALPN};
use space_core::{Config, Daemon};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start(root: &Path, name: &str, limit: u32) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(free_port());
    c.connect_timeout_secs = 3;
    c.device_name = name.into();
    c.live_connections = false;
    c.ops_hourly_limit = limit;
    Daemon::start_loopback(c).await.unwrap()
}

async fn pair(host: &Daemon, guest: &Daemon) {
    let code = match host.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

/// A besitzt die Freigabe „Fotos“, B ist gekoppelt (nur Standardrechte + files.read).
async fn setup(tmp: &Path, limit: u32) -> (Daemon, Daemon, PathBuf) {
    let share = tmp.join("fotos");
    std::fs::create_dir_all(share.join("2024")).unwrap();
    for i in 0..5 {
        std::fs::write(share.join(format!("2024/bild{i}.jpg")), b"jpg").unwrap();
    }
    std::fs::create_dir_all(tmp.join("geheim")).unwrap();
    std::fs::write(tmp.join("geheim/passwort.txt"), b"x").unwrap();
    std::os::unix::fs::symlink(tmp.join("geheim"), share.join("abkuerzung")).unwrap();
    let a = start(&tmp.join("a"), "PC", limit).await;
    let b = start(&tmp.join("b"), "Handy", limit).await;
    pair(&a, &b).await;
    let res = a.request(Request::ShareAdd { path: share.clone(), name: Some("Fotos".into()), devices: None }).await;
    assert!(matches!(res, Response::ShareAdded { .. }), "{res:?}");
    (a, b, std::fs::canonicalize(share).unwrap())
}

async fn grant(a: &Daemon, b: &Daemon, cap: &str, folder: &Path) {
    let res = a
        .request(Request::PairGrant {
            device: b.node_id()[..12].to_string(),
            capability: cap.into(),
            folder: Some(folder.to_path_buf()),
        })
        .await;
    assert!(matches!(res, Response::Success { .. }), "{res:?}");
}

async fn remote(b: &Daemon, op: RemoteOp) -> Response {
    b.request(Request::RemoteOp { device: "PC".into(), op }).await
}

fn code(res: &Response) -> Option<ErrorCode> {
    match res {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    }
}

fn trash(paths: &[&str]) -> RemoteOp {
    RemoteOp::Trash { share: "Fotos".into(), paths: paths.iter().map(|s| s.to_string()).collect() }
}

#[tokio::test(flavor = "multi_thread")]
async fn rights_paths_and_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, share) = setup(tmp.path(), 100).await;
    let rename =
        |path: &str, new: &str| RemoteOp::Rename { share: "Fotos".into(), path: path.into(), new_name: new.into() };

    // Nur files.read (Standard der Freigabe): Schreiben/Löschen verboten, nichts passiert
    assert_eq!(code(&remote(&b, rename("2024/bild0.jpg", "neu.jpg")).await), Some(ErrorCode::PermissionDenied));
    assert_eq!(code(&remote(&b, trash(&["2024/bild0.jpg"])).await), Some(ErrorCode::PermissionDenied));
    assert!(share.join("2024/bild0.jpg").exists());

    // Mit files.write: umbenennen, Ordner anlegen, verschieben, kopieren
    grant(&a, &b, "files.write", &share).await;
    assert!(matches!(remote(&b, rename("2024/bild0.jpg", "Strand.jpg")).await, Response::Success { .. }));
    assert!(share.join("2024/Strand.jpg").exists());
    let mk = RemoteOp::Mkdir { share: "Fotos".into(), parent: "".into(), name: "Urlaub".into() };
    assert!(matches!(remote(&b, mk).await, Response::Success { .. }));
    let mv = RemoteOp::Move {
        share: "Fotos".into(),
        paths: vec!["2024/Strand.jpg".into()],
        dest_dir: "Urlaub".into(),
        conflict: Conflict::Fail,
    };
    assert!(matches!(remote(&b, mv).await, Response::Success { .. }));
    assert!(share.join("Urlaub/Strand.jpg").exists());
    let cp = RemoteOp::Copy {
        share: "Fotos".into(),
        paths: vec!["Urlaub/Strand.jpg".into()],
        dest_dir: "".into(),
        conflict: Conflict::KeepBoth,
    };
    assert!(matches!(remote(&b, cp).await, Response::Success { .. }));
    assert!(share.join("Strand.jpg").exists());
    // „Ersetzen“ braucht zusätzlich files.delete (altes Ziel → Papierkorb)
    let replace = RemoteOp::Copy {
        share: "Fotos".into(),
        paths: vec!["Urlaub/Strand.jpg".into()],
        dest_dir: "".into(),
        conflict: Conflict::Replace,
    };
    assert_eq!(code(&remote(&b, replace).await), Some(ErrorCode::PermissionDenied));

    // Pfadtricks: nie außerhalb der Freigabe
    for bad in ["../geheim/passwort.txt", "/etc/passwd", "2024/../../geheim", "", "a//b", "./x"] {
        let res = remote(&b, rename(bad, "x")).await;
        assert_eq!(code(&res), Some(ErrorCode::InvalidInput), "{bad}: {res:?}");
    }
    for bad_name in ["../x", "a/b", "..", ""] {
        assert_eq!(code(&remote(&b, rename("Strand.jpg", bad_name)).await), Some(ErrorCode::InvalidInput));
    }
    // Symlink in der Freigabe, der nach außen zeigt: weder hindurch noch auf das Ziel
    let res = remote(&b, rename("abkuerzung/passwort.txt", "weg.txt")).await;
    assert_eq!(code(&res), Some(ErrorCode::PermissionDenied), "{res:?}");
    let mv_out = RemoteOp::Move {
        share: "Fotos".into(),
        paths: vec!["Strand.jpg".into()],
        dest_dir: "abkuerzung".into(),
        conflict: Conflict::Fail,
    };
    assert_eq!(code(&remote(&b, mv_out).await), Some(ErrorCode::PermissionDenied));
    assert!(tmp.path().join("geheim/passwort.txt").exists());
    assert!(!tmp.path().join("geheim/Strand.jpg").exists());
    // Unbekannte Freigabe
    let other = RemoteOp::Trash { share: "Steuer".into(), paths: vec!["a".into()] };
    assert_eq!(code(&remote(&b, other).await), Some(ErrorCode::NotFound));

    // Papierkorb mit files.delete; endgültig nur mit files.purge
    grant(&a, &b, "files.delete", &share).await;
    assert!(matches!(remote(&b, trash(&["Strand.jpg"])).await, Response::Success { .. }));
    assert!(!share.join("Strand.jpg").exists());
    let entries = match a.request(Request::TrashList).await {
        Response::Trash { entries } => entries,
        other => panic!("{other:?}"),
    };
    assert_eq!(entries.len(), 1, "landet im Papierkorb von A");
    let purge = RemoteOp::Purge { share: "Fotos".into(), paths: vec!["2024/bild1.jpg".into()] };
    assert_eq!(code(&remote(&b, purge.clone()).await), Some(ErrorCode::PermissionDenied));
    assert!(share.join("2024/bild1.jpg").exists());
    grant(&a, &b, "files.purge", &share).await;
    assert!(matches!(remote(&b, purge).await, Response::Success { .. }));
    assert!(!share.join("2024/bild1.jpg").exists());
    // Die Freigabe-Wurzel selbst ist nie Ziel
    assert_eq!(code(&remote(&b, trash(&[""])).await), Some(ErrorCode::InvalidInput));

    // Fernänderungen lassen sich auf A rückgängig machen
    assert!(matches!(a.request(Request::Undo).await, Response::Success { .. }));
    assert!(share.join("Strand.jpg").exists(), "Papierkorb-Eintrag wiederhergestellt");

    // Audit-Log auf A hält Ablehnungen fest
    let audit = match a.request(Request::Audit { limit: 200 }).await {
        Response::Audit { entries } => entries,
        other => panic!("{other:?}"),
    };
    assert!(audit.iter().any(|e| !e.allowed && e.operation == "ops.purge"));
    assert!(audit.iter().any(|e| e.allowed && e.operation == "ops.rename"));

    // Entkoppelt: sofort abgelehnt
    a.request(Request::PairRemove { device: "Handy".into() }).await;
    let res = remote(&b, trash(&["2024/bild2.jpg"])).await;
    assert!(code(&res).is_some(), "{res:?}");
    assert!(share.join("2024/bild2.jpg").exists());
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mass_changes_need_approval() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, share) = setup(tmp.path(), 3).await;
    grant(&a, &b, "files.delete", &share).await;
    let mut events = a.subscribe();

    // 2 Dateien: unter dem Limit (3/h)
    assert!(matches!(remote(&b, trash(&["2024/bild0.jpg", "2024/bild1.jpg"])).await, Response::Success { .. }));
    // 3 weitere (ganzer Ordner zählt seine Dateien): angehalten, nichts gelöscht
    let res = remote(&b, trash(&["2024"])).await;
    assert_eq!(code(&res), Some(ErrorCode::NeedsApproval), "{res:?}");
    assert!(share.join("2024/bild2.jpg").exists());
    let ev = loop {
        match tokio::time::timeout(Duration::from_secs(5), events.next()).await.unwrap() {
            Some(space_core::events::Event::ApprovalRequested { id, summary, .. }) => break (id, summary),
            Some(_) => continue,
            None => panic!("Bus zu"),
        }
    };
    assert!(ev.1.contains("Papierkorb"), "{}", ev.1);

    // Weitere Anfragen über dem Rest des Stundenfensters bleiben ebenfalls angehalten
    let res = remote(&b, trash(&["2024/bild2.jpg", "2024/bild3.jpg"])).await;
    assert_eq!(code(&res), Some(ErrorCode::NeedsApproval));

    let items = match a.request(Request::OpsPending).await {
        Response::Approvals { items } => items,
        other => panic!("{other:?}"),
    };
    assert_eq!(items.len(), 2);
    assert_eq!(items[1].id, ev.0);
    assert_eq!(items[0].device_name.as_deref(), Some("Handy"));
    // Ablehnen und bestätigen
    assert!(matches!(a.request(Request::OpsReject { id: items[0].id }).await, Response::Success { .. }));
    assert!(share.join("2024/bild3.jpg").exists(), "abgelehnt: nichts passiert");
    assert!(matches!(a.request(Request::OpsApprove { id: ev.0 }).await, Response::Success { .. }));
    assert!(!share.join("2024").exists(), "nach Bestätigung ausgeführt");
    assert_eq!(code(&a.request(Request::OpsApprove { id: ev.0 }).await), Some(ErrorCode::NotFound), "nur einmal");
    // Gerät kann sich nicht selbst bestätigen
    let dev = space_core::api::auth::Caller::Device(b.node_id());
    let res = a.request_as(&dev, Request::OpsApprove { id: 1 }).await;
    assert_eq!(code(&res), Some(ErrorCode::PermissionDenied));
    a.shutdown().await;
    b.shutdown().await;
}

/// Rohe Verbindungen: fremde Gegenstelle, kaputte und manipulierte Nachrichten.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_peers_and_frames() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b, share) = setup(tmp.path(), 100).await;
    grant(&a, &b, "files.delete", &share).await;
    let a_addr = a.network().endpoint().addr();

    // Fremder (nicht gekoppelt): Verbindung wird ohne Antwort getrennt
    let stranger = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let conn = stranger.connect(a_addr.clone(), OPS_ALPN).await.unwrap();
    let got = async {
        let (mut s, mut r) = conn.open_bi().await?;
        let req = serde_json::json!({"v":1,"op":{"op":"Trash","share":"Fotos","paths":["2024/bild0.jpg"]}});
        write_frame_max(&mut s, &req, 64 * 1024).await?;
        s.finish()?;
        read_frame_max::<_, OpsReply>(&mut r, 64 * 1024).await
    }
    .await;
    assert!(got.is_err(), "Fremder bekommt keine Antwort: {got:?}");
    assert!(share.join("2024/bild0.jpg").exists());

    // Gekoppelt, aber manipulierte Nachrichten
    let raw = |payload: Vec<u8>| {
        let ep = b.network().endpoint().clone();
        let addr = a_addr.clone();
        async move {
            let conn = ep.connect(addr, OPS_ALPN).await.unwrap();
            let (mut s, mut r) = conn.open_bi().await.unwrap();
            s.write_all(&payload).await.unwrap();
            s.finish().unwrap();
            let reply = read_frame_max::<_, OpsReply>(&mut r, 64 * 1024).await;
            conn.close(0u32.into(), b"");
            reply
        }
    };
    let frame = |json: &str| {
        let mut v = (json.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(json.as_bytes());
        v
    };
    for bad in [
        r#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":["../geheim/passwort.txt"]}}"#,
        r#"{"v":1,"op":{"op":"Trash","share":"Fotos","paths":["2024/bild0.jpg"],"als":"root"}}"#,
        r#"{"v":1,"op":{"op":"Format","share":"Fotos"}}"#,
        r#"{"v":1}"#,
        "kein json",
    ] {
        let reply = raw(frame(bad)).await.expect("Antwort mit Fehler");
        assert!(!reply.ok, "{bad}");
        assert_eq!(reply.code, Some(ErrorCode::InvalidInput), "{bad}: {reply:?}");
    }
    let reply = raw(frame(r#"{"v":99,"op":{"op":"Trash","share":"Fotos","paths":["2024/bild0.jpg"]}}"#)).await.unwrap();
    assert_eq!(reply.code, Some(ErrorCode::VersionMismatch));
    // Zu großer Frame (angekündigt 1 GiB) und abgeschnittener Frame
    let reply = raw((1u32 << 30).to_be_bytes().to_vec()).await.unwrap();
    assert_eq!(reply.code, Some(ErrorCode::InvalidInput));
    let reply = raw(vec![0, 0, 1, 0, b'{']).await.unwrap();
    assert_eq!(reply.code, Some(ErrorCode::InvalidInput));
    assert!(share.join("2024/bild0.jpg").exists());
    assert!(tmp.path().join("geheim/passwort.txt").exists());

    // Daemon arbeitet danach normal weiter
    assert!(matches!(remote(&b, trash(&["2024/bild0.jpg"])).await, Response::Success { .. }));
    a.shutdown().await;
    b.shutdown().await;
}
