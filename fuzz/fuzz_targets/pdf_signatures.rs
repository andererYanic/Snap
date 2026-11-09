//! PDF-Signaturen prüfen und PDFs markieren (feindliche Dateien). Invarianten: kein Absturz;
//! zufällige Daten ergeben nie eine gültige Signatur; eine als gültig gemeldete Signatur hat
//! keinen Problemhinweis; Markieren lässt das Original als Anfang der Ausgabe unverändert.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::pdfsign::{self, cms, pdf};

fuzz_target!(|data: &[u8]| {
    // 1. Rohe CMS-Struktur: nie gültig über fremde Bytes (Fälschen ist ausgeschlossen)
    if let Ok(c) = cms::verify(data, &[b"inhalt"]) {
        assert!(!c.valid, "zufällige CMS-Daten als gültig erkannt");
    }
    // 2. Als PDF (mit Kopf, damit der Parser weit kommt)
    let mut doc = b"%PDF-1.7\n".to_vec();
    doc.extend_from_slice(data);
    if let Ok(reports) = pdfsign::verify(&doc, None) {
        for r in reports {
            if r.valid {
                assert!(r.problem.is_none(), "{r:?}");
            }
            assert!(!r.own);
        }
    }
    // 3. Markieren: Original bleibt erhalten, Ergebnis ist wieder lesbar
    let layer = pdf::Layer {
        page: 0,
        strokes: vec![vec![(0.1, 0.1), (0.9, 0.9)]],
        color: [1.0, 0.9, 0.0],
        width: 0.02,
        opacity: 0.4,
        multiply: true,
    };
    if let Ok(out) = pdf::draw(&doc, &[layer]) {
        assert!(out.starts_with(&doc));
        assert!(out.len() > doc.len());
    }
});
