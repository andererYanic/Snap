//! Wecken im Hintergrund (Android): Ein Sender funkt per Bluetooth LE kurz ein **Weck-Tag**,
//! der Bluetooth-Chip des Empfängers filtert danach, ohne dass Snap läuft.
//!
//! - Je Kontakt gibt es einen Weck-Schlüssel, abgeleitet aus dem gemeinsamen Kontakt-Geheimnis
//!   (Einwegableitung – er erlaubt weder Anmelden noch Entschlüsseln).
//! - Das Tag ist HMAC-SHA256(Weck-Schlüssel, „snap-wake-v1“ ‖ Epoche)[..8], Epoche = Unix-Zeit /
//!   15 Minuten. Für Dritte ist es Zufall und wechselt alle 15 Minuten (nicht verfolgbar).
//! - HMAC-SHA256 statt BLAKE3, weil Android das Tag ohne Rust-Kern prüft (javax.crypto).
//! - Ein Tag weckt nur; die eigentliche Übertragung läuft wie immer über die verschlüsselte,
//!   gegenseitig geprüfte Drop-Verbindung.

use sha2::{Digest, Sha256};

/// Länge einer Epoche (Sekunden).
pub const EPOCH_SECS: i64 = 900;
const TAG_CONTEXT: &[u8] = b"snap-wake-v1";

/// Weck-Schlüssel eines Kontakts (auf beiden Seiten gleich).
pub fn wake_key(contact_secret: &[u8]) -> [u8; 32] {
    blake3::derive_key("snap drop wake key v1", contact_secret)
}

/// HMAC-SHA256 (RFC 2104) mit `sha2` – ohne zusätzliche Abhängigkeit.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// Epoche zu einer Unix-Zeit.
pub fn epoch(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(EPOCH_SECS)
}

/// Weck-Tag für eine Epoche.
pub fn wake_tag(wake_key: &[u8; 32], epoch: i64) -> [u8; 8] {
    let mut msg = Vec::with_capacity(TAG_CONTEXT.len() + 8);
    msg.extend_from_slice(TAG_CONTEXT);
    msg.extend_from_slice(&epoch.to_be_bytes());
    let mac = hmac_sha256(wake_key, &msg);
    let mut t = [0u8; 8];
    t.copy_from_slice(&mac[..8]);
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        data_encoding::HEXLOWER.encode(b)
    }

    #[test]
    fn hmac_wie_rfc_4231() {
        // Testfall 1 und 6 (Schlüssel länger als ein Block)
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn tags_wechseln_und_passen_nur_zum_kontakt() {
        let k = wake_key(&[7; 32]);
        assert_eq!(wake_tag(&k, 5), wake_tag(&k, 5));
        assert_ne!(wake_tag(&k, 5), wake_tag(&k, 6));
        assert_ne!(wake_tag(&k, 5), wake_tag(&wake_key(&[8; 32]), 5));
        assert_eq!(epoch(899), 0);
        assert_eq!(epoch(900), 1);
        assert_eq!(epoch(-1), -1);
        // Fester Vektor – dieselbe Rechnung prüft die Android-Seite (DropWake.kt)
        let fixed = [0x11u8; 32];
        assert_eq!(hex(&wake_tag(&fixed, 1_000_000)), "57c4dc25019c290f");
    }
}
