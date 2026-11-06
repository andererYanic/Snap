//! `.trashinfo`-Dateien (von beliebigen Programmen geschrieben). Invarianten: kein NUL im Pfad,
//! Wiederherstellungsziele sind absolut, ohne `..`/`.`, und bei Laufwerks-Papierkörben
//! innerhalb des Laufwerks; Kodieren und Dekodieren sind verlustfrei.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::fileops::trash::{parse_trashinfo, percent_encode, restore_target};
use std::path::{Component, Path};

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    if let Some(info) = parse_trashinfo(s) {
        assert!(!info.path.is_empty() && !info.path.contains(&0));
        assert!(info.deletion_date.chars().all(|c| !c.is_control()));
        let top = Path::new("/media/usb");
        for (topdir, target) in [(None, restore_target(&info, None)), (Some(top), restore_target(&info, Some(top)))] {
            if let Some(t) = target {
                assert!(t.is_absolute());
                assert!(t.file_name().is_some());
                assert!(t.components().all(|c| !matches!(c, Component::ParentDir | Component::CurDir)), "{t:?}");
                if let Some(top) = topdir {
                    assert!(t.starts_with(top), "{t:?}");
                }
            }
        }
        // Rundreise: kodieren → parsen ergibt denselben Pfad
        let again = parse_trashinfo(&format!("[Trash Info]\nPath={}\n", percent_encode(&info.path))).unwrap();
        assert_eq!(again.path, info.path);
    }
});
