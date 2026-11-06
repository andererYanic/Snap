//! Snap – Desktop-App (Tauri v2). Die Oberfläche liegt in `../src` (React).
#![deny(clippy::undocumented_unsafe_blocks)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod commands;

use bridge::{percent_decode, Daemon};
use space_core::ipc::{read_frame, write_frame, Envelope, Request, Response};
use tauri::http::{header, Response as HttpResponse, StatusCode};
use tauri::{Emitter, Manager};

/// Leitet Ereignisse des Daemons an die Oberfläche weiter (`daemon-event`) und meldet, ob der
/// Dienst erreichbar ist (`daemon-status`). Verbindet sich bei Abbruch neu.
async fn forward_events(app: tauri::AppHandle, daemon: Daemon) {
    loop {
        if let Ok(mut stream) = daemon.connect().await {
            if write_frame(&mut stream, &Envelope::new(Request::Subscribe)).await.is_ok()
                && read_frame::<_, Response>(&mut stream).await.is_ok()
            {
                let _ = app.emit("daemon-status", true);
                while let Ok(Response::Event(ev)) = read_frame::<_, Response>(&mut stream).await {
                    let _ = app.emit("daemon-event", ev);
                }
            }
        }
        let _ = app.emit("daemon-status", false);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

/// `thumb://localhost/<Pfad, URL-kodiert>?large` → JPEG-Vorschaubild aus dem Cache des Daemons.
/// Nur lesend; liefert ausschließlich Bilddaten.
async fn thumbnail(daemon: &Daemon, uri: &tauri::http::Uri) -> HttpResponse<Vec<u8>> {
    let not_found = || HttpResponse::builder().status(StatusCode::NOT_FOUND).body(Vec::new()).unwrap_or_default();
    let Some(path) = percent_decode(uri.path()).and_then(|p| bridge::ui_path(&p).ok()) else {
        return not_found();
    };
    let size = if uri.query() == Some("large") {
        space_core::media::ThumbSize::Large
    } else {
        space_core::media::ThumbSize::Small
    };
    let Ok(v) = daemon.request(Request::Thumbnail { path, size }).await else { return not_found() };
    let Some(file) = v.get("path").and_then(|p| p.as_str()) else { return not_found() };
    match tokio::fs::read(file).await {
        Ok(bytes) => HttpResponse::builder()
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "max-age=600")
            .body(bytes)
            .unwrap_or_else(|_| not_found()),
        Err(_) => not_found(),
    }
}

fn main() {
    let daemon = match Daemon::from_config() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Snap: {e:#}");
            std::process::exit(1);
        }
    };
    let thumb_daemon = daemon.clone();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(daemon.clone())
        .register_asynchronous_uri_scheme_protocol("thumb", move |_ctx, request, responder| {
            let d = thumb_daemon.clone();
            let uri = request.uri().clone();
            tauri::async_runtime::spawn(async move {
                responder.respond(thumbnail(&d, &uri).await);
            });
        })
        .setup(move |app| {
            tauri::async_runtime::spawn(forward_events(app.handle().clone(), daemon.clone()));
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title("Snap");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::info,
            commands::list_dir,
            commands::search,
            commands::metadata,
            commands::index_roots,
            commands::trash_list,
            commands::stream_url,
            commands::timeline,
            commands::timeline_months,
            commands::copy_items,
            commands::move_items,
            commands::rename,
            commands::mkdir,
            commands::trash,
            commands::undo,
            commands::trash_restore,
            commands::delete_permanently,
            commands::trash_empty,
            commands::open_path,
            commands::start_daemon,
            commands::places,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            eprintln!("Snap konnte nicht starten: {e}");
            std::process::exit(1);
        });
}
