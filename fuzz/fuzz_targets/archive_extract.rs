//! Archive entpacken (ZIP, tar, tar.gz): beliebige Bytes als Archiv.
//! Invarianten:
//! - nie eine Panik;
//! - Erfolg: genau ein neuer Ordner neben dem Archiv, alles darin liegt innerhalb, keine
//!   Verknüpfungen, keine Rechte über 0o777 (kein setuid), Summe der Dateigrößen = gemeldete Bytes
//!   und ≤ Grenze, Anzahl Dateien ≤ Grenze;
//! - Fehler: kein Ordner und kein Arbeitsordner bleibt übrig, nichts außerhalb verändert.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::fileops::archive::{extract, Limits, Password};
use space_core::fileops::ops::Progress;
use std::os::unix::fs::PermissionsExt;

const LIMITS: Limits = Limits { max_entries: 200, max_bytes: 4 * 1024 * 1024, max_ratio: 1000 };

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else { return };
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("ordner");
    std::fs::create_dir(&dir).unwrap();
    // Verlockendes Ziel außerhalb: darf nie verändert werden
    std::fs::write(base.path().join("draussen.txt"), b"unberuehrt").unwrap();
    let archive = dir.join(if flags & 1 == 0 { "probe.zip" } else { "probe.tar.gz" });
    std::fs::write(&archive, rest).unwrap();
    let pw = (flags & 2 != 0).then(|| Password("pw".into()));

    let result = extract(&archive, pw.as_ref(), Some(LIMITS), &Progress::default());

    assert_eq!(std::fs::read(base.path().join("draussen.txt")).unwrap(), b"unberuehrt");
    let mut top: Vec<_> = std::fs::read_dir(base.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    top.sort();
    assert_eq!(top, ["draussen.txt", "ordner"], "nichts neben dem Ordner angelegt");
    let siblings: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p != &archive).collect();
    match result {
        Err(_) => assert!(siblings.is_empty(), "Fehler hinterlässt nichts: {siblings:?}"),
        Ok(x) => {
            assert_eq!(siblings, vec![x.dir.clone()], "genau ein neuer Ordner");
            assert!(!x.dir.file_name().unwrap().to_string_lossy().starts_with(".snap-"));
            let (mut files, mut bytes) = (0u64, 0u64);
            for e in walkdir::WalkDir::new(&x.dir).follow_links(false) {
                let e = e.unwrap();
                let m = e.path().symlink_metadata().unwrap();
                assert!(!m.file_type().is_symlink(), "keine Verknüpfungen: {}", e.path().display());
                assert!(e.path().starts_with(&x.dir));
                assert_eq!(m.permissions().mode() & 0o7000, 0, "keine Sonderrechte");
                if m.is_file() {
                    files += 1;
                    bytes += m.len();
                }
            }
            assert_eq!((files, bytes), (x.files, x.bytes));
            assert!(bytes <= LIMITS.max_bytes && files <= LIMITS.max_entries);
        }
    }
});
