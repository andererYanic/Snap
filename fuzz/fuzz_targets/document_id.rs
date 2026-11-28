//! Kennungen der Dokument-Quelle „Snap“ (System-Dateiauswahl), wie sie von außen zurückkommen.
//! Invarianten: nie eine Panik; angenommen wird nur die kanonische Form (erneut erzeugt genau
//! dieselbe Zeichenkette) mit gültigen Teilen – Node-ID/Hash hexadezimal, relative Pfade ohne
//! `.`/`..`/leere Teile, Namen ohne `/`.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::intent::{doc_id_decode, doc_id_encode, doc_id_valid, DocId};

fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else { return };
    let Some(id) = doc_id_decode(raw) else { return };
    assert!(doc_id_valid(&id), "{id:?}");
    assert_eq!(doc_id_encode(&id).as_deref(), Some(raw), "kanonisch");
    let rel_ok = |s: &str| s.is_empty() || s.split('/').all(|p| !p.is_empty() && p != "." && p != "..");
    match &id {
        DocId::Drop(rel) => assert!(rel_ok(rel) && !rel.starts_with('/')),
        DocId::Dir { share, dir, .. } => assert!(!share.is_empty() && rel_ok(share) && rel_ok(dir)),
        DocId::RemoteFile { name, .. } => assert!(!name.contains('/') && name != "." && name != ".."),
        _ => {}
    }
});
