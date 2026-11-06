//! Freigabelinks gegen einen nachgebauten S3-Speicher (wie Cloudflare R2), inklusive
//! Kreuztest mit dem Browser-Code (Node/WebCrypto) und Angriffs-/Fehlerfällen.

use space_core::api::error::ErrorCode;
use space_core::ipc::{Request, Response};
use space_core::links::crypto::{open_all, open_meta, LinkKeys};
use space_core::links::{s3::S3Config, LinkSettings};
use space_core::{Config, Daemon};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

type Store = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Nachgebauter S3-Speicher: prüft die Signatur-Kopfzeilen, speichert PUT, löscht bei DELETE.
async fn fake_s3() -> (u16, Store) {
    use http_body_util::BodyExt;
    let store: Store = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let s = store.clone();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let s = s.clone();
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let s = s.clone();
                    async move {
                        let auth =
                            req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                        let ok = auth.starts_with("AWS4-HMAC-SHA256 Credential=TESTKEY/")
                            && auth.contains("/auto/s3/aws4_request")
                            && auth.contains("Signature=")
                            && req.headers().get("x-amz-content-sha256").is_some();
                        let status = if !ok {
                            403
                        } else {
                            let path = req.uri().path().to_string();
                            match *req.method() {
                                hyper::Method::PUT => {
                                    let body = req.into_body().collect().await.unwrap().to_bytes().to_vec();
                                    s.lock().unwrap().insert(path, body);
                                    200
                                }
                                hyper::Method::DELETE => {
                                    s.lock().unwrap().remove(&path);
                                    204
                                }
                                _ => 405,
                            }
                        };
                        Ok::<_, std::convert::Infallible>(
                            hyper::Response::builder()
                                .status(status)
                                .body(http_body_util::Full::new(bytes::Bytes::new()))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(tcp), svc)
                    .await;
            });
        }
    });
    (port, store)
}

async fn start(root: &Path) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = None;
    c.live_connections = false;
    Daemon::start_loopback(c).await.unwrap()
}

fn code(r: &Response) -> Option<ErrorCode> {
    match r {
        Response::Error { code, .. } => Some(*code),
        _ => None,
    }
}

fn settings(port: u16, key: &str) -> LinkSettings {
    LinkSettings {
        base_url: "http://localhost:8787".into(),
        s3: S3Config {
            endpoint: format!("http://127.0.0.1:{port}"),
            bucket: "snap-links".into(),
            region: "auto".into(),
            access_key_id: key.into(),
            secret_access_key: "geheimer-schluessel".into(),
        },
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn links_are_end_to_end_encrypted_and_revocable() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(&tmp.path().join("snap")).await;
    let (port, store) = fake_s3().await;
    let file = tmp.path().join("Urlaubsvideo.mp4");
    let plain: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&file, &plain).unwrap();
    let create = |pw: Option<&str>, days: Option<u32>| Request::LinkCreate {
        path: file.clone(),
        days,
        max_downloads: None,
        password: pw.map(String::from),
    };

    // Noch nicht eingerichtet
    assert_eq!(code(&d.request(create(None, None)).await), Some(ErrorCode::Unavailable));
    // Ungültige Einrichtung
    let mut bad = settings(port, "TESTKEY");
    bad.base_url = "http://evil.example".into();
    assert_eq!(code(&d.request(Request::LinkSetup(bad)).await), Some(ErrorCode::InvalidInput));
    let mut bad = settings(port, "TESTKEY");
    bad.s3.endpoint = "https://x.example/pfad".into();
    assert_eq!(code(&d.request(Request::LinkSetup(bad)).await), Some(ErrorCode::InvalidInput));
    // Einrichten; Zugangsdaten nur für den Benutzer lesbar und nie über die API zurück
    assert!(matches!(d.request(Request::LinkSetup(settings(port, "TESTKEY"))).await, Response::Success { .. }));
    let cfg = d.config().data_dir.join("links.json");
    assert_eq!(std::fs::metadata(&cfg).unwrap().permissions().mode() & 0o777, 0o600);
    let listed = format!("{:?}", d.request(Request::LinkList).await);
    assert!(!listed.contains("geheimer-schluessel"));

    // Link mit Passwort
    let link = match d.request(create(Some("korrekt-pferd"), None)).await {
        Response::Link(l) => l,
        other => panic!("{other:?}"),
    };
    assert!(link.url.starts_with(&format!("http://localhost:8787/l/{}#", link.id)), "{}", link.url);
    let secret = link.url.split('#').nth(1).unwrap().to_string();
    let objects = store.lock().unwrap().clone();
    let key = |p: &str| format!("/snap-links/links/{}/{p}", link.id);
    let (data, meta, policy) = (&objects[&key("data")], &objects[&key("meta")], &objects[&key("policy.json")]);
    // Nichts Lesbares beim Speicher: weder Inhalt noch Name
    assert!(!data.windows(16).any(|w| w == &plain[1000..1016]));
    assert!(!String::from_utf8_lossy(meta).contains("Urlaub"));
    assert!(!String::from_utf8_lossy(policy).contains("Urlaub"));
    let pol: serde_json::Value = serde_json::from_slice(policy).unwrap();
    assert_eq!(pol["password"], true);
    let days = (pol["expires"].as_i64().unwrap() - chrono::Utc::now().timestamp() + 60) / 86_400;
    assert_eq!(days, 7, "Standard 7 Tage");
    // Mit Schlüssel aus dem Link + Passwort entschlüsselbar – ohne Passwort nicht
    let raw: [u8; 32] = data_encoding::BASE64URL_NOPAD.decode(secret.as_bytes()).unwrap().try_into().unwrap();
    let keys = LinkKeys::derive(&raw, &link.id, Some("korrekt-pferd"));
    let m = open_meta(&keys, meta).unwrap();
    assert_eq!((m.name.as_str(), m.size, m.mime.as_str()), ("Urlaubsvideo.mp4", plain.len() as u64, "video/mp4"));
    assert_eq!(open_all(&keys, data, m.size).unwrap(), plain);
    assert!(open_meta(&LinkKeys::derive(&raw, &link.id, None), meta).is_err());
    assert!(open_meta(&LinkKeys::derive(&raw, &link.id, Some("falsch-falsch")), meta).is_err());

    // Kreuztest mit dem Browser-Code (WebCrypto in Node), falls Node vorhanden ist
    if std::process::Command::new("node").arg("--version").output().is_ok() {
        let dir = tmp.path().join("kreuz");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("secret"), &secret).unwrap();
        std::fs::write(dir.join("id"), &link.id).unwrap();
        std::fs::write(dir.join("password"), "korrekt-pferd").unwrap();
        std::fs::write(dir.join("meta.bin"), meta).unwrap();
        std::fs::write(dir.join("data.bin"), data).unwrap();
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateway/cloudflare/test/decrypt.mjs");
        let out = std::process::Command::new("node").arg(&script).arg(&dir).output().unwrap();
        assert!(out.status.success(), "Browser-Code: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(std::fs::read(dir.join("out.bin")).unwrap(), plain, "Browser entschlüsselt identisch");
        let js_meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        assert_eq!(js_meta["name"], "Urlaubsvideo.mp4");
    } else {
        eprintln!("Node nicht gefunden – Kreuztest übersprungen");
    }

    // Standard-Gültigkeit zur Laufzeit ändern; Angabe je Link hat Vorrang
    d.request(Request::SettingSet { key: "link_default_days".into(), value: Some(30) }).await;
    let l30 = match d.request(create(None, None)).await {
        Response::Link(l) => l,
        other => panic!("{other:?}"),
    };
    assert!((l30.expires_at - l30.created_at - 30 * 86_400).abs() < 5);
    let l1 = match d.request(create(None, Some(1))).await {
        Response::Link(l) => l,
        other => panic!("{other:?}"),
    };
    assert!((l1.expires_at - l1.created_at - 86_400).abs() < 5);

    // Ungültige Eingaben
    assert_eq!(code(&d.request(create(Some("kurz"), None)).await), Some(ErrorCode::InvalidInput));
    std::os::unix::fs::symlink(&file, tmp.path().join("link.mp4")).unwrap();
    let sym =
        Request::LinkCreate { path: tmp.path().join("link.mp4"), days: None, max_downloads: None, password: None };
    assert_eq!(code(&d.request(sym).await), Some(ErrorCode::InvalidInput), "Symlinks werden nicht geteilt");
    let dir = Request::LinkCreate { path: tmp.path().to_path_buf(), days: None, max_downloads: None, password: None };
    assert_eq!(code(&d.request(dir).await), Some(ErrorCode::InvalidInput));
    let zero = Request::LinkCreate { path: file.clone(), days: None, max_downloads: Some(0), password: None };
    assert_eq!(code(&d.request(zero).await), Some(ErrorCode::InvalidInput));

    // Widerruf (ID-Präfix) löscht die verschlüsselte Kopie
    let before = store.lock().unwrap().len();
    match d.request(Request::LinkRevoke { id: link.id[..8].into() }).await {
        Response::Link(l) => assert_eq!(l.state, "revoked"),
        other => panic!("{other:?}"),
    }
    assert_eq!(store.lock().unwrap().len(), before - 3);
    assert!(!store.lock().unwrap().contains_key(&key("policy.json")));
    assert_eq!(code(&d.request(Request::LinkRevoke { id: "zz".into() }).await), Some(ErrorCode::InvalidInput));
    match d.request(Request::LinkList).await {
        Response::Links { items, base_url } => {
            assert_eq!(base_url.as_deref(), Some("http://localhost:8787"));
            assert_eq!(items.iter().find(|l| l.id == link.id).unwrap().state, "revoked");
            assert_eq!(items.len(), 3);
        }
        other => panic!("{other:?}"),
    }

    // Speicher lehnt ab (falscher Schlüssel) → Fehler, kein halber Link in der Liste
    d.request(Request::LinkSetup(settings(port, "WRONGKEY"))).await;
    assert_eq!(code(&d.request(create(None, None)).await), Some(ErrorCode::Unavailable));
    match d.request(Request::LinkList).await {
        Response::Links { items, .. } => assert_eq!(items.len(), 3),
        other => panic!("{other:?}"),
    }
    d.shutdown().await;
}
