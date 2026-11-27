//! Volltext: Text aus PDFs von außen lesen.
//! Invarianten: nie eine Panik (auch nicht im PDF-Leser – abgefangen), Ergebnis höchstens
//! MAX_TEXT Zeichen, nicht leer, ohne Steuerzeichen außer Zeilenumbruch, und die Abfrage-
//! Übersetzung für FTS5 liefert nur Phrasen in Anführungszeichen.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::content::{fts_query, pdf_text, MAX_TEXT};

fuzz_target!(|data: &[u8]| {
    if let Some(t) = pdf_text(data) {
        assert!(t.chars().count() <= MAX_TEXT);
        assert!(!t.trim().is_empty());
        assert!(t.chars().all(|c| c == '\n' || !c.is_control()));
    }
    if let Ok(s) = std::str::from_utf8(data) {
        if let Some(q) = fts_query(s) {
            assert!(q.starts_with('"') && q.ends_with("\"*"), "{q}");
            // Ohne die verdoppelten Anführungszeichen bleiben nur Paare aus Phrasen-Grenzen
            assert_eq!(q.replace("\"\"", "").matches('"').count() % 2, 0, "{q}");
        }
    }
});
