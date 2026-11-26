//! Drop-Schlüssel, Sitzungswert, Prüfcode und Kontakt-Tags.
//!
//! - Der Drop-Schlüssel (Ed25519) ist dauerhaft, aber **getrennt vom Geräteschlüssel**: Wer mit
//!   mir tauscht, lernt nie die Kennung, mit der meine Geräte gekoppelt sind.
//! - Der Sitzungswert stammt aus der TLS-Verbindung (RFC 5705). Er ist auf beiden Seiten gleich,
//!   für jede Verbindung neu und für Dritte (auch einen Mittelsmann mit eigener TLS-Sitzung)
//!   unbekannt. Prüfcode, Kontakt-Geheimnis und Signaturen hängen an ihm.

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::{PublicKey, SecretKey, Signature};
use std::path::Path;

const EXPORT_LABEL: &[u8] = b"EXPORTER-snap-drop-v1";
const SIG_CONTEXT: &[u8] = b"snap-drop-session-sig-v1";

/// Dauerhafter Drop-Schlüssel dieses Geräts.
pub struct DropIdentity {
    key: SecretKey,
}

impl DropIdentity {
    /// Lädt den Schlüssel oder erzeugt ihn beim ersten Start (Datei 0600, gleiche Prüfungen wie
    /// beim Geräteschlüssel).
    pub fn load_or_create(path: &Path) -> Result<Self> {
        Ok(Self { key: crate::secure_fs::load_or_create_secret_key(path)? })
    }

    /// Nur für Tests: Schlüssel im Speicher.
    pub fn ephemeral() -> Self {
        Self { key: SecretKey::generate() }
    }

    pub fn public(&self) -> PublicKey {
        self.key.public()
    }

    /// Signatur über den Sitzungswert (beweist den Besitz des Schlüssels in genau dieser Verbindung).
    pub fn sign_session(&self, session: &[u8; 32]) -> Signature {
        self.key.sign(&session_message(session))
    }
}

fn session_message(session: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIG_CONTEXT.len() + 32);
    m.extend_from_slice(SIG_CONTEXT);
    m.extend_from_slice(session);
    m
}

pub fn verify_session(key: &PublicKey, session: &[u8; 32], sig: &Signature) -> bool {
    key.verify(&session_message(session), sig).is_ok()
}

/// Sitzungswert der Verbindung (auf beiden Seiten gleich).
pub fn session_value(conn: &Connection) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    conn.export_keying_material(&mut out, EXPORT_LABEL, b"session")
        .map_err(|_| anyhow::anyhow!("Sitzungsschlüssel nicht ableitbar"))
        .context("TLS-Export")?;
    Ok(out)
}

/// Sechsstelliger Prüfcode („123 456“), auf beiden Bildschirmen gleich.
pub fn check_code(session: &[u8; 32]) -> String {
    let k = blake3::derive_key("snap drop check code v1", session);
    let n = u32::from_be_bytes([k[0], k[1], k[2], k[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// Gemeinsames Kontakt-Geheimnis aus dieser Sitzung (nur gespeichert, wenn der Nutzer es will).
pub fn contact_secret(session: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("snap drop contact secret v1", session)
}

/// Kontakt-Tag für eine Probe: ohne das Geheimnis zufällig, je Verbindung anders.
pub fn contact_tag(secret: &[u8; 32], session: &[u8; 32]) -> [u8; 8] {
    let h = blake3::keyed_hash(secret, session);
    let mut t = [0u8; 8];
    t.copy_from_slice(&h.as_bytes()[..8]);
    t
}

/// Kurzer Fingerabdruck eines Drop-Schlüssels zum Vergleichen (z. B. „a1b2 c3d4 e5f6“).
pub fn fingerprint(key: &PublicKey) -> String {
    let h = blake3::hash(key.as_bytes());
    let hex = data_encoding::HEXLOWER.encode(&h.as_bytes()[..6]);
    format!("{} {} {}", &hex[0..4], &hex[4..8], &hex[8..12])
}

/// Konstanter Vergleich (Tags sollen kein Timing verraten).
pub fn ct_eq8(a: &[u8; 8], b: &[u8; 8]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatur_gilt_nur_fuer_ihre_sitzung() {
        let id = DropIdentity::ephemeral();
        let s1 = [1u8; 32];
        let s2 = [2u8; 32];
        let sig = id.sign_session(&s1);
        assert!(verify_session(&id.public(), &s1, &sig));
        assert!(!verify_session(&id.public(), &s2, &sig));
        assert!(!verify_session(&DropIdentity::ephemeral().public(), &s1, &sig));
    }

    #[test]
    fn pruefcode_und_tags() {
        let s = [7u8; 32];
        let c = check_code(&s);
        assert_eq!(c.len(), 7);
        assert!(c.chars().enumerate().all(|(i, ch)| if i == 3 { ch == ' ' } else { ch.is_ascii_digit() }));
        assert_eq!(c, check_code(&s));
        assert_ne!(c, check_code(&[8u8; 32]));
        let secret = contact_secret(&s);
        assert_ne!(contact_tag(&secret, &[1; 32]), contact_tag(&secret, &[2; 32]));
        assert_ne!(contact_tag(&secret, &[1; 32]), contact_tag(&contact_secret(&[9; 32]), &[1; 32]));
        assert!(ct_eq8(&[1; 8], &[1; 8]) && !ct_eq8(&[1; 8], &[2; 8]));
    }
}
