//! Adressen fremder Android-Apps („Öffnen mit“, „Ordner öffnen“).
//! Invarianten: nie eine Panik; ein Ergebnis ist ein absoluter Pfad ohne `.`/`..`/leere Teile,
//! liegt unter dem gemeinsamen Speicher bzw. `/storage`, und als `file://`-Adresse kodiert
//! ergibt es wieder denselben Pfad (stabil) – sofern die neu kodierte Adresse nicht länger als
//! erlaubt ist (Prozentkodierung verdreifacht Sonderzeichen; gefunden per Fuzzing).
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::fileops::trash::percent_encode;
use space_core::intent::{uri_to_path, MAX_URI};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let Ok(uri) = std::str::from_utf8(data) else { return };
    let root = Path::new("/storage/emulated/0");
    let Some(path) = uri_to_path(uri, root) else { return };
    let s = path.to_str().expect("UTF-8");
    assert!(path.is_absolute(), "{s}");
    assert!(s[1..].split('/').all(|seg| !seg.is_empty() && seg != "." && seg != ".."), "{s}");
    assert!(!s.contains('\0'));
    // Nur file:// und raw: liefern beliebige absolute Pfade; die Prüfung `checked` grenzt danach
    // auf den gemeinsamen Speicher ein. Dokument-Adressen bleiben immer darin.
    if !uri.starts_with("file://") && !uri.contains("raw%3A") && !uri.contains("raw:") {
        assert!(path.starts_with(root) || path.starts_with("/storage"), "{s} aus {uri}");
    }
    let again = format!("file://{}", percent_encode(path.as_os_str().as_bytes()));
    if again.len() <= MAX_URI {
        assert_eq!(uri_to_path(&again, root).as_deref(), Some(path.as_path()), "stabil: {again}");
    } else {
        assert_eq!(uri_to_path(&again, root), None, "zu lang wird abgewiesen");
    }
});
