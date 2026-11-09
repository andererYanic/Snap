//! Digitale ID zum Signieren: ECDSA-P-256-Schlüssel (RustCrypto `p256`) mit selbst signiertem
//! X.509-Zertifikat. Der private Schlüssel liegt nur verschlüsselt vor (Argon2id + AES-256-GCM)
//! und ist damit als Datei portabel (Export/Import auf andere Geräte).

use crate::api_bail;
use anyhow::{Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use p256::ecdsa::{SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x509_cert::der::{Decode, Encode};
use x509_cert::name::Name;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::SubjectPublicKeyInfoOwned;
use x509_cert::time::{Time, Validity};
use x509_cert::Certificate;

/// Kürzestes Passwort für die digitale ID.
pub const MIN_PASSWORD: usize = 8;
/// Längster Name im Zertifikat.
pub const MAX_NAME: usize = 64;
/// Argon2id-Kosten (wie von OWASP empfohlen, auf Telefonen ≈ 0,3–1 s).
const ARGON_M_KIB: u32 = 64 * 1024;
const ARGON_T: u32 = 3;
const ARGON_P: u32 = 1;
const AAD: &[u8] = b"snap-sign-id-v1";

/// OID ecdsa-with-SHA256 (1.2.840.10045.4.3.2).
pub(crate) const OID_ECDSA_SHA256: x509_cert::der::oid::ObjectIdentifier =
    x509_cert::der::oid::ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");

/// Gespeicherte ID (`sign/id.json`, 0600). Enthält den Schlüssel nur verschlüsselt.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdFile {
    pub v: u32,
    pub name: String,
    pub created: i64,
    /// Zertifikat (DER, Base64).
    pub cert: String,
    pub kdf: Kdf,
    pub nonce: String,
    /// PKCS#8-Schlüssel, AES-256-GCM.
    pub key: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Kdf {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
}

/// Für die Oberfläche: was über die ID bekannt ist (ohne Geheimnisse).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IdInfo {
    pub name: String,
    pub created: i64,
    /// SHA-256 des Zertifikats (Hex, zum Vergleichen mit Empfängern).
    pub fingerprint: String,
}

/// Entsperrte ID: Schlüssel und Zertifikat (nur kurz im Speicher, nie gespeichert).
pub struct UnlockedId {
    pub key: SigningKey,
    pub cert: Certificate,
    pub cert_der: Vec<u8>,
}

fn b64() -> data_encoding::Encoding {
    data_encoding::BASE64
}

/// Hex-Fingerabdruck eines Zertifikats (SHA-256 über DER).
pub fn fingerprint(cert_der: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(&Sha256::digest(cert_der))
}

fn kek(password: &str, kdf: &Kdf) -> Result<[u8; 32]> {
    if kdf.alg != "argon2id" {
        api_bail!(InvalidInput, "Unbekanntes Schlüsselverfahren in der ID-Datei");
    }
    // Grenzen gegen manipulierte Dateien (Speicher/Zeit)
    if !(8 * 1024..=512 * 1024).contains(&kdf.m_kib) || !(1..=10).contains(&kdf.t) || !(1..=4).contains(&kdf.p) {
        api_bail!(InvalidInput, "Ungültige Schlüsselparameter in der ID-Datei");
    }
    let salt = b64().decode(kdf.salt.as_bytes()).context("ID-Datei beschädigt")?;
    if !(16..=64).contains(&salt.len()) {
        api_bail!(InvalidInput, "ID-Datei beschädigt");
    }
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), &salt, &mut out)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(out)
}

fn rand<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("Zufallszahlen nicht verfügbar: {e}"))?;
    Ok(b)
}

/// Zeichen, die in Namen von Zertifikaten erlaubt sind (keine Steuerzeichen, kein `=,+;<>\"#`).
fn clean_name(name: &str) -> Result<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME {
        api_bail!(InvalidInput, "Name muss 1–{MAX_NAME} Zeichen lang sein.");
    }
    if n.chars().any(|c| c.is_control() || "=,+;<>\\\"#".contains(c)) {
        api_bail!(InvalidInput, "Der Name enthält unzulässige Zeichen.");
    }
    Ok(n.to_string())
}

/// Erstellt eine neue ID: Schlüssel, selbst signiertes Zertifikat (10 Jahre), verschlüsselt.
pub fn create(name: &str, password: &str, now: i64) -> Result<IdFile> {
    let name = clean_name(name)?;
    if password.chars().count() < MIN_PASSWORD {
        api_bail!(InvalidInput, "Das Passwort braucht mindestens {MIN_PASSWORD} Zeichen.");
    }
    let key = SigningKey::from_bytes(&rand::<32>()?.into()).map_err(|_| anyhow::anyhow!("Schlüssel ungültig"))?;
    let cert_der = self_signed(&key, &name, now)?;
    seal(&name, now, &cert_der, &key, password)
}

fn seal(name: &str, now: i64, cert_der: &[u8], key: &SigningKey, password: &str) -> Result<IdFile> {
    let kdf =
        Kdf { alg: "argon2id".into(), m_kib: ARGON_M_KIB, t: ARGON_T, p: ARGON_P, salt: b64().encode(&rand::<16>()?) };
    let k = kek(password, &kdf)?;
    let nonce: [u8; 12] = rand()?;
    let mut buf = key.to_pkcs8_der().map_err(|e| anyhow::anyhow!("{e}"))?.as_bytes().to_vec();
    let aead = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &k).map_err(|_| anyhow::anyhow!("AES"))?);
    // Zertifikat als AAD: Schlüssel und Zertifikat lassen sich nicht unbemerkt vertauschen.
    let aad = [AAD, cert_der].concat();
    aead.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(&aad[..]), &mut buf)
        .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
    Ok(IdFile {
        v: 1,
        name: name.to_string(),
        created: now,
        cert: b64().encode(cert_der),
        kdf,
        nonce: b64().encode(&nonce),
        key: b64().encode(&buf),
    })
}

/// Prüft eine ID-Datei (z. B. beim Import) ohne Passwort: Format, Zertifikat, Grenzen.
pub fn check(file: &IdFile) -> Result<IdInfo> {
    if file.v != 1 {
        api_bail!(InvalidInput, "Unbekannte Version der ID-Datei");
    }
    let name = clean_name(&file.name)?;
    let cert_der = b64().decode(file.cert.as_bytes()).context("ID-Datei beschädigt")?;
    if cert_der.len() > 16 * 1024 || file.key.len() > 4096 || file.nonce.len() > 64 {
        api_bail!(InvalidInput, "ID-Datei beschädigt");
    }
    let cert =
        Certificate::from_der(&cert_der).map_err(|_| anyhow::anyhow!("Zertifikat in der ID-Datei ist ungültig"))?;
    if cert.signature_algorithm().oid != OID_ECDSA_SHA256 {
        api_bail!(InvalidInput, "Nur P-256-IDs werden unterstützt");
    }
    // Schlüsselparameter prüfen, ohne zu rechnen
    if file.kdf.alg != "argon2id" {
        api_bail!(InvalidInput, "Unbekanntes Schlüsselverfahren in der ID-Datei");
    }
    Ok(IdInfo { name, created: file.created, fingerprint: fingerprint(&cert_der) })
}

/// Entsperrt die ID mit dem Passwort.
pub fn unlock(file: &IdFile, password: &str) -> Result<UnlockedId> {
    check(file)?;
    let cert_der = b64().decode(file.cert.as_bytes())?;
    let nonce: [u8; 12] = b64()
        .decode(file.nonce.as_bytes())
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| anyhow::anyhow!("ID-Datei beschädigt"))?;
    let mut buf = b64().decode(file.key.as_bytes()).context("ID-Datei beschädigt")?;
    let k = kek(password, &file.kdf)?;
    let aead = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &k).map_err(|_| anyhow::anyhow!("AES"))?);
    let aad = [AAD, &cert_der[..]].concat();
    let plain =
        aead.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(&aad[..]), &mut buf).map_err(|_| {
            crate::api::error::ApiError::new(crate::api::error::ErrorCode::PasswordRequired, "Falsches Passwort.")
        })?;
    let key = SigningKey::from_pkcs8_der(plain).map_err(|_| anyhow::anyhow!("ID-Datei beschädigt"))?;
    let cert = Certificate::from_der(&cert_der)?;
    // Schlüssel passt zum Zertifikat?
    let spki = VerifyingKey::from(&key).to_public_key_der().map_err(|e| anyhow::anyhow!("{e}"))?;
    if spki.as_bytes() != cert.tbs_certificate().subject_public_key_info().to_der()?.as_slice() {
        api_bail!(InvalidInput, "Schlüssel und Zertifikat passen nicht zusammen");
    }
    Ok(UnlockedId { key, cert, cert_der })
}

/// Profil für das eigene, selbst signierte Signaturzertifikat (Aussteller = Inhaber).
struct SelfSigned {
    subject: Name,
}

impl x509_cert::builder::profile::BuilderProfile for SelfSigned {
    fn get_issuer(&self, subject: &Name) -> Name {
        subject.clone()
    }
    fn get_subject(&self) -> Name {
        self.subject.clone()
    }
    fn build_extensions(
        &self,
        _spk: x509_cert::spki::SubjectPublicKeyInfoRef<'_>,
        _issuer_spk: x509_cert::spki::SubjectPublicKeyInfoRef<'_>,
        _tbs: &x509_cert::TbsCertificate,
    ) -> x509_cert::builder::Result<Vec<x509_cert::ext::Extension>> {
        extensions().map_err(|_| x509_cert::builder::Error::Signature(Default::default()))
    }
}

/// Selbst signiertes Zertifikat (X.509 v3) für `name`, gültig 10 Jahre ab `now`.
fn self_signed(key: &SigningKey, name: &str, now: i64) -> Result<Vec<u8>> {
    use std::str::FromStr;
    use x509_cert::builder::{Builder, CertificateBuilder};
    // Name als RFC-4514-Text; Sonderzeichen sind oben ausgeschlossen
    let subject = Name::from_str(&format!("CN={name},O=Snap (selbst signiert)")).map_err(|e| anyhow::anyhow!("{e}"))?;
    let serial: [u8; 16] = {
        let mut s = rand::<16>()?;
        s[0] &= 0x7f; // positiv
        s[0] |= 0x01; // nicht mit 0 beginnend
        s
    };
    let time = |t: i64| -> Result<Time> {
        let st = std::time::UNIX_EPOCH + std::time::Duration::from_secs(t.max(0) as u64);
        Time::try_from(st).map_err(|e| anyhow::anyhow!("{e}"))
    };
    let validity = Validity::new(time(now - 60)?, time(now + 10 * 365 * 86_400)?);
    let spki_der = VerifyingKey::from(key).to_public_key_der().map_err(|e| anyhow::anyhow!("{e}"))?;
    let spki = SubjectPublicKeyInfoOwned::from_der(spki_der.as_bytes())?;
    let builder = CertificateBuilder::new(
        SelfSigned { subject },
        SerialNumber::new(&serial).map_err(|e| anyhow::anyhow!("{e}"))?,
        validity,
        spki,
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    let cert =
        builder.build::<SigningKey, p256::ecdsa::DerSignature>(key).map_err(|e| anyhow::anyhow!("Zertifikat: {e}"))?;
    Ok(cert.to_der()?)
}

/// Erweiterungen: Schlüsselverwendung „digitale Signatur + Nichtabstreitbarkeit“, kein CA.
fn extensions() -> Result<Vec<x509_cert::ext::Extension>> {
    use x509_cert::der::asn1::OctetString;
    use x509_cert::ext::pkix::{BasicConstraints, KeyUsage, KeyUsages};
    let ku = KeyUsage(KeyUsages::DigitalSignature | KeyUsages::NonRepudiation);
    let bc = BasicConstraints { ca: false, path_len_constraint: None };
    Ok(vec![
        x509_cert::ext::Extension {
            extn_id: x509_cert::der::oid::ObjectIdentifier::new_unwrap("2.5.29.15"),
            critical: true,
            extn_value: OctetString::new(ku.to_der()?)?,
        },
        x509_cert::ext::Extension {
            extn_id: x509_cert::der::oid::ObjectIdentifier::new_unwrap("2.5.29.19"),
            critical: true,
            extn_value: OctetString::new(bc.to_der()?)?,
        },
    ])
}

/// Für Tests: schnelle Argon2-Parameter wären unsicher, daher nur hier sichtbar.
#[cfg(test)]
pub(crate) fn create_fast(name: &str, password: &str, now: i64) -> Result<IdFile> {
    let key = SigningKey::from_bytes(&rand::<32>()?.into()).map_err(|_| anyhow::anyhow!("Schlüssel ungültig"))?;
    let cert_der = self_signed(&key, &clean_name(name)?, now)?;
    let mut f = seal(name, now, &cert_der, &key, password)?;
    // erneut mit kleinen Kosten verschlüsseln
    f.kdf.m_kib = 8 * 1024;
    f.kdf.t = 1;
    let k = kek(password, &f.kdf)?;
    let nonce: [u8; 12] = rand()?;
    let mut buf = key.to_pkcs8_der().map_err(|e| anyhow::anyhow!("{e}"))?.as_bytes().to_vec();
    let aead = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &k).map_err(|_| anyhow::anyhow!("AES"))?);
    let aad = [AAD, &cert_der[..]].concat();
    aead.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(&aad[..]), &mut buf)
        .map_err(|_| anyhow::anyhow!("AES"))?;
    f.nonce = b64().encode(&nonce);
    f.key = b64().encode(&buf);
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_unlock_and_reject_wrong_password() {
        let f = create_fast("Max Muster", "geheim-123", 1_700_000_000).unwrap();
        let info = check(&f).unwrap();
        assert_eq!(info.name, "Max Muster");
        assert_eq!(info.fingerprint.len(), 64);
        let id = unlock(&f, "geheim-123").unwrap();
        assert!(id.cert_der.len() > 200);
        let e = unlock(&f, "falsch-123").err().unwrap();
        assert_eq!(crate::api::error::code_of(&e), crate::api::error::ErrorCode::PasswordRequired);
        // Vertauschtes Zertifikat fällt auf (AAD)
        let other = create_fast("Eva", "geheim-123", 1_700_000_000).unwrap();
        let mut mixed = f.clone();
        mixed.cert = other.cert.clone();
        assert!(unlock(&mixed, "geheim-123").is_err());
        // Ungültige Namen und Passwörter
        assert!(create("", "geheim-123", 0).is_err());
        assert!(create("A=B", "geheim-123", 0).is_err());
        assert!(create("Max", "kurz", 0).is_err());
        // Manipulierte Kosten werden abgelehnt (kein Speicher-DoS)
        let mut huge = f.clone();
        huge.kdf.m_kib = 4 * 1024 * 1024;
        assert!(unlock(&huge, "geheim-123").is_err());
    }
}
