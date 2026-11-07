//! Archive entpacken (ZIP inkl. Passwort, tar, tar.gz).
//!
//! Archive kommen von außen und werden als feindlich behandelt (Entwurf vom Nutzer freigegeben,
//! `docs/ROADMAP.md`, 2026-11-07):
//! - Ziel ist immer ein **neuer** Ordner neben dem Archiv („Fotos.zip“ → „Fotos“, sonst
//!   „Fotos (2)“). Entpackt wird zuerst in einen versteckten Arbeitsordner, der erst am Ende
//!   umbenannt wird – Abbruch oder Fehler hinterlassen nichts.
//! - Jeder Pfad im Archiv wird zerlegt und geprüft: keine absoluten Pfade, kein `..`, keine
//!   NUL-Bytes, keine Namen über 255 Bytes, höchstens [`MAX_DEPTH`] Ebenen. Ein einziger
//!   unzulässiger Pfad lehnt das ganze Archiv ab.
//! - Verknüpfungen (Symlinks, Hardlinks) und Gerätedateien werden nie angelegt, sondern
//!   ausgelassen und gezählt. Rechte aus dem Archiv werden nicht übernommen.
//! - Grenzen gegen Zip-Bomben: höchstens [`Limits::max_entries`] Einträge, höchstens
//!   [`Limits::max_bytes`] entpackte Bytes (gemessen beim Schreiben, nicht aus den Kopfdaten),
//!   und ab [`RATIO_FROM`] Bytes höchstens das [`Limits::max_ratio`]-Fache der Archivgröße.
//! - Dateien werden mit `create_new` geschrieben: doppelte Einträge überschreiben nichts.

use super::ops::{rename_noreplace, unique_path, validate_name, Done, Progress};
use crate::api_bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// Höchste Verzeichnistiefe innerhalb eines Archivs.
pub const MAX_DEPTH: usize = 64;
/// Ab so vielen entpackten Bytes greift die Grenze für das Kompressionsverhältnis.
pub const RATIO_FROM: u64 = 64 * 1024 * 1024;
/// Bleibt auf dem Laufwerk mindestens frei.
pub const FREE_RESERVE: u64 = 512 * 1024 * 1024;

/// Passwort für verschlüsselte Archive. Erscheint nie in Protokollen (`Debug` zeigt `***`).
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(transparent)]
pub struct Password(pub String);

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

/// Grenzen für ein Archiv.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: u64,
    pub max_bytes: u64,
    pub max_ratio: u64,
}

impl Limits {
    /// Standard: 100 000 Einträge, freier Platz minus Reserve, Verhältnis 1000 : 1.
    pub fn for_target(dir: &Path) -> Result<Self> {
        let free = free_space(dir)?;
        Ok(Self { max_entries: 100_000, max_bytes: free.saturating_sub(FREE_RESERVE), max_ratio: 1000 })
    }
}

/// Ergebnis des Entpackens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extracted {
    pub dir: PathBuf,
    pub files: u64,
    pub bytes: u64,
    /// Ausgelassene Verknüpfungen und Sonderdateien.
    pub skipped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zip,
    Tar,
    TarGz,
}

fn detect(path: &Path) -> Result<Kind> {
    let mut head = [0u8; 512];
    let mut f = std::fs::File::open(path).with_context(|| format!("{} nicht lesbar", path.display()))?;
    let mut n = 0;
    while n < head.len() {
        match f.read(&mut head[n..])? {
            0 => break,
            k => n += k,
        }
    }
    let head = &head[..n];
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return Ok(Kind::Zip);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return Ok(Kind::TarGz);
    }
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return Ok(Kind::Tar);
    }
    api_bail!(InvalidInput, "{} ist kein unterstütztes Archiv (ZIP, tar, tar.gz).", path.display())
}

/// Zielname für den neuen Ordner: „Fotos.zip“ → „Fotos“, „Daten.tar.gz“ → „Daten“.
pub fn folder_name(archive: &Path) -> String {
    let name = archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_lowercase();
    for ext in [".tar.gz", ".tgz", ".zip", ".tar"] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return name[..name.len() - ext.len()].to_string();
        }
    }
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => format!("{name} (entpackt)"),
    }
}

/// Zerlegt einen Pfad aus dem Archiv in geprüfte Bestandteile. `None` = Eintrag ohne Namen
/// (z. B. „./“), wird übersprungen. Fehler = unzulässiger Pfad.
pub fn safe_components(raw: &[u8]) -> Result<Option<Vec<String>>> {
    if raw.contains(&0) {
        api_bail!(InvalidInput, "Archiv enthält einen Namen mit NUL-Byte.");
    }
    let text = String::from_utf8_lossy(raw);
    if text.starts_with('/') || text.starts_with('\\') {
        api_bail!(InvalidInput, "Archiv enthält einen absoluten Pfad: {text:?}");
    }
    let mut out = Vec::new();
    for part in text.split(['/', '\\']) {
        match part {
            "" | "." => continue,
            ".." => api_bail!(InvalidInput, "Archiv enthält einen Pfad nach außen: {text:?}"),
            p => {
                // Laufwerksangaben aus Windows-Archiven („C:“) nicht als Ordner anlegen
                if out.is_empty() && p.len() == 2 && p.ends_with(':') {
                    api_bail!(InvalidInput, "Archiv enthält einen absoluten Pfad: {text:?}");
                }
                validate_name(p)?;
                out.push(p.to_string());
            }
        }
    }
    if out.len() > MAX_DEPTH {
        api_bail!(InvalidInput, "Archiv ist zu tief verschachtelt (mehr als {MAX_DEPTH} Ebenen).");
    }
    Ok(if out.is_empty() { None } else { Some(out) })
}

/// Zähler und Grenzen während des Entpackens.
struct Budget<'a> {
    limits: Limits,
    archive_size: u64,
    entries: u64,
    bytes: u64,
    files: u64,
    skipped: u64,
    p: &'a Progress,
}

impl Budget<'_> {
    fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        if self.entries > self.limits.max_entries {
            api_bail!(InvalidInput, "Archiv hat zu viele Einträge (mehr als {}).", self.limits.max_entries);
        }
        if self.p.cancel.load(Ordering::Relaxed) {
            api_bail!(Cancelled, "Vorgang abgebrochen.");
        }
        Ok(())
    }

    fn written(&mut self, n: u64) -> Result<()> {
        self.bytes += n;
        self.p.done_bytes.fetch_add(n, Ordering::Relaxed);
        if self.bytes > self.limits.max_bytes {
            api_bail!(
                Unavailable,
                "Nicht genug Platz: Entpackt wäre das Archiv größer als {:.1} GB (freier Platz abzüglich Reserve).",
                self.limits.max_bytes as f64 / 1e9
            );
        }
        if self.bytes > RATIO_FROM && self.bytes / self.archive_size.max(1) > self.limits.max_ratio {
            api_bail!(InvalidInput, "Archiv entpackt sich verdächtig stark (Zip-Bombe?) – abgebrochen.");
        }
        if self.p.cancel.load(Ordering::Relaxed) {
            api_bail!(Cancelled, "Vorgang abgebrochen.");
        }
        Ok(())
    }
}

/// Legt Ordner unterhalb von `root` an (ohne Symlinks zu folgen: alles darunter stammt von uns).
fn make_dirs(root: &Path, parts: &[String]) -> Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for part in parts {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => api_bail!(InvalidInput, "Archiv enthält „{part}“ zugleich als Datei und als Ordner."),
            Err(_) => std::fs::create_dir(&dir).with_context(|| format!("{} nicht anlegbar", dir.display()))?,
        }
    }
    Ok(dir)
}

/// Schreibt einen Eintrag als neue Datei (nie überschreiben) und zählt mit.
fn write_file(root: &Path, parts: &[String], src: &mut dyn Read, b: &mut Budget) -> Result<()> {
    let (name, dirs) = parts.split_last().expect("nicht leer");
    let dir = make_dirs(root, dirs)?;
    let path = dir.join(name);
    let mut out = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            api_bail!(InvalidInput, "Archiv enthält „{}“ mehrfach.", parts.join("/"))
        }
        Err(e) => return Err(e).with_context(|| format!("{} nicht anlegbar", path.display())),
    };
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => return Err(read_error(e)),
        };
        out.write_all(&buf[..n]).with_context(|| format!("{} nicht schreibbar", path.display()))?;
        b.written(n as u64)?;
    }
    b.files += 1;
    b.p.done_files.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Lesefehler aus dem Archiv: falsches Passwort als eigener Code, sonst „beschädigt“.
fn read_error(e: std::io::Error) -> anyhow::Error {
    let text = e.to_string();
    if text.contains("password") {
        return crate::api::error::ApiError::new(crate::api::error::ErrorCode::PasswordRequired, "Falsches Passwort.")
            .into();
    }
    crate::api::error::ApiError::new(
        crate::api::error::ErrorCode::InvalidInput,
        format!("Archiv ist beschädigt: {text}"),
    )
    .into()
}

fn extract_zip(archive: &Path, root: &Path, password: Option<&Password>, b: &mut Budget) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = match zip::ZipArchive::new(std::io::BufReader::new(file)) {
        Ok(z) => z,
        Err(e) => api_bail!(InvalidInput, "Archiv ist beschädigt: {e}"),
    };
    if zip.len() as u64 > b.limits.max_entries {
        api_bail!(InvalidInput, "Archiv hat zu viele Einträge ({}; höchstens {}).", zip.len(), b.limits.max_entries);
    }
    // Erwartete Größe für den Fortschritt (nur Anzeige; die Grenzen messen beim Schreiben).
    let declared: u64 =
        (0..zip.len()).filter_map(|i| zip.by_index_data(i).ok().map(|e| e.size())).fold(0, u64::saturating_add);
    b.p.total_bytes.store(declared, Ordering::Relaxed);
    b.p.total_files.store(zip.len() as u64, Ordering::Relaxed);
    for i in 0..zip.len() {
        b.entry()?;
        let entry = match password {
            Some(pw) => zip.by_index_decrypt(i, pw.0.as_bytes()),
            None => zip.by_index(i),
        };
        let mut entry = match entry {
            Ok(e) => e,
            Err(zip::result::ZipError::InvalidPassword) => api_bail!(PasswordRequired, "Falsches Passwort."),
            Err(zip::result::ZipError::UnsupportedArchive(m)) if m == zip::result::ZipError::PASSWORD_REQUIRED => {
                api_bail!(PasswordRequired, "Dieses Archiv ist mit einem Passwort geschützt.")
            }
            Err(zip::result::ZipError::UnsupportedArchive(m)) => {
                api_bail!(InvalidInput, "Archiv nutzt ein nicht unterstütztes Verfahren: {m}")
            }
            Err(e) => api_bail!(InvalidInput, "Archiv ist beschädigt: {e}"),
        };
        let Some(parts) = safe_components(entry.name_raw())? else { continue };
        if entry.is_symlink() {
            b.skipped += 1;
            continue;
        }
        if entry.is_dir() {
            make_dirs(root, &parts)?;
            continue;
        }
        // Nur normale Dateien (Unix-Modus ohne Typangabe gilt als Datei)
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 {
                b.skipped += 1;
                continue;
            }
        }
        write_file(root, &parts, &mut entry, b)?;
    }
    Ok(())
}

fn extract_tar(reader: &mut dyn Read, root: &Path, b: &mut Budget) -> Result<()> {
    let mut tar = tar::Archive::new(reader);
    let entries = tar.entries().map_err(read_error)?;
    for entry in entries {
        b.entry()?;
        let mut entry = entry.map_err(read_error)?;
        let kind = entry.header().entry_type();
        let raw = entry.path_bytes().into_owned();
        let Some(parts) = safe_components(&raw)? else { continue };
        if kind.is_dir() {
            make_dirs(root, &parts)?;
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            write_file(root, &parts, &mut entry, b)?;
        } else if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            // Verwaltungsdaten, von `tar` bereits ausgewertet
        } else {
            // Symlinks, Hardlinks, Geräte, FIFOs: nie anlegen
            b.skipped += 1;
        }
    }
    Ok(())
}

/// Entpackt `archive` in einen neuen Ordner daneben. Blockierend – in `spawn_blocking` aufrufen.
pub fn extract(archive: &Path, password: Option<&Password>, limits: Option<Limits>, p: &Progress) -> Result<Extracted> {
    if !archive.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", archive.display());
    }
    let meta = match std::fs::symlink_metadata(archive) {
        Ok(m) => m,
        Err(_) => api_bail!(NotFound, "{} existiert nicht.", archive.display()),
    };
    if !meta.is_file() {
        api_bail!(InvalidInput, "{} ist keine Datei.", archive.display());
    }
    let parent = archive.parent().context("Archiv ohne übergeordneten Ordner")?;
    let kind = detect(archive)?;
    let limits = match limits {
        Some(l) => l,
        None => Limits::for_target(parent)?,
    };

    // Versteckter Arbeitsordner im selben Ordner (gleiches Laufwerk → Umbenennen ist atomar)
    let work = parent.join(format!(".snap-entpacken-{:016x}", fastrand::u64(..)));
    std::fs::create_dir(&work).with_context(|| format!("{} nicht anlegbar", work.display()))?;
    let mut b = Budget { limits, archive_size: meta.len(), entries: 0, bytes: 0, files: 0, skipped: 0, p };
    let result = match kind {
        Kind::Zip => extract_zip(archive, &work, password, &mut b),
        Kind::Tar => std::fs::File::open(archive)
            .map_err(anyhow::Error::from)
            .and_then(|f| extract_tar(&mut std::io::BufReader::new(f), &work, &mut b)),
        Kind::TarGz => std::fs::File::open(archive).map_err(anyhow::Error::from).and_then(|f| {
            let mut gz = flate2::read::MultiGzDecoder::new(std::io::BufReader::new(f));
            extract_tar(&mut gz, &work, &mut b)
        }),
    };
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&work);
        return Err(e);
    }

    // Fertig: unter freiem Namen sichtbar machen (nie überschreiben)
    let plain = parent.join(folder_name(archive));
    let mut target = if std::fs::symlink_metadata(&plain).is_err() { plain } else { unique_path(&plain) };
    for _ in 0..8 {
        match rename_noreplace(&work, &target) {
            Ok(()) => {
                return Ok(Extracted { dir: target, files: b.files, bytes: b.bytes, skipped: b.skipped });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => target = unique_path(&target),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                return Err(e).with_context(|| format!("{} nicht anlegbar", target.display()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    api_bail!(AlreadyExists, "Kein freier Name für den Zielordner gefunden.")
}

/// Ergebnis für „Rückgängig“: Archiv → neuer Ordner.
pub fn done_of(archive: &Path, x: &Extracted) -> Done {
    Done { from: archive.to_path_buf(), to: x.dir.clone() }
}

/// Freier Platz (Bytes) auf dem Laufwerk von `path`.
pub fn free_space(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: statvfs ist ein reines C-Struct; Nullen sind ein gültiger Wert.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: gültiger, NUL-terminierter Pfad; `st` wird vollständig von statvfs beschrieben.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::error::{code_of, ErrorCode};
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn big() -> Limits {
        Limits { max_entries: 100_000, max_bytes: 1 << 40, max_ratio: 1000 }
    }

    /// Baut ein ZIP aus (Name, Inhalt); Name mit „/“ am Ende = Ordner.
    fn make_zip(entries: &[(&str, &[u8])], password: Option<&str>) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in entries {
            let mut opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            if let Some(pw) = password {
                opt = opt.with_aes_encryption(zip::AesMode::Aes256, pw);
            }
            if name.ends_with('/') {
                w.add_directory(*name, opt).unwrap();
            } else {
                w.start_file(*name, opt).unwrap();
                w.write_all(data).unwrap();
            }
        }
        w.finish().unwrap().into_inner()
    }

    fn files_under(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = walkdir::WalkDir::new(dir)
            .min_depth(1)
            .into_iter()
            .map(|e| e.unwrap().path().strip_prefix(dir).unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn zip_entpacken_in_neuen_ordner_ohne_ueberschreiben() {
        let d = tempfile::tempdir().unwrap();
        let zip = d.path().join("Fotos.zip");
        std::fs::write(&zip, make_zip(&[("a.txt", b"hallo"), ("Unter/", b""), ("Unter/b.txt", b"welt")], None))
            .unwrap();
        std::fs::create_dir(d.path().join("Fotos")).unwrap(); // Name schon belegt
        let x = extract(&zip, None, Some(big()), &Progress::default()).unwrap();
        assert_eq!(x.dir, d.path().join("Fotos (2)"));
        assert_eq!((x.files, x.bytes, x.skipped), (2, 9, 0));
        assert_eq!(files_under(&x.dir), ["Unter", "Unter/b.txt", "a.txt"]);
        assert_eq!(std::fs::read(x.dir.join("Unter/b.txt")).unwrap(), b"welt");
        // Kein Arbeitsordner bleibt übrig
        assert_eq!(files_under(d.path()).iter().filter(|n| n.starts_with(".snap-")).count(), 0);
    }

    #[test]
    fn unzulaessige_pfade_lehnen_das_archiv_ab() {
        for bad in ["../ausbruch.txt", "a/../../x", "/etc/passwd", "\\windows\\x", "C:/x.txt", "a/..\\..\\x"] {
            assert!(safe_components(bad.as_bytes()).is_err(), "{bad}");
        }
        assert_eq!(safe_components(b"./a//b/./c.txt").unwrap().unwrap(), ["a", "b", "c.txt"]);
        assert_eq!(safe_components(b"./").unwrap(), None);
        assert!(safe_components(b"a\0b").is_err());
        assert!(safe_components("x/".repeat(MAX_DEPTH + 1).as_bytes()).is_err());

        let d = tempfile::tempdir().unwrap();
        let zip = d.path().join("boese.zip");
        std::fs::write(&zip, make_zip(&[("gut.txt", b"1"), ("../../ausbruch.txt", b"2")], None)).unwrap();
        let e = extract(&zip, None, Some(big()), &Progress::default()).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::InvalidInput);
        // Nichts außerhalb, nichts innerhalb übrig
        assert!(!d.path().join("ausbruch.txt").exists() && !d.path().parent().unwrap().join("ausbruch.txt").exists());
        assert_eq!(files_under(d.path()), ["boese.zip"]);
    }

    #[test]
    fn symlinks_werden_ausgelassen() {
        let d = tempfile::tempdir().unwrap();
        // tar mit Symlink auf /etc/passwd und Hardlink
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        builder.append_link(&mut h, "link", "/etc/passwd").unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Link);
        h.set_size(0);
        builder.append_link(&mut h, "hard", "/etc/passwd").unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o4755); // setuid wird nicht übernommen
        h.set_cksum();
        builder.append_data(&mut h, "datei.txt", &b"abc"[..]).unwrap();
        let tar_path = d.path().join("x.tar");
        std::fs::write(&tar_path, builder.into_inner().unwrap()).unwrap();
        let x = extract(&tar_path, None, Some(big()), &Progress::default()).unwrap();
        assert_eq!((x.files, x.skipped), (1, 2));
        assert_eq!(files_under(&x.dir), ["datei.txt"]);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(x.dir.join("datei.txt")).unwrap().permissions().mode() & 0o7000, 0);
    }

    #[test]
    fn tar_gz_und_namen() {
        let d = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_cksum();
        builder.append_data(&mut h, "ordner/hallo.txt", &b"hallo"[..]).unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&builder.into_inner().unwrap()).unwrap();
        let p = d.path().join("Daten.tar.gz");
        std::fs::write(&p, gz.finish().unwrap()).unwrap();
        let x = extract(&p, None, Some(big()), &Progress::default()).unwrap();
        assert_eq!(x.dir, d.path().join("Daten"));
        assert_eq!(std::fs::read(x.dir.join("ordner/hallo.txt")).unwrap(), b"hallo");
        assert_eq!(folder_name(Path::new("/a/x.tgz")), "x");
        assert_eq!(folder_name(Path::new("/a/.zip")), ".zip (entpackt)");
    }

    #[test]
    fn passwort_aes() {
        let d = tempfile::tempdir().unwrap();
        let zip = d.path().join("geheim.zip");
        std::fs::write(&zip, make_zip(&[("geheim.txt", b"streng geheim")], Some("richtig"))).unwrap();
        let p = Progress::default();
        let e = extract(&zip, None, Some(big()), &p).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::PasswordRequired, "{e:#}");
        let e = extract(&zip, Some(&Password("falsch".into())), Some(big()), &p).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::PasswordRequired, "{e:#}");
        assert_eq!(files_under(d.path()), ["geheim.zip"], "Fehlversuche hinterlassen nichts");
        let x = extract(&zip, Some(&Password("richtig".into())), Some(big()), &p).unwrap();
        assert_eq!(std::fs::read(x.dir.join("geheim.txt")).unwrap(), b"streng geheim");
        assert_eq!(format!("{:?}", Password("richtig".into())), "***");
    }

    #[test]
    fn grenzen_gegen_zip_bomben() {
        let d = tempfile::tempdir().unwrap();
        // 100 MB Nullen komprimieren auf ~100 KB: Verhältnis ~1000:1
        let zeros = vec![0u8; 100 * 1024 * 1024];
        let zip = d.path().join("bombe.zip");
        std::fs::write(&zip, make_zip(&[("nullen.bin", &zeros)], None)).unwrap();
        let strict = Limits { max_entries: 100, max_bytes: 1 << 40, max_ratio: 100 };
        let e = extract(&zip, None, Some(strict), &Progress::default()).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::InvalidInput, "{e:#}");
        assert!(format!("{e:#}").contains("Zip-Bombe"));
        // Größengrenze (wie freier Platz)
        let small = Limits { max_entries: 100, max_bytes: 10 * 1024 * 1024, max_ratio: 1 << 30 };
        let e = extract(&zip, None, Some(small), &Progress::default()).unwrap_err();
        assert_eq!(code_of(&e), ErrorCode::Unavailable, "{e:#}");
        // Zu viele Einträge
        let many: Vec<(String, Vec<u8>)> = (0..20).map(|i| (format!("f{i}"), vec![])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        let z2 = d.path().join("viele.zip");
        std::fs::write(&z2, make_zip(&refs, None)).unwrap();
        let few = Limits { max_entries: 10, max_bytes: 1 << 40, max_ratio: 1000 };
        assert_eq!(code_of(&extract(&z2, None, Some(few), &Progress::default()).unwrap_err()), ErrorCode::InvalidInput);
        assert_eq!(files_under(d.path()), ["bombe.zip", "viele.zip"]);
    }

    #[test]
    fn doppelte_eintraege_und_abbruch() {
        let d = tempfile::tempdir().unwrap();
        // Datei und gleichnamiger Ordner
        let zip = d.path().join("konflikt.zip");
        std::fs::write(&zip, make_zip(&[("a", b"1"), ("a/b.txt", b"2")], None)).unwrap();
        assert!(extract(&zip, None, Some(big()), &Progress::default()).is_err());
        // Abbruch vor dem Start
        let ok = d.path().join("ok.zip");
        std::fs::write(&ok, make_zip(&[("x.txt", b"x")], None)).unwrap();
        let p = Progress::default();
        p.cancel.store(true, Ordering::Relaxed);
        assert_eq!(code_of(&extract(&ok, None, Some(big()), &p).unwrap_err()), ErrorCode::Cancelled);
        assert_eq!(files_under(d.path()), ["konflikt.zip", "ok.zip"]);
        // Kein Archiv
        let txt = d.path().join("kein.zip");
        std::fs::write(&txt, b"das ist kein zip").unwrap();
        assert_eq!(
            code_of(&extract(&txt, None, Some(big()), &Progress::default()).unwrap_err()),
            ErrorCode::InvalidInput
        );
    }
}
