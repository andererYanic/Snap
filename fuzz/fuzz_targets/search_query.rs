//! Suchanfragen (lokal über IPC): Sonderzeichen dürfen weder die FTS5-Syntax noch LIKE
//! beeinflussen. Invarianten: nie ein Fehler, höchstens `limit` Treffer, keine Doppelten; exakte
//! Treffer enthalten den Suchbegriff wirklich (für ASCII ohne Groß-/Kleinschreibung), ähnliche
//! Treffer (`fuzzy`) gibt es nur ab 6 Zeichen und sie liegen nachweislich innerhalb der erlaubten
//! Fehlerzahl – geprüft mit einer unabhängigen, absichtlich einfachen Referenz-Implementierung.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::index::{LocalIndex, NodeKind, SearchFilter};
use std::sync::OnceLock;

const NAMES: &[&str] = &[
    "\"zitat\".txt",
    "100% fertig.pdf",
    "a_b-c.rs",
    "ÄÖÜ straße.md",
    "back\\slash",
    "stern*chen NEAR(x)",
    "OR AND NOT.txt",
    "(klammer) ^dach",
    "Urlaub 2024.jpg",
    "'; DROP TABLE nodes; --",
];

fn index() -> &'static LocalIndex {
    static IDX: OnceLock<LocalIndex> = OnceLock::new();
    IDX.get_or_init(|| {
        let base = std::env::temp_dir().join(format!("snap-fuzz-search-{}", std::process::id()));
        let root = base.join("baum");
        std::fs::create_dir_all(root.join("ordner (1)")).unwrap();
        for n in NAMES {
            std::fs::write(root.join(n), b"x").unwrap();
            std::fs::write(root.join("ordner (1)").join(n), b"xy").unwrap();
        }
        let idx = LocalIndex::open(&base.join("index.db"), vec![]).unwrap();
        let id = idx.add_root(&root).unwrap();
        idx.rebuild_root(id, &root).unwrap();
        idx
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else { return };
    let Ok(query) = std::str::from_utf8(rest) else { return };
    let filter = SearchFilter {
        kind: match flags & 3 {
            1 => Some(NodeKind::File),
            2 => Some(NodeKind::Dir),
            _ => None,
        },
        extensions: if flags & 4 != 0 { vec![query.chars().take(8).collect()] } else { Vec::new() },
        min_size: (flags & 8 != 0).then_some(2),
        include_hidden: flags & 16 != 0,
        ..Default::default()
    };
    let limit = (flags >> 5) as usize + 1;
    let hits = index().search(query, &filter, limit).expect("Suche darf nie fehlschlagen");
    assert!(hits.len() <= limit);
    let mut seen = std::collections::HashSet::new();
    for h in &hits {
        assert!(seen.insert(&h.path), "doppelter Treffer {:?}", h.path);
    }
    let q = query.trim();
    if q.is_ascii() && !q.is_empty() {
        let q = q.to_ascii_lowercase();
        let qc: Vec<char> = q.chars().collect();
        for h in &hits {
            let name = h.name.to_ascii_lowercase();
            if h.fuzzy {
                let max = allowed_errors(qc.len());
                assert!(max > 0, "ähnlicher Treffer für zu kurzen Begriff {q:?} -> {:?}", h.name);
                let d = reference_distance(&qc, &name.chars().collect::<Vec<_>>());
                assert!(d <= max, "{q:?} -> {:?}: Abstand {d} > {max}", h.name);
            } else {
                assert!(name.contains(&q), "{q:?} -> {:?}", h.name);
            }
        }
    }
});

/// Erlaubte Fehler je Begriffslänge (Spezifikation, absichtlich hier nachgeschrieben).
fn allowed_errors(len: usize) -> usize {
    match len {
        0..=5 => 0,
        6..=8 => 1,
        9..=11 => 2,
        _ => 3,
    }
}

/// Kleinster Abstand (Einfügen, Löschen, Ersetzen, Vertauschen benachbarter Zeichen) zwischen `q`
/// und irgendeinem Teilstück von `t`. Volle Tabelle, ohne Abkürzungen – langsam, aber leicht
/// nachprüfbar.
fn reference_distance(q: &[char], t: &[char]) -> usize {
    let (m, n) = (q.len(), t.len());
    // d[i][j]: Abstand von q[..i] zu einem Teilstück, das bei t[j] endet (Anfang frei).
    let mut d = vec![vec![0usize; n + 1]; m + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = usize::from(q[i - 1] != t[j - 1]);
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && q[i - 1] == t[j - 2] && q[i - 2] == t[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    (0..=n).map(|j| d[m][j]).min().unwrap_or(m)
}
