//! Lokale Dateioperationen: kopieren, verschieben, umbenennen, Ordner anlegen, löschen.
//!
//! Sicherheitsregeln:
//! - Nie stillschweigend überschreiben: Umbenennen/Verschieben nutzt `renameat2(RENAME_NOREPLACE)`;
//!   „Ersetzen“ verschiebt das alte Ziel zuerst in den Papierkorb.
//! - Symlinks werden nie verfolgt, sondern als Symlinks kopiert.
//! - Kopien entstehen unter einem temporären Namen und werden erst vollständig umbenannt –
//!   ein Abbruch hinterlässt keine halben Dateien.
//! - Verschieben über Laufwerksgrenzen: kopieren, prüfen (Anzahl und Bytes), erst dann löschen.

use super::trash::{remove_any, Trash};
use crate::api_bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Ab dieser Größe wird blockweise kopiert (Fortschritt und Abbruch innerhalb der Datei).
const CHUNKED_FROM: u64 = 64 * 1024 * 1024;
const CHUNK: usize = 8 * 1024 * 1024;

/// Was passiert, wenn das Ziel schon existiert.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Conflict {
    /// Abbrechen, bevor irgendetwas geändert wird.
    #[default]
    Fail,
    /// Diesen Eintrag auslassen.
    Skip,
    /// Freien Namen wählen: „Bericht (2).pdf“.
    KeepBoth,
    /// Altes Ziel in den Papierkorb, dann ersetzen.
    Replace,
}

/// Fortschritt und Abbruch eines Auftrags (von mehreren Threads lesbar).
#[derive(Debug, Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    pub total_files: AtomicU64,
    pub total_bytes: AtomicU64,
    pub done_files: AtomicU64,
    pub done_bytes: AtomicU64,
}

impl Progress {
    fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            api_bail!(Cancelled, "Vorgang abgebrochen.");
        }
        Ok(())
    }
    fn add(&self, files: u64, bytes: u64) {
        self.done_files.fetch_add(files, Ordering::Relaxed);
        self.done_bytes.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Ergebnis einer Operation für einen Eintrag (Grundlage für „Rückgängig“).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Done {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Prüft einen neuen Datei- oder Ordnernamen.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 255 || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        api_bail!(InvalidInput, "Ungültiger Name: {name:?}");
    }
    Ok(())
}

/// Freier Name neben `path`: „Bericht.pdf“ → „Bericht (2).pdf“, „Ordner“ → „Ordner (2)“.
pub fn unique_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let is_dir = std::fs::symlink_metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !is_dir => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for n in 2..100_000u32 {
        let candidate = path.with_file_name(format!("{stem} ({n}){ext}"));
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    path.with_file_name(format!("{stem} ({}){ext}", std::process::id()))
}

/// Umbenennen ohne Überschreiben (Linux: `renameat2` mit `RENAME_NOREPLACE`).
pub fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput);
    let (f, t) = (c(from)?, c(to)?);
    // Direkter Systemaufruf statt libc-Funktion: Androids bionic bietet renameat2 erst ab API 30,
    // der Kernel kann es seit 3.15 (alle unterstützten Android-Versionen).
    // SAFETY: gültige, NUL-terminierte Pfade; AT_FDCWD für relative Auflösung (Pfade sind absolut).
    let r = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE as libc::c_uint,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    // Dateisystem ohne RENAME_NOREPLACE: prüfen, dann umbenennen (kleines Zeitfenster, nur lokal).
    if err.raw_os_error() == Some(libc::EINVAL) {
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        return std::fs::rename(from, to);
    }
    Err(err)
}

fn require_abs(p: &Path) -> Result<()> {
    if !p.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", p.display());
    }
    Ok(())
}

/// Zählt Dateien und Bytes unterhalb von `path` (ohne Symlinks zu folgen).
pub fn measure(path: &Path) -> (u64, u64) {
    let (mut files, mut bytes) = (0, 0);
    for e in walkdir::WalkDir::new(path).follow_links(false).into_iter().flatten() {
        if !e.file_type().is_dir() {
            files += 1;
            if e.file_type().is_file() {
                bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    (files, bytes)
}

/// Geplantes Ziel je Quelle (`None` = auslassen) – alles geprüft, bevor etwas passiert.
struct Plan {
    src: PathBuf,
    dst: PathBuf,
    replace: bool,
}

fn plan(sources: &[PathBuf], dest_dir: &Path, conflict: Conflict, is_copy: bool) -> Result<Vec<Plan>> {
    require_abs(dest_dir)?;
    if !std::fs::metadata(dest_dir).map(|m| m.is_dir()).unwrap_or(false) {
        api_bail!(NotFound, "Zielordner {} existiert nicht.", dest_dir.display());
    }
    if sources.is_empty() {
        api_bail!(InvalidInput, "Keine Quellen angegeben.");
    }
    let real_dest = std::fs::canonicalize(dest_dir)?;
    let mut out = Vec::new();
    for src in sources {
        require_abs(src)?;
        let meta = match std::fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(_) => api_bail!(NotFound, "{} existiert nicht.", src.display()),
        };
        let Some(name) = src.file_name() else { api_bail!(InvalidInput, "Ungültige Quelle: {}", src.display()) };
        // Ordner nicht in sich selbst kopieren/verschieben.
        if meta.is_dir() {
            let real_src = std::fs::canonicalize(src)?;
            if real_dest.starts_with(&real_src) {
                api_bail!(InvalidInput, "{} kann nicht in sich selbst abgelegt werden.", src.display());
            }
        }
        let mut dst = dest_dir.join(name);
        if dst == *src {
            if !is_copy {
                continue; // Verschieben an denselben Ort: nichts zu tun
            }
            dst = unique_path(&dst); // Kopie im selben Ordner: „Name (2)“
        }
        let mut replace = false;
        if std::fs::symlink_metadata(&dst).is_ok() {
            match conflict {
                Conflict::Fail => api_bail!(AlreadyExists, "{} existiert bereits.", dst.display()),
                Conflict::Skip => continue,
                Conflict::KeepBoth => dst = unique_path(&dst),
                Conflict::Replace => replace = true,
            }
        }
        out.push(Plan { src: src.clone(), dst, replace });
    }
    Ok(out)
}

fn set_progress_totals(p: &Progress, plans: &[Plan]) {
    let (mut f, mut b) = (0, 0);
    for pl in plans {
        let (pf, pb) = measure(&pl.src);
        f += pf;
        b += pb;
    }
    p.total_files.store(f, Ordering::Relaxed);
    p.total_bytes.store(b, Ordering::Relaxed);
}

fn mtime_of(m: &std::fs::Metadata) -> Option<std::time::SystemTime> {
    m.modified().ok()
}

/// Kopiert eine einzelne Datei über einen temporären Namen.
fn copy_file(src: &Path, dst: &Path, meta: &std::fs::Metadata, p: &Progress) -> Result<()> {
    p.check()?;
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dst.with_file_name(format!(".{name}.snap-part"));
    let result = (|| -> Result<()> {
        if meta.len() < CHUNKED_FROM {
            // std::fs::copy nutzt copy_file_range (schnell, ggf. reflink) und übernimmt die Rechte.
            std::fs::copy(src, &tmp).with_context(|| format!("{} nicht kopierbar", src.display()))?;
            p.add(0, meta.len());
        } else {
            use std::io::{Read, Write};
            let mut input = std::fs::File::open(src)?;
            let mut output = std::fs::File::create(&tmp)?;
            let mut buf = vec![0u8; CHUNK];
            loop {
                p.check()?;
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                output.write_all(&buf[..n])?;
                p.add(0, n as u64);
            }
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        if let Some(t) = mtime_of(meta) {
            let _ = std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(t));
        }
        rename_noreplace(&tmp, dst).with_context(|| format!("{} nicht anlegbar", dst.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    p.add(1, 0);
    result
}

/// Kopiert Datei, Symlink oder Ordnerbaum von `src` nach `dst` (existiert noch nicht).
fn copy_tree(src: &Path, dst: &Path, p: &Progress) -> Result<()> {
    let mut dirs: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
    for entry in walkdir::WalkDir::new(src).follow_links(false) {
        p.check()?;
        let entry = entry.with_context(|| format!("{} nicht lesbar", src.display()))?;
        let rel = entry.path().strip_prefix(src).unwrap_or(Path::new(""));
        let target = if rel.as_os_str().is_empty() { dst.to_path_buf() } else { dst.join(rel) };
        let meta = entry.metadata()?;
        let ft = entry.file_type();
        if ft.is_dir() {
            std::fs::create_dir(&target).with_context(|| format!("{} nicht anlegbar", target.display()))?;
            dirs.push((target, meta));
        } else if ft.is_symlink() {
            let link = std::fs::read_link(entry.path())?;
            std::os::unix::fs::symlink(&link, &target)?;
            p.add(1, 0);
        } else if ft.is_file() {
            copy_file(entry.path(), &target, &meta, p)?;
        } else {
            p.add(1, 0); // Geräte, Sockets, FIFOs werden nicht kopiert
        }
    }
    // Rechte und Datum der Ordner zuletzt (schreibgeschützte Ordner blockieren sonst den Inhalt).
    for (dir, meta) in dirs.into_iter().rev() {
        let _ = std::fs::set_permissions(&dir, meta.permissions());
        if let Some(t) = mtime_of(&meta) {
            let _ = std::fs::File::open(&dir).and_then(|f| f.set_modified(t));
        }
    }
    Ok(())
}

/// Kopiert `sources` in `dest_dir`.
pub fn copy_into(
    sources: &[PathBuf],
    dest_dir: &Path,
    conflict: Conflict,
    trash: &Trash,
    p: &Progress,
) -> Result<Vec<Done>> {
    let plans = plan(sources, dest_dir, conflict, true)?;
    set_progress_totals(p, &plans);
    let mut done = Vec::new();
    for pl in plans {
        if pl.replace {
            trash.trash(&pl.dst)?;
        }
        if let Err(e) = copy_tree(&pl.src, &pl.dst, p) {
            // Unvollständige Kopie entfernen.
            let _ = remove_any(&pl.dst);
            return Err(e);
        }
        done.push(Done { from: pl.src, to: pl.dst });
    }
    Ok(done)
}

/// Verschiebt `sources` in `dest_dir`.
pub fn move_into(
    sources: &[PathBuf],
    dest_dir: &Path,
    conflict: Conflict,
    trash: &Trash,
    p: &Progress,
) -> Result<Vec<Done>> {
    let plans = plan(sources, dest_dir, conflict, false)?;
    set_progress_totals(p, &plans);
    let mut done = Vec::new();
    for pl in plans {
        p.check()?;
        if pl.replace {
            trash.trash(&pl.dst)?;
        }
        match rename_noreplace(&pl.src, &pl.dst) {
            Ok(()) => {
                let (f, b) = measure(&pl.dst);
                p.add(f, b);
            }
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                // Anderes Laufwerk: kopieren, prüfen, dann Quelle löschen.
                let before = measure(&pl.src);
                if let Err(e) = copy_tree(&pl.src, &pl.dst, p) {
                    let _ = remove_any(&pl.dst);
                    return Err(e);
                }
                if measure(&pl.dst) != before {
                    let _ = remove_any(&pl.dst);
                    api_bail!(Internal, "Kopie von {} unvollständig – Quelle bleibt erhalten.", pl.src.display());
                }
                remove_any(&pl.src)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                api_bail!(AlreadyExists, "{} existiert bereits.", pl.dst.display())
            }
            Err(e) => return Err(e).with_context(|| format!("{} nicht verschiebbar", pl.src.display())),
        }
        done.push(Done { from: pl.src, to: pl.dst });
    }
    Ok(done)
}

/// Benennt einen Eintrag im selben Ordner um.
pub fn rename(path: &Path, new_name: &str) -> Result<Done> {
    require_abs(path)?;
    validate_name(new_name)?;
    if std::fs::symlink_metadata(path).is_err() {
        api_bail!(NotFound, "{} existiert nicht.", path.display());
    }
    let to = path.with_file_name(new_name);
    if to == path {
        return Ok(Done { from: path.to_path_buf(), to });
    }
    match rename_noreplace(path, &to) {
        Ok(()) => Ok(Done { from: path.to_path_buf(), to }),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            api_bail!(AlreadyExists, "{} existiert bereits.", to.display())
        }
        Err(e) => Err(e).with_context(|| format!("{} nicht umbenennbar", path.display())),
    }
}

/// Legt einen Ordner an.
pub fn mkdir(parent: &Path, name: &str) -> Result<PathBuf> {
    require_abs(parent)?;
    validate_name(name)?;
    let p = parent.join(name);
    match std::fs::create_dir(&p) {
        Ok(()) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            api_bail!(AlreadyExists, "{} existiert bereits.", p.display())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            api_bail!(NotFound, "{} existiert nicht.", parent.display())
        }
        Err(e) => Err(e).with_context(|| format!("{} nicht anlegbar", p.display())),
    }
}

/// Legt eine leere Datei an (nie überschreiben, kein Symlink-Folgen durch `create_new`).
pub fn create_file(parent: &Path, name: &str) -> Result<PathBuf> {
    require_abs(parent)?;
    validate_name(name)?;
    let p = parent.join(name);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
        Ok(_) => Ok(p),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            api_bail!(AlreadyExists, "{} existiert bereits.", p.display())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            api_bail!(NotFound, "{} existiert nicht.", parent.display())
        }
        Err(e) => Err(e).with_context(|| format!("{} nicht anlegbar", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::error::{code_of, ErrorCode};

    fn setup() -> (tempfile::TempDir, PathBuf, Trash) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(root.join("quelle/unter")).unwrap();
        std::fs::create_dir_all(root.join("ziel")).unwrap();
        std::fs::write(root.join("quelle/a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("quelle/unter/b.txt"), b"bb").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("quelle/link")).unwrap();
        let trash = Trash::new(dir.path().join("Trash"));
        (dir, root, trash)
    }

    #[test]
    fn copy_tree_preserves_links_and_never_overwrites() {
        let (_d, root, trash) = setup();
        let p = Progress::default();
        let done = copy_into(&[root.join("quelle")], &root.join("ziel"), Conflict::Fail, &trash, &p).unwrap();
        assert_eq!(done[0].to, root.join("ziel/quelle"));
        assert_eq!(std::fs::read(root.join("ziel/quelle/unter/b.txt")).unwrap(), b"bb");
        // Symlink als Symlink, nicht der Inhalt von /etc/passwd
        let l = std::fs::symlink_metadata(root.join("ziel/quelle/link")).unwrap();
        assert!(l.file_type().is_symlink());
        assert_eq!(p.done_files.load(Ordering::Relaxed), 3);
        assert_eq!(p.total_bytes.load(Ordering::Relaxed), 5);
        // Keine temporären Reste
        assert!(!std::fs::read_dir(root.join("ziel/quelle"))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains("snap-part")));

        // Konflikte
        let again = |c| copy_into(&[root.join("quelle/a.txt")], &root.join("ziel/quelle"), c, &trash, &p);
        assert_eq!(code_of(&again(Conflict::Fail).unwrap_err()), ErrorCode::AlreadyExists);
        assert!(again(Conflict::Skip).unwrap().is_empty());
        assert_eq!(again(Conflict::KeepBoth).unwrap()[0].to, root.join("ziel/quelle/a (2).txt"));
        std::fs::write(root.join("ziel/quelle/a.txt"), b"alt").unwrap();
        again(Conflict::Replace).unwrap();
        assert_eq!(std::fs::read(root.join("ziel/quelle/a.txt")).unwrap(), b"aaa");
        assert_eq!(trash.list().len(), 1, "ersetztes Ziel liegt im Papierkorb");
        // Kopie im selben Ordner → „(2)“
        let same = copy_into(&[root.join("quelle/a.txt")], &root.join("quelle"), Conflict::Fail, &trash, &p).unwrap();
        assert_eq!(same[0].to, root.join("quelle/a (2).txt"));
        // Ordner nicht in sich selbst
        let err = copy_into(&[root.join("quelle")], &root.join("quelle/unter"), Conflict::Fail, &trash, &p);
        assert_eq!(code_of(&err.unwrap_err()), ErrorCode::InvalidInput);
    }

    #[test]
    fn move_rename_mkdir_and_cancel() {
        let (_d, root, trash) = setup();
        let p = Progress::default();
        let done = move_into(&[root.join("quelle/a.txt")], &root.join("ziel"), Conflict::Fail, &trash, &p).unwrap();
        assert_eq!(done[0].to, root.join("ziel/a.txt"));
        assert!(!root.join("quelle/a.txt").exists());
        // Verschieben an denselben Ort: nichts
        assert!(move_into(&[root.join("ziel/a.txt")], &root.join("ziel"), Conflict::Fail, &trash, &p)
            .unwrap()
            .is_empty());
        // Umbenennen ohne Überschreiben
        std::fs::write(root.join("ziel/b.txt"), b"b").unwrap();
        assert_eq!(code_of(&rename(&root.join("ziel/a.txt"), "b.txt").unwrap_err()), ErrorCode::AlreadyExists);
        assert_eq!(rename(&root.join("ziel/a.txt"), "c.txt").unwrap().to, root.join("ziel/c.txt"));
        for bad in ["", ".", "..", "a/b", "x\0y", &"n".repeat(256)] {
            assert_eq!(code_of(&rename(&root.join("ziel/c.txt"), bad).unwrap_err()), ErrorCode::InvalidInput);
            assert!(mkdir(&root, bad).is_err());
        }
        assert_eq!(mkdir(&root, "Neu").unwrap(), root.join("Neu"));
        assert_eq!(code_of(&mkdir(&root, "Neu").unwrap_err()), ErrorCode::AlreadyExists);
        assert_eq!(code_of(&mkdir(&root.join("fehlt"), "x").unwrap_err()), ErrorCode::NotFound);
        assert_eq!(code_of(&mkdir(Path::new("rel"), "x").unwrap_err()), ErrorCode::InvalidInput);

        // Abbruch: nichts Halbes bleibt liegen
        let cancelled = Progress::default();
        cancelled.cancel.store(true, Ordering::Relaxed);
        let err = copy_into(&[root.join("quelle")], &root.join("Neu"), Conflict::Fail, &trash, &cancelled).unwrap_err();
        assert_eq!(code_of(&err), ErrorCode::Cancelled);
        assert!(!root.join("Neu/quelle").exists());
        // Fehlende Quelle / fehlendes Ziel
        let e = move_into(&[root.join("weg")], &root.join("ziel"), Conflict::Fail, &trash, &p).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::NotFound);
        let e = move_into(&[root.join("ziel/c.txt")], &root.join("weg"), Conflict::Fail, &trash, &p).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::NotFound);
        assert_eq!(unique_path(&root.join(".bashrc")), root.join(".bashrc (2)"));
    }

    #[test]
    fn large_file_is_copied_in_chunks() {
        let (_d, root, trash) = setup();
        let big = root.join("gross.bin");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(CHUNKED_FROM + 123).unwrap();
        let p = Progress::default();
        copy_into(std::slice::from_ref(&big), &root.join("ziel"), Conflict::Fail, &trash, &p).unwrap();
        assert_eq!(std::fs::metadata(root.join("ziel/gross.bin")).unwrap().len(), CHUNKED_FROM + 123);
        assert_eq!(p.done_bytes.load(Ordering::Relaxed), CHUNKED_FROM + 123);
    }
}
