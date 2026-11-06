//! Hilfsfunktionen für Dateien und Verzeichnisse mit restriktiven Rechten
//! (Datenordner, Socket-Ordner, Node-Schlüssel).

use anyhow::{bail, Context, Result};
use iroh::SecretKey;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Effektive User-ID des laufenden Prozesses.
pub fn current_uid() -> u32 {
    // SAFETY: geteuid ist immer erfolgreich und hat keine Seiteneffekte.
    unsafe { libc::geteuid() }
}

/// Stellt sicher, dass `dir` existiert, dem aktuellen Benutzer gehört und nur
/// für ihn zugänglich ist (0700). Zu offene Rechte werden korrigiert, fremde
/// Besitzer führen zu einem Fehler.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("Verzeichnis {} nicht anlegbar", dir.display()))?;
    }

    let meta = fs::symlink_metadata(dir).with_context(|| format!("Verzeichnis {} nicht lesbar", dir.display()))?;
    if meta.file_type().is_symlink() {
        bail!("{} ist ein symbolischer Link – aus Sicherheitsgründen nicht erlaubt", dir.display());
    }
    if !meta.is_dir() {
        bail!("{} ist kein Verzeichnis", dir.display());
    }
    if meta.uid() != current_uid() {
        bail!(
            "{} gehört einem anderen Benutzer (uid {}) – aus Sicherheitsgründen abgebrochen",
            dir.display(),
            meta.uid()
        );
    }
    if meta.mode() & 0o077 != 0 {
        tracing::warn!("Rechte von {} waren zu offen ({:o}) – setze 0700", dir.display(), meta.mode() & 0o777);
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Lädt den persistenten Node-Schlüssel oder erzeugt beim ersten Start einen neuen.
/// Damit bleibt die Node-ID (und damit alle Tickets) über Neustarts hinweg gültig.
pub fn load_or_create_secret_key(path: &Path) -> Result<SecretKey> {
    match OpenOptions::new().read(true).open(path) {
        Ok(file) => {
            let meta = file.metadata()?;
            if meta.uid() != current_uid() {
                bail!("Schlüsseldatei {} gehört einem anderen Benutzer", path.display());
            }
            if meta.mode() & 0o077 != 0 {
                tracing::warn!(
                    "Schlüsseldatei {} war für andere lesbar ({:o}) – setze 0600. \
                     Falls andere Benutzer Zugriff hatten, Schlüssel löschen und neu erzeugen lassen.",
                    path.display(),
                    meta.mode() & 0o777
                );
                fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            }
            let mut bytes = Vec::with_capacity(32);
            file.take(33).read_to_end(&mut bytes)?;
            let bytes: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("Schlüsseldatei {} ist beschädigt (erwartet 32 Byte)", path.display()))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
                .with_context(|| format!("Schlüsseldatei {} nicht anlegbar", path.display()))?;
            file.write_all(&key.to_bytes())?;
            file.sync_all()?;
            tracing::info!("Neuer Node-Schlüssel erzeugt: {}", path.display());
            Ok(key)
        }
        Err(e) => Err(e).with_context(|| format!("Schlüsseldatei {} nicht lesbar", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_roundtrip_and_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        let k1 = load_or_create_secret_key(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let k2 = load_or_create_secret_key(&path).unwrap();
        assert_eq!(k1.public(), k2.public());
    }

    #[test]
    fn too_open_key_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        load_or_create_secret_key(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        load_or_create_secret_key(&path).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn corrupt_key_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.key");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(b"short")
            .unwrap();
        assert!(load_or_create_secret_key(&path).is_err());
    }

    #[test]
    fn private_dir_is_created_and_tightened() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a/b");
        ensure_private_dir(&sub).unwrap();
        assert_eq!(fs::metadata(&sub).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&sub).unwrap();
        assert_eq!(fs::metadata(&sub).unwrap().mode() & 0o777, 0o700);
    }
}
