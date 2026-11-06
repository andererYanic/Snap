//! Typisierte Befehle für die Oberfläche. Jeder Befehl baut genau eine Daemon-Anfrage; es gibt
//! bewusst keinen Befehl, der beliebige Anfragen durchreicht (siehe `bridge.rs`).

use crate::bridge::{is_risky_to_open, ui_path, Daemon, UiError, UiResult};
use serde_json::Value;
use space_core::fileops::jobs::FileOp;
use space_core::fileops::ops::Conflict;
use space_core::ipc::Request;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// Nativer Bestätigungsdialog (außerhalb der Web-Ansicht). `true` = bestätigt.
async fn confirm(app: &AppHandle, title: &str, text: String, ok: &str) -> bool {
    let app = app.clone();
    let (title, ok) = (title.to_string(), ok.to_string());
    tokio::task::spawn_blocking(move || {
        app.dialog()
            .message(text)
            .title(title)
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(ok, "Abbrechen".into()))
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
        s.push(format!("… und {} weitere", list.len() - 5));
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
    let text = format!(
        "{} endgültig löschen? Das kann nicht rückgängig gemacht werden.\n\n{}",
        if list.len() == 1 { "Dieses Element".to_string() } else { format!("Diese {} Elemente", list.len()) },
        summary(&list)
    );
    if !confirm(&app, "Endgültig löschen", text, "Endgültig löschen").await {
        return Err(cancelled());
    }
    file_op(&d, FileOp::Delete { paths: list }).await
}

#[tauri::command]
pub async fn trash_empty(app: AppHandle, d: State<'_, Daemon>) -> UiResult<Value> {
    let text = "Alle Elemente im Papierkorb endgültig löschen?".to_string();
    if !confirm(&app, "Papierkorb leeren", text, "Leeren").await {
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
        let text = format!(
            "„{}“ kann Programme ausführen. Nur öffnen, wenn die Datei aus einer vertrauenswürdigen Quelle stammt.",
            summary(std::slice::from_ref(&p)).trim_start_matches("• ")
        );
        if !confirm(&app, "Datei öffnen?", text, "Trotzdem öffnen").await {
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
    let text = format!(
        "„{}“ entkoppeln? Das Gerät verliert alle Rechte und sieht deine Freigaben nicht mehr.",
        short_text(&name, 80)
    );
    if !confirm(&app, "Gerät entkoppeln", text, "Entkoppeln").await {
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
    let scope = folder.as_ref().map(|f| format!(" für {}", short_text(&f.to_string_lossy(), 120))).unwrap_or_default();
    let text = format!(
        "„{}“ das Recht „{}“{scope} geben?\n\nGib Rechte nur Geräten, die dir gehören.",
        short_text(&name, 80),
        short_text(&capability, 40)
    );
    if !confirm(&app, "Recht gewähren", text, "Gewähren").await {
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
    let text = format!("Diese Änderung eines anderen Geräts zulassen?\n\n{}", short_text(&summary, 300));
    if !confirm(&app, "Fernzugriff zulassen", text, "Zulassen").await {
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
    let text = "Diese Sicherung aus Snap entfernen? Die gesicherten Daten am Ziel bleiben erhalten und lassen sich mit Passwort oder Wiederherstellungsschlüssel wieder verbinden.".to_string();
    if !confirm(&app, "Sicherung entfernen", text, "Entfernen").await {
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
