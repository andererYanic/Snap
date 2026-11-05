//! Integrationstests für Pairing und Ledger-Gossip mit echten Daemons auf 127.0.0.1.

use iroh::{endpoint::presets, Endpoint, RelayMode};
use space_core::ipc::{Request, Response};
use space_core::ledger::LEDGER_ALPN;
use space_core::pairing::PAIR_ALPN;
use space_core::{Config, Daemon, RemoteFile};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = Some(free_port());
    c.connect_timeout_secs = 2;
    c.device_name = name.to_string();
    Daemon::start_loopback(c).await.expect("Daemon startet")
}

async fn invite(d: &Daemon) -> String {
    match d.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("Invite fehlgeschlagen: {other:?}"),
    }
}

/// Koppelt `host` und `guest` über den echten Pairing-Ablauf.
async fn pair(host: &Daemon, guest: &Daemon) {
    let code = invite(host).await;
    let host2 = host.clone();
    let waiter = tokio::spawn(async move { host2.request(Request::PairInviteWait).await });
    match guest.request(Request::PairJoin { code }).await {
        Response::Paired { .. } => {}
        other => panic!("Join fehlgeschlagen: {other:?}"),
    }
    match tokio::time::timeout(Duration::from_secs(10), waiter).await.unwrap().unwrap() {
        Response::Paired { .. } => {}
        other => panic!("Host hat Kopplung nicht bestätigt: {other:?}"),
    }
}

async fn publish(d: &Daemon, dir: &Path, name: &str, content: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    match d.request(Request::Publish { path }).await {
        Response::Published { hash, .. } => hash,
        other => panic!("Publish fehlgeschlagen: {other:?}"),
    }
}

async fn sync(d: &Daemon) -> Vec<space_core::ipc::SyncResult> {
    match d.request(Request::Sync).await {
        Response::SyncReport { results } => results,
        other => panic!("Sync fehlgeschlagen: {other:?}"),
    }
}

async fn remote_files(d: &Daemon) -> Vec<RemoteFile> {
    match d.request(Request::List).await {
        Response::Catalog { remote, .. } => remote,
        other => panic!("List fehlgeschlagen: {other:?}"),
    }
}

async fn wait_for<F: Fn(&[RemoteFile]) -> bool>(d: &Daemon, cond: F) -> Vec<RemoteFile> {
    for _ in 0..100 {
        let files = remote_files(d).await;
        if cond(&files) {
            return files;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("Bedingung nicht erreicht");
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_sync_download_by_name_and_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let pc = start(&tmp.path().join("pc"), "Arch-Desktop").await;
    let phone = start(&tmp.path().join("phone"), "Pixel").await;
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();

    let hash = publish(&pc, &files, "urlaub.jpg", b"strand und meer").await;
    pair(&pc, &phone).await;

    // Nach dem Koppeln gleicht das Handy automatisch ab
    let remote = wait_for(&phone, |f| !f.is_empty()).await;
    assert_eq!(remote.len(), 1);
    assert_eq!(remote[0].name, "urlaub.jpg");
    assert_eq!(remote[0].hash, hash);
    assert_eq!(remote[0].origin_name.as_deref(), Some("Arch-Desktop"));
    assert!(remote.iter().all(|f| !f.name.contains('/')), "keine Pfade im Ledger");

    // Download per Dateiname, echter Name im Standardordner
    let path = match phone.request(Request::Download { target: "urlaub.jpg".into(), output: None, force: false }).await {
        Response::Downloaded { path } => path,
        other => panic!("{other:?}"),
    };
    assert_eq!(path.file_name().unwrap(), "urlaub.jpg");
    assert_eq!(std::fs::read(&path).unwrap(), b"strand und meer");

    // Rück-Abgleich: Das Handy gibt etwas frei, der PC gleicht ab und das Handy
    // erfährt dabei gleichzeitig (über dieselbe Verbindung) nichts Neues – der PC aber schon.
    publish(&phone, &files, "notiz.txt", b"vom handy").await;
    let results = sync(&pc).await;
    assert!(results.iter().all(|r| r.ok), "{results:?}");
    let pc_remote = wait_for(&pc, |f| f.iter().any(|x| x.name == "notiz.txt")).await;
    assert_eq!(pc_remote.len(), 1);

    // Löschen auf dem PC wird beim nächsten Abgleich übernommen
    match pc.request(Request::Delete { hash: Some(hash[..10].into()), path: None }).await {
        Response::Success { .. } => {}
        other => panic!("{other:?}"),
    }
    sync(&phone).await;
    assert!(remote_files(&phone).await.iter().all(|f| f.name != "urlaub.jpg"));

    pc.shutdown().await;
    phone.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reverse_sync_on_same_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A").await;
    let b = start(&tmp.path().join("b"), "B").await;
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    pair(&a, &b).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    publish(&a, &files, "von_a.txt", b"a").await;
    publish(&b, &files, "von_b.txt", b"b").await;

    // Nur A gleicht ab – B bekommt A's Datei trotzdem über den Rück-Abgleich.
    sync(&a).await;
    assert!(remote_files(&a).await.iter().any(|f| f.name == "von_b.txt"));
    wait_for(&b, |f| f.iter().any(|x| x.name == "von_a.txt")).await;

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn entries_are_relayed_through_hub() {
    // Handy und PC sind nur mit dem Server gekoppelt.
    let tmp = tempfile::tempdir().unwrap();
    let server = start(&tmp.path().join("server"), "Server").await;
    let pc = start(&tmp.path().join("pc"), "PC").await;
    let phone = start(&tmp.path().join("phone"), "Handy").await;
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();

    pair(&server, &pc).await;
    pair(&server, &phone).await;
    publish(&pc, &files, "bericht.pdf", b"inhalt vom pc").await;

    sync(&server).await; // Server holt sich die Liste des PCs
    sync(&phone).await; // Handy holt sich die Liste vom Server – inkl. PC-Einträgen
    let remote = remote_files(&phone).await;
    let entry = remote.iter().find(|f| f.name == "bericht.pdf").expect("über den Server weitergereicht");
    assert_eq!(entry.origin, pc.node_id(), "Ursprung bleibt der PC");
    assert!(entry.origin_name.is_none(), "PC ist mit dem Handy nicht direkt gekoppelt");

    // Das Handy lädt direkt vom PC (Ticket-Prinzip), der PC muss das Handy nicht kennen.
    // Ohne Relais braucht es dafür Adress-Hinweise – die gibt es nur von direkt
    // gekoppelten Geräten. Daher landet der Auftrag hier in der Warteschlange.
    let res = phone.request(Request::Download { target: "bericht.pdf".into(), output: None, force: false }).await;
    assert!(matches!(res, Response::Downloaded { .. } | Response::Queued { .. }), "{res:?}");

    for d in [server, pc, phone] {
        d.shutdown().await;
    }
}

/// Fremder Client ohne Kopplung.
async fn stranger() -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// Versucht als Fremder das Ledger zu lesen. Gibt (gelesene Bytes, Schließgrund) zurück.
async fn stranger_reads_ledger(target: &Daemon) -> (usize, String) {
    let ep = stranger().await;
    let conn = ep.connect(target.network().endpoint().addr(), LEDGER_ALPN).await.expect("Verbindung");
    let mut total = 0;
    if let Ok((mut send, mut recv)) = conn.open_bi().await {
        let req = br#"{"version":1,"have":{},"addrs":[]}"#;
        let _ = send.write_u32(req.len() as u32).await;
        let _ = send.write_all(req).await;
        let _ = send.finish();
        let mut buf = vec![0u8; 4096];
        if let Ok(Ok(Some(n))) = tokio::time::timeout(Duration::from_secs(3), recv.read(&mut buf)).await {
            total = n;
        }
    }
    let reason = tokio::time::timeout(Duration::from_secs(3), conn.closed())
        .await
        .map(|e| e.to_string())
        .unwrap_or_default();
    ep.close().await;
    (total, reason)
}

#[tokio::test(flavor = "multi_thread")]
async fn security_unpaired_and_revoked_devices_cannot_read_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "A").await;
    let b = start(&tmp.path().join("b"), "B").await;
    let files = tmp.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    publish(&a, &files, "geheim.txt", b"x").await;

    let (bytes, reason) = stranger_reads_ledger(&a).await;
    assert_eq!(bytes, 0, "Fremde dürfen das Ledger nicht lesen");
    assert!(reason.contains("not paired"), "Abweisung aus dem richtigen Grund: {reason}");

    pair(&a, &b).await;
    wait_for(&b, |f| !f.is_empty()).await;

    // A entkoppelt B -> B bekommt keine neuen Einträge mehr
    match a.request(Request::PairRemove { device: "B".into() }).await {
        Response::Success { .. } => {}
        other => panic!("{other:?}"),
    }
    publish(&a, &files, "neu.txt", b"y").await;
    let results = sync(&b).await;
    assert_eq!(results.len(), 1);
    assert!(!results[0].ok && results[0].new_entries == 0, "A muss B abweisen: {results:?}");
    assert!(remote_files(&b).await.iter().all(|f| f.name != "neu.txt"));

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn security_invite_codes() {
    let tmp = tempfile::tempdir().unwrap();
    let host = start(&tmp.path().join("host"), "Host").await;
    let g1 = start(&tmp.path().join("g1"), "G1").await;
    let g2 = start(&tmp.path().join("g2"), "G2").await;

    // Ohne aktive Einladung: abgelehnt
    let code = invite(&host).await;
    let replaced = code.clone();
    let code = invite(&host).await; // ersetzt die vorherige
    let err = |r: Response| match r {
        Response::Error { message } => message,
        other => panic!("erwartet Fehler, bekam {other:?}"),
    };
    let msg = err(g1.request(Request::PairJoin { code: replaced }).await);
    assert!(msg.contains("Ungültiger Einladungscode"), "ersetzter Code: {msg}");

    // Manipulierter Code (anderes Token, gleiche Node-ID) wird abgelehnt
    let mut chars: Vec<char> = code.chars().collect();
    let idx = 6 + 60; // innerhalb des Tokens
    chars[idx] = if chars[idx] == 'a' { 'b' } else { 'a' };
    let tampered: String = chars.into_iter().collect();
    let msg = err(g1.request(Request::PairJoin { code: tampered }).await);
    assert!(msg.contains("Ungültiger Einladungscode"), "manipulierter Code: {msg}");

    // Der echte Code funktioniert danach noch (falsche Versuche verbrauchen ihn nicht) …
    assert!(matches!(g1.request(Request::PairJoin { code: code.clone() }).await, Response::Paired { .. }));
    // … aber nur einmal
    let msg = err(g2.request(Request::PairJoin { code: code.clone() }).await);
    assert!(msg.contains("Keine aktive Einladung"), "Zweitnutzung: {msg}");

    // Eigener Code
    let own = invite(&g2).await;
    let msg = err(g2.request(Request::PairJoin { code: own }).await);
    assert!(msg.contains("dieses Geräts"), "eigener Code: {msg}");

    // Fremder ohne Token über das Pairing-Protokoll
    let ep = stranger().await;
    let conn = ep.connect(host.network().endpoint().addr(), PAIR_ALPN).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    let req = br#"{"version":1,"token":"00","name":"evil","addrs":[]}"#;
    send.write_u32(req.len() as u32).await.unwrap();
    send.write_all(req).await.unwrap();
    send.finish().unwrap();
    let buf = match tokio::time::timeout(Duration::from_secs(3), recv.read_to_end(64 * 1024)).await {
        Ok(Ok(b)) => b,
        _ => Vec::new(),
    };
    assert!(String::from_utf8_lossy(&buf).contains("\"accepted\":false"));
    ep.close().await;

    match host.request(Request::PairList).await {
        Response::Devices { devices } => {
            assert_eq!(devices.len(), 1);
            assert_eq!(devices[0].name, "G1");
        }
        other => panic!("{other:?}"),
    }
    for d in [host, g1, g2] {
        d.shutdown().await;
    }
}
