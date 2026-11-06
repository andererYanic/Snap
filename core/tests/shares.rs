//! Integrationstests für Ordner-Freigaben (Phase 1a) mit echten Daemons.

use iroh::{endpoint::presets, Endpoint, RelayMode};
use iroh_blobs::{store::mem::MemStore, Hash};
use space_core::api::error::ErrorCode;
use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon, RemoteFile};
use std::path::{Path, PathBuf};
use std::time::Duration;

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    // Port vom Betriebssystem (0): kein Wettlauf mit parallel laufenden Tests.
    c.bind_port = None;
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    Daemon::start_loopback(c).await.unwrap()
}

async fn pair(host: &Daemon, guest: &Daemon) {
    let code = match host.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

async fn remote(d: &Daemon) -> Vec<RemoteFile> {
    match d.request(Request::List).await {
        Response::Catalog { remote, .. } => remote,
        other => panic!("{other:?}"),
    }
}

/// Gleicht ab, bis `cond` erfüllt ist (Watcher + Abgleich brauchen etwas Zeit).
async fn sync_until(d: &Daemon, cond: impl Fn(&[RemoteFile]) -> bool) -> Vec<RemoteFile> {
    for _ in 0..60 {
        d.request(Request::Sync).await;
        let files = remote(d).await;
        if cond(&files) {
            return files;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("Bedingung nicht erreicht: {:?}", remote(d).await);
}

fn has(files: &[RemoteFile], path: &str) -> bool {
    files.iter().any(|f| {
        f.share.is_some() && format!("{}/{}", f.share.as_deref().unwrap(), f.rel_path.as_deref().unwrap()) == path
    })
}

async fn download(d: &Daemon, target: &str, out: PathBuf) -> Response {
    d.request(Request::Download { target: target.into(), output: Some(out), force: true }).await
}

async fn add_share(d: &Daemon, path: &Path, devices: Option<Vec<String>>) {
    match d.request(Request::ShareAdd { path: path.to_path_buf(), name: Some("Fotos".into()), devices }).await {
        Response::ShareAdded { .. } => {}
        other => panic!("{other:?}"),
    }
}

/// Versucht als Fremder (ohne Kopplung), einen Hash direkt zu holen.
async fn stranger_fetch(target: &Daemon, hash: Hash) -> bool {
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let store = MemStore::new();
    let conn = ep.connect(target.network().endpoint().addr(), iroh_blobs::ALPN).await.unwrap();
    let ok = store.remote().fetch(conn, hash).await.is_ok() && store.has(hash).await.unwrap_or(false);
    ep.close().await;
    ok
}

#[tokio::test(flavor = "multi_thread")]
async fn share_sync_watch_and_download_by_path() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    pair(&pc, &phone).await;

    let fotos = tmp.path().join("Fotos");
    std::fs::create_dir_all(fotos.join("2024")).unwrap();
    std::fs::write(fotos.join("2024/strand.jpg"), b"strand").unwrap();
    std::fs::write(fotos.join("notiz.txt"), b"notiz").unwrap();
    add_share(&pc, &fotos, None).await;

    let files = sync_until(&phone, |f| has(f, "Fotos/2024/strand.jpg") && has(f, "Fotos/notiz.txt")).await;
    let entry = files.iter().find(|f| f.rel_path.as_deref() == Some("2024/strand.jpg")).unwrap();
    assert_eq!(entry.name, "strand.jpg");
    assert!(!format!("{files:?}").contains(fotos.to_str().unwrap()), "keine absoluten Pfade im Ledger");

    // Download per Freigabe-Pfad
    let out = tmp.path().join("geladen.jpg");
    assert!(matches!(download(&phone, "Fotos/2024/strand.jpg", out.clone()).await, Response::Downloaded { .. }));
    assert_eq!(std::fs::read(&out).unwrap(), b"strand");

    // Watcher: neue Datei, Änderung, Löschung
    std::fs::write(fotos.join("2024/neu.jpg"), b"neu").unwrap();
    sync_until(&phone, |f| has(f, "Fotos/2024/neu.jpg")).await;
    std::fs::write(fotos.join("notiz.txt"), b"notiz, geaendert").unwrap();
    let before = entry_hash(&phone, "notiz.txt").await;
    let mut changed = false;
    for _ in 0..60 {
        phone.request(Request::Sync).await;
        if entry_hash(&phone, "notiz.txt").await != before {
            changed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(changed, "Änderung wurde nicht erkannt");
    std::fs::remove_file(fotos.join("2024/strand.jpg")).unwrap();
    sync_until(&phone, |f| !has(f, "Fotos/2024/strand.jpg")).await;

    // Freigabe beenden -> verschwindet beim Handy
    assert!(matches!(pc.request(Request::ShareRemove { name: "Fotos".into() }).await, Response::Success { .. }));
    sync_until(&phone, |f| f.iter().all(|x| x.share.is_none())).await;
    pc.shutdown().await;
    phone.shutdown().await;
}

async fn entry_hash(d: &Daemon, rel: &str) -> String {
    remote(d).await.into_iter().find(|f| f.rel_path.as_deref() == Some(rel)).map(|f| f.hash).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn share_access_requires_files_read() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    let laptop = start(&tmp.path().join("laptop"), "Laptop").await;
    pair(&pc, &phone).await;
    pair(&pc, &laptop).await;

    let fotos = tmp.path().join("Fotos");
    std::fs::create_dir_all(&fotos).unwrap();
    std::fs::write(fotos.join("privat.jpg"), b"privat").unwrap();
    // Geheimnis außerhalb + Symlink in der Freigabe
    let secret_dir = tmp.path().join("geheim");
    std::fs::create_dir_all(&secret_dir).unwrap();
    std::fs::write(secret_dir.join("id_rsa"), b"PRIVATE KEY").unwrap();
    std::os::unix::fs::symlink(&secret_dir, fotos.join("link-ordner")).unwrap();
    std::os::unix::fs::symlink(secret_dir.join("id_rsa"), fotos.join("link-datei")).unwrap();

    // Nur das Handy bekommt Zugriff
    add_share(&pc, &fotos, Some(vec!["Handy".into()])).await;
    let files = sync_until(&laptop, |f| has(f, "Fotos/privat.jpg")).await;
    assert!(
        !files.iter().any(|f| f.name.contains("id_rsa") || f.name.starts_with("link-")),
        "Symlinks werden nicht geteilt"
    );
    let hash: Hash = files.iter().find(|f| f.name == "privat.jpg").unwrap().hash.parse().unwrap();

    // Laptop sieht die Liste (catalog.read), darf die Datei aber nicht laden
    match download(&laptop, "Fotos/privat.jpg", tmp.path().join("l.jpg")).await {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::Rejected),
        other => panic!("Laptop ohne files.read darf nicht laden: {other:?}"),
    }
    // Fremder kennt den Hash (z. B. aus einem gestohlenen Ledger) -> trotzdem nichts
    assert!(!stranger_fetch(&pc, hash).await, "Share-Datei an Fremden ausgeliefert");
    // Geheimnis außerhalb ist nicht einmal importiert
    let secret_hash = Hash::new(b"PRIVATE KEY");
    assert!(!stranger_fetch(&pc, secret_hash).await);

    // Handy darf
    sync_until(&phone, |f| has(f, "Fotos/privat.jpg")).await;
    assert!(matches!(
        download(&phone, "Fotos/privat.jpg", tmp.path().join("p.jpg")).await,
        Response::Downloaded { .. }
    ));

    // Recht entziehen -> Handy darf nicht mehr (neue Datei, damit nichts aus dem Cache kommt)
    std::fs::write(fotos.join("neu.jpg"), b"neu nach entzug").unwrap();
    sync_until(&phone, |f| has(f, "Fotos/neu.jpg")).await;
    let canon = std::fs::canonicalize(&fotos).unwrap();
    assert!(matches!(
        pc.request(Request::PairRevoke {
            device: "Handy".into(),
            capability: "files.read".into(),
            folder: Some(canon)
        })
        .await,
        Response::Success { .. }
    ));
    match download(&phone, "Fotos/neu.jpg", tmp.path().join("n.jpg")).await {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::Rejected),
        other => panic!("nach Entzug darf nicht geladen werden: {other:?}"),
    }

    // Abgelehnte Zugriffe stehen im Audit-Log
    match pc.request(Request::Audit { limit: 100 }).await {
        Response::Audit { entries } => assert!(entries.iter().any(|e| !e.allowed && e.operation == "blob.get")),
        other => panic!("{other:?}"),
    }
    for d in [pc, phone, laptop] {
        d.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn later_paired_devices_get_access_to_auto_shares() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let fotos = tmp.path().join("Fotos");
    std::fs::create_dir_all(&fotos).unwrap();
    std::fs::write(fotos.join("a.jpg"), b"a").unwrap();
    add_share(&pc, &fotos, None).await; // noch kein Gerät gekoppelt

    let phone = start(&tmp.path().join("phone"), "Handy").await;
    pair(&pc, &phone).await;
    sync_until(&phone, |f| has(f, "Fotos/a.jpg")).await;
    assert!(matches!(download(&phone, "Fotos/a.jpg", tmp.path().join("a.jpg")).await, Response::Downloaded { .. }));

    // Ungültige Freigaben
    for bad in [PathBuf::from("/"), PathBuf::from("/etc"), pc.config().data_dir.clone(), fotos.join("a.jpg")] {
        match pc.request(Request::ShareAdd { path: bad.clone(), name: Some("X".into()), devices: None }).await {
            Response::Error { .. } => {}
            other => panic!("{bad:?} darf nicht freigegeben werden: {other:?}"),
        }
    }
    // Überschneidung
    std::fs::create_dir_all(fotos.join("unter")).unwrap();
    match pc.request(Request::ShareAdd { path: fotos.join("unter"), name: Some("Unter".into()), devices: None }).await {
        Response::Error { code, .. } => assert_eq!(code, ErrorCode::AlreadyExists),
        other => panic!("{other:?}"),
    }
    pc.shutdown().await;
    phone.shutdown().await;
}
