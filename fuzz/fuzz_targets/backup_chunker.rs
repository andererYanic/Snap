//! Inhaltsaufteilung der Sicherungen. Invarianten: Die Stücke ergeben zusammen genau die
//! Eingabe, keines ist größer als MAX, nur das letzte darf kleiner als MIN sein, und der
//! Schnittpunkt hängt nur vom Inhalt ab (gleiche Eingabe → gleiche Stücke).
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::backup::chunker::{cut_point, Chunker, MAX, MIN};

fuzz_target!(|data: &[u8]| {
    // Kleine Eingaben auf Stückgröße strecken, damit die Schnittlogik wirklich läuft.
    // Meist bis 1,5 MB (über MIN und AVG), gelegentlich über MAX hinaus.
    let cap = if data.first().is_some_and(|b| b % 16 == 0) { 9 << 20 } else { 3 << 19 };
    let input: Vec<u8> = if data.len() < 64 { data.to_vec() } else { data.iter().cycle().take((data.len() * 4096).min(cap)).copied().collect() };
    let chunks: Vec<Vec<u8>> = Chunker::new(&input[..]).collect::<std::io::Result<_>>().unwrap();
    assert_eq!(chunks.concat(), input);
    for (i, c) in chunks.iter().enumerate() {
        assert!(c.len() <= MAX);
        assert!(!c.is_empty());
        if i + 1 < chunks.len() {
            assert!(c.len() > MIN);
        }
    }
    let n = cut_point(&input);
    assert!(n <= input.len() && n <= MAX);
    assert_eq!(cut_point(&input), n, "deterministisch");
});
