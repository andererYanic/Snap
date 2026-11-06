//! Sicherungen und Versionen (siehe docs/BACKUP_FORMAT.md).

pub mod chunker;
pub mod engine;
pub mod repo;

use crate::api_bail;
use crate::daemon::DaemonInner;
use anyhow::Result;
use repo::{BackupTarget, Repo, SnapshotInfo, Storage};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Laufende Sicherungen (je Ablage höchstens eine).
#[derive(Default)]
pub struct BackupService {
    running: Mutex<HashSet<String>>,
}

/// Übersicht einer eingerichteten Sicherung.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BackupJob {
    pub id: String,
    pub target: BackupTarget,
    pub sources: Vec<PathBuf>,
    pub last_run: Option<i64>,
    pub last_ok: Option<i64>,
    pub last_error: Option<String>,
    pub last_snapshot: Option<String>,
    pub running: bool,
}

/// Ergebnis beim Einrichten: der Wiederherstellungsschlüssel wird genau einmal angezeigt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BackupCreated {
    pub id: String,
    pub recovery_key: String,
}

fn key_dir(inner: &DaemonInner) -> PathBuf {
    inner.config.data_dir.join("backup")
}

fn s3_config(inner: &DaemonInner) -> Option<crate::links::s3::S3Config> {
    crate::links::load_settings(&inner.config.data_dir).ok().flatten().map(|s| s.s3)
}

fn job(inner: &DaemonInner, id: &str) -> Result<crate::database::BackupJobRow> {
    let id = id.trim();
    let found: Vec<_> = inner.db.backup_jobs()?.into_iter().filter(|j| j.id.starts_with(id)).collect();
    match (id.len() >= 4, found.len()) {
        (false, _) => api_bail!(InvalidInput, "Bitte mindestens 4 Zeichen der Sicherungs-ID angeben."),
        (_, 0) => api_bail!(NotFound, "Keine Sicherung {id}."),
        (_, 1) => Ok(found.into_iter().next().expect("eins")),
        _ => api_bail!(Ambiguous, "Sicherungs-ID {id} ist nicht eindeutig."),
    }
}

fn open_repo(inner: &DaemonInner, row: &crate::database::BackupJobRow) -> Result<Repo> {
    let target: BackupTarget = serde_json::from_str(&row.target)?;
    let storage = Storage::open(&target, &row.id, s3_config(inner))?;
    if !storage.available() {
        api_bail!(Unavailable, "Sicherungsziel ist nicht erreichbar (Laufwerk eingehängt?).");
    }
    let master = repo::load_local_key(&key_dir(inner), &row.id)?;
    Ok(Repo::with_master(&row.id, storage, &master))
}

fn check_sources(inner: &DaemonInner, sources: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for s in sources {
        let real = match std::fs::canonicalize(s) {
            Ok(p) if p.is_dir() => p,
            _ => api_bail!(InvalidInput, "{} ist kein Ordner.", s.display()),
        };
        if inner.protected_paths().iter().any(|p| real.starts_with(p)) {
            api_bail!(InvalidInput, "Die Snap-Daten selbst werden nicht gesichert.");
        }
        out.push(real);
    }
    Ok(out)
}

pub(crate) async fn create(
    inner: &Arc<DaemonInner>,
    target: BackupTarget,
    sources: Vec<PathBuf>,
    password: String,
) -> Result<BackupCreated> {
    let sources = check_sources(inner, &sources)?;
    if let BackupTarget::Dir { path } = &target {
        if sources.iter().any(|s| path.starts_with(s)) {
            api_bail!(InvalidInput, "Das Ziel darf nicht in einem gesicherten Ordner liegen.");
        }
    }
    let (repo, master, recovery) = Repo::init(&target, s3_config(inner), &password).await?;
    repo::save_local_key(&key_dir(inner), &repo.id, &master)?;
    inner.db.add_backup_job(&repo.id, &serde_json::to_string(&target)?, &serde_json::to_string(&sources)?)?;
    Ok(BackupCreated { id: repo.id, recovery_key: repo::format_recovery(&recovery) })
}

/// Verbindet eine bestehende Sicherung (z. B. auf einem neuen Gerät) – nur zum Wiederherstellen,
/// bis Quellordner hinzugefügt werden.
pub(crate) async fn connect(
    inner: &Arc<DaemonInner>,
    target: BackupTarget,
    repo_id: &str,
    secret: String,
) -> Result<String> {
    if repo_id.len() != 32 || !repo_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        api_bail!(InvalidInput, "Ungültige Sicherungs-ID (32 Hex-Zeichen, steht im Ordnernamen snap-backup-…).");
    }
    let storage = Storage::open(&target, repo_id, s3_config(inner))?;
    let (repo, master) = Repo::connect(storage, &secret).await?;
    repo::save_local_key(&key_dir(inner), &repo.id, &master)?;
    inner.db.add_backup_job(&repo.id, &serde_json::to_string(&target)?, "[]")?;
    Ok(repo.id)
}

pub(crate) fn list(inner: &DaemonInner) -> Result<Vec<BackupJob>> {
    let running = inner.backups.running.lock().unwrap_or_else(|e| e.into_inner()).clone();
    inner
        .db
        .backup_jobs()?
        .into_iter()
        .map(|r| {
            Ok(BackupJob {
                running: running.contains(&r.id),
                target: serde_json::from_str(&r.target)?,
                sources: serde_json::from_str(&r.sources)?,
                id: r.id,
                last_run: r.last_run,
                last_ok: r.last_ok,
                last_error: r.last_error,
                last_snapshot: r.last_snapshot,
            })
        })
        .collect()
}

pub(crate) fn set_sources(inner: &DaemonInner, id: &str, sources: Vec<PathBuf>) -> Result<()> {
    let row = job(inner, id)?;
    let sources = check_sources(inner, &sources)?;
    inner.db.set_backup_sources(&row.id, &serde_json::to_string(&sources)?)?;
    Ok(())
}

/// Führt eine Sicherung jetzt aus.
pub(crate) async fn run(inner: &Arc<DaemonInner>, id: &str) -> Result<SnapshotInfo> {
    let row = job(inner, id)?;
    let sources: Vec<PathBuf> = serde_json::from_str(&row.sources)?;
    if sources.is_empty() {
        api_bail!(InvalidInput, "Diese Sicherung hat keine Quellordner (backup sources …).");
    }
    if !inner.backups.running.lock().unwrap_or_else(|e| e.into_inner()).insert(row.id.clone()) {
        api_bail!(AlreadyExists, "Diese Sicherung läuft bereits.");
    }
    let mut pruned = (0, 0);
    let result = async {
        let repo = open_repo(inner, &row)?;
        let mut known = engine::Known::load(key_dir(inner).join(format!("{}.known", row.id)));
        let progress = engine::BackupProgress::default();
        let res = engine::backup(&repo, &mut known, &sources, &progress).await;
        // Auch nach Fehlern merken, was hochgeladen wurde (sonst blieben Reste unentdeckt).
        let _ = known.flush();
        let snap = res?;
        // Alte Stände nach den Regeln ausdünnen (Fehler hier lassen die Sicherung gültig).
        match engine::forget_and_prune(&repo, &mut known, inner.settings.backup_retention()).await {
            Ok(p) => pruned = p,
            Err(e) => tracing::warn!("Ausdünnen der Sicherung {} fehlgeschlagen: {e:#}", &row.id[..8]),
        }
        anyhow::Ok(snap)
    }
    .await;
    inner.backups.running.lock().unwrap_or_else(|e| e.into_inner()).remove(&row.id);
    match &result {
        Ok(s) => inner.db.finish_backup_run(&row.id, true, None, Some(&s.id))?,
        Err(e) => inner.db.finish_backup_run(&row.id, false, Some(&format!("{e:#}")), None)?,
    }
    inner.db.events().emit(crate::events::Event::BackupFinished {
        id: row.id.clone(),
        ok: result.is_ok(),
        message: match &result {
            Ok(s) => format!(
                "{} Dateien, {} Bytes neu{}",
                s.files,
                s.new_bytes,
                if pruned.0 > 0 { format!(", {} alte Stände entfernt", pruned.0) } else { String::new() }
            ),
            Err(e) => format!("{e:#}"),
        },
    });
    result
}

pub(crate) async fn snapshots(inner: &Arc<DaemonInner>, id: &str) -> Result<Vec<SnapshotInfo>> {
    let row = job(inner, id)?;
    Ok(open_repo(inner, &row)?.load_catalog().await?.snapshots)
}

pub(crate) async fn browse(
    inner: &Arc<DaemonInner>,
    id: &str,
    snap: &str,
    path: &Path,
) -> Result<Vec<engine::BrowseEntry>> {
    let row = job(inner, id)?;
    let repo = open_repo(inner, &row)?;
    let cat = repo.load_catalog().await?;
    engine::browse(&repo, engine::snapshot(&cat, snap)?, path).await
}

pub(crate) async fn restore(
    inner: &Arc<DaemonInner>,
    id: &str,
    snap: &str,
    path: &Path,
    dest: &Path,
) -> Result<(PathBuf, u64)> {
    let row = job(inner, id)?;
    let repo = open_repo(inner, &row)?;
    let cat = repo.load_catalog().await?;
    let out = engine::restore(&repo, engine::snapshot(&cat, snap)?, path, dest).await?;
    inner.refresh_index(vec![dest.to_path_buf()]).await;
    Ok(out)
}

pub(crate) async fn check(inner: &Arc<DaemonInner>, id: &str, sample: usize) -> Result<(usize, usize)> {
    let row = job(inner, id)?;
    engine::check(&open_repo(inner, &row)?, sample).await
}

/// Entfernt die Sicherung von diesem Gerät (Daten am Ziel bleiben erhalten).
pub(crate) fn remove(inner: &DaemonInner, id: &str) -> Result<()> {
    let row = job(inner, id)?;
    inner.db.remove_backup_job(&row.id)?;
    let _ = std::fs::remove_file(key_dir(inner).join(format!("{}.key", row.id)));
    let _ = std::fs::remove_file(key_dir(inner).join(format!("{}.known", row.id)));
    let _ = std::fs::remove_file(key_dir(inner).join(format!("{}.last", row.id)));
    Ok(())
}

/// Hintergrund: Sicherungen nach Zeitplan (Einstellung `backup_interval_minutes`).
/// Nicht erreichbare Ziele (Laufwerk abgezogen) werden still übersprungen und später nachgeholt.
pub(crate) async fn run_scheduler(inner: Arc<DaemonInner>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let Some(interval) = inner.settings.backup_interval() else { continue };
        let now = crate::database::now_unix();
        for row in inner.db.backup_jobs().unwrap_or_default() {
            let has_sources = serde_json::from_str::<Vec<PathBuf>>(&row.sources).is_ok_and(|s| !s.is_empty());
            let due = row.last_run.is_none_or(|t| now - t >= interval);
            if !has_sources || !due {
                continue;
            }
            let available = serde_json::from_str::<BackupTarget>(&row.target)
                .ok()
                .and_then(|t| Storage::open(&t, &row.id, s3_config(&inner)).ok())
                .is_some_and(|s| s.available());
            if !available {
                continue;
            }
            if let Err(e) = run(&inner, &row.id).await {
                tracing::warn!("Geplante Sicherung {} fehlgeschlagen: {e:#}", &row.id[..8]);
            }
        }
    }
}
