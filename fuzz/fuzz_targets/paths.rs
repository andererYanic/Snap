//! Pfad- und Namensprüfungen für Eingaben von außen.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::api::auth::resolve_for_check;
use space_core::api::transfer::sanitize_file_name;
use space_core::database::normalize_hash_prefix;
use std::path::{Component, Path};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    // Hash-Präfixe: nur Hex, keine SQL-Platzhalter
    if let Ok(h) = normalize_hash_prefix(s) {
        assert!(h.len() >= 6 && h.len() <= 64);
        assert!(h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
    }
    // Dateinamen aus fremden Quellen: nie ein Pfad, nie versteckt/navigierend
    let name = sanitize_file_name(s);
    assert!(!name.is_empty());
    assert!(!name.contains('/'), "{name:?}");
    assert!(!name.starts_with('.'), "{name:?}");
    // Pfadprüfung der Rechte: Ergebnis ist absolut und ohne Navigation
    if let Ok(resolved) = resolve_for_check(Path::new(s)) {
        assert!(resolved.is_absolute());
        assert!(resolved.components().all(|c| !matches!(c, Component::ParentDir | Component::CurDir)));
    }
});
