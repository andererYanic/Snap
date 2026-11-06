//! Sicherungsablage („Repository“): Schlüssel, verschlüsselte Objekte, Speicherziele, Katalog.
//! Format: `docs/BACKUP_FORMAT.md`.

use crate::api_bail;
use crate::links::s3::{S3Client, S3Config};
use anyhow::{Context, Result};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::hmac;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

pub const FORMAT: u32 = 1;
const PBKDF2_ROUNDS: u32 = 600_000;
const B64: data_encoding::Encoding = data_encoding::BASE64;

fn rand<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("Zufallszahlen des Betriebssystems: {e}"))?;
    Ok(b)
}

/// Wohin gesichert wird.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum BackupTarget {
    /// Ordner, z. B. auf einer externen Platte.
    Dir { path: PathBuf },
    /// S3-kompatibler Speicher (Cloudflare R2) – Zugang aus `links.json`, eigener Bereich.
    S3,
}

/// Speicherziel mit einfachen Objektoperationen.
pub enum Storage {
    Local(PathBuf),
    S3 { client: S3Client, prefix: String },
}

/// Objektnamen sind vom Programm erzeugt; trotzdem streng prüfen (keine Pfadtricks).
fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && name.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b));
    if !ok {
        api_bail!(InvalidInput, "Ungültiger Objektname");
    }
    Ok(())
}

impl Storage {
    pub fn open(target: &BackupTarget, repo_id: &str, s3: Option<S3Config>) -> Result<Self> {
        match target {
            BackupTarget::Dir { path } => {
                if !path.is_absolute() {
                    api_bail!(InvalidInput, "Zielordner muss absolut sein.");
                }
                Ok(Storage::Local(path.join(format!("snap-backup-{repo_id}"))))
            }
            BackupTarget::S3 => {
                let Some(cfg) = s3 else {
                    api_bail!(Unavailable, "Kein S3/R2-Zugang eingerichtet (space-daemon links setup …).");
                };
                Ok(Storage::S3 { client: S3Client::new(cfg)?, prefix: format!("backups/{repo_id}/") })
            }
        }
    }

    /// Ist das Ziel erreichbar (z. B. Laufwerk eingehängt)?
    pub fn available(&self) -> bool {
        match self {
            Storage::Local(p) => p.parent().is_some_and(|d| d.is_dir()),
            Storage::S3 { .. } => true,
        }
    }

    pub async fn put(&self, name: &str, data: Vec<u8>) -> Result<()> {
        check_name(name)?;
        match self {
            Storage::Local(root) => {
                let path = root.join(name);
                let parent = path.parent().context("Pfad")?.to_path_buf();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    std::fs::create_dir_all(&parent)?;
                    let tmp = parent.join(format!(".tmp-{}", fastrand::u64(..)));
                    {
                        use std::io::Write;
                        let mut f = std::fs::File::create(&tmp)?;
                        f.write_all(&data)?;
                        // Kein fsync je Objekt (teuer bei vielen kleinen Dateien): `sync()` vor dem
                        // Katalog macht alles auf einmal dauerhaft.
                    }
                    std::fs::rename(&tmp, &path)?;
                    Ok(())
                })
                .await?
            }
            Storage::S3 { client, prefix } => {
                client.put_bytes(&format!("{prefix}{name}"), data, "application/octet-stream").await
            }
        }
    }

    /// Macht alle bisher geschriebenen Objekte dauerhaft (lokal: `syncfs` des Laufwerks).
    pub async fn sync(&self) -> Result<()> {
        if let Storage::Local(root) = self {
            let root = root.clone();
            tokio::task::spawn_blocking(move || -> Result<()> {
                use std::os::unix::io::AsRawFd;
                let dir = std::fs::File::open(&root)?;
                // SAFETY: gültiger Dateideskriptor eines geöffneten Ordners.
                if unsafe { libc::syncfs(dir.as_raw_fd()) } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                Ok(())
            })
            .await??;
        }
        Ok(())
    }

    pub async fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        check_name(name)?;
        match self {
            Storage::Local(root) => {
                let path = root.join(name);
                match tokio::fs::read(&path).await {
                    Ok(b) => Ok(Some(b)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e.into()),
                }
            }
            Storage::S3 { client, prefix } => client.get(&format!("{prefix}{name}")).await,
        }
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        check_name(name)?;
        match self {
            Storage::Local(root) => match tokio::fs::remove_file(root.join(name)).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            },
            Storage::S3 { client, prefix } => client.delete(&format!("{prefix}{name}")).await,
        }
    }
}

/// Unverschlüsselte Beschreibung der Ablage.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepoConfig {
    pub v: u32,
    pub id: String,
    pub created: i64,
    pub chunker: String,
}

/// Verpackter Hauptschlüssel (mit Passwort bzw. Wiederherstellungsschlüssel).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyFile {
    pub v: u32,
    /// "password" | "recovery"
    pub kind: String,
    #[serde(default)]
    pub rounds: u32,
    pub salt: String,
    pub nonce: String,
    pub wrapped: String,
}

/// Abgeleitete Schlüssel einer Ablage.
pub struct Keys {
    data: LessSafeKey,
    id: hmac::Key,
}

struct Len(usize);
impl ring::hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, salt).extract(ikm);
    let mut out = [0u8; 32];
    prk.expand(&[info], Len(32)).expect("HKDF").fill(&mut out).expect("HKDF");
    out
}

fn aes(key: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).expect("AES-256"))
}

impl Keys {
    pub fn from_master(master: &[u8; 32]) -> Self {
        Self {
            data: aes(&hkdf(master, b"snap-backup-v1", b"data")),
            id: hmac::Key::new(hmac::HMAC_SHA256, &hkdf(master, b"snap-backup-v1", b"id")),
        }
    }

    /// Kennung eines Inhalts (HMAC – verrät der Ablage nicht, welche bekannten Dateien es sind).
    pub fn content_id(&self, plain: &[u8]) -> String {
        data_encoding::HEXLOWER.encode(hmac::sign(&self.id, plain).as_ref())
    }

    /// Verschlüsselt ein Objekt; der Name ist als AAD gebunden (kein Vertauschen möglich).
    pub fn seal(&self, name: &str, plain: &[u8]) -> Result<Vec<u8>> {
        let nonce: [u8; 12] = rand()?;
        let mut buf = plain.to_vec();
        self.data
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(format!("snap-backup-v1/{name}").as_bytes()),
                &mut buf,
            )
            .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buf);
        Ok(out)
    }

    pub fn open(&self, name: &str, blob: &[u8]) -> Result<Vec<u8>> {
        if blob.len() < 12 + 16 {
            anyhow::bail!("Objekt {name} ist beschädigt");
        }
        let mut buf = blob[12..].to_vec();
        let nonce: [u8; 12] = blob[..12].try_into().expect("12");
        let plain = self
            .data
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(format!("snap-backup-v1/{name}").as_bytes()),
                &mut buf,
            )
            .map_err(|_| anyhow::anyhow!("Objekt {name} ist beschädigt oder manipuliert"))?;
        Ok(plain.to_vec())
    }
}

fn wrap_kek(kek: &[u8; 32], kind: &str, master: &[u8; 32], salt: &[u8], rounds: u32) -> Result<KeyFile> {
    let nonce: [u8; 12] = rand()?;
    let mut buf = master.to_vec();
    aes(kek)
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(format!("snap-backup-v1/key/{kind}").as_bytes()),
            &mut buf,
        )
        .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
    Ok(KeyFile {
        v: FORMAT,
        kind: kind.into(),
        rounds,
        salt: B64.encode(salt),
        nonce: B64.encode(&nonce),
        wrapped: B64.encode(&buf),
    })
}

fn password_kek(password: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut kek = [0u8; 32];
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(rounds).expect("> 0"),
        salt,
        password.as_bytes(),
        &mut kek,
    );
    kek
}

/// Wiederherstellungsschlüssel lesbar darstellen: Base32 in Vierergruppen.
pub fn format_recovery(key: &[u8; 32]) -> String {
    let s = data_encoding::BASE32_NOPAD.encode(key);
    s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).expect("ASCII")).collect::<Vec<_>>().join("-")
}

pub fn parse_recovery(s: &str) -> Option<[u8; 32]> {
    let clean: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    data_encoding::BASE32_NOPAD.decode(clean.as_bytes()).ok()?.try_into().ok()
}

/// Erstellt die Schlüsseldateien für einen neuen Hauptschlüssel.
/// Gibt (Hauptschlüssel, Passwort-Datei, Wiederherstellungs-Datei, Wiederherstellungsschlüssel) zurück.
pub fn new_keys(password: &str) -> Result<([u8; 32], KeyFile, KeyFile, [u8; 32])> {
    let master: [u8; 32] = rand()?;
    let salt: [u8; 16] = rand()?;
    let pw = wrap_kek(&password_kek(password, &salt, PBKDF2_ROUNDS), "password", &master, &salt, PBKDF2_ROUNDS)?;
    let recovery: [u8; 32] = rand()?;
    let rsalt: [u8; 16] = rand()?;
    let rec = wrap_kek(&hkdf(&recovery, &rsalt, b"snap-backup-recovery"), "recovery", &master, &rsalt, 0)?;
    Ok((master, pw, rec, recovery))
}

/// Prüft eine Schlüsseldatei aus fremder Quelle (Fuzz-Ziel): Werte in sinnvollen Grenzen.
pub fn parse_keyfile(bytes: &[u8]) -> Result<KeyFile> {
    if bytes.len() > 4096 {
        anyhow::bail!("Schlüsseldatei zu groß");
    }
    let k: KeyFile = serde_json::from_slice(bytes)?;
    let salt = B64.decode(k.salt.as_bytes()).map_err(|_| anyhow::anyhow!("Ungültiges Salz"))?;
    let nonce = B64.decode(k.nonce.as_bytes()).map_err(|_| anyhow::anyhow!("Ungültige Nonce"))?;
    let wrapped = B64.decode(k.wrapped.as_bytes()).map_err(|_| anyhow::anyhow!("Ungültiger Schlüssel"))?;
    let rounds_ok = match k.kind.as_str() {
        "password" => (100_000..=10_000_000).contains(&k.rounds),
        "recovery" => k.rounds == 0,
        _ => false,
    };
    if k.v != FORMAT || !rounds_ok || salt.len() != 16 || nonce.len() != 12 || wrapped.len() != 32 + 16 {
        anyhow::bail!("Ungültige Schlüsseldatei");
    }
    Ok(k)
}

/// Entpackt den Hauptschlüssel mit Passwort oder Wiederherstellungsschlüssel.
pub fn unwrap_key(k: &KeyFile, secret: &str) -> Result<[u8; 32]> {
    let salt = B64.decode(k.salt.as_bytes())?;
    let kek = match k.kind.as_str() {
        "password" => password_kek(secret, &salt, k.rounds),
        "recovery" => {
            let Some(r) = parse_recovery(secret) else {
                api_bail!(InvalidInput, "Ungültiger Wiederherstellungsschlüssel")
            };
            hkdf(&r, &salt, b"snap-backup-recovery")
        }
        _ => anyhow::bail!("Unbekannte Schlüsselart"),
    };
    let nonce: [u8; 12] = B64.decode(k.nonce.as_bytes())?.try_into().map_err(|_| anyhow::anyhow!("Nonce"))?;
    let mut buf = B64.decode(k.wrapped.as_bytes())?;
    let plain = aes(&kek)
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(format!("snap-backup-v1/key/{}", k.kind).as_bytes()),
            &mut buf,
        )
        .map_err(|_| {
            crate::api::error::ApiError::new(
                crate::api::error::ErrorCode::PermissionDenied,
                "Falsches Passwort bzw. falscher Wiederherstellungsschlüssel",
            )
        })?;
    plain.try_into().map_err(|_| anyhow::anyhow!("Ungültiger Schlüssel"))
}

/// Ein gesicherter Stand.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub id: String,
    pub time: i64,
    pub host: String,
    /// Gesicherte Ordner mit ihrem Wurzelbaum.
    pub sources: Vec<SourceRoot>,
    pub files: u64,
    pub bytes: u64,
    /// In diesem Lauf neu gespeicherte Bytes (nach Deduplizierung).
    pub new_bytes: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SourceRoot {
    pub path: String,
    pub tree: String,
}

/// Verzeichnis aller Stände (verschlüsselt).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    pub v: u32,
    pub snapshots: Vec<SnapshotInfo>,
}

/// Eintrag in einem Verzeichnisbaum.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    /// 'f' Datei, 'd' Ordner, 'l' Symlink
    pub kind: char,
    #[serde(default)]
    pub size: u64,
    /// mtime in Nanosekunden
    #[serde(default)]
    pub mtime: i64,
    #[serde(default)]
    pub mode: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chunks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

/// Geöffnete Ablage.
pub struct Repo {
    pub id: String,
    pub storage: Storage,
    pub keys: Keys,
}

pub fn chunk_name(id: &str) -> String {
    format!("data/{}/{id}", &id[..2])
}

pub fn tree_name(id: &str) -> String {
    format!("trees/{}/{id}", &id[..2])
}

impl Repo {
    /// Legt eine neue Ablage an. Gibt (Ablage, Hauptschlüssel, Wiederherstellungsschlüssel) zurück.
    pub async fn init(
        target: &BackupTarget,
        s3: Option<S3Config>,
        password: &str,
    ) -> Result<(Repo, [u8; 32], [u8; 32])> {
        if password.chars().count() < 10 {
            api_bail!(InvalidInput, "Passwort zu kurz (mindestens 10 Zeichen).");
        }
        let id = data_encoding::HEXLOWER.encode(&rand::<16>()?);
        let storage = Storage::open(target, &id, s3)?;
        if !storage.available() {
            api_bail!(Unavailable, "Ziel ist nicht erreichbar (Laufwerk eingehängt?).");
        }
        let pw = password.to_string();
        let (master, pwk, reck, recovery) = tokio::task::spawn_blocking(move || new_keys(&pw)).await??;
        let config = RepoConfig {
            v: FORMAT,
            id: id.clone(),
            created: crate::database::now_unix(),
            chunker: "fastcdc-v1".into(),
        };
        storage.put("config", serde_json::to_vec(&config)?).await?;
        storage.put("keys/password", serde_json::to_vec(&pwk)?).await?;
        storage.put("keys/recovery", serde_json::to_vec(&reck)?).await?;
        let repo = Repo { id, storage, keys: Keys::from_master(&master) };
        repo.save_catalog(&Catalog { v: FORMAT, snapshots: vec![] }).await?;
        Ok((repo, master, recovery))
    }

    /// Öffnet eine bestehende Ablage mit dem lokal gespeicherten Hauptschlüssel.
    pub fn with_master(id: &str, storage: Storage, master: &[u8; 32]) -> Repo {
        Repo { id: id.into(), storage, keys: Keys::from_master(master) }
    }

    /// Öffnet eine Ablage auf einem neuen Gerät mit Passwort oder Wiederherstellungsschlüssel.
    pub async fn connect(storage: Storage, secret: &str) -> Result<(Repo, [u8; 32])> {
        let Some(cfg) = storage.get("config").await? else {
            api_bail!(NotFound, "Am Ziel liegt keine Snap-Sicherung.");
        };
        let cfg: RepoConfig = serde_json::from_slice(&cfg).context("config ist beschädigt")?;
        if cfg.v != FORMAT
            || cfg.chunker != "fastcdc-v1"
            || cfg.id.len() != 32
            || !cfg.id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            api_bail!(InvalidInput, "Unbekanntes Sicherungsformat.");
        }
        let kind = if parse_recovery(secret).is_some() && secret.contains('-') { "recovery" } else { "password" };
        let Some(kf) = storage.get(&format!("keys/{kind}")).await? else {
            api_bail!(NotFound, "Schlüsseldatei fehlt am Ziel.");
        };
        let kf = parse_keyfile(&kf)?;
        let secret = secret.to_string();
        let master = tokio::task::spawn_blocking(move || unwrap_key(&kf, &secret)).await??;
        Ok((Repo { id: cfg.id, storage, keys: Keys::from_master(&master) }, master))
    }

    pub async fn load_catalog(&self) -> Result<Catalog> {
        match self.storage.get("catalog").await? {
            Some(b) => {
                let plain = self.keys.open("catalog", &b)?;
                Ok(serde_json::from_slice(&plain)?)
            }
            None => api_bail!(NotFound, "Katalog der Sicherung fehlt."),
        }
    }

    /// Schreibt den Katalog – erst nachdem alle Daten dauerhaft sind (Reihenfolge = Konsistenz).
    pub async fn save_catalog(&self, c: &Catalog) -> Result<()> {
        let plain = serde_json::to_vec(c)?;
        self.storage.sync().await?;
        self.storage.put("catalog", self.keys.seal("catalog", &plain)?).await?;
        self.storage.sync().await
    }

    pub async fn load_tree(&self, id: &str) -> Result<Tree> {
        let name = tree_name(id);
        let Some(b) = self.storage.get(&name).await? else { api_bail!(NotFound, "Baum {id} fehlt in der Sicherung.") };
        let plain = self.keys.open(&name, &b)?;
        if self.keys.content_id(&plain) != id {
            anyhow::bail!("Baum {id} ist manipuliert");
        }
        Ok(serde_json::from_slice(&plain)?)
    }

    pub async fn load_chunk(&self, id: &str) -> Result<Vec<u8>> {
        let name = chunk_name(id);
        let Some(b) = self.storage.get(&name).await? else {
            api_bail!(NotFound, "Stück {id} fehlt in der Sicherung.")
        };
        let plain = self.keys.open(&name, &b)?;
        if self.keys.content_id(&plain) != id {
            anyhow::bail!("Stück {id} ist manipuliert");
        }
        Ok(plain)
    }
}

/// Lokal gespeicherter Hauptschlüssel (nur für den Benutzer lesbar).
pub fn save_local_key(dir: &Path, repo_id: &str, master: &[u8; 32]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    crate::secure_fs::ensure_private_dir(dir)?;
    let p = dir.join(format!("{repo_id}.key"));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&p)?;
    f.write_all(B64.encode(master).as_bytes())?;
    f.sync_all()?;
    Ok(())
}

pub fn load_local_key(dir: &Path, repo_id: &str) -> Result<[u8; 32]> {
    let s = std::fs::read_to_string(dir.join(format!("{repo_id}.key"))).context("Lokaler Sicherungsschlüssel fehlt")?;
    B64.decode(s.trim().as_bytes())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Lokaler Sicherungsschlüssel ist beschädigt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_wrap_and_unwrap() {
        let (master, pw, rec, recovery) = new_keys("ein langes passwort").unwrap();
        assert_eq!(unwrap_key(&pw, "ein langes passwort").unwrap(), master);
        assert!(unwrap_key(&pw, "falsch").is_err());
        let shown = format_recovery(&recovery);
        assert!(shown.contains('-'));
        assert_eq!(unwrap_key(&rec, &shown).unwrap(), master);
        assert_eq!(
            unwrap_key(&rec, &shown.to_lowercase().replace('-', " ")).unwrap(),
            master,
            "tolerant bei der Eingabe"
        );
        assert!(unwrap_key(&rec, "AAAA-BBBB").is_err());
        // Schlüsseldateien sind nach dem Parser gültig, Manipulationen nicht
        let bytes = serde_json::to_vec(&pw).unwrap();
        parse_keyfile(&bytes).unwrap();
        let mut bad = pw.clone();
        bad.rounds = 1;
        assert!(parse_keyfile(&serde_json::to_vec(&bad).unwrap()).is_err(), "zu wenige Runden");
        let mut swapped = rec.clone();
        swapped.kind = "password".into();
        assert!(parse_keyfile(&serde_json::to_vec(&swapped).unwrap()).is_err());
    }

    #[test]
    fn objects_are_bound_to_their_name() {
        let k = Keys::from_master(&[3u8; 32]);
        let blob = k.seal("data/ab/abc", b"inhalt").unwrap();
        assert_eq!(k.open("data/ab/abc", &blob).unwrap(), b"inhalt");
        assert!(k.open("data/ab/abd", &blob).is_err(), "anderer Name → abgelehnt");
        let mut t = blob.clone();
        t[20] ^= 1;
        assert!(k.open("data/ab/abc", &t).is_err());
        assert!(Keys::from_master(&[4u8; 32]).open("data/ab/abc", &blob).is_err());
        assert_ne!(k.content_id(b"x"), Keys::from_master(&[4u8; 32]).content_id(b"x"), "IDs geheim je Ablage");
        for bad in ["../x", "a//b", "/abs", "data/../config", "a b", ""] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
