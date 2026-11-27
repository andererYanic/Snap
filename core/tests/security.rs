//! Sicherheitsübersicht: Rechte, „Alle Dateien“, Tresor, abgewiesene Zugriffe und Hinweise.

use space_core::fileops::archive::Password;
use space_core::ipc::{Request, Response};
use space_core::security::SecurityReport;
use space_core::{Config, Daemon};
use std::path::Path;

async fn start(root: &Path, name: &str) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = None;
    c.connect_timeout_secs = 2;
    c.device_name = name.into();
    c.live_connections = false;
    c.storage_root = Some(root.join("speicher"));
    std::fs::create_dir_all(root.join("speicher/DCIM")).unwrap();
    Daemon::start_loopback(c).await.unwrap()
}

async fn pair(host: &Daemon, guest: &Daemon) {
    let code = match host.request(Request::PairInvite).await {
        Response::PairInvite { code, .. } => code,
        other => panic!("{other:?}"),
    };
    host.confirm_next_pairing();
    assert!(matches!(guest.request(Request::PairJoin { code }).await, Response::Paired { .. }));
}

async fn report(d: &Daemon) -> SecurityReport {
    match d.request(Request::SecurityOverview).await {
        Response::SecurityOverview(r) => r,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn uebersicht_zeigt_rechte_sichtbarkeit_und_auffaelliges() {
    let tmp = tempfile::tempdir().unwrap();
    let a = start(&tmp.path().join("a"), "Laptop").await;
    let b = start(&tmp.path().join("b"), "Handy").await;

    let r = report(&a).await;
    assert!(r.devices.is_empty() && !r.all_files && !r.vault_exists && r.denied_7d == 0);
    assert!(r.hints.iter().any(|h| h.text.contains("Noch kein Gerät")));

    pair(&a, &b).await;
    let folder = tmp.path().join("a/speicher/DCIM");
    let ok = |r: Response| assert!(!matches!(r, Response::Error { .. }), "{r:?}");
    ok(a.request(Request::PairGrant {
        device: "Handy".into(),
        capability: "files.purge".into(),
        folder: Some(folder.clone()),
    })
    .await);
    ok(a.request(Request::ShareAll { enabled: true }).await);
    match a.request(Request::VaultCreate { password: Password("ein-gutes-passwort".into()) }).await {
        Response::VaultCreated { .. } => {}
        other => panic!("{other:?}"),
    }
    // Abgewiesener Zugriff: Handy will die Zwischenablage füllen (ohne Recht)
    let r = b.request(Request::ClipboardSend { device: "Laptop".into(), text: "hallo".into() }).await;
    assert!(matches!(r, Response::Error { .. }));

    let r = report(&a).await;
    assert_eq!(r.devices.len(), 1);
    let d = &r.devices[0];
    assert!(d.strong, "{d:?}");
    assert!(d.rights.iter().any(|x| x.starts_with("files.purge: ")), "{:?}", d.rights);
    assert!(r.all_files && r.vault_exists && r.vault_unlocked);
    assert!(r.denied_7d >= 1, "{}", r.denied_7d);
    let warn: Vec<&str> = r.hints.iter().filter(|h| h.level == "warn").map(|h| h.text.as_str()).collect();
    assert!(warn.iter().any(|t| t.contains("endgültig löschen")), "{warn:?}");
    assert!(warn.iter().any(|t| t.contains("Alle Dateien")), "{warn:?}");
    assert!(warn.iter().any(|t| t.contains("abgewiesene")), "{warn:?}");
    assert!(r.hints.iter().any(|h| h.text.contains("Tresor ist entsperrt")));

    // Für fremde Geräte nicht abrufbar (nur lokal): Anfrage verlangt Admin-Recht
    let cap = space_core::api::auth::required_capability(&Request::SecurityOverview);
    assert_eq!(cap, space_core::api::auth::Capability::Admin);
    a.shutdown().await;
    b.shutdown().await;
}
