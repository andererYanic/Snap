//! Lokale Dateioperationen über den Daemon: Aufträge, Ereignisse, Suche, Rückgängig, Papierkorb.

use space_core::api::error::ErrorCode;
use space_core::events::Event;
use space_core::fileops::jobs::FileOp;
use space_core::fileops::ops::Conflict;
use space_core::index::SearchFilter;
use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::{Duration, Instant};

async fn start(root: &Path) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    // Port vom Betriebssystem (0): kein Wettlauf mit parallel laufenden Tests.
    c.bind_port = None;
    c.live_connections = false;
    Daemon::start_loopback(c).await.unwrap()
}

async fn op(d: &Daemon, op: FileOp) -> Response {
    d.request(Request::FileOp { op, wait: true }).await
}

async fn search(d: &Daemon, q: &str) -> Vec<String> {
    match d.request(Request::Search { query: q.into(), filter: SearchFilter::default(), limit: 50 }).await {
        Response::SearchResults { hits } => hits.into_iter().map(|h| h.path).collect(),
        other => panic!("{other:?}"),
    }
}

fn code(res: &Response) -> Option<ErrorCode> {
    match res {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn file_ops_update_search_immediately_and_undo() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("Eingang")).unwrap();
    std::fs::create_dir_all(home.join("Archiv")).unwrap();
    std::fs::write(home.join("Eingang/Vertrag.pdf"), b"pdf").unwrap();
    let d = start(&tmp.path().join("snap")).await;
    d.request(Request::IndexAdd { path: home.clone() }).await;
    let t = Instant::now();
    while search(&d, "vertrag").await.len() != 1 {
        assert!(t.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Verschieben → Suche sofort aktuell (ohne Watcher-Verzögerung)
    let mv = FileOp::Move {
        sources: vec![home.join("Eingang/Vertrag.pdf")],
        dest: home.join("Archiv"),
        conflict: Conflict::Fail,
    };
    assert!(matches!(op(&d, mv).await, Response::FileOpDone { .. }));
    let hits = search(&d, "vertrag").await;
    assert_eq!(hits, [home.join("Archiv/Vertrag.pdf").to_string_lossy()], "Suche sofort aktuell");

    // Umbenennen, Ordner anlegen, Papierkorb
    op(&d, FileOp::Rename { path: home.join("Archiv/Vertrag.pdf"), new_name: "Mietvertrag.pdf".into() }).await;
    assert_eq!(search(&d, "mietvertrag").await.len(), 1);
    op(&d, FileOp::Mkdir { parent: home.clone(), name: "Projekte".into() }).await;
    assert!(home.join("Projekte").is_dir());
    match op(&d, FileOp::Trash { paths: vec![home.join("Archiv/Mietvertrag.pdf")] }).await {
        Response::FileOpDone { outcome, .. } => assert_eq!(outcome.trashed.len(), 1),
        other => panic!("{other:?}"),
    }
    assert!(search(&d, "mietvertrag").await.is_empty());
    let entries = match d.request(Request::TrashList).await {
        Response::Trash { entries } => entries,
        other => panic!("{other:?}"),
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].original_path, home.join("Archiv/Mietvertrag.pdf").to_string_lossy());
    assert!(tmp.path().join("snap/Trash/files/Mietvertrag.pdf").exists(), "eigener Papierkorb in Tests");

    // Rückgängig: Papierkorb → zurück, Ordner weg, Name zurück, Ort zurück
    assert!(matches!(d.request(Request::Undo).await, Response::Success { .. }));
    assert_eq!(search(&d, "mietvertrag").await.len(), 1);
    d.request(Request::Undo).await;
    assert!(!home.join("Projekte").exists());
    d.request(Request::Undo).await;
    d.request(Request::Undo).await;
    assert_eq!(search(&d, "vertrag").await, [home.join("Eingang/Vertrag.pdf").to_string_lossy()]);
    assert_eq!(code(&d.request(Request::Undo).await), Some(ErrorCode::NotFound));

    // Konflikte und Fehler mit Codes
    std::fs::write(home.join("Archiv/Vertrag.pdf"), b"anderer").unwrap();
    let cp =
        |c| FileOp::Copy { sources: vec![home.join("Eingang/Vertrag.pdf")], dest: home.join("Archiv"), conflict: c };
    assert_eq!(code(&op(&d, cp(Conflict::Fail)).await), Some(ErrorCode::AlreadyExists));
    match op(&d, cp(Conflict::KeepBoth)).await {
        Response::FileOpDone { outcome, .. } => assert_eq!(outcome.done[0].to, home.join("Archiv/Vertrag (2).pdf")),
        other => panic!("{other:?}"),
    }
    let rel = FileOp::Trash { paths: vec!["relativ".into()] };
    assert_eq!(code(&op(&d, rel).await), Some(ErrorCode::InvalidInput));
    assert_eq!(code(&d.request(Request::FileOpCancel { job: 9999 }).await), Some(ErrorCode::NotFound));

    // Wiederherstellen aus dem Papierkorb und leeren
    op(&d, FileOp::Trash { paths: vec![home.join("Archiv/Vertrag (2).pdf")] }).await;
    let id = match d.request(Request::TrashList).await {
        Response::Trash { entries } => entries[0].id.clone(),
        other => panic!("{other:?}"),
    };
    assert!(matches!(d.request(Request::TrashRestore { id, keep_both: false }).await, Response::Success { .. }));
    assert!(home.join("Archiv/Vertrag (2).pdf").exists());
    op(&d, FileOp::Trash { paths: vec![home.join("Archiv/Vertrag (2).pdf")] }).await;
    assert!(matches!(d.request(Request::TrashEmpty).await, Response::Success { .. }));
    assert!(matches!(d.request(Request::TrashList).await, Response::Trash { entries } if entries.is_empty()));
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn background_job_reports_progress_and_can_be_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("quelle");
    std::fs::create_dir_all(&src).unwrap();
    // Große Datei (blockweise kopiert) → mehrere Fortschrittsereignisse, Abbruch möglich
    std::fs::File::create(src.join("gross.bin")).unwrap().set_len(1024 * 1024 * 1024).unwrap();
    std::fs::create_dir_all(tmp.path().join("ziel")).unwrap();
    let d = start(&tmp.path().join("snap")).await;
    let mut events = d.subscribe();

    let copy = FileOp::Copy { sources: vec![src.clone()], dest: tmp.path().join("ziel"), conflict: Conflict::Fail };
    let job = match d.request(Request::FileOp { op: copy, wait: false }).await {
        Response::FileOpStarted { job } => job,
        other => panic!("{other:?}"),
    };
    // Auf das erste Fortschrittsereignis warten, dann abbrechen
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "kein Fortschritt");
        match tokio::time::timeout(Duration::from_secs(5), events.next()).await {
            Ok(Some(Event::FileOpProgress { job: j, total_bytes, .. })) if j == job => {
                assert_eq!(total_bytes, 1024 * 1024 * 1024);
                break;
            }
            Ok(Some(Event::FileOpFinished { job: j, .. })) if j == job => panic!("zu schnell fertig"),
            _ => {}
        }
    }
    assert!(matches!(d.request(Request::FileOpCancel { job }).await, Response::Success { .. }));
    loop {
        match tokio::time::timeout(Duration::from_secs(10), events.next()).await {
            Ok(Some(Event::FileOpFinished { job: j, ok, message })) if j == job => {
                assert!(!ok);
                assert!(message.contains("abgebrochen"), "{message}");
                break;
            }
            Ok(Some(_)) => {}
            other => panic!("kein Ende: {other:?}"),
        }
    }
    // Keine halbe Kopie
    assert!(!tmp.path().join("ziel/quelle").exists());
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn thumbnails_and_metadata_via_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(&tmp.path().join("snap")).await;
    let img = image::RgbImage::from_fn(800, 600, |x, y| image::Rgb([x as u8, y as u8, 0]));
    let photo = tmp.path().join("foto.jpg");
    img.save(&photo).unwrap();

    match d.request(Request::Thumbnail { path: photo.clone(), size: Default::default() }).await {
        Response::Thumbnail(t) => {
            assert_eq!((t.width, t.height), (256, 192));
            assert!(t.path.starts_with(&d.config().data_dir), "Cache im privaten Datenordner");
            assert!(t.path.exists());
        }
        other => panic!("{other:?}"),
    }
    match d.request(Request::Metadata { path: photo.clone() }).await {
        Response::Metadata(m) => {
            assert_eq!((m.width, m.height), (Some(800), Some(600)));
            assert_eq!(m.mime.as_deref(), Some("image/jpeg"));
        }
        other => panic!("{other:?}"),
    }
    // Keine Bilddatei / relativer Pfad / fehlend
    std::fs::write(tmp.path().join("text.txt"), b"hallo").unwrap();
    let res = d.request(Request::Thumbnail { path: tmp.path().join("text.txt"), size: Default::default() }).await;
    assert_eq!(code(&res), Some(ErrorCode::InvalidInput));
    let res = d.request(Request::Thumbnail { path: "foto.jpg".into(), size: Default::default() }).await;
    assert_eq!(code(&res), Some(ErrorCode::InvalidInput));
    let res = d.request(Request::Metadata { path: tmp.path().join("fehlt") }).await;
    assert_eq!(code(&res), Some(ErrorCode::NotFound));
    d.shutdown().await;
}
