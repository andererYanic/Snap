//! `Range`-Köpfe des lokalen Streaming-Servers. Invarianten: Teilbereiche liegen immer
//! vollständig in der Datei und sind nicht leer; ohne Kopf gibt es immer die ganze Datei.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::stream::{host_ok, parse_range, RangeReq};

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let size = u64::from_le_bytes(data[..8].try_into().unwrap()) >> (data[0] % 64);
    let Ok(h) = std::str::from_utf8(&data[8..]) else { return };
    match parse_range(Some(h), size) {
        RangeReq::Partial(r) => assert!(r.start < r.end && r.end <= size, "{h:?} {size} {r:?}"),
        RangeReq::Full | RangeReq::Unsatisfiable => {}
    }
    assert_eq!(parse_range(None, size), RangeReq::Full);
    // Host-Prüfung: nur exakt die lokalen Namen
    if host_ok(Some(h), 8080) {
        assert!(h == "127.0.0.1:8080" || h == "localhost:8080");
    }
});
