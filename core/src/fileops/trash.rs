//! Papierkorb nach der freedesktop.org-Spezifikation (Trash 1.0).
//!
//! - Dateien auf demselben Laufwerk wie der Home-Papierkorb landen in
//!   `$XDG_DATA_HOME/Trash/{files,info}` – dieselben Einträge sehen Dateimanager und Desktop.
//! - Auf anderen Laufwerken: `$topdir/.Trash/$uid` (nur wenn `.Trash` ein echter Ordner mit
//!   Sticky-Bit ist), sonst `$topdir/.Trash-$uid` (wird mit 0700 angelegt). So wird nie über
//!   Laufwerksgrenzen kopiert.
//! - Zu jedem Eintrag gehört eine `.trashinfo` mit Originalpfad und Löschzeit. Sie wird zuerst
//!   exklusiv angelegt (`O_EXCL`), dann die Datei verschoben – so gibt es keine Namenskollisionen.
//! - `.trashinfo`-Dateien gelten als Eingaben von außen (andere Programme schreiben sie auch):
//!   strenger Parser mit Fuzz-Ziel, wiederhergestellt wird nur an absolute Pfade ohne `..`.

use crate::api_bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

const INFO_EXT: &str = ".trashinfo";
/// Größte akzeptierte `.trashinfo` (der Pfad ist höchstens 4 KiB, prozentkodiert ×3).
const MAX_INFO_LEN: u64 = 16 * 1024;

/// Ein Papierkorb-Ordner (mit `files/` und `info/`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashDir {
    pub base: PathBuf,
    /// Basis für relative Pfade in `.trashinfo` (Laufwerks-Papierkorb); `None` = Home (absolut).
    pub topdir: Option<PathBuf>,
}

impl TrashDir {
    pub fn files(&self) -> PathBuf {
        self.base.join("files")
    }
    pub fn info(&self) -> PathBuf {
        self.base.join("info")
    }
}

/// Ein Eintrag im Papierkorb.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// Kennung für Wiederherstellen/Löschen: Pfad der `.trashinfo`.
    pub id: String,
    pub name: String,
    pub original_path: String,
    /// Löschzeit wie in der `.trashinfo` (lokale Zeit, `YYYY-MM-DDThh:mm:ss`).
    pub deleted_at: String,
    pub is_dir: bool,
    /// Bytes (nur Dateien).
    pub size: u64,
}

/// Prozentkodierung für `Path=` (RFC 2396: alles außer unreservierten Zeichen und `/`).
pub fn percent_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3)?;
            let v = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Inhalt einer `.trashinfo`: (dekodierter Pfad als Bytes, Löschzeit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashInfo {
    pub path: Vec<u8>,
    pub deletion_date: String,
}

/// Strenger Parser für `.trashinfo`. `None` bei allem, was nicht der Spezifikation entspricht
/// (fehlende Gruppe, fehlender/doppelter Pfad, NUL-Bytes, kaputte Prozentkodierung).
pub fn parse_trashinfo(content: &str) -> Option<TrashInfo> {
    let mut lines = content.lines().map(str::trim_end).filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
    if lines.next()? != "[Trash Info]" {
        return None;
    }
    let mut path = None;
    let mut date = None;
    for line in lines {
        if line.starts_with('[') {
            break; // weitere Gruppen ignorieren
        }
        let (k, v) = line.split_once('=')?;
        match k.trim() {
            "Path" if path.is_none() => path = Some(percent_decode(v.trim())?),
            "Path" => return None,
            "DeletionDate" if date.is_none() => date = Some(v.trim().to_string()),
            _ => {}
        }
    }
    let path = path?;
    if path.is_empty() || path.contains(&0) {
        return None;
    }
    let deletion_date: String = date.unwrap_or_default().chars().filter(|c| !c.is_control()).take(64).collect();
    Some(TrashInfo { path, deletion_date })
}

/// Löst den Pfad aus einer `.trashinfo` zum Wiederherstellungsziel auf. Nur absolute Ziele
/// ohne `..`/`.`; bei Laufwerks-Papierkörben relativ zum Laufwerk und innerhalb davon.
pub fn restore_target(info: &TrashInfo, topdir: Option<&Path>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let p = Path::new(std::ffi::OsStr::from_bytes(&info.path));
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return None;
    }
    let target = match topdir {
        None if p.is_absolute() => p.to_path_buf(),
        None => return None,
        Some(top) if p.is_absolute() => p.starts_with(top).then(|| p.to_path_buf())?,
        Some(top) => top.join(p),
    };
    // Ein Ziel muss einen Namen haben (nicht "/").
    target.file_name()?;
    Some(target)
}

/// Erzeugt einen Ordner mit 0700, falls er fehlt, und prüft, dass er ein echter Ordner
/// (kein Symlink) des aktuellen Benutzers ist.
fn ensure_own_dir(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) => {
            if !m.is_dir() || m.uid() != crate::secure_fs::current_uid() {
                api_bail!(PermissionDenied, "{} ist kein eigener Ordner – Papierkorb nicht nutzbar", dir.display());
            }
        }
        Err(_) => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("Papierkorb {} nicht anlegbar", dir.display()))?;
        }
    }
    Ok(())
}

/// Einhängepunkt (topdir) des Laufwerks, auf dem `path` liegt.
fn mount_root(path: &Path) -> Result<PathBuf> {
    let dev = std::fs::symlink_metadata(path)?.dev();
    let mut cur = path.to_path_buf();
    while let Some(parent) = cur.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(m) if m.dev() == dev => cur = parent.to_path_buf(),
            _ => break,
        }
    }
    Ok(cur)
}

/// Verwaltet die Papierkörbe dieses Benutzers.
#[derive(Debug, Clone)]
pub struct Trash {
    /// Home-Papierkorb, normalerweise `$XDG_DATA_HOME/Trash`.
    home: PathBuf,
}

impl Trash {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn home_dir(&self) -> TrashDir {
        TrashDir { base: self.home.clone(), topdir: None }
    }

    /// Gerät des Home-Papierkorbs (bzw. seines nächsten existierenden Vorfahren).
    fn home_dev(&self) -> Option<u64> {
        self.home.ancestors().find_map(|a| std::fs::metadata(a).ok()).map(|m| m.dev())
    }

    /// Papierkorb für eine Datei: Home, wenn auf demselben Laufwerk, sonst der des Laufwerks.
    pub fn dir_for(&self, path: &Path) -> Result<TrashDir> {
        let dev = std::fs::symlink_metadata(path)?.dev();
        if Some(dev) == self.home_dev() {
            return Ok(self.home_dir());
        }
        let top = mount_root(path)?;
        let uid = crate::secure_fs::current_uid();
        // $topdir/.Trash nur, wenn es ein echter Ordner mit Sticky-Bit ist (Spezifikation).
        let shared = top.join(".Trash");
        if let Ok(m) = std::fs::symlink_metadata(&shared) {
            if m.is_dir() && m.permissions().mode() & 0o1000 != 0 {
                let dir = shared.join(uid.to_string());
                if ensure_own_dir(&dir).is_ok() {
                    return Ok(TrashDir { base: dir, topdir: Some(top) });
                }
            }
        }
        Ok(TrashDir { base: top.join(format!(".Trash-{uid}")), topdir: Some(top) })
    }

    /// Verschiebt eine Datei oder einen Ordner in den Papierkorb. Gibt die Kennung
    /// (Pfad der `.trashinfo`) zurück.
    pub fn trash(&self, path: &Path) -> Result<PathBuf> {
        if !path.is_absolute() {
            api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
        }
        if std::fs::symlink_metadata(path).is_err() {
            api_bail!(NotFound, "{} existiert nicht.", path.display());
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            api_bail!(InvalidInput, "Ungültiger Dateiname: {}", path.display());
        };
        let td = self.dir_for(path)?;
        if path.starts_with(&td.base) {
            api_bail!(InvalidInput, "Der Papierkorb selbst kann nicht in den Papierkorb verschoben werden.");
        }
        ensure_own_dir(&td.files())?;
        ensure_own_dir(&td.info())?;
        let stored = match &td.topdir {
            Some(top) => path.strip_prefix(top).unwrap_or(path),
            None => path,
        };
        use std::os::unix::ffi::OsStrExt;
        let date = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
        let content =
            format!("[Trash Info]\nPath={}\nDeletionDate={date}\n", percent_encode(stored.as_os_str().as_bytes()));
        for n in 1..10_000u32 {
            let candidate = if n == 1 { name.clone() } else { numbered(&name, n) };
            let info_path = td.info().join(format!("{candidate}{INFO_EXT}"));
            let file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info_path);
            let mut file = match file {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("{} nicht anlegbar", info_path.display())),
            };
            // Auch ein verwaister Eintrag in files/ blockiert den Namen.
            let dest = td.files().join(&candidate);
            if std::fs::symlink_metadata(&dest).is_ok() {
                let _ = std::fs::remove_file(&info_path);
                continue;
            }
            // Kein fsync je Eintrag (kostete ~0,35 ms/Datei): Die Spezifikation verlangt es nicht,
            // und eine verwaiste .trashinfo nach einem Absturz ist harmlos (wird übersprungen).
            file.write_all(content.as_bytes())?;
            if let Err(e) = std::fs::rename(path, &dest) {
                let _ = std::fs::remove_file(&info_path);
                return Err(e).with_context(|| format!("{} nicht in den Papierkorb verschiebbar", path.display()));
            }
            return Ok(info_path);
        }
        api_bail!(AlreadyExists, "Kein freier Name im Papierkorb für {name}")
    }

    /// Alle Papierkörbe: Home und die der eingehängten Laufwerke (sofern vorhanden).
    pub fn all_dirs(&self) -> Vec<TrashDir> {
        let mut dirs = vec![self.home_dir()];
        let uid = crate::secure_fs::current_uid();
        let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
        for line in mounts.lines() {
            let mut f = line.split_whitespace();
            let (Some(dev), Some(mp)) = (f.next(), f.next()) else { continue };
            if !dev.starts_with("/dev/") || mp.contains('\\') {
                continue;
            }
            let top = PathBuf::from(mp);
            for base in [top.join(".Trash").join(uid.to_string()), top.join(format!(".Trash-{uid}"))] {
                if base != self.home
                    && std::fs::symlink_metadata(base.join("info")).map(|m| m.is_dir()).unwrap_or(false)
                {
                    dirs.push(TrashDir { base, topdir: Some(top.clone()) });
                }
            }
        }
        dirs.dedup();
        dirs
    }

    /// Einträge aller Papierkörbe, neueste zuerst. Unlesbare Einträge werden übersprungen.
    pub fn list(&self) -> Vec<TrashEntry> {
        let mut out = Vec::new();
        for td in self.all_dirs() {
            let Ok(rd) = std::fs::read_dir(td.info()) else { continue };
            for e in rd.flatten() {
                let info_path = e.path();
                let Some((name, info, target)) = read_entry(&td, &info_path) else { continue };
                let meta = std::fs::symlink_metadata(td.files().join(&name)).ok();
                let Some(meta) = meta else { continue };
                out.push(TrashEntry {
                    id: info_path.to_string_lossy().into_owned(),
                    name,
                    original_path: target.to_string_lossy().into_owned(),
                    deleted_at: info.deletion_date,
                    is_dir: meta.is_dir(),
                    size: if meta.is_file() { meta.len() } else { 0 },
                });
            }
        }
        out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// Ordnet eine Kennung einem bekannten Papierkorb zu (keine beliebigen Pfade).
    fn locate(&self, id: &str) -> Result<(TrashDir, PathBuf, String)> {
        let info_path = PathBuf::from(id);
        let name = info_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(INFO_EXT))
            .filter(|n| !n.is_empty() && *n != "." && *n != "..")
            .map(str::to_string);
        let parent = info_path.parent().map(Path::to_path_buf);
        let (Some(name), Some(parent)) = (name, parent) else {
            api_bail!(InvalidInput, "Ungültige Papierkorb-Kennung.");
        };
        // Schnellweg Home-Papierkorb (häufigster Fall), sonst alle Laufwerke prüfen.
        let home = self.home_dir();
        let found =
            if home.info() == parent { Some(home) } else { self.all_dirs().into_iter().find(|td| td.info() == parent) };
        let Some(td) = found else {
            api_bail!(NotFound, "Kein Eintrag im Papierkorb mit dieser Kennung.");
        };
        Ok((td, info_path, name))
    }

    /// Stellt einen Eintrag am Originalort wieder her. Existiert dort schon etwas, wird mit
    /// `keep_both` ein freier Name gewählt, sonst abgebrochen. Gibt den neuen Pfad zurück.
    pub fn restore(&self, id: &str, keep_both: bool) -> Result<PathBuf> {
        let (td, info_path, _) = self.locate(id)?;
        let Some((name, _, target)) = read_entry(&td, &info_path) else {
            api_bail!(InvalidInput, "Papierkorb-Eintrag ist beschädigt oder verweist auf einen ungültigen Pfad.");
        };
        let src = td.files().join(&name);
        if std::fs::symlink_metadata(&src).is_err() {
            api_bail!(NotFound, "Die Datei zum Eintrag fehlt im Papierkorb.");
        }
        let mut dest = target.clone();
        if std::fs::symlink_metadata(&dest).is_ok() {
            if !keep_both {
                api_bail!(AlreadyExists, "{} existiert bereits.", dest.display());
            }
            dest = super::ops::unique_path(&dest);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("{} nicht anlegbar", parent.display()))?;
        }
        std::fs::rename(&src, &dest)
            .with_context(|| format!("Wiederherstellen nach {} fehlgeschlagen", dest.display()))?;
        let _ = std::fs::remove_file(&info_path);
        Ok(dest)
    }

    /// Löscht einen Eintrag endgültig.
    pub fn purge(&self, id: &str) -> Result<()> {
        let (td, info_path, name) = self.locate(id)?;
        remove_any(&td.files().join(&name))?;
        std::fs::remove_file(&info_path).with_context(|| format!("{} nicht löschbar", info_path.display()))?;
        Ok(())
    }

    /// Leert alle Papierkörbe. Gibt die Zahl der gelöschten Einträge zurück.
    pub fn empty(&self) -> Result<u64> {
        let mut n = 0;
        for e in self.list() {
            if self.purge(&e.id).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

/// Liest einen Eintrag: (Name in files/, Info, Wiederherstellungsziel).
fn read_entry(td: &TrashDir, info_path: &Path) -> Option<(String, TrashInfo, PathBuf)> {
    let name = info_path.file_name()?.to_str()?.strip_suffix(INFO_EXT)?.to_string();
    let meta = std::fs::symlink_metadata(info_path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INFO_LEN {
        return None;
    }
    let content = std::fs::read_to_string(info_path).ok()?;
    let info = parse_trashinfo(&content)?;
    let target = restore_target(&info, td.topdir.as_deref())?;
    Some((name, info, target))
}

/// Löscht Datei, Symlink oder Ordner (ohne Symlinks zu folgen).
pub fn remove_any(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path).with_context(|| format!("{} existiert nicht", path.display()))?;
    if m.is_dir() {
        // remove_dir_all folgt keinen Symlinks (seit Rust 1.58.1 auch ohne Race).
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
    .with_context(|| format!("{} nicht löschbar", path.display()))
}

/// "foto.jpg" → "foto.2.jpg" (Namensschema im Papierkorb).
fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem}.{n}.{ext}"),
        _ => format!("{name}.{n}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_is_strict() {
        let ok = parse_trashinfo("[Trash Info]\nPath=/home/a/Bericht%20%C3%BC.pdf\nDeletionDate=2026-11-06T10:00:00\n")
            .unwrap();
        assert_eq!(ok.path, "/home/a/Bericht ü.pdf".as_bytes());
        assert_eq!(ok.deletion_date, "2026-11-06T10:00:00");
        for bad in [
            "",
            "Path=/x",
            "[Trash Info]\nDeletionDate=x",
            "[Trash Info]\nPath=/a\nPath=/b",
            "[Trash Info]\nPath=/a%00b",
            "[Trash Info]\nPath=/a%zz",
            "[Trash Info]\nPath=/a%2",
            "[Trash Info]\nPath=",
            "[Andere]\nPath=/a",
        ] {
            assert!(parse_trashinfo(bad).is_none(), "{bad:?}");
        }
        // Ziele: absolut ohne Navigation; Laufwerk: relativ und innerhalb
        let info = |p: &str| TrashInfo { path: p.as_bytes().to_vec(), deletion_date: String::new() };
        assert_eq!(restore_target(&info("/home/a/x"), None), Some(PathBuf::from("/home/a/x")));
        assert_eq!(restore_target(&info("x/y"), None), None);
        assert_eq!(restore_target(&info("/home/../etc/passwd"), None), None);
        assert_eq!(restore_target(&info("/"), None), None);
        let top = Path::new("/media/usb");
        assert_eq!(restore_target(&info("a/b"), Some(top)), Some(PathBuf::from("/media/usb/a/b")));
        assert_eq!(restore_target(&info("../x"), Some(top)), None);
        assert_eq!(restore_target(&info("/etc/x"), Some(top)), None);
        assert_eq!(restore_target(&info("/media/usb/z"), Some(top)), Some(PathBuf::from("/media/usb/z")));
        assert_eq!(percent_encode(b"/a b/%.txt"), "/a%20b/%25.txt");
    }

    #[test]
    fn trash_list_restore_purge() {
        let dir = tempfile::tempdir().unwrap();
        let trash = Trash::new(dir.path().join("Trash"));
        let docs = dir.path().join("Dokumente");
        std::fs::create_dir_all(docs.join("Ordner/innen")).unwrap();
        std::fs::write(docs.join("Bericht 1.pdf"), b"abc").unwrap();
        std::fs::write(docs.join("Ordner/innen/x.txt"), b"x").unwrap();

        let id1 = trash.trash(&docs.join("Bericht 1.pdf")).unwrap();
        let id2 = trash.trash(&docs.join("Ordner")).unwrap();
        assert!(!docs.join("Bericht 1.pdf").exists());
        let info = std::fs::read_to_string(&id1).unwrap();
        assert!(info.contains("Path=") && info.contains("Bericht%201.pdf"), "{info}");
        // Gleicher Name erneut → eigener Eintrag
        std::fs::write(docs.join("Bericht 1.pdf"), b"neu").unwrap();
        let id3 = trash.trash(&docs.join("Bericht 1.pdf")).unwrap();
        assert!(id3.to_string_lossy().ends_with("Bericht 1.2.pdf.trashinfo"), "{id3:?}");

        let list = trash.list();
        assert_eq!(list.len(), 3);
        let folder = list.iter().find(|e| e.name == "Ordner").unwrap();
        assert!(folder.is_dir);
        assert_eq!(folder.original_path, docs.join("Ordner").to_string_lossy());

        // Wiederherstellen (Ordner samt Inhalt), Konflikt erkennen, beide behalten
        trash.restore(&id2.to_string_lossy(), false).unwrap();
        assert!(docs.join("Ordner/innen/x.txt").exists());
        std::fs::write(docs.join("Bericht 1.pdf"), b"belegt").unwrap();
        let err = trash.restore(&id1.to_string_lossy(), false).unwrap_err();
        assert_eq!(crate::api::error::code_of(&err), crate::api::error::ErrorCode::AlreadyExists);
        let restored = trash.restore(&id1.to_string_lossy(), true).unwrap();
        assert_eq!(restored, docs.join("Bericht 1 (2).pdf"));
        assert_eq!(std::fs::read(&restored).unwrap(), b"abc");

        // Fremde Kennungen werden abgelehnt
        for bad in ["/etc/passwd", "../x.trashinfo", "/tmp/info/a.trashinfo", ""] {
            assert!(trash.restore(bad, true).is_err(), "{bad}");
            assert!(trash.purge(bad).is_err(), "{bad}");
        }
        // Manipulierte .trashinfo (Pfad mit ..) wird nicht wiederhergestellt
        std::fs::write(&id3, "[Trash Info]\nPath=/home/../etc/evil\n").unwrap();
        assert!(trash.restore(&id3.to_string_lossy(), true).is_err());
        assert!(trash.list().iter().all(|e| !e.original_path.contains("..")));

        // Endgültig löschen / leeren
        trash.purge(&id3.to_string_lossy()).unwrap();
        assert!(trash.list().is_empty());
        trash.trash(&docs.join("Ordner")).unwrap();
        assert_eq!(trash.empty().unwrap(), 1);
        assert!(std::fs::read_dir(dir.path().join("Trash/files")).unwrap().next().is_none());
        // Der Papierkorb selbst nicht
        assert!(trash.trash(&dir.path().join("Trash/files")).is_err());
    }
}
