//! Integrationstests für Smart-Pinning (Phase 1c) mit echten Daemons.

use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn config(root: &Path, name: &str) -> Config {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(free_port());
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    c
}

async fn start(root: &Path, name: &str) -> Daemon {
    Daemon::start_loopback(config(root, name)).await.unwrap()
}

async fn pair(host: &Daemon, guest: &Daemon) {
    let code = match host.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

async fn share(d: &Daemon, path: &Path, name: &str) {
    match d.request(Request::ShareAdd { path: path.to_path_buf(), name: Some(name.into()), devices: None }).await {
        Response::ShareAdded { .. } => {}
        other => panic!("{other:?}"),
    }
}

async fn eventually(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < Duration::from_secs(secs), "Zeitüberschreitung: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Fügt einen Pin hinzu, sobald die Freigabe im Katalog bekannt ist.
async fn pin(d: &Daemon, target: &str) -> Response {
    for _ in 0..100 {
        d.request(Request::Sync).await;
        match d.request(Request::PinAdd { target: target.into() }).await {
            Response::Error { message, .. } if message.contains("bekannt") || message.contains("bietet keine") => {
                tokio::time::sleep(Duration::from_millis(200)).await
            }
            other => return other,
        }
    }
    panic!("Freigabe für Pin nicht gefunden");
}

fn read(p: &Path) -> Option<Vec<u8>> {
    std::fs::read(p).ok()
}

async fn pin_conflicts(d: &Daemon) -> u64 {
    match d.request(Request::PinList).await {
        Response::Pins { pins, .. } => pins.iter().map(|p| p.conflicts).sum(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pinned_share_is_mirrored_and_kept_current() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    pair(&pc, &phone).await;

    let fotos = tmp.path().join("Fotos");
    std::fs::create_dir_all(fotos.join("2024")).unwrap();
    std::fs::write(fotos.join("a.jpg"), b"a1").unwrap();
    std::fs::write(fotos.join("2024/b.jpg"), b"b1").unwrap();
    share(&pc, &fotos, "Fotos").await;

    assert!(matches!(pin(&phone, "Fotos").await, Response::PinAdded { .. }));
    let mirror = phone.config().pin_dir.join("PC/Fotos");
    eventually("Spiegelung", 30, || {
        read(&mirror.join("a.jpg")).as_deref() == Some(b"a1")
            && read(&mirror.join("2024/b.jpg")).as_deref() == Some(b"b1")
    })
    .await;

    // Neue Datei, Änderung, Löschung auf dem PC -> automatisch auf dem Handy
    std::fs::write(fotos.join("neu.jpg"), b"neu").unwrap();
    eventually("neue Datei", 30, || read(&mirror.join("neu.jpg")).as_deref() == Some(b"neu")).await;
    std::fs::write(fotos.join("a.jpg"), b"a2-geaendert").unwrap();
    eventually("Änderung", 30, || read(&mirror.join("a.jpg")).as_deref() == Some(b"a2-geaendert")).await;
    std::fs::remove_file(fotos.join("2024/b.jpg")).unwrap();
    eventually("Löschung", 30, || !mirror.join("2024/b.jpg").exists()).await;
    assert!(!mirror.join("2024").exists(), "leere Ordner werden aufgeräumt");

    // Doppelter Pin wird erkannt
    assert!(matches!(phone.request(Request::PinAdd { target: "Fotos".into() }).await, Response::Error { .. }));
    pc.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn locally_modified_copies_are_never_overwritten_or_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    pair(&pc, &phone).await;
    let docs = tmp.path().join("Docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("notiz.txt"), b"original").unwrap();
    std::fs::write(docs.join("weg.txt"), b"wird geloescht").unwrap();
    share(&pc, &docs, "Docs").await;
    assert!(matches!(pin(&phone, "Docs").await, Response::PinAdded { .. }));
    let mirror = phone.config().pin_dir.join("PC/Docs");
    eventually("Spiegelung", 30, || mirror.join("notiz.txt").exists() && mirror.join("weg.txt").exists()).await;

    // Nutzer bearbeitet beide Kopien auf dem Handy (andere Größe -> sicher erkennbar)
    std::fs::write(mirror.join("notiz.txt"), b"meine lokale aenderung").unwrap();
    std::fs::write(mirror.join("weg.txt"), b"lokal behalten!").unwrap();
    // PC ändert bzw. löscht die Originale
    std::fs::write(docs.join("notiz.txt"), b"neue version vom pc").unwrap();
    std::fs::remove_file(docs.join("weg.txt")).unwrap();

    let mut conflicts = 0;
    for _ in 0..100 {
        conflicts = pin_conflicts(&phone).await;
        if conflicts >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(conflicts, 2, "beide lokalen Änderungen als Konflikt erkannt");
    assert_eq!(read(&mirror.join("notiz.txt")).unwrap(), b"meine lokale aenderung");
    assert_eq!(read(&mirror.join("weg.txt")).unwrap(), b"lokal behalten!");
    pc.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_copy_serves_when_origin_is_offline() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let server = start(&tmp.path().join("server"), "Server").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    pair(&pc, &server).await;
    pair(&pc, &phone).await;
    pair(&server, &phone).await;

    let fotos = tmp.path().join("Fotos");
    std::fs::create_dir_all(&fotos).unwrap();
    std::fs::write(fotos.join("urlaub.jpg"), b"strand und meer").unwrap();
    share(&pc, &fotos, "Fotos").await;

    // Server spiegelt die Fotos und gibt seinen Pin-Ordner frei (für alle gekoppelten Geräte)
    assert!(matches!(pin(&server, "Fotos").await, Response::PinAdded { .. }));
    let server_pins = server.config().pin_dir.clone();
    eventually("Server-Kopie", 30, || server_pins.join("PC/Fotos/urlaub.jpg").exists()).await;
    share(&server, &server_pins, "Kopien").await;

    // Handy kennt beide Quellen
    for _ in 0..100 {
        phone.request(Request::Sync).await;
        if let Response::Catalog { remote, .. } = phone.request(Request::List).await {
            if remote.iter().filter(|f| f.name == "urlaub.jpg").count() >= 2 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // PC ist aus -> Download per Pfad kommt vom Server
    pc.shutdown().await;
    let out = tmp.path().join("vom-server.jpg");
    match phone
        .request(Request::Download { target: "Fotos/urlaub.jpg".into(), output: Some(out.clone()), force: false })
        .await
    {
        Response::Downloaded { .. } => {}
        other => panic!("Server hätte liefern müssen: {other:?}"),
    }
    assert_eq!(std::fs::read(&out).unwrap(), b"strand und meer");
    server.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn planted_symlink_and_quota_are_respected() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let mut cfg = config(&tmp.path().join("phone"), "Handy");
    cfg.pin_max_bytes = 10; // nur sehr wenig Platz
    let phone = Daemon::start_loopback(cfg.clone()).await.unwrap();
    pair(&pc, &phone).await;

    let fotos = tmp.path().join("Fotos");
    std::fs::create_dir_all(&fotos).unwrap();
    std::fs::write(fotos.join("klein.txt"), b"1234").unwrap(); // 4 Byte
    std::fs::write(fotos.join("gross.txt"), b"0123456789abcdef").unwrap(); // 16 Byte
    share(&pc, &fotos, "Fotos").await;
    let docs = tmp.path().join("Docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("x.txt"), b"x").unwrap();
    share(&pc, &docs, "Docs").await;

    // Angreifer (oder Fehler) legt im Pin-Ordner einen Link nach draußen an
    let outside = tmp.path().join("draussen");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(cfg.pin_dir.join("PC")).unwrap();
    std::os::unix::fs::symlink(&outside, cfg.pin_dir.join("PC/Docs")).unwrap();

    assert!(matches!(pin(&phone, "Fotos").await, Response::PinAdded { .. }));
    assert!(matches!(pin(&phone, "Docs").await, Response::PinAdded { .. }));
    eventually("kleine Datei", 30, || cfg.pin_dir.join("PC/Fotos/klein.txt").exists()).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!cfg.pin_dir.join("PC/Fotos/gross.txt").exists(), "Kontingent überschritten");
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none(), "nichts durch den Link geschrieben");
    pc.shutdown().await;
    phone.shutdown().await;
}
