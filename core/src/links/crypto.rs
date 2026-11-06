//! Verschlüsselungsformat für Freigabelinks (v1) – siehe `docs/LINK_FORMAT.md`.
//!
//! Gleiches Format wie `gateway/cloudflare/snap-crypto.js` (Browser, WebCrypto); ein
//! Kreuztest (`core/tests/links.rs`) prüft beide Implementierungen gegeneinander.
//!
//! - Link-Geheimnis `S` (32 Byte Zufall) steht nur im Link-Teil nach `#`.
//! - Optionales Passwort: `P = PBKDF2-HMAC-SHA256(pw, "snap-link-v1/pw/" ‖ id, 600 000, 32)`,
//!   `IKM = S ‖ P` (sonst `IKM = S`).
//! - `HKDF-SHA256(salt = "snap-link-v1/" ‖ id, IKM)` → Schlüssel `data` und `meta`.
//! - Daten: Stücke à 64 KiB, AES-256-GCM, Nonce = 11 Byte Stücknummer (big endian) ‖ Endmarke
//!   (1 = letztes Stück). Vertauschen, Weglassen und Abschneiden fallen auf.
//! - Metadaten (Name, Größe, Typ): AES-256-GCM mit eigenem Schlüssel, Nonce 0.

use anyhow::{bail, Result};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

pub const CHUNK: usize = 64 * 1024;
pub const TAG: usize = 16;
pub const PBKDF2_ROUNDS: u32 = 600_000;

/// Schlüssel eines Links.
pub struct LinkKeys {
    data: LessSafeKey,
    meta: LessSafeKey,
}

struct Len(usize);
impl ring::hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf_expand(prk: &ring::hkdf::Prk, info: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    prk.expand(&[info], Len(32)).expect("HKDF-Länge").fill(&mut out).expect("HKDF-Länge");
    out
}

impl LinkKeys {
    /// Leitet die Schlüssel aus Geheimnis, Link-ID und optionalem Passwort ab.
    pub fn derive(secret: &[u8; 32], id: &str, password: Option<&str>) -> Self {
        let mut ikm = secret.to_vec();
        if let Some(pw) = password {
            let mut p = [0u8; 32];
            ring::pbkdf2::derive(
                ring::pbkdf2::PBKDF2_HMAC_SHA256,
                NonZeroU32::new(PBKDF2_ROUNDS).expect("> 0"),
                format!("snap-link-v1/pw/{id}").as_bytes(),
                pw.as_bytes(),
                &mut p,
            );
            ikm.extend_from_slice(&p);
        }
        let salt = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, format!("snap-link-v1/{id}").as_bytes());
        let prk = salt.extract(&ikm);
        let key = |info: &[u8]| {
            LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &hkdf_expand(&prk, info)).expect("AES-256-Schlüssel"))
        };
        Self { data: key(b"data"), meta: key(b"meta") }
    }
}

fn chunk_nonce(index: u64, last: bool) -> Nonce {
    let mut n = [0u8; 12];
    n[3..11].copy_from_slice(&index.to_be_bytes());
    n[11] = u8::from(last);
    Nonce::assume_unique_for_key(n)
}

/// Anzahl der Stücke für `size` Bytes (eine leere Datei hat ein leeres Stück).
pub fn chunk_count(size: u64) -> u64 {
    size.div_ceil(CHUNK as u64).max(1)
}

/// Größe der verschlüsselten Daten.
pub fn encrypted_size(size: u64) -> u64 {
    size + chunk_count(size) * TAG as u64
}

/// Verschlüsselt ein Stück (in place) und hängt den Tag an.
pub fn seal_chunk(keys: &LinkKeys, index: u64, last: bool, buf: &mut Vec<u8>) -> Result<()> {
    if buf.len() > CHUNK || (!last && buf.len() != CHUNK) {
        bail!("Ungültige Stückgröße");
    }
    keys.data
        .seal_in_place_append_tag(chunk_nonce(index, last), Aad::empty(), buf)
        .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))
}

/// Entschlüsselt ein Stück (in place). Fehler bei falschem Schlüssel oder Manipulation.
pub fn open_chunk(keys: &LinkKeys, index: u64, last: bool, buf: &mut Vec<u8>) -> Result<()> {
    if buf.len() < TAG || buf.len() > CHUNK + TAG {
        bail!("Ungültige Stückgröße");
    }
    let n = keys
        .data
        .open_in_place(chunk_nonce(index, last), Aad::empty(), buf)
        .map_err(|_| anyhow::anyhow!("Entschlüsselung fehlgeschlagen (falscher Schlüssel oder manipuliert)"))?
        .len();
    buf.truncate(n);
    Ok(())
}

/// Metadaten einer geteilten Datei (verschlüsselt gespeichert).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkMeta {
    pub v: u32,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub chunk: u32,
}

pub fn seal_meta(keys: &LinkKeys, meta: &LinkMeta) -> Result<Vec<u8>> {
    let mut buf = serde_json::to_vec(meta)?;
    keys.meta
        .seal_in_place_append_tag(Nonce::assume_unique_for_key([0; 12]), Aad::empty(), &mut buf)
        .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
    Ok(buf)
}

pub fn open_meta(keys: &LinkKeys, data: &[u8]) -> Result<LinkMeta> {
    if data.len() > 64 * 1024 {
        bail!("Metadaten zu groß");
    }
    let mut buf = data.to_vec();
    let plain = keys
        .meta
        .open_in_place(Nonce::assume_unique_for_key([0; 12]), Aad::empty(), &mut buf)
        .map_err(|_| anyhow::anyhow!("Falscher Schlüssel oder falsches Passwort"))?;
    let meta: LinkMeta = serde_json::from_slice(plain)?;
    if meta.v != 1 || meta.chunk as usize != CHUNK {
        bail!("Unbekanntes Format");
    }
    Ok(meta)
}

/// Verschlüsselt eine ganze Datei im Speicher (für Tests und kleine Dateien).
pub fn seal_all(keys: &LinkKeys, plain: &[u8]) -> Result<Vec<u8>> {
    let n = chunk_count(plain.len() as u64);
    let mut out = Vec::with_capacity(encrypted_size(plain.len() as u64) as usize);
    for i in 0..n {
        let start = (i as usize) * CHUNK;
        let end = (start + CHUNK).min(plain.len());
        let mut buf = plain[start.min(plain.len())..end].to_vec();
        seal_chunk(keys, i, i == n - 1, &mut buf)?;
        out.extend_from_slice(&buf);
    }
    Ok(out)
}

/// Entschlüsselt eine ganze Datei; prüft, dass die Größe zur Metadatenangabe passt.
pub fn open_all(keys: &LinkKeys, data: &[u8], size: u64) -> Result<Vec<u8>> {
    if data.len() as u64 != encrypted_size(size) {
        bail!("Größe passt nicht (abgeschnitten oder verlängert)");
    }
    let n = chunk_count(size);
    let mut out = Vec::with_capacity(size as usize);
    for i in 0..n {
        let start = (i as usize) * (CHUNK + TAG);
        let end = (start + CHUNK + TAG).min(data.len());
        let mut buf = data[start..end].to_vec();
        open_chunk(keys, i, i == n - 1, &mut buf)?;
        out.extend_from_slice(&buf);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(pw: Option<&str>) -> LinkKeys {
        LinkKeys::derive(&[7u8; 32], "0123456789abcdef0123456789abcdef", pw)
    }

    #[test]
    fn roundtrip_and_tamper_detection() {
        let k = keys(None);
        for size in [0usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let plain: Vec<u8> = (0..size).map(|i| (i * 31) as u8).collect();
            let enc = seal_all(&k, &plain).unwrap();
            assert_eq!(enc.len() as u64, encrypted_size(size as u64));
            assert_eq!(open_all(&k, &enc, size as u64).unwrap(), plain, "{size}");
        }
        let plain = vec![1u8; 3 * CHUNK];
        let enc = seal_all(&k, &plain).unwrap();
        // Bit kippen
        let mut bad = enc.clone();
        bad[100] ^= 1;
        assert!(open_all(&k, &bad, plain.len() as u64).is_err());
        // Stücke vertauschen
        let mut swapped = enc.clone();
        let (a, b) = swapped.split_at_mut(CHUNK + TAG);
        a.swap_with_slice(&mut b[..CHUNK + TAG]);
        assert!(open_all(&k, &swapped, plain.len() as u64).is_err());
        // Abschneiden nach einem vollständigen Stück (als hätte es nur 2 Stücke)
        let truncated = &enc[..2 * (CHUNK + TAG)];
        assert!(open_all(&k, truncated, 2 * CHUNK as u64).is_err(), "Endmarke fehlt");
        // Falsches Passwort / Passwort fehlt
        let kp = keys(Some("geheim"));
        let encp = seal_all(&kp, b"hallo").unwrap();
        assert!(open_all(&keys(Some("falsch")), &encp, 5).is_err());
        assert!(open_all(&keys(None), &encp, 5).is_err());
        assert_eq!(open_all(&keys(Some("geheim")), &encp, 5).unwrap(), b"hallo");
        // Andere Link-ID → anderer Schlüssel
        let other = LinkKeys::derive(&[7u8; 32], "ffffffffffffffffffffffffffffffff", None);
        assert!(open_all(&other, &enc, plain.len() as u64).is_err());
    }

    #[test]
    fn meta_roundtrip() {
        let k = keys(None);
        let m =
            LinkMeta { v: 1, name: "Urlaub 2024.mp4".into(), size: 42, mime: "video/mp4".into(), chunk: CHUNK as u32 };
        let enc = seal_meta(&k, &m).unwrap();
        assert!(!String::from_utf8_lossy(&enc).contains("Urlaub"), "Name verschlüsselt");
        assert_eq!(open_meta(&k, &enc).unwrap(), m);
        assert!(open_meta(&keys(Some("x")), &enc).is_err());
    }
}
