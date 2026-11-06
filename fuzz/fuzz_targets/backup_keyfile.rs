//! Schlüsseldateien vom Sicherungsziel (fremde Quelle). Invarianten: Was der Parser annimmt,
//! hat gültige Längen und Rundenzahlen; Entpacken mit beliebigem Geheimnis stürzt nie ab und
//! liefert nur bei korrekter Authentifizierung einen Schlüssel.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::backup::repo::{parse_keyfile, unwrap_key};

fuzz_target!(|data: &[u8]| {
    let Ok(k) = parse_keyfile(data) else { return };
    assert!(k.kind == "password" || k.kind == "recovery");
    if k.kind == "password" {
        assert!((100_000..=10_000_000).contains(&k.rounds));
        return; // PBKDF2 mit vielen Runden ist für das Fuzzing zu langsam
    }
    assert_eq!(k.rounds, 0);
    // Beliebiger „Wiederherstellungsschlüssel“: muss scheitern (AEAD), darf nicht abstürzen.
    assert!(unwrap_key(&k, "AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA-AAAA").is_err());
});
