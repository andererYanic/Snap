//! RAW-Fotos von außen: eingebettete JPEG-Vorschau suchen.
//! Invarianten: nie eine Panik, kein Endlos-Lauf (IFD-Schleifen), die gefundene Vorschau liegt
//! vollständig in der Datei, ist höchstens MAX_PREVIEW groß, beginnt mit einem JPEG-Kopf und die
//! Ausrichtung ist ein gültiger EXIF-Wert.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::raw::{find_preview, read_preview, MAX_PREVIEW};
use std::io::Cursor;

fuzz_target!(|data: &[u8]| {
    let size = data.len() as u64;
    if let Some(p) = find_preview(&mut Cursor::new(data), size) {
        assert!(p.offset.checked_add(p.len).is_some_and(|end| end <= size));
        assert!(p.len <= MAX_PREVIEW);
        assert!((1..=8).contains(&p.orientation));
        let (bytes, o) = read_preview(&mut Cursor::new(data), size).expect("gefunden heißt lesbar");
        assert!(bytes.starts_with(&[0xFF, 0xD8, 0xFF]));
        assert_eq!(bytes.len() as u64, p.len);
        assert_eq!(o, p.orientation);
    }
});
