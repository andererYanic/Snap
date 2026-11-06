//! Suchanfragen (lokal über IPC): Sonderzeichen dürfen weder die FTS5-Syntax noch LIKE
//! beeinflussen. Invarianten: nie ein Fehler, höchstens `limit` Treffer, und jeder Treffer
//! enthält den Suchbegriff wirklich (für ASCII ohne Groß-/Kleinschreibung).
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
    let q = query.trim();
    if q.is_ascii() && !q.is_empty() {
        let q = q.to_ascii_lowercase();
        for h in &hits {
            assert!(h.name.to_ascii_lowercase().contains(&q), "{q:?} -> {:?}", h.name);
        }
    }
});
