//! Fernoperationen (snap/ops/1). Invarianten: Was die Prüfung besteht, hat nur gültige
//! relative Pfade (kein `..`, nicht absolut, nicht leer – außer Zielordner), gültige Namen und
//! höchstens MAX_PATHS Einträge; die Auflösung bleibt immer in der Freigabe.
#![no_main]
use libfuzzer_sys::fuzz_target;
use space_core::remote_ops::{decode_request, resolve_in_share, RemoteOp, MAX_IMPORT_BYTES, MAX_PATHS};
use std::path::{Component, Path};

fn check_rel(p: &str, allow_empty: bool) {
    if p.is_empty() {
        assert!(allow_empty);
        return;
    }
    assert!(!p.starts_with('/'), "{p:?}");
    assert!(p.split('/').all(|s| !s.is_empty() && s != "." && s != ".."), "{p:?}");
    let root = Path::new("/nicht/vorhanden/freigabe");
    if let Ok(abs) = resolve_in_share(root, p) {
        assert!(abs.starts_with(root));
        assert!(abs.components().all(|c| !matches!(c, Component::ParentDir | Component::CurDir)));
    }
}

fn check_name(n: &str) {
    assert!(!n.is_empty() && n != "." && n != ".." && !n.contains('/') && !n.contains('\0'), "{n:?}");
}

fuzz_target!(|data: &[u8]| {
    let Ok(req) = decode_request(data) else { return };
    let share = req.op.share();
    assert!(!share.is_empty() && !share.contains('/') && share != "..");
    match &req.op {
        RemoteOp::Rename { path, new_name, .. } => {
            check_rel(path, false);
            check_name(new_name);
        }
        RemoteOp::Move { paths, dest_dir, .. } | RemoteOp::Copy { paths, dest_dir, .. } => {
            assert!(!paths.is_empty() && paths.len() <= MAX_PATHS);
            paths.iter().for_each(|p| check_rel(p, false));
            check_rel(dest_dir, true);
        }
        RemoteOp::Mkdir { parent, name, .. } => {
            check_rel(parent, true);
            check_name(name);
        }
        RemoteOp::Trash { paths, .. } | RemoteOp::Purge { paths, .. } => {
            assert!(!paths.is_empty() && paths.len() <= MAX_PATHS);
            paths.iter().for_each(|p| check_rel(p, false));
        }
        RemoteOp::Import { dest_dir, name, hash, size, .. } => {
            check_rel(dest_dir, true);
            check_name(name);
            assert!(!name.starts_with('.'), "versteckter Name {name:?}");
            assert!(hash.parse::<iroh_blobs::Hash>().is_ok());
            assert!(*size <= MAX_IMPORT_BYTES);
        }
    }
});
