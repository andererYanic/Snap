//! Ledger-Einträge kommen von anderen Geräten (ggf. kompromittiert oder weitergereicht).
//! Invariante: Was `verify`/`validate_rel_path` akzeptiert, kann beim Ablegen nie aus
//! einem Zielordner herausführen.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::ledger::{validate_rel_path, validate_share_name, LedgerEntry};
use std::path::{Component, Path};

fn assert_contained(rel: &str) {
    let p = Path::new(rel);
    assert!(!p.is_absolute(), "absolut: {rel:?}");
    for c in p.components() {
        assert!(matches!(c, Component::Normal(_)), "Navigation in {rel:?}");
    }
    let joined = Path::new("/ziel").join(p);
    assert!(joined.starts_with("/ziel"));
    assert!(!rel.contains('\0'));
}

fuzz_target!(|data: &[u8]| {
    if let Ok(entry) = serde_json::from_slice::<LedgerEntry>(data) {
        if entry.verify().is_ok() && !entry.rel_path.is_empty() {
            assert_contained(&entry.rel_path);
            assert!(validate_share_name(&entry.share).is_ok());
        }
    }
    if let Ok(list) = serde_json::from_slice::<Vec<LedgerEntry>>(data) {
        let _ = space_core::ledger::verify_each(&list);
    }
    if let Ok(s) = std::str::from_utf8(data) {
        if validate_rel_path(s).is_ok() {
            assert_contained(s);
        }
        if validate_share_name(s).is_ok() {
            assert!(!s.contains('/') && s != ".." && s != ".");
        }
    }
});
