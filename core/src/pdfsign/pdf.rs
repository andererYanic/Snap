//! PDF ändern, ohne das Original anzutasten: Alles Neue kommt in ein **inkrementelles Update**
//! am Dateiende (die ursprünglichen Bytes bleiben unverändert – wichtig für Signaturen).
//!
//! Gezeichnetes (Textmarker, sichtbare Unterschrift) wird als **Seiteninhalt** eingefügt, nicht als
//! Anmerkung: Androids `PdfRenderer` und viele Drucker zeigen Anmerkungen nicht zuverlässig, der
//! Seiteninhalt erscheint überall. Der bisherige Inhalt wird mit `q … Q` eingeklammert, damit er
//! den neuen nicht verschieben kann.

use crate::api_bail;
use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::io::Write;

/// Größte PDF-Datei, die bearbeitet oder geprüft wird.
pub const MAX_PDF_BYTES: usize = 256 * 1024 * 1024;
/// Höchstzahl Striche und Punkte (Eingaben aus der Oberfläche).
pub const MAX_STROKES: usize = 2_000;
pub const MAX_POINTS: usize = 200_000;

/// Ein Strich: Punkte im angezeigten (gedrehten) Seitenbild, normiert auf 0..1, Ursprung oben links.
pub type Stroke = Vec<(f32, f32)>;

/// Gezeichnete Ebene auf einer Seite.
#[derive(Debug, Clone)]
pub struct Layer {
    pub page: usize,
    pub strokes: Vec<Stroke>,
    /// RGB 0..1
    pub color: [f32; 3],
    /// Strichbreite relativ zur angezeigten Seitenbreite.
    pub width: f32,
    /// Deckkraft 0..1 (Textmarker: durchscheinend, „Multiplizieren“).
    pub opacity: f32,
    pub multiply: bool,
}

/// Kleiner Text (z. B. „Digital signiert von …“) unter einer Unterschrift.
#[derive(Debug, Clone)]
pub struct Label {
    pub page: usize,
    /// Linke untere Ecke (normiert, Ursprung oben links) und Schriftgröße relativ zur Seitenbreite.
    pub at: (f32, f32),
    pub size: f32,
    pub lines: Vec<String>,
}

/// Seitengeometrie: sichtbarer Ausschnitt (CropBox) und Drehung.
#[derive(Debug, Clone, Copy)]
pub struct PageGeom {
    pub crop: [f32; 4],
    pub rotate: i64,
}

impl PageGeom {
    /// Normierter Punkt im angezeigten Bild → PDF-Koordinaten.
    pub fn map(&self, u: f32, v: f32) -> (f32, f32) {
        let [x0, y0, x1, y1] = self.crop;
        let (w, h) = (x1 - x0, y1 - y0);
        let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
        match self.rotate {
            90 => (x0 + v * w, y0 + u * h),
            180 => (x1 - u * w, y0 + v * h),
            270 => (x1 - v * w, y1 - u * h),
            _ => (x0 + u * w, y1 - v * h),
        }
    }

    /// Breite des angezeigten Bildes in PDF-Punkten.
    pub fn display_width(&self) -> f32 {
        let [x0, y0, x1, y1] = self.crop;
        if self.rotate == 90 || self.rotate == 270 {
            y1 - y0
        } else {
            x1 - x0
        }
    }
}

/// Laden mit Grenzen; verschlüsselte PDFs werden abgelehnt.
pub fn load(bytes: &[u8]) -> Result<Document> {
    if bytes.len() > MAX_PDF_BYTES {
        api_bail!(InvalidInput, "Die PDF-Datei ist zu groß (höchstens 256 MB).");
    }
    if !bytes.starts_with(b"%PDF-") {
        api_bail!(InvalidInput, "Keine PDF-Datei.");
    }
    let doc = Document::load_mem(bytes).map_err(|e| {
        crate::api::error::ApiError::new(crate::api::error::ErrorCode::InvalidInput, format!("PDF nicht lesbar: {e}"))
    })?;
    if doc.is_encrypted() || doc.trailer.get(b"Encrypt").is_ok() {
        api_bail!(InvalidInput, "Verschlüsselte PDFs werden nicht unterstützt.");
    }
    Ok(doc)
}

fn deref<'a>(doc: &'a Document, o: &'a Object) -> Result<&'a Object> {
    Ok(doc.dereference(o).map_err(|e| anyhow::anyhow!("PDF-Verweis ungültig: {e}"))?.1)
}

/// Eintrag einer Seite, auch geerbt (Resources, MediaBox, CropBox, Rotate).
fn inherited<'a>(doc: &'a Document, page: ObjectId, key: &[u8]) -> Result<Option<&'a Object>> {
    let mut id = page;
    for _ in 0..64 {
        let d = doc.get_dictionary(id).map_err(|e| anyhow::anyhow!("Seite fehlt: {e}"))?;
        if let Ok(v) = d.get(key) {
            return Ok(Some(deref(doc, v)?));
        }
        match d.get(b"Parent") {
            Ok(Object::Reference(p)) => id = *p,
            _ => return Ok(None),
        }
    }
    api_bail!(InvalidInput, "Seitenbaum zu tief");
}

fn rect(doc: &Document, o: &Object) -> Option<[f32; 4]> {
    let a = o.as_array().ok()?;
    if a.len() != 4 {
        return None;
    }
    let mut v = [0f32; 4];
    for (i, x) in a.iter().enumerate() {
        v[i] = deref(doc, x).ok()?.as_float().ok()?;
        if !v[i].is_finite() {
            return None;
        }
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

pub fn page_id(doc: &Document, page: usize) -> Result<ObjectId> {
    // lopdf zählt Seiten ab 1
    let pages = doc.get_pages();
    match pages.get(&(page as u32 + 1)) {
        Some(id) => Ok(*id),
        None => api_bail!(InvalidInput, "Seite {} gibt es nicht.", page + 1),
    }
}

pub fn geometry(doc: &Document, page: ObjectId) -> Result<PageGeom> {
    let media = inherited(doc, page, b"MediaBox")?.and_then(|o| rect(doc, o)).unwrap_or([0.0, 0.0, 612.0, 792.0]);
    let crop = inherited(doc, page, b"CropBox")?.and_then(|o| rect(doc, o)).unwrap_or(media);
    let rotate = inherited(doc, page, b"Rotate")?.and_then(|o| o.as_i64().ok()).unwrap_or(0).rem_euclid(360);
    let rotate = if rotate % 90 == 0 { rotate } else { 0 };
    if crop[2] - crop[0] < 1.0 || crop[3] - crop[1] < 1.0 {
        api_bail!(InvalidInput, "Seite hat keine gültige Größe.");
    }
    Ok(PageGeom { crop, rotate })
}

/// Zahl im PDF-Format (kurz, ohne Exponent).
fn num(v: f32) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Zeichenkette als PDF-Literal (WinAnsi; Zeichen außerhalb werden „?“).
pub fn pdf_string(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for c in s.chars() {
        let b = match c as u32 {
            0x20..=0x7e => c as u8,
            0xa0..=0xff => c as u32 as u8,
            0x20ac => 0x80,
            _ => b'?',
        };
        if matches!(b, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b')');
    out
}

/// Zeichenbefehle für Ebenen und Beschriftungen einer Seite.
fn draw_ops(g: &PageGeom, layers: &[&Layer], labels: &[&Label], gs: &[(usize, String)]) -> Vec<u8> {
    let mut c = Vec::new();
    for (i, l) in layers.iter().enumerate() {
        let w = (l.width * g.display_width()).max(0.1);
        let [r, gg, b] = l.color;
        let _ = write!(c, "q\n/{} gs\n{} {} {} RG\n{} w 1 J 1 j\n", gs[i].1, num(r), num(gg), num(b), num(w));
        for s in &l.strokes {
            let mut it = s.iter();
            let Some(&(u, v)) = it.next() else { continue };
            let (x, y) = g.map(u, v);
            let _ = writeln!(c, "{} {} m", num(x), num(y));
            if s.len() == 1 {
                // Einzelner Punkt: winziger Strich, damit die runde Kappe einen Punkt zeichnet
                let _ = writeln!(c, "{} {} l", num(x + 0.01), num(y));
            }
            for &(u, v) in it {
                let (x, y) = g.map(u, v);
                let _ = writeln!(c, "{} {} l", num(x), num(y));
            }
            c.extend_from_slice(b"S\n");
        }
        c.extend_from_slice(b"Q\n");
    }
    for l in labels {
        let size = (l.size * g.display_width()).clamp(3.0, 48.0);
        let (x, y) = g.map(l.at.0, l.at.1);
        // Text in Leserichtung des angezeigten Bildes drehen
        let (a, b, cc, d) = match g.rotate {
            90 => (0.0, 1.0, -1.0, 0.0),
            180 => (-1.0, 0.0, 0.0, -1.0),
            270 => (0.0, -1.0, 1.0, 0.0),
            _ => (1.0, 0.0, 0.0, 1.0),
        };
        let _ = write!(
            c,
            "q\nBT\n/SnapF1 {} Tf\n{} {} {} {} {} {} Tm\n{} TL\n0.2 0.2 0.25 rg\n",
            num(size),
            a,
            b,
            cc,
            d,
            num(x),
            num(y),
            num(size * 1.25)
        );
        for (i, line) in l.lines.iter().enumerate() {
            if i > 0 {
                c.extend_from_slice(b"T*\n");
            }
            c.extend_from_slice(&pdf_string(line));
            c.extend_from_slice(b" Tj\n");
        }
        c.extend_from_slice(b"ET\nQ\n");
    }
    c
}

fn stream(body: &[u8]) -> Vec<u8> {
    let mut v = format!("<< /Length {} >>\nstream\n", body.len()).into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"\nendstream");
    v
}

fn ser(o: &Object) -> Vec<u8> {
    let mut v = Vec::new();
    let _ = lopdf::Writer::write_object(&mut v, o);
    v
}

/// Sammelt neue und geänderte Objekte und schreibt sie als Update ans Dateiende.
pub struct Update<'a> {
    orig: &'a [u8],
    pub doc: Document,
    objs: Vec<(ObjectId, Vec<u8>)>,
    next: u32,
}

impl<'a> Update<'a> {
    pub fn new(orig: &'a [u8]) -> Result<Self> {
        let doc = load(orig)?;
        let size = doc.trailer.get(b"Size").and_then(|o| o.as_i64()).unwrap_or(0).max(0) as u32;
        let next = doc.max_id.max(size.saturating_sub(1)) + 1;
        if next > 8_000_000 {
            api_bail!(InvalidInput, "PDF hat zu viele Objekte.");
        }
        Ok(Self { orig, doc, objs: Vec::new(), next })
    }

    pub fn add(&mut self, body: Vec<u8>) -> ObjectId {
        let id = (self.next, 0);
        self.next += 1;
        self.objs.push((id, body));
        id
    }

    pub fn put(&mut self, id: ObjectId, body: Vec<u8>) {
        self.objs.retain(|(i, _)| *i != id);
        self.objs.push((id, body));
    }

    /// Fügt einer Seite Zeichnungen und Beschriftungen als Inhalt hinzu.
    /// `extra_annot`: Verweis, der zusätzlich in /Annots kommt (Signaturfeld).
    pub fn draw_on_page(
        &mut self,
        page: usize,
        layers: &[&Layer],
        labels: &[&Label],
        extra_annot: Option<ObjectId>,
    ) -> Result<()> {
        let pid = page_id(&self.doc, page)?;
        let g = geometry(&self.doc, pid)?;
        let mut page_dict = self.doc.get_dictionary(pid).map_err(|e| anyhow::anyhow!("{e}"))?.clone();

        // Ressourcen (auch geerbte) übernehmen und um Transparenz und Schrift ergänzen
        let mut res = match inherited(&self.doc, pid, b"Resources")? {
            Some(Object::Dictionary(d)) => d.clone(),
            _ => Dictionary::new(),
        };
        let sub = |key: &[u8]| -> Dictionary {
            match res.get(key).ok().and_then(|o| deref(&self.doc, o).ok()) {
                Some(Object::Dictionary(d)) => d.clone(),
                _ => Dictionary::new(),
            }
        };
        let mut ext = sub(b"ExtGState");
        let mut fonts = sub(b"Font");
        let mut gs_names = Vec::new();
        for (i, l) in layers.iter().enumerate() {
            let mut name = format!("SnapGs{i}");
            let mut n = 0;
            while ext.has(name.as_bytes()) {
                n += 1;
                name = format!("SnapGs{i}x{n}");
            }
            let mut d = Dictionary::new();
            d.set("Type", Object::Name(b"ExtGState".to_vec()));
            d.set("CA", Object::Real(l.opacity.clamp(0.05, 1.0)));
            d.set("ca", Object::Real(l.opacity.clamp(0.05, 1.0)));
            d.set("BM", Object::Name(if l.multiply { b"Multiply".to_vec() } else { b"Normal".to_vec() }));
            ext.set(name.as_bytes(), Object::Dictionary(d));
            gs_names.push((i, name));
        }
        if !labels.is_empty() {
            let mut f = Dictionary::new();
            f.set("Type", Object::Name(b"Font".to_vec()));
            f.set("Subtype", Object::Name(b"Type1".to_vec()));
            f.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
            f.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
            fonts.set("SnapF1", Object::Dictionary(f));
        }
        res.set("ExtGState", Object::Dictionary(ext));
        if !labels.is_empty() {
            res.set("Font", Object::Dictionary(fonts));
        }
        page_dict.set("Resources", Object::Dictionary(res));

        // Inhalt: q [alter Inhalt] Q [neuer Inhalt]
        let mut contents: Vec<Object> = match page_dict.get(b"Contents").ok() {
            Some(Object::Reference(r)) => match self.doc.get_object(*r).ok() {
                Some(Object::Array(a)) => a.clone(),
                _ => vec![Object::Reference(*r)],
            },
            Some(Object::Array(a)) => a.clone(),
            _ => vec![],
        };
        if contents.len() > 10_000 {
            api_bail!(InvalidInput, "Seite hat zu viele Inhaltsteile.");
        }
        let q = self.add(stream(b"q"));
        let qq = self.add(stream(b"Q"));
        let new = self.add(stream(&draw_ops(&g, layers, labels, &gs_names)));
        contents.insert(0, Object::Reference(q));
        contents.push(Object::Reference(qq));
        contents.push(Object::Reference(new));
        page_dict.set("Contents", Object::Array(contents));

        if let Some(a) = extra_annot {
            let mut annots = match page_dict.get(b"Annots").ok().and_then(|o| deref(&self.doc, o).ok()) {
                Some(Object::Array(v)) => v.clone(),
                _ => vec![],
            };
            annots.push(Object::Reference(a));
            page_dict.set("Annots", Object::Array(annots));
        }
        self.put(pid, ser(&Object::Dictionary(page_dict)));
        Ok(())
    }

    /// Fügt ein Signaturfeld in /AcroForm des Katalogs ein.
    pub fn add_field(&mut self, field: ObjectId) -> Result<()> {
        let root = match self.doc.trailer.get(b"Root") {
            Ok(Object::Reference(r)) => *r,
            _ => api_bail!(InvalidInput, "PDF ohne Katalog"),
        };
        let mut cat = self.doc.get_dictionary(root).map_err(|e| anyhow::anyhow!("{e}"))?.clone();
        let (form_id, mut form) = match cat.get(b"AcroForm").ok() {
            Some(Object::Reference(r)) => (Some(*r), self.doc.get_dictionary(*r).cloned().unwrap_or_default()),
            Some(Object::Dictionary(d)) => (None, d.clone()),
            _ => (None, Dictionary::new()),
        };
        let mut fields = match form.get(b"Fields").ok().and_then(|o| deref(&self.doc, o).ok()) {
            Some(Object::Array(v)) => v.clone(),
            _ => vec![],
        };
        fields.push(Object::Reference(field));
        form.set("Fields", Object::Array(fields));
        form.set("SigFlags", Object::Integer(3));
        match form_id {
            Some(id) => self.put(id, ser(&Object::Dictionary(form))),
            None => {
                cat.set("AcroForm", Object::Dictionary(form));
                self.put(root, ser(&Object::Dictionary(cat)));
            }
        }
        Ok(())
    }

    /// Schreibt das Update: Original + neue Objekte + Querverweise + Trailer.
    pub fn finish(mut self) -> Result<Vec<u8>> {
        let orig = self.orig;
        let prev = startxref(orig).context("PDF ohne gültiges „startxref“")?;
        let classic = orig.get(prev..).is_some_and(|t| t.starts_with(b"xref"));
        let mut out = orig.to_vec();
        if !out.ends_with(b"\n") && !out.ends_with(b"\r") {
            out.push(b'\n');
        }
        self.objs.sort_by_key(|(id, _)| *id);
        let mut offsets = Vec::new();
        for (id, body) in &self.objs {
            offsets.push((*id, out.len()));
            let _ = writeln!(out, "{} {} obj", id.0, id.1);
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let mut trailer = Dictionary::new();
        for key in [&b"Root"[..], b"Info", b"ID"] {
            if let Ok(v) = self.doc.trailer.get(key) {
                trailer.set(key, v.clone());
            }
        }
        trailer.set("Prev", Object::Integer(prev as i64));
        if classic {
            trailer.set("Size", Object::Integer(self.next as i64));
            let xref_at = out.len();
            out.extend_from_slice(b"xref\n");
            for (id, off) in &offsets {
                let _ = write!(out, "{} 1\n{:010} {:05} n \n", id.0, off, id.1);
            }
            out.extend_from_slice(b"trailer\n");
            out.extend_from_slice(&ser(&Object::Dictionary(trailer)));
            let _ = write!(out, "\nstartxref\n{xref_at}\n%%EOF\n");
        } else {
            // Querverweis-Strom (wie im Original): Einträge Typ 1, Offset 4 Byte, Generation 2 Byte
            let xid = self.next;
            let xref_at = out.len();
            let mut entries = offsets.clone();
            entries.push(((xid, 0), xref_at));
            let mut index = Vec::new();
            let mut data = Vec::new();
            for (id, off) in &entries {
                index.push(Object::Integer(id.0 as i64));
                index.push(Object::Integer(1));
                data.push(1u8);
                data.extend_from_slice(&(*off as u32).to_be_bytes());
                data.extend_from_slice(&id.1.to_be_bytes());
            }
            if xref_at > u32::MAX as usize {
                api_bail!(InvalidInput, "PDF zu groß für Querverweis-Strom");
            }
            trailer.set("Type", Object::Name(b"XRef".to_vec()));
            trailer.set("Size", Object::Integer(xid as i64 + 1));
            trailer.set("W", Object::Array(vec![Object::Integer(1), Object::Integer(4), Object::Integer(2)]));
            trailer.set("Index", Object::Array(index));
            trailer.set("Length", Object::Integer(data.len() as i64));
            let _ = writeln!(out, "{xid} 0 obj");
            out.extend_from_slice(&ser(&Object::Dictionary(trailer)));
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&data);
            out.extend_from_slice(b"\nendstream\nendobj\n");
            let _ = write!(out, "startxref\n{xref_at}\n%%EOF\n");
        }
        Ok(out)
    }
}

/// Offset des letzten Querverweises (aus „startxref“ am Dateiende).
pub fn startxref(bytes: &[u8]) -> Option<usize> {
    let tail_start = bytes.len().saturating_sub(4096);
    let tail = &bytes[tail_start..];
    let pos = tail.windows(9).rposition(|w| w == b"startxref")?;
    let rest = &tail[pos + 9..];
    let digits: String = rest
        .iter()
        .skip_while(|b| b.is_ascii_whitespace())
        .take_while(|b| b.is_ascii_digit())
        .map(|&b| b as char)
        .collect();
    let v: usize = digits.parse().ok()?;
    (v < bytes.len()).then_some(v)
}

/// Prüft Eingaben aus der Oberfläche (Anzahl, Werte endlich, im Bereich).
pub fn check_layers(layers: &[Layer], pages: usize) -> Result<()> {
    let strokes: usize = layers.iter().map(|l| l.strokes.len()).sum();
    let points: usize = layers.iter().flat_map(|l| &l.strokes).map(|s| s.len()).sum();
    if strokes > MAX_STROKES || points > MAX_POINTS {
        api_bail!(InvalidInput, "Zu viele Striche.");
    }
    for l in layers {
        if l.page >= pages {
            api_bail!(InvalidInput, "Seite {} gibt es nicht.", l.page + 1);
        }
        let ok = |v: f32| v.is_finite() && (-0.01..=1.01).contains(&v);
        if !l.color.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c))
            || !(0.0005..=0.2).contains(&l.width)
            || !(0.0..=1.0).contains(&l.opacity)
            || !l.strokes.iter().flatten().all(|&(u, v)| ok(u) && ok(v))
        {
            api_bail!(InvalidInput, "Ungültige Zeichnung.");
        }
    }
    Ok(())
}

/// Textmarker bzw. Zeichnungen in ein PDF übernehmen (neue Bytes, Original unverändert davor).
pub fn draw(orig: &[u8], layers: &[Layer]) -> Result<Vec<u8>> {
    let mut up = Update::new(orig)?;
    let pages = up.doc.get_pages().len();
    check_layers(layers, pages)?;
    let mut by_page: std::collections::BTreeMap<usize, Vec<&Layer>> = Default::default();
    for l in layers.iter().filter(|l| !l.strokes.is_empty()) {
        by_page.entry(l.page).or_default().push(l);
    }
    if by_page.is_empty() {
        api_bail!(InvalidInput, "Nichts markiert.");
    }
    for (page, ls) in by_page {
        up.draw_on_page(page, &ls, &[], None)?;
    }
    up.finish()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Kleines PDF mit `n` Seiten (A4), klassischer Querverweis-Tabelle.
    pub fn sample_pdf(n: usize, rotate: i64) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offs = vec![];
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + 2 * i)).collect();
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!("<< /Type /Pages /Kids [{}] /Count {n} /MediaBox [0 0 595 842] >>", kids.join(" ")),
        ];
        for i in 0..n {
            objs.push(format!("<< /Type /Page /Parent 2 0 R /Rotate {rotate} /Contents {} 0 R /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>", 4 + 2 * i));
            let body = format!("BT /F1 12 Tf 72 720 Td (Seite {}) Tj ET", i + 1);
            objs.push(format!("<< /Length {} >>\nstream\n{body}\nendstream", body.len()));
        }
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let x = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
        for o in offs {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n", objs.len() + 1).as_bytes(),
        );
        out
    }

    #[test]
    fn draw_keeps_original_and_adds_content() {
        let orig = sample_pdf(2, 0);
        let hl = Layer {
            page: 1,
            strokes: vec![vec![(0.1, 0.1), (0.5, 0.1)]],
            color: [1.0, 0.9, 0.0],
            width: 0.02,
            opacity: 0.35,
            multiply: true,
        };
        let out = draw(&orig, std::slice::from_ref(&hl)).unwrap();
        assert!(out.starts_with(&orig), "Original bleibt Byte für Byte erhalten");
        let doc = load(&out).unwrap();
        let pid = page_id(&doc, 1).unwrap();
        let content = doc.get_page_content(pid);
        let text = String::from_utf8_lossy(&content);
        assert!(text.contains("Seite 2"), "alter Inhalt bleibt");
        assert!(text.contains(" RG") && text.contains(" l\n"), "{text}");
        // Zweites Update auf das erste (Kette über /Prev)
        let out2 = draw(&out, &[Layer { page: 0, ..hl.clone() }]).unwrap();
        assert!(out2.starts_with(&out));
        assert!(load(&out2).is_ok());
        // Ungültige Eingaben
        assert!(draw(&orig, &[Layer { page: 5, ..hl.clone() }]).is_err());
        assert!(draw(&orig, &[Layer { width: f32::NAN, ..hl.clone() }]).is_err());
        assert!(draw(&orig, &[Layer { strokes: vec![], ..hl.clone() }]).is_err());
        assert!(draw(b"keine pdf", std::slice::from_ref(&hl)).is_err());
    }

    #[test]
    fn mapping_follows_rotation() {
        let g = |r| PageGeom { crop: [0.0, 0.0, 100.0, 200.0], rotate: r };
        assert_eq!(g(0).map(0.0, 0.0), (0.0, 200.0)); // oben links
        assert_eq!(g(90).map(0.0, 0.0), (0.0, 0.0));
        assert_eq!(g(90).map(1.0, 0.0), (0.0, 200.0));
        assert_eq!(g(180).map(0.0, 0.0), (100.0, 0.0));
        assert_eq!(g(270).map(0.0, 0.0), (100.0, 200.0));
        assert_eq!(g(90).display_width(), 200.0);
        assert_eq!(pdf_string("a(b)\\ä€☃"), b"(a\\(b\\)\\\\\xe4\x80?)".to_vec());
    }
}
