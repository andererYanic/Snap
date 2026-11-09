//! CMS-Signaturen für PDFs (PAdES-Basisprofil B-B, `ETSI.CAdES.detached`): selbst
//! zusammengesetzt aus den Typen von RustCrypto `cms`/`x509-cert` (ohne deren „builder“, der
//! `rsa` zum Signieren mitbrächte). Signiert wird nur mit ECDSA P-256; geprüft werden auch
//! fremde Signaturen mit RSA (PKCS#1 v1.5) – nur öffentliche Schlüssel, keine geheimen Operationen.

use super::id::{UnlockedId, OID_ECDSA_SHA256};
use crate::api_bail;
use anyhow::{Context, Result};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{
    CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos,
};
use p256::ecdsa::signature::hazmat::PrehashVerifier;
use sha2::{Digest, Sha256, Sha384, Sha512};
use x509_cert::attr::Attribute;
use x509_cert::der::asn1::{Any, ObjectIdentifier, OctetString, SetOfVec};
use x509_cert::der::{Decode, Encode};
use x509_cert::spki::AlgorithmIdentifierOwned;
use x509_cert::Certificate;

const OID_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.1");
const OID_SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const OID_CONTENT_TYPE: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.3");
const OID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const OID_SIGNING_TIME: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.5");
const OID_SIGNING_CERT_V2: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.2.47");
const OID_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const OID_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.2");
const OID_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.3");
const OID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const OID_P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const OID_ECDSA_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
const OID_ECDSA_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.4");
const OID_RSA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");
const OID_RSA_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
const OID_RSA_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.12");
const OID_RSA_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.13");

/// Größte CMS-Struktur, die geprüft wird.
pub const MAX_CMS: usize = 512 * 1024;

fn attr(oid: ObjectIdentifier, value_der: &[u8]) -> Result<Attribute> {
    let mut values = SetOfVec::new();
    values.insert(Any::from_der(value_der)?).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Attribute { oid, values })
}

/// DER-Länge + Inhalt mit Kennung `tag`.
fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = body.len();
    if n < 128 {
        v.push(n as u8);
    } else {
        let bytes = (n as u32).to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        v.push(0x80 | (4 - skip) as u8);
        v.extend_from_slice(&bytes[skip..]);
    }
    v.extend_from_slice(body);
    v
}

/// Erstellt die CMS-Signatur (DER) über den Hash `digest` (SHA-256 des signierten Bereichs).
pub fn sign_digest(id: &UnlockedId, digest: &[u8; 32]) -> Result<Vec<u8>> {
    use p256::ecdsa::signature::Signer;
    // signingCertificateV2: SEQUENCE { SEQUENCE OF ESSCertIDv2 { certHash } } (SHA-256 = Standard)
    let cert_hash = Sha256::digest(&id.cert_der);
    let ess = tlv(0x30, &tlv(0x30, &tlv(0x30, &tlv(0x04, &cert_hash))));
    let mut attrs = SetOfVec::new();
    attrs.insert(attr(OID_CONTENT_TYPE, &OID_DATA.to_der()?)?).map_err(|e| anyhow::anyhow!("{e}"))?;
    attrs
        .insert(attr(OID_MESSAGE_DIGEST, &OctetString::new(digest.to_vec())?.to_der()?)?)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    attrs.insert(attr(OID_SIGNING_CERT_V2, &ess)?).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Signiert wird die DER-Kodierung als SET OF (RFC 5652, 5.4)
    let to_sign = attrs.to_der()?;
    let sig: p256::ecdsa::DerSignature = id.key.sign(&to_sign);
    let sha256 = AlgorithmIdentifierOwned { oid: OID_SHA256, parameters: None };
    let tbs = id.cert.tbs_certificate();
    let signer = SignerInfo {
        version: CmsVersion::V1,
        sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer: tbs.issuer().clone(),
            serial_number: tbs.serial_number().clone(),
        }),
        digest_alg: sha256.clone(),
        signed_attrs: Some(attrs),
        signature_algorithm: AlgorithmIdentifierOwned { oid: OID_ECDSA_SHA256, parameters: None },
        signature: OctetString::new(sig.as_bytes().to_vec())?,
        unsigned_attrs: None,
    };
    let mut digests = SetOfVec::new();
    digests.insert(sha256).map_err(|e| anyhow::anyhow!("{e}"))?;
    let sd = SignedData {
        version: CmsVersion::V1,
        digest_algorithms: digests,
        encap_content_info: EncapsulatedContentInfo { econtent_type: OID_DATA, econtent: None },
        certificates: Some(CertificateSet::try_from(vec![CertificateChoices::Certificate(id.cert.clone())])?),
        crls: None,
        signer_infos: SignerInfos::try_from(vec![signer])?,
    };
    let ci = ContentInfo { content_type: OID_SIGNED_DATA, content: Any::from_der(&sd.to_der()?)? };
    Ok(ci.to_der()?)
}

/// Länge einer DER-Struktur am Anfang von `b` (Kopf + Inhalt), um Füllnullen abzuschneiden.
pub fn der_len(b: &[u8]) -> Option<usize> {
    if b.len() < 2 || b[0] != 0x30 {
        return None;
    }
    let (n, head) = match b[1] {
        l if l < 0x80 => (l as usize, 2),
        0x81..=0x84 => {
            let k = (b[1] & 0x7f) as usize;
            let mut n = 0usize;
            for i in 0..k {
                n = (n << 8) | *b.get(2 + i)? as usize;
            }
            (n, 2 + k)
        }
        _ => return None,
    };
    let total = head.checked_add(n)?;
    (total <= b.len()).then_some(total)
}

/// Ergebnis der kryptografischen Prüfung einer CMS-Signatur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmsCheck {
    /// Hash des signierten Bereichs passt und die Signatur ist mathematisch gültig.
    pub valid: bool,
    pub signer: String,
    pub issuer: String,
    pub self_signed: bool,
    /// SHA-256 des Unterzeichner-Zertifikats.
    pub fingerprint: String,
    pub algorithm: String,
    /// Zeitpunkt laut Signatur (signingTime), falls vorhanden – nicht vertrauenswürdig.
    pub claimed_time: Option<String>,
    pub not_before: String,
    pub not_after: String,
    pub problem: Option<String>,
}

fn common_name(name: &x509_cert::name::Name) -> String {
    let cn: Option<String> = name.common_name().ok().flatten().map(String::from);
    cn.unwrap_or_else(|| name.to_string()).chars().filter(|c| !c.is_control()).take(128).collect()
}

enum Hash {
    S256,
    S384,
    S512,
}

impl Hash {
    fn of(oid: &ObjectIdentifier) -> Option<Self> {
        match *oid {
            OID_SHA256 => Some(Hash::S256),
            OID_SHA384 => Some(Hash::S384),
            OID_SHA512 => Some(Hash::S512),
            _ => None,
        }
    }
    fn digest(&self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut h = D::new();
            for p in parts {
                h.update(p);
            }
            h.finalize().to_vec()
        }
        match self {
            Hash::S256 => run::<Sha256>(parts),
            Hash::S384 => run::<Sha384>(parts),
            Hash::S512 => run::<Sha512>(parts),
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Hash::S256 => "SHA-256",
            Hash::S384 => "SHA-384",
            Hash::S512 => "SHA-512",
        }
    }
}

/// Prüft eine CMS-Signatur über die Bytes `signed` (die Teile des PDFs laut /ByteRange).
pub fn verify(cms_der: &[u8], signed: &[&[u8]]) -> Result<CmsCheck> {
    if cms_der.len() > MAX_CMS {
        api_bail!(InvalidInput, "Signatur zu groß");
    }
    let ci = ContentInfo::from_der(cms_der).context("Signatur ist keine gültige CMS-Struktur")?;
    if ci.content_type != OID_SIGNED_DATA {
        api_bail!(InvalidInput, "Signatur hat einen unbekannten Inhaltstyp");
    }
    let sd: SignedData = ci.content.decode_as().context("Signatur beschädigt")?;
    let signer = sd.signer_infos.0.iter().next().context("Signatur ohne Unterzeichner")?;
    let certs: Vec<&Certificate> = sd
        .certificates
        .as_ref()
        .map(|c| {
            c.0.iter()
                .filter_map(|c| match c {
                    CertificateChoices::Certificate(c) => Some(c),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let cert = match &signer.sid {
        SignerIdentifier::IssuerAndSerialNumber(isn) => certs.iter().find(|c| {
            c.tbs_certificate().issuer() == &isn.issuer && c.tbs_certificate().serial_number() == &isn.serial_number
        }),
        SignerIdentifier::SubjectKeyIdentifier(_) => certs.first(),
    }
    .context("Zertifikat des Unterzeichners fehlt in der Signatur")?;
    let tbs = cert.tbs_certificate();
    let cert_der = cert.to_der()?;
    let mut out = CmsCheck {
        valid: false,
        signer: common_name(tbs.subject()),
        issuer: common_name(tbs.issuer()),
        self_signed: tbs.subject() == tbs.issuer(),
        fingerprint: data_encoding::HEXLOWER.encode(&Sha256::digest(&cert_der)),
        algorithm: String::new(),
        claimed_time: None,
        not_before: tbs.validity().not_before.to_string(),
        not_after: tbs.validity().not_after.to_string(),
        problem: None,
    };
    let Some(hash) = Hash::of(&signer.digest_alg.oid) else {
        out.problem = Some("Unbekanntes Hashverfahren".into());
        return Ok(out);
    };
    let content_digest = hash.digest(signed);
    // Was tatsächlich signiert wurde: die signierten Attribute (Normalfall) oder direkt der Inhalt
    let signed_hash = match &signer.signed_attrs {
        Some(attrs) => {
            let md = attrs.iter().find(|a| a.oid == OID_MESSAGE_DIGEST).and_then(|a| a.values.iter().next());
            let md: Option<OctetString> = md.and_then(|v| v.decode_as().ok());
            if md.as_ref().map(|m| m.as_bytes()) != Some(&content_digest[..]) {
                out.problem = Some("Das Dokument wurde nach dem Signieren verändert (Hash passt nicht).".into());
                return Ok(out);
            }
            let ct = attrs.iter().find(|a| a.oid == OID_CONTENT_TYPE).and_then(|a| a.values.iter().next());
            if ct.and_then(|v| v.decode_as::<ObjectIdentifier>().ok()) != Some(OID_DATA) {
                out.problem = Some("Signatur hat einen unerwarteten Inhaltstyp.".into());
                return Ok(out);
            }
            if let Some(t) = attrs.iter().find(|a| a.oid == OID_SIGNING_TIME).and_then(|a| a.values.iter().next()) {
                out.claimed_time =
                    t.to_der().ok().and_then(|d| x509_cert::time::Time::from_der(&d).ok()).map(|t| t.to_string());
            }
            hash.digest(&[&attrs.to_der()?])
        }
        None => content_digest,
    };
    let sig = signer.signature.as_bytes();
    let spki = tbs.subject_public_key_info();
    let sig_oid = signer.signature_algorithm.oid;
    let key_oid = spki.algorithm.oid;
    let ok = if key_oid == OID_EC_PUBLIC_KEY {
        let curve: Option<ObjectIdentifier> = spki.algorithm.parameters.as_ref().and_then(|p| p.decode_as().ok());
        if curve != Some(OID_P256) {
            out.problem = Some("Signaturverfahren wird nicht unterstützt (nur P-256)".into());
            return Ok(out);
        }
        if ![OID_ECDSA_SHA256, OID_ECDSA_SHA384, OID_ECDSA_SHA512, OID_EC_PUBLIC_KEY].contains(&sig_oid) {
            out.problem = Some("Unbekanntes Signaturverfahren".into());
            return Ok(out);
        }
        out.algorithm = format!("ECDSA P-256 / {}", hash.name());
        let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(spki.subject_public_key.raw_bytes());
        let s = p256::ecdsa::Signature::from_der(sig);
        match (vk, s) {
            (Ok(vk), Ok(s)) => vk.verify_prehash(&signed_hash, &s).is_ok(),
            _ => false,
        }
    } else if key_oid == OID_RSA {
        if ![OID_RSA, OID_RSA_SHA256, OID_RSA_SHA384, OID_RSA_SHA512].contains(&sig_oid) {
            out.problem = Some("Signaturverfahren wird nicht unterstützt (RSA-PSS)".into());
            return Ok(out);
        }
        use rsa::pkcs8::DecodePublicKey;
        let Ok(pk) = rsa::PublicKey::from_public_key_der(&spki.to_der()?) else {
            out.problem = Some("Öffentlicher Schlüssel ungültig".into());
            return Ok(out);
        };
        let bits = rsa::traits::PublicKeyParts::size(&pk) * 8;
        if bits < 2048 {
            out.problem = Some(format!("RSA-Schlüssel zu kurz ({bits} Bit)"));
            return Ok(out);
        }
        out.algorithm = format!("RSA {bits} / {}", hash.name());
        let scheme = match hash {
            Hash::S256 => rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha256>(),
            Hash::S384 => rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha384>(),
            Hash::S512 => rsa::pkcs1v15::Pkcs1v15Sign::new::<Sha512>(),
        };
        pk.verify(scheme, &signed_hash, sig).is_ok()
    } else {
        out.problem = Some("Signaturverfahren wird nicht unterstützt".into());
        return Ok(out);
    };
    if !ok {
        out.problem = Some("Die Signatur ist ungültig.".into());
        return Ok(out);
    }
    out.valid = true;
    Ok(out)
}
