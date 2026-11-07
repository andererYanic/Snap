//! Aufträge für Dateioperationen: Ausführung, Fortschritt, Abbruch und „Rückgängig“.

use super::archive::{self, Password};
use super::ops::{self, Conflict, Done, Progress};
use super::trash::{remove_any, Trash};
use crate::api_bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// So viele Schritte lassen sich rückgängig machen (nur im Speicher).
const UNDO_DEPTH: usize = 20;
/// Höchstzahl Einträge pro Operation (begrenzt Speicher und Antwortgröße).
pub const MAX_PATHS: usize = 10_000;

/// Eine Dateioperation. Alle Pfade absolut.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub enum FileOp {
    Copy {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        #[serde(default)]
        conflict: Conflict,
    },
    Move {
        sources: Vec<PathBuf>,
        dest: PathBuf,
        #[serde(default)]
        conflict: Conflict,
    },
    Rename {
        path: PathBuf,
        new_name: String,
    },
    Mkdir {
        parent: PathBuf,
        name: String,
    },
    /// Leere Datei anlegen (nie überschreiben).
    CreateFile {
        parent: PathBuf,
        name: String,
    },
    /// In den Papierkorb (wiederherstellbar).
    Trash {
        paths: Vec<PathBuf>,
    },
    /// Endgültig löschen (nicht rückgängig zu machen).
    Delete {
        paths: Vec<PathBuf>,
    },
    /// Dateien und Ordner in ein neues ZIP neben der ersten Quelle packen.
    Compress {
        sources: Vec<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// `path` durch `with` ersetzen (z. B. bearbeitetes Bild): das Original wandert in den
    /// Papierkorb, `with` (im selben Ordner) nimmt seinen Platz ein. Rückgängig stellt es wieder her.
    Replace {
        path: PathBuf,
        with: PathBuf,
    },
    /// Archiv (ZIP, tar, tar.gz) in einen neuen Ordner daneben entpacken.
    Extract {
        archive: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<Password>,
    },
}

impl FileOp {
    /// Kurzbeschreibung für Meldungen und das Audit-Log.
    pub fn label(&self) -> String {
        match self {
            FileOp::Copy { sources, .. } => format!("{} Einträge kopieren", sources.len()),
            FileOp::Move { sources, .. } => format!("{} Einträge verschieben", sources.len()),
            FileOp::Rename { new_name, .. } => format!("umbenennen in '{new_name}'"),
            FileOp::Mkdir { name, .. } => format!("Ordner '{name}' anlegen"),
            FileOp::CreateFile { name, .. } => format!("Datei '{name}' anlegen"),
            FileOp::Trash { paths } => format!("{} Einträge in den Papierkorb", paths.len()),
            FileOp::Delete { paths } => format!("{} Einträge endgültig löschen", paths.len()),
            FileOp::Compress { sources, .. } => format!("{} Einträge packen", sources.len()),
            FileOp::Replace { path, .. } => {
                format!("'{}' ersetzen", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            }
            FileOp::Extract { archive, .. } => format!(
                "'{}' entpacken",
                archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ),
        }
    }

    fn path_count(&self) -> usize {
        match self {
            FileOp::Copy { sources, .. } | FileOp::Move { sources, .. } => sources.len(),
            FileOp::Trash { paths } | FileOp::Delete { paths } => paths.len(),
            FileOp::Compress { sources, .. } => sources.len(),
            _ => 1,
        }
    }
}

/// Ergebnis einer Operation.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Quelle → Ziel (kopiert, verschoben, umbenannt, angelegt).
    pub done: Vec<Done>,
    /// Kennungen der Papierkorb-Einträge.
    pub trashed: Vec<String>,
    /// Endgültig gelöschte Pfade.
    pub deleted: Vec<PathBuf>,
    /// Beim Entpacken ausgelassene Verknüpfungen und Sonderdateien.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub skipped: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Outcome {
    /// Ordner, deren Inhalt sich geändert hat (für den Suchindex).
    pub fn affected_dirs(&self, op: &FileOp) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut add = |p: &Path| {
            if let Some(parent) = p.parent() {
                dirs.push(parent.to_path_buf());
            }
        };
        for d in &self.done {
            add(&d.from);
            add(&d.to);
        }
        for p in &self.deleted {
            add(p);
        }
        if let FileOp::Trash { paths } = op {
            paths.iter().for_each(|p| add(p));
        }
        dirs.sort();
        dirs.dedup();
        dirs
    }
}

#[derive(Debug, Clone)]
enum Undo {
    /// Zurückverschieben/-benennen (Ziel → Quelle).
    MoveBack(Vec<Done>),
    /// Aus dem Papierkorb wiederherstellen.
    Restore(Vec<String>),
    /// Kopien in den Papierkorb.
    TrashCopies(Vec<PathBuf>),
    /// Angelegten (leeren) Ordner entfernen.
    RemoveDir(PathBuf),
    /// Ersetzte Datei: neue Fassung in den Papierkorb, Original zurückholen.
    ReplaceBack { path: PathBuf, original: String },
}

/// Verwaltet laufende Aufträge und den „Rückgängig“-Stapel.
pub struct FileJobs {
    pub trash: Trash,
    next: AtomicU64,
    running: Mutex<HashMap<u64, Arc<Progress>>>,
    undo: Mutex<VecDeque<(String, Undo)>>,
}

impl FileJobs {
    pub fn new(trash: Trash) -> Self {
        Self { trash, next: AtomicU64::new(1), running: Mutex::new(HashMap::new()), undo: Mutex::new(VecDeque::new()) }
    }

    /// Meldet einen neuen Auftrag an (für Fortschritt und Abbruch).
    pub fn register(&self) -> (u64, Arc<Progress>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let p = Arc::new(Progress::default());
        self.running.lock().unwrap_or_else(|e| e.into_inner()).insert(id, p.clone());
        (id, p)
    }

    pub fn unregister(&self, id: u64) {
        self.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    }

    /// Bricht einen laufenden Auftrag ab. `false`, wenn er nicht (mehr) läuft.
    pub fn cancel(&self, id: u64) -> bool {
        match self.running.lock().unwrap_or_else(|e| e.into_inner()).get(&id) {
            Some(p) => {
                p.cancel.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Führt eine Operation aus (blockierend – in `spawn_blocking` aufrufen) und merkt sie
    /// sich für „Rückgängig“.
    pub fn run(&self, op: &FileOp, p: &Progress) -> Result<Outcome> {
        if op.path_count() > MAX_PATHS {
            api_bail!(InvalidInput, "Zu viele Einträge auf einmal (höchstens {MAX_PATHS}).");
        }
        let mut out = Outcome::default();
        let undo = match op {
            FileOp::Copy { sources, dest, conflict } => {
                out.done = ops::copy_into(sources, dest, *conflict, &self.trash, p)?;
                Some(Undo::TrashCopies(out.done.iter().map(|d| d.to.clone()).collect()))
            }
            FileOp::Move { sources, dest, conflict } => {
                out.done = ops::move_into(sources, dest, *conflict, &self.trash, p)?;
                Some(Undo::MoveBack(out.done.clone()))
            }
            FileOp::Rename { path, new_name } => {
                out.done = vec![ops::rename(path, new_name)?];
                Some(Undo::MoveBack(out.done.clone()))
            }
            FileOp::Mkdir { parent, name } => {
                let created = ops::mkdir(parent, name)?;
                out.done = vec![Done { from: created.clone(), to: created.clone() }];
                Some(Undo::RemoveDir(created))
            }
            FileOp::CreateFile { parent, name } => {
                let created = ops::create_file(parent, name)?;
                out.done = vec![Done { from: created.clone(), to: created.clone() }];
                // Rückgängig: in den Papierkorb – auch wenn die Datei inzwischen bearbeitet wurde.
                Some(Undo::TrashCopies(vec![created]))
            }
            FileOp::Trash { paths } => {
                p.total_files.store(paths.len() as u64, Ordering::Relaxed);
                for path in paths {
                    if p.cancel.load(Ordering::Relaxed) {
                        api_bail!(Cancelled, "Vorgang abgebrochen.");
                    }
                    out.trashed.push(self.trash.trash(path)?.to_string_lossy().into_owned());
                    p.done_files.fetch_add(1, Ordering::Relaxed);
                }
                Some(Undo::Restore(out.trashed.clone()))
            }
            FileOp::Replace { path, with } => {
                if !path.is_absolute() || !with.is_absolute() || path.parent() != with.parent() {
                    api_bail!(InvalidInput, "Ersetzen nur mit einer Datei im selben Ordner.");
                }
                let (Ok(old), Ok(new)) = (std::fs::symlink_metadata(path), std::fs::symlink_metadata(with)) else {
                    api_bail!(NotFound, "Datei existiert nicht.");
                };
                if !old.is_file() || !new.is_file() {
                    api_bail!(InvalidInput, "Nur Dateien lassen sich ersetzen.");
                }
                let id = self.trash.trash(path)?;
                if let Err(e) = ops::rename_noreplace(with, path) {
                    // Original sofort zurück, nichts verlieren
                    let _ = self.trash.restore(&id.to_string_lossy(), false);
                    return Err(e).context("Ersetzen fehlgeschlagen");
                }
                out.done = vec![Done { from: with.clone(), to: path.clone() }];
                out.trashed = vec![id.to_string_lossy().into_owned()];
                Some(Undo::ReplaceBack { path: path.clone(), original: id.to_string_lossy().into_owned() })
            }
            FileOp::Compress { sources, name } => {
                let x = archive::compress(sources, name.as_deref(), p)?;
                out.done = vec![Done { from: sources[0].clone(), to: x.archive.clone() }];
                out.skipped = x.skipped;
                Some(Undo::TrashCopies(vec![x.archive]))
            }
            FileOp::Extract { archive, password } => {
                let x = archive::extract(archive, password.as_ref(), None, p)?;
                out.done = vec![archive::done_of(archive, &x)];
                out.skipped = x.skipped;
                // Rückgängig: der neue Ordner wandert in den Papierkorb
                Some(Undo::TrashCopies(vec![x.dir]))
            }
            FileOp::Delete { paths } => {
                for path in paths {
                    if !path.is_absolute() {
                        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
                    }
                }
                for path in paths {
                    remove_any(path)?;
                    out.deleted.push(path.clone());
                }
                None
            }
        };
        if let Some(u) = undo {
            let mut stack = self.undo.lock().unwrap_or_else(|e| e.into_inner());
            stack.push_back((op.label(), u));
            while stack.len() > UNDO_DEPTH {
                stack.pop_front();
            }
        }
        Ok(out)
    }

    /// Macht die letzte Operation rückgängig. Gibt (Beschreibung, betroffene Ordner) zurück.
    pub fn undo(&self) -> Result<(String, Vec<PathBuf>)> {
        let Some((label, action)) = self.undo.lock().unwrap_or_else(|e| e.into_inner()).pop_back() else {
            api_bail!(NotFound, "Es gibt nichts rückgängig zu machen.");
        };
        let mut dirs = Vec::new();
        let p = Progress::default();
        match action {
            Undo::MoveBack(done) => {
                for d in done.iter().rev() {
                    let (Some(from_dir), Some(from_name)) = (d.from.parent(), d.from.file_name()) else { continue };
                    dirs.extend([from_dir.to_path_buf(), d.to.parent().map(Path::to_path_buf).unwrap_or_default()]);
                    if d.to.parent() == Some(from_dir) {
                        ops::rename(&d.to, &from_name.to_string_lossy())?;
                    } else {
                        let moved =
                            ops::move_into(std::slice::from_ref(&d.to), from_dir, Conflict::Fail, &self.trash, &p)?;
                        // Wurde beim Verschieben umbenannt („(2)“), den alten Namen wiederherstellen.
                        if let Some(m) = moved.first() {
                            if m.to != d.from {
                                ops::rename(&m.to, &from_name.to_string_lossy())?;
                            }
                        }
                    }
                }
            }
            Undo::Restore(ids) => {
                for id in ids.iter().rev() {
                    let restored = self.trash.restore(id, false)?;
                    dirs.extend(restored.parent().map(Path::to_path_buf));
                }
            }
            Undo::TrashCopies(paths) => {
                for path in &paths {
                    self.trash.trash(path)?;
                    dirs.extend(path.parent().map(Path::to_path_buf));
                }
            }
            Undo::ReplaceBack { path, original } => {
                self.trash.trash(&path)?;
                self.trash.restore(&original, false)?;
                dirs.extend(path.parent().map(Path::to_path_buf));
            }
            Undo::RemoveDir(dir) => {
                if std::fs::remove_dir(&dir).is_err() {
                    api_bail!(InvalidInput, "{} ist nicht mehr leer und bleibt bestehen.", dir.display());
                }
                dirs.extend(dir.parent().map(Path::to_path_buf));
            }
        }
        Ok((label, dirs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_can_be_undone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a/x.txt"), b"x").unwrap();
        let jobs = FileJobs::new(Trash::new(dir.path().join("Trash")));
        let p = Progress::default();

        let mv = FileOp::Move { sources: vec![root.join("a/x.txt")], dest: root.join("b"), conflict: Conflict::Fail };
        jobs.run(&mv, &p).unwrap();
        assert!(root.join("b/x.txt").exists());
        let ren = FileOp::Rename { path: root.join("b/x.txt"), new_name: "y.txt".into() };
        jobs.run(&ren, &p).unwrap();
        let cp = FileOp::Copy { sources: vec![root.join("b/y.txt")], dest: root.join("a"), conflict: Conflict::Fail };
        jobs.run(&cp, &p).unwrap();
        let tr = FileOp::Trash { paths: vec![root.join("a/y.txt")] };
        let out = jobs.run(&tr, &p).unwrap();
        assert_eq!(out.trashed.len(), 1);
        jobs.run(&FileOp::Mkdir { parent: root.clone(), name: "neu".into() }, &p).unwrap();
        // Datei anlegen: leer, nie überschreiben, Rückgängig legt sie in den Papierkorb
        jobs.run(&FileOp::CreateFile { parent: root.clone(), name: "notiz.txt".into() }, &p).unwrap();
        assert_eq!(std::fs::metadata(root.join("notiz.txt")).unwrap().len(), 0);
        assert!(jobs.run(&FileOp::CreateFile { parent: root.clone(), name: "notiz.txt".into() }, &p).is_err());
        assert!(jobs.run(&FileOp::CreateFile { parent: root.clone(), name: "../ausbruch.txt".into() }, &p).is_err());
        jobs.undo().unwrap();
        assert!(!root.join("notiz.txt").exists());

        // Rückwärts: Ordner weg, Kopie zurück aus Papierkorb, Kopie in Papierkorb, Name, Ort
        jobs.undo().unwrap();
        assert!(!root.join("neu").exists());
        jobs.undo().unwrap();
        assert!(root.join("a/y.txt").exists());
        jobs.undo().unwrap();
        assert!(!root.join("a/y.txt").exists());
        jobs.undo().unwrap();
        assert!(root.join("b/x.txt").exists());
        jobs.undo().unwrap();
        assert!(root.join("a/x.txt").exists() && !root.join("b/x.txt").exists());
        assert!(jobs.undo().is_err(), "Stapel leer");

        // Endgültiges Löschen ist nicht rückgängig zu machen
        jobs.run(&FileOp::Delete { paths: vec![root.join("a/x.txt")] }, &p).unwrap();
        assert!(!root.join("a/x.txt").exists());
        assert!(jobs.undo().is_err());
        // Abbruch über die Kennung
        let (id, prog) = jobs.register();
        assert!(jobs.cancel(id));
        assert!(prog.cancel.load(Ordering::Relaxed));
        jobs.unregister(id);
        assert!(!jobs.cancel(id));
    }

    #[test]
    fn extract_and_undo() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        w.start_file("a.txt", zip::write::SimpleFileOptions::default()).unwrap();
        w.write_all(b"inhalt").unwrap();
        std::fs::write(root.join("Paket.zip"), w.finish().unwrap().into_inner()).unwrap();
        let jobs = FileJobs::new(Trash::new(dir.path().join("Trash")));
        let op = FileOp::Extract { archive: root.join("Paket.zip"), password: None };
        // Passwort taucht weder in der Beschreibung noch in Debug auf
        let secret = FileOp::Extract { archive: root.join("Paket.zip"), password: Some(Password("geheim123".into())) };
        assert!(!format!("{secret:?}").contains("geheim123") && !secret.label().contains("geheim123"));
        let out = jobs.run(&op, &Progress::default()).unwrap();
        assert_eq!(out.done[0].to, root.join("Paket"));
        assert_eq!(std::fs::read(root.join("Paket/a.txt")).unwrap(), b"inhalt");
        assert_eq!(out.affected_dirs(&op), vec![root.clone()]);
        jobs.undo().unwrap();
        assert!(!root.join("Paket").exists() && root.join("Paket.zip").exists());
    }

    #[test]
    fn replace_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("bild.jpg"), b"original").unwrap();
        std::fs::write(root.join(".snap-edit-1.jpg"), b"bearbeitet").unwrap();
        let jobs = FileJobs::new(Trash::new(dir.path().join("Trash")));
        let p = Progress::default();
        // Nur im selben Ordner
        let other = dir.path().join("anders.jpg");
        std::fs::write(&other, b"x").unwrap();
        assert!(jobs.run(&FileOp::Replace { path: root.join("bild.jpg"), with: other }, &p).is_err());
        let op = FileOp::Replace { path: root.join("bild.jpg"), with: root.join(".snap-edit-1.jpg") };
        let out = jobs.run(&op, &p).unwrap();
        assert_eq!(out.trashed.len(), 1);
        assert_eq!(std::fs::read(root.join("bild.jpg")).unwrap(), b"bearbeitet");
        assert!(!root.join(".snap-edit-1.jpg").exists());
        jobs.undo().unwrap();
        assert_eq!(std::fs::read(root.join("bild.jpg")).unwrap(), b"original");
    }
}
