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
        text: None,
        remote: None,
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

#[tokio::test]
async fn bluetooth_daten_offline_geraete_und_neustart() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let b = node(tmp.path(), "Ben");

    // Name nur im Modus „Alle“ in den Bluetooth-Verbindungsdaten.
    assert!(b.svc.connect_info().await.is_none(), "unsichtbar: nichts");
    b.svc.set_visibility(Mode::Contacts).await.unwrap();
    assert_eq!(b.svc.connect_info().await.unwrap().name, None);
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let info = b.svc.connect_info().await.unwrap();
    assert_eq!(info.name.as_deref(), Some("Ben"));

    // Neustart der Sitzung: neue Transport-Kennung.
    let neu = b.svc.refresh_session().await.unwrap().unwrap();
    assert_ne!(neu.endpoint, info.endpoint);
    assert!(a.svc.add_code(&info.to_code()).await.is_err(), "alte Kennung ist weg");
    assert!(a.svc.add_code(&neu.to_code()).await.is_ok());

    // Über Bluetooth gefunden, per Netz nicht erreichbar → bleibt als „offline“ mit Namen.
    let fremd = ConnectInfo {
        endpoint: iroh::SecretKey::generate().public(),
        token: [1; 8],
        can_join_hotspot: true,
        relay: false,
        addrs: vec!["127.0.0.1:9".parse().unwrap()],
        name: Some("Offline-Olli".into()),
    };
    let mut ev = a.events;
    a.svc.add_peer(&fremd, "bluetooth");
    next_event(&mut ev, |e| matches!(e, Event::DropNearbyChanged)).await;
    let list = a.svc.nearby();
    let o = list.iter().find(|p| p.offline).expect("offline-Eintrag");
    assert_eq!(o.name.as_deref(), Some("Offline-Olli"));
    assert_eq!(o.via, "bluetooth");
    // Ohne Namen (Kontakte-Modus beim Gegenüber) und unerreichbar → verschwindet.
    let stumm = ConnectInfo { endpoint: iroh::SecretKey::generate().public(), name: None, ..fremd };
    a.svc.add_peer(&stumm, "bluetooth");
    next_event(&mut ev, |e| matches!(e, Event::DropNearbyChanged)).await;
    assert!(!a.svc.nearby().iter().any(|p| p.id == wire::hex(stumm.endpoint.as_bytes())));
}

#[tokio::test]
async fn eigener_anzeigename_und_lokale_kontaktnamen() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");

    // Eigener Name: andere sehen ihn, Zurücksetzen nimmt wieder den Gerätenamen
    let st = b.svc.set_name(Some("  Bens\u{202E} Laptop ")).unwrap();
    assert_eq!(st.name, "Bens Laptop");
    assert!(st.custom_name);
    assert_eq!(st.default_name, "Ben");
    assert!(b.svc.set_name(Some(&"x".repeat(41))).is_err());
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    assert_eq!(peer.name.as_deref(), Some("Bens Laptop"));
    assert_eq!(b.svc.connect_info().await.unwrap().name.as_deref(), Some("Bens Laptop"));

    // Tausch, dann Kontakt speichern und lokal umbenennen
    let f = write(&tmp.path().join("src"), "a.txt", b"x");
    let send = {
        let (svc, id, f) = (a.svc.clone(), peer.id.clone(), f.clone());
        tokio::spawn(async move { svc.send(&id, vec![f]).await.unwrap() })
    };
    let offer_id = answer(&mut b, true, false).await;
    let r = send.await.unwrap();
    assert!(r.ok);
    let c = a.svc.save_contact(r.id).unwrap();
    assert_eq!((c.name.as_str(), c.renamed), ("Bens Laptop", false));
    b.svc.save_contact(offer_id).unwrap();
    let c = a.svc.rename_contact(&c.key, Some("Ben (Arbeit)")).unwrap();
    assert_eq!((c.name.as_str(), c.device_name.as_str(), c.renamed), ("Ben (Arbeit)", "Bens Laptop", true));
    assert!(a.svc.rename_contact(&"00".repeat(32), Some("x")).is_err());

    // Gegenüber benennt sich um: der lokale Name bleibt, der Gerätename wird aktualisiert
    b.svc.set_name(None).unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    assert_eq!(peer.contact.as_deref(), Some("Ben (Arbeit)"));
    let send = {
        let (svc, id) = (a.svc.clone(), peer.id.clone());
        tokio::spawn(async move { svc.send(&id, vec![f]).await.unwrap() })
    };
    answer(&mut b, true, false).await;
    assert!(send.await.unwrap().ok);
    let c = &a.svc.contacts().unwrap()[0];
    assert_eq!((c.name.as_str(), c.device_name.as_str()), ("Ben (Arbeit)", "Ben"));
    // Lokalen Namen entfernen → wieder der Name des Geräts
    let c = a.svc.rename_contact(&c.key, None).unwrap();
    assert_eq!((c.name.as_str(), c.renamed), ("Ben", false));
}

#[tokio::test]
async fn text_und_links_teilen() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();

    // Link: Angebot zeigt ihn erkannt, nach dem Annehmen kommt er im Ergebnis an – keine Datei
    let send = {
        let (svc, id) = (a.svc.clone(), peer.id.clone());
        tokio::spawn(async move {
            svc.send_with_text(&id, vec![], Some("https://example.org/rezept".into())).await.unwrap()
        })
    };
    let ev = next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await;
    let Event::DropOffer { id, files, kinds, .. } = ev else { unreachable!() };
    assert_eq!(files, 0);
    assert_eq!(kinds, vec!["link".to_string()]);
    let offer = b.svc.pending_offers().into_iter().find(|o| o.id == id).unwrap();
    assert_eq!(offer.link.as_deref(), Some("https://example.org/rezept"));
    b.svc.respond(id, true, false).unwrap();
    assert!(send.await.unwrap().ok);
    let fin = next_event(&mut b.events, |e| matches!(e, Event::DropFinished { incoming: true, .. })).await;
    let Event::DropFinished { ok, text, link, files, .. } = fin else { unreachable!() };
    assert!(ok && files.is_empty());
    assert_eq!(text.as_deref(), Some("https://example.org/rezept"));
    assert_eq!(link.as_deref(), Some("https://example.org/rezept"));
    assert!(inbox_files(&b).is_empty());

    // Text mit Steuerzeichen: bereinigt, kein Link; „javascript:“ wird nie ein Link
    let send = {
        let (svc, id) = (a.svc.clone(), peer.id.clone());
        tokio::spawn(async move {
            svc.send_with_text(&id, vec![], Some("javascript:alert(1)\u{202E}\u{7}".into())).await.unwrap()
        })
    };
    answer(&mut b, true, false).await;
    assert!(send.await.unwrap().ok);
    let fin = next_event(&mut b.events, |e| matches!(e, Event::DropFinished { incoming: true, .. })).await;
    let Event::DropFinished { text, link, .. } = fin else { unreachable!() };
    assert_eq!(text.as_deref(), Some("javascript:alert(1)"));
    assert_eq!(link, None);

    // Zu langer oder leerer Text wird schon beim Sender abgewiesen
    let r = a.svc.send_with_text(&peer.id, vec![], Some("x".repeat(70_000))).await.unwrap();
    assert!(!r.ok);
    let r = a.svc.send_with_text(&peer.id, vec![], Some("   ".into())).await.unwrap();
    assert!(!r.ok);
}

fn opts_for(root: &Path, name: &str) -> DropOptions {
    DropOptions {
        dir: root.join(name).join("Snap Drop"),
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
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn hintergrund_fenster_aendert_die_sichtbarkeit_nicht() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("a")).unwrap();
    let db = Arc::new(DatabaseManager::open_in_memory().unwrap());
    let svc = DropService::with_identity(opts_for(tmp.path(), "a"), db.clone(), DropIdentity::ephemeral());

    // Aus → im Hintergrund kurz für Kontakte sichtbar, danach wieder aus
    let st = svc.background(60, false).await.unwrap();
    assert_eq!(st.mode, Mode::Contacts);
    assert!(st.code.is_some(), "Sitzung läuft");
    assert_eq!(svc.background(0, false).await.unwrap().mode, Mode::Off);

    // Vom Nutzer sichtbar geschaltet: der Hintergrund ändert nichts und schaltet nichts ab
    svc.set_visibility(Mode::Contacts).await.unwrap();
    assert_eq!(svc.background(60, false).await.unwrap().mode, Mode::Contacts);
    assert_eq!(svc.background(0, false).await.unwrap().mode, Mode::Contacts);
    svc.set_visibility(Mode::Off).await.unwrap();

    // „Alle“ läuft nach einem Neustart (App geschlossen) nur weiter, wenn es erlaubt ist
    svc.set_visibility(Mode::Everyone).await.unwrap();
    let until = svc.status().unwrap().until.unwrap();
    svc.shutdown().await;
    drop(svc);
    let svc = DropService::with_identity(opts_for(tmp.path(), "a"), db.clone(), DropIdentity::ephemeral());
    assert_eq!(svc.status().unwrap().mode, Mode::Off);
    let st = svc.background(60, false).await.unwrap();
    assert_eq!(st.mode, Mode::Contacts, "„Nur Kontakte“ weckt nie für alle");
    svc.background(0, false).await.unwrap();
    let st = svc.background(60, true).await.unwrap();
    assert_eq!((st.mode, st.until), (Mode::Everyone, Some(until)), "das alte Fenster läuft weiter, nicht länger");
    svc.background(0, true).await.unwrap();
    // Abgelaufenes Fenster: nur Kontakte
    svc.set_visibility(Mode::Off).await.unwrap();
    assert_eq!(svc.background(60, true).await.unwrap().mode, Mode::Contacts);
    svc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn weck_schluessel_nur_als_private_datei() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("a")).unwrap();
    let db = Arc::new(DatabaseManager::open_in_memory().unwrap());
    let svc = DropService::with_identity(opts_for(tmp.path(), "a"), db.clone(), DropIdentity::ephemeral());
    let secret = [9u8; 32];
    db.upsert_drop_contact(&space_core::database::DropContactRow {
        key: "aa".repeat(32),
        name: "Anna".into(),
        alias: None,
        secret: secret.to_vec(),
        added_at: 0,
        last_seen: 0,
        remote: None,
        remote_ok: true,
    })
    .unwrap();
    let file = svc.wake_file();
    assert!(!file.exists(), "ohne Einschalten keine Datei");
    svc.set_wake_export(true).unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    let keys: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let expected = data_encoding::HEXLOWER.encode(&space_core::drop::wake::wake_key(&secret));
    assert_eq!(keys[0]["key"], expected.as_str());
    assert_eq!(keys[0]["name"], "Anna");
    // Kontakt entfernen → Datei ohne ihn
    assert!(svc.remove_contact(&"aa".repeat(32)).unwrap());
    let keys: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(keys.as_array().unwrap().len(), 0);
    // Ausschalten löscht die Datei
    svc.set_wake_export(false).unwrap();
    assert!(!file.exists());
    svc.shutdown().await;
}

/// Tauscht einmal in der Nähe (A → B) und speichert beide als Kontakt.
async fn befreunden(a: &mut Node, b: &mut Node, src: &Path) {
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(b)).await.unwrap();
    let f = write(src, "hallo.txt", b"hallo");
    let send = {
        let svc = a.svc.clone();
        tokio::spawn(async move { svc.send(&peer.id, vec![f]).await.unwrap() })
    };
    let offer_id = answer(b, true, false).await;
    let res = send.await.unwrap();
    assert!(res.ok, "{:?}", res.error);
    b.svc.save_contact(offer_id).unwrap();
    a.svc.save_contact(res.id).unwrap();
    b.svc.set_visibility(Mode::Off).await.unwrap();
}

fn key_of(n: &Node) -> String {
    data_encoding::HEXLOWER.encode(n.svc.drop_key().as_bytes())
}

#[tokio::test(flavor = "multi_thread")]
async fn von_ueberall_an_kontakte() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    let src = tmp.path().join("src");
    befreunden(&mut a, &mut b, &src).await;
    let c = a.svc.contacts().unwrap().into_iter().find(|c| c.name == "Ben").unwrap();
    assert!(c.remote && c.remote_ok, "Adresse nach dem Annehmen bekannt: {c:?}");
    assert!(b.svc.contacts().unwrap()[0].remote, "auch umgekehrt (aus dem Angebot)");

    // B ist unsichtbar – trotzdem kommt ein Angebot von Kontakt A an (Annehmen bleibt Pflicht)
    assert_eq!(b.svc.status().unwrap().mode, Mode::Off);
    let f = write(&src, "fern.txt", b"von weit weg");
    let send = {
        let svc = a.svc.clone();
        let key = key_of(&b);
        tokio::spawn(async move { svc.send_to_contact(&key, vec![f], None).await.unwrap() })
    };
    let Event::DropOffer { id, contact, .. } =
        next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(contact.as_deref(), Some("Anna"));
    b.svc.respond(id, true, false).unwrap();
    let res = send.await.unwrap();
    assert!(res.ok, "{:?}", res.error);
    assert_eq!(std::fs::read(b.inbox.join("fern.txt")).unwrap(), b"von weit weg");

    // Ablehnen wirkt wie immer
    let f = write(&src, "nein.txt", b"x");
    let send = {
        let svc = a.svc.clone();
        let key = key_of(&b);
        tokio::spawn(async move { svc.send_to_contact(&key, vec![f], None).await.unwrap() })
    };
    answer(&mut b, false, false).await;
    assert!(!send.await.unwrap().ok);

    // B erlaubt A nicht mehr von überall → abgewiesen, ohne Angebot
    b.svc.set_contact_remote(&key_of(&a), false).unwrap();
    let f = write(&src, "zu.txt", b"x");
    let r = a.svc.send_to_contact(&key_of(&b), vec![f.clone()], None).await.unwrap();
    assert!(!r.ok);
    b.svc.set_contact_remote(&key_of(&a), true).unwrap();

    // B schaltet „von überall“ ganz aus → abgewiesen
    b.svc.set_remote(false).await.unwrap();
    let r = a.svc.send_to_contact(&key_of(&b), vec![f], None).await.unwrap();
    assert!(!r.ok);
    // Unbekannter Kontakt
    assert!(a.svc.send_to_contact(&"00".repeat(32), vec![], Some("x".into())).await.is_err());
    a.svc.shutdown().await;
    b.svc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fern_endpunkt_nur_fuer_kontakte() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    let mut c = node(tmp.path(), "Carla");
    let src = tmp.path().join("src");
    befreunden(&mut a, &mut b, &src).await;
    befreunden(&mut a, &mut c, &src).await;
    let db_remote = |n: &Node, name: &str| {
        let contacts = n.svc.contacts().unwrap();
        let key = contacts.iter().find(|c| c.name == name).unwrap().key.clone();
        (key, n.svc.contact_remote_for_tests(&contacts.iter().find(|c| c.name == name).unwrap().key))
    };

    // Fremder ohne Kontakt-Tag am Fern-Endpunkt von B: keine Karte, kein Angebot möglich
    let (_, remote_b) = db_remote(&a, "Ben");
    let (id, addrs) = remote_b.unwrap().validate().unwrap();
    let ep = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .bind()
        .await
        .unwrap();
    let mut addr = EndpointAddr::from(id);
    for x in &addrs {
        addr.addrs.insert(TransportAddr::Ip(*x));
    }
    let conn = ep.connect(addr, wire::DROP_ALPN).await.unwrap();
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame_max(&mut send, &Probe { v: 1, tags: vec!["00".repeat(8)] }, wire::MAX_FRAME).await.unwrap();
    let card: Card = read_frame_max(&mut recv, wire::MAX_FRAME).await.unwrap();
    assert!(matches!(card, Card::Hidden), "Fremde sehen am Fern-Endpunkt nichts");

    // Manipulierte Adresse: A's Eintrag für Ben zeigt auf Carlas Fern-Endpunkt → Carla weist
    // sich aus, ist aber nicht Ben → A bricht ab, bevor irgendetwas gesendet wird
    let (key_b, _) = db_remote(&a, "Ben");
    let (_, remote_c) = db_remote(&a, "Carla");
    a.svc.set_contact_remote_for_tests(&key_b, &remote_c.unwrap());
    let f = write(&src, "falsch.txt", b"x");
    let r = a.svc.send_to_contact(&key_b, vec![f], None).await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("nicht als dieser Kontakt"));
    assert!(tokio::time::timeout(Duration::from_millis(500), c.events.next())
        .await
        .map_or(true, |e| !matches!(e, Some(Event::DropOffer { .. }))));
    let _ = &mut b;
    a.svc.shutdown().await;
    b.svc.shutdown().await;
    c.svc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn abbrechen_und_verlauf() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    let src = tmp.path().join("src");

    // Erfolgreich empfangen → im Verlauf (ohne Textinhalt), Datei existiert
    let f = write(&src, "eins.txt", b"1");
    let send = {
        let svc = a.svc.clone();
        let id = peer.id.clone();
        tokio::spawn(async move { svc.send(&id, vec![f]).await.unwrap() })
    };
    answer(&mut b, true, false).await;
    assert!(send.await.unwrap().ok);
    let h = b.svc.history(10).unwrap();
    assert_eq!((h.len(), h[0].kind.as_str(), h[0].count, h[0].exists), (1, "files", 1, true));
    assert_eq!(h[0].peer, "Anna");

    // Großer Transfer: der Empfänger bricht mitten drin ab → beide Seiten erfahren es, keine Reste
    let big: Vec<u8> = vec![7u8; 64 << 20];
    let f = write(&src, "gross.bin", &big);
    let send = {
        let svc = a.svc.clone();
        let id = peer.id.clone();
        tokio::spawn(async move { svc.send(&id, vec![f]).await.unwrap() })
    };
    let id = answer(&mut b, true, false).await;
    next_event(
        &mut b.events,
        |e| matches!(e, Event::DropProgress { incoming: true, done_bytes, .. } if *done_bytes > 0),
    )
    .await;
    b.svc.cancel(id).unwrap();
    let Event::DropFinished { ok, error, .. } =
        next_event(&mut b.events, |e| matches!(e, Event::DropFinished { incoming: true, .. })).await
    else {
        unreachable!()
    };
    assert!(!ok);
    assert_eq!(error.as_deref(), Some("Abgebrochen"));
    let r = send.await.unwrap();
    assert!(!r.ok);
    assert!(r.error.unwrap().contains("abgebrochen"), "Sender erfährt den Abbruch");
    assert!(!inbox_files(&b).iter().any(|f| f.contains("gross") || f.ends_with(".snap-part")), "{:?}", inbox_files(&b));
    assert_eq!(b.svc.history(10).unwrap().len(), 1, "Abgebrochenes nicht im Verlauf");

    // Unbekannte Kennung
    assert!(b.svc.cancel(999_999).is_err());
    b.svc.clear_history().unwrap();
    assert!(b.svc.history(10).unwrap().is_empty());
    a.svc.shutdown().await;
    b.svc.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ordner_senden_kommt_als_ordner_an() {
    let tmp = tempfile::tempdir().unwrap();
    let a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    let dir = tmp.path().join("src").join("Urlaub");
    write(&dir, "a.txt", b"eins");
    write(&dir.join("Tag 2"), "b.txt", b"zwei");
    let single = write(&tmp.path().join("src"), "einzeln.txt", b"x");
    let send = {
        let svc = a.svc.clone();
        let id = peer.id.clone();
        let d = dir.clone();
        tokio::spawn(async move { svc.send(&id, vec![d, single]).await.unwrap() })
    };
    answer(&mut b, true, false).await;
    let Event::DropFinished { ok, files, .. } =
        next_event(&mut b.events, |e| matches!(e, Event::DropFinished { incoming: true, .. })).await
    else {
        unreachable!()
    };
    assert!(ok);
    assert!(send.await.unwrap().ok);
    // Der Ordner liegt als Ordner da (kein ZIP mehr), Unterordner inklusive
    let folder = files.iter().map(PathBuf::from).find(|p| p.is_dir()).expect("Ordner im Ergebnis");
    let walk: Vec<String> = walkdir_names(&folder);
    assert!(folder.file_name().unwrap().to_string_lossy().starts_with("Urlaub"), "{folder:?}");
    assert_eq!(std::fs::read(folder.join("a.txt")).unwrap(), b"eins", "nicht doppelt verschachtelt: {walk:?}");
    assert_eq!(std::fs::read(folder.join("Tag 2/b.txt")).unwrap(), b"zwei");
    assert!(!inbox_files(&b).iter().any(|f| f.ends_with(".zip")), "{:?}", inbox_files(&b));
    // Der Arbeitsordner des Senders ist wieder leer
    let pack = tmp.path().join("Anna").join("drop-pack");
    assert!(std::fs::read_dir(&pack).map(|d| d.count()).unwrap_or(0) == 0);
    a.svc.shutdown().await;
    b.svc.shutdown().await;
}

fn walkdir_names(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn sender_sieht_den_pruefcode_vor_dem_annehmen() {
    let tmp = tempfile::tempdir().unwrap();
    let mut a = node(tmp.path(), "Anna");
    let mut b = node(tmp.path(), "Ben");
    b.svc.set_visibility(Mode::Everyone).await.unwrap();
    let peer = a.svc.add_code(&code_of(&b)).await.unwrap();
    let f = write(&tmp.path().join("src"), "x.txt", b"x");
    let send = {
        let svc = a.svc.clone();
        tokio::spawn(async move { svc.send(&peer.id, vec![f]).await.unwrap() })
    };
    // Der Sender bekommt den Code, bevor der Empfänger antwortet – und es ist derselbe
    let Event::DropSending { code: sender_code, .. } =
        next_event(&mut a.events, |e| matches!(e, Event::DropSending { .. })).await
    else {
        unreachable!()
    };
    let Event::DropOffer { id, code: receiver_code, .. } =
        next_event(&mut b.events, |e| matches!(e, Event::DropOffer { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(sender_code, receiver_code);
    b.svc.respond(id, true, false).unwrap();
    let r = send.await.unwrap();
    assert!(r.ok);
    assert_eq!(r.code.as_deref(), Some(sender_code.as_str()));
    a.svc.shutdown().await;
    b.svc.shutdown().await;
}
