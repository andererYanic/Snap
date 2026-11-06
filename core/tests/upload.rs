//! Hochladen auf gekoppelte Geräte und Kamera-Upload-Regeln – mit Angriffstests
//! (fehlende Rechte, gesperrt, Platzreserve, versteckte Namen, nicht angebotene Inhalte,
//! falsche Größenangaben) und Einstellungen, die zur Laufzeit gelten.

use space_core::api::error::ErrorCode;
use space_core::database::UploadRow;
use space_core::ipc::{Request, Response};
use space_core::remote_ops::RemoteOp;
use space_core::{Config, Daemon};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = None;
    c.connect_timeout_secs = 3;
    c.device_name = name.into();
    c.live_connections = false;
    Daemon::start_loopback(c).await.unwrap()
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    }
}

async fn uploads(d: &Daemon) -> Vec<UploadRow> {
    match d.request(Request::UploadList).await {
        Response::Uploads { items } => items,
        other => panic!("{other:?}"),
    }
}

/// Lädt hoch und wartet, bis der Auftrag nicht mehr beim ersten Versuch ist.
async fn upload_and_wait(b: &Daemon, file: &Path, dest_dir: &str) -> UploadRow {
    let req =
        Request::Upload { device: "PC".into(), share: "Fotos".into(), dest_dir: dest_dir.into(), path: file.into() };
    let id: i64 = match b.request(req).await {
        Response::Success { message } => message.split_whitespace().nth(1).unwrap().parse().unwrap(),
        other => panic!("{other:?}"),
    };
    let t = Instant::now();
    loop {
        let row = uploads(b).await.into_iter().find(|u| u.id == id).unwrap();
        if row.state != "pending" || row.attempts > 0 {
            return row;
        }
        assert!(t.elapsed() < Duration::from_secs(20), "Upload hängt: {row:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn set(d: &Daemon, key: &str, value: Option<i64>) {
    let res = d.request(Request::SettingSet { key: key.into(), value }).await;
    assert!(matches!(res, Response::Settings { .. }), "{res:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_rights_limits_and_attacks() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("fotos");
    std::fs::create_dir_all(&share).unwrap();
    let a = start(&tmp.path().join("a"), "PC").await;
    let b = start(&tmp.path().join("b"), "Handy").await;
    let code_inv = match a.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(b.request(Request::PairJoin { code: code_inv }).await, Response::Paired { .. }));
    a.request(Request::ShareAdd { path: share.clone(), name: Some("Fotos".into()), devices: None }).await;
    let share = std::fs::canonicalize(&share).unwrap();
    let photo = tmp.path().join("IMG_0001.jpg");
    std::fs::write(&photo, b"jpeg-daten").unwrap();
    std::fs::set_permissions(&photo, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Ohne files.write: abgelehnt, endgültig (kein endloses Wiederholen)
    let row = upload_and_wait(&b, &photo, "Handy").await;
    assert_eq!(row.state, "failed", "{row:?}");
    assert!(!share.join("Handy").exists());

    // Mit files.write: angekommen, Inhalt gleich, nie ausführbar
    let res = a
        .request(Request::PairGrant {
            device: "Handy".into(),
            capability: "files.write".into(),
            folder: Some(share.clone()),
        })
        .await;
    assert!(matches!(res, Response::Success { .. }), "{res:?}");
    let row = upload_and_wait(&b, &photo, "Handy/2024").await;
    assert_eq!(row.state, "done", "{row:?}");
    assert_eq!(row.result.as_deref(), Some("Handy/2024/IMG_0001.jpg"));
    let got = share.join("Handy/2024/IMG_0001.jpg");
    assert_eq!(std::fs::read(&got).unwrap(), b"jpeg-daten");
    assert_eq!(std::fs::metadata(&got).unwrap().permissions().mode() & 0o777, 0o644);
    // Gleicher Name erneut: nie überschreiben
    std::fs::write(&photo, b"anderes foto").unwrap();
    let row = upload_and_wait(&b, &photo, "Handy/2024").await;
    assert_eq!(row.result.as_deref(), Some("Handy/2024/IMG_0001 (2).jpg"), "{row:?}");
    assert_eq!(std::fs::read(&got).unwrap(), b"jpeg-daten");

    // Einstellungen gelten sofort: gesperrt → abgelehnt; Reserve zu groß → später erneut
    set(&a, "upload_daily_gb", Some(0)).await;
    let row = upload_and_wait(&b, &photo, "Handy").await;
    assert_eq!(row.state, "failed");
    assert!(row.last_error.unwrap().contains("gesperrt"));
    set(&a, "upload_daily_gb", None).await;
    set(&a, "upload_reserve_gb", Some(100_000)).await;
    let row = upload_and_wait(&b, &photo, "Handy").await;
    assert_eq!(row.state, "pending", "vorübergehend → neuer Versuch später");
    assert!(row.last_error.as_deref().unwrap_or("").contains("Platz"), "{row:?}");
    b.request(Request::UploadCancel { id: row.id }).await;
    set(&a, "upload_reserve_gb", Some(0)).await;
    match a.request(Request::Settings).await {
        Response::Settings { items } => {
            let r = items.iter().find(|i| i.key == "upload_reserve_gb").unwrap();
            assert_eq!(r.value, 0);
        }
        other => panic!("{other:?}"),
    }

    // Versteckte Namen werden gar nicht erst eingeplant
    let hidden = tmp.path().join(".bashrc");
    std::fs::write(&hidden, b"x").unwrap();
    let res = b
        .request(Request::Upload { device: "PC".into(), share: "Fotos".into(), dest_dir: "".into(), path: hidden })
        .await;
    assert_eq!(code(&res), Some(ErrorCode::InvalidInput));
    let res = b
        .request(Request::Upload {
            device: "PC".into(),
            share: "Fotos".into(),
            dest_dir: "../x".into(),
            path: photo.clone(),
        })
        .await;
    assert_eq!(code(&res), Some(ErrorCode::InvalidInput));

    // Manipulierter Auftrag: Inhalt nicht angeboten → A kann ihn nicht holen, nichts entsteht
    let fake = RemoteOp::Import {
        share: "Fotos".into(),
        dest_dir: "".into(),
        name: "fremd.bin".into(),
        hash: iroh_blobs::Hash::new(b"nicht angeboten").to_string(),
        size: 15,
        conflict: space_core::fileops::ops::Conflict::KeepBoth,
    };
    let res = b.request(Request::RemoteOp { device: "PC".into(), op: fake }).await;
    assert!(code(&res).is_some(), "{res:?}");
    assert!(!share.join("fremd.bin").exists());

    // Gekoppeltes Gerät darf die Grenzen nicht ändern
    let dev = space_core::api::auth::Caller::Device(b.node_id());
    let res = a.request_as(&dev, Request::SettingSet { key: "upload_daily_gb".into(), value: Some(100_000) }).await;
    assert_eq!(code(&res), Some(ErrorCode::PermissionDenied));
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn camera_rule_uploads_new_files_once() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("fotos");
    let cam = tmp.path().join("DCIM/Camera");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::create_dir_all(&cam).unwrap();
    std::fs::write(cam.join("alt.jpg"), b"schon da").unwrap();
    std::fs::write(cam.join(".pending-123.jpg"), b"wird geschrieben").unwrap();
    // Älter machen als die Ruhezeit
    let old = std::time::SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options().write(true).open(cam.join("alt.jpg")).unwrap().set_modified(old).unwrap();

    let a = start(&tmp.path().join("a"), "PC").await;
    let b = start(&tmp.path().join("b"), "Handy").await;
    let c = match a.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(b.request(Request::PairJoin { code: c }).await, Response::Paired { .. }));
    a.request(Request::ShareAdd { path: share.clone(), name: Some("Fotos".into()), devices: None }).await;
    let share = std::fs::canonicalize(&share).unwrap();
    a.request(Request::PairGrant {
        device: "Handy".into(),
        capability: "files.write".into(),
        folder: Some(share.clone()),
    })
    .await;

    let res = b
        .request(Request::UploadRuleAdd {
            path: cam.clone(),
            device: "PC".into(),
            share: "Fotos".into(),
            dest_dir: "Handy".into(),
        })
        .await;
    assert!(matches!(res, Response::UploadRules { ref rules } if rules.len() == 1), "{res:?}");

    let wait_for = |p: std::path::PathBuf| async move {
        let t = Instant::now();
        while !p.exists() {
            assert!(t.elapsed() < Duration::from_secs(30), "{} nicht angekommen", p.display());
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    // Bestehende Datei wird hochgeladen, versteckte Teildatei nicht
    wait_for(share.join("Handy/alt.jpg")).await;
    // Neues Foto (auch in Unterordner) folgt automatisch
    std::fs::create_dir_all(cam.join("2024")).unwrap();
    std::fs::write(cam.join("2024/neu.jpg"), b"neues foto").unwrap();
    let t = Instant::now();
    wait_for(share.join("Handy/2024/neu.jpg")).await;
    println!("Neues Foto nach {:?} auf dem PC", t.elapsed());
    assert!(!share.join("Handy/.pending-123.jpg").exists());

    // Kein doppeltes Hochladen bei weiteren Durchläufen
    b.request(Request::UploadRetry).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let n = uploads(&b).await.len();
    assert_eq!(n, 2, "{:?}", uploads(&b).await);
    assert!(!share.join("Handy/alt (2).jpg").exists());
    a.shutdown().await;
    b.shutdown().await;
}
