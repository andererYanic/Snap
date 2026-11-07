//! DOCX-Leseansicht: beliebige Bytes als Word-Datei (bzw. als document.xml in einem gültigen
//! ZIP). Invarianten: nie eine Panik; höchstens MAX_BLOCKS Blöcke; Links nur http(s)/mailto;
//! Bilder nur im Bildordner und höchstens MAX_IMAGES; außerhalb wird nichts angelegt.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::docx::{read_docx, DocBlock, MAX_BLOCKS, MAX_IMAGES};
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else { return };
    let base = tempfile::tempdir().unwrap();
    let path = base.path().join("probe.docx");
    let bytes = if mode & 1 == 0 {
        rest.to_vec()
    } else {
        // Gültiges ZIP mit fremdem XML: prüft vor allem den XML-Teil
        let (doc, rels) = rest.split_at(rest.len() / 2);
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opt = zip::write::SimpleFileOptions::default();
        w.start_file("word/document.xml", opt).unwrap();
        w.write_all(doc).unwrap();
        w.start_file("word/_rels/document.xml.rels", opt).unwrap();
        w.write_all(rels).unwrap();
        w.start_file("word/media/a.png", opt).unwrap();
        w.write_all(b"PNG").unwrap();
        w.finish().unwrap().into_inner()
    };
    std::fs::write(&path, &bytes).unwrap();
    let images = base.path().join("bilder");
    let Ok(doc) = read_docx(&path, Some(&images)) else {
        assert!(!images.exists() || std::fs::read_dir(&images).unwrap().next().is_none());
        return;
    };
    assert!(doc.blocks.len() <= MAX_BLOCKS);
    let mut n_images = 0;
    for b in &doc.blocks {
        let spans: &[space_core::docx::Span] = match b {
            DocBlock::Heading { inline, .. } | DocBlock::Paragraph { inline } => inline,
            DocBlock::Image { file } => {
                n_images += 1;
                assert!(file.starts_with(&images), "{}", file.display());
                assert!(file.is_file());
                &[]
            }
            _ => &[],
        };
        for s in spans {
            if let Some(l) = &s.link {
                let l = l.to_ascii_lowercase();
                assert!(l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:"), "{l}");
            }
        }
    }
    assert!(n_images <= MAX_IMAGES);
    let mut top: Vec<_> = std::fs::read_dir(base.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    top.sort();
    assert!(top.iter().all(|n| n == "probe.docx" || n == "bilder"), "{top:?}");
});
