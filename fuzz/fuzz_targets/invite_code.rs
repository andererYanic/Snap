//! Einladungscodes kommen vom Nutzer (kopiert, evtl. manipuliert): Decoder darf nie
//! abstürzen, und alles, was er akzeptiert, muss sich verlustfrei neu kodieren lassen.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::pairing::InviteCode;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(code) = InviteCode::decode(s) {
            assert!(code.addrs.len() <= 16);
            let again = InviteCode::decode(&code.encode()).expect("eigene Kodierung muss lesbar sein");
            assert_eq!(again, code);
        }
    }
    // Auch rohe Bytes hinter dem Präfix
    let s = format!("snap1-{}", String::from_utf8_lossy(data));
    let _ = InviteCode::decode(&s);
});
