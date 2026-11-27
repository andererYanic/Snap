//! Zwei-Wege-Sync (Pins mit `two_way`) mit echten Daemons: Ändern, Neu, Löschen in beide
//! Richtungen, „Neuestes gewinnt“, Papierkorb statt Löschen, Massenschutz und fehlende Rechte.

use space_core::events::Event;
use space_core::ipc::{PinInfo, Request, Response};
use space_core::{Config, Daemon};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

fn config(root: &Path, name: &str) -> Config {
    let mut c = Config::portable(root);
    c.use_relay = false;
    // Port vom Betriebssystem (0): kein Wettlauf mit parallel laufenden Tests.
    c.bind_port = None;
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
    host.confirm_next_pairing();
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
async fn pin(d: &Daemon, target: &str, two_way: bool) -> Response {
    for _ in 0..100 {
        d.request(Request::Sync).await;
        match d.request(Request::PinAdd { target: target.into(), two_way }).await {
            Response::Error { message, .. } if message.contains("bekannt") || message.contains("bietet keine") => {
                tokio::time::sleep(Duration::from_millis(200)).await
            }
            other => return other,
        }
    }
    panic!("Freigabe für Pin nicht gefunden");
}

async fn grant(on: &Daemon, device: &str, cap: &str, folder: &Path) {
    let r = on
        .request(Request::PairGrant {
            device: device.into(),
            capability: cap.into(),
            folder: Some(folder.to_path_buf()),
        })
        .await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
}

async fn pins(d: &Daemon) -> Vec<PinInfo> {
    match d.request(Request::PinList).await {
        Response::Pins { pins, .. } => pins,
        other => panic!("{other:?}"),
    }
}

/// Datei schreiben und Zeitstempel setzen (in Sekunden relativ zu jetzt).
fn write_at(p: &Path, data: &[u8], secs_ago: u64) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, data).unwrap();
    let t = SystemTime::now() - Duration::from_secs(secs_ago);
    std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
}

/// Inhalte im Papierkorb eines Daemons (Dateinamen).
async fn trash_names(d: &Daemon) -> Vec<String> {
    match d.request(Request::TrashList).await {
        Response::Trash { entries } => entries.into_iter().map(|e| e.name).collect(),
        other => panic!("{other:?}"),
    }
}

fn mirror(phone: &Daemon) -> PathBuf {
    phone.config().pin_dir.join("PC").join("Docs")
}

async fn setup(tmp: &Path, rights: bool) -> (Daemon, Daemon, PathBuf) {
    let pc = start(&tmp.join("pc"), "PC").await;
    let phone = start(&tmp.join("phone"), "Handy").await;
    pair(&pc, &phone).await;
    let docs = tmp.join("Docs");
    for f in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write_at(&docs.join(f), format!("{f} vom pc").as_bytes(), 600);
    }
    share(&pc, &docs, "Docs").await;
    if rights {
        grant(&pc, "Handy", "files.write", &docs).await;
        grant(&pc, "Handy", "files.delete", &docs).await;
    }
    assert!(matches!(pin(&phone, "Docs", true).await, Response::PinAdded { .. }));
    let m = mirror(&phone);
    eventually("Spiegel", 30, || ["a.txt", "b.txt", "c.txt", "d.txt"].iter().all(|f| m.join(f).exists())).await;
    // Spiegel-Stand liegt vor
    let t0 = Instant::now();
    while pins(&phone).await[0].files_ok < 4 {
        assert!(t0.elapsed() < Duration::from_secs(20), "Spiegel nicht vollständig");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    (pc, phone, docs)
}

#[tokio::test(flavor = "multi_thread")]
async fn aenderungen_gehen_in_beide_richtungen() {
    let tmp = tempfile::tempdir().unwrap();
    let (pc, phone, docs) = setup(tmp.path(), true).await;
    let m = mirror(&phone);
    assert!(pins(&phone).await[0].two_way);

    // Handy: ändern, neu anlegen, löschen
    write_at(&m.join("a.txt"), b"a.txt vom handy, laenger", 10);
    write_at(&m.join("neu/notiz.txt"), b"neu vom handy", 10);
    std::fs::remove_file(m.join("b.txt")).unwrap();
    phone.request(Request::Sync).await;

    eventually("Änderung beim PC", 40, || {
        std::fs::read(docs.join("a.txt")).ok().as_deref() == Some(b"a.txt vom handy, laenger")
    })
    .await;
    eventually("Neue Datei beim PC", 40, || {
        std::fs::read(docs.join("neu/notiz.txt")).ok().as_deref() == Some(b"neu vom handy")
    })
    .await;
    eventually("Löschung beim PC", 40, || !docs.join("b.txt").exists()).await;
    // Beim PC landet nichts endgültig im Nirgendwo: alte Fassung und gelöschte Datei im Papierkorb
    let t = trash_names(&pc).await;
    assert!(t.iter().any(|n| n == "a.txt") && t.iter().any(|n| n == "b.txt"), "{t:?}");

    // PC: löscht c.txt → beim Handy in den Papierkorb (nicht endgültig)
    std::fs::remove_file(docs.join("c.txt")).unwrap();
    pc.request(Request::ShareRescan { name: Some("Docs".into()) }).await;
    eventually("Löschung beim Handy", 40, || !m.join("c.txt").exists()).await;
    let t0 = Instant::now();
    while !trash_names(&phone).await.iter().any(|n| n == "c.txt") {
        assert!(t0.elapsed() < Duration::from_secs(10), "c.txt nicht im Papierkorb des Handys");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Beide ändern d.txt, während das Handy aus ist: der PC ist neuer → seine Fassung gewinnt, die
    // des Handys geht in dessen Papierkorb (auch wenn das Handy beim Start noch den alten Katalog hat)
    phone.shutdown().await;
    drop(phone);
    write_at(&m.join("d.txt"), b"handy alt", 300);
    write_at(&docs.join("d.txt"), b"pc ist neuer, laenger", 0);
    pc.request(Request::ShareRescan { name: Some("Docs".into()) }).await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    phone.request(Request::Sync).await;
    eventually("Neuere Fassung beim Handy", 40, || {
        std::fs::read(m.join("d.txt")).ok().as_deref() == Some(b"pc ist neuer, laenger")
    })
    .await;
    let t0 = Instant::now();
    while !trash_names(&phone).await.iter().any(|n| n == "d.txt") {
        assert!(t0.elapsed() < Duration::from_secs(10), "ältere Fassung nicht im Papierkorb");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Die Änderungen kommen nicht als Pingpong zurück
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(std::fs::read(docs.join("a.txt")).unwrap(), b"a.txt vom handy, laenger");
    assert_eq!(std::fs::read(docs.join("d.txt")).unwrap(), b"pc ist neuer, laenger");
    pc.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ohne_rechte_bleibt_der_ursprung_unberuehrt() {
    let tmp = tempfile::tempdir().unwrap();
    let (pc, phone, docs) = setup(tmp.path(), false).await;
    let m = mirror(&phone);
    write_at(&m.join("a.txt"), b"versuch ohne recht", 10);
    std::fs::remove_file(m.join("b.txt")).unwrap();
    phone.request(Request::Sync).await;
    // Als Konflikt sichtbar, beim PC unverändert
    let t0 = Instant::now();
    while pins(&phone).await[0].conflicts == 0 {
        assert!(t0.elapsed() < Duration::from_secs(30), "kein Konflikt gemeldet");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(std::fs::read(docs.join("a.txt")).unwrap(), b"a.txt vom pc");
    assert!(docs.join("b.txt").exists());
    assert_eq!(std::fs::read(m.join("a.txt")).unwrap(), b"versuch ohne recht", "lokale Arbeit bleibt");
    pc.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn viele_loeschungen_werden_angehalten() {
    let tmp = tempfile::tempdir().unwrap();
    let (pc, phone, docs) = setup(tmp.path(), true).await;
    let m = mirror(&phone);
    for i in 0..25 {
        write_at(&docs.join(format!("viel/{i}.txt")), format!("{i}").as_bytes(), 600);
    }
    pc.request(Request::ShareRescan { name: Some("Docs".into()) }).await;
    eventually("viele Dateien gespiegelt", 40, || (0..25).all(|i| m.join(format!("viel/{i}.txt")).exists())).await;
    let t0 = Instant::now();
    while pins(&phone).await[0].files_ok < 29 {
        assert!(t0.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let mut sub = phone.subscribe();
    std::fs::remove_dir_all(docs.join("viel")).unwrap();
    pc.request(Request::ShareRescan { name: Some("Docs".into()) }).await;
    let held = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            if let Some(Event::SyncHeld { deletes, pin_id, .. }) = sub.next().await {
                return (pin_id, deletes);
            }
        }
    })
    .await
    .expect("Massenlöschung angehalten");
    assert_eq!(held.1, 25);
    assert!((0..25).all(|i| m.join(format!("viel/{i}.txt")).exists()), "noch nichts gelöscht");
    assert_eq!(pins(&phone).await[0].held_deletes, 25);
    // Bestätigen → in den Papierkorb
    let r = phone.request(Request::PinAllowDeletes { id: held.0, allow: true }).await;
    assert!(matches!(r, Response::Success { .. }), "{r:?}");
    eventually("gelöscht", 30, || (0..25).all(|i| !m.join(format!("viel/{i}.txt")).exists())).await;
    assert!(trash_names(&phone).await.len() >= 25);
    pc.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_als_zentrale_reicht_aenderungen_weiter() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let server = start(&tmp.path().join("server"), "Server").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    pair(&pc, &server).await;
    pair(&server, &phone).await;
    let docs = tmp.path().join("Docs");
    write_at(&docs.join("a.txt"), b"vom pc", 600);
    share(&pc, &docs, "Docs").await;
    grant(&pc, "Server", "files.write", &docs).await;
    grant(&pc, "Server", "files.delete", &docs).await;

    // Server spiegelt den PC in beide Richtungen und gibt seine Kopien dem Handy frei
    assert!(matches!(pin(&server, "Docs", true).await, Response::PinAdded { .. }));
    let pins_dir = server.config().pin_dir.clone();
    eventually("Kopie auf dem Server", 30, || pins_dir.join("PC/Docs/a.txt").exists()).await;
    share(&server, &pins_dir, "Kopien").await;
    grant(&server, "Handy", "files.write", &pins_dir).await;
    grant(&server, "Handy", "files.delete", &pins_dir).await;
    assert!(matches!(pin(&phone, "Kopien/PC/Docs", true).await, Response::PinAdded { .. }));
    let m = phone.config().pin_dir.join("Server/Kopien/PC/Docs");
    eventually("Kopie auf dem Handy", 30, || m.join("a.txt").exists()).await;
    let t0 = Instant::now();
    while pins(&phone).await[0].files_ok < 1 {
        assert!(t0.elapsed() < Duration::from_secs(20));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Handy ändert → über den Server zum PC
    write_at(&m.join("a.txt"), b"vom handy, ueber den server", 5);
    phone.request(Request::Sync).await;
    eventually("Änderung beim PC", 60, || {
        std::fs::read(docs.join("a.txt")).ok().as_deref() == Some(b"vom handy, ueber den server")
    })
    .await;
    pc.shutdown().await;
    server.shutdown().await;
    phone.shutdown().await;
}
