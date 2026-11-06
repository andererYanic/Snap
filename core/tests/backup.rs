//! Sicherungen über den Daemon: einrichten, sichern, Versionen, wiederherstellen, prüfen,
//! auf einem zweiten Gerät verbinden – inklusive Fehl- und Missbrauchsfällen.

use space_core::api::error::ErrorCode;
use space_core::backup::repo::BackupTarget;
use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::Path;

async fn start(root: &Path) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = None;
    c.live_connections = false;
    Daemon::start_loopback(c).await.unwrap()
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_versions_restore_and_new_device() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("Dokumente");
    std::fs::create_dir_all(docs.join("Verträge")).unwrap();
    std::fs::write(docs.join("Verträge/miete.pdf"), b"Version 1").unwrap();
    std::fs::write(docs.join("notizen.txt"), b"hallo").unwrap();
    let usb = tmp.path().join("usb");
    std::fs::create_dir_all(&usb).unwrap();
    let target = BackupTarget::Dir { path: usb.clone() };
    let a = start(&tmp.path().join("a")).await;

    // Ungültig: Ziel im gesicherten Ordner, Snap-Daten als Quelle, kurzes Passwort
    let inside = Request::BackupCreate {
        target: BackupTarget::Dir { path: docs.join("sicherung") },
        sources: vec![docs.clone()],
        password: "langes passwort 1".into(),
    };
    std::fs::create_dir_all(docs.join("sicherung")).unwrap();
    assert_eq!(code(&a.request(inside).await), Some(ErrorCode::InvalidInput));
    std::fs::remove_dir(docs.join("sicherung")).unwrap();
    let data = Request::BackupCreate {
        target: target.clone(),
        sources: vec![a.config().data_dir.clone()],
        password: "langes passwort 1".into(),
    };
    assert_eq!(code(&a.request(data).await), Some(ErrorCode::InvalidInput));
    let short = Request::BackupCreate { target: target.clone(), sources: vec![docs.clone()], password: "kurz".into() };
    assert_eq!(code(&a.request(short).await), Some(ErrorCode::InvalidInput));

    // Einrichten
    let created = match a
        .request(Request::BackupCreate {
            target: target.clone(),
            sources: vec![docs.clone()],
            password: "langes passwort 1".into(),
        })
        .await
    {
        Response::BackupCreated(c) => c,
        other => panic!("{other:?}"),
    };
    assert_eq!(created.recovery_key.split('-').count(), 13);
    let key = a.config().data_dir.join(format!("backup/{}.key", created.id));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    let id = created.id[..8].to_string();

    // Zwei Stände
    assert!(matches!(a.request(Request::BackupRun { id: id.clone() }).await, Response::Snapshot(_)));
    std::fs::write(docs.join("Verträge/miete.pdf"), b"Version 2 (anders)").unwrap();
    let s2 = match a.request(Request::BackupRun { id: id.clone() }).await {
        Response::Snapshot(s) => s,
        other => panic!("{other:?}"),
    };
    let snaps = match a.request(Request::BackupSnapshots { id: id.clone() }).await {
        Response::Snapshots { items } => items,
        other => panic!("{other:?}"),
    };
    assert_eq!(snaps.len(), 2);
    match a.request(Request::BackupList).await {
        Response::Backups { jobs } => {
            assert_eq!(jobs[0].last_snapshot.as_deref(), Some(s2.id.as_str()));
            assert!(jobs[0].last_error.is_none());
        }
        other => panic!("{other:?}"),
    }

    // Alte Version einer Datei zurückholen
    let out = tmp.path().join("zurück");
    std::fs::create_dir_all(&out).unwrap();
    let res = a
        .request(Request::BackupRestore {
            id: id.clone(),
            snapshot: snaps[0].id[..6].into(),
            path: docs.join("Verträge/miete.pdf"),
            dest: out.clone(),
        })
        .await;
    match res {
        Response::Restored { path, files } => {
            assert_eq!(files, 1);
            assert_eq!(std::fs::read(path).unwrap(), b"Version 1");
        }
        other => panic!("{other:?}"),
    }
    match a.request(Request::BackupBrowse { id: id.clone(), snapshot: s2.id.clone(), path: docs.clone() }).await {
        Response::BackupEntries { entries } => assert_eq!(entries.len(), 2),
        other => panic!("{other:?}"),
    }
    assert!(matches!(a.request(Request::BackupCheck { id: id.clone(), sample: 0 }).await, Response::Success { .. }));

    // Zweites Gerät: falsches Passwort abgelehnt, Wiederherstellungsschlüssel funktioniert
    let b = start(&tmp.path().join("b")).await;
    let wrong =
        Request::BackupConnect { target: target.clone(), id: created.id.clone(), secret: "falsches passwort".into() };
    assert_eq!(code(&b.request(wrong).await), Some(ErrorCode::PermissionDenied));
    let ok =
        Request::BackupConnect { target: target.clone(), id: created.id.clone(), secret: created.recovery_key.clone() };
    assert!(matches!(b.request(ok).await, Response::Success { .. }), "Wiederherstellungsschlüssel");
    match b.request(Request::BackupSnapshots { id: id.clone() }).await {
        Response::Snapshots { items } => assert_eq!(items.len(), 2),
        other => panic!("{other:?}"),
    }
    // Ohne Quellordner kann B nur wiederherstellen
    assert_eq!(code(&b.request(Request::BackupRun { id: id.clone() }).await), Some(ErrorCode::InvalidInput));
    let out_b = tmp.path().join("auf-b");
    std::fs::create_dir_all(&out_b).unwrap();
    let res = b
        .request(Request::BackupRestore {
            id: id.clone(),
            snapshot: s2.id.clone(),
            path: docs.clone(),
            dest: out_b.clone(),
        })
        .await;
    match res {
        Response::Restored { path, files } => {
            assert_eq!(files, 2);
            assert_eq!(std::fs::read(path.join("Verträge/miete.pdf")).unwrap(), b"Version 2 (anders)");
        }
        other => panic!("{other:?}"),
    }

    // Laufwerk weg → klarer Fehler; Entfernen löscht nur die lokale Einrichtung
    std::fs::rename(&usb, tmp.path().join("abgezogen")).unwrap();
    assert_eq!(code(&a.request(Request::BackupRun { id: id.clone() }).await), Some(ErrorCode::Unavailable));
    assert!(matches!(a.request(Request::BackupRemove { id: id.clone() }).await, Response::Success { .. }));
    assert!(!key.exists());
    assert!(tmp.path().join(format!("abgezogen/snap-backup-{}/config", created.id)).exists(), "Daten am Ziel bleiben");
    a.shutdown().await;
    b.shutdown().await;
}

type Store = std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>;

/// Nachgebauter S3-Speicher mit PUT/GET/DELETE (prüft die Signatur-Kopfzeilen).
async fn fake_s3() -> (u16, Store) {
    use http_body_util::BodyExt;
    let store: Store = Default::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = store.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let s = s.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let s = s.clone();
                    async move {
                        let signed = req
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .is_some_and(|a| a.starts_with("AWS4-HMAC-SHA256 Credential=K/"));
                        let path = req.uri().path().to_string();
                        let (status, body) = if !signed {
                            (403, vec![])
                        } else {
                            match *req.method() {
                                hyper::Method::PUT => {
                                    let b = req.into_body().collect().await.unwrap().to_bytes().to_vec();
                                    s.lock().unwrap().insert(path, b);
                                    (200, vec![])
                                }
                                hyper::Method::GET => match s.lock().unwrap().get(&path) {
                                    Some(b) => (200, b.clone()),
                                    None => (404, vec![]),
                                },
                                hyper::Method::DELETE => {
                                    s.lock().unwrap().remove(&path);
                                    (204, vec![])
                                }
                                _ => (405, vec![]),
                            }
                        };
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(status)
                                .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(tcp), svc)
                    .await;
            });
        }
    });
    (port, store)
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_to_s3_and_retention() {
    let tmp = tempfile::tempdir().unwrap();
    let docs = tmp.path().join("Fotos");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("a.jpg"), vec![7u8; 300_000]).unwrap();
    let (port, store) = fake_s3().await;
    let a = start(&tmp.path().join("a")).await;
    a.request(Request::LinkSetup(space_core::links::LinkSettings {
        base_url: "http://localhost:8787".into(),
        s3: space_core::links::s3::S3Config {
            endpoint: format!("http://127.0.0.1:{port}"),
            bucket: "b".into(),
            region: "auto".into(),
            access_key_id: "K".into(),
            secret_access_key: "S".into(),
        },
    }))
    .await;
    let created = match a
        .request(Request::BackupCreate {
            target: BackupTarget::S3,
            sources: vec![docs.clone()],
            password: "r2 passwort lang".into(),
        })
        .await
    {
        Response::BackupCreated(c) => c,
        other => panic!("{other:?}"),
    };
    let id = created.id[..8].to_string();
    // Nur den neuesten Stand behalten
    for k in ["backup_keep_hourly", "backup_keep_daily", "backup_keep_monthly"] {
        a.request(Request::SettingSet { key: k.into(), value: Some(0) }).await;
    }
    assert!(matches!(a.request(Request::BackupRun { id: id.clone() }).await, Response::Snapshot(_)));
    std::fs::write(docs.join("a.jpg"), vec![8u8; 300_000]).unwrap();
    assert!(matches!(a.request(Request::BackupRun { id: id.clone() }).await, Response::Snapshot(_)));
    // Alles liegt unter backups/<id>/ und ist verschlüsselt
    let keys: Vec<String> = store.lock().unwrap().keys().cloned().collect();
    assert!(keys.iter().all(|k| k.starts_with(&format!("/b/backups/{}/", created.id))), "{keys:?}");
    assert!(store.lock().unwrap().values().all(|v| !v.windows(64).any(|w| w.iter().all(|b| *b == 8))));
    match a.request(Request::BackupSnapshots { id: id.clone() }).await {
        Response::Snapshots { items } => assert_eq!(items.len(), 1, "alter Stand entfernt"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(a.request(Request::BackupCheck { id: id.clone(), sample: 0 }).await, Response::Success { .. }));
    // Alte Daten (Version 1) sind gelöscht, neue wiederherstellbar
    let out = tmp.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let snap = match a.request(Request::BackupSnapshots { id: id.clone() }).await {
        Response::Snapshots { items } => items[0].id.clone(),
        other => panic!("{other:?}"),
    };
    match a.request(Request::BackupRestore { id, snapshot: snap, path: docs.join("a.jpg"), dest: out }).await {
        Response::Restored { path, .. } => assert_eq!(std::fs::read(path).unwrap(), vec![8u8; 300_000]),
        other => panic!("{other:?}"),
    }
    a.shutdown().await;
}
