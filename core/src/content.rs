//! Volltextsuche: Text in Dokumenten finden (lokal, nie an andere Geräte).
//!
//! - Eigener FTS5-Index in `index.db` (Wörter, ohne Akzente, Präfixsuche), je Datei höchstens
//!   [`MAX_TEXT`] Zeichen. Erfasst werden Textdateien, Office-Dokumente (DOCX, ODT, PPTX, XLSX,
//!   EPUB) und PDFs mit Textschicht.
//! - Läuft nach jedem Abgleich des Dateiindex im Hintergrund, in kleinen Portionen
//!   ([`BATCH`] Dateien), nur für neue oder geänderte Dateien.
//! - Dateien kommen von außen: Größe begrenzt, Leser mit eigenen Grenzen, Abstürze eines Lesers
//!   werden abgefangen (`catch_unwind`), PDFs nur bis [`MAX_PDF_PAGES`] Seiten.

use crate::index::LocalIndex;
use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Höchstens so viele Zeichen Text je Datei.
pub const MAX_TEXT: usize = 100_000;
/// Dateien je Durchlauf.
pub const BATCH: usize = 200;
const MAX_PLAIN: u64 = 2 * 1024 * 1024;
const MAX_DOC: u64 = 50 * 1024 * 1024;
const MAX_PDF_PAGES: u32 = 50;

const PLAIN: &[&str] = &[
    "txt", "text", "md", "markdown", "csv", "tsv", "json", "log", "xml", "html", "htm", "ini", "conf", "cfg", "yaml",
    "yml", "toml", "tex", "rst", "org", "srt", "vtt", "rs", "py", "js", "ts", "java", "kt", "c", "h", "cpp", "go",
    "rb", "sh",
];
const DOCS: &[&str] = &["docx", "odt", "ods", "odp", "epub", "pptx", "xlsx"];

pub fn schema() -> &'static str {
    "CREATE VIRTUAL TABLE IF NOT EXISTS content_fts USING fts5(body, tokenize='unicode61 remove_diacritics 2');
     CREATE TABLE IF NOT EXISTS content_meta (
        node_id INTEGER PRIMARY KEY,
        size    INTEGER NOT NULL,
        mtime   INTEGER NOT NULL
     );"
}

/// Treffer der Volltextsuche.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ContentHit {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    /// Ausschnitt um die Fundstelle (Treffer in «…» markiert).
    pub snippet: String,
}

fn ext_list() -> String {
    PLAIN.iter().chain(DOCS).chain(["pdf"].iter()).map(|e| format!("'{e}'")).collect::<Vec<_>>().join(",")
}

/// Text einer Datei (gekürzt), `None`, wenn nicht lesbar oder leer.
pub fn extract(path: &Path, ext: &str, size: u64) -> Option<String> {
    let text = std::panic::catch_unwind(|| -> Option<String> {
        if PLAIN.contains(&ext) {
            if size > MAX_PLAIN {
                return None;
            }
            let bytes = std::fs::read(path).ok()?;
            let s = String::from_utf8_lossy(&bytes);
            Some(if ext == "html" || ext == "htm" || ext == "xml" { strip_tags(&s) } else { s.into_owned() })
        } else if DOCS.contains(&ext) {
            if size > MAX_DOC {
                return None;
            }
            let doc = crate::docs::read_document(path, None).ok()?;
            let mut out = String::new();
            for b in doc.blocks {
                use crate::docx::DocBlock;
                match b {
                    DocBlock::Heading { inline, .. } | DocBlock::Paragraph { inline } => {
                        inline.iter().for_each(|s| out.push_str(&s.text));
                    }
                    DocBlock::List { items, .. } => items.iter().flatten().for_each(|s| {
                        out.push_str(&s.text);
                        out.push(' ');
                    }),
                    DocBlock::Table { rows } => rows.iter().flatten().for_each(|c| {
                        out.push_str(c);
                        out.push(' ');
                    }),
                    DocBlock::Image { .. } => {}
                }
                out.push('\n');
                if out.len() > MAX_TEXT * 4 {
                    break;
                }
            }
            Some(out)
        } else if ext == "pdf" {
            if size > MAX_DOC {
                return None;
            }
            pdf_text(&std::fs::read(path).ok()?)
        } else {
            None
        }
    })
    .ok()
    .flatten()?;
    clean(&text)
}

/// Text einer PDF (erste [`MAX_PDF_PAGES`] Seiten), bereinigt und gekürzt. Für Fuzzing öffentlich.
pub fn pdf_text(bytes: &[u8]) -> Option<String> {
    let text = std::panic::catch_unwind(|| -> Option<String> {
        let doc = lopdf::Document::load_mem(bytes).ok()?;
        let pages: Vec<u32> = doc.get_pages().keys().copied().take(MAX_PDF_PAGES as usize).collect();
        doc.extract_text(&pages).ok()
    })
    .ok()
    .flatten()?;
    clean(&text)
}

fn clean(text: &str) -> Option<String> {
    let clean: String =
        text.chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).take(MAX_TEXT).collect();
    (!clean.trim().is_empty()).then_some(clean)
}

/// Grob Tags entfernen (nur für die Suche, nicht zur Anzeige).
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Neue oder geänderte Dateien erfassen (höchstens `budget`). Gibt die Zahl erfasster Dateien
/// zurück; 0 heißt: alles aktuell. Blockierend.
pub fn refresh(idx: &LocalIndex, budget: usize) -> Result<usize> {
    let todo: Vec<(i64, u64, i64, String, String)> = {
        let conn = idx.reader();
        let sql = format!(
            "SELECT n.id, n.size, n.mtime, n.ext FROM nodes n LEFT JOIN content_meta m ON m.node_id = n.id
             WHERE n.kind = 0 AND n.hidden = 0 AND n.ext IN ({}) AND n.size <= {MAX_DOC}
               AND (m.node_id IS NULL OR m.size != n.size OR m.mtime != n.mtime)
             LIMIT ?1",
            ext_list()
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(i64, i64, i64, String)> = stmt
            .query_map(params![budget as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, Option<String>>(3)?.unwrap_or_default()))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut cache = HashMap::new();
        rows.into_iter()
            .filter_map(|(id, size, mtime, ext)| {
                let path = LocalIndex::path_of(&conn, id, &mut cache).ok()?;
                Some((id, size.max(0) as u64, mtime, ext, path))
            })
            .collect()
    };
    let mut done = 0;
    let mut batch: Vec<(i64, u64, i64, Option<String>)> = Vec::with_capacity(todo.len());
    for (id, size, mtime, ext, path) in todo {
        let text = extract(Path::new(&path), &ext, size);
        batch.push((id, size, mtime, text));
        done += 1;
    }
    if !batch.is_empty() {
        let mut conn = idx.writer();
        let tx = conn.transaction()?;
        for (id, size, mtime, text) in &batch {
            tx.execute("DELETE FROM content_fts WHERE rowid = ?1", params![id])?;
            if let Some(t) = text {
                tx.execute("INSERT INTO content_fts(rowid, body) VALUES (?1, ?2)", params![id, t])?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO content_meta (node_id, size, mtime) VALUES (?1, ?2, ?3)",
                params![id, *size as i64, mtime],
            )?;
        }
        tx.commit()?;
    }
    Ok(done)
}

/// Einträge gelöschter Dateien entfernen. Blockierend.
pub fn prune(idx: &LocalIndex) -> Result<()> {
    let conn = idx.writer();
    conn.execute_batch(
        "DELETE FROM content_fts WHERE rowid NOT IN (SELECT id FROM nodes);
         DELETE FROM content_meta WHERE node_id NOT IN (SELECT id FROM nodes);",
    )?;
    Ok(())
}

/// Anfrage in eine sichere FTS5-Abfrage übersetzen: jedes Wort als Präfix-Phrase, alle müssen
/// vorkommen. Sonderzeichen gelten wörtlich.
pub fn fts_query(q: &str) -> Option<String> {
    let parts: Vec<String> = q
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .take(8)
        .map(|w| format!("\"{}\"*", w.replace('"', "\"\"")))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub fn search(idx: &LocalIndex, query: &str, limit: usize) -> Result<Vec<ContentHit>> {
    let Some(q) = fts_query(query) else { return Ok(Vec::new()) };
    let conn = idx.reader();
    let mut stmt = conn.prepare_cached(
        "SELECT f.rowid, snippet(content_fts, 0, '«', '»', '…', 12), n.name, n.size, n.mtime
         FROM content_fts f JOIN nodes n ON n.id = f.rowid
         WHERE content_fts MATCH ?1 ORDER BY rank LIMIT ?2",
    )?;
    let rows: Vec<(i64, String, String, i64, i64)> = stmt
        .query_map(params![q, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut cache = HashMap::new();
    let mut out = Vec::new();
    for (id, snippet, name, size, mtime) in rows {
        if let Ok(path) = LocalIndex::path_of(&conn, id, &mut cache) {
            let snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push(ContentHit { path, name, size: size.max(0) as u64, mtime, snippet });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abfrage_ist_sicher() {
        assert_eq!(fts_query("Rechnung 2024").as_deref(), Some("\"Rechnung\"* \"2024\"*"));
        assert_eq!(fts_query("a\" OR 1=1 --").as_deref(), Some("\"a\"\"\"* \"OR\"* \"1=1\"*"));
        assert_eq!(fts_query("  -- ** "), None);
    }

    #[test]
    fn text_aus_dateien() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.html");
        std::fs::write(&p, "<html><body><p>Steuererklärung <b>2024</b></p><script>x</script></body></html>").unwrap();
        let t = extract(&p, "html", 100).unwrap();
        assert!(t.contains("Steuererklärung") && t.contains("2024") && !t.contains("<p>"));
        let big = dir.path().join("b.txt");
        std::fs::write(&big, "x".repeat(10)).unwrap();
        assert!(extract(&big, "txt", MAX_PLAIN + 1).is_none(), "zu groß");
        let bad = dir.path().join("c.pdf");
        std::fs::write(&bad, b"%PDF-1.4 kaputt").unwrap();
        assert!(extract(&bad, "pdf", 15).is_none());
    }
}
