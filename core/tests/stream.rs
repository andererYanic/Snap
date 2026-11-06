//! Streaming über den lokalen HTTP-Server mit echten Daemons – inklusive Angriffstests
//! (fremder Host / DNS-Rebinding, falsches Token, falsche Methode, kaputte Bereiche,
//! entzogenes Recht beim Ursprungsgerät).

use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = None;
    c.connect_timeout_secs = 3;
    c.device_name = name.into();
    c.live_connections = false;
    Daemon::start_loopback(c).await.unwrap()
}

struct HttpReply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpReply {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

/// Minimaler HTTP/1.1-Client (eine Anfrage, Verbindung wird danach geschlossen).
async fn http(port: u16, raw: &str) -> HttpReply {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(raw.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), s.read_to_end(&mut buf)).await.unwrap().unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("Kopf");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let headers =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    HttpReply { status, headers, body: buf[split + 4..].to_vec() }
}

fn get(port: u16, path: &str, extra: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{extra}\r\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_ranges_from_another_device() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("filme");
    std::fs::create_dir_all(&share).unwrap();
    // 5 MiB pseudo-zufällige Daten (prüfbar)
    let data: Vec<u8> = (0..5 * 1024 * 1024u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    std::fs::write(share.join("film.mkv"), &data).unwrap();

    let a = start(&tmp.path().join("a"), "PC").await;
    let b = start(&tmp.path().join("b"), "Handy").await;
    let code = match a.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(b.request(Request::PairJoin { code }).await, Response::Paired { .. }));
    a.request(Request::ShareAdd { path: share.clone(), name: Some("Filme".into()), devices: None }).await;
    // Warten, bis der Katalog bei B ist
    let mut ok = false;
    for _ in 0..80 {
        b.request(Request::Sync).await;
        if let Response::Catalog { remote, .. } = b.request(Request::List).await {
            if remote.iter().any(|f| f.rel_path.as_deref() == Some("film.mkv")) {
                ok = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(ok, "Katalog nicht angekommen");

    let info = match b.request(Request::StreamUrl { target: "Filme/film.mkv".into() }).await {
        Response::StreamUrl(i) => i,
        other => panic!("{other:?}"),
    };
    assert!(info.url.starts_with("http://127.0.0.1:"), "{}", info.url);
    assert_eq!(info.mime, "video/x-matroska");
    let port: u16 = info.url["http://127.0.0.1:".len()..].split('/').next().unwrap().parse().unwrap();
    let path = &info.url[format!("http://127.0.0.1:{port}").len()..];

    // Bereich mitten aus der Datei: nur dieser Teil wird übertragen
    let r = http(port, &get(port, path, "Range: bytes=3000000-3000999\r\n")).await;
    assert_eq!(r.status, 206);
    assert_eq!(r.header("content-range"), Some(format!("bytes 3000000-3000999/{}", data.len()).as_str()));
    assert_eq!(r.body, &data[3_000_000..3_001_000]);
    let hash = iroh_blobs::Hash::new(&data);
    let local = b.network().local_bytes(hash).await;
    assert!(local > 0 && local < data.len() as u64 / 2, "nur ein Teil geladen: {local}");
    // Ende der Datei, HEAD, ganze Datei
    let r = http(port, &get(port, path, "Range: bytes=-10\r\n")).await;
    assert_eq!(r.body, &data[data.len() - 10..]);
    let head =
        http(port, &format!("HEAD {path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")).await;
    assert_eq!(head.status, 200);
    assert_eq!(head.header("content-length"), Some(data.len().to_string().as_str()));
    assert_eq!(head.header("accept-ranges"), Some("bytes"));
    assert_eq!(head.header("x-content-type-options"), Some("nosniff"));
    let full = http(port, &get(port, path, "")).await;
    assert_eq!(full.status, 200);
    assert!(full.body == data, "ganze Datei identisch");

    // Angriffe
    let evil_host = format!("GET {path} HTTP/1.1\r\nHost: evil.example:{port}\r\nConnection: close\r\n\r\n");
    assert_eq!(http(port, &evil_host).await.status, 403, "DNS-Rebinding");
    let no_host = format!("GET {path} HTTP/1.1\r\nConnection: close\r\n\r\n");
    assert!(matches!(http(port, &no_host).await.status, 400 | 403));
    let wrong_token = format!("/s/{}/film.mkv", "0".repeat(64));
    assert_eq!(http(port, &get(port, &wrong_token, "")).await.status, 404);
    assert_eq!(http(port, &get(port, "/s/../../etc/passwd", "")).await.status, 404);
    assert_eq!(http(port, &get(port, "/", "")).await.status, 404);
    let post =
        format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    assert_eq!(http(port, &post).await.status, 405);
    let r = http(port, &get(port, path, "Range: bytes=999999999-\r\n")).await;
    assert_eq!(r.status, 416);
    assert_eq!(r.header("content-range"), Some(format!("bytes */{}", data.len()).as_str()));
    let garbage = http(port, "KAPUTT\r\n\r\n").await;
    assert_eq!(garbage.status, 400);

    // Ursprungsgerät entzieht das Leserecht: neue (noch nicht geladene) Teile kommen nicht mehr
    let other = b"anderer inhalt".repeat(100_000);
    std::fs::write(share.join("zweiter.mkv"), &other).unwrap();
    let mut seen = false;
    for _ in 0..80 {
        b.request(Request::Sync).await;
        if let Response::Catalog { remote, .. } = b.request(Request::List).await {
            if remote.iter().any(|f| f.rel_path.as_deref() == Some("zweiter.mkv")) {
                seen = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(seen);
    let res =
        a.request(Request::PairRevoke { device: "Handy".into(), capability: "files.read".into(), folder: None }).await;
    assert!(matches!(res, Response::Success { .. }), "{res:?}");
    let info2 = match b.request(Request::StreamUrl { target: "Filme/zweiter.mkv".into() }).await {
        Response::StreamUrl(i) => i,
        other => panic!("{other:?}"),
    };
    let path2 = &info2.url[format!("http://127.0.0.1:{port}").len()..];
    assert_eq!(http(port, &get(port, path2, "Range: bytes=0-99\r\n")).await.status, 502);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn thumbnails_of_remote_shares_need_files_read() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("fotos");
    std::fs::create_dir_all(share.join("2024")).unwrap();
    image::RgbImage::from_fn(1200, 800, |x, y| image::Rgb([x as u8, y as u8, 7]))
        .save(share.join("2024/strand.jpg"))
        .unwrap();
    std::os::unix::fs::symlink(tmp.path(), share.join("ausweg")).unwrap();
    let a = start(&tmp.path().join("a"), "PC").await;
    let b = start(&tmp.path().join("b"), "Handy").await;
    let code = match a.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    assert!(matches!(b.request(Request::PairJoin { code }).await, Response::Paired { .. }));
    a.request(Request::ShareAdd { path: share.clone(), name: Some("Fotos".into()), devices: None }).await;
    let req = |path: &str| Request::RemoteThumbnail {
        device: "PC".into(),
        share: "Fotos".into(),
        path: path.into(),
        size: Default::default(),
    };

    // Wie in der Oberfläche: Die Galerie zeigt Dateien aus dem abgeglichenen Katalog.
    for _ in 0..80 {
        b.request(Request::Sync).await;
        if let Response::Catalog { remote, .. } = b.request(Request::List).await {
            if remote.iter().any(|f| f.rel_path.as_deref() == Some("2024/strand.jpg")) {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let t = match b.request(req("2024/strand.jpg")).await {
        Response::Thumbnail(t) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!((t.width, t.height, t.cached), (256, 171, false));
    assert!(t.path.starts_with(&b.config().data_dir), "liegt im Cache von B");
    match b.request(req("2024/strand.jpg")).await {
        Response::Thumbnail(t) => assert!(t.cached),
        other => panic!("{other:?}"),
    }
    let code_of = |r: &Response| match r {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    };
    use space_core::api::error::ErrorCode;
    assert_eq!(code_of(&b.request(req("../a/secret.key")).await), Some(ErrorCode::InvalidInput));
    assert_eq!(code_of(&b.request(req("ausweg/a/secret.key")).await), Some(ErrorCode::PermissionDenied));
    assert_eq!(code_of(&b.request(req("2024/fehlt.jpg")).await), Some(ErrorCode::NotFound));
    // Recht entzogen → abgelehnt
    a.request(Request::PairRevoke { device: "Handy".into(), capability: "files.read".into(), folder: None }).await;
    std::fs::copy(share.join("2024/strand.jpg"), share.join("2024/neu.jpg")).unwrap();
    assert_eq!(code_of(&b.request(req("2024/neu.jpg")).await), Some(ErrorCode::PermissionDenied));
    a.shutdown().await;
    b.shutdown().await;
}
