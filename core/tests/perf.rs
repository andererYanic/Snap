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
            };
            e.signature = e.sign(key);
            e
        })
        .collect()
}

fn report(name: &str, n: usize, d: Duration) {
    println!("{name:<44} {n:>7} ops  {:>9.2} ms  {:>9.1} µs/op", d.as_secs_f64() * 1e3, d.as_secs_f64() * 1e6 / n as f64);
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
        db.record_own_entry(&origin, id, &hash(100_000 + i), &format!("f{i}"), 1, 0, false, &move |e: &LedgerEntry| e.sign(&k))
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
