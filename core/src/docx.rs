//! Einfache Leseansicht für Word-Dateien (DOCX).
//!
//! Liest Text, Überschriften, Listen, Tabellen, Links und Bilder aus – genug zum Lesen, ohne das
//! Layout nachzubauen. DOCX kommt von außen und wird als feindlich behandelt:
//! - Es ist ein ZIP: jeder Teil wird höchstens bis [`MAX_PART`] Bytes gelesen (gemessen, nicht aus
//!   den Kopfdaten), höchstens [`MAX_BLOCKS`] Blöcke und [`MAX_TEXT`] Zeichen Text.
//! - XML wird ohne DTD und ohne eigene Entitäten gelesen (nur `&amp;` usw. und Zeichencodes) –
//!   keine XXE- oder „Billion Laughs“-Angriffe.
//! - Bilder nur, wenn sie innerhalb von `word/` liegen, mit bekannter Endung und höchstens
//!   [`MAX_IMAGE`] Bytes; sie werden unter festen, eigenen Namen in `image_dir` abgelegt.
//! - Links nur `http(s)` und `mailto`.

use crate::api_bail;
use anyhow::{Context, Result};
use quick_xml::events::{BytesStart, Event};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const MAX_PART: u64 = 64 * 1024 * 1024;
pub const MAX_BLOCKS: usize = 20_000;
pub const MAX_TEXT: usize = 16 * 1024 * 1024;
pub const MAX_IMAGE: u64 = 20 * 1024 * 1024;
pub const MAX_IMAGES: usize = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DocBlock {
    Heading { level: u8, inline: Vec<Span> },
    Paragraph { inline: Vec<Span> },
    List { ordered: bool, items: Vec<Vec<Span>> },
    Table { rows: Vec<Vec<String>> },
    Image { file: PathBuf },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocxDoc {
    pub blocks: Vec<DocBlock>,
    /// Grenzen erreicht – nur der Anfang wird gezeigt.
    pub truncated: bool,
}

/// Liest einen Teil des ZIP vollständig, aber höchstens `MAX_PART` Bytes. `None`, wenn er fehlt.
fn read_part<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str, max: u64) -> Result<Option<Vec<u8>>> {
    let entry = match zip.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => api_bail!(InvalidInput, "DOCX ist beschädigt: {e}"),
    };
    let mut out = Vec::new();
    entry.take(max + 1).read_to_end(&mut out).map_err(|e| {
        crate::api::error::ApiError::new(
            crate::api::error::ErrorCode::InvalidInput,
            format!("DOCX ist beschädigt: {e}"),
        )
    })?;
    if out.len() as u64 > max {
        api_bail!(InvalidInput, "DOCX-Teil {name} ist zu groß.");
    }
    Ok(Some(out))
}

/// Wert eines Attributs nach lokalem Namen (Präfix wie `w:` oder `r:` egal).
fn attr(e: &BytesStart, local: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref().as_bytes() == local)
        .and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
}

/// Text eines Entitäten-Verweises: nur die fünf vordefinierten und Zeichencodes.
fn entity(r: &quick_xml::events::BytesRef) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    let name: &str = r;
    Some(
        match name {
            "amp" => "&",
            "lt" => "<",
            "gt" => ">",
            "quot" => "\"",
            "apos" => "'",
            _ => return None,
        }
        .to_string(),
    )
}

fn reader(xml: &[u8]) -> quick_xml::Reader<&[u8]> {
    let mut r = quick_xml::Reader::from_reader(xml);
    r.config_mut().check_end_names = false;
    r
}

/// Beziehungen `rId → Ziel` (Bilder relativ zu `word/`, Links extern).
fn parse_rels(xml: &[u8]) -> HashMap<String, (String, bool)> {
    let mut out = HashMap::new();
    let mut r = reader(xml);
    let mut buf = Vec::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref().as_bytes() == b"Relationship" => {
                if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    let external = attr(&e, b"TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
                    if out.len() < 10_000 {
                        out.insert(id, (target, external));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Stil-ID → Überschriftsebene (1–6), aus Namen wie „heading 2“/„Title“ oder `outlineLvl`.
fn parse_styles(xml: &[u8]) -> HashMap<String, u8> {
    let mut out = HashMap::new();
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut cur: Option<String> = None;
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref().as_bytes() {
                b"style" => cur = attr(&e, b"styleId"),
                b"name" => {
                    if let (Some(id), Some(name)) = (cur.as_ref(), attr(&e, b"val")) {
                        let n = name.to_ascii_lowercase();
                        let level = if n == "title" {
                            Some(1)
                        } else if n == "subtitle" {
                            Some(2)
                        } else {
                            n.strip_prefix("heading ").and_then(|d| d.trim().parse::<u8>().ok())
                        };
                        if let Some(l) = level {
                            out.insert(id.clone(), l.clamp(1, 6));
                        }
                    }
                }
                b"outlineLvl" => {
                    if let (Some(id), Some(v)) = (cur.as_ref(), attr(&e, b"val").and_then(|v| v.parse::<u8>().ok())) {
                        if v < 9 {
                            out.entry(id.clone()).or_insert((v + 1).min(6));
                        }
                    }
                }
                _ => {}
            },
            Event::End(e) if e.local_name().as_ref().as_bytes() == b"style" => cur = None,
            Event::Eof => break,
            _ => {}
        }
        if out.len() > 10_000 {
            break;
        }
        buf.clear();
    }
    out
}

/// numId → nummeriert (true) oder Aufzählungszeichen (false), aus der ersten Ebene.
fn parse_numbering(xml: &[u8]) -> HashMap<String, bool> {
    let mut abs_fmt: HashMap<String, bool> = HashMap::new();
    let mut num_abs: HashMap<String, String> = HashMap::new();
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let (mut cur_abs, mut cur_num, mut lvl0) = (None::<String>, None::<String>, false);
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref().as_bytes() {
                b"abstractNum" => cur_abs = attr(&e, b"abstractNumId"),
                b"lvl" => lvl0 = attr(&e, b"ilvl").as_deref() == Some("0"),
                b"numFmt" if lvl0 => {
                    if let (Some(a), Some(f)) = (cur_abs.as_ref(), attr(&e, b"val")) {
                        abs_fmt.entry(a.clone()).or_insert(f != "bullet" && f != "none");
                    }
                }
                b"num" => cur_num = attr(&e, b"numId"),
                b"abstractNumId" => {
                    if let (Some(n), Some(a)) = (cur_num.as_ref(), attr(&e, b"val")) {
                        num_abs.insert(n.clone(), a);
                    }
                }
                _ => {}
            },
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"abstractNum" => cur_abs = None,
                b"num" => cur_num = None,
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        if abs_fmt.len() + num_abs.len() > 20_000 {
            break;
        }
        buf.clear();
    }
    num_abs.into_iter().map(|(n, a)| (n, abs_fmt.get(&a).copied().unwrap_or(false))).collect()
}

/// Bildziel aus den Beziehungen in einen sicheren ZIP-Namen unter `word/` übersetzen.
fn image_part(target: &str) -> Option<String> {
    let t = target.replace('\\', "/");
    if t.starts_with('/') || t.contains("://") {
        return None;
    }
    let mut parts: Vec<&str> = vec!["word"];
    for p in t.split('/') {
        match p {
            "" | "." => {}
            ".." => return None,
            x => parts.push(x),
        }
    }
    let name = parts.join("/");
    let lower = name.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp"].iter().any(|e| lower.ends_with(e)).then_some(name)
}

fn safe_link(url: &str) -> Option<String> {
    let l = url.to_ascii_lowercase();
    (l.starts_with("https://") || l.starts_with("http://") || l.starts_with("mailto:")).then(|| url.to_string())
}

#[derive(Default)]
struct Para {
    style: Option<String>,
    outline: Option<u8>,
    num: Option<String>,
    spans: Vec<Span>,
    images: Vec<String>,
}

/// Liest ein DOCX. Bilder werden nach `image_dir` geschrieben (wenn angegeben), sonst ausgelassen.
pub fn read_docx(path: &Path, image_dir: Option<&Path>) -> Result<DocxDoc> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let file = std::fs::File::open(path).with_context(|| format!("{} nicht lesbar", path.display()))?;
    let meta = file.metadata()?;
    let mut zip = match zip::ZipArchive::new(std::io::BufReader::new(file)) {
        Ok(z) => z,
        Err(_) => api_bail!(InvalidInput, "{} ist keine gültige DOCX-Datei.", path.display()),
    };
    if zip.len() > 20_000 {
        api_bail!(InvalidInput, "DOCX hat zu viele Teile.");
    }
    let Some(document) = read_part(&mut zip, "word/document.xml", MAX_PART)? else {
        api_bail!(InvalidInput, "{} ist keine gültige DOCX-Datei (word/document.xml fehlt).", path.display());
    };
    let rels =
        read_part(&mut zip, "word/_rels/document.xml.rels", MAX_PART)?.map(|x| parse_rels(&x)).unwrap_or_default();
    let styles = read_part(&mut zip, "word/styles.xml", MAX_PART)?.map(|x| parse_styles(&x)).unwrap_or_default();
    let numbering =
        read_part(&mut zip, "word/numbering.xml", MAX_PART)?.map(|x| parse_numbering(&x)).unwrap_or_default();

    // Bilder unter eindeutigem Präfix je Datei und Stand ablegen
    let stamp = {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let h = blake3::hash(format!("{}|{}|{}", path.display(), mtime, meta.len()).as_bytes());
        h.to_hex()[..16].to_string()
    };
    let mut images_written = 0usize;
    let mut image_cache: HashMap<String, Option<PathBuf>> = HashMap::new();
    let mut extract_image =
        |zip: &mut zip::ZipArchive<std::io::BufReader<std::fs::File>>, part: &str| -> Option<PathBuf> {
            if let Some(v) = image_cache.get(part) {
                return v.clone();
            }
            let dir = image_dir?;
            if images_written >= MAX_IMAGES {
                return None;
            }
            let data = read_part(zip, part, MAX_IMAGE).ok().flatten()?;
            let ext = part.rsplit('.').next().unwrap_or("png").to_ascii_lowercase();
            let out = dir.join(format!("docx-{stamp}-{images_written}.{ext}"));
            let tmp = dir.join(format!(".docx-{stamp}-{images_written}.tmp"));
            std::fs::create_dir_all(dir).ok()?;
            std::fs::write(&tmp, &data).ok()?;
            std::fs::rename(&tmp, &out).ok()?;
            images_written += 1;
            image_cache.insert(part.to_string(), Some(out.clone()));
            Some(out)
        };

    let mut doc = DocxDoc::default();
    let mut text_len = 0usize;
    let mut r = reader(&document);
    let mut buf = Vec::new();
    let mut para: Option<Para> = None;
    let (mut bold, mut italic, mut in_rpr, mut in_text) = (false, false, false, false);
    let mut link: Option<String> = None;
    // Tabellen: Stapel (verschachtelte werden in die Zelle der äußeren geschrieben)
    let mut tables: Vec<Vec<Vec<String>>> = Vec::new();
    let mut deleted = 0u32; // gelöschte Änderungen (w:del) nicht anzeigen

    let push_text =
        |para: &mut Option<Para>, text: &str, bold: bool, italic: bool, link: &Option<String>, text_len: &mut usize| {
            let Some(p) = para.as_mut() else { return };
            if text.is_empty() || *text_len >= MAX_TEXT {
                return;
            }
            *text_len += text.len();
            if let Some(last) = p.spans.last_mut() {
                if last.bold == bold && last.italic == italic && &last.link == link {
                    last.text.push_str(text);
                    return;
                }
            }
            p.spans.push(Span { text: text.to_string(), bold, italic, link: link.clone() });
        };

    loop {
        if doc.blocks.len() >= MAX_BLOCKS || text_len >= MAX_TEXT {
            doc.truncated = true;
            break;
        }
        let ev = match r.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => {
                // Kaputtes XML: zeigen, was bis hierhin lesbar war
                doc.truncated = true;
                break;
            }
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref().as_bytes() {
                    b"p" if !empty => para = Some(Para::default()),
                    b"pStyle" => {
                        if let Some(p) = para.as_mut() {
                            p.style = attr(e, b"val");
                        }
                    }
                    b"outlineLvl" => {
                        if let (Some(p), Some(v)) = (para.as_mut(), attr(e, b"val").and_then(|v| v.parse::<u8>().ok()))
                        {
                            if v < 9 {
                                p.outline = Some((v + 1).min(6));
                            }
                        }
                    }
                    b"numId" => {
                        if let Some(p) = para.as_mut() {
                            p.num = attr(e, b"val").filter(|v| v != "0");
                        }
                    }
                    b"r" if !empty => {
                        bold = false;
                        italic = false;
                    }
                    b"rPr" if !empty => in_rpr = true,
                    b"b" if in_rpr => bold = !matches!(attr(e, b"val").as_deref(), Some("0" | "false")),
                    b"i" if in_rpr => italic = !matches!(attr(e, b"val").as_deref(), Some("0" | "false")),
                    b"t" if !empty => in_text = true,
                    b"tab" if deleted == 0 && !in_rpr => push_text(&mut para, "\t", bold, italic, &link, &mut text_len),
                    b"br" | b"cr" if deleted == 0 => push_text(&mut para, "\n", bold, italic, &link, &mut text_len),
                    b"del" if !empty => deleted += 1,
                    b"hyperlink" if !empty => {
                        link = attr(e, b"id")
                            .and_then(|id| rels.get(&id).filter(|(_, ext)| *ext).and_then(|(t, _)| safe_link(t)));
                    }
                    b"blip" => {
                        if let (Some(p), Some(id)) = (para.as_mut(), attr(e, b"embed")) {
                            if let Some(part) = rels.get(&id).filter(|(_, ext)| !*ext).and_then(|(t, _)| image_part(t))
                            {
                                p.images.push(part);
                            }
                        }
                    }
                    b"tbl" if !empty => tables.push(Vec::new()),
                    b"tr" if !empty => {
                        if let Some(t) = tables.last_mut() {
                            if t.len() < 5000 {
                                t.push(Vec::new());
                            }
                        }
                    }
                    b"tc" if !empty => {
                        if let Some(row) = tables.last_mut().and_then(|t| t.last_mut()) {
                            if row.len() < 100 {
                                row.push(String::new());
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) if in_text && deleted == 0 => {
                let s = t.xml10_content();
                push_text(&mut para, &s, bold, italic, &link, &mut text_len);
            }
            Event::GeneralRef(g) if in_text && deleted == 0 => {
                if let Some(s) = entity(&g) {
                    push_text(&mut para, &s, bold, italic, &link, &mut text_len);
                }
            }
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"t" => in_text = false,
                b"rPr" => in_rpr = false,
                b"del" => deleted = deleted.saturating_sub(1),
                b"hyperlink" => link = None,
                b"p" => {
                    let Some(p) = para.take() else { continue };
                    let text: String = p.spans.iter().map(|s| s.text.as_str()).collect();
                    if let Some(row) = tables.last_mut().and_then(|t| t.last_mut()) {
                        // In einer Tabellenzelle: nur Text sammeln
                        if let Some(cell) = row.last_mut() {
                            if !text.trim().is_empty() && cell.len() < 10_000 {
                                if !cell.is_empty() {
                                    cell.push('\n');
                                }
                                cell.push_str(text.trim());
                            }
                        }
                        continue;
                    }
                    if !text.trim().is_empty() {
                        let level = p.style.as_ref().and_then(|s| styles.get(s).copied()).or(p.outline);
                        if let Some(level) = level {
                            doc.blocks.push(DocBlock::Heading { level, inline: p.spans });
                        } else if let Some(num) = &p.num {
                            let ordered = numbering.get(num).copied().unwrap_or(false);
                            match doc.blocks.last_mut() {
                                Some(DocBlock::List { ordered: o, items }) if *o == ordered && items.len() < 10_000 => {
                                    items.push(p.spans)
                                }
                                _ => doc.blocks.push(DocBlock::List { ordered, items: vec![p.spans] }),
                            }
                        } else {
                            doc.blocks.push(DocBlock::Paragraph { inline: p.spans });
                        }
                    }
                    for part in &p.images {
                        if let Some(file) = extract_image(&mut zip, part) {
                            doc.blocks.push(DocBlock::Image { file });
                        }
                    }
                }
                b"tbl" => {
                    if let Some(rows) = tables.pop() {
                        let rows: Vec<Vec<String>> =
                            rows.into_iter().filter(|r| r.iter().any(|c| !c.is_empty())).collect();
                        if let Some(outer_cell) =
                            tables.last_mut().and_then(|t| t.last_mut()).and_then(|r| r.last_mut())
                        {
                            // Verschachtelte Tabelle: als Text in die Zelle der äußeren
                            for row in rows {
                                if outer_cell.len() < 10_000 {
                                    if !outer_cell.is_empty() {
                                        outer_cell.push('\n');
                                    }
                                    outer_cell.push_str(&row.join(" | "));
                                }
                            }
                        } else if !rows.is_empty() {
                            doc.blocks.push(DocBlock::Table { rows });
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Baut ein minimales DOCX aus den angegebenen Teilen.
    pub(crate) fn make_docx(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, data) in parts {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="w" xmlns:r="r" xmlns:a="a"><w:body>
<w:p><w:pPr><w:pStyle w:val="berschrift1"/></w:pPr><w:r><w:t>Titel &amp; mehr</w:t></w:r></w:p>
<w:p><w:r><w:t xml:space="preserve">Normal </w:t></w:r><w:r><w:rPr><w:b/></w:rPr><w:t>fett</w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t> kursiv</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Punkt eins</w:t></w:r></w:p>
<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Punkt zwei</w:t></w:r></w:p>
<w:p><w:hyperlink r:id="rIdLink"><w:r><w:t>Webseite</w:t></w:r></w:hyperlink><w:hyperlink r:id="rIdBad"><w:r><w:t> böse</w:t></w:r></w:hyperlink></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
<w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
<w:p><w:r><w:drawing><a:blip r:embed="rIdImg"/></w:drawing></w:r></w:p>
<w:p><w:r><w:drawing><a:blip r:embed="rIdEvil"/></w:drawing></w:r></w:p>
<w:p><w:del><w:r><w:t>gelöscht</w:t></w:r></w:del><w:r><w:t>sichtbar</w:t></w:r></w:p>
</w:body></w:document>"#;
    const RELS: &str = r#"<Relationships>
<Relationship Id="rIdLink" Target="https://example.org" TargetMode="External"/>
<Relationship Id="rIdBad" Target="javascript:alert(1)" TargetMode="External"/>
<Relationship Id="rIdImg" Target="media/bild.png"/>
<Relationship Id="rIdEvil" Target="../../../etc/passwd.png"/>
</Relationships>"#;
    const STYLES: &str =
        r#"<w:styles xmlns:w="w"><w:style w:styleId="berschrift1"><w:name w:val="heading 1"/></w:style></w:styles>"#;
    const NUMBERING: &str = r#"<w:numbering xmlns:w="w"><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="7"/></w:num></w:numbering>"#;

    #[test]
    fn liest_struktur_und_haelt_sich_an_grenzen() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("brief.docx");
        std::fs::write(
            &p,
            make_docx(&[
                ("word/document.xml", DOC.as_bytes()),
                ("word/_rels/document.xml.rels", RELS.as_bytes()),
                ("word/styles.xml", STYLES.as_bytes()),
                ("word/numbering.xml", NUMBERING.as_bytes()),
                ("word/media/bild.png", b"PNGDATA"),
            ]),
        )
        .unwrap();
        let imgs = d.path().join("bilder");
        let doc = read_docx(&p, Some(&imgs)).unwrap();
        let kinds: Vec<&str> = doc
            .blocks
            .iter()
            .map(|b| match b {
                DocBlock::Heading { .. } => "h",
                DocBlock::Paragraph { .. } => "p",
                DocBlock::List { .. } => "l",
                DocBlock::Table { .. } => "t",
                DocBlock::Image { .. } => "i",
            })
            .collect();
        assert_eq!(kinds, ["h", "p", "l", "p", "t", "i", "p"]);
        assert!(matches!(&doc.blocks[0], DocBlock::Heading { level: 1, inline } if inline[0].text == "Titel & mehr"));
        let DocBlock::Paragraph { inline } = &doc.blocks[1] else { panic!() };
        assert_eq!(
            inline.iter().map(|s| (s.text.as_str(), s.bold, s.italic)).collect::<Vec<_>>(),
            [("Normal ", false, false), ("fett", true, false), (" kursiv", false, true)]
        );
        assert!(matches!(&doc.blocks[2], DocBlock::List { ordered: true, items } if items.len() == 2));
        // Nur sichere Links; javascript: wird zu reinem Text
        let DocBlock::Paragraph { inline } = &doc.blocks[3] else { panic!() };
        assert_eq!(inline[0].link.as_deref(), Some("https://example.org"));
        assert_eq!(inline[1].link, None);
        assert!(
            matches!(&doc.blocks[4], DocBlock::Table { rows } if rows == &vec![vec!["A1".to_string(), "B1".into()], vec!["A2".into(), "B2".into()]])
        );
        // Bild aus word/media in den Bildordner, das mit Pfad nach außen nicht
        let DocBlock::Image { file } = &doc.blocks[5] else { panic!() };
        assert!(file.starts_with(&imgs));
        assert_eq!(std::fs::read(file).unwrap(), b"PNGDATA");
        assert_eq!(std::fs::read_dir(&imgs).unwrap().count(), 1);
        // Gelöschte Änderungen nicht anzeigen
        assert!(matches!(&doc.blocks[6], DocBlock::Paragraph { inline } if inline[0].text == "sichtbar"));
        assert!(!doc.truncated);
        // Ohne Bildordner: keine Bilder, keine Dateien
        let doc = read_docx(&p, None).unwrap();
        assert!(!doc.blocks.iter().any(|b| matches!(b, DocBlock::Image { .. })));
    }

    #[test]
    fn feindliche_eingaben() {
        let d = tempfile::tempdir().unwrap();
        // Kein ZIP, ZIP ohne document.xml
        let p = d.path().join("x.docx");
        std::fs::write(&p, b"kein zip").unwrap();
        assert!(read_docx(&p, None).is_err());
        std::fs::write(&p, make_docx(&[("andere.xml", b"<a/>")])).unwrap();
        assert!(read_docx(&p, None).is_err());
        // Entitäten aus einer DTD werden nicht aufgelöst (kein XXE / Billion Laughs)
        let xxe = r#"<?xml version="1.0"?><!DOCTYPE d [<!ENTITY x SYSTEM "file:///etc/passwd"><!ENTITY a "aaaaaaaaaa"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">]>
<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>vor &x; &b; nach</w:t></w:r></w:p></w:body></w:document>"#;
        std::fs::write(&p, make_docx(&[("word/document.xml", xxe.as_bytes())])).unwrap();
        let doc = read_docx(&p, None).unwrap();
        let text: String = doc
            .blocks
            .iter()
            .map(|b| match b {
                DocBlock::Paragraph { inline } => inline.iter().map(|s| s.text.clone()).collect(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(text, "vor   nach");
        assert!(!text.contains("root") && !text.contains("aaaa"));
        // Kaputtes XML: zeigt den lesbaren Anfang, statt zu scheitern
        let broken = r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>Anfang</w:t></w:r></w:p><w:p><w:r><w:t>"#;
        std::fs::write(&p, make_docx(&[("word/document.xml", broken.as_bytes())])).unwrap();
        let doc = read_docx(&p, None).unwrap();
        assert!(matches!(&doc.blocks[0], DocBlock::Paragraph { inline } if inline[0].text == "Anfang"));
        assert_eq!(image_part("media/a.png").as_deref(), Some("word/media/a.png"));
        assert_eq!(image_part("../x.png"), None);
        assert_eq!(image_part("/abs.png"), None);
        assert_eq!(image_part("media/a.exe"), None);
        assert_eq!(image_part("https://x/y.png"), None);
    }
}
