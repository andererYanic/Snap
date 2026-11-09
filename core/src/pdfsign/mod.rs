//! PDFs markieren und signieren (Textmarker, sichtbare Unterschrift + PAdES-Signatur) sowie
//! Signaturen in empfangenen PDFs prüfen. Design und Risiken: `docs/SECURITY_MODEL.md`
//! („PDF-Signaturen“), Entscheidung vom 2026-11-09 in `docs/ROADMAP.md`.

pub mod cms;
pub mod id;
pub mod pdf;

use crate::api_bail;
use anyhow::{Context, Result};
use lopdf::{Document, Object};
use pdf::{Label, Layer, Stroke, Update};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Platz für die CMS-Signatur im PDF (Bytes; als Hex doppelt so viel).
pub const SIG_RESERVE: usize = 16 * 1024;
const BYTE_RANGE_PLACEHOLDER: &str = "[0 0000000000 0000000000 0000000000]";
/// Höchstzahl geprüfter Formularfelder (gegen absichtlich riesige Formulare).
const MAX_FIELDS: usize = 10_000;

/// Ein geprüfte Signatur in einem PDF.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SignatureReport {
    pub field: String,
    /// Mathematisch gültig und der signierte Bereich ist unverändert.
    pub valid: bool,
    /// Die Signatur deckt das ganze Dokument ab (nichts wurde danach angehängt).
    pub covers_document: bool,
    pub signer: String,
    pub issuer: String,
    pub self_signed: bool,
    /// Zertifikat gehört zur eigenen digitalen ID.
    pub own: bool,
    pub fingerprint: String,
    pub algorithm: String,
    /// Zeitpunkt laut PDF (/M) bzw. Signatur – vom Unterzeichner angegeben, nicht bestätigt.
    pub signed_at: Option<String>,
    pub reason: Option<String>,
    pub problem: Option<String>,
}

/// Unterschrift auf einer Seite: Striche (normiert auf die angezeigte Seite).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Placement {
    pub page: usize,
    pub strokes: Vec<Stroke>,
    /// Strichbreite relativ zur Seitenbreite.
    #[serde(default = "default_ink")]
    pub width: f32,
}

fn default_ink() -> f32 {
    0.004
}

/// Datum im PDF-Format (UTC): `D:YYYYMMDDHHmmSS+00'00'`.
pub fn pdf_date(unix: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(unix);
    format!("D:{y:04}{mo:02}{d:02}{h:02}{mi:02}{s:02}+00'00'")
}

/// Lesbares Datum (UTC) für Beschriftungen.
pub fn human_date(unix: i64) -> String {
    let (y, mo, d, h, mi, _) = civil(unix);
    format!("{d:02}.{mo:02}.{y:04} {h:02}:{mi:02} UTC")
}

fn civil(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Howard Hinnant, civil_from_days
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, (secs / 3600) as u32, (secs % 3600 / 60) as u32, (secs % 60) as u32)
}

/// Signiert ein PDF: sichtbare Unterschrift (Seiteninhalt) + unsichtbares Signaturfeld mit
/// CMS-Signatur über das ganze Dokument. Das Original bleibt als Anfang der Datei unverändert.
pub fn sign(orig: &[u8], place: &Placement, signer: &id::UnlockedId, signer_name: &str, now: i64) -> Result<Vec<u8>> {
    let mut up = Update::new(orig)?;
    let pages = up.doc.get_pages().len();
    let ink = Layer {
        page: place.page,
        strokes: place.strokes.clone(),
        color: [0.08, 0.12, 0.35],
        width: place.width,
        opacity: 1.0,
        multiply: false,
    };
    if place.strokes.is_empty() {
        api_bail!(InvalidInput, "Bitte zuerst unterschreiben.");
    }
    pdf::check_layers(std::slice::from_ref(&ink), pages)?;
    // Beschriftung unter der Unterschrift
    let (mut x0, mut y1) = (1f32, 0f32);
    for &(u, v) in place.strokes.iter().flatten() {
        x0 = x0.min(u);
        y1 = y1.max(v);
    }
    let label = Label {
        page: place.page,
        at: (x0.clamp(0.0, 0.8), (y1 + 0.02).min(0.97)),
        size: 0.012,
        lines: vec![format!("Digital signiert von {signer_name}"), human_date(now)],
    };
    // Signaturwert mit Platzhaltern, Feld ohne sichtbare Fläche (die Unterschrift steht im Inhalt)
    let existing = count_signatures(&up.doc);
    let field_name = format!("Signatur{}", existing + 1);
    let mut sig = format!(
        "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /ETSI.CAdES.detached /ByteRange {BYTE_RANGE_PLACEHOLDER} /Contents <"
    )
    .into_bytes();
    sig.extend(std::iter::repeat_n(b'0', SIG_RESERVE * 2));
    sig.extend_from_slice(b"> /M ");
    sig.extend_from_slice(&pdf::pdf_string(&pdf_date(now)));
    sig.extend_from_slice(b" /Name ");
    sig.extend_from_slice(&pdf::pdf_string(signer_name));
    sig.extend_from_slice(b" >>");
    let sig_id = up.add(sig);
    let pid = pdf::page_id(&up.doc, place.page)?;
    let widget = format!(
        "<< /Type /Annot /Subtype /Widget /FT /Sig /T {} /V {} {} R /F 132 /Rect [0 0 0 0] /P {} {} R >>",
        String::from_utf8_lossy(&pdf::pdf_string(&field_name)),
        sig_id.0,
        sig_id.1,
        pid.0,
        pid.1
    );
    let widget_id = up.add(widget.into_bytes());
    up.draw_on_page(place.page, &[&ink], &[&label], Some(widget_id))?;
    up.add_field(widget_id)?;
    let mut out = up.finish()?;

    // Platzhalter im neuen Teil finden und füllen
    let tail = orig.len();
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    let br_at = tail + find(&out[tail..], BYTE_RANGE_PLACEHOLDER.as_bytes()).context("Platzhalter fehlt")?;
    let marker = b"/Contents <";
    let c_open = br_at + find(&out[br_at..], marker).context("Platzhalter fehlt")? + marker.len() - 1;
    let c_close = c_open + 1 + SIG_RESERVE * 2;
    if out.get(c_close) != Some(&b'>') {
        api_bail!(Internal, "Platzhalter beschädigt");
    }
    let (b, c) = (c_open, c_close + 1);
    let d = out.len() - c;
    let br = format!("[0 {b} {c} {d}]");
    let padded = format!("{br:<width$}", width = BYTE_RANGE_PLACEHOLDER.len());
    out[br_at..br_at + padded.len()].copy_from_slice(padded.as_bytes());
    let digest: [u8; 32] = {
        let mut h = Sha256::new();
        h.update(&out[..b]);
        h.update(&out[c..]);
        h.finalize().into()
    };
    let cms = cms::sign_digest(signer, &digest)?;
    if cms.len() > SIG_RESERVE {
        api_bail!(Internal, "Signatur passt nicht in den Platzhalter");
    }
    let hex = data_encoding::HEXUPPER.encode(&cms);
    out[b + 1..b + 1 + hex.len()].copy_from_slice(hex.as_bytes());
    Ok(out)
}

/// Zieht die Signaturfelder (FT /Sig mit Wert) aus dem Formular, rekursiv über /Kids.
fn signature_fields(doc: &Document) -> Vec<(String, lopdf::Dictionary)> {
    let mut out = Vec::new();
    let Ok(cat) = doc.catalog() else { return out };
    let form = match cat.get(b"AcroForm").ok().and_then(|o| doc.dereference(o).ok()) {
        Some((_, Object::Dictionary(d))) => d.clone(),
        _ => return out,
    };
    let Some(Object::Array(fields)) =
        form.get(b"Fields").ok().and_then(|o| doc.dereference(o).ok()).map(|x| x.1.clone())
    else {
        return out;
    };
    let mut stack: Vec<(Object, Option<Vec<u8>>, String)> =
        fields.into_iter().map(|f| (f, None, String::new())).collect();
    let mut seen = std::collections::HashSet::new();
    let mut visited = 0;
    while let Some((o, parent_ft, parent_name)) = stack.pop() {
        visited += 1;
        if visited > MAX_FIELDS {
            break;
        }
        if let Object::Reference(r) = o {
            if !seen.insert(r) {
                continue; // Zyklen
            }
        }
        let Ok((_, Object::Dictionary(d))) = doc.dereference(&o) else { continue };
        let ft = d.get(b"FT").ok().and_then(|f| f.as_name().ok()).map(|n| n.to_vec()).or(parent_ft);
        let part = d.get(b"T").ok().and_then(|t| t.as_str().ok()).map(|t| String::from_utf8_lossy(t).into_owned());
        let name = match (parent_name.is_empty(), part) {
            (_, None) => parent_name.clone(),
            (true, Some(p)) => p,
            (false, Some(p)) => format!("{parent_name}.{p}"),
        };
        if ft.as_deref() == Some(b"Sig") {
            if let Some(Ok((_, Object::Dictionary(v)))) = d.get(b"V").ok().map(|v| doc.dereference(v)) {
                out.push((name.chars().filter(|c| !c.is_control()).take(80).collect(), v.clone()));
            }
        }
        if let Some(Ok((_, Object::Array(kids)))) = d.get(b"Kids").ok().map(|k| doc.dereference(k)) {
            for k in kids.iter().take(MAX_FIELDS) {
                stack.push((k.clone(), ft.clone(), name.clone()));
            }
        }
    }
    out
}

fn count_signatures(doc: &Document) -> usize {
    signature_fields(doc).len()
}

fn readable_pdf_date(s: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(s).ok()?.trim_start_matches("D:");
    let d = |r: std::ops::Range<usize>| s.get(r).filter(|x| x.bytes().all(|b| b.is_ascii_digit()));
    Some(format!("{}.{}.{} {}:{}", d(6..8)?, d(4..6)?, d(0..4)?, d(8..10).unwrap_or("00"), d(10..12).unwrap_or("00")))
}

/// Prüft alle Signaturen eines PDFs. `own_fp`: Fingerabdruck der eigenen ID (falls vorhanden).
pub fn verify(bytes: &[u8], own_fp: Option<&str>) -> Result<Vec<SignatureReport>> {
    let doc = pdf::load(bytes)?;
    let mut out = Vec::new();
    for (field, v) in signature_fields(&doc).into_iter().take(100) {
        let mut rep = SignatureReport {
            field,
            valid: false,
            covers_document: false,
            signer: String::new(),
            issuer: String::new(),
            self_signed: false,
            own: false,
            fingerprint: String::new(),
            algorithm: String::new(),
            signed_at: v.get(b"M").ok().and_then(|m| m.as_str().ok()).and_then(readable_pdf_date),
            reason: v
                .get(b"Reason")
                .ok()
                .and_then(|r| r.as_str().ok())
                .map(|r| String::from_utf8_lossy(r).chars().filter(|c| !c.is_control()).take(200).collect()),
            problem: None,
        };
        match check_one(bytes, &v) {
            Ok((check, covers)) => {
                rep.valid = check.valid;
                rep.covers_document = covers;
                rep.signer = check.signer;
                rep.issuer = check.issuer;
                rep.self_signed = check.self_signed;
                rep.own = own_fp.is_some_and(|f| f == check.fingerprint);
                rep.fingerprint = check.fingerprint;
                rep.algorithm = check.algorithm;
                rep.problem = check.problem;
                if rep.signed_at.is_none() {
                    rep.signed_at = check.claimed_time;
                }
            }
            Err(e) => rep.problem = Some(format!("{e:#}")),
        }
        out.push(rep);
    }
    Ok(out)
}

/// Prüft /ByteRange und /Contents gegen die Datei und dann die CMS-Signatur.
fn check_one(bytes: &[u8], v: &lopdf::Dictionary) -> Result<(cms::CmsCheck, bool)> {
    let br = match v.get(b"ByteRange") {
        Ok(Object::Array(a)) if a.len() == 4 => {
            let n: Vec<i64> = a.iter().filter_map(|x| x.as_i64().ok()).collect();
            if n.len() != 4 || n.iter().any(|x| *x < 0) {
                api_bail!(InvalidInput, "Signaturbereich ungültig");
            }
            n.into_iter().map(|x| x as usize).collect::<Vec<_>>()
        }
        _ => api_bail!(InvalidInput, "Signaturbereich fehlt"),
    };
    let (a, b, c, d) = (br[0], br[1], br[2], br[3]);
    let len = bytes.len();
    if a != 0 || b == 0 || c <= b + 1 || c.checked_add(d).is_none_or(|e| e > len) {
        api_bail!(InvalidInput, "Signaturbereich passt nicht zur Datei");
    }
    // Die Lücke muss genau der Hex-Wert von /Contents sein (sonst Signatur-Umpacken möglich)
    if bytes[b] != b'<' || bytes[c - 1] != b'>' {
        api_bail!(InvalidInput, "Signaturbereich zeigt nicht auf die Signatur");
    }
    let hex: Vec<u8> = bytes[b + 1..c - 1].iter().copied().filter(|x| !x.is_ascii_whitespace()).collect();
    let raw =
        data_encoding::HEXLOWER_PERMISSIVE.decode(&hex).map_err(|_| anyhow::anyhow!("Signatur ist kein Hex-Wert"))?;
    let contents = v.get(b"Contents").ok().and_then(|c| c.as_str().ok()).context("Signaturwert fehlt")?;
    if contents != raw.as_slice() {
        api_bail!(InvalidInput, "Signaturwert passt nicht zum Signaturbereich");
    }
    let n = cms::der_len(&raw).context("Signaturwert beschädigt")?;
    if raw[n..].iter().any(|x| *x != 0) {
        api_bail!(InvalidInput, "Signaturwert enthält zusätzliche Daten");
    }
    let check = cms::verify(&raw[..n], &[&bytes[..b], &bytes[c..c + d]])?;
    // Abgedeckt bis zum Ende? Sonst wurde danach etwas angehängt (inkrementelle Änderung).
    let covers = c + d == len;
    Ok((check, covers))
}

// ---------------------------------------------------------------- Ablage und Daemon-Anbindung

fn dir(data_dir: &Path) -> Result<PathBuf> {
    let d = data_dir.join("sign");
    crate::secure_fs::ensure_private_dir(&d)?;
    Ok(d)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_id(data_dir: &Path) -> Result<Option<id::IdFile>> {
    let p = dir(data_dir)?.join("id.json");
    match std::fs::read(&p) {
        Ok(b) if b.len() <= 64 * 1024 => Ok(Some(serde_json::from_slice(&b).context("ID-Datei beschädigt")?)),
        Ok(_) => api_bail!(InvalidInput, "ID-Datei zu groß"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn store_id(data_dir: &Path, f: &id::IdFile) -> Result<()> {
    id::check(f)?;
    write_private(&dir(data_dir)?.join("id.json"), &serde_json::to_vec_pretty(f)?)
}

pub fn delete_id(data_dir: &Path) -> Result<()> {
    let p = dir(data_dir)?.join("id.json");
    match std::fs::remove_file(p) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Gespeicherte Unterschrift (Striche, normiert auf ein Feld 3:1). Nicht geheim, aber persönlich.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SavedSignature {
    pub strokes: Vec<Stroke>,
}

pub fn load_signature(data_dir: &Path) -> Result<Option<SavedSignature>> {
    match std::fs::read(dir(data_dir)?.join("unterschrift.json")) {
        Ok(b) if b.len() <= 4 * 1024 * 1024 => Ok(serde_json::from_slice(&b).ok()),
        _ => Ok(None),
    }
}

pub fn store_signature(data_dir: &Path, s: &SavedSignature) -> Result<()> {
    let points: usize = s.strokes.iter().map(|x| x.len()).sum();
    if s.strokes.len() > pdf::MAX_STROKES || points > 50_000 {
        api_bail!(InvalidInput, "Unterschrift zu groß.");
    }
    if !s
        .strokes
        .iter()
        .flatten()
        .all(|(u, v)| u.is_finite() && v.is_finite() && (0.0..=1.0).contains(u) && (0.0..=1.0).contains(v))
    {
        api_bail!(InvalidInput, "Ungültige Unterschrift.");
    }
    write_private(&dir(data_dir)?.join("unterschrift.json"), &serde_json::to_vec(s)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> (id::IdFile, id::UnlockedId) {
        let f = id::create_fast("Max Muster", "geheim-123", 1_700_000_000).unwrap();
        let u = id::unlock(&f, "geheim-123").unwrap();
        (f, u)
    }

    fn place() -> Placement {
        Placement { page: 0, strokes: vec![vec![(0.6, 0.8), (0.7, 0.78), (0.8, 0.82)]], width: 0.004 }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let (f, u) = id();
        let orig = pdf::tests::sample_pdf(2, 0);
        let signed = sign(&orig, &place(), &u, "Max Muster", 1_700_000_100).unwrap();
        assert!(signed.starts_with(&orig));
        let fp = id::check(&f).unwrap().fingerprint;
        let reps = verify(&signed, Some(&fp)).unwrap();
        assert_eq!(reps.len(), 1, "{reps:?}");
        let r = &reps[0];
        assert!(r.valid && r.covers_document && r.own && r.self_signed, "{r:?}");
        assert_eq!(r.signer, "Max Muster");
        assert_eq!(r.field, "Signatur1");
        assert!(r.algorithm.starts_with("ECDSA P-256"));
        assert_eq!(r.signed_at.as_deref(), Some("14.11.2023 22:15"));
        // Sichtbar: Beschriftung steht im Seiteninhalt
        let doc = pdf::load(&signed).unwrap();
        let content = doc.get_page_content(pdf::page_id(&doc, 0).unwrap());
        assert!(String::from_utf8_lossy(&content).contains("Digital signiert von Max Muster"));
    }

    #[test]
    fn tampering_is_detected() {
        let (_, u) = id();
        let orig = pdf::tests::sample_pdf(1, 0);
        let signed = sign(&orig, &place(), &u, "Max Muster", 1_700_000_100).unwrap();
        // 1. Ein Byte im signierten Bereich ändern (Text „Seite 1“ → „Seite 9“)
        let mut evil = signed.clone();
        let pos = evil.windows(7).position(|w| w == b"Seite 1").unwrap();
        evil[pos + 6] = b'9';
        let r = &verify(&evil, None).unwrap()[0];
        assert!(!r.valid, "{r:?}");
        assert!(r.problem.as_deref().unwrap_or("").contains("verändert"), "{r:?}");
        // 2. Nach dem Signieren etwas anhängen (inkrementelles Update): Signatur gültig, aber
        //    deckt das Dokument nicht mehr ab – muss deutlich gemeldet werden
        let hl = pdf::Layer {
            page: 0,
            strokes: vec![vec![(0.1, 0.1), (0.4, 0.1)]],
            color: [1.0, 0.9, 0.0],
            width: 0.02,
            opacity: 0.35,
            multiply: true,
        };
        let later = pdf::draw(&signed, &[hl]).unwrap();
        let r = &verify(&later, None).unwrap()[0];
        assert!(r.valid && !r.covers_document, "{r:?}");
        // 3. Signatur eines anderen Schlüssels hineinkopieren → ungültig
        let (_, other) = id();
        let signed_other = sign(&orig, &place(), &other, "Eva", 1_700_000_100).unwrap();
        let mut swapped = signed.clone();
        let hex_at = |b: &[u8]| b.windows(11).position(|w| w == b"/Contents <").unwrap() + 11;
        let (a, o) = (hex_at(&signed), hex_at(&signed_other));
        swapped[a..a + SIG_RESERVE * 2].copy_from_slice(&signed_other[o..o + SIG_RESERVE * 2]);
        let r = &verify(&swapped, None).unwrap()[0];
        assert!(!r.valid, "{r:?}");
        // 4. ByteRange so manipuliert, dass ein Teil des Dokuments ungeschützt bliebe
        let mut br = signed.clone();
        let p = br.windows(12).position(|w| w == b"/ByteRange [").unwrap() + 12;
        br[p] = b'1'; // erster Wert nicht 0
        let r = &verify(&br, None).unwrap()[0];
        assert!(!r.valid && r.problem.is_some(), "{r:?}");
    }

    #[test]
    fn dates() {
        assert_eq!(pdf_date(0), "D:19700101000000+00'00'");
        assert_eq!(pdf_date(1_700_000_100), "D:20231114221500+00'00'");
        assert_eq!(human_date(951_782_400), "29.02.2000 00:00 UTC");
    }
}

/// Schreibt Beispieldateien für die Prüfung mit fremden Werkzeugen (pdfsig, qpdf, pyHanko):
/// `SNAP_PDF_OUT=/pfad cargo test -p space-core --lib pdfsign::external -- --ignored`
#[cfg(test)]
mod external {
    use super::*;

    #[test]
    #[ignore]
    fn write_samples() {
        let Ok(out) = std::env::var("SNAP_PDF_OUT") else { return };
        let out = std::path::Path::new(&out);
        let f = id::create_fast("Max Muster", "geheim-123", 1_700_000_000).unwrap();
        let u = id::unlock(&f, "geheim-123").unwrap();
        let p = Placement { page: 0, strokes: vec![vec![(0.6, 0.8), (0.7, 0.78), (0.8, 0.82)]], width: 0.004 };
        for (name, src) in [("klassisch", pdf::tests::sample_pdf(2, 0)), ("gedreht", pdf::tests::sample_pdf(1, 90))] {
            std::fs::write(out.join(format!("{name}.pdf")), &src).unwrap();
            let s = sign(&src, &p, &u, "Max Muster", 1_700_000_100).unwrap();
            std::fs::write(out.join(format!("{name}-signiert.pdf")), &s).unwrap();
            let hl = pdf::Layer {
                page: 0,
                strokes: vec![vec![(0.1, 0.1), (0.5, 0.1)]],
                color: [1.0, 0.9, 0.0],
                width: 0.02,
                opacity: 0.35,
                multiply: true,
            };
            std::fs::write(
                out.join(format!("{name}-markiert.pdf")),
                pdf::draw(&src, std::slice::from_ref(&hl)).unwrap(),
            )
            .unwrap();
            std::fs::write(out.join(format!("{name}-signiert-danach-markiert.pdf")), pdf::draw(&s, &[hl]).unwrap())
                .unwrap();
        }
        std::fs::write(out.join("zertifikat.der"), &u.cert_der).unwrap();
    }
}
