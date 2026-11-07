//! Übersetzungen für die nativen Dialoge. Gleiche Dateien wie die Oberfläche
//! (`apps/desktop/locales/*.json`): deutscher Text → Übersetzung.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

#[derive(serde::Deserialize)]
struct LocaleFile {
    strings: HashMap<String, String>,
}

fn tables() -> &'static HashMap<&'static str, HashMap<String, String>> {
    static T: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(|| {
        LOCALES
            .iter()
            .filter_map(|(code, json)| serde_json::from_str::<LocaleFile>(json).ok().map(|f| (*code, f.strings)))
            .collect()
    })
}

static CURRENT: RwLock<String> = RwLock::new(String::new());

/// Sprache setzen (nur bekannte Kürzel; sonst Deutsch).
pub fn set(code: &str) {
    let code = if tables().contains_key(code) { code } else { "de" };
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = code.to_string();
}

/// Übersetzt den deutschen Text `de` und setzt `{name}`-Platzhalter ein.
pub fn tr(de: &str, vars: &[(&str, &str)]) -> String {
    let cur = CURRENT.read().unwrap_or_else(|e| e.into_inner());
    let mut s = tables().get(cur.as_str()).and_then(|t| t.get(de)).cloned().unwrap_or_else(|| de.to_string());
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Alle Schlüssel im Code: `t("…")` in der Oberfläche und `tr("…"` im Rust-Teil.
    fn keys_in_code() -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut scan = |dir: &str, marker: &str| {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                if e.file_name() == "i18n.rs" {
                    continue; // eigene Testtexte
                }
                let text = std::fs::read_to_string(e.path()).unwrap_or_default();
                let mut rest = text.as_str();
                while let Some(i) = rest.find(marker) {
                    // Nur eigenständige Aufrufe (nicht `split("…")` o. Ä.)
                    let before = rest[..i].chars().next_back();
                    rest = &rest[i + marker.len()..];
                    if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                        continue;
                    }
                    // Zeichenkette bis zum nächsten unmaskierten Anführungszeichen
                    let mut out = String::new();
                    let mut chars = rest.chars();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => out.push(chars.next().unwrap_or_default()),
                            '"' => break,
                            c => out.push(c),
                        }
                    }
                    keys.insert(out);
                }
            }
        };
        scan("../src", "t(\"");
        scan("src", "tr(\"");
        keys.remove("…");
        keys
    }

    fn placeholders(s: &str) -> BTreeSet<String> {
        s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(k, _)| k.to_string())).collect()
    }

    /// Jede Sprachdatei übersetzt alle Texte des Codes und behält die Platzhalter bei.
    #[test]
    fn every_language_is_complete() {
        let code_keys = keys_in_code();
        assert!(code_keys.len() > 100, "Schlüssel gefunden: {}", code_keys.len());
        for (code, table) in tables() {
            let missing: Vec<&String> = code_keys.iter().filter(|k| !table.contains_key(*k)).collect();
            assert!(
                missing.is_empty(),
                "{code}.json: {} fehlende Übersetzungen, z. B. {:?}",
                missing.len(),
                &missing[..missing.len().min(10)]
            );
            for (de, tr) in table {
                assert_eq!(placeholders(de), placeholders(tr), "{code}.json: Platzhalter in {de:?}");
                assert!(!tr.trim().is_empty(), "{code}.json: leere Übersetzung für {de:?}");
            }
        }
        assert_eq!(LOCALES.len(), tables().len(), "jede Sprachdatei ist gültiges JSON");
    }

    #[test]
    fn translates_with_placeholders_and_falls_back() {
        set("en");
        assert_eq!(tr("Abbrechen", &[]), "Cancel");
        set("xx");
        assert_eq!(tr("Abbrechen", &[]), "Abbrechen");
        assert_eq!(tr("„{name}“ entkoppeln?", &[("name", "Laptop")]), "„Laptop“ entkoppeln?");
    }
}
