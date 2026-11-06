//! Sichern, Durchsuchen, Wiederherstellen und Prüfen.

use super::chunker::Chunker;
use super::repo::{chunk_name, tree_name, Catalog, Repo, SnapshotInfo, SourceRoot, Tree, TreeEntry};
use crate::api_bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Ordner, die nie gesichert werden (Caches, Abhängigkeiten, Versionsverwaltung, Papierkorb).
const EXCLUDED: &[&str] = &[
    ".cache",
    "node_modules",
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".venv",
    ".tox",
    "target",
    ".Trash",
    "lost+found",
];

/// Fortschritt eines Laufs (von mehreren Stellen lesbar).
#[derive(Default, Debug)]
pub struct BackupProgress {
    pub files: AtomicU64,
    pub bytes: AtomicU64,
    pub new_bytes: AtomicU64,
}

/// Bekannte Inhalte der Ablage (lokaler Zwischenspeicher, damit nichts doppelt hochgeladen wird).
pub struct Known {
    path: PathBuf,
    set: HashSet<String>,
    added: Vec<String>,
}

impl Known {
    pub fn load(path: PathBuf) -> Self {
        let set = std::fs::read_to_string(&path).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default();
        Self { path, set, added: Vec::new() }
    }
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
    fn insert(&mut self, id: String) {
        if self.set.insert(id.clone()) {
            self.added.push(id);
        }
    }
    /// Neue Einträge anhängen (nach erfolgreichem Lauf).
    pub fn flush(&mut self) -> Result<()> {
        use std::io::Write;
        if self.added.is_empty() {
            return Ok(());
        }
        if let Some(p) = self.path.parent() {
            crate::secure_fs::ensure_private_dir(p)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&self.path)?;
        for id in self.added.drain(..) {
            writeln!(f, "{id}")?;
        }
        Ok(())
    }
    /// Ersetzt den Inhalt (z. B. nach dem Ausdünnen).
    pub fn replace(&mut self, ids: HashSet<String>) -> Result<()> {
        use std::io::Write;
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
            for id in &ids {
                writeln!(f, "{id}")?;
            }
        }
        std::fs::rename(&tmp, &self.path)?;
        self.set = ids;
        self.added.clear();
        Ok(())
    }
}

fn mtime_ns(m: &std::fs::Metadata) -> i64 {
    m.mtime().saturating_mul(1_000_000_000).saturating_add(m.mtime_nsec())
}

/// Alle Inhalte (Stücke und Bäume), auf die die Stände verweisen.
pub async fn referenced(repo: &Repo, catalog: &Catalog) -> Result<HashSet<String>> {
    let (trees, chunks) = referenced_split(repo, catalog).await?;
    Ok(trees.into_iter().chain(chunks).collect())
}

/// Wie [`referenced`], getrennt nach (Bäume, Stücke). Lädt und prüft dabei alle Bäume.
pub async fn referenced_split(repo: &Repo, catalog: &Catalog) -> Result<(HashSet<String>, HashSet<String>)> {
    let mut seen = HashSet::new();
    let mut chunks = HashSet::new();
    let mut stack: Vec<String> =
        catalog.snapshots.iter().flat_map(|s| s.sources.iter().map(|r| r.tree.clone())).collect();
    while let Some(t) = stack.pop() {
        if !seen.insert(t.clone()) {
            continue;
        }
        let tree = repo.load_tree(&t).await?;
        for e in tree.entries {
            chunks.extend(e.chunks);
            if let Some(sub) = e.tree {
                stack.push(sub);
            }
        }
    }
    Ok((seen, chunks))
}

/// Erstellt einen neuen Stand der Ordner `sources`. Unveränderte Dateien (Größe und mtime wie im
/// letzten Stand) werden nicht gelesen.
pub async fn backup(
    repo: &Repo,
    known: &mut Known,
    sources: &[PathBuf],
    progress: &BackupProgress,
) -> Result<SnapshotInfo> {
    let mut catalog = repo.load_catalog().await?;
    if known.is_empty() && !catalog.snapshots.is_empty() {
        // Neues Gerät oder verlorener Zwischenspeicher: aus den Ständen wiederaufbauen.
        for id in referenced(repo, &catalog).await? {
            known.insert(id);
        }
    }
    let prev = catalog.snapshots.last().cloned();
    let mut roots = Vec::new();
    for src in sources {
        let meta = std::fs::symlink_metadata(src).with_context(|| format!("{} nicht lesbar", src.display()))?;
        if !meta.is_dir() {
            api_bail!(InvalidInput, "{} ist kein Ordner.", src.display());
        }
        let prev_tree = prev
            .as_ref()
            .and_then(|p| p.sources.iter().find(|r| Path::new(&r.path) == src.as_path()))
            .map(|r| r.tree.clone());
        let tree = Box::pin(backup_dir(repo, known, src, prev_tree, progress)).await?;
        roots.push(SourceRoot { path: src.to_string_lossy().into_owned(), tree });
    }
    let info = SnapshotInfo {
        id: data_encoding::HEXLOWER.encode(&{
            let mut b = [0u8; 8];
            getrandom::getrandom(&mut b).map_err(|e| anyhow::anyhow!("Zufall: {e}"))?;
            b
        }),
        time: crate::database::now_unix(),
        host: std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string(),
        sources: roots,
        files: progress.files.load(Ordering::Relaxed),
        bytes: progress.bytes.load(Ordering::Relaxed),
        new_bytes: progress.new_bytes.load(Ordering::Relaxed),
    };
    catalog.snapshots.push(info.clone());
    repo.save_catalog(&catalog).await?;
    known.flush()?;
    Ok(info)
}

async fn put_tree(repo: &Repo, known: &mut Known, tree: &Tree) -> Result<String> {
    let plain = serde_json::to_vec(tree)?;
    let id = repo.keys.content_id(&plain);
    if !known.contains(&id) {
        let name = tree_name(&id);
        repo.storage.put(&name, repo.keys.seal(&name, &plain)?).await?;
        known.insert(id.clone());
    }
    Ok(id)
}

async fn backup_dir(
    repo: &Repo,
    known: &mut Known,
    dir: &Path,
    prev: Option<String>,
    progress: &BackupProgress,
) -> Result<String> {
    let prev_entries: HashMap<String, TreeEntry> = match prev {
        Some(id) => repo
            .load_tree(&id)
            .await
            .map(|t| t.entries.into_iter().map(|e| (e.name.clone(), e)).collect())
            .unwrap_or_default(),
        None => HashMap::new(),
    };
    let mut names: Vec<(String, std::fs::Metadata)> = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("{} nicht lesbar", dir.display()))?.flatten() {
        let Ok(name) = e.file_name().into_string() else { continue };
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() && (EXCLUDED.contains(&name.as_str()) || dir.join(&name).join("CACHEDIR.TAG").exists()) {
            continue;
        }
        names.push((name, meta));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    let mut entries = Vec::with_capacity(names.len());
    for (name, meta) in names {
        let path = dir.join(&name);
        let mode = meta.permissions().mode() & 0o7777;
        let ft = meta.file_type();
        if ft.is_dir() {
            let prev_sub = prev_entries.get(&name).filter(|p| p.kind == 'd').and_then(|p| p.tree.clone());
            let tree = Box::pin(backup_dir(repo, known, &path, prev_sub, progress)).await?;
            entries.push(TreeEntry {
                name,
                kind: 'd',
                size: 0,
                mtime: mtime_ns(&meta),
                mode,
                chunks: vec![],
                tree: Some(tree),
                target: None,
            });
        } else if ft.is_symlink() {
            let target = std::fs::read_link(&path).ok().and_then(|t| t.to_str().map(str::to_string));
            entries.push(TreeEntry {
                name,
                kind: 'l',
                size: 0,
                mtime: mtime_ns(&meta),
                mode: 0,
                chunks: vec![],
                tree: None,
                target,
            });
        } else if ft.is_file() {
            let size = meta.len();
            let mtime = mtime_ns(&meta);
            progress.files.fetch_add(1, Ordering::Relaxed);
            progress.bytes.fetch_add(size, Ordering::Relaxed);
            // Unverändert → Stückliste übernehmen, ohne die Datei zu lesen.
            if let Some(p) = prev_entries.get(&name).filter(|p| p.kind == 'f' && p.size == size && p.mtime == mtime) {
                if p.chunks.iter().all(|c| known.contains(c)) {
                    entries.push(TreeEntry {
                        name,
                        kind: 'f',
                        size,
                        mtime,
                        mode,
                        chunks: p.chunks.clone(),
                        tree: None,
                        target: None,
                    });
                    continue;
                }
            }
            let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&path) {
                Ok(f) => f,
                Err(e) => {
                    tracing::debug!("{} übersprungen: {e}", path.display());
                    continue;
                }
            };
            let mut chunks = Vec::new();
            for chunk in Chunker::new(std::io::BufReader::new(file)) {
                let chunk = chunk.with_context(|| format!("{} nicht lesbar", path.display()))?;
                let id = repo.keys.content_id(&chunk);
                if !known.contains(&id) {
                    let name = chunk_name(&id);
                    progress.new_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    let blob = repo.keys.seal(&name, &chunk)?;
                    repo.storage.put(&name, blob).await?;
                    known.insert(id.clone());
                }
                chunks.push(id);
            }
            entries.push(TreeEntry { name, kind: 'f', size, mtime, mode, chunks, tree: None, target: None });
        }
    }
    put_tree(repo, known, &Tree { entries }).await
}

/// Eintrag beim Durchsuchen eines Stands.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BrowseEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

/// Prüft einen Namen aus der Sicherung (Bäume sind authentifiziert – trotzdem defensiv).
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

async fn find(repo: &Repo, snap: &SnapshotInfo, path: &Path) -> Result<(TreeEntry, PathBuf)> {
    // Längste passende Quelle
    let Some(src) = snap.sources.iter().filter(|s| path.starts_with(&s.path)).max_by_key(|s| s.path.len()) else {
        api_bail!(NotFound, "{} gehört nicht zu diesem Stand.", path.display());
    };
    let mut entry = TreeEntry {
        name: Path::new(&src.path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        kind: 'd',
        size: 0,
        mtime: 0,
        mode: 0o755,
        chunks: vec![],
        tree: Some(src.tree.clone()),
        target: None,
    };
    let rel = path.strip_prefix(&src.path).unwrap_or(Path::new(""));
    for comp in rel.components() {
        let name = comp.as_os_str().to_string_lossy();
        let Some(t) = entry.tree.clone() else {
            api_bail!(NotFound, "{} gibt es in diesem Stand nicht.", path.display())
        };
        let tree = repo.load_tree(&t).await?;
        let Some(e) = tree.entries.into_iter().find(|e| e.name == name) else {
            api_bail!(NotFound, "{} gibt es in diesem Stand nicht.", path.display());
        };
        entry = e;
    }
    Ok((entry, path.to_path_buf()))
}

pub fn snapshot<'a>(catalog: &'a Catalog, id: &str) -> Result<&'a SnapshotInfo> {
    let found: Vec<&SnapshotInfo> = catalog.snapshots.iter().filter(|s| s.id.starts_with(id)).collect();
    match found.len() {
        0 => api_bail!(NotFound, "Kein Stand {id}."),
        1 => Ok(found[0]),
        _ => api_bail!(Ambiguous, "Stand {id} ist nicht eindeutig."),
    }
}

/// Inhalt eines Ordners in einem Stand.
pub async fn browse(repo: &Repo, snap: &SnapshotInfo, path: &Path) -> Result<Vec<BrowseEntry>> {
    let (entry, _) = find(repo, snap, path).await?;
    let Some(t) = entry.tree else { api_bail!(InvalidInput, "{} ist kein Ordner.", path.display()) };
    let tree = repo.load_tree(&t).await?;
    Ok(tree
        .entries
        .into_iter()
        .map(|e| BrowseEntry { is_dir: e.kind == 'd', name: e.name, size: e.size, mtime: e.mtime })
        .collect())
}

/// Stellt `path` (Datei oder Ordner) aus einem Stand in den Ordner `dest` wieder her.
/// Nie überschreibend: Existiert das Ziel, wird ein freier Name gewählt. Gibt das Ziel zurück.
pub async fn restore(repo: &Repo, snap: &SnapshotInfo, path: &Path, dest: &Path) -> Result<(PathBuf, u64)> {
    if !dest.is_absolute() || !dest.is_dir() {
        api_bail!(InvalidInput, "Zielordner muss ein vorhandener absoluter Ordner sein.");
    }
    let (entry, _) = find(repo, snap, path).await?;
    let mut target = dest.join(&entry.name);
    if std::fs::symlink_metadata(&target).is_ok() {
        target = crate::fileops::ops::unique_path(&target);
    }
    let mut files = 0;
    Box::pin(restore_entry(repo, &entry, &target, &mut files)).await?;
    Ok((target, files))
}

async fn restore_entry(repo: &Repo, e: &TreeEntry, target: &Path, files: &mut u64) -> Result<()> {
    match e.kind {
        'd' => {
            std::fs::create_dir(target).with_context(|| format!("{} nicht anlegbar", target.display()))?;
            if let Some(t) = &e.tree {
                let tree = repo.load_tree(t).await?;
                let mut seen = HashSet::new();
                for child in tree.entries {
                    if !safe_name(&child.name) || !seen.insert(child.name.clone()) {
                        continue; // ungültig oder doppelt – nie außerhalb des Ziels schreiben
                    }
                    Box::pin(restore_entry(repo, &child, &target.join(&child.name), files)).await?;
                }
            }
            let _ = std::fs::set_permissions(target, std::fs::Permissions::from_mode((e.mode & 0o777) | 0o700));
        }
        'f' => {
            use std::io::Write;
            // create_new folgt keinem vorhandenen Symlink und überschreibt nichts.
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(target)
                .with_context(|| format!("{} nicht anlegbar", target.display()))?;
            let mut written = 0u64;
            for c in &e.chunks {
                let data = repo.load_chunk(c).await?;
                written += data.len() as u64;
                f.write_all(&data)?;
            }
            if written != e.size {
                anyhow::bail!("{}: Größe passt nicht zur Sicherung", target.display());
            }
            // Keine setuid/setgid-Bits aus einer Sicherung übernehmen.
            f.set_permissions(std::fs::Permissions::from_mode(e.mode & 0o777))?;
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(e.mtime.max(0) as u64);
            let _ = f.set_modified(t);
            *files += 1;
        }
        'l' => {
            if let Some(t) = &e.target {
                std::os::unix::fs::symlink(t, target)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Prüft alle Bäume und eine Stichprobe der Stücke (`sample` = 0: alle).
pub async fn check(repo: &Repo, sample: usize) -> Result<(usize, usize)> {
    let catalog = repo.load_catalog().await?;
    let (_, ids) = referenced_split(repo, &catalog).await?; // lädt und prüft dabei alle Bäume
    let mut chunks: Vec<&String> = ids.iter().collect();
    chunks.sort();
    fastrand::shuffle(&mut chunks);
    let n = if sample == 0 { chunks.len() } else { sample.min(chunks.len()) };
    for c in &chunks[..n] {
        repo.load_chunk(c).await?;
    }
    Ok((catalog.snapshots.len(), n))
}

#[cfg(test)]
mod tests {
    use super::super::repo::{format_recovery, BackupTarget, Storage};
    use super::*;

    fn tree_eq(a: &Path, b: &Path) {
        let list = |p: &Path| {
            let mut v: Vec<(PathBuf, Vec<u8>)> = walkdir::WalkDir::new(p)
                .follow_links(false)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file())
                .map(|e| (e.path().strip_prefix(p).unwrap().to_path_buf(), std::fs::read(e.path()).unwrap()))
                .collect();
            v.sort();
            v
        };
        assert_eq!(list(a), list(b));
    }

    #[tokio::test]
    async fn backup_restore_dedup_and_tamper() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("Dokumente");
        std::fs::create_dir_all(src.join("Steuer/2024")).unwrap();
        std::fs::create_dir_all(src.join("node_modules/x")).unwrap();
        let mut x = 0x1234_5678_9abc_def0u64;
        let big: Vec<u8> = (0..10_000_000u32)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect();
        std::fs::write(src.join("video.mp4"), &big).unwrap();
        std::fs::write(src.join("Steuer/2024/bescheid.pdf"), b"pdf-inhalt").unwrap();
        std::fs::write(src.join("leer.txt"), b"").unwrap();
        std::fs::write(src.join("node_modules/x/index.js"), b"ausgeschlossen").unwrap();
        std::os::unix::fs::symlink("Steuer", src.join("verknuepfung")).unwrap();
        let usb = tmp.path().join("usb");
        std::fs::create_dir_all(&usb).unwrap();
        let target = BackupTarget::Dir { path: usb.clone() };

        let (repo, master, recovery) = Repo::init(&target, None, "korrekt pferd batterie").await.unwrap();
        let mut known = Known::load(tmp.path().join("known"));
        let p1 = BackupProgress::default();
        let s1 = backup(&repo, &mut known, std::slice::from_ref(&src), &p1).await.unwrap();
        assert_eq!(s1.files, 3, "node_modules ausgeschlossen");
        assert!(s1.new_bytes >= 10_000_000);
        // Nichts Lesbares am Ziel
        let store = usb.join(format!("snap-backup-{}", repo.id));
        for e in walkdir::WalkDir::new(&store).into_iter().flatten().filter(|e| e.file_type().is_file()) {
            let b = std::fs::read(e.path()).unwrap();
            assert!(!b.windows(10).any(|w| w == b"pdf-inhalt"), "{}", e.path().display());
            assert!(!String::from_utf8_lossy(&b).contains("bescheid"), "Dateinamen verschlüsselt");
        }

        // Zweiter Lauf: unveränderte Dateien werden nicht gelesen (unlesbar gemacht), nur Neues gespeichert
        std::fs::set_permissions(src.join("video.mp4"), std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::write(src.join("Steuer/2024/neu.txt"), b"neu").unwrap();
        let p2 = BackupProgress::default();
        let s2 = backup(&repo, &mut known, std::slice::from_ref(&src), &p2).await.unwrap();
        std::fs::set_permissions(src.join("video.mp4"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(s2.files, 4);
        assert!(s2.new_bytes < 100, "nur die neue Datei: {}", s2.new_bytes);

        // Durchsuchen und Wiederherstellen (alter und neuer Stand)
        let cat = repo.load_catalog().await.unwrap();
        assert_eq!(cat.snapshots.len(), 2);
        let first = snapshot(&cat, &s1.id[..6]).unwrap();
        let names: Vec<String> =
            browse(&repo, first, &src.join("Steuer/2024")).await.unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["bescheid.pdf"], "alter Stand ohne neu.txt");
        let out = tmp.path().join("wiederhergestellt");
        std::fs::create_dir_all(&out).unwrap();
        let latest = snapshot(&cat, &s2.id).unwrap();
        let (restored, files) = restore(&repo, latest, &src, &out).await.unwrap();
        assert_eq!(files, 4);
        std::fs::remove_dir_all(src.join("node_modules")).unwrap();
        tree_eq(&src, &restored);
        assert_eq!(std::fs::read_link(restored.join("verknuepfung")).unwrap(), PathBuf::from("Steuer"));
        // Einzelne Datei; nie überschreiben
        let (one, _) = restore(&repo, latest, &src.join("video.mp4"), &out).await.unwrap();
        assert_eq!(std::fs::read(&one).unwrap(), big);
        let (again, _) = restore(&repo, latest, &src.join("video.mp4"), &out).await.unwrap();
        assert_eq!(again, out.join("video (2).mp4"));
        assert!(restore(&repo, latest, Path::new("/woanders"), &out).await.is_err());

        // Prüfen
        let (snaps, checked) = check(&repo, 0).await.unwrap();
        assert_eq!(snaps, 2);
        assert!(checked >= 10);

        // Neues Gerät: nur mit Passwort oder Wiederherstellungsschlüssel
        let open = |s: &str| {
            let target = target.clone();
            let id = repo.id.clone();
            let s = s.to_string();
            async move { Repo::connect(Storage::open(&target, &id, None).unwrap(), &s).await }
        };
        let (r2, m2) = open("korrekt pferd batterie").await.unwrap();
        assert_eq!(m2, master);
        assert_eq!(r2.load_catalog().await.unwrap().snapshots.len(), 2);
        assert_eq!(open(&format_recovery(&recovery)).await.unwrap().1, master);
        let err = open("falsches passwort").await.err().unwrap();
        assert_eq!(crate::api::error::code_of(&err), crate::api::error::ErrorCode::PermissionDenied);

        // Manipuliertes Stück am Ziel → Wiederherstellen schlägt fehl statt falscher Daten
        let chunk =
            walkdir::WalkDir::new(store.join("data")).into_iter().flatten().find(|e| e.file_type().is_file()).unwrap();
        let mut b = std::fs::read(chunk.path()).unwrap();
        b[40] ^= 1;
        std::fs::write(chunk.path(), b).unwrap();
        let out2 = tmp.path().join("w2");
        std::fs::create_dir_all(&out2).unwrap();
        assert!(restore(&repo, latest, &src, &out2).await.is_err());
        assert!(check(&repo, 0).await.is_err());

        // Zu kurzes Passwort, nicht erreichbares Ziel
        assert!(Repo::init(&target, None, "kurz").await.is_err());
        let gone = BackupTarget::Dir { path: tmp.path().join("nicht/da") };
        assert!(Repo::init(&gone, None, "langes passwort hier").await.is_err());
    }
}
