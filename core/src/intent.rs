//! Adressen, mit denen andere Android-Apps Snap Dateien oder Ordner zeigen (`ACTION_VIEW`,
//! „Öffnen mit“, „Ordner öffnen“) bzw. die Snap als Dateiauswahl zurückgibt.
//!
//! Alles hier ist Eingabe von außen (beliebige Apps):
//! - [`uri_to_path`] übersetzt nur bekannte Formen in einen Pfad-Kandidaten: `file://` und die
//!   Dokument-Adressen des Systemspeichers (`com.android.externalstorage.documents`) bzw. des
//!   Download-Ordners (`com.android.providers.downloads.documents`). Alles andere → `None`
//!   (die App kopiert solche Inhalte dann in einen eigenen Cache, statt Pfade zu raten).
//! - [`checked`] löst den Kandidaten auf (Symlinks, `..`) und lässt nur Pfade im gemeinsamen
//!   Speicher zu – nie Snaps eigene Ordner (Datenordner mit Schlüsseln, Datenbank, entschlüsselten
//!   Tresor-Dateien). So kann keine App Snap dazu bringen, Privates anzuzeigen oder herauszugeben.

use std::path::{Path, PathBuf};

const EXTERNAL_STORAGE: &str = "com.android.externalstorage.documents";
const DOWNLOADS: &str = "com.android.providers.downloads.documents";
/// Höchstlänge einer Adresse (länger ist nie echt).
pub const MAX_URI: usize = 8192;

/// Was erwartet wird.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

/// Übersetzt eine Adresse in einen Pfad-Kandidaten (noch ungeprüft). `storage_root` ist der
/// gemeinsame Speicher des Nutzers (Android: `/storage/emulated/<Nutzer>`).
pub fn uri_to_path(uri: &str, storage_root: &Path) -> Option<PathBuf> {
    if uri.len() > MAX_URI {
        return None;
    }
    if let Some(rest) = uri.strip_prefix("file://") {
        // file:///pfad (ohne Rechner); Anfrage- und Fragmentteile gibt es bei Dateien nicht
        if !rest.starts_with('/') || rest.contains(['?', '#']) {
            return None;
        }
        return absolute(&decode(rest)?);
    }
    let rest = uri.strip_prefix("content://")?;
    let (authority, path) = rest.split_once('/')?;
    if path.contains(['?', '#']) {
        return None;
    }
    let parts: Vec<&str> = path.split('/').collect();
    // …/document/ID, …/tree/ID oder …/tree/ID/document/ID2 (dann gilt ID2)
    let id = match parts.as_slice() {
        ["document", id] | ["tree", id] => *id,
        ["tree", _, "document", id] => *id,
        _ => return None,
    };
    let id = decode(id)?;
    match authority {
        EXTERNAL_STORAGE => {
            let (volume, rel) = id.split_once(':')?;
            let base = match volume {
                "primary" => storage_root.to_path_buf(),
                "home" => storage_root.join("Documents"),
                v if is_volume_id(v) => Path::new("/storage").join(v),
                _ => return None,
            };
            let rel = relative(rel)?;
            Some(if rel.as_os_str().is_empty() { base } else { base.join(rel) })
        }
        DOWNLOADS => match id.as_str() {
            "downloads" => Some(storage_root.join("Download")),
            other => absolute(other.strip_prefix("raw:")?),
        },
        _ => None,
    }
}

/// Prüft einen Pfad: aufgelöst (Symlinks, `..`), vorhanden, von der erwarteten Art, im
/// gemeinsamen Speicher (`storage_root` oder ein Speicher unter `/storage`) und nicht in
/// `own` (Snaps eigene Ordner). Gibt den aufgelösten Pfad zurück.
pub fn checked(path: &Path, kind: Kind, storage_root: &Path, own: &[PathBuf]) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&real).ok()?;
    let ok_kind = match kind {
        Kind::File => meta.is_file(),
        Kind::Dir => meta.is_dir(),
    };
    if !ok_kind {
        return None;
    }
    let root = std::fs::canonicalize(storage_root).unwrap_or_else(|_| storage_root.to_path_buf());
    let in_storage = real.starts_with(&root) || (real.starts_with("/storage") && real != Path::new("/storage"));
    if !in_storage {
        return None;
    }
    let mine = own.iter().any(|o| {
        let o = std::fs::canonicalize(o).unwrap_or_else(|_| o.clone());
        real.starts_with(&o)
    });
    (!mine).then_some(real)
}

/// Adresse übersetzen und prüfen in einem Schritt.
pub fn resolve(uri: &str, kind: Kind, storage_root: &Path, own: &[PathBuf]) -> Option<PathBuf> {
    checked(&uri_to_path(uri, storage_root)?, kind, storage_root, own)
}

fn decode(s: &str) -> Option<String> {
    let bytes = crate::fileops::trash::percent_decode(s)?;
    let s = String::from_utf8(bytes).ok()?;
    (!s.contains('\0')).then_some(s)
}

/// Absoluter Pfad ohne `..`, `.` oder leere Teile (ein abschließender `/` ist erlaubt).
fn absolute(p: &str) -> Option<PathBuf> {
    let body = p.strip_prefix('/')?;
    let body = body.strip_suffix('/').unwrap_or(body);
    if body.is_empty() || !segments_ok(body) {
        return None;
    }
    Some(Path::new("/").join(body))
}

/// Relativer Pfad ohne `..`, `.` oder leere Teile (darf leer sein = Wurzel des Speichers).
fn relative(p: &str) -> Option<PathBuf> {
    let p = p.trim_start_matches('/');
    let p = p.strip_suffix('/').unwrap_or(p);
    if p.is_empty() {
        return Some(PathBuf::new());
    }
    segments_ok(p).then(|| PathBuf::from(p))
}

fn segments_ok(p: &str) -> bool {
    p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Kennung einer SD-Karte bzw. eines USB-Speichers, z. B. `1A2B-3C4D`.
fn is_volume_id(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 9
        && b[4] == b'-'
        && b.iter().enumerate().all(|(i, c)| i == 4 || c.is_ascii_digit() || (b'A'..=b'F').contains(c))
}

/// Kennung eines Eintrags der Dokument-Quelle „Snap“ in der System-Dateiauswahl
/// (`SnapDocumentsProvider`). Kennungen kommen von der Systemoberfläche zurück und gelten als
/// Eingabe von außen: [`doc_id_decode`] nimmt nur genau die Formen an, die [`doc_id_encode`]
/// erzeugt, mit geprüften Teilen (Node-ID und Hash hexadezimal, Pfade ohne `.`/`..`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocId {
    Root,
    Devices,
    /// Datei oder Ordner im Snap-Drop-Empfangsordner (relativ, leer = der Ordner selbst).
    Drop(String),
    /// Wurzel eines gekoppelten Geräts (Freigaben).
    Device(String),
    /// Ordner in einer Freigabe eines Geräts (`dir` relativ, leer = Wurzel der Freigabe).
    Dir {
        node: String,
        share: String,
        dir: String,
    },
    /// Datei eines Geräts (Inhalts-Hash und Anzeigename).
    RemoteFile {
        node: String,
        hash: String,
        name: String,
    },
}

const MAX_DOC_ID: usize = 4096;

fn enc(s: &str) -> String {
    crate::fileops::trash::percent_encode(s.as_bytes()).replace('/', "%2F")
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && !s.contains('/') && s != "." && s != ".."
}

fn safe_rel_or_empty(s: &str) -> bool {
    s.is_empty() || (s.len() < 2048 && segments_ok(s))
}

/// Prüft die Teile einer Kennung (für beide Richtungen).
pub fn doc_id_valid(id: &DocId) -> bool {
    match id {
        DocId::Root | DocId::Devices => true,
        DocId::Drop(rel) => safe_rel_or_empty(rel),
        DocId::Device(node) => is_hex64(node),
        DocId::Dir { node, share, dir } => {
            is_hex64(node) && !share.is_empty() && share.len() < 2048 && segments_ok(share) && safe_rel_or_empty(dir)
        }
        DocId::RemoteFile { node, hash, name } => is_hex64(node) && is_hex64(hash) && safe_name(name),
    }
}

/// Kennung erzeugen. `None`, wenn ein Teil ungültig ist (z. B. ein fremder Name mit `/`).
pub fn doc_id_encode(id: &DocId) -> Option<String> {
    if !doc_id_valid(id) {
        return None;
    }
    let s = match id {
        DocId::Root => "root".to_string(),
        DocId::Devices => "devices".to_string(),
        DocId::Drop(rel) if rel.is_empty() => "drop".to_string(),
        DocId::Drop(rel) => format!("drop/{}", enc(rel)),
        DocId::Device(node) => format!("dev/{node}"),
        DocId::Dir { node, share, dir } => format!("dir/{node}/{}/{}", enc(share), enc(dir)),
        DocId::RemoteFile { node, hash, name } => format!("file/{node}/{hash}/{}", enc(name)),
    };
    (s.len() <= MAX_DOC_ID).then_some(s)
}

/// Kennung zerlegen (streng; Unbekanntes → `None`).
pub fn doc_id_decode(raw: &str) -> Option<DocId> {
    if raw.len() > MAX_DOC_ID {
        return None;
    }
    let p: Vec<&str> = raw.split('/').collect();
    let id = match p.as_slice() {
        ["root"] => DocId::Root,
        ["devices"] => DocId::Devices,
        ["drop"] => DocId::Drop(String::new()),
        ["drop", rel] if !rel.is_empty() => DocId::Drop(decode(rel)?),
        ["dev", node] => DocId::Device(node.to_string()),
        ["dir", node, share, dir] => DocId::Dir { node: node.to_string(), share: decode(share)?, dir: decode(dir)? },
        ["file", node, hash, name] => {
            DocId::RemoteFile { node: node.to_string(), hash: hash.to_string(), name: decode(name)? }
        }
        _ => return None,
    };
    // Nur kanonische Form: erneut erzeugt muss genau dieselbe Kennung herauskommen
    (doc_id_valid(&id) && doc_id_encode(&id).as_deref() == Some(raw)).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/storage/emulated/0";

    fn p(uri: &str) -> Option<String> {
        uri_to_path(uri, Path::new(ROOT)).map(|p| p.to_string_lossy().into_owned())
    }

    #[test]
    fn bekannte_adressen() {
        assert_eq!(
            p("file:///storage/emulated/0/Download/a%20b.pdf").as_deref(),
            Some("/storage/emulated/0/Download/a b.pdf")
        );
        let ext = "content://com.android.externalstorage.documents";
        assert_eq!(p(&format!("{ext}/tree/primary%3ADCIM")).as_deref(), Some("/storage/emulated/0/DCIM"));
        assert_eq!(p(&format!("{ext}/tree/primary%3A")).as_deref(), Some("/storage/emulated/0"));
        assert_eq!(
            p(&format!("{ext}/tree/primary%3ADCIM/document/primary%3ADCIM%2FCamera")).as_deref(),
            Some("/storage/emulated/0/DCIM/Camera")
        );
        assert_eq!(p(&format!("{ext}/document/1A2B-3C4D%3AMusik")).as_deref(), Some("/storage/1A2B-3C4D/Musik"));
        assert_eq!(
            p(&format!("{ext}/document/home%3ABrief.odt")).as_deref(),
            Some("/storage/emulated/0/Documents/Brief.odt")
        );
        let dl = "content://com.android.providers.downloads.documents/document";
        assert_eq!(p(&format!("{dl}/downloads")).as_deref(), Some("/storage/emulated/0/Download"));
        assert_eq!(
            p(&format!("{dl}/raw%3A%2Fstorage%2Femulated%2F0%2FDownload%2Fx.zip")).as_deref(),
            Some("/storage/emulated/0/Download/x.zip")
        );
    }

    #[test]
    fn fremde_und_manipulierte_adressen() {
        let ext = "content://com.android.externalstorage.documents";
        for bad in [
            "",
            "file://",
            "file://rechner/etc/passwd",
            "file:///storage/emulated/0/../../../data/data/de.snap.mobile/files/snap/node.key",
            "file:///storage/emulated/0/./x",
            "file:///storage/emulated/0/x?y",
            "file:///storage/emulated/0/a%00b",
            "file:///storage/emulated/0/%ZZ",
            "http://example.org/x",
            "content://de.snap.mobile.files/speicher/x",
            "content://media/external/images/media/12",
            "content://com.android.providers.downloads.documents/document/msf%3A123",
            "content://com.android.providers.downloads.documents/document/raw%3Arelativ",
            "content://com.android.providers.downloads.documents/document/raw%3A%2Fa%2F..%2Fb",
            &format!("{ext}/tree/primary%3A..%2F..%2Fdata"),
            &format!("{ext}/document/primary%3ADCIM%2F..%2F..%2F.."),
            &format!("{ext}/document/fremd%3Ax"),
            &format!("{ext}/document/1a2b-3c4d%3Ax"),
            &format!("{ext}/document/../primary%3Ax"),
            &format!("{ext}/document/primary%3Ax/zuviel"),
            &format!("{ext}/document/primary%3Ax?y=1"),
        ] {
            assert_eq!(p(bad), None, "{bad}");
        }
        assert_eq!(p(&format!("file:///{}", "a/".repeat(5000))), None, "zu lang");
        // Fuzzing-Fund: kurze raw:-Adresse, deren Pfad neu kodiert die Höchstlänge übersteigt –
        // erlaubt, aber als file:// zu lang (wird abgewiesen statt abgeschnitten)
        // Steuerzeichen unkodiert (wie im Fund), nur „/“ kodiert: 5 Zeichen je Teil statt 7 als file://
        let long = format!("/{}", "\u{f}:/".repeat(1500));
        let id = format!("raw%3A{}", long.replace('/', "%2F"));
        let raw = format!("content://com.android.providers.downloads.documents/document/{id}");
        assert!(raw.len() <= MAX_URI);
        let path = p(&raw).expect("gültiger Pfad");
        let again = format!("file://{}", crate::fileops::trash::percent_encode(path.as_bytes()));
        assert!(again.len() > MAX_URI);
        assert_eq!(p(&again), None);
    }

    #[test]
    fn nur_gemeinsamer_speicher_nie_eigene_ordner() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("speicher");
        let own_ext = storage.join("Android/data/de.snap.mobile");
        let private = dir.path().join("privat");
        std::fs::create_dir_all(storage.join("Download")).unwrap();
        std::fs::create_dir_all(&own_ext).unwrap();
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(storage.join("Download/a.pdf"), b"%PDF").unwrap();
        std::fs::write(own_ext.join("cache.bin"), b"x").unwrap();
        std::fs::write(private.join("node.key"), b"geheim").unwrap();
        std::os::unix::fs::symlink(private.join("node.key"), storage.join("Download/link.pdf")).unwrap();
        std::os::unix::fs::symlink(&private, storage.join("Download/ordner")).unwrap();
        let own = vec![private.clone(), own_ext.clone()];
        let ok = |p: &Path, k| checked(p, k, &storage, &own);

        assert!(ok(&storage.join("Download/a.pdf"), Kind::File).is_some());
        assert!(ok(&storage.join("Download"), Kind::Dir).is_some());
        assert!(ok(&storage, Kind::Dir).is_some());
        // Falsche Art, fehlend
        assert!(ok(&storage.join("Download"), Kind::File).is_none());
        assert!(ok(&storage.join("Download/a.pdf"), Kind::Dir).is_none());
        assert!(ok(&storage.join("fehlt.pdf"), Kind::File).is_none());
        // Privates: direkt, über Symlink, über `..`, eigene Ordner im Speicher
        assert!(ok(&private.join("node.key"), Kind::File).is_none());
        assert!(ok(&storage.join("Download/link.pdf"), Kind::File).is_none(), "Symlink nach privat");
        assert!(ok(&storage.join("Download/ordner"), Kind::Dir).is_none());
        assert!(ok(&storage.join("Download/../../privat/node.key"), Kind::File).is_none());
        assert!(ok(&own_ext.join("cache.bin"), Kind::File).is_none(), "Snaps Ordner im Speicher");
        assert!(ok(Path::new("/etc/passwd"), Kind::File).is_none());
        assert!(ok(Path::new("/storage"), Kind::Dir).is_none());
        // In einem Schritt
        let uri = format!("file://{}", storage.join("Download/a.pdf").display()).replace(' ', "%20");
        assert!(resolve(&uri, Kind::File, &storage, &own).is_some());
    }

    #[test]
    fn dokument_kennungen() {
        let node = "a".repeat(64);
        let hash = "0123456789abcdef".repeat(4);
        let ids = [
            DocId::Root,
            DocId::Devices,
            DocId::Drop(String::new()),
            DocId::Drop("Laptop/Urlaub 2024/foto #1.jpg".into()),
            DocId::Device(node.clone()),
            DocId::Dir { node: node.clone(), share: "Fotos".into(), dir: String::new() },
            DocId::Dir { node: node.clone(), share: "Daten/Projekt".into(), dir: "a b/c%d".into() },
            DocId::RemoteFile { node: node.clone(), hash: hash.clone(), name: "Brief (1).pdf".into() },
        ];
        for id in &ids {
            let s = doc_id_encode(id).expect("gültig");
            assert_eq!(doc_id_decode(&s).as_ref(), Some(id), "{s}");
        }
        // Ungültige Teile werden gar nicht erst erzeugt
        for bad in [
            DocId::Drop("../privat".into()),
            DocId::Drop("a/../../b".into()),
            DocId::Drop("/absolut".into()),
            DocId::Device("ABC".into()),
            DocId::Device("g".repeat(64)),
            DocId::Dir { node: node.clone(), share: String::new(), dir: String::new() },
            DocId::Dir { node: node.clone(), share: "..".into(), dir: String::new() },
            DocId::Dir { node: node.clone(), share: "S".into(), dir: "x/./y".into() },
            DocId::RemoteFile { node: node.clone(), hash: "kurz".into(), name: "a".into() },
            DocId::RemoteFile { node: node.clone(), hash: hash.clone(), name: "a/b".into() },
            DocId::RemoteFile { node: node.clone(), hash: hash.clone(), name: "..".into() },
            DocId::RemoteFile { node: node.clone(), hash: hash.clone(), name: "x".repeat(256) },
        ] {
            assert_eq!(doc_id_encode(&bad), None, "{bad:?}");
        }
        // Manipulierte Kennungen
        for raw in [
            "",
            "root/",
            "Root",
            "drop/",
            "drop/..%2Fprivat",
            "drop/a%2F..%2F..%2Fb",
            "drop/%2Fetc%2Fpasswd",
            "drop/a/b",
            "drop/a%00b",
            "dev/ABC",
            "dev/../x",
            &format!("dev/{node}/zuviel"),
            &format!("dir/{node}//"),
            &format!("dir/{node}/..%2F../x"),
            &format!("file/{node}/{hash}/a%2Fb"),
            &format!("file/{node}/{hash}/"),
            &format!("file/{node}/{}/x", hash.to_uppercase()),
            &format!("file/{node}/{hash}/%61"), // nicht kanonisch kodiert
            &format!("drop/{}", "a".repeat(5000)),
        ] {
            assert_eq!(doc_id_decode(raw), None, "{raw}");
        }
    }
}
