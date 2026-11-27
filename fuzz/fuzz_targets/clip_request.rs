//! Zwischenablage zwischen eigenen Geräten: Nachrichten gekoppelter (evtl. manipulierter) Geräte.
//! Invarianten: nie eine Panik; angenommener Text ist nicht leer, höchstens 64 KB groß, enthält
//! keine Steuerzeichen außer Zeilenumbruch und Tabulator, und die Prüfung ist stabil.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::clipboard::{decode, MAX_TEXT};

fuzz_target!(|data: &[u8]| {
    if let Some(t) = decode(data) {
        assert!(!t.trim().is_empty());
        assert!(t.len() <= MAX_TEXT);
        assert!(t.chars().all(|c| c == '\n' || c == '\t' || !c.is_control()));
        let again = serde_json::json!({ "v": 1, "text": t }).to_string();
        assert_eq!(decode(again.as_bytes()).as_deref(), Some(t.as_str()), "stabil");
    }
});
