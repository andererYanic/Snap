//! Leseansicht für weitere Dokumentformate und Inhaltslisten von Archiven.
//!
//! - OpenDocument (ODT Text, ODS Tabellen, ODP Präsentationen), EPUB, PowerPoint (PPTX),
//!   Excel (XLSX) – alles ZIP + XML, gelesen mit denselben Schutzregeln wie DOCX (`docx.rs`):
//!   gemessene Teilgrößen, keine DTD/eigenen Entitäten, feste Obergrenzen für Blöcke, Text,
//!   Tabellenzeilen und -spalten (wiederholte Zellen in ODS werden nicht aufgebläht).
//! - Ergebnis ist dieselbe Blockstruktur wie bei DOCX (Überschrift, Absatz, Liste, Tabelle).
//! - Archive (ZIP, TAR, TAR.GZ): nur die Liste der Einträge, ohne etwas zu entpacken.

use crate::api_bail;
use crate::docx::{attr, entity, read_part, reader, DocBlock, DocxDoc, Span, MAX_BLOCKS, MAX_PART, MAX_TEXT};
use anyhow::{Context, Result};
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

pub const MAX_ROWS: usize = 5_000;
pub const MAX_COLS: usize = 200;
pub const MAX_CELL: usize = 2_000;
pub const MAX_SLIDES: usize = 2_000;
pub const MAX_CHAPTERS: usize = 2_000;
pub const MAX_ENTRIES: usize = 10_000;

type Zip = zip::ZipArchive<std::io::BufReader<std::fs::File>>;

/// Formate, die der Kern als Leseansicht liefert (nach Endung).
pub fn supported(path: &Path) -> bool {
    matches!(ext(path).as_str(), "docx" | "odt" | "ods" | "odp" | "epub" | "pptx" | "xlsx")
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

fn open_zip(path: &Path, what: &str) -> Result<Zip> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let file = std::fs::File::open(path).with_context(|| format!("{} nicht lesbar", path.display()))?;
    let zip = match zip::ZipArchive::new(std::io::BufReader::new(file)) {
        Ok(z) => z,
        Err(_) => api_bail!(InvalidInput, "{} ist keine gültige {what}-Datei.", path.display()),
    };
    if zip.len() > 20_000 {
        api_bail!(InvalidInput, "{what} hat zu viele Teile.");
    }
    Ok(zip)
}

/// Liest ein Dokument je nach Endung (DOCX über `docx.rs`).
pub fn read_document(path: &Path, image_dir: Option<&Path>) -> Result<DocxDoc> {
    match ext(path).as_str() {
        "docx" => crate::docx::read_docx(path, image_dir),
        "odt" | "ods" | "odp" => read_odf(path),
        "epub" => read_epub(path),
        "pptx" => read_pptx(path),
        "xlsx" => read_xlsx(path),
        _ => api_bail!(InvalidInput, "Dieses Format kann Snap nicht anzeigen."),
    }
}

/// Sammelt Blöcke unter den gemeinsamen Grenzen.
#[derive(Default)]
struct Out {
    blocks: Vec<DocBlock>,
    text: usize,
    truncated: bool,
}

impl Out {
    fn full(&self) -> bool {
        self.truncated || self.blocks.len() >= MAX_BLOCKS || self.text >= MAX_TEXT
    }
    fn push(&mut self, b: DocBlock) {
        if self.blocks.len() >= MAX_BLOCKS || self.text >= MAX_TEXT {
            self.truncated = true;
            return;
        }
        self.text += match &b {
            DocBlock::Heading { inline, .. } | DocBlock::Paragraph { inline } => {
                inline.iter().map(|s| s.text.len()).sum()
            }
            DocBlock::List { items, .. } => items.iter().flatten().map(|s| s.text.len()).sum(),
            DocBlock::Table { rows } => rows.iter().flatten().map(String::len).sum(),
            DocBlock::Image { .. } => 0,
        };
        self.blocks.push(b);
    }
    fn para(&mut self, text: String) {
        let t = tidy(&text);
        if !t.is_empty() {
            self.push(DocBlock::Paragraph { inline: vec![Span { text: t, ..Default::default() }] });
        }
    }
    fn heading(&mut self, level: u8, text: String) {
        let t = tidy(&text);
        if !t.is_empty() {
            self.push(DocBlock::Heading {
                level: level.clamp(1, 6),
                inline: vec![Span { text: t, ..Default::default() }],
            });
        }
    }
    fn done(self) -> DocxDoc {
        DocxDoc { blocks: self.blocks, truncated: self.truncated }
    }
}

/// Leerraum zusammenfassen, Steuerzeichen entfernen.
fn tidy(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|c| !c.is_control()).collect()
}

fn cell_text(s: &str) -> String {
    let t = tidy(s);
    if t.len() > MAX_CELL {
        let mut end = MAX_CELL;
        while !t.is_char_boundary(end) {
            end -= 1;
        }
        return format!("{}…", &t[..end]);
    }
    t
}

/// Tabelle ohne leere Zeilen am Ende und ohne leere Spalten rechts.
fn trim_table(mut rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    while rows.last().is_some_and(|r| r.iter().all(String::is_empty)) {
        rows.pop();
    }
    let width = rows.iter().map(|r| r.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1)).max().unwrap_or(0);
    for r in &mut rows {
        r.truncate(width);
        r.resize(width, String::new());
    }
    rows
}

// ------------------------------------------------------------ OpenDocument

/// ODT/ODS/ODP: `content.xml` – Überschriften, Absätze, Listen, Tabellen, Folien.
pub fn read_odf(path: &Path) -> Result<DocxDoc> {
    let mut zip = open_zip(path, "OpenDocument")?;
    let Some(xml) = read_part(&mut zip, "content.xml", MAX_PART)? else {
        api_bail!(InvalidInput, "{} ist keine gültige OpenDocument-Datei (content.xml fehlt).", path.display());
    };
    Ok(parse_odf(&xml))
}

pub fn parse_odf(xml: &[u8]) -> DocxDoc {
    let mut out = Out::default();
    let mut r = reader(xml);
    let mut buf = Vec::new();
    // Zustand
    let mut text: Option<String> = None; // laufender Absatz/Überschrift
    let mut heading: Option<u8> = None;
    let mut list_depth = 0usize;
    let mut list_items: Vec<Vec<Span>> = Vec::new();
    let mut table: Option<Vec<Vec<String>>> = None;
    let mut table_name: Option<String> = None;
    let mut row: Option<Vec<String>> = None;
    let mut row_repeat = 1usize;
    let mut cell: Option<(String, usize)> = None; // Text, Wiederholungen
    let mut slide = 0usize;
    loop {
        if out.full() {
            out.truncated = true;
            break;
        }
        let ev = match r.read_event_into(&mut buf) {
            Ok(e) => e,
            Err(_) => break,
        };
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref().as_bytes() {
                    b"page" => {
                        slide += 1;
                        if slide > MAX_SLIDES {
                            out.truncated = true;
                            break;
                        }
                        let name = attr(e, b"name").filter(|n| !n.starts_with("page")).unwrap_or_default();
                        out.heading(
                            2,
                            if name.is_empty() { format!("Folie {slide}") } else { format!("Folie {slide}: {name}") },
                        );
                    }
                    b"h" if !empty && cell.is_none() => {
                        heading = Some(attr(e, b"outline-level").and_then(|v| v.parse().ok()).unwrap_or(1));
                        text = Some(String::new());
                    }
                    b"p" if !empty => {
                        if let Some(c) = cell.as_mut() {
                            // Mehrere Absätze in einer Zelle durch Leerzeichen trennen
                            if !c.0.is_empty() {
                                c.0.push(' ');
                            }
                        } else if text.is_none() {
                            text = Some(String::new());
                        }
                    }
                    b"s" => {
                        let n: usize = attr(e, b"c").and_then(|v| v.parse().ok()).unwrap_or(1).min(20);
                        if let Some(t) = text.as_mut().or(cell.as_mut().map(|c| &mut c.0)) {
                            t.push_str(&" ".repeat(n));
                        }
                    }
                    b"tab" | b"line-break" => {
                        if let Some(t) = text.as_mut() {
                            t.push(' ');
                        }
                    }
                    b"list" if !empty => list_depth += 1,
                    b"table" if !empty => {
                        table = Some(Vec::new());
                        table_name = attr(e, b"name");
                    }
                    b"table-row" if table.is_some() => {
                        row_repeat = attr(e, b"number-rows-repeated")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_ROWS);
                        row = Some(Vec::new());
                        if empty {
                            // leere Zeile: nur einmal, nie aufblähen
                            if let (Some(t), Some(rw)) = (table.as_mut(), row.take()) {
                                if t.len() < MAX_ROWS {
                                    t.push(rw);
                                }
                            }
                        }
                    }
                    b"table-cell" | b"covered-table-cell" if row.is_some() => {
                        let rep = attr(e, b"number-columns-repeated")
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(1)
                            .clamp(1, MAX_COLS);
                        if empty {
                            push_cell(row.as_mut(), String::new(), rep);
                        } else {
                            cell = Some((String::new(), rep));
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                let s = t.xml10_content();
                if let Some(c) = cell.as_mut() {
                    c.0.push_str(&s);
                } else if let Some(cur) = text.as_mut() {
                    cur.push_str(&s);
                }
            }
            Event::GeneralRef(g) => {
                if let Some(s) = entity(&g) {
                    if let Some(c) = cell.as_mut() {
                        c.0.push_str(&s);
                    } else if let Some(cur) = text.as_mut() {
                        cur.push_str(&s);
                    }
                }
            }
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"h" if heading.is_some() => {
                    out.heading(heading.take().unwrap_or(1), text.take().unwrap_or_default());
                }
                b"p" if cell.is_none() => {
                    let t = text.take().unwrap_or_default();
                    if list_depth > 0 {
                        let t = tidy(&t);
                        if !t.is_empty() {
                            list_items.push(vec![Span { text: t, ..Default::default() }]);
                        }
                    } else {
                        out.para(t);
                    }
                }
                b"p" => {}
                b"list" => {
                    list_depth = list_depth.saturating_sub(1);
                    if list_depth == 0 && !list_items.is_empty() {
                        out.push(DocBlock::List { ordered: false, items: std::mem::take(&mut list_items) });
                    }
                }
                b"table-cell" | b"covered-table-cell" => {
                    if let Some((t, rep)) = cell.take() {
                        let t = cell_text(&t);
                        // Wiederholte leere Zellen (oft 1000+) nicht aufblähen
                        let rep = if t.is_empty() { rep.min(1) } else { rep };
                        push_cell(row.as_mut(), t, rep);
                    }
                }
                b"table-row" => {
                    if let (Some(t), Some(rw)) = (table.as_mut(), row.take()) {
                        let empty_row = rw.iter().all(String::is_empty);
                        let times = if empty_row { 1 } else { row_repeat };
                        for _ in 0..times {
                            if t.len() >= MAX_ROWS {
                                out.truncated = true;
                                break;
                            }
                            t.push(rw.clone());
                        }
                    }
                }
                b"table" => {
                    if let Some(t) = table.take() {
                        let rows = trim_table(t);
                        if !rows.is_empty() {
                            if let Some(n) = table_name.take() {
                                out.heading(2, n);
                            }
                            out.push(DocBlock::Table { rows });
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
    out.done()
}

fn push_cell(row: Option<&mut Vec<String>>, text: String, rep: usize) {
    if let Some(r) = row {
        for _ in 0..rep {
            if r.len() >= MAX_COLS {
                break;
            }
            r.push(text.clone());
        }
    }
}

// ------------------------------------------------------------ PowerPoint

/// PPTX: Text je Folie (in Folienreihenfolge).
pub fn read_pptx(path: &Path) -> Result<DocxDoc> {
    let mut zip = open_zip(path, "PowerPoint")?;
    let mut slides: Vec<(usize, String)> = zip
        .file_names()
        .filter_map(|n| {
            let n = n.ok()?;
            let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
            num.parse::<usize>().ok().map(|i| (i, n.to_string()))
        })
        .collect();
    if slides.is_empty() {
        api_bail!(InvalidInput, "{} enthält keine Folien.", path.display());
    }
    slides.sort();
    let mut out = Out::default();
    for (i, (_, name)) in slides.iter().enumerate().take(MAX_SLIDES) {
        if out.full() {
            break;
        }
        let Some(xml) = read_part(&mut zip, name, MAX_PART)? else { continue };
        out.heading(2, format!("Folie {}", i + 1));
        for p in drawing_paragraphs(&xml) {
            out.para(p);
        }
    }
    if slides.len() > MAX_SLIDES {
        out.truncated = true;
    }
    Ok(out.done())
}

/// Absätze aus DrawingML (`a:p` mit `a:t`), wie in Folien und Formen.
pub fn drawing_paragraphs(xml: &[u8]) -> Vec<String> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut in_t = false;
    let mut total = 0usize;
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) => match e.local_name().as_ref().as_bytes() {
                b"p" => cur = Some(String::new()),
                b"t" => in_t = true,
                _ => {}
            },
            Event::Empty(e) if e.local_name().as_ref().as_bytes() == b"br" => {
                if let Some(c) = cur.as_mut() {
                    c.push(' ');
                }
            }
            Event::Text(t) if in_t => {
                if let Some(c) = cur.as_mut() {
                    c.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(g) if in_t => {
                if let (Some(c), Some(s)) = (cur.as_mut(), entity(&g)) {
                    c.push_str(&s);
                }
            }
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"t" => in_t = false,
                b"p" => {
                    if let Some(c) = cur.take() {
                        total += c.len();
                        out.push(c);
                        if out.len() >= MAX_BLOCKS || total >= MAX_TEXT {
                            break;
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
    out
}

// ------------------------------------------------------------ Excel

/// XLSX: jede Tabelle als Tabelle (Werte, geteilte Zeichenketten; ohne Formate).
pub fn read_xlsx(path: &Path) -> Result<DocxDoc> {
    let mut zip = open_zip(path, "Excel")?;
    let Some(workbook) = read_part(&mut zip, "xl/workbook.xml", MAX_PART)? else {
        api_bail!(InvalidInput, "{} ist keine gültige Excel-Datei.", path.display());
    };
    let rels = read_part(&mut zip, "xl/_rels/workbook.xml.rels", MAX_PART)?.map(|x| xlsx_rels(&x)).unwrap_or_default();
    let shared = read_part(&mut zip, "xl/sharedStrings.xml", MAX_PART)?.map(|x| shared_strings(&x)).unwrap_or_default();
    let sheets = xlsx_sheets(&workbook);
    let mut out = Out::default();
    for (name, rid) in sheets.into_iter().take(200) {
        if out.full() {
            break;
        }
        let Some(target) = rels.get(&rid) else { continue };
        // Nur Teile unter xl/ (keine Pfade nach außen)
        let part = format!("xl/{}", target.trim_start_matches('/').trim_start_matches("xl/"));
        if part.contains("..") {
            continue;
        }
        let Some(xml) = read_part(&mut zip, &part, MAX_PART)? else { continue };
        let (rows, cut) = xlsx_rows(&xml, &shared);
        if cut {
            out.truncated = true;
        }
        out.heading(2, name);
        let rows = trim_table(rows);
        if rows.is_empty() {
            out.para("(leer)".into());
        } else {
            out.push(DocBlock::Table { rows });
        }
    }
    Ok(out.done())
}

fn xlsx_rels(xml: &[u8]) -> HashMap<String, String> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut out = HashMap::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref().as_bytes() == b"Relationship" => {
                if let (Some(id), Some(t)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    out.insert(id, t);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn xlsx_sheets(xml: &[u8]) -> Vec<(String, String)> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref().as_bytes() == b"sheet" => {
                if let (Some(n), Some(id)) = (attr(&e, b"name"), attr(&e, b"id")) {
                    out.push((tidy(&n), id));
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn shared_strings(xml: &[u8]) -> Vec<String> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut in_t = false;
    let mut total = 0usize;
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) => match e.local_name().as_ref().as_bytes() {
                b"si" => cur = Some(String::new()),
                b"t" => in_t = true,
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Some(c) = cur.as_mut() {
                    if c.len() < MAX_CELL * 2 {
                        c.push_str(&t.xml10_content());
                    }
                }
            }
            Event::GeneralRef(g) if in_t => {
                if let (Some(c), Some(s)) = (cur.as_mut(), entity(&g)) {
                    c.push_str(&s);
                }
            }
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"t" => in_t = false,
                b"si" => {
                    let c = cur.take().unwrap_or_default();
                    total += c.len();
                    out.push(cell_text(&c));
                    if out.len() > 1_000_000 || total > MAX_TEXT {
                        break;
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Spaltenindex aus einer Zellreferenz („B3“ → 1).
fn col_of(r: &str) -> Option<usize> {
    let mut n = 0usize;
    let mut any = false;
    for c in r.chars() {
        if c.is_ascii_alphabetic() {
            n = n.checked_mul(26)?.checked_add((c.to_ascii_uppercase() as u8 - b'A' + 1) as usize)?;
            any = true;
        } else {
            break;
        }
    }
    (any && n > 0).then(|| n - 1)
}

/// Zeilen einer Tabelle; `true`, wenn Grenzen erreicht wurden.
fn xlsx_rows(xml: &[u8], shared: &[String]) -> (Vec<Vec<String>>, bool) {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cur: Option<Vec<String>> = None;
    let mut cell: Option<(usize, String, String)> = None; // Spalte, Typ, Wert
    let mut in_v = false;
    let mut cut = false;
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(ev, Event::Empty(_));
                match e.local_name().as_ref().as_bytes() {
                    b"row" => {
                        // Zeilennummer beachten (übersprungene Zeilen als leer), begrenzt
                        let n: usize = attr(e, b"r").and_then(|v| v.parse().ok()).unwrap_or(rows.len() + 1);
                        if n > MAX_ROWS {
                            cut = true;
                            break;
                        }
                        while rows.len() + 1 < n {
                            rows.push(Vec::new());
                        }
                        cur = Some(Vec::new());
                        if empty {
                            rows.push(cur.take().unwrap_or_default());
                        }
                    }
                    b"c" if cur.is_some() && !empty => {
                        let col =
                            attr(e, b"r").and_then(|v| col_of(&v)).unwrap_or_else(|| cur.as_ref().map_or(0, Vec::len));
                        cell = Some((col, attr(e, b"t").unwrap_or_default(), String::new()));
                    }
                    b"v" | b"t" if cell.is_some() => in_v = true,
                    _ => {}
                }
            }
            Event::Text(t) if in_v => {
                if let Some(c) = cell.as_mut() {
                    if c.2.len() < MAX_CELL * 2 {
                        c.2.push_str(&t.xml10_content());
                    }
                }
            }
            Event::GeneralRef(g) if in_v => {
                if let (Some(c), Some(s)) = (cell.as_mut(), entity(&g)) {
                    c.2.push_str(&s);
                }
            }
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"v" | b"t" => in_v = false,
                b"c" => {
                    if let (Some((col, ty, v)), Some(rw)) = (cell.take(), cur.as_mut()) {
                        if col >= MAX_COLS {
                            cut = true;
                            continue;
                        }
                        let text = match ty.as_str() {
                            "s" => {
                                v.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default()
                            }
                            "b" => {
                                if v.trim() == "1" {
                                    "WAHR".into()
                                } else {
                                    "FALSCH".into()
                                }
                            }
                            _ => cell_text(&v),
                        };
                        if rw.len() <= col {
                            rw.resize(col + 1, String::new());
                        }
                        rw[col] = text;
                    }
                }
                b"row" => {
                    if let Some(rw) = cur.take() {
                        rows.push(rw);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    (rows, cut)
}

// ------------------------------------------------------------ EPUB

/// EPUB: Kapitel in Lesereihenfolge (Spine) als Überschriften, Absätze und Listen.
pub fn read_epub(path: &Path) -> Result<DocxDoc> {
    let mut zip = open_zip(path, "EPUB")?;
    let Some(container) = read_part(&mut zip, "META-INF/container.xml", MAX_PART)? else {
        api_bail!(InvalidInput, "{} ist keine gültige EPUB-Datei.", path.display());
    };
    let Some(opf_path) = first_attr(&container, b"rootfile", b"full-path") else {
        api_bail!(InvalidInput, "{} ist keine gültige EPUB-Datei.", path.display());
    };
    let opf_path = normalize(&opf_path, "").context("EPUB ist beschädigt")?;
    let Some(opf) = read_part(&mut zip, &opf_path, MAX_PART)? else {
        api_bail!(InvalidInput, "{} ist keine gültige EPUB-Datei.", path.display());
    };
    let base = opf_path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let (manifest, spine) = parse_opf(&opf);
    let mut out = Out::default();
    for idref in spine.iter().take(MAX_CHAPTERS) {
        if out.full() {
            break;
        }
        let Some(href) = manifest.get(idref) else { continue };
        let Some(part) = normalize(href, &base) else { continue };
        let Some(xhtml) = read_part(&mut zip, &part, MAX_PART)? else { continue };
        parse_xhtml(&xhtml, &mut out);
    }
    if spine.len() > MAX_CHAPTERS {
        out.truncated = true;
    }
    Ok(out.done())
}

fn first_attr(xml: &[u8], tag: &[u8], name: &[u8]) -> Option<String> {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref().as_bytes() == tag => return attr(&e, name),
            Event::Eof => return None,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// Relativen Pfad innerhalb des ZIP auflösen; `None` bei Pfaden nach außen oder Adressen.
fn normalize(href: &str, base: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or("");
    if href.is_empty() || href.contains("://") || href.starts_with('/') {
        return None;
    }
    let decoded = href.replace("%20", " ");
    let mut parts: Vec<&str> = if base.is_empty() { Vec::new() } else { base.split('/').collect() };
    for seg in decoded.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn parse_opf(xml: &[u8]) -> (HashMap<String, String>, Vec<String>) {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    while let Ok(ev) = r.read_event_into(&mut buf) {
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref().as_bytes() {
                b"item" => {
                    let html = attr(&e, b"media-type").is_some_and(|m| m.contains("html"));
                    if let (true, Some(id), Some(href)) = (html, attr(&e, b"id"), attr(&e, b"href")) {
                        if manifest.len() < 20_000 {
                            manifest.insert(id, href);
                        }
                    }
                }
                b"itemref" => {
                    if let Some(id) = attr(&e, b"idref") {
                        if spine.len() < MAX_CHAPTERS + 1 {
                            spine.push(id);
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
    (manifest, spine)
}

/// XHTML eines Kapitels → Blöcke. Skripte und Stile werden übersprungen; unbekannte Entitäten
/// (z. B. `&nbsp;`) werden zu Leerzeichen.
fn parse_xhtml(xml: &[u8], out: &mut Out) {
    let mut r = reader(xml);
    let mut buf = Vec::new();
    let mut skip = 0usize;
    let mut cur: Option<(Option<u8>, String)> = None; // (Überschriftsebene, Text)
    let mut in_li = 0usize;
    let mut items: Vec<Vec<Span>> = Vec::new();
    let mut ordered = false;
    while let Ok(ev) = r.read_event_into(&mut buf) {
        if out.full() {
            break;
        }
        match ev {
            Event::Start(e) => match e.local_name().as_ref().as_bytes() {
                b"script" | b"style" | b"head" => skip += 1,
                b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" if skip == 0 => {
                    let lvl = e.local_name().as_ref().as_bytes()[1] - b'0';
                    cur = Some((Some(lvl), String::new()));
                }
                b"p" | b"div" | b"blockquote" | b"pre" if skip == 0 && cur.is_none() && in_li == 0 => {
                    cur = Some((None, String::new()))
                }
                b"ol" | b"ul" if skip == 0 => ordered = e.local_name().as_ref().as_bytes() == b"ol",
                b"li" if skip == 0 => {
                    in_li += 1;
                    cur = Some((None, String::new()));
                }
                _ => {}
            },
            Event::Empty(e) if skip == 0 && e.local_name().as_ref().as_bytes() == b"br" => {
                if let Some(c) = cur.as_mut() {
                    c.1.push(' ');
                }
            }
            Event::Text(t) if skip == 0 => {
                if let Some(c) = cur.as_mut() {
                    c.1.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(g) if skip == 0 => {
                if let Some(c) = cur.as_mut() {
                    c.1.push_str(&entity(&g).unwrap_or_else(|| " ".into()));
                }
            }
            Event::End(e) => match e.local_name().as_ref().as_bytes() {
                b"script" | b"style" | b"head" => skip = skip.saturating_sub(1),
                b"h1" | b"h2" | b"h3" | b"h4" | b"h5" | b"h6" | b"p" | b"div" | b"blockquote" | b"pre"
                    if in_li == 0 =>
                {
                    if let Some((lvl, text)) = cur.take() {
                        match lvl {
                            Some(l) => out.heading(l, text),
                            None => out.para(text),
                        }
                    }
                }
                b"li" => {
                    in_li = in_li.saturating_sub(1);
                    if let Some((_, text)) = cur.take() {
                        let t = tidy(&text);
                        if !t.is_empty() {
                            items.push(vec![Span { text: t, ..Default::default() }]);
                        }
                    }
                }
                b"ol" | b"ul" if !items.is_empty() => {
                    out.push(DocBlock::List { ordered, items: std::mem::take(&mut items) });
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
}

// ------------------------------------------------------------ Archive

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    /// Pfad im Archiv (bereinigt, ohne führende „/“ oder „..“).
    pub name: String,
    pub size: u64,
    pub dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveListing {
    pub entries: Vec<ArchiveEntry>,
    pub total_size: u64,
    pub truncated: bool,
    /// Einträge sind verschlüsselt (Passwort nötig zum Entpacken).
    pub encrypted: bool,
}

fn clean_entry(name: &str) -> String {
    name.split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .map(|s| s.chars().filter(|c| !c.is_control()).collect::<String>())
        .collect::<Vec<_>>()
        .join("/")
}

/// Inhalt eines Archivs auflisten (ZIP, TAR, TAR.GZ/TGZ), ohne etwas zu entpacken.
pub fn list_archive(path: &Path) -> Result<ArchiveListing> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut out = ArchiveListing::default();
    let open = || std::fs::File::open(path).with_context(|| format!("{} nicht lesbar", path.display()));
    if name.ends_with(".zip") {
        let mut zip = match zip::ZipArchive::new(std::io::BufReader::new(open()?)) {
            Ok(z) => z,
            Err(_) => api_bail!(InvalidInput, "{} ist kein gültiges ZIP-Archiv.", path.display()),
        };
        for i in 0..zip.len() {
            if out.entries.len() >= MAX_ENTRIES {
                out.truncated = true;
                break;
            }
            let Ok(e) = zip.by_index_raw(i) else { continue };
            out.encrypted |= e.encrypted();
            let n = clean_entry(&e.name().map(|c| c.into_owned()).unwrap_or_default());
            if n.is_empty() {
                continue;
            }
            out.total_size = out.total_size.saturating_add(e.size());
            out.entries.push(ArchiveEntry { name: n, size: e.size(), dir: e.is_dir() });
        }
    } else if name.ends_with(".tar") || name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        let reader: Box<dyn Read> = if name.ends_with(".tar") {
            Box::new(std::io::BufReader::new(open()?))
        } else {
            Box::new(flate2::read::MultiGzDecoder::new(std::io::BufReader::new(open()?)))
        };
        let mut tar = tar::Archive::new(reader);
        let entries = match tar.entries() {
            Ok(e) => e,
            Err(_) => api_bail!(InvalidInput, "{} ist kein gültiges Archiv.", path.display()),
        };
        for e in entries {
            if out.entries.len() >= MAX_ENTRIES {
                out.truncated = true;
                break;
            }
            let Ok(e) = e else { break };
            let n = e.path().ok().map(|p| clean_entry(&p.to_string_lossy())).unwrap_or_default();
            if n.is_empty() {
                continue;
            }
            let size = e.header().size().unwrap_or(0);
            let dir = e.header().entry_type().is_dir();
            out.total_size = out.total_size.saturating_add(size);
            out.entries.push(ArchiveEntry { name: n, size, dir });
        }
    } else {
        api_bail!(InvalidInput, "Dieses Archivformat kann Snap nicht öffnen.");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_with(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            for (n, c) in files {
                z.start_file(*n, o).unwrap();
                z.write_all(c.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    fn write_tmp(dir: &Path, name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn odt_ueberschriften_absaetze_listen_tabellen() {
        let xml = r#"<office:document-content xmlns:office="o" xmlns:text="t" xmlns:table="tb"><office:body><office:text>
            <text:h text:outline-level="1">Titel &amp; mehr</text:h>
            <text:p>Erster<text:s text:c="2"/>Absatz</text:p>
            <text:list><text:list-item><text:p>Punkt A</text:p></text:list-item><text:list-item><text:p>Punkt B</text:p></text:list-item></text:list>
            <table:table table:name="Preise"><table:table-row><table:table-cell><text:p>Brot</text:p></table:table-cell><table:table-cell><text:p>2,50</text:p></table:table-cell><table:table-cell table:number-columns-repeated="1000"/></table:table-row>
            <table:table-row table:number-rows-repeated="100000"><table:table-cell table:number-columns-repeated="1000"/></table:table-row></table:table>
        </office:text></office:body></office:document-content>"#;
        let d = parse_odf(xml.as_bytes());
        assert!(matches!(&d.blocks[0], DocBlock::Heading { level: 1, inline } if inline[0].text == "Titel & mehr"));
        assert!(matches!(&d.blocks[1], DocBlock::Paragraph { inline } if inline[0].text == "Erster Absatz"));
        assert!(matches!(&d.blocks[2], DocBlock::List { items, .. } if items.len() == 2));
        assert!(matches!(&d.blocks[3], DocBlock::Heading { inline, .. } if inline[0].text == "Preise"));
        // Wiederholte leere Zellen/Zeilen blähen nicht auf
        assert!(
            matches!(&d.blocks[4], DocBlock::Table { rows } if rows == &vec![vec!["Brot".to_string(), "2,50".to_string()]])
        );
    }

    #[test]
    fn xlsx_mit_geteilten_zeichenketten() {
        let tmp = tempfile::tempdir().unwrap();
        let data = zip_with(&[
            (
                "xl/workbook.xml",
                r#"<workbook><sheets><sheet name="Einkauf" sheetId="1" r:id="rId1" xmlns:r="r"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            ("xl/sharedStrings.xml", r#"<sst><si><t>Milch</t></si><si><r><t>Kä</t></r><r><t>se</t></r></si></sst>"#),
            (
                "xl/worksheets/sheet1.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="C1"><v>1.5</v></c></row><row r="3"><c r="B3" t="s"><v>1</v></c><c r="D3" t="b"><v>1</v></c></row></sheetData></worksheet>"#,
            ),
        ]);
        let d = read_xlsx(&write_tmp(tmp.path(), "a.xlsx", &data)).unwrap();
        assert!(matches!(&d.blocks[0], DocBlock::Heading { inline, .. } if inline[0].text == "Einkauf"));
        let DocBlock::Table { rows } = &d.blocks[1] else { panic!("{:?}", d.blocks) };
        assert_eq!(rows[0], vec!["Milch", "", "1.5", ""]);
        assert_eq!(rows[1], vec!["", "", "", ""]);
        assert_eq!(rows[2], vec!["", "Käse", "", "WAHR"]);
        assert_eq!(col_of("AA10"), Some(26));
        assert_eq!(col_of("10"), None);
    }

    #[test]
    fn pptx_folien_in_reihenfolge() {
        let tmp = tempfile::tempdir().unwrap();
        let slide = |t: &str| format!(r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#);
        let data = zip_with(&[("ppt/slides/slide10.xml", &slide("Zehn")), ("ppt/slides/slide2.xml", &slide("Zwei"))]);
        let d = read_pptx(&write_tmp(tmp.path(), "a.pptx", &data)).unwrap();
        let texts: Vec<String> = d
            .blocks
            .iter()
            .map(|b| match b {
                DocBlock::Heading { inline, .. } | DocBlock::Paragraph { inline } => inline[0].text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts, vec!["Folie 1", "Zwei", "Folie 2", "Zehn"]);
    }

    #[test]
    fn epub_kapitel_ohne_skripte_und_ohne_pfade_nach_aussen() {
        let tmp = tempfile::tempdir().unwrap();
        let data = zip_with(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
            ),
            (
                "OEBPS/content.opf",
                r#"<package><manifest><item id="c1" href="kap1.xhtml" media-type="application/xhtml+xml"/><item id="x" href="../../etc/passwd" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="x"/><itemref idref="c1"/></spine></package>"#,
            ),
            (
                "OEBPS/kap1.xhtml",
                r#"<html><head><title>T</title><style>p{}</style></head><body><h1>Kapitel&nbsp;1</h1><script>alert(1)</script><p>Es war <b>einmal</b>.</p><ul><li>eins</li><li>zwei</li></ul></body></html>"#,
            ),
        ]);
        let d = read_epub(&write_tmp(tmp.path(), "a.epub", &data)).unwrap();
        assert!(matches!(&d.blocks[0], DocBlock::Heading { level: 1, inline } if inline[0].text == "Kapitel 1"));
        assert!(matches!(&d.blocks[1], DocBlock::Paragraph { inline } if inline[0].text == "Es war einmal."));
        assert!(matches!(&d.blocks[2], DocBlock::List { items, .. } if items.len() == 2));
        assert_eq!(d.blocks.len(), 3, "kein Skripttext, kein Pfad nach außen");
        assert_eq!(normalize("../a.xhtml", "OEBPS"), Some("a.xhtml".into()));
        assert_eq!(normalize("../../a", "OEBPS"), None);
        assert_eq!(normalize("http://x/a", "OEBPS"), None);
    }

    #[test]
    fn archive_auflisten() {
        let tmp = tempfile::tempdir().unwrap();
        let data = zip_with(&[("a/b.txt", "hallo"), ("../../boese.sh", "x"), ("c.txt", "")]);
        let l = list_archive(&write_tmp(tmp.path(), "a.zip", &data)).unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a/b.txt", "boese.sh", "c.txt"]);
        assert_eq!(l.total_size, 6);
        assert!(list_archive(&write_tmp(tmp.path(), "kaputt.zip", b"kein zip")).is_err());
        assert!(list_archive(&write_tmp(tmp.path(), "x.rar", b"x")).is_err());
    }
}
