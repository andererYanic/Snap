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
}
