//! Typisierte Befehle für die Oberfläche. Jeder Befehl baut genau eine Daemon-Anfrage; es gibt
//! bewusst keinen Befehl, der beliebige Anfragen durchreicht (siehe `bridge.rs`).

use crate::bridge::{is_risky_to_open, ui_path, Daemon, UiError, UiResult};
use crate::i18n::tr;
use serde_json::Value;
use space_core::fileops::jobs::FileOp;
use space_core::fileops::ops::Conflict;
use space_core::ipc::Request;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Nativer Bestätigungsdialog (außerhalb der Web-Ansicht). `true` = bestätigt.
async fn confirm(app: &AppHandle, title: String, text: String, ok: String) -> bool {
    let app = app.clone();
    let cancel = tr("Abbrechen", &[]);
    tokio::task::spawn_blocking(move || {
        app.dialog()
            .message(text)
            .title(title)
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(ok, cancel))
            .blocking_show()
    })
    .await
    .unwrap_or(false)
}

fn cancelled() -> UiError {
    UiError::new("Cancelled", "Abgebrochen.")
}

fn paths(list: Vec<String>) -> UiResult<Vec<std::path::PathBuf>> {
    if list.is_empty() || list.len() > 10_000 {
        return Err(UiError::new("InvalidInput", "Ungültige Auswahl."));
    }
    list.iter().map(|p| ui_path(p)).collect()
}

/// Kurze Liste der betroffenen Namen für Dialoge (Namen als reiner Text, gekürzt).
fn summary(list: &[std::path::PathBuf]) -> String {
    let mut s: Vec<String> = list
        .iter()
        .take(5)
        .map(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let n: String = n.chars().filter(|c| !c.is_control()).take(80).collect();
            format!("• {n}")
        })
        .collect();
    if list.len() > 5 {
        s.push(tr("… und {n} weitere", &[("n", &(list.len() - 5).to_string())]));
    }
    s.join("\n")
}

// ---------------------------------------------------------------- Lesen

#[tauri::command]
pub async fn info(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::Info).await
}

#[tauri::command]
pub async fn list_dir(d: State<'_, Daemon>, path: String, show_hidden: bool, offset: u32) -> UiResult<Value> {
    d.request(Request::ListDir { path: ui_path(&path)?, show_hidden, offset, limit: 1000 }).await
}

#[tauri::command]
pub async fn search(d: State<'_, Daemon>, query: String, include_hidden: bool) -> UiResult<Value> {
    if query.len() > 200 {
        return Err(UiError::new("InvalidInput", "Suchbegriff zu lang."));
    }
    let filter = space_core::index::SearchFilter { include_hidden, ..Default::default() };
    d.request(Request::Search { query, filter, limit: 500 }).await
}

#[tauri::command]
pub async fn metadata(d: State<'_, Daemon>, path: String) -> UiResult<Value> {
    d.request(Request::Metadata { path: ui_path(&path)? }).await
}

#[tauri::command]
pub async fn index_roots(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::IndexRoots).await
}

#[tauri::command]
pub async fn trash_list(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::TrashList).await
}

#[tauri::command]
pub async fn stream_url(d: State<'_, Daemon>, path: String) -> UiResult<Value> {
    d.request(Request::StreamUrl { target: ui_path(&path)?.to_string_lossy().into_owned() }).await
}

#[tauri::command]
pub async fn timeline(d: State<'_, Daemon>, path: Option<String>, before: Option<String>) -> UiResult<Value> {
    let path = path.map(|p| ui_path(&p)).transpose()?;
    d.request(Request::Timeline { path, before, limit: 300 }).await
}

#[tauri::command]
pub async fn timeline_months(d: State<'_, Daemon>, path: Option<String>) -> UiResult<Value> {
    let path = path.map(|p| ui_path(&p)).transpose()?;
    d.request(Request::TimelineMonths { path }).await
}

// ---------------------------------------------------------------- Ändern (rückgängig machbar)

async fn file_op(d: &Daemon, op: FileOp) -> UiResult<Value> {
    d.request(Request::FileOp { op, wait: false }).await
}

#[tauri::command]
pub async fn copy_items(d: State<'_, Daemon>, sources: Vec<String>, dest: String) -> UiResult<Value> {
    file_op(&d, FileOp::Copy { sources: paths(sources)?, dest: ui_path(&dest)?, conflict: Conflict::KeepBoth }).await
}

#[tauri::command]
pub async fn move_items(d: State<'_, Daemon>, sources: Vec<String>, dest: String) -> UiResult<Value> {
    file_op(&d, FileOp::Move { sources: paths(sources)?, dest: ui_path(&dest)?, conflict: Conflict::KeepBoth }).await
}

#[tauri::command]
pub async fn rename(d: State<'_, Daemon>, path: String, new_name: String) -> UiResult<Value> {
    file_op(&d, FileOp::Rename { path: ui_path(&path)?, new_name }).await
}

#[tauri::command]
pub async fn mkdir(d: State<'_, Daemon>, parent: String, name: String) -> UiResult<Value> {
    file_op(&d, FileOp::Mkdir { parent: ui_path(&parent)?, name }).await
}

#[tauri::command]
pub async fn create_file(d: State<'_, Daemon>, parent: String, name: String) -> UiResult<Value> {
    file_op(&d, FileOp::CreateFile { parent: ui_path(&parent)?, name }).await
}

/// In den Papierkorb – wiederherstellbar, daher ohne Rückfrage (wie in gängigen Dateimanagern).
#[tauri::command]
pub async fn trash(d: State<'_, Daemon>, items: Vec<String>) -> UiResult<Value> {
    file_op(&d, FileOp::Trash { paths: paths(items)? }).await
}

#[tauri::command]
pub async fn undo(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::Undo).await
}

#[tauri::command]
pub async fn trash_restore(d: State<'_, Daemon>, id: String) -> UiResult<Value> {
    d.request(Request::TrashRestore { id, keep_both: true }).await
}

// ---------------------------------------------------------------- Endgültig (native Bestätigung)

#[tauri::command]
pub async fn delete_permanently(app: AppHandle, d: State<'_, Daemon>, items: Vec<String>) -> UiResult<Value> {
    let list = paths(items)?;
    let question = if list.len() == 1 {
        tr("Dieses Element endgültig löschen? Das kann nicht rückgängig gemacht werden.", &[])
    } else {
        tr(
            "Diese {n} Elemente endgültig löschen? Das kann nicht rückgängig gemacht werden.",
            &[("n", &list.len().to_string())],
        )
    };
    let text = format!("{question}\n\n{}", summary(&list));
    if !confirm(&app, tr("Endgültig löschen", &[]), text, tr("Endgültig löschen", &[])).await {
        return Err(cancelled());
    }
    file_op(&d, FileOp::Delete { paths: list }).await
}

#[tauri::command]
pub async fn trash_empty(app: AppHandle, d: State<'_, Daemon>) -> UiResult<Value> {
    let text = tr("Alle Elemente im Papierkorb endgültig löschen?", &[]);
    if !confirm(&app, tr("Papierkorb leeren", &[]), text, tr("Leeren", &[])).await {
        return Err(cancelled());
    }
    d.request(Request::TrashEmpty).await
}

/// Öffnet eine Datei mit dem Standardprogramm. Ausführbare/riskante Dateien nur nach Rückfrage.
#[tauri::command]
pub async fn open_path(app: AppHandle, path: String) -> UiResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let p = ui_path(&path)?;
    let meta = std::fs::metadata(&p).map_err(|e| UiError::new("NotFound", e.to_string()))?;
    if !meta.is_file() {
        return Err(UiError::new("InvalidInput", "Nur Dateien lassen sich öffnen."));
    }
    if is_risky_to_open(&p, meta.permissions().mode()) {
        let name = summary(std::slice::from_ref(&p)).trim_start_matches("• ").to_string();
        let text = tr(
            "„{name}“ kann Programme ausführen. Nur öffnen, wenn die Datei aus einer vertrauenswürdigen Quelle stammt.",
            &[("name", &name)],
        );
        if !confirm(&app, tr("Datei öffnen?", &[]), text, tr("Trotzdem öffnen", &[])).await {
            return Err(cancelled());
        }
    }
    // Kein Shell-Aufruf: Pfad als einzelnes Argument.
    std::process::Command::new("xdg-open")
        .arg(&p)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| UiError::new("Internal", format!("Öffnen nicht möglich: {e}")))?;
    Ok(())
}

/// Startet den Dienst, falls er nicht läuft (systemd-Benutzerdienst, sonst `space-daemon start`).
#[tauri::command]
pub async fn start_daemon(d: State<'_, Daemon>) -> UiResult<()> {
    if d.connect().await.is_ok() {
        return Ok(());
    }
    let systemd = std::process::Command::new("systemctl")
        .args(["--user", "start", "snap.service"])
        .status()
        .is_ok_and(|s| s.success());
    if !systemd {
        // Neben der App installiert oder im PATH.
        let bin = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("space-daemon")))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| "space-daemon".into());
        std::process::Command::new(bin)
            .arg("start")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| UiError::new("DaemonDown", format!("Snap-Dienst nicht startbar: {e}")))?;
    }
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if d.connect().await.is_ok() {
            return Ok(());
        }
    }
    Err(UiError::new("DaemonDown", "Der Snap-Dienst startet nicht. Details: space-daemon start im Terminal."))
}

/// Orte für die Seitenleiste: persönlicher Ordner und Standardordner (nur vorhandene).
#[tauri::command]
pub async fn places() -> UiResult<Value> {
    let mut out = Vec::new();
    if let Some(u) = directories::UserDirs::new() {
        out.push(serde_json::json!({ "kind": "home", "name": "Persönlicher Ordner", "path": u.home_dir() }));
        let dirs = [
            ("desktop", "Schreibtisch", u.desktop_dir()),
            ("documents", "Dokumente", u.document_dir()),
            ("downloads", "Downloads", u.download_dir()),
            ("pictures", "Bilder", u.picture_dir()),
            ("music", "Musik", u.audio_dir()),
            ("videos", "Videos", u.video_dir()),
        ];
        for (kind, name, dir) in dirs {
            if let Some(d) = dir.filter(|d| d.is_dir() && Some(*d) != Some(u.home_dir())) {
                out.push(serde_json::json!({ "kind": kind, "name": name, "path": d }));
            }
        }
    }
    Ok(Value::Array(out))
}

// ================================================================ 3b: Geräte, Sicherungen, Einstellungen

fn short_text(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

#[tauri::command]
pub async fn devices(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::PairList).await
}

#[tauri::command]
pub async fn peers(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::Peers).await
}

/// Erzeugt einen Einladungscode (zeitlich begrenzt, einmalig) für ein neues Gerät.
#[tauri::command]
pub async fn pair_invite(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::PairInvite).await
}

/// Wartet, bis die Einladung angenommen wurde oder abläuft.
#[tauri::command]
pub async fn pair_invite_wait(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::PairInviteWait).await
}

/// Mit dem Code eines anderen Geräts koppeln. Der Nutzer hat den Code selbst eingegeben; die
/// Kopplung gibt dem anderen Gerät noch keine Rechte (die werden einzeln gewährt).
#[tauri::command]
pub async fn pair_join(d: State<'_, Daemon>, code: String) -> UiResult<Value> {
    if code.len() > 4096 {
        return Err(UiError::new("InvalidInput", "Code zu lang."));
    }
    d.request(Request::PairJoin { code: code.trim().to_string() }).await
}

#[tauri::command]
pub async fn pair_remove(app: AppHandle, d: State<'_, Daemon>, device: String, name: String) -> UiResult<Value> {
    let text = tr(
        "„{name}“ entkoppeln? Das Gerät verliert alle Rechte und sieht deine Freigaben nicht mehr.",
        &[("name", &short_text(&name, 80))],
    );
    if !confirm(&app, tr("Gerät entkoppeln", &[]), text, tr("Entkoppeln", &[])).await {
        return Err(cancelled());
    }
    d.request(Request::PairRemove { device }).await
}

/// Ein Recht gewähren – sicherheitsrelevant, daher immer mit nativer Bestätigung.
#[tauri::command]
pub async fn pair_grant(
    app: AppHandle,
    d: State<'_, Daemon>,
    device: String,
    name: String,
    capability: String,
    folder: Option<String>,
) -> UiResult<Value> {
    let folder = folder.map(|f| ui_path(&f)).transpose()?;
    let vars = [("name", short_text(&name, 80)), ("right", short_text(&capability, 40))];
    let mut vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let folder_text = folder.as_ref().map(|f| short_text(&f.to_string_lossy(), 120));
    let question = match &folder_text {
        Some(f) => {
            vars.push(("folder", f));
            tr("„{name}“ das Recht „{right}“ für {folder} geben?", &vars)
        }
        None => tr("„{name}“ das Recht „{right}“ geben?", &vars),
    };
    let text = format!("{question}\n\n{}", tr("Gib Rechte nur Geräten, die dir gehören.", &[]));
    if !confirm(&app, tr("Recht gewähren", &[]), text, tr("Gewähren", &[])).await {
        return Err(cancelled());
    }
    d.request(Request::PairGrant { device, capability, folder }).await
}

/// Ein Recht entziehen (schränkt nur ein – ohne Rückfrage).
#[tauri::command]
pub async fn pair_revoke(
    d: State<'_, Daemon>,
    device: String,
    capability: String,
    folder: Option<String>,
) -> UiResult<Value> {
    let folder = folder.map(|f| ui_path(&f)).transpose()?;
    d.request(Request::PairRevoke { device, capability, folder }).await
}

/// Ausstehende Fernoperationen anderer Geräte.
#[tauri::command]
pub async fn ops_pending(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::OpsPending).await
}

#[tauri::command]
pub async fn ops_approve(app: AppHandle, d: State<'_, Daemon>, id: i64, summary: String) -> UiResult<Value> {
    let text = format!("{}\n\n{}", tr("Diese Änderung eines anderen Geräts zulassen?", &[]), short_text(&summary, 300));
    if !confirm(&app, tr("Fernzugriff zulassen", &[]), text, tr("Zulassen", &[])).await {
        return Err(cancelled());
    }
    d.request(Request::OpsApprove { id }).await
}

#[tauri::command]
pub async fn ops_reject(d: State<'_, Daemon>, id: i64) -> UiResult<Value> {
    d.request(Request::OpsReject { id }).await
}

#[tauri::command]
pub async fn backups(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::BackupList).await
}

/// Neue Sicherung. Ziel: Ordner (z. B. externe Platte) oder `r2`.
#[tauri::command]
pub async fn backup_create(
    d: State<'_, Daemon>,
    target: Option<String>,
    sources: Vec<String>,
    password: String,
) -> UiResult<Value> {
    let target = match target {
        Some(p) => space_core::backup::repo::BackupTarget::Dir { path: ui_path(&p)? },
        None => space_core::backup::repo::BackupTarget::S3,
    };
    d.request(Request::BackupCreate { target, sources: paths(sources)?, password }).await
}

#[tauri::command]
pub async fn backup_run(d: State<'_, Daemon>, id: String) -> UiResult<Value> {
    d.request(Request::BackupRun { id }).await
}

#[tauri::command]
pub async fn backup_snapshots(d: State<'_, Daemon>, id: String) -> UiResult<Value> {
    d.request(Request::BackupSnapshots { id }).await
}

#[tauri::command]
pub async fn backup_browse(d: State<'_, Daemon>, id: String, snapshot: String, path: String) -> UiResult<Value> {
    d.request(Request::BackupBrowse { id, snapshot, path: ui_path(&path)? }).await
}

/// Wiederherstellen schreibt nur neue Dateien (nie überschreiben) – ohne Rückfrage.
#[tauri::command]
pub async fn backup_restore(
    d: State<'_, Daemon>,
    id: String,
    snapshot: String,
    path: String,
    dest: String,
) -> UiResult<Value> {
    d.request(Request::BackupRestore { id, snapshot, path: ui_path(&path)?, dest: ui_path(&dest)? }).await
}

#[tauri::command]
pub async fn backup_remove(app: AppHandle, d: State<'_, Daemon>, id: String) -> UiResult<Value> {
    let text = tr("Diese Sicherung aus Snap entfernen? Die gesicherten Daten am Ziel bleiben erhalten und lassen sich mit Passwort oder Wiederherstellungsschlüssel wieder verbinden.", &[]);
    if !confirm(&app, tr("Sicherung entfernen", &[]), text, tr("Entfernen", &[])).await {
        return Err(cancelled());
    }
    d.request(Request::BackupRemove { id }).await
}

#[tauri::command]
pub async fn settings(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::Settings).await
}

/// Grenze ändern (`None` = Standard). Der Daemon prüft Bereich und Einheit.
#[tauri::command]
pub async fn setting_set(d: State<'_, Daemon>, key: String, value: Option<i64>) -> UiResult<Value> {
    if key.len() > 64 {
        return Err(UiError::new("InvalidInput", "Ungültige Einstellung."));
    }
    d.request(Request::SettingSet { key, value }).await
}

/// Sprache der nativen Dialoge (gleiche Wahl wie die Oberfläche). Unbekannte Kürzel → Deutsch.
#[tauri::command]
pub fn set_language(code: String) {
    crate::i18n::set(&code);
}

// ---------------------------------------------------------------- Freigabelinks

#[tauri::command]
pub async fn links(d: State<'_, Daemon>) -> UiResult<Value> {
    d.request(Request::LinkList).await
}

/// Freigabelinks einrichten. Das Geheimnis geht nur an den Dienst und kommt nie zurück.
#[tauri::command]
pub async fn link_setup(
    d: State<'_, Daemon>,
    base_url: String,
    endpoint: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
) -> UiResult<Value> {
    if [&base_url, &endpoint, &bucket, &access_key_id, &secret_access_key].iter().any(|s| s.len() > 512) {
        return Err(UiError::new("InvalidInput", "Eingabe zu lang."));
    }
    let s3 = space_core::links::s3::S3Config {
        endpoint: endpoint.trim().to_string(),
        bucket: bucket.trim().to_string(),
        region: "auto".into(),
        access_key_id: access_key_id.trim().to_string(),
        secret_access_key,
    };
    d.request(Request::LinkSetup(space_core::links::LinkSettings { base_url: base_url.trim().to_string(), s3 })).await
}

/// Link für eine Datei erstellen. Die Datei wird verschlüsselt hochgeladen – daher bestätigt
/// der Nutzer im nativen Dialog (eine manipulierte Web-Ansicht kann so nichts unbemerkt teilen).
#[tauri::command]
pub async fn link_create(
    app: AppHandle,
    d: State<'_, Daemon>,
    path: String,
    days: Option<u32>,
    max_downloads: Option<u32>,
    password: Option<String>,
) -> UiResult<Value> {
    let p = ui_path(&path)?;
    if password.as_ref().is_some_and(|pw| pw.len() > 256) {
        return Err(UiError::new("InvalidInput", "Passwort zu lang."));
    }
    let name = summary(std::slice::from_ref(&p)).trim_start_matches("• ").to_string();
    // Ziel aus dem Dienst (nicht aus der Web-Ansicht) anzeigen: eine manipulierte Einrichtung
    // fiele hier auf.
    let target = d
        .request(Request::LinkList)
        .await
        .ok()
        .and_then(|v| v.get("base_url").and_then(|u| u.as_str()).map(|u| short_text(u, 120)))
        .unwrap_or_default();
    let text = tr(
        "„{name}“ verschlüsselt hochladen und einen Link erstellen? Wer den Link hat, kann die Datei herunterladen.\n\nZiel: {target}",
        &[("name", &name), ("target", &target)],
    );
    if !confirm(&app, tr("Link erstellen", &[]), text, tr("Link erstellen", &[])).await {
        return Err(cancelled());
    }
    let password = password.filter(|pw| !pw.is_empty());
    d.request(Request::LinkCreate { path: p, days, max_downloads, password }).await
}

#[tauri::command]
pub async fn link_revoke(d: State<'_, Daemon>, id: String) -> UiResult<Value> {
    if id.is_empty() || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(UiError::new("InvalidInput", "Ungültige Link-ID."));
    }
    d.request(Request::LinkRevoke { id }).await
}

// ---------------------------------------------------------------- Speicheranalyse

#[tauri::command]
pub async fn space_usage(d: State<'_, Daemon>, path: String) -> UiResult<Value> {
    d.request(Request::SpaceUsage { path: ui_path(&path)?, limit: 50 }).await
}

#[tauri::command]
pub async fn duplicates(d: State<'_, Daemon>, path: Option<String>) -> UiResult<Value> {
    let path = path.map(|p| ui_path(&p)).transpose()?;
    d.request(Request::Duplicates { path, min_size: Some(1024 * 1024), limit: 200 }).await
}

// ---------------------------------------------------------------- Schriftart

/// Höchstgröße einer eigenen Schriftdatei.
const MAX_FONT: u64 = 16 * 1024 * 1024;

fn font_dir() -> UiResult<std::path::PathBuf> {
    directories::ProjectDirs::from("de", "snap", "snap")
        .map(|d| d.data_dir().join("fonts"))
        .ok_or_else(|| UiError::new("Internal", "Kein Datenordner."))
}

/// Erkennt TrueType/OpenType an den ersten Bytes (keine weitere Auswertung hier – die
/// Schrift lädt die Web-Ansicht in ihrem eigenen Prozess).
pub fn is_font(head: &[u8]) -> bool {
    matches!(head.get(..4), Some([0, 1, 0, 0]) | Some(b"OTTO") | Some(b"true"))
}

/// Gespeicherte eigene Schriftart (Bytes) oder leer.
#[tauri::command]
pub async fn font_get() -> UiResult<tauri::ipc::Response> {
    let file = font_dir()?.join("eigene.font");
    let bytes = match std::fs::metadata(&file) {
        Ok(m) if m.is_file() && m.len() <= MAX_FONT => std::fs::read(&file).unwrap_or_default(),
        _ => Vec::new(),
    };
    Ok(tauri::ipc::Response::new(if is_font(&bytes) { bytes } else { Vec::new() }))
}

/// Schriftdatei im nativen Dialog wählen, prüfen und als eigene Schrift speichern.
/// Ergebnis: Dateiname (zur Anzeige) oder Abbruch.
#[tauri::command]
pub async fn font_pick(app: AppHandle) -> UiResult<String> {
    let app2 = app.clone();
    let picked = tokio::task::spawn_blocking(move || {
        app2.dialog().file().add_filter("Schriftart", &["ttf", "otf"]).blocking_pick_file()
    })
    .await
    .map_err(|e| UiError::new("Internal", e.to_string()))?;
    let Some(picked) = picked else { return Err(cancelled()) };
    let src = picked.into_path().map_err(|_| UiError::new("InvalidInput", "Ungültige Datei."))?;
    let meta = std::fs::metadata(&src).map_err(|e| UiError::new("NotFound", e.to_string()))?;
    if !meta.is_file() || meta.len() > MAX_FONT {
        return Err(UiError::new("InvalidInput", "Keine Schriftdatei (höchstens 16 MB)."));
    }
    let bytes = std::fs::read(&src).map_err(|e| UiError::new("Internal", e.to_string()))?;
    if !is_font(&bytes) {
        return Err(UiError::new("InvalidInput", "Keine TrueType- oder OpenType-Schrift."));
    }
    let dir = font_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| UiError::new("Internal", e.to_string()))?;
    let tmp = dir.join(".eigene.font.tmp");
    std::fs::write(&tmp, &bytes).and_then(|_| std::fs::rename(&tmp, dir.join("eigene.font"))).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        UiError::new("Internal", e.to_string())
    })?;
    let name = src
        .file_stem()
        .map(|n| n.to_string_lossy().chars().filter(|c| !c.is_control()).take(80).collect())
        .unwrap_or_default();
    let _ = std::fs::write(dir.join("eigene.name"), &name);
    Ok(name)
}

/// Name der eigenen Schrift (leer = Inconsolata).
#[tauri::command]
pub async fn font_name() -> UiResult<String> {
    let dir = font_dir()?;
    if !dir.join("eigene.font").is_file() {
        return Ok(String::new());
    }
    Ok(std::fs::read_to_string(dir.join("eigene.name")).unwrap_or_default().chars().take(80).collect())
}

/// Zurück zu Inconsolata.
#[tauri::command]
pub async fn font_reset() -> UiResult<()> {
    let dir = font_dir()?;
    let _ = std::fs::remove_file(dir.join("eigene.font"));
    let _ = std::fs::remove_file(dir.join("eigene.name"));
    Ok(())
}

#[cfg(test)]
mod font_tests {
    use super::is_font;

    #[test]
    fn erkennt_nur_schriftdateien() {
        assert!(is_font(&[0, 1, 0, 0, 9]));
        assert!(is_font(b"OTTO...."));
        assert!(is_font(b"true...."));
        assert!(!is_font(b"wOFF")); // Web-Schrift: nicht unterstützt
        assert!(!is_font(b"<svg"));
        assert!(!is_font(&[]));
        assert!(!is_font(&[0, 1]));
    }
}
