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
