//! Foto-Zeitachse: Bilder und Videos aller indexierten Orte nach Aufnahmezeit.
//!
//! - Die Aufnahmezeit kommt aus EXIF (`DateTimeOriginal`), ersatzweise aus der mtime. Sie wird als
//!   **lokale Uhrzeit ohne Zeitzone** gespeichert (Sekunden seit 1970, „als wäre es UTC“), wie
//!   Kameras sie schreiben – so stimmen Tages- und Monatsgrenzen mit dem Foto überein.
//! - Die Tabelle `media_meta` im lokalen Index wird im Hintergrund gefüllt und ist gültig,
//!   solange Größe und mtime der Datei passen.
//! - Abfragen sind seitenweise (Cursor) und laufen über einen Index auf der Aufnahmezeit.

use crate::api_bail;
use crate::index::LocalIndex;
use anyhow::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Component, Path};

/// Endungen, die in der Zeitachse erscheinen.
pub const EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "heic", "heif", "webp", "gif", "tif", "tiff", "dng", "cr2", "cr3", "nef", "arw", "mp4",
    "mov", "m4v", "3gp", "mkv", "webm",
];
const VIDEO: &[&str] = &["mp4", "mov", "m4v", "3gp", "mkv", "webm"];

/// Höchstzahl Einträge pro Seite.
pub const MAX_PAGE: usize = 1000;

fn ext_list() -> String {
    EXTENSIONS.iter().map(|e| format!("'{e}'")).collect::<Vec<_>>().join(",")
}

/// Bedingung für Kandidaten in `nodes` (gleicher Wortlaut wie der Teilindex, damit SQLite ihn nutzt).
fn media_where() -> String {
    format!("kind = 0 AND ext IN ({})", ext_list())
}

/// Schema (wird beim Öffnen des Index angelegt).
pub(crate) fn schema() -> String {
    format!(
        "{SCHEMA}
        -- Teilindex: nur Bilder/Videos (Abgleich der Zeitachse ohne Durchlauf aller Einträge).
        CREATE INDEX IF NOT EXISTS idx_nodes_media ON nodes(id) WHERE {};",
        media_where()
    )
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS media_meta (
        node_id INTEGER PRIMARY KEY,
        parent  INTEGER NOT NULL,
        size    INTEGER NOT NULL,
        mtime   INTEGER NOT NULL,
        taken   INTEGER NOT NULL,  -- lokale Uhrzeit ohne Zeitzone, Sekunden
        month   INTEGER NOT NULL,  -- Jahr * 12 + Monat (0–11)
        exif    INTEGER NOT NULL,  -- 1 = Zeit aus EXIF
        video   INTEGER NOT NULL,
        hidden  INTEGER NOT NULL,  -- liegt in einem versteckten Ordner
        width   INTEGER,
        height  INTEGER,
        camera  TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_media_taken ON media_meta(hidden, taken, node_id);
    CREATE INDEX IF NOT EXISTS idx_media_month ON media_meta(hidden, month);
    CREATE INDEX IF NOT EXISTS idx_media_parent ON media_meta(parent, hidden, month);
";

/// Ein Eintrag der Zeitachse.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TimelineItem {
    pub path: String,
    /// Aufnahmezeit (lokal, ohne Zeitzone), Sekunden.
    pub taken: i64,
    /// `true`, wenn die Zeit aus EXIF stammt (sonst Änderungszeit).
    pub exif: bool,
    pub video: bool,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub camera: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct TimelinePage {
    pub items: Vec<TimelineItem>,
    /// Cursor für die nächste (ältere) Seite.
    pub next: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MonthCount {
    pub year: i32,
    /// 1–12
    pub month: u32,
    pub count: u64,
}

/// EXIF-Datum (`YYYY-MM-DD hh:mm:ss` oder `YYYY:MM:DD hh:mm:ss`) → Sekunden. Nur 1900–2199.
pub fn parse_taken(s: &str) -> Option<i64> {
    let s = s.trim();
    let dt = ["%Y-%m-%d %H:%M:%S", "%Y:%m:%d %H:%M:%S"]
        .iter()
        .find_map(|f| chrono::NaiveDateTime::parse_from_str(s, f).ok())?;
    use chrono::Datelike;
    (1900..2200).contains(&dt.year()).then(|| dt.and_utc().timestamp())
}

/// Unix-Zeit → lokale Uhrzeit ohne Zeitzone (für die mtime als Ersatz).
fn local_naive(unix: i64) -> i64 {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(unix, 0) {
        chrono::LocalResult::Single(d) | chrono::LocalResult::Ambiguous(d, _) => d.naive_local().and_utc().timestamp(),
        chrono::LocalResult::None => unix,
    }
}

fn month_of(taken: i64) -> i64 {
    use chrono::Datelike;
    let d = chrono::DateTime::from_timestamp(taken, 0).unwrap_or_default();
    i64::from(d.year()) * 12 + i64::from(d.month0())
}

/// Cursor `<taken>.<node_id>` (beides Ganzzahlen).
pub fn parse_cursor(s: &str) -> Option<(i64, i64)> {
    let (a, b) = s.split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

pub fn format_cursor(taken: i64, id: i64) -> String {
    format!("{taken}.{id}")
}

/// Liegt `path` unterhalb von `root` in einem versteckten Ordner (oder ist selbst versteckt)?
fn is_hidden(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| matches!(c, Component::Normal(n) if n.as_encoded_bytes().first() == Some(&b'.')))
}

/// Ergänzt fehlende oder veraltete Einträge (blockierend). `stop` wird zwischen Stapeln
/// abgefragt. Gibt die Zahl der gelesenen Dateien zurück.
/// `cleanup`: zusätzlich verwaiste Einträge entfernen (Sicherheitsnetz; das Löschen im Index
/// nimmt sie normalerweise schon mit).
pub fn refresh(index: &LocalIndex, cleanup: bool, stop: impl Fn() -> bool) -> Result<u64> {
    if cleanup {
        index.writer().execute("DELETE FROM media_meta WHERE node_id NOT IN (SELECT id FROM nodes)", [])?;
    }
    let (todo, roots) = {
        let conn = index.reader();
        let mut stmt = conn.prepare(&format!(
            "SELECT n.id, n.parent, n.size, n.mtime, n.root_id, n.ext
             FROM (SELECT * FROM nodes INDEXED BY idx_nodes_media WHERE {}) n
             LEFT JOIN media_meta m ON m.node_id = n.id
             WHERE n.parent IS NOT NULL AND (m.node_id IS NULL OR m.size != n.size OR m.mtime != n.mtime)",
            media_where()
        ))?;
        let todo: Vec<(i64, i64, i64, i64, i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let roots: HashMap<i64, String> = conn
            .prepare("SELECT id, path FROM roots")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        (todo, roots)
    };
    let mut done = 0u64;
    let mut cache = HashMap::new();
    for batch in todo.chunks(100) {
        if stop() {
            break;
        }
        let mut rows = Vec::with_capacity(batch.len());
        for (id, parent, size, mtime, root_id, ext) in batch {
            let path = {
                let conn = index.reader();
                match LocalIndex::path_of(&conn, *id, &mut cache) {
                    Ok(p) => p,
                    Err(_) => continue,
                }
            };
            let root = roots.get(root_id).map(String::as_str).unwrap_or("/");
            let hidden = is_hidden(Path::new(&path), Path::new(root));
            // Metadaten nur lesen, wenn sichtbar (versteckte Ordner sind oft Caches mit vielen Bildern).
            let info = if hidden { None } else { crate::media::metadata(Path::new(&path)).ok() };
            let exif_time = info.as_ref().and_then(|i| i.taken_at.as_deref()).and_then(parse_taken);
            let taken = exif_time.unwrap_or_else(|| local_naive(*mtime));
            let camera = info.as_ref().and_then(|i| i.camera.clone()).filter(|c| !c.is_empty());
            let (w, h) = info.as_ref().map(|i| (i.width, i.height)).unwrap_or_default();
            rows.push((
                *id,
                *parent,
                *size,
                *mtime,
                taken,
                exif_time.is_some(),
                VIDEO.contains(&ext.as_str()),
                hidden,
                w,
                h,
                camera,
            ));
            done += 1;
        }
        let mut w = index.writer();
        let tx = w.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT OR REPLACE INTO media_meta
                 (node_id, parent, size, mtime, taken, month, exif, video, hidden, width, height, camera)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12
                 WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1 AND size = ?3 AND mtime = ?4)",
            )?;
            for (id, parent, size, mtime, taken, exif, video, hidden, w, h, camera) in rows {
                ins.execute(params![
                    id,
                    parent,
                    size,
                    mtime,
                    taken,
                    month_of(taken),
                    exif,
                    video,
                    hidden,
                    w,
                    h,
                    camera
                ])?;
            }
        }
        tx.commit()?;
    }
    Ok(done)
}

/// Hält die Zeitachse aktuell: nach jedem Abgleich des Index (gebündelt) und alle 10 Minuten.
pub(crate) async fn run_refresher(inner: std::sync::Arc<crate::daemon::DaemonInner>) {
    let Some(svc) = inner.index.clone() else { return };
    loop {
        let timer = tokio::select! {
            _ = svc.media_dirty.notified() => false,
            _ = tokio::time::sleep(std::time::Duration::from_secs(600)) => true,
        };
        // Mehrere Änderungen kurz hintereinander zusammenfassen.
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let s = svc.clone();
        let res = tokio::task::spawn_blocking(move || {
            let n = refresh(&s.index, timer, || s.stopping.load(std::sync::atomic::Ordering::Relaxed))?;
            // Monatsübersicht vorberechnen: Die Abfrage der App trifft dann den Zwischenspeicher.
            let m = months(&s.index, None)?;
            *s.months_cache.lock().unwrap_or_else(|e| e.into_inner()) = Some(m);
            anyhow::Ok(n)
        })
        .await;
        match res {
            Ok(Ok(n)) if n > 0 => tracing::info!("Foto-Zeitachse: {n} Dateien ergänzt"),
            Ok(Err(e)) => tracing::warn!("Foto-Zeitachse nicht aktualisierbar: {e:#}"),
            _ => {}
        }
    }
}

/// Ab so vielen Unterordnern wird über den Zeitindex gesucht (dichter Bereich), darunter über
/// den Ordnerindex mit anschließender Sortierung (kleiner Bereich).
const SMALL_SCOPE_DIRS: usize = 1000;

/// Eingrenzung auf einen Ordner.
enum Scope {
    All,
    /// Wenige Ordner: Liste der Ordner-IDs.
    Dirs(Vec<i64>),
    /// Viele Ordner: Bedingung über eine rekursive Abfrage ab diesem Ordner.
    Tree(i64),
}

fn scope_of(index: &LocalIndex, scope: Option<&Path>) -> Result<Scope> {
    let Some(p) = scope else { return Ok(Scope::All) };
    let Some(top) = index.find_dir(p)? else {
        api_bail!(NotFound, "{} ist kein indexierter Ordner.", p.display());
    };
    let conn = index.reader();
    let mut stmt = conn.prepare_cached(
        "WITH RECURSIVE dirs(id) AS (
            SELECT ?1 UNION ALL SELECT n.id FROM nodes n JOIN dirs ON n.parent = dirs.id WHERE n.kind = 1
         ) SELECT id FROM dirs LIMIT ?2",
    )?;
    let ids: Vec<i64> =
        stmt.query_map(params![top, SMALL_SCOPE_DIRS as i64 + 1], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(if ids.len() > SMALL_SCOPE_DIRS { Scope::Tree(top) } else { Scope::Dirs(ids) })
}

/// SQL-Bedingung für die Eingrenzung (nur Ganzzahlen aus der eigenen Datenbank).
/// Gibt (Vorspann, Tabellenhinweis, Bedingung) zurück.
fn scope_sql(scope: &Scope) -> (String, &'static str, String) {
    match scope {
        Scope::All => (String::new(), "", String::new()),
        // Kleiner Bereich: über den Ordnerindex lesen und sortieren (sonst wählt der Planer den
        // Zeitindex und prüft jede Zeile).
        Scope::Dirs(ids) => {
            let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            (String::new(), " INDEXED BY idx_media_parent", format!(" AND m.parent IN ({list})"))
        }
        Scope::Tree(top) => (
            format!(
                "WITH RECURSIVE dirs(id) AS (SELECT {top} UNION ALL
                 SELECT n.id FROM nodes n JOIN dirs ON n.parent = dirs.id WHERE n.kind = 1) "
            ),
            "",
            " AND m.parent IN dirs".into(),
        ),
    }
}

/// Eine Seite der Zeitachse, neueste zuerst. `before`: Cursor aus der vorherigen Seite.
pub fn timeline(index: &LocalIndex, scope: Option<&Path>, before: Option<&str>, limit: usize) -> Result<TimelinePage> {
    let limit = limit.clamp(1, MAX_PAGE);
    let (bt, bid) = match before {
        Some(c) => match parse_cursor(c) {
            Some(v) => v,
            None => api_bail!(InvalidInput, "Ungültiger Cursor."),
        },
        None => (i64::MAX, i64::MAX),
    };
    let scope = scope_of(index, scope)?;
    let (with, hint, cond) = scope_sql(&scope);
    let conn = index.reader();
    let sql = format!(
        "{with}SELECT m.node_id, m.taken, m.exif, m.video, m.size, m.width, m.height, m.camera
         FROM media_meta m{hint} WHERE m.hidden = 0 AND (m.taken, m.node_id) < (?1, ?2){cond}
         ORDER BY m.taken DESC, m.node_id DESC LIMIT ?3"
    );
    // Nicht im Anweisungs-Cache ablegen (Ordnerlisten ändern sich ständig).
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(i64, TimelineItem)> = stmt
        .query_map(params![bt, bid, limit as i64], |r| {
            Ok((
                r.get(0)?,
                TimelineItem {
                    path: String::new(),
                    taken: r.get(1)?,
                    exif: r.get(2)?,
                    video: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    width: r.get(5)?,
                    height: r.get(6)?,
                    camera: r.get(7)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut page = TimelinePage::default();
    if rows.len() == limit {
        page.next = rows.last().map(|(id, item)| format_cursor(item.taken, *id));
    }
    let mut cache = HashMap::new();
    for (id, mut item) in rows {
        let Ok(path) = LocalIndex::path_of(&conn, id, &mut cache) else { continue };
        item.path = path;
        page.items.push(item);
    }
    Ok(page)
}

/// Anzahl je Monat (neueste zuerst) – für Sprungmarken und Bildlaufleiste.
pub fn months(index: &LocalIndex, scope: Option<&Path>) -> Result<Vec<MonthCount>> {
    let scope = scope_of(index, scope)?;
    let (with, hint, cond) = scope_sql(&scope);
    let conn = index.reader();
    let sql = format!(
        "{with}SELECT m.month, COUNT(*) FROM media_meta m{hint} WHERE m.hidden = 0{cond}
         GROUP BY m.month ORDER BY m.month DESC"
    );
    // Nicht im Anweisungs-Cache ablegen (Ordnerlisten ändern sich ständig).
    let mut stmt = conn.prepare(&sql)?;
    let out = stmt
        .query_map([], |r| {
            let m: i64 = r.get(0)?;
            Ok(MonthCount {
                year: m.div_euclid(12) as i32,
                month: m.rem_euclid(12) as u32 + 1,
                count: r.get::<_, i64>(1)? as u64,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_cursors() {
        assert_eq!(parse_taken("2024-07-14 18:30:00"), Some(1_720_981_800));
        assert_eq!(parse_taken("2024:07:14 18:30:00"), Some(1_720_981_800));
        for bad in ["", "0000:00:00 00:00:00", "2024-13-01 00:00:00", "1800-01-01 00:00:00", "gestern", "2024-07-14"] {
            assert_eq!(parse_taken(bad), None, "{bad}");
        }
        assert_eq!(parse_cursor(&format_cursor(-5, 42)), Some((-5, 42)));
        for bad in ["", ".", "1.", "a.b", "1.2.3", "99999999999999999999.1"] {
            assert_eq!(parse_cursor(bad), None, "{bad}");
        }
        assert_eq!(month_of(1_720_981_800), 2024 * 12 + 6);
        assert!(is_hidden(Path::new("/h/Bilder/.thumbnails/a.jpg"), Path::new("/h/Bilder")));
        assert!(!is_hidden(Path::new("/h/.lokal/Bilder/a.jpg"), Path::new("/h/.lokal/Bilder")));
    }

    #[test]
    fn timeline_pages_months_and_updates() {
        use crate::media::tests::{jpeg, jpeg_with_exif};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("home");
        std::fs::create_dir_all(root.join("Fotos/Urlaub")).unwrap();
        std::fs::create_dir_all(root.join("Fotos/.thumbnails")).unwrap();
        std::fs::write(root.join("Fotos/Urlaub/exif.jpg"), jpeg_with_exif(40, 20)).unwrap();
        std::fs::write(root.join("Fotos/.thumbnails/cache.jpg"), jpeg(8, 8)).unwrap();
        std::fs::write(root.join("notiz.txt"), b"keine Zeitachse").unwrap();
        // 5 Bilder ohne EXIF mit bekannten Dateizeiten (1.–5. März 2023)
        for i in 0..5i64 {
            let p = root.join(format!("Fotos/ohne{i}.png"));
            std::fs::write(&p, jpeg(4, 4)).unwrap();
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs((1_677_628_800 + i * 86_400) as u64);
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(t).unwrap();
        }
        let idx = LocalIndex::open(&dir.path().join("index.db"), vec![]).unwrap();
        let id = idx.add_root(&root).unwrap();
        idx.rebuild_root(id, &root).unwrap();
        assert_eq!(refresh(&idx, false, || false).unwrap(), 7);
        assert_eq!(refresh(&idx, false, || false).unwrap(), 0, "nichts Neues");

        // Seitenweise, neueste zuerst; versteckte Ordner und andere Dateien fehlen
        let p1 = timeline(&idx, None, None, 4).unwrap();
        assert_eq!(p1.items.len(), 4);
        assert!(p1.items[0].path.ends_with("Urlaub/exif.jpg"));
        assert!(p1.items[0].exif && !p1.items[0].video);
        assert_eq!(p1.items[0].taken, parse_taken("2024-07-14 18:30:00").unwrap());
        assert_eq!(p1.items[0].camera.as_deref(), Some("Fairphone FP5"));
        assert_eq!((p1.items[0].width, p1.items[0].height), (Some(40), Some(20)));
        assert!(!p1.items[1].exif);
        let p2 = timeline(&idx, None, p1.next.as_deref(), 4).unwrap();
        assert_eq!(p2.items.len(), 2);
        assert!(p2.next.is_none());
        let all: Vec<&str> = p1.items.iter().chain(&p2.items).map(|i| i.path.as_str()).collect();
        assert!(all.iter().all(|p| !p.contains(".thumbnails") && !p.ends_with(".txt")));
        assert!(all.windows(2).all(|w| w[0] != w[1]));
        assert!(p1.items.iter().chain(&p2.items).collect::<Vec<_>>().windows(2).all(|w| w[0].taken >= w[1].taken));

        // Eingeschränkt auf einen Ordner
        let scoped = timeline(&idx, Some(&root.join("Fotos/Urlaub")), None, 50).unwrap();
        assert_eq!(scoped.items.len(), 1);
        assert!(timeline(&idx, Some(&dir.path().join("fremd")), None, 50).is_err());
        assert!(timeline(&idx, None, Some("kaputt"), 50).is_err());

        // Monate
        let m = months(&idx, None).unwrap();
        assert_eq!(m[0], MonthCount { year: 2024, month: 7, count: 1 });
        assert_eq!(m.iter().map(|m| m.count).sum::<u64>(), 6);

        // Datei geändert → neu gelesen; gelöscht → verschwindet
        std::fs::write(root.join("Fotos/ohne0.png"), jpeg(6, 6)).unwrap();
        std::fs::remove_file(root.join("Fotos/Urlaub/exif.jpg")).unwrap();
        // (wie ein Watcher-Ereignis: der betroffene Ordner wird eingelesen)
        idx.refresh(id, &root, &root.join("Fotos"), true).unwrap();
        assert_eq!(refresh(&idx, false, || false).unwrap(), 1);
        let p = timeline(&idx, None, None, 50).unwrap();
        assert_eq!(p.items.len(), 5);
        assert!(p.items[0].path.ends_with("ohne0.png"), "neue Dateizeit = jetzt");
    }
}
