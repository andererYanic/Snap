//! Snap Drop: Angebote, Namen und Vorschaubilder fremder Geräte.
//! Invarianten:
//! - nie eine Panik;
//! - bereinigte Dateinamen: nicht leer, ≤ 255 Byte, ohne `/ \ :`, ohne Steuer- und
//!   Formatzeichen, nicht `.`/`..`, beginnen nicht mit `.`;
//! - geprüfte Dateiliste: 1–1000 Einträge, Gesamtgröße = Summe ohne Überlauf;
//! - Anzeigenamen: nicht leer, ≤ 64 Byte, ohne Steuerzeichen;
//! - Vorschaubild: Ergebnis ist ein PNG mit höchstens 64 × 64 Pixeln.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::drop::wire::{self, Card, OfferFile, Probe};

fn check_name(n: &str) {
    assert!(!n.is_empty() && n.len() <= wire::MAX_FILE_NAME, "{n:?}");
    assert!(!n.starts_with('.') && n != "..");
    assert!(!n.contains(['/', '\\', ':']), "{n:?}");
    assert!(!n.chars().any(|c| c.is_control() || matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF)));
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    match sel % 4 {
        0 => {
            if let Ok(files) = serde_json::from_slice::<Vec<OfferFile>>(rest) {
                if let Ok((valid, total)) = wire::validate_files(&files) {
                    assert!(!valid.is_empty() && valid.len() <= wire::MAX_FILES);
                    assert_eq!(valid.iter().map(|f| f.size).sum::<u64>(), total);
                    for f in &valid {
                        check_name(&f.name);
                        assert!(f.mime.len() <= wire::MAX_MIME);
                    }
                }
            }
        }
        1 => {
            let s = String::from_utf8_lossy(rest);
            if let Some(n) = wire::sanitize_file_name(&s) {
                check_name(&n);
                assert_eq!(wire::sanitize_file_name(&n).as_deref(), Some(n.as_str()), "stabil");
            }
            let d = wire::display_name(&s);
            assert!(!d.is_empty() && d.len() <= wire::MAX_DISPLAY_NAME && !d.chars().any(char::is_control));
        }
        2 => {
            let b64 = data_encoding::BASE64.encode(rest);
            if let Ok(png) = wire::decode_preview(&b64) {
                let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
                assert!(img.width() <= wire::MAX_PREVIEW_EDGE && img.height() <= wire::MAX_PREVIEW_EDGE);
            }
        }
        _ => {
            if let Ok(p) = serde_json::from_slice::<Probe>(rest) {
                if let Ok(tags) = p.tag_bytes() {
                    assert!(tags.len() <= wire::MAX_TAGS);
                }
            }
            let _ = serde_json::from_slice::<Card>(rest);
        }
    }
});
