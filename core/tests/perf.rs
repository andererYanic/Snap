//! Mikro-Benchmarks für Hot Paths. Ausführen mit:
//!   cargo test --release -p space-core --test perf -- --ignored --nocapture
//! Die Werte dienen dem Vergleich vor/nach Optimierungen (siehe docs/PERFORMANCE.md).

use iroh::SecretKey;
use space_core::ledger::LedgerEntry;
use space_core::DatabaseManager;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn hash(i: usize) -> String {
    format!("{:064x}", i)
}

fn signed_entries(key: &SecretKey, n: usize) -> Vec<LedgerEntry> {
    (0..n)
        .map(|i| {
            let mut e = LedgerEntry {
                origin: key.public().to_string(),
                entry_id: i as i64 + 1,
                seq: i as i64 + 1,
                hash: hash(i),
                name: format!("datei-{i}.jpg"),
                size: 1000 + i as u64,
                created_at: 1_700_000_000,
                deleted: false,
                signature: String::new(),
                ..Default::default()
            };
            e.signature = e.sign(key);
            e
        })
        .collect()
}

fn report(name: &str, n: usize, d: Duration) {
    println!(
        "{name:<44} {n:>7} ops  {:>9.2} ms  {:>9.1} µs/op",
        d.as_secs_f64() * 1e3,
        d.as_secs_f64() * 1e6 / n as f64
    );
}

#[test]
#[ignore]
fn db_hot_paths() {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(DatabaseManager::open(&dir.path().join("bench.db")).unwrap());
    let key = SecretKey::generate();
    let entries = signed_entries(&key, 10_000);

    // 1. Signaturprüfung (pro Seite 1000 Einträge)
    let t = Instant::now();
    for e in &entries[..1000] {
        e.verify().unwrap();
    }
    report("ledger verify (seriell)", 1000, t.elapsed());
    let t = Instant::now();
    space_core::ledger::verify_all(&entries[..1000]).unwrap();
    report("ledger verify_all (parallel, Seite)", 1000, t.elapsed());

    // 2. Fremde Einträge übernehmen
    let t = Instant::now();
    let applied = db.apply_remote_entries(&entries).unwrap();
    assert_eq!(applied, entries.len());
    report("apply_remote_entries (10k)", entries.len(), t.elapsed());

    // 3. Eigene Dateien veröffentlichen (upsert + signierter Ledger-Eintrag)
    let own = SecretKey::generate();
    let origin = own.public().to_string();
    let t = Instant::now();
    for i in 0..2000 {
        let (_, id) = db.upsert_file(&format!("/data/f{i}"), &hash(100_000 + i), &format!("f{i}"), 1, "t").unwrap();
        let k = own.clone();
        db.record_own_entry(
            &origin,
            id,
            &hash(100_000 + i),
            &format!("f{i}"),
            1,
            0,
            false,
            "",
            "",
            &move |e: &LedgerEntry| e.sign(&k),
        )
        .unwrap();
    }
    report("publish (upsert + record_own_entry)", 2000, t.elapsed());

    // 4. Freigabe-Prüfung pro Blob-Anfrage
    let t = Instant::now();
    for i in 0..100_000 {
        db.is_shared(&hash(100_000 + (i % 2000))).unwrap();
    }
    report("is_shared", 100_000, t.elapsed());

    // 5. Große Listen
    let t = Instant::now();
    for _ in 0..20 {
        assert_eq!(db.remote_files(&origin).unwrap().len(), 10_000);
    }
    report("remote_files (10k Einträge)", 20, t.elapsed());
    let t = Instant::now();
    for _ in 0..20 {
        db.entries_since(&Default::default(), 1000).unwrap();
    }
    report("entries_since (Seite 1000)", 20, t.elapsed());

    // 6. Lese-Latenz während Schreiblast (GUI-Flüssigkeit)
    let writer_db = db.clone();
    let more = signed_entries(&SecretKey::generate(), 20_000);
    let writer = std::thread::spawn(move || {
        for chunk in more.chunks(1000) {
            writer_db.apply_remote_entries(chunk).unwrap();
        }
    });
    let mut lat = Vec::new();
    while !writer.is_finished() {
        let t = Instant::now();
        db.list_devices().unwrap();
        db.is_shared(&hash(100_001)).unwrap();
        lat.push(t.elapsed());
    }
    writer.join().unwrap();
    lat.sort();
    let p = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
    println!(
        "{:<44} {:>7} ops  p50 {:>7.1} µs  p99 {:>9.1} µs  max {:>9.1} µs",
        "Lesen während Schreiblast",
        lat.len(),
        p(0.5).as_secs_f64() * 1e6,
        p(0.99).as_secs_f64() * 1e6,
        lat.last().unwrap().as_secs_f64() * 1e6
    );
}

#[test]
#[ignore]
fn api_browse_and_events() {
    let dir = tempfile::tempdir().unwrap();
    let db = DatabaseManager::open(&dir.path().join("bench.db")).unwrap();
    let key = SecretKey::generate();
    // 100 000 Einträge eines anderen Geräts + 5 000 eigene Dateien
    let entries = signed_entries(&key, 100_000);
    for chunk in entries.chunks(1000) {
        db.apply_remote_entries(chunk).unwrap();
    }
    for i in 0..5000 {
        db.upsert_file(&format!("/data/f{i}"), &hash(200_000 + i), &format!("eigene-{i}.txt"), 1, "t").unwrap();
    }

    let run = |name: &str, n: usize, f: &dyn Fn()| {
        let t = Instant::now();
        for _ in 0..n {
            f();
        }
        report(name, n, t.elapsed());
    };
    run("browse erste Seite (105k, 100 Einträge)", 20, &|| {
        db.browse("self", None, None, 0, 100, None).unwrap();
    });
    run("browse tiefe Seite (Offset 90 000)", 20, &|| {
        db.browse("self", None, None, 90_000, 100, None).unwrap();
    });
    run("browse Suche \"datei-9999\"", 20, &|| {
        db.browse("self", None, Some("datei-9999"), 0, 100, None).unwrap();
    });
    let (_, _, cursor) = db.browse("self", None, None, 0, 100, None).unwrap();
    let mut c = cursor.unwrap();
    for _ in 0..900 {
        c = db.browse("self", None, None, 0, 100, Some(&c)).unwrap().2.unwrap();
    }
    run("browse tiefe Seite per Cursor (~Eintrag 90 000)", 20, &|| {
        db.browse("self", None, None, 0, 100, Some(&c)).unwrap();
    });
    run("browse nur eigene (5k)", 20, &|| {
        db.browse("self", Some(None), None, 0, 100, None).unwrap();
    });

    // Ereignis-Overhead: Übernahme mit und ohne Abonnent
    let more = signed_entries(&SecretKey::generate(), 10_000);
    let t = Instant::now();
    for c in more[..5000].chunks(1000) {
        db.apply_remote_entries(c).unwrap();
    }
    report("apply ohne Abonnent", 5000, t.elapsed());
    let _sub = db.events().subscribe();
    let t = Instant::now();
    for c in more[5000..].chunks(1000) {
        db.apply_remote_entries(c).unwrap();
    }
    report("apply mit Abonnent", 5000, t.elapsed());
}

#[test]
#[ignore]
fn auth_checks() {
    use space_core::api::auth::{authorize, Caller, Capability};
    let dir = tempfile::tempdir().unwrap();
    let db = DatabaseManager::open(&dir.path().join("bench.db")).unwrap();
    let shared = dir.path().join("geteilt/a/b/c");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("f.txt"), b"x").unwrap();
    db.add_device("dev", "Handy").unwrap();
    let root = std::fs::canonicalize(dir.path().join("geteilt")).unwrap();
    db.add_grant("dev", "files.read", Some(root.to_str().unwrap())).unwrap();
    let dev = Caller::Device("dev".into());
    let prot = vec![dir.path().join("data")];

    let t = Instant::now();
    for _ in 0..100_000 {
        authorize(&db, &[], &Caller::Local, Capability::Admin, None, "x").unwrap();
    }
    report("authorize lokal", 100_000, t.elapsed());
    let t = Instant::now();
    for _ in 0..100_000 {
        authorize(&db, &[], &dev, Capability::CatalogRead, None, "x").unwrap();
    }
    report("authorize Gerät catalog.read", 100_000, t.elapsed());
    let target = shared.join("f.txt");
    let t = Instant::now();
    for _ in 0..10_000 {
        authorize(&db, &prot, &dev, Capability::FilesRead, Some(&target), "x").unwrap();
    }
    report("authorize Gerät files.read + Pfad + Audit", 10_000, t.elapsed());
}

#[test]
#[ignore]
fn share_scan() {
    use space_core::events::Event;
    use space_core::ipc::{Request, Response};
    use space_core::{Config, Daemon};
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        // 10 000 Dateien à 4 KiB in 100 Ordnern
        for d in 0..100 {
            let sub = share.join(format!("ordner-{d:03}"));
            std::fs::create_dir_all(&sub).unwrap();
            for f in 0..100 {
                let mut data = vec![0u8; 4096];
                data[..8].copy_from_slice(&((d * 100 + f) as u64).to_le_bytes());
                std::fs::write(sub.join(format!("datei-{f:03}.bin")), data).unwrap();
            }
        }
        let mut c = Config::portable(&dir.path().join("node"));
        c.use_relay = false;
        let d = Daemon::start_loopback(c).await.unwrap();
        let mut sub = d.subscribe();

        let t = Instant::now();
        assert!(matches!(
            d.request(Request::ShareAdd { path: share.clone(), name: Some("S".into()), devices: Some(vec![]) }).await,
            Response::ShareAdded { .. }
        ));
        loop {
            if let Some(Event::ShareScanned { total, .. }) = sub.next().await {
                if total >= 10_000 {
                    break;
                }
            }
        }
        report("Erstscan (hashen + importieren)", 10_000, t.elapsed());

        let t = Instant::now();
        match d.request(Request::ShareRescan { name: Some("S".into()) }).await {
            Response::Scanned { results } => assert_eq!(results[0].unchanged, 10_000),
            other => panic!("{other:?}"),
        }
        report("Rescan ohne Änderungen (nur stat)", 10_000, t.elapsed());

        // Eine Datei ändern -> Watcher -> Katalog. Vorher prüfen, dass der Watcher ruhig ist
        // (keine Scans durch eigene Lesezugriffe).
        let quiet = tokio::time::timeout(std::time::Duration::from_secs(4), async {
            loop {
                if let Some(e @ Event::ShareScanned { .. }) = sub.next().await {
                    return e;
                }
            }
        })
        .await;
        assert!(quiet.is_err(), "Watcher hat ohne Änderung gescannt: {quiet:?}");
        let t = Instant::now();
        std::fs::write(share.join("ordner-042/datei-007.bin"), b"geaendert").unwrap();
        loop {
            if let Some(Event::ShareScanned { changed, .. }) = sub.next().await {
                if changed >= 1 {
                    break;
                }
            }
        }
        report("Änderung -> Katalog (inkl. 2 s Bündelung)", 1, t.elapsed());
        d.shutdown().await;
    });
}

#[test]
#[ignore]
fn pin_mirror() {
    use space_core::ipc::{Request, Response};
    use space_core::{Config, Daemon};
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let port = || std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mk = |n: &str| {
            let mut c = Config::portable(&dir.path().join(n));
            c.use_relay = false;
            c.bind_port = Some(port());
            c.device_name = n.into();
            c
        };
        let pc = Daemon::start_loopback(mk("PC")).await.unwrap();
        let phone = Daemon::start_loopback(mk("Handy")).await.unwrap();
        let code = match pc.request(Request::PairInvite).await {
            Response::PairInvite { code, .. } => code,
            other => panic!("{other:?}"),
        };
        assert!(matches!(phone.request(Request::PairJoin { code }).await, Response::Paired { .. }));
        const N: usize = 2000;
        let share = dir.path().join("Fotos");
        for d in 0..20 {
            std::fs::create_dir_all(share.join(format!("o{d:02}"))).unwrap();
            for f in 0..N / 20 {
                std::fs::write(share.join(format!("o{d:02}/f{f:03}.bin")), format!("{d}-{f}").repeat(100)).unwrap();
            }
        }
        assert!(matches!(
            pc.request(Request::ShareAdd { path: share.clone(), name: Some("Fotos".into()), devices: None }).await,
            Response::ShareAdded { .. }
        ));
        // warten, bis der Katalog vollständig beim Handy ist
        loop {
            phone.request(Request::Sync).await;
            if let Response::Catalog { remote, .. } = phone.request(Request::List).await {
                if remote.len() >= N {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let mirror = phone.config().pin_dir.join("PC/Fotos");
        let count = |p: &std::path::Path| walk_count(p);
        let t = Instant::now();
        assert!(matches!(phone.request(Request::PinAdd { target: "Fotos".into() }).await, Response::PinAdded { .. }));
        while count(&mirror) < N {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        report("Erste Spiegelung (Dateien)", N, t.elapsed());

        let target = mirror.join("o07/f042.bin");
        let t = Instant::now();
        std::fs::write(share.join("o07/f042.bin"), b"geaendert").unwrap();
        while std::fs::read(&target).ok().as_deref() != Some(b"geaendert") {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        report("Einzeländerung -> Kopie aktualisiert", 1, t.elapsed());
        pc.shutdown().await;
        phone.shutdown().await;
    });
}

fn walk_count(p: &std::path::Path) -> usize {
    let Ok(rd) = std::fs::read_dir(p) else { return 0 };
    rd.flatten().map(|e| if e.path().is_dir() { walk_count(&e.path()) } else { 1 }).sum()
}

/// Aktueller und maximaler Arbeitsspeicher dieses Prozesses (MiB) aus /proc.
fn rss_mib() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |k: &str| {
        status
            .lines()
            .find(|l| l.starts_with(k))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    (field("VmRSS:"), field("VmHWM:"))
}

#[test]
#[ignore]
fn local_index_million() {
    use space_core::index::{LocalIndex, NodeKind, SearchFilter};
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("index.db");
    let (rss0, _) = rss_mib();

    // 1) Echter Scan: 100 000 leere Dateien in 1 000 Ordnern
    let tree = dir.path().join("baum");
    for d in 0..1_000 {
        let p = tree.join(format!("ordner-{}/sub-{d}", d / 50));
        std::fs::create_dir_all(&p).unwrap();
        for f in 0..100 {
            std::fs::File::create(p.join(format!("datei-{d}-{f}.txt"))).unwrap();
        }
    }
    let idx = LocalIndex::open(&db_path, vec![]).unwrap();
    let root = idx.add_root(&tree).unwrap();
    let t = Instant::now();
    let st = idx.rebuild_root(root, &tree).unwrap();
    report("index: echter Scan (Dateien)", st.files as usize, t.elapsed());
    // Periodischer Abgleich ohne Änderungen: nur `stat` je Ordner
    let t = Instant::now();
    let r = idx.refresh(root, &tree, &tree, true).unwrap();
    report("index: Abgleich unverändert (Ordner)", (r.dirs_read + r.dirs_unchanged) as usize, t.elapsed());
    // 100 neue Dateien in 100 Ordnern
    for d in 0..100 {
        let p = tree.join(format!("ordner-{}/sub-{}", d * 10 / 50, d * 10));
        std::fs::File::create(p.join("neu.txt")).unwrap();
    }
    let t = Instant::now();
    let r = idx.refresh(root, &tree, &tree, true).unwrap();
    assert_eq!(r.added, 100);
    report("index: Abgleich mit 100 Änderungen (Ordner)", (r.dirs_read + r.dirs_unchanged) as usize, t.elapsed());
    // Watcher-Ereignis: ein Ordner mit 100 Dateien
    let one = tree.join("ordner-0/sub-0");
    let t = Instant::now();
    let n = 200;
    for _ in 0..n {
        idx.refresh(root, &tree, &one, false).unwrap();
    }
    report("index: Watcher-Abgleich eines Ordners", n, t.elapsed());
    let t = Instant::now();
    let dirs = idx.dir_paths(root, 50_000).unwrap();
    report("index: Ordnerliste für den Watcher", dirs.len(), t.elapsed());
    std::fs::remove_dir_all(&tree).unwrap();

    // Explorer: Ordner mit 10 000 Dateien live lesen und sortieren
    let big = dir.path().join("gross");
    std::fs::create_dir(&big).unwrap();
    for i in 0..10_000 {
        std::fs::File::create(big.join(format!("Bild_{i}.jpg"))).unwrap();
    }
    let t = Instant::now();
    let n = 20;
    for _ in 0..n {
        let (page, total) = space_core::api::explorer::read_dir_page(&big, false, 0, 200).unwrap();
        assert_eq!((page.len(), total), (200, 10_000));
    }
    report("explorer: 10 000 Dateien, erste Seite (200)", n, t.elapsed());
    std::fs::remove_dir_all(&big).unwrap();

    // 2) 1 Mio. synthetische Einträge
    let syn = idx.add_root(std::path::Path::new("/synthetisch")).unwrap();
    let t = Instant::now();
    idx.insert_synthetic(syn, 10_000, 100).unwrap();
    report("index: 1 Mio. Einträge einfügen + FTS", 1_000_000, t.elapsed());
    let total = idx.count().unwrap();
    let db_mib = ["", "-wal"]
        .iter()
        .map(|s| std::fs::metadata(format!("{}{s}", db_path.display())).map(|m| m.len()).unwrap_or(0))
        .sum::<u64>() as f64
        / 1_048_576.0;
    println!("index: {total} Einträge, Datei {db_mib:.0} MiB ({:.0} B/Eintrag)", db_mib * 1_048_576.0 / total as f64);

    // 3) Suchen (Index neu geöffnet = kalter Cache)
    drop(idx);
    let idx = LocalIndex::open(&db_path, vec![]).unwrap();
    let f = SearchFilter::default();
    let mut worst = Duration::ZERO;
    for (q, filter) in [
        ("rechnung-0042", f.clone()),
        ("laub", f.clone()),
        ("0099999", f.clone()),
        ("vertrag", SearchFilter { extensions: vec!["pdf".into()], ..Default::default() }),
        ("projekt 77", SearchFilter { kind: Some(NodeKind::Dir), ..Default::default() }),
        ("gibtsnicht", f.clone()),
        ("mu", f.clone()),
    ] {
        let t = Instant::now();
        let hits = idx.search(q, &filter, 100).unwrap();
        let d = t.elapsed();
        worst = worst.max(d);
        println!("index: suche {q:<16} {:>4} Treffer  {:>8.2} ms", hits.len(), d.as_secs_f64() * 1e3);
    }
    let n = 200;
    let t = Instant::now();
    for i in 0..n {
        idx.search(&format!("{:07}", i * 4_999), &f, 50).unwrap();
    }
    report("index: suche (warm, zufällige Nummern)", n, t.elapsed());
    let (rss, hwm) = rss_mib();
    println!(
        "index: RAM jetzt {rss:.0} MiB, Spitze {hwm:.0} MiB (Start {rss0:.0} MiB); langsamste Suche {:.2} ms",
        worst.as_secs_f64() * 1e3
    );
}

#[test]
#[ignore]
fn file_ops() {
    use space_core::fileops::jobs::{FileJobs, FileOp};
    use space_core::fileops::ops::{Conflict, Progress};
    use space_core::fileops::trash::Trash;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("r");
    const N: usize = 2000;
    for d in 0..20 {
        let p = root.join(format!("quelle/o{d:02}"));
        std::fs::create_dir_all(&p).unwrap();
        for f in 0..N / 20 {
            std::fs::write(p.join(format!("f{f:03}.txt")), b"inhalt").unwrap();
        }
    }
    std::fs::create_dir_all(root.join("ziel")).unwrap();
    let jobs = FileJobs::new(Trash::new(dir.path().join("Trash")));
    let p = Progress::default();

    let t = Instant::now();
    jobs.run(
        &FileOp::Copy { sources: vec![root.join("quelle")], dest: root.join("ziel"), conflict: Conflict::Fail },
        &p,
    )
    .unwrap();
    report("fileops: Ordner kopieren (Dateien)", N, t.elapsed());
    let t = Instant::now();
    jobs.run(
        &FileOp::Move { sources: vec![root.join("ziel/quelle")], dest: root.clone(), conflict: Conflict::KeepBoth },
        &p,
    )
    .unwrap();
    report("fileops: Ordner verschieben, gleiches Laufwerk", N, t.elapsed());
    // 1000 Einzeldateien in den Papierkorb, auflisten, alles rückgängig
    let files: Vec<_> = (0..10)
        .flat_map(|d| (0..100).map(move |f| (d, f)))
        .map(|(d, f)| root.join(format!("quelle/o{d:02}/f{f:03}.txt")))
        .collect();
    let t = Instant::now();
    jobs.run(&FileOp::Trash { paths: files.clone() }, &p).unwrap();
    report("fileops: Dateien in den Papierkorb", files.len(), t.elapsed());
    let t = Instant::now();
    let n = jobs.trash.list().len();
    report("fileops: Papierkorb auflisten (Einträge)", n, t.elapsed());
    let t = Instant::now();
    jobs.undo().unwrap();
    report("fileops: Rückgängig (wiederherstellen)", files.len(), t.elapsed());
    assert!(files.iter().all(|f| f.exists()));

    // Fernoperation: Umlaufzeit über QUIC (loopback)
    use space_core::ipc::{Request, Response};
    use space_core::remote_ops::RemoteOp;
    use space_core::{Config, Daemon};
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let port = || std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mk = |n: &str| {
            let mut c = Config::portable(&dir.path().join(n));
            c.use_relay = false;
            c.bind_port = Some(port());
            c.device_name = n.into();
            c.live_connections = false;
            c
        };
        let pc = Daemon::start_loopback(mk("PC")).await.unwrap();
        let phone = Daemon::start_loopback(mk("Handy")).await.unwrap();
        let code = match pc.request(Request::PairInvite).await {
            Response::PairInvite { code, .. } => code,
            other => panic!("{other:?}"),
        };
        assert!(matches!(phone.request(Request::PairJoin { code }).await, Response::Paired { .. }));
        let share = std::fs::canonicalize(root.join("quelle")).unwrap();
        pc.request(Request::ShareAdd { path: share.clone(), name: Some("Q".into()), devices: None }).await;
        pc.request(Request::PairGrant {
            device: "Handy".into(),
            capability: "files.write".into(),
            folder: Some(share.clone()),
        })
        .await;
        let n = 50;
        let t = Instant::now();
        for i in 0..n {
            let (from, to) = if i % 2 == 0 { ("o00/f000.txt", "x.txt") } else { ("o00/x.txt", "f000.txt") };
            let op = RemoteOp::Rename { share: "Q".into(), path: from.into(), new_name: to.into() };
            let res = phone.request(Request::RemoteOp { device: "PC".into(), op }).await;
            assert!(matches!(res, Response::Success { .. }), "{res:?}");
        }
        report("remote: Umbenennen (Verbindung je Auftrag)", n, t.elapsed());
        pc.shutdown().await;
        phone.shutdown().await;
    });
}

#[test]
#[ignore]
fn media_thumbnails() {
    use space_core::media::{metadata, ThumbCache, ThumbSize};
    let dir = tempfile::tempdir().unwrap();
    // 12-MP-Foto (4000 × 3000), typische Handykamera
    let img = image::RgbImage::from_fn(4000, 3000, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8])
    });
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90).encode_image(&img).unwrap();
    let n = 10;
    for i in 0..n {
        std::fs::write(dir.path().join(format!("foto{i}.jpg")), &buf).unwrap();
    }
    println!("Testfoto: {:.1} MB", buf.len() as f64 / 1e6);
    let cache = ThumbCache::new(dir.path().join("thumbs"), 100 << 20);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (rss0, _) = rss_mib();
        let t = Instant::now();
        for i in 0..n {
            cache.get(&dir.path().join(format!("foto{i}.jpg")), ThumbSize::Small).await.unwrap();
        }
        report("media: Vorschaubild 12 MP → 256 px (neu)", n, t.elapsed());
        let (_, hwm) = rss_mib();
        println!("media: RAM-Spitze {hwm:.0} MiB (vorher {rss0:.0} MiB)");
        let t = Instant::now();
        let m = 1000;
        for i in 0..m {
            let t = cache.get(&dir.path().join(format!("foto{}.jpg", i % n)), ThumbSize::Small).await.unwrap();
            assert!(t.cached);
        }
        report("media: Vorschaubild aus dem Cache", m, t.elapsed());
        // Parallel angefragt (wie ein Raster in der Oberfläche); höchstens 2 decodieren gleichzeitig
        let par = std::sync::Arc::new(ThumbCache::new(dir.path().join("thumbs2"), 100 << 20));
        let t = Instant::now();
        let tasks: Vec<_> = (0..n)
            .map(|i| {
                let (c, p) = (par.clone(), dir.path().join(format!("foto{i}.jpg")));
                tokio::spawn(async move { c.get(&p, ThumbSize::Small).await.unwrap() })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        report("media: Vorschaubild parallel angefragt (neu)", n, t.elapsed());
        let (_, hwm) = rss_mib();
        println!("media: RAM-Spitze parallel {hwm:.0} MiB");
    });
    let t = Instant::now();
    for i in 0..n {
        metadata(&dir.path().join(format!("foto{i}.jpg"))).unwrap();
    }
    report("media: Metadaten (EXIF + Abmessungen)", n, t.elapsed());
}

#[test]
#[ignore]
fn streaming() {
    use space_core::ipc::{Request, Response};
    use space_core::{Config, Daemon};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mk = |n: &str| {
            let mut c = Config::portable(&dir.path().join(n));
            c.use_relay = false;
            c.device_name = n.into();
            c.live_connections = false;
            c.stream_cache_bytes = 1 << 30;
            c
        };
        let share = dir.path().join("Filme");
        std::fs::create_dir_all(&share).unwrap();
        const SIZE: usize = 64 * 1024 * 1024;
        for name in ["a.mkv", "b.mkv"] {
            let data: Vec<u8> =
                (0..SIZE as u32).map(|i| (i.wrapping_mul(2654435761) >> (13 + name.len())) as u8).collect();
            std::fs::write(share.join(name), data).unwrap();
        }
        let pc = Daemon::start_loopback(mk("PC")).await.unwrap();
        let phone = Daemon::start_loopback(mk("Handy")).await.unwrap();
        let code = match pc.request(Request::PairInvite).await {
            Response::PairInvite { code, .. } => code,
            other => panic!("{other:?}"),
        };
        assert!(matches!(phone.request(Request::PairJoin { code }).await, Response::Paired { .. }));
        pc.request(Request::ShareAdd { path: share.clone(), name: Some("Filme".into()), devices: None }).await;
        loop {
            phone.request(Request::Sync).await;
            if let Response::Catalog { remote, .. } = phone.request(Request::List).await {
                if remote.len() >= 2 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let url = |t: &str| {
            let phone = phone.clone();
            let t = t.to_string();
            async move {
                match phone.request(Request::StreamUrl { target: t }).await {
                    Response::StreamUrl(i) => i.url,
                    other => panic!("{other:?}"),
                }
            }
        };
        async fn fetch(url: &str, range: Option<(u64, u64)>) -> (Duration, Duration, usize) {
            let rest = &url["http://".len()..];
            let (hostport, path) = rest.split_at(rest.find('/').unwrap());
            let t = Instant::now();
            let mut s = tokio::net::TcpStream::connect(hostport).await.unwrap();
            let r = range.map(|(a, b)| format!("Range: bytes={a}-{b}\r\n")).unwrap_or_default();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n{r}\r\n").as_bytes())
                .await
                .unwrap();
            let mut first = [0u8; 1];
            s.read_exact(&mut first).await.unwrap();
            let ttfb = t.elapsed();
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            (ttfb, t.elapsed(), buf.len() + 1)
        }
        let a = url("Filme/a.mkv").await;
        let (ttfb, _, _) = fetch(&a, Some((32 << 20, (33 << 20) - 1))).await;
        println!("stream: Spulen in die Mitte, erstes Byte (kalt) {:>8.2} ms", ttfb.as_secs_f64() * 1e3);
        let (ttfb, total, n) = fetch(&a, None).await;
        println!(
            "stream: 64 MiB am Stück (kalt)       erstes Byte {:>6.2} ms, {:>7.1} MB/s",
            ttfb.as_secs_f64() * 1e3,
            n as f64 / total.as_secs_f64() / 1e6
        );
        let (ttfb, total, n) = fetch(&a, None).await;
        println!(
            "stream: 64 MiB am Stück (aus Cache)  erstes Byte {:>6.2} ms, {:>7.1} MB/s",
            ttfb.as_secs_f64() * 1e3,
            n as f64 / total.as_secs_f64() / 1e6
        );
        let (_, rss) = rss_mib();
        println!("stream: RAM-Spitze Testprozess (2 Daemons) {rss:.0} MiB");
        pc.shutdown().await;
        phone.shutdown().await;
    });
}

#[test]
#[ignore]
fn upload() {
    use space_core::ipc::{Request, Response};
    use space_core::{Config, Daemon};
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let mk = |n: &str| {
            let mut c = Config::portable(&dir.path().join(n));
            c.use_relay = false;
            c.device_name = n.into();
            c.live_connections = false;
            c
        };
        let share = dir.path().join("Fotos");
        std::fs::create_dir_all(&share).unwrap();
        let pc = Daemon::start_loopback(mk("PC")).await.unwrap();
        let phone = Daemon::start_loopback(mk("Handy")).await.unwrap();
        let code = match pc.request(Request::PairInvite).await {
            Response::PairInvite { code, .. } => code,
            other => panic!("{other:?}"),
        };
        assert!(matches!(phone.request(Request::PairJoin { code }).await, Response::Paired { .. }));
        pc.request(Request::ShareAdd { path: share.clone(), name: Some("Fotos".into()), devices: None }).await;
        let share = std::fs::canonicalize(&share).unwrap();
        pc.request(Request::PairGrant {
            device: "Handy".into(),
            capability: "files.write".into(),
            folder: Some(share.clone()),
        })
        .await;
        let wait_done = |n: usize| {
            let phone = phone.clone();
            async move {
                let start = Instant::now();
                loop {
                    if let Response::Uploads { items } = phone.request(Request::UploadList).await {
                        if items.iter().filter(|u| u.state == "done").count() >= n {
                            return;
                        }
                        assert!(!items.iter().any(|u| u.state == "failed"), "{items:?}");
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    if start.elapsed() > Duration::from_secs(120) {
                        if let Response::Uploads { items } = phone.request(Request::UploadList).await {
                            let done = items.iter().filter(|u| u.state == "done").count();
                            println!("HÄNGT: {done} fertig");
                            for u in items.iter().rev().filter(|u| u.state != "done").take(2) {
                                println!("HÄNGT: {u:?}");
                            }
                        }
                        panic!("Zeitüberschreitung");
                    }
                }
            }
        };
        // Ein großes Video
        let video = dir.path().join("video.mp4");
        let data: Vec<u8> = (0..64 * 1024 * 1024u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
        std::fs::write(&video, &data).unwrap();
        let t = Instant::now();
        phone
            .request(Request::Upload { device: "PC".into(), share: "Fotos".into(), dest_dir: "".into(), path: video })
            .await;
        wait_done(1).await;
        let d = t.elapsed();
        println!(
            "upload: 64 MiB Video                {:>8.0} ms  {:>6.1} MB/s",
            d.as_secs_f64() * 1e3,
            64.0 * 1.048576 / d.as_secs_f64()
        );
        // 100 Fotos à 3 MB
        let photos: Vec<_> = (0..100)
            .map(|i| {
                let p = dir.path().join(format!("IMG_{i:04}.jpg"));
                std::fs::write(&p, vec![i as u8; 3_000_000]).unwrap();
                p
            })
            .collect();
        let t = Instant::now();
        for p in &photos {
            phone
                .request(Request::Upload {
                    device: "PC".into(),
                    share: "Fotos".into(),
                    dest_dir: "".into(),
                    path: p.clone(),
                })
                .await;
        }
        wait_done(101).await;
        let d = t.elapsed();
        report("upload: Fotos à 3 MB (je Datei)", photos.len(), d);
        println!("upload: Fotos gesamt {:>6.1} MB/s", 300.0 / d.as_secs_f64());
        pc.shutdown().await;
        phone.shutdown().await;
    });
}

#[test]
#[ignore]
fn share_links() {
    use space_core::ipc::{Request, Response};
    use space_core::links::crypto::{seal_all, LinkKeys};
    use space_core::{Config, Daemon};
    // Passwort-Ableitung (600 000 Runden PBKDF2) und reine Verschlüsselung
    let t = Instant::now();
    let keys = LinkKeys::derive(&[1; 32], "0123456789abcdef0123456789abcdef", Some("passwort123"));
    println!("links: Schlüssel mit Passwort ableiten   {:>8.0} ms", t.elapsed().as_secs_f64() * 1e3);
    let plain = vec![7u8; 64 * 1024 * 1024];
    let t = Instant::now();
    let enc = seal_all(&keys, &plain).unwrap();
    let d = t.elapsed();
    println!(
        "links: 64 MiB verschlüsseln            {:>8.0} ms  {:>6.0} MB/s",
        d.as_secs_f64() * 1e3,
        67.1 / d.as_secs_f64()
    );
    drop(enc);
    // Ende-zu-Ende: Link erstellen gegen einen lokalen S3-Ersatz (Netz/Platte des Speichers zählen nicht)
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        use http_body_util::BodyExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(|req: hyper::Request<hyper::body::Incoming>| async move {
                        let _ = req.into_body().collect().await;
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(http_body_util::Full::new(
                            bytes::Bytes::new(),
                        )))
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(tcp), svc)
                        .await;
                });
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config::portable(&dir.path().join("snap"));
        c.use_relay = false;
        c.live_connections = false;
        let d = Daemon::start_loopback(c).await.unwrap();
        d.request(Request::LinkSetup(space_core::links::LinkSettings {
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
        let file = dir.path().join("video.mp4");
        std::fs::write(&file, &plain).unwrap();
        let t = Instant::now();
        let res = d.request(Request::LinkCreate { path: file, days: None, max_downloads: None, password: None }).await;
        assert!(matches!(res, Response::Link(_)), "{res:?}");
        let el = t.elapsed();
        println!(
            "links: Link für 64 MiB erstellen        {:>8.0} ms  {:>6.0} MB/s",
            el.as_secs_f64() * 1e3,
            67.1 / el.as_secs_f64()
        );
        let (_, hwm) = rss_mib();
        println!("links: RAM-Spitze Testprozess {hwm:.0} MiB (enthält 64 MiB Testdaten + Verschlüsselungstest)");
        d.shutdown().await;
    });
}

#[test]
#[ignore]
fn storage_analysis() {
    use space_core::analysis::{duplicates, space_usage};
    use space_core::index::LocalIndex;
    let dir = tempfile::tempdir().unwrap();
    let idx = LocalIndex::open(&dir.path().join("index.db"), vec![]).unwrap();
    let syn = idx.add_root(std::path::Path::new("/synthetisch")).unwrap();
    idx.insert_synthetic(syn, 10_000, 100).unwrap();
    let t = Instant::now();
    let u = space_usage(&idx, std::path::Path::new("/synthetisch"), 30).unwrap();
    report("analyse: Speicher über 1,1 Mio. Einträge", 1, t.elapsed());
    assert_eq!(u.files, 1_000_000);
    let t = Instant::now();
    space_usage(&idx, std::path::Path::new("/synthetisch/Gruppe 7"), 30).unwrap();
    report("analyse: Speicher eines Unterordners (10 000)", 1, t.elapsed());

    // Duplikate: 1 000 echte Dateien à 256 KB, davon 250 Paare
    let real = dir.path().join("echt");
    std::fs::create_dir_all(&real).unwrap();
    for i in 0..1000u32 {
        let content: Vec<u8> = (0..256 * 1024u32).map(|j| ((j ^ (i / 4 * 7919)) % 251) as u8).collect();
        std::fs::write(real.join(format!("f{i:04}.bin")), content).unwrap();
    }
    let root = idx.add_root(&real).unwrap();
    idx.rebuild_root(root, &real).unwrap();
    let t = Instant::now();
    let r = duplicates(&idx, Some(&real), 1, 10_000, |_, _| {}).unwrap();
    let d = t.elapsed();
    report("analyse: Duplikate, erster Lauf (Dateien)", 1000, d);
    println!(
        "analyse: {} Gruppen, {:.0} MB gelesen, {:.0} MB/s",
        r.groups.len(),
        r.hashed_bytes as f64 / 1e6,
        r.hashed_bytes as f64 / 1e6 / d.as_secs_f64()
    );
    let t = Instant::now();
    let r2 = duplicates(&idx, Some(&real), 1, 10_000, |_, _| {}).unwrap();
    report("analyse: Duplikate, zweiter Lauf (Cache)", 1000, t.elapsed());
    assert_eq!(r2.hashed_bytes, 0);
    assert_eq!(r.groups.len(), 250);
}
