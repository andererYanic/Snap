//! Tresor-Dateien von der Platte (könnten verändert sein): entschlüsseln.
//! Invarianten: nie eine Panik; entweder ein Fehler oder – nur wenn die Datei unverändert ist –
//! genau der ursprüngliche Inhalt. Jede Veränderung (Bits, Länge, Blöcke) wird erkannt.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::vault::{decrypt_stream, encrypt_stream};
use std::io::Cursor;
use std::sync::OnceLock;

const KEY: [u8; 32] = [9; 32];
const ID: [u8; 16] = [3; 16];

fn original() -> &'static (Vec<u8>, Vec<u8>) {
    static O: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
    O.get_or_init(|| {
        let plain: Vec<u8> = (0..150_000u32).map(|i| (i % 253) as u8).collect();
        let mut enc = Vec::new();
        encrypt_stream(&KEY, &ID, &mut Cursor::new(&plain), &mut enc).unwrap();
        (plain, enc)
    })
}

fuzz_target!(|data: &[u8]| {
    let (plain, enc) = original();
    // Eingabe = Veränderungen an der echten Datei: (Position, XOR) Paare und optional neue Länge
    let mut file = enc.clone();
    let mut it = data.chunks_exact(5);
    for c in it.by_ref() {
        let pos = u32::from_le_bytes([c[0], c[1], c[2], c[3]]) as usize;
        if pos == u32::MAX as usize {
            file.truncate(c[4] as usize * 1000);
        } else if !file.is_empty() {
            let p = pos % file.len();
            file[p] ^= c[4];
        }
    }
    file.extend_from_slice(it.remainder());
    let mut out = Vec::new();
    match decrypt_stream(&KEY, &ID, &mut Cursor::new(&file), file.len() as u64, &mut out) {
        Ok(n) => {
            assert_eq!(&file, enc, "nur die unveränderte Datei darf gelingen");
            assert_eq!(&out, plain);
            assert_eq!(n as usize, plain.len());
        }
        Err(_) => assert_ne!(&file, enc),
    }
    // Rohe Eingabe als Datei: darf nie gelingen (falscher Kopf oder Prüfsumme)
    assert!(decrypt_stream(&KEY, &ID, &mut Cursor::new(data), data.len() as u64, &mut Vec::new()).is_err());
});
