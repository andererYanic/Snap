//! PDFs markieren, signieren und prüfen über den Daemon – mit Angriffstests: Pfadtricks,
//! falsches Passwort, manipulierte ID-Dateien, nichts wird überschrieben, Passwort nie im Log.

use space_core::api::error::ErrorCode;
use space_core::fileops::archive::Password;
use space_core::ipc::{PdfLayer, Request, Response};
use space_core::pdfsign::Placement;
use space_core::{Config, Daemon};
use std::path::{Path, PathBuf};

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

fn sample_pdf() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 595 842] >>",
        "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << >> >>",
        "<< /Length 2 >>\nstream\nq\nendstream",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let x = out.len();
    out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n").as_bytes());
    out
}

fn written(r: Response) -> PathBuf {
    match r {
        Response::PdfWritten { path } => path,
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mark_sign_verify_and_attacks() {
    let tmp = tempfile::tempdir().unwrap();
    let d = start(&tmp.path().join("d")).await;
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let pdf = docs.join("Vertrag.pdf");
    std::fs::write(&pdf, sample_pdf()).unwrap();
    let pw = |s: &str| Password(s.into());

    // Ohne ID: signieren geht nicht
    let place = Placement { page: 0, strokes: vec![vec![(0.6, 0.8), (0.75, 0.82)]], width: 0.004 };
    let r = d
        .request(Request::PdfSign {
            path: pdf.clone(),
            placement: place.clone(),
            password: pw("geheim-123"),
            mode: "copy".into(),
        })
        .await;
    assert!(code(&r).is_some(), "{r:?}");

    // ID anlegen (Passwort zu kurz → abgelehnt)
    assert_eq!(
        code(&d.request(Request::SignIdCreate { name: "Max".into(), password: pw("kurz") }).await),
        Some(ErrorCode::InvalidInput)
    );
    let r = d.request(Request::SignIdCreate { name: "Max Muster".into(), password: pw("geheim-123") }).await;
    let fp = match r {
        Response::SignId { id: Some(i), .. } => i.fingerprint,
        other => panic!("{other:?}"),
    };
    // Zweite ID überschreibt nie die erste
    assert_eq!(
        code(&d.request(Request::SignIdCreate { name: "Eva".into(), password: pw("geheim-123") }).await),
        Some(ErrorCode::AlreadyExists)
    );

    // Falsches Passwort
    let r = d
        .request(Request::PdfSign {
            path: pdf.clone(),
            placement: place.clone(),
            password: pw("falsch-123"),
            mode: "copy".into(),
        })
        .await;
    assert_eq!(code(&r), Some(ErrorCode::PasswordRequired), "{r:?}");
    // Pfadtricks und kaputte Eingaben
    for bad in [PathBuf::from("relativ.pdf"), docs.join("../docs/Vertrag.pdf"), docs.clone(), docs.join("fehlt.pdf")] {
        let r = d
            .request(Request::PdfSign {
                path: bad.clone(),
                placement: place.clone(),
                password: pw("geheim-123"),
                mode: "copy".into(),
            })
            .await;
        assert!(code(&r).is_some(), "{bad:?}: {r:?}");
    }
    let r = d
        .request(Request::PdfSign {
            path: pdf.clone(),
            placement: place.clone(),
            password: pw("geheim-123"),
            mode: "../x".into(),
        })
        .await;
    assert_eq!(code(&r), Some(ErrorCode::InvalidInput));
    let bad_place = Placement { page: 0, strokes: vec![vec![(f32::NAN, 0.5)]], width: 0.004 };
    let r = d
        .request(Request::PdfSign {
            path: pdf.clone(),
            placement: bad_place,
            password: pw("geheim-123"),
            mode: "copy".into(),
        })
        .await;
    assert_eq!(code(&r), Some(ErrorCode::InvalidInput));

    // Signieren: Kopie daneben, Original unverändert, nichts überschrieben
    let signed = written(
        d.request(Request::PdfSign {
            path: pdf.clone(),
            placement: place.clone(),
            password: pw("geheim-123"),
            mode: "copy".into(),
        })
        .await,
    );
    assert_eq!(signed, docs.join("Vertrag (signiert).pdf"));
    assert_eq!(std::fs::read(&pdf).unwrap(), sample_pdf());
    let signed2 = written(
        d.request(Request::PdfSign {
            path: pdf.clone(),
            placement: place.clone(),
            password: pw("geheim-123"),
            mode: "copy".into(),
        })
        .await,
    );
    assert_eq!(signed2, docs.join("Vertrag (signiert 2).pdf"));

    // Prüfen: gültig, eigene ID
    let items = match d.request(Request::PdfVerify { path: signed.clone() }).await {
        Response::PdfSignatures { items } => items,
        other => panic!("{other:?}"),
    };
    assert_eq!(items.len(), 1);
    assert!(items[0].valid && items[0].covers_document && items[0].own, "{items:?}");
    assert_eq!(items[0].fingerprint, fp);

    // Textmarker auf das signierte PDF: Signatur bleibt gültig, deckt aber nicht mehr alles ab
    let layer = PdfLayer { page: 0, strokes: vec![vec![(0.1, 0.2), (0.6, 0.2)]], color: "#ffe14d".into(), width: 0.02 };
    let marked = written(
        d.request(Request::PdfMark { path: signed.clone(), layers: vec![layer.clone()], mode: "copy".into() }).await,
    );
    let items = match d.request(Request::PdfVerify { path: marked }).await {
        Response::PdfSignatures { items } => items,
        other => panic!("{other:?}"),
    };
    assert!(items[0].valid && !items[0].covers_document, "{items:?}");
    let bad_color = PdfLayer { color: "rot".into(), ..layer.clone() };
    assert_eq!(
        code(&d.request(Request::PdfMark { path: pdf.clone(), layers: vec![bad_color], mode: "copy".into() }).await),
        Some(ErrorCode::InvalidInput)
    );
    // Keine PDF-Datei
    let txt = docs.join("notiz.pdf");
    std::fs::write(&txt, b"kein pdf").unwrap();
    assert_eq!(code(&d.request(Request::PdfVerify { path: txt }).await), Some(ErrorCode::InvalidInput));

    // Export (verschlüsselt) und manipulierte Importe
    let exported = written(d.request(Request::SignIdExport { dest: docs.clone() }).await);
    let text = std::fs::read_to_string(&exported).unwrap();
    assert!(!text.contains("geheim-123") && text.contains("argon2id"));
    assert_eq!(
        code(&d.request(Request::SignIdImport { path: exported.clone() }).await),
        Some(ErrorCode::AlreadyExists)
    );
    assert!(matches!(d.request(Request::SignIdDelete).await, Response::SignId { id: None, .. }));
    let mut evil: serde_json::Value = serde_json::from_str(&text).unwrap();
    evil["kdf"]["m_kib"] = serde_json::json!(4_000_000);
    let evil_path = docs.join("boese.snapid");
    std::fs::write(&evil_path, evil.to_string()).unwrap();
    // Import prüft das Format; die überhöhten Kosten werden spätestens beim Entsperren abgelehnt
    let _ = d.request(Request::SignIdImport { path: evil_path }).await;
    let r = d
        .request(Request::PdfSign {
            path: pdf.clone(),
            placement: place.clone(),
            password: pw("geheim-123"),
            mode: "copy".into(),
        })
        .await;
    assert!(code(&r).is_some(), "manipulierte Kosten: {r:?}");
    let _ = d.request(Request::SignIdDelete).await;
    assert!(matches!(d.request(Request::SignIdImport { path: exported }).await, Response::SignId { id: Some(_), .. }));

    // Passwort taucht nicht im Audit-Log auf
    let audit = format!("{:?}", d.request(Request::Audit { limit: 1000 }).await);
    assert!(!audit.contains("geheim-123") && !audit.contains("falsch-123"));
    d.shutdown().await;
}
