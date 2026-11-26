//! Weitere Dokumentformate (ODT/ODS/ODP, PPTX, XLSX, EPUB) und Archiv-Inhaltslisten.
//! Invarianten:
//! - nie eine Panik, kein unbegrenzter Speicher;
//! - höchstens MAX_BLOCKS Blöcke; Tabellen höchstens MAX_ROWS × MAX_COLS, Zellen begrenzt, alle
//!   Zeilen gleich breit; keine Steuerzeichen im Text;
//! - Archive: höchstens MAX_ENTRIES Einträge, Namen ohne „..“, ohne führendes „/“.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::docs::{self, MAX_COLS, MAX_ENTRIES, MAX_ROWS};
use space_core::docx::{DocBlock, MAX_BLOCKS};

fn check(blocks: &[DocBlock]) {
    assert!(blocks.len() <= MAX_BLOCKS);
    for b in blocks {
        match b {
            DocBlock::Table { rows } => {
                assert!(rows.len() <= MAX_ROWS && !rows.is_empty());
                let w = rows[0].len();
                assert!(w <= MAX_COLS);
                assert!(rows.iter().all(|r| r.len() == w), "gleich breite Zeilen");
                assert!(rows.iter().flatten().all(|c| c.len() <= docs::MAX_CELL + 4 && !c.chars().any(char::is_control)));
            }
            DocBlock::Heading { inline, .. } | DocBlock::Paragraph { inline } => {
                assert!(inline.iter().all(|s| !s.text.chars().any(char::is_control)));
            }
            _ => {}
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&sel, rest)) = data.split_first() else { return };
    match sel % 4 {
        0 => check(&docs::parse_odf(rest).blocks),
        1 => {
            let p = docs::drawing_paragraphs(rest);
            assert!(p.len() <= MAX_BLOCKS);
        }
        _ => {
            let ext = ["odt", "ods", "pptx", "xlsx", "epub", "zip", "tar", "tgz"][(sel as usize / 4) % 8];
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("probe.{ext}"));
            std::fs::write(&path, rest).unwrap();
            if matches!(ext, "zip" | "tar" | "tgz") {
                if let Ok(l) = docs::list_archive(&path) {
                    assert!(l.entries.len() <= MAX_ENTRIES);
                    for e in &l.entries {
                        assert!(!e.name.is_empty() && !e.name.starts_with('/'));
                        assert!(!e.name.split('/').any(|s| s == ".." || s == "."), "{}", e.name);
                    }
                }
            } else if let Ok(d) = docs::read_document(&path, None) {
                check(&d.blocks);
            }
        }
    }
});
