//! Tresor über die API: Anlegen, Aufnehmen, Öffnen, Sperren, falsche Passwörter (Drosselung),
//! Wiederherstellungscode, Manipulation auf der Platte, keine Vorschaubilder, keine Passwörter in
//! Protokollen.

use space_core::fileops::archive::Password;
use space_core::ipc::{Request, Response};
use space_core::{Config, Daemon};
use std::path::Path;

async fn start(root: &Path) -> Daemon {
    let mut c = Config::portable(root);
    c.use_relay = false;
    c.bind_port = None;
    Daemon::start_loopback(c).await.unwrap()
}

fn pw(s: &str) -> Password {
    Password(s.into())
}

fn ok(r: &Response) {
    assert!(!matches!(r, Response::Error { .. }), "{r:?}");
}

async fn entries(d: &Daemon) -> Vec<space_core::vault::VaultEntry> {
    match d.request(Request::VaultList).await {
        Response::VaultEntries { entries } => entries,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tresor_von_anfang_bis_ende() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(&tmp.path().join("d")).await;
    let docs = tmp.path().join("Dokumente");
    std::fs::create_dir_all(docs.join("Ausweise")).unwrap();
    std::fs::write(docs.join("Ausweise/pass.pdf"), b"%PDF geheim").unwrap();
    std::fs::write(docs.join("steuer.txt"), vec![7u8; 200_000]).unwrap();

    // Anlegen: Wiederherstellungscode genau einmal
    let code = match d.request(Request::VaultCreate { password: pw("kurz") }).await {
        Response::Error { .. } => match d.request(Request::VaultCreate { password: pw("richtig-gutes-pw") }).await {
            Response::VaultCreated { recovery_code } => recovery_code,
            other => panic!("{other:?}"),
        },
        other => panic!("zu kurzes Passwort angenommen: {other:?}"),
    };
    assert!(matches!(
        d.request(Request::VaultCreate { password: pw("noch-ein-tresor") }).await,
        Response::Error { .. }
    ));

    // Aufnehmen (Ordner rekursiv), Originale entfernen
    ok(&d
        .request(Request::VaultAdd { paths: vec![docs.join("Ausweise"), docs.join("steuer.txt")], remove: true })
        .await);
    assert!(!docs.join("steuer.txt").exists() && !docs.join("Ausweise/pass.pdf").exists());
    let list = entries(&d).await;
    let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["Ausweise/pass.pdf", "steuer.txt"]);
    // Auf der Platte steht nichts lesbar (weder Inhalt noch Namen)
    let vault_dir = d.config().data_dir.join("vault");
    for e in walk(&vault_dir) {
        let data = std::fs::read(&e).unwrap();
        assert!(
            !contains(&data, b"geheim") && !contains(&data, b"pass.pdf") && !contains(&data, b"steuer"),
            "{}",
            e.display()
        );
    }

    // Öffnen: Inhalt stimmt, keine Vorschaubilder
    let pass = list.iter().find(|e| e.name.ends_with("pass.pdf")).unwrap().clone();
    let opened = match d.request(Request::VaultOpen { id: pass.id.clone() }).await {
        Response::VaultPath { path } => path,
        other => panic!("{other:?}"),
    };
    assert_eq!(std::fs::read(&opened).unwrap(), b"%PDF geheim");
    assert!(matches!(
        d.request(Request::Thumbnail { path: opened.clone(), size: Default::default() }).await,
        Response::Error { .. }
    ));
    // Herausholen: bleibt im Tresor
    let out = tmp.path().join("raus");
    std::fs::create_dir_all(&out).unwrap();
    ok(&d.request(Request::VaultExport { id: pass.id.clone(), dest_dir: out.clone() }).await);
    assert_eq!(std::fs::read(out.join("pass.pdf")).unwrap(), b"%PDF geheim");
    assert_eq!(entries(&d).await.len(), 2);

    // Sperren: Liste verweigert, geöffnete Datei weg
    ok(&d.request(Request::VaultLock).await);
    assert!(!opened.exists());
    assert!(matches!(d.request(Request::VaultList).await, Response::Error { .. }));
    assert!(matches!(d.request(Request::VaultOpen { id: pass.id.clone() }).await, Response::Error { .. }));

    // Falsches Passwort, nach 5 Versuchen Wartezeit
    for _ in 0..5 {
        assert!(matches!(
            d.request(Request::VaultUnlock { password: pw("falsch-falsch") }).await,
            Response::Error { .. }
        ));
    }
    match d.request(Request::VaultUnlock { password: pw("richtig-gutes-pw") }).await {
        Response::Error { message, .. } => assert!(message.contains("warten"), "{message}"),
        other => panic!("Drosselung fehlt: {other:?}"),
    }
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    ok(&d.request(Request::VaultUnlock { password: pw("richtig-gutes-pw") }).await);

    // Passwort vergessen: mit dem Code ein neues setzen
    ok(&d.request(Request::VaultLock).await);
    assert!(matches!(
        d.request(Request::VaultRecover { code: pw("AAAA-BBBB-CCCC"), new_password: pw("neues-passwort") }).await,
        Response::Error { .. }
    ));
    ok(&d.request(Request::VaultRecover { code: pw(&code.to_lowercase()), new_password: pw("neues-passwort") }).await);
    assert_eq!(entries(&d).await.len(), 2);
    ok(&d.request(Request::VaultLock).await);
    assert!(matches!(
        d.request(Request::VaultUnlock { password: pw("richtig-gutes-pw") }).await,
        Response::Error { .. }
    ));
    ok(&d.request(Request::VaultUnlock { password: pw("neues-passwort") }).await);

    // Manipulation auf der Platte wird erkannt, nichts Halbes bleibt liegen
    let steuer = entries(&d).await.into_iter().find(|e| e.name == "steuer.txt").unwrap();
    let obj = vault_dir.join("objects").join(&steuer.id);
    let mut data = std::fs::read(&obj).unwrap();
    data[1000] ^= 0x40;
    std::fs::write(&obj, &data).unwrap();
    assert!(matches!(
        d.request(Request::VaultExport { id: steuer.id.clone(), dest_dir: out.clone() }).await,
        Response::Error { .. }
    ));
    assert!(!out.join("steuer.txt").exists());
    assert!(std::fs::read_dir(&out).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().starts_with('.')));

    // Löschen und Kennungen von außen
    ok(&d.request(Request::VaultRemove { id: steuer.id.clone() }).await);
    assert!(!obj.exists());
    assert!(matches!(d.request(Request::VaultOpen { id: "../../vault.json".into() }).await, Response::Error { .. }));
    d.shutdown().await;
}

#[test]
fn passwoerter_nie_in_protokollen() {
    let r = Request::VaultRecover { code: pw("CODE-GEHEIM"), new_password: pw("pw-geheim") };
    let s = format!("{r:?}");
    assert!(!s.contains("GEHEIM") && !s.contains("pw-geheim"), "{s}");
}

fn walk(p: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
