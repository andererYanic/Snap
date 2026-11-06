//! Lokale Suche und Explorer-API über den Daemon (Watcher, Rechte, Eingaben).

use space_core::api::auth::Caller;
use space_core::api::error::ErrorCode;
use space_core::index::{NodeKind, SearchFilter};
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

async fn search(d: &Daemon, q: &str) -> Vec<String> {
    match d.request(Request::Search { query: q.into(), filter: SearchFilter::default(), limit: 50 }).await {
        Response::SearchResults { hits } => hits.into_iter().map(|h| h.path).collect(),
        other => panic!("{other:?}"),
    }
}

/// Wartet, bis die Suche `pred` erfüllt (Indexierung/Watcher laufen im Hintergrund).
async fn wait_search(d: &Daemon, q: &str, pred: impl Fn(&[String]) -> bool) -> Duration {
    let t = Instant::now();
    loop {
        let hits = search(d, q).await;
        if pred(&hits) {
            return t.elapsed();
        }
        assert!(t.elapsed() < Duration::from_secs(15), "Zeitüberschreitung bei Suche '{q}': {hits:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn code(res: &Response) -> Option<ErrorCode> {
    match res {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn search_follows_filesystem_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(home.join("Dokumente/Steuer")).unwrap();
    std::fs::write(home.join("Dokumente/Steuer/Rechnung-2024.pdf"), b"pdf").unwrap();
    let d = start(&tmp.path().join("snap")).await;

    // Ort hinzufügen → erste Indexierung im Hintergrund
    match d.request(Request::IndexAdd { path: home.clone() }).await {
        Response::IndexRoots { roots, .. } => assert_eq!(roots.len(), 1),
        other => panic!("{other:?}"),
    }
    wait_search(&d, "rechnung", |h| h.len() == 1).await;

    // Neue Datei in bestehendem Ordner → Watcher
    std::fs::write(home.join("Dokumente/Steuer/Quittung.pdf"), b"x").unwrap();
    let t_new = wait_search(&d, "quittung", |h| h.len() == 1).await;
    // Neuer Ordnerbaum → auch Dateien darin, und der neue Ordner wird beobachtet
    std::fs::create_dir_all(home.join("Urlaub/Strand")).unwrap();
    std::fs::write(home.join("Urlaub/Strand/Bild.jpg"), b"x").unwrap();
    wait_search(&d, "bild.jpg", |h| h.len() == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(home.join("Urlaub/Strand/Muschel.jpg"), b"x").unwrap();
    wait_search(&d, "muschel", |h| h.len() == 1).await;
    // Umbenennen und Löschen
    std::fs::rename(home.join("Dokumente/Steuer"), home.join("Dokumente/Finanzen")).unwrap();
    wait_search(&d, "rechnung", |h| h.len() == 1 && h[0].contains("/Finanzen/")).await;
    std::fs::remove_dir_all(home.join("Urlaub")).unwrap();
    let t_del = wait_search(&d, "muschel", |h| h.is_empty()).await;
    println!("Neue Datei auffindbar nach {t_new:?}, Löschung nach {t_del:?}");

    match d.request(Request::IndexRoots).await {
        Response::IndexRoots { roots, watched_dirs } => {
            assert!(roots[0].last_scan.is_some());
            assert!(watched_dirs >= 3, "{watched_dirs}");
        }
        other => panic!("{other:?}"),
    }

    // Filter
    let res = d
        .request(Request::Search {
            query: "fin".into(),
            filter: SearchFilter { kind: Some(NodeKind::Dir), ..Default::default() },
            limit: 10,
        })
        .await;
    assert!(matches!(&res, Response::SearchResults { hits } if hits.len() == 1), "{res:?}");

    // Entfernen leert die Suche
    let id = match d.request(Request::IndexRoots).await {
        Response::IndexRoots { roots, .. } => roots[0].id,
        other => panic!("{other:?}"),
    };
    assert!(matches!(d.request(Request::IndexRemove { id }).await, Response::IndexRoots { .. }));
    assert!(search(&d, "rechnung").await.is_empty());
    assert_eq!(code(&d.request(Request::IndexRemove { id: 999 }).await), Some(ErrorCode::NotFound));
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn explorer_inputs_and_rights() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(&tmp.path().join("snap")).await;
    let dir = tmp.path().join("ordner");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..25 {
        std::fs::write(dir.join(format!("datei-{i:02}.txt")), b"x").unwrap();
    }
    std::fs::write(dir.join(".versteckt"), b"x").unwrap();

    // Seitenweises Lesen, versteckte nur auf Wunsch
    let page = |offset, limit, show_hidden| Request::ListDir { path: dir.clone(), show_hidden, offset, limit };
    match d.request(page(20, 10, false)).await {
        Response::DirListing { entries, total, .. } => {
            assert_eq!(total, 25);
            assert_eq!(entries.len(), 5);
            assert_eq!(entries[0].name, "datei-20.txt");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(d.request(page(0, 0, true)).await, Response::DirListing { total: 26, ref entries, .. } if entries.len() == 1)
    );

    // Ungültige Eingaben
    let rel = Request::ListDir { path: "relativ".into(), show_hidden: false, offset: 0, limit: 10 };
    assert_eq!(code(&d.request(rel).await), Some(ErrorCode::InvalidInput));
    let missing = Request::ListDir { path: tmp.path().join("fehlt"), show_hidden: false, offset: 0, limit: 10 };
    assert_eq!(code(&d.request(missing).await), Some(ErrorCode::NotFound));
    assert_eq!(code(&d.request(Request::IndexAdd { path: "relativ".into() }).await), Some(ErrorCode::InvalidInput));
    let file = Request::IndexAdd { path: dir.join("datei-01.txt") };
    assert_eq!(code(&d.request(file).await), Some(ErrorCode::InvalidInput));
    // Snap-Daten (Schlüssel, Datenbanken) sind kein Suchort – auch nicht über einen Symlink
    let data = d.config().data_dir.clone();
    assert_eq!(code(&d.request(Request::IndexAdd { path: data.clone() }).await), Some(ErrorCode::InvalidInput));
    std::os::unix::fs::symlink(&data, tmp.path().join("tarnung")).unwrap();
    let sneaky = Request::IndexAdd { path: tmp.path().join("tarnung") };
    assert_eq!(code(&d.request(sneaky).await), Some(ErrorCode::InvalidInput));
    let long = Request::Search { query: "x".repeat(300), filter: SearchFilter::default(), limit: 10 };
    assert_eq!(code(&d.request(long).await), Some(ErrorCode::InvalidInput));

    // Gekoppelte oder fremde Geräte: weder Suche noch Dateisystem
    let dev = Caller::Device("a".repeat(64));
    for req in [
        Request::Search { query: "datei".into(), filter: SearchFilter::default(), limit: 10 },
        page(0, 10, true),
        Request::IndexAdd { path: dir.clone() },
        Request::IndexRoots,
    ] {
        assert_eq!(code(&d.request_as(&dev, req).await), Some(ErrorCode::PermissionDenied));
    }
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn index_can_be_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Config::portable(tmp.path());
    c.use_relay = false;
    // Port vom Betriebssystem (0): kein Wettlauf mit parallel laufenden Tests.
    c.bind_port = None;
    c.index_enabled = false;
    let d = Daemon::start_loopback(c).await.unwrap();
    let res = d.request(Request::Search { query: "abc".into(), filter: SearchFilter::default(), limit: 10 }).await;
    assert_eq!(code(&res), Some(ErrorCode::Unavailable));
    assert!(!tmp.path().join("data/index.db").exists());
    d.shutdown().await;
}
