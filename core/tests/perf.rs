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
    std::fs::remove_dir_all(&tree).unwrap();

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
