//! Zwei-Wege-Sync für Pins: lokale Änderungen am Spiegel gehen zum Ursprung zurück.
//!
//! Regeln (Entscheidungen des Nutzers vom 2026-11-27):
//! - **Geändert:** die lokale Fassung ersetzt die des Ursprungs (dessen alte Fassung landet dort
//!   im Papierkorb). Haben sich beide geändert, gewinnt die neuere; die ältere landet im
//!   Papierkorb des jeweiligen Geräts.
//! - **Neu:** neue lokale Dateien werden zum Ursprung hochgeladen (nie überschreibend).
//! - **Gelöscht:** lokal gelöscht → beim Ursprung in den Papierkorb. Beim Ursprung gelöscht →
//!   hier in den Papierkorb. Viele Löschungen auf einmal ([`MASS_DELETE`]) werden angehalten, bis
//!   der Nutzer bestätigt ([`Event::SyncHeld`]); „Behalten“ stellt die Dateien beim Ursprung
//!   wieder her.
//!
//! Sicherheit: Alles läuft über die Fernoperationen (`snap/ops/1`) und damit über die
//! Rechteprüfung des Ursprungs (`files.write`, `files.delete`, ordnergebunden), dessen
//! Massenschutz und Audit-Log. Pfade stammen aus dem eigenen Spiegel und werden wie jede
//! Fernoperation geprüft; versteckte und temporäre Dateien werden nie gesendet.

use crate::api::error::{code_of, ErrorCode};
use crate::daemon::DaemonInner;
use crate::events::Event;
use crate::fileops::ops::Conflict;
use crate::remote_ops::RemoteOp;
use anyhow::{Context, Result};
use iroh::EndpointId;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ab so vielen Löschungen durch den Ursprung in einem Abgleich wird angehalten und gefragt.
pub const MASS_DELETE: usize = 20;
/// So lange wartet eine gesendete Änderung darauf, dass der Ursprung sie meldet.
const AWAIT_MAX: Duration = Duration::from_secs(15 * 60);
/// Gerade erst geschriebene Dateien (z. B. noch im Editor) abwarten.
const SETTLE: Duration = Duration::from_secs(3);

/// Eine auszuführende Änderung beim Ursprung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Datei hochladen; `replace`: vorhandene Fassung beim Ursprung ersetzen (alte in dessen
    /// Papierkorb), sonst freien Namen wählen.
    /// `expect`: Hash, den der Ursprung zuletzt meldete ("" = keine Datei) – vor dem Senden wird
    /// geprüft, dass er inzwischen nichts Neueres hat.
    Push { origin: String, share: String, rel: String, path: PathBuf, replace: bool, expect: String },
    /// Datei beim Ursprung in den Papierkorb legen.
    Delete { origin: String, share: String, rel: String, path: String, expect: String },
}

#[derive(Debug, Clone)]
struct Awaiting {
    /// Hash, den der Ursprung vor unserer Änderung meldete ("" = unbekannt/neu).
    old_hash: String,
    /// Gesendeter Inhalt (`None` = läuft noch, `Some("")` = Löschung).
    new_hash: Option<String>,
    since: Instant,
}

/// Zustand des Zwei-Wege-Syncs (nur im Speicher).
#[derive(Default)]
pub struct SyncState {
    awaiting: Mutex<HashMap<String, Awaiting>>,
    /// Pins, deren angehaltene Löschungen der Nutzer einmalig erlaubt hat.
    allow_deletes: Mutex<HashSet<i64>>,
    /// Angehaltene Löschungen je Pin (für die Anzeige).
    held: Mutex<HashMap<i64, usize>>,
}

/// Was der Abgleich mit einer gespiegelten Datei tun soll.
#[derive(Debug, PartialEq, Eq)]
pub enum Wait {
    /// Eine eigene Änderung ist unterwegs: nichts tun.
    Pending,
    /// Der Ursprung meldet unsere Fassung: als „ok“ übernehmen.
    Adopt,
    /// Normal weiter.
    No,
}

impl SyncState {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Awaiting>> {
        self.awaiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ist für `path` eine eigene Änderung unterwegs, und meldet der Ursprung schon die neue Fassung?
    pub fn check(&self, path: &str, origin_hash: &str) -> Wait {
        let mut m = self.lock();
        let Some(a) = m.get(path) else { return Wait::No };
        if a.since.elapsed() > AWAIT_MAX {
            m.remove(path);
            return Wait::No;
        }
        match &a.new_hash {
            Some(h) if !h.is_empty() && h == origin_hash => {
                m.remove(path);
                Wait::Adopt
            }
            None => Wait::Pending,
            // Ursprung meldet noch den alten Stand (oder bei Löschung noch die Datei)
            Some(_) if origin_hash == a.old_hash || a.old_hash.is_empty() => Wait::Pending,
            // Ursprung hat inzwischen etwas anderes: normal weiter (Neuestes gewinnt)
            Some(_) => {
                m.remove(path);
                Wait::No
            }
        }
    }

    pub fn is_pending(&self, path: &str) -> bool {
        self.lock().get(path).is_some_and(|a| a.since.elapsed() <= AWAIT_MAX)
    }

    fn start(&self, path: &str, old_hash: &str) {
        self.lock()
            .insert(path.to_string(), Awaiting { old_hash: old_hash.into(), new_hash: None, since: Instant::now() });
    }

    fn sent(&self, path: &str, hash: &str) {
        if let Some(a) = self.lock().get_mut(path) {
            a.new_hash = Some(hash.to_string());
            a.since = Instant::now();
        }
    }

    fn clear(&self, path: &str) {
        self.lock().remove(path);
    }

    /// Eingereihte, aber nicht gesendete Änderung vergessen.
    pub(crate) fn forget(&self, path: &str) {
        self.clear(path);
    }

    /// Darf ein Abgleich `count` Löschungen des Ursprungs ausführen? Sonst angehalten.
    pub fn may_delete(&self, pin_id: i64, count: usize) -> bool {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if count <= MASS_DELETE || self.allow_deletes.lock().unwrap_or_else(|e| e.into_inner()).remove(&pin_id) {
            held.remove(&pin_id);
            return true;
        }
        held.insert(pin_id, count);
        false
    }

    /// Angehaltene Löschungen eines Pins (beider Richtungen; lokale unter `-pin_id`).
    pub fn held(&self, pin_id: i64) -> usize {
        let h = self.held.lock().unwrap_or_else(|e| e.into_inner());
        h.get(&pin_id).copied().unwrap_or(0) + h.get(&-pin_id).copied().unwrap_or(0)
    }

    /// Angehaltene Löschungen erlauben (einmalig, beide Richtungen).
    pub fn allow(&self, pin_id: i64) {
        let mut a = self.allow_deletes.lock().unwrap_or_else(|e| e.into_inner());
        a.insert(pin_id);
        a.insert(-pin_id);
    }

    /// Angehaltene Löschungen verwerfen.
    pub fn unhold(&self, pin_id: i64) {
        let mut h = self.held.lock().unwrap_or_else(|e| e.into_inner());
        h.remove(&pin_id);
        h.remove(&-pin_id);
    }
}

/// Wird `path` gerade noch geschrieben?
pub(crate) fn settling(meta: &std::fs::Metadata) -> bool {
    meta.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age < SETTLE)
}

/// Inhalt einer lokalen Datei als BLAKE3-Hash (wie im Katalog).
pub(crate) fn file_hash(path: &Path) -> Option<String> {
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = blake3::Hasher::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(iroh_blobs::Hash::from(h.finalize()).to_string())
}

/// Neue lokale Dateien im Spiegel eines Pins (noch nie gesehen): relative Pfade zur Freigabe.
/// Zweiter Wert: Es gab Dateien, die gerade noch geschrieben werden (später erneut prüfen).
pub(crate) fn untracked(share_root: &Path, prefix: &str, known: &HashSet<String>) -> (Vec<(String, PathBuf)>, bool) {
    let start = if prefix.is_empty() { share_root.to_path_buf() } else { share_root.join(prefix) };
    let mut out = Vec::new();
    let mut busy = false;
    if std::fs::symlink_metadata(&start).map(|m| !m.is_dir()).unwrap_or(true) {
        return (out, busy);
    }
    for e in walkdir::WalkDir::new(&start)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .flatten()
    {
        if !e.file_type().is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy();
        if crate::shares::is_ignored(&name) {
            continue;
        }
        let path_str = e.path().to_string_lossy();
        if known.contains(path_str.as_ref()) {
            continue;
        }
        if e.metadata().map(|m| settling(&m)).unwrap_or(true) {
            busy = true;
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(share_root) else { continue };
        let Some(rel) = rel.to_str() else { continue };
        if crate::ledger::validate_rel_path(rel).is_ok() {
            out.push((rel.to_string(), e.path().to_path_buf()));
        }
        if out.len() >= 1000 {
            break; // Rest beim nächsten Abgleich
        }
    }
    (out, busy)
}

/// Merkt eine Änderung als unterwegs und reiht sie ein.
pub(crate) fn queue(inner: &DaemonInner, actions: &mut Vec<Action>, action: Action, old_hash: &str) {
    let key = match &action {
        Action::Push { path, .. } => path.to_string_lossy().into_owned(),
        Action::Delete { path, .. } => path.clone(),
    };
    inner.sync.start(&key, old_hash);
    actions.push(action);
}

/// Angehaltene Löschungen melden.
pub(crate) fn report_held(inner: &DaemonInner, pin: &crate::database::PinRow, count: usize) {
    tracing::info!("Zwei-Wege-Sync '{}': {count} Löschungen angehalten (Bestätigung nötig)", pin.share);
    inner.db.events().emit(Event::SyncHeld { pin_id: pin.id, share: pin.share.clone(), deletes: count });
}

/// Führt die Änderungen nacheinander beim Ursprung aus (im Hintergrund). Vorher wird der Katalog
/// des Ursprungs aufgefrischt: Hat er inzwischen eine neuere Fassung, wird nicht gesendet, sondern
/// neu abgeglichen („Neuestes gewinnt“ mit aktuellem Stand – etwa nach einer Offline-Phase).
pub(crate) async fn run(inner: Arc<DaemonInner>, actions: Vec<Action>) {
    let _guard = inner.sync_push.lock().await;
    let mut by_origin: HashMap<String, Vec<Action>> = HashMap::new();
    for a in actions {
        let origin = match &a {
            Action::Push { origin, .. } | Action::Delete { origin, .. } => origin.clone(),
        };
        by_origin.entry(origin).or_default().push(a);
    }
    let timeout = Duration::from_secs(inner.config.connect_timeout_secs);
    let mut changed = false;
    let mut recheck = false;
    for (origin, actions) in by_origin {
        let fresh = crate::sync::sync_one(&inner.ledger, &inner.db, origin.clone(), String::new(), timeout).await;
        for action in actions {
            let (key, share, rel, expect) = match &action {
                Action::Push { path, share, rel, expect, .. } => {
                    (path.to_string_lossy().into_owned(), share, rel, expect)
                }
                Action::Delete { path, share, rel, expect, .. } => (path.clone(), share, rel, expect),
            };
            if !fresh.ok {
                inner.sync.clear(&key); // Ursprung nicht erreichbar: später erneut
                continue;
            }
            let now = inner.db.ledger_live_hash(&origin, share, rel).ok().flatten().unwrap_or_default();
            if &now != expect {
                inner.sync.clear(&key);
                recheck = true;
                continue;
            }
            match execute(&inner, &action).await {
                Ok(hash) => {
                    inner.sync.sent(&key, &hash);
                    if let Action::Delete { path, .. } = &action {
                        let _ = inner.db.pinned_remove(path);
                    }
                    changed = true;
                }
                Err(e) => {
                    let code = code_of(&e);
                    tracing::info!("Zwei-Wege-Sync: {key}: {e:#}");
                    inner.sync.clear(&key);
                    // Fehlende Rechte o. Ä.: nicht endlos wiederholen – als Konflikt sichtbar machen.
                    if matches!(code, ErrorCode::PermissionDenied | ErrorCode::InvalidInput | ErrorCode::Rejected) {
                        let _ = inner.db.pinned_mark_conflict(&key);
                    }
                }
            }
        }
    }
    if changed {
        // Ursprung um einen Abgleich bitten, damit unsere Änderungen schnell zurückkommen
        inner.sync_notifier.notify_one();
    }
    if recheck {
        inner.pin_notify.notify_one();
    }
}

/// Eine Änderung ausführen. Ergebnis: gesendeter Hash ("" bei Löschung).
async fn execute(inner: &Arc<DaemonInner>, action: &Action) -> Result<String> {
    match action {
        Action::Push { origin, share, rel, path, replace, .. } => {
            let meta = std::fs::symlink_metadata(path).context("Datei verschwunden")?;
            if !meta.is_file() {
                anyhow::bail!("keine normale Datei");
            }
            let (dir, name) = match rel.rsplit_once('/') {
                Some((d, n)) => (d.to_string(), n.to_string()),
                None => (String::new(), rel.clone()),
            };
            let node: EndpointId = origin.parse().context("Ungültige Node-ID")?;
            let hash = inner.network.import_for_upload(path).await?;
            inner.offers.offer(hash, node);
            let op = RemoteOp::Import {
                share: share.clone(),
                dest_dir: dir,
                name,
                hash: hash.to_string(),
                size: meta.len(),
                conflict: if *replace { Conflict::Replace } else { Conflict::KeepBoth },
            };
            let timeout = Duration::from_secs(120 + meta.len() / 1_000_000);
            let res = crate::remote_ops::send_op(inner, origin, op, timeout).await;
            inner.offers.withdraw(hash, node);
            let _ = inner.network.untag_upload(hash).await;
            let (_, reply) = res?;
            // Beim Ursprung angehalten (Massenschutz): wird dort bestätigt oder verfällt
            if reply.ok || reply.approval_id.is_some() {
                Ok(hash.to_string())
            } else {
                Err(crate::api::error::ApiError::new(reply.code.unwrap_or(ErrorCode::Internal), reply.message).into())
            }
        }
        Action::Delete { origin, share, rel, .. } => {
            let op = RemoteOp::Trash { share: share.clone(), paths: vec![rel.clone()] };
            let (_, reply) = crate::remote_ops::send_op(inner, origin, op, Duration::from_secs(60)).await?;
            if reply.ok || reply.approval_id.is_some() || reply.code == Some(ErrorCode::NotFound) {
                Ok(String::new())
            } else {
                Err(crate::api::error::ApiError::new(reply.code.unwrap_or(ErrorCode::Internal), reply.message).into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warten_auf_den_ursprung() {
        let s = SyncState::default();
        assert_eq!(s.check("/a", "h1"), Wait::No);
        s.start("/a", "h1");
        assert_eq!(s.check("/a", "h1"), Wait::Pending, "Senden läuft");
        s.sent("/a", "h2");
        assert_eq!(s.check("/a", "h1"), Wait::Pending, "Ursprung meldet noch den alten Stand");
        assert_eq!(s.check("/a", "h2"), Wait::Adopt, "unsere Fassung ist angekommen");
        assert_eq!(s.check("/a", "h2"), Wait::No);
        // Ursprung hat inzwischen etwas Drittes → normal weiter
        s.start("/b", "h1");
        s.sent("/b", "h2");
        assert_eq!(s.check("/b", "h3"), Wait::No);
    }

    #[test]
    fn massenloeschung_anhalten() {
        let s = SyncState::default();
        assert!(s.may_delete(1, MASS_DELETE));
        assert!(!s.may_delete(1, MASS_DELETE + 1));
        assert_eq!(s.held(1), MASS_DELETE + 1);
        s.allow(1);
        assert!(s.may_delete(1, 500), "einmal erlaubt");
        assert_eq!(s.held(1), 0);
        assert!(!s.may_delete(1, 500), "nur einmal");
    }

    #[test]
    fn neue_dateien_finden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Docs");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        for f in ["a.txt", "sub/b.txt", ".versteckt", ".git/config", "x.part"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        // Zeitstempel in die Vergangenheit (sonst „wird noch geschrieben“)
        let old = std::time::SystemTime::now() - Duration::from_secs(60);
        for f in ["a.txt", "sub/b.txt", "x.part"] {
            std::fs::File::options().write(true).open(root.join(f)).unwrap().set_modified(old).unwrap();
        }
        let known: HashSet<String> = [root.join("a.txt").to_string_lossy().into_owned()].into();
        let (found, busy) = untracked(&root, "", &known);
        let mut found: Vec<String> = found.into_iter().map(|(r, _)| r).collect();
        found.sort();
        assert_eq!(found, vec!["sub/b.txt"]);
        assert!(!busy);
        assert!(untracked(&root, "fehlt", &known).0.is_empty());
        std::fs::write(root.join("frisch.txt"), b"y").unwrap();
        assert!(untracked(&root, "", &known).1, "gerade geschrieben → später erneut");
    }
}
