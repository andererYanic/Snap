//! Snap Drop: echte Verbindungen auf 127.0.0.1 und Angriffstests (fremde, gesperrte und
//! manipulierte Gegenstellen).

use iroh::endpoint::{presets, Connection, VarInt};
use iroh::{Endpoint, EndpointAddr, RelayMode, TransportAddr};
use space_core::database::DatabaseManager;
use space_core::drop::identity::{self, DropIdentity};
use space_core::drop::wire::{self, Card, ConnectInfo, DeviceKind, Offer, OfferFile, Probe, Proof, Reply};
use space_core::drop::{DropOptions, DropService, Mode};
use space_core::events::{Event, Subscription};
use space_core::ipc::{read_frame_max, write_frame_max};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

struct Node {
    svc: Arc<DropService>,
    events: Subscription,
    inbox: PathBuf,
}

fn node(root: &Path, name: &str) -> Node {
    let db = Arc::new(DatabaseManager::open_in_memory().unwrap());
    let events = db.events().subscribe();
    let inbox = root.join(name).join("Snap Drop");
    let opts = DropOptions {
        dir: inbox.clone(),
        identity_path: root.join(name).join("drop.key"),
        name: name.into(),
        kind: DeviceKind::Phone,
        use_relay: false,
        relay_urls: vec![],
        relay_fallback_n0: false,
        loopback_only: true,
        mdns: false,
        reserve_bytes: 0,
        can_join_hotspot: false,
    };
    Node { svc: DropService::with_identity(opts, db, DropIdentity::ephemeral()), events, inbox }
}

async fn next_event(sub: &mut Subscription, pred: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let e = sub.next().await.expect("Ereignisstrom offen");
            if pred(&e) {
                return e;
            }
        }
    })
    .await
    .expect("Ereignis kam nicht")
}

fn code_of(n: &Node) -> String {
    n.svc.status().unwrap().code.expect("sichtbar mit Code")
}

/// Wartet auf das Angebot beim Empfänger und beantwortet es.
async fn answer(n: &mut Node, accept: bool, block: bool) -> u64 {
    let Event::DropOffer { id, .. } = next_event(&mut n.events, |e| matches!(e, Event::DropOffer { .. })).await else {
        unreachable!()
    };
    n.svc.respond(id, accept, block).unwrap();
    id
}

fn write(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn inbox_files(n: &Node) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(&n.inbox)
        .map(|r| r.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[tokio::test]
async fn senden_annehmen_kontakt_und_kontaktmodus() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    let c = node(tmp.path(), "Carla");

    // Unsichtbar: kein Code, keine Sitzung.
    assert_eq!(b.svc.status().unwrap().mode, Mode::Off);
    assert!(b.svc.status().unwrap().code.is_none());

    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let st = b.svc.status().unwrap();
    assert!(st.until.is_some());
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    assert_eq!(peer.name.as_deref(), Some("Ben"));
    assert_eq!(peer.contact, None);

    let src = tmp.path().join("src");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let f1 = write(&src, "urlaub.bin", &data);
    let f2 = write(&src, "notiz.txt", b"hallo");
    let send = {
        let svc = a.svc.clone();
        let id = peer.id.clone();
        tokio::spawn(async move { svc.send(&id, vec![f1, f2]).await.unwrap() })
    };
    let offer_id = answer(&mut b, true, false).await;
    let res = send.await.unwrap();
    assert!(res.ok, "{:?}", res.error);
    let fin = next_event(&mut b.events, |e| matches!(e, Event::DropFinished { .. })).await;
    let Event::DropFinished { ok, files, can_save_contact, .. } = fin else { unreachable!() };
    assert!(ok && can_save_contact);
    assert_eq!(files.len(), 2);
    assert_eq!(std::fs::read(b.inbox.join("urlaub.bin")).unwrap(), data);
    assert_eq!(inbox_files(&b), vec!["notiz.txt", "urlaub.bin"], "keine Teil-Dateien übrig");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(b.inbox.join("notiz.txt")).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0, "nie ausführbar");
    }

    // Beide speichern einander als Kontakt.
    b.svc.save_contact(offer_id).unwrap();
    a.svc.save_contact(res.id).unwrap();
    assert!(a.svc.save_contact(res.id).is_err(), "nur einmal");
    assert_eq!(a.svc.contacts().unwrap()[0].name, "Ben");

    // Kontakte-Modus: Anna sieht Ben als Kontakt, Carla sieht nichts.
    b.svc.set_visibility(Mode::Contacts).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    assert_eq!(peer.contact.as_deref(), Some("Ben"));
    let err = c.svc.add_code(&code_of(&b)).await.unwrap_err();
    assert!(format!("{err:#}").contains("nur von Kontakten"));

    // Erneut senden: als Kontakt erkannt.
    let f3 = write(&src, "noch.txt", b"x");
    let send = {
        let svc = a.svc.clone();
        tokio::spawn(async move { svc.send(&peer.id, vec![f3]).await.unwrap() })
    };
    let ev = next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await;
    let Event::DropOffer { id, contact, code, .. } = ev else { unreachable!() };
    assert_eq!(contact.as_deref(), Some("Anna"));
    b.svc.respond(id, true, false).unwrap();
    let res = send.await.unwrap();
    assert!(res.ok);
    assert_eq!(res.code.as_deref(), Some(code.as_str()), "Prüfcode auf beiden Seiten gleich");

    // Aus: Code weg, Verbindung scheitert.
    let old = code_of(&b);
    b.svc.set_visibility(Mode::Off).await.unwrap();
    assert!(b.svc.status().unwrap().code.is_none());
    assert!(a.svc.add_code(&old).await.is_err());
}

#[tokio::test]
async fn ablehnen_sperren_und_auto_aus() {
    let tmp = tempfile::tempdir().unwrap();
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let src = tmp.path().join("src");
    let f = write(&src, "a.txt", b"spam");

    // Gesperrter Absender: zweites Angebot kommt gar nicht erst an.
    let spammer = node(tmp.path(), "Spam");
    let peer = spammer.svc.add_code(&code_of(&b)).await.unwrap();
    let send = {
        let (svc, f, id) = (spammer.svc.clone(), f.clone(), peer.id.clone());
        tokio::spawn(async move { svc.send(&id, vec![f]).await.unwrap() })
    };
    answer(&mut b, false, true).await;
    let r = send.await.unwrap();
    assert!(!r.ok && r.error.unwrap().contains("Abgelehnt"));
    let r = spammer.svc.send(&peer.id, vec![f.clone()]).await.unwrap();
    assert!(!r.ok);
    assert!(b.svc.pending_offers().is_empty());
    assert!(inbox_files(&b).is_empty());

    // Drei abgelehnte Fremde: „Alle“ schaltet ab.
    for i in 0..2 {
        let s = node(tmp.path(), &format!("Fremd{i}"));
        let p = s.svc.add_code(&code_of(&b)).await.unwrap();
        let send = {
            let (svc, f) = (s.svc.clone(), f.clone());
            tokio::spawn(async move { svc.send(&p.id, vec![f]).await.unwrap() })
        };
        answer(&mut b, false, false).await;
        assert!(!send.await.unwrap().ok);
    }
    next_event(&mut b.events, |e| matches!(e, Event::DropVisibilityChanged { mode, .. } if mode == "off")).await;
    assert_eq!(b.svc.status().unwrap().mode, Mode::Off);
}

#[tokio::test]
async fn kein_platz_wird_abgelehnt() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let db = Arc::new(DatabaseManager::open_in_memory().unwrap());
    let opts = DropOptions {
        dir: tmp.path().join("b"),
        identity_path: tmp.path().join("b.key"),
        name: "Ben".into(),
        kind: DeviceKind::Laptop,
        use_relay: false,
        relay_urls: vec![],
        relay_fallback_n0: false,
        loopback_only: true,
        mdns: false,
        reserve_bytes: u64::MAX / 2,
        can_join_hotspot: false,
    };
    let b = DropService::with_identity(opts, db, DropIdentity::ephemeral());
    b.set_visibility(Mode::Everyone).await.unwrap();
    let p = a.svc.add_code(&b.status().unwrap().code.unwrap()).await.unwrap();
    let f = write(&tmp.path().join("src"), "a.txt", b"x");
    let r = a.svc.send(&p.id, vec![f]).await.unwrap();
    assert!(!r.ok && r.error.unwrap().contains("Platz"));
    assert!(b.pending_offers().is_empty());
}

// --- Manipulierte Gegenstellen (eigener Client ohne DropService) ---

async fn raw_connect(target: &str) -> (Endpoint, Connection) {
    let info = ConnectInfo::from_code(target).unwrap();
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let mut addr = EndpointAddr::from(info.endpoint);
    for a in info.addrs {
        addr.addrs.insert(TransportAddr::Ip(a));
    }
    let conn = ep.connect(addr, wire::DROP_ALPN).await.unwrap();
    (ep, conn)
}

struct Raw {
    _ep: Endpoint,
    conn: Connection,
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
    session: [u8; 32],
}

async fn raw_probe(target: &str) -> (Raw, Card) {
    let (ep, conn) = raw_connect(target).await;
    let session = identity::session_value(&conn).unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame_max(&mut send, &Probe { v: 1, tags: vec![] }, wire::MAX_FRAME).await.unwrap();
    let card: Card = read_frame_max(&mut recv, wire::MAX_FRAME).await.unwrap();
    (Raw { _ep: ep, conn, send, recv, session }, card)
}

fn file(name: &str, data: &[u8]) -> OfferFile {
    OfferFile {
        name: name.into(),
        size: data.len() as u64,
        hash: wire::hex(blake3::hash(data).as_bytes()),
        mime: "text/plain".into(),
    }
}

fn offer(id: &DropIdentity, session: &[u8; 32], files: Vec<OfferFile>) -> Offer {
    Offer {
        name: "Mallory".into(),
        kind: DeviceKind::Laptop,
        proof: Proof::new(&id.public(), &id.sign_session(session)),
        files,
        preview: None,
    }
}

/// Sendet ein Angebot, lässt es annehmen und schickt dann rohe Datei-Ströme.
async fn raw_offer_accepted(b: &mut Node, files: Vec<OfferFile>) -> Raw {
    let (mut raw, card) = raw_probe(&code_of(b)).await;
    assert!(matches!(card, Card::Visible { proof: None, .. }), "Fremde bekommen keinen Schlüssel");
    let me = DropIdentity::ephemeral();
    write_frame_max(&mut raw.send, &offer(&me, &raw.session, files), wire::MAX_FRAME).await.unwrap();
    answer(b, true, false).await;
    let r: Reply = read_frame_max(&mut raw.recv, wire::MAX_FRAME).await.unwrap();
    assert!(r.accepted);
    raw
}

async fn uni(conn: &Connection, idx: u32, data: &[u8]) {
    let mut s = conn.open_uni().await.unwrap();
    s.write_u32(idx).await.unwrap();
    let _ = s.write_all(data).await;
    let _ = s.finish();
}

async fn expect_failed(b: &mut Node) {
    let ev = next_event(&mut b.events, |e| matches!(e, Event::DropFinished { .. })).await;
    assert!(matches!(ev, Event::DropFinished { ok: false, .. }), "{ev:?}");
}

#[tokio::test]
async fn manipulierte_gegenstellen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();

    // 1. Beweis aus einer anderen Sitzung → ungültig, kein Angebot beim Menschen.
    let (mut raw, _) = raw_probe(&code_of(&b)).await;
    let me = DropIdentity::ephemeral();
    write_frame_max(&mut raw.send, &offer(&me, &[0u8; 32], vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
    let r: Reply = read_frame_max(&mut raw.recv, wire::MAX_FRAME).await.unwrap();
    assert_eq!((r.accepted, r.reason.as_deref()), (false, Some("invalid")));
    assert!(b.svc.pending_offers().is_empty());

    // 2. Unbekannte Felder / falsche Version in der Probe → Verbindung endet ohne Karte.
    let (_ep, conn) = raw_connect(&code_of(&b)).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame_max(&mut send, &serde_json::json!({"v": 1, "tags": [], "x": 1}), wire::MAX_FRAME).await.unwrap();
    assert!(read_frame_max::<_, Card>(&mut recv, wire::MAX_FRAME).await.is_err());
    let (_ep, conn) = raw_connect(&code_of(&b)).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame_max(&mut send, &Probe { v: 99, tags: vec![] }, wire::MAX_FRAME).await.unwrap();
    assert!(read_frame_max::<_, Card>(&mut recv, wire::MAX_FRAME).await.is_err());

    // 3. Riesiger Rahmen → abgewiesen, bevor er gelesen wird.
    let (_ep, conn) = raw_connect(&code_of(&b)).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    send.write_u32(wire::MAX_FRAME + 1).await.unwrap();
    let _ = send.write_all(&[b'{'; 1024]).await;
    assert!(read_frame_max::<_, Card>(&mut recv, wire::MAX_FRAME).await.is_err());

    // 4. Pfad im Dateinamen → nur der letzte Teil landet in der Ablage.
    let raw = raw_offer_accepted(&mut b, vec![file("../../.ssh/authorized_keys", b"key")]).await;
    uni(&raw.conn, 0, b"key").await;
    next_event(&mut b.events, |e| matches!(e, Event::DropFinished { ok: true, .. })).await;
    assert_eq!(inbox_files(&b), vec!["authorized_keys".to_string()]);
    assert!(!tmp.path().join(".ssh").exists());
    let _ = std::fs::remove_file(b.inbox.join("authorized_keys"));

    // 5. Mehr Daten als angekündigt → verworfen, keine Reste.
    let raw = raw_offer_accepted(&mut b, vec![file("a.txt", b"abc")]).await;
    uni(&raw.conn, 0, b"abcdefgh").await;
    expect_failed(&mut b).await;

    // 6. Falsche Prüfsumme → verworfen.
    let raw = raw_offer_accepted(&mut b, vec![file("b.txt", b"abc")]).await;
    uni(&raw.conn, 0, b"xyz").await;
    expect_failed(&mut b).await;

    // 7. Unbekannter / doppelter Index → Abbruch; schon Empfangenes wird entfernt.
    let raw = raw_offer_accepted(&mut b, vec![file("c.txt", b"1"), file("d.txt", b"2")]).await;
    uni(&raw.conn, 0, b"1").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    uni(&raw.conn, 0, b"1").await;
    expect_failed(&mut b).await;
    let raw = raw_offer_accepted(&mut b, vec![file("e.txt", b"1")]).await;
    uni(&raw.conn, 7, b"1").await;
    expect_failed(&mut b).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(inbox_files(&b).is_empty(), "keine Reste: {:?}", inbox_files(&b));
}

#[tokio::test]
async fn absender_bricht_ab() {
    let tmp = tempfile::tempdir().unwrap();
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let raw = raw_offer_accepted(&mut b, vec![file("f.txt", &[1u8; 100_000])]).await;
    let mut s = raw.conn.open_uni().await.unwrap();
    s.write_u32(0).await.unwrap();
    s.write_all(&[1u8; 1000]).await.unwrap();
    raw.conn.close(VarInt::from_u32(9), b"weg");
    expect_failed(&mut b).await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(inbox_files(&b).is_empty(), "keine Reste: {:?}", inbox_files(&b));
}

#[tokio::test]
async fn grenzen_fuer_anfragen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let code = code_of(&b);

    // Ein Absender (Schlüssel) hat höchstens ein offenes Angebot.
    let me = DropIdentity::ephemeral();
    let (mut r1, _) = raw_probe(&code).await;
    write_frame_max(&mut r1.send, &offer(&me, &r1.session, vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
    next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await;
    let (mut r2, _) = raw_probe(&code).await;
    write_frame_max(&mut r2.send, &offer(&me, &r2.session, vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
    let r: Reply = read_frame_max(&mut r2.recv, wire::MAX_FRAME).await.unwrap();
    assert_eq!(r.reason.as_deref(), Some("busy"));

    // Höchstens drei offene Angebote insgesamt.
    let mut keep = vec![r1];
    for _ in 0..2 {
        let (mut r, _) = raw_probe(&code).await;
        let id = DropIdentity::ephemeral();
        write_frame_max(&mut r.send, &offer(&id, &r.session, vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
        next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await;
        keep.push(r);
    }
    assert_eq!(b.svc.pending_offers().len(), 3);
    let (mut r, _) = raw_probe(&code).await;
    let id = DropIdentity::ephemeral();
    write_frame_max(&mut r.send, &offer(&id, &r.session, vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
    let rep: Reply = read_frame_max(&mut r.recv, wire::MAX_FRAME).await.unwrap();
    assert_eq!(rep.reason.as_deref(), Some("busy"));

    // Absender legt auf → Angebot verschwindet beim Empfänger.
    keep[0].conn.close(VarInt::from_u32(0), b"weg");
    next_event(&mut b.events, |e| matches!(e, Event::DropOfferClosed { .. })).await;
    assert_eq!(b.svc.pending_offers().len(), 2);

    // Mehr als 6 Angebote pro Minute → „busy“ ohne Rückfrage.
    for k in &mut keep[1..] {
        k.conn.close(VarInt::from_u32(0), b"weg");
    }
    for _ in 0..2 {
        next_event(&mut b.events, |e| matches!(e, Event::DropOfferClosed { .. })).await;
    }
    // Sechstes Angebot dieser Minute kommt noch an, das siebte nicht mehr.
    let (mut r6, _) = raw_probe(&code).await;
    let id = DropIdentity::ephemeral();
    write_frame_max(&mut r6.send, &offer(&id, &r6.session, vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
    next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await;
    let (mut r, _) = raw_probe(&code).await;
    let id = DropIdentity::ephemeral();
    write_frame_max(&mut r.send, &offer(&id, &r.session, vec![file("a", b"a")]), wire::MAX_FRAME).await.unwrap();
    let rep: Reply = read_frame_max(&mut r.recv, wire::MAX_FRAME).await.unwrap();
    assert_eq!(rep.reason.as_deref(), Some("busy"), "Angebots-Limit erreicht");
    assert_eq!(b.svc.pending_offers().len(), 1);
}

#[tokio::test]
async fn offline_kurzanfrage_nimmt_passendes_angebot_an() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    let src = tmp.path().join("src");
    let f1 = write(&src, "a.jpg", b"bild");
    let f2 = write(&src, "b.txt", b"text");

    // Kurzanfrage wie über Bluetooth: prüfen, anzeigen, annehmen.
    let req = a.svc.hotspot_request("Snap-1234", "geheimespasswort", &[f1.clone(), f2.clone()]).await.unwrap();
    let shown = b.svc.parse_hotspot(&req).unwrap();
    assert_eq!((shown.name.as_str(), shown.files, shown.total), ("Anna", 2, 8));
    assert_eq!(shown.ssid, "Snap-1234");
    b.svc.pre_accept(&req).unwrap();

    // Passendes Angebot läuft ohne zweite Rückfrage durch.
    let r = a.svc.send(&peer.id, vec![f1.clone(), f2.clone()]).await.unwrap();
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(inbox_files(&b), vec!["a.jpg", "b.txt"]);

    // Nur einmal: das nächste Angebot fragt wieder nach.
    let send = {
        let (svc, id, f1) = (a.svc.clone(), peer.id.clone(), f1.clone());
        tokio::spawn(async move { svc.send(&id, vec![f1]).await.unwrap() })
    };
    answer(&mut b, false, false).await;
    assert!(!send.await.unwrap().ok);

    // Abweichendes Angebot (andere Anzahl) wird nicht automatisch angenommen.
    b.svc.pre_accept(&req).unwrap();
    let send = {
        let (svc, id) = (a.svc.clone(), peer.id.clone());
        tokio::spawn(async move { svc.send(&id, vec![f1]).await.unwrap() })
    };
    answer(&mut b, false, false).await;
    assert!(!send.await.unwrap().ok);

    // Beschädigte Kurzanfragen werden abgewiesen.
    assert!(b.svc.parse_hotspot(&req[..req.len() - 1]).is_err());
    assert!(b.svc.pre_accept(&[]).is_err());
}
