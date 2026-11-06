//! Eingaben der Foto-Zeitachse von außen: EXIF-Datum (aus Dateien) und Cursor (über IPC).
//! Invarianten: Ein erkanntes Datum liegt in 1900–2199 und ergibt formatiert wieder denselben
//! Wert; ein erkannter Cursor ergibt formatiert wieder denselben Cursor.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::photos::{format_cursor, parse_cursor, parse_taken};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    if let Some(t) = parse_taken(s) {
        let d = chrono::DateTime::from_timestamp(t, 0).expect("gültige Zeit");
        use chrono::Datelike;
        assert!((1900..2200).contains(&d.year()), "{s:?} -> {t}");
        let again = d.format("%Y-%m-%d %H:%M:%S").to_string();
        assert_eq!(parse_taken(&again), Some(t));
    }
    if let Some((a, b)) = parse_cursor(s) {
        assert_eq!(parse_cursor(&format_cursor(a, b)), Some((a, b)));
    }
});
