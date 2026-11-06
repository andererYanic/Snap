//! Freigabelinks für Personen ohne Snap (Cloudflare Worker + R2, Ende-zu-Ende-verschlüsselt).
//!
//! Snap verschlüsselt die Datei auf diesem Gerät und legt nur Chiffretext in den eigenen
//! R2-Bucket (`links/<id>/data`, `links/<id>/meta`, `links/<id>/policy.json`). Der Schlüssel
//! steht ausschließlich im Link-Teil nach `#`, den Browser nie an Server schicken. Der Worker
//! (`gateway/cloudflare/`) liefert die Seite, prüft Ablauf und Zähler und reicht die
//! verschlüsselten Daten durch; entschlüsselt wird im Browser.
//!
//! Widerruf und Ablauf löschen die Objekte. Zugangsdaten liegen in `links.json` im privaten
//! Datenordner (0600) und werden nie über die API zurückgegeben.

pub mod crypto;
pub mod s3;

use crate::api_bail;
use crate::daemon::DaemonInner;
use anyhow::{Context, Result};
use crypto::{LinkKeys, LinkMeta, CHUNK};
use s3::{S3Client, S3Config};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// R2 nimmt bis 5 GiB in einem PUT an (größer: mehrteilig – noch nicht unterstützt).
pub const MAX_LINK_BYTES: u64 = 5 * 1024 * 1024 * 1024 - 1024 * 1024;
const MIN_PASSWORD: usize = 8;

/// Einrichtung (gespeichert in `links.json`, 0600).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkSettings {
    /// Öffentliche Adresse des Workers, z. B. `https://share.example.de`
    pub base_url: String,
    pub s3: S3Config,
}

impl LinkSettings {
    pub fn validate(&self) -> Result<()> {
        let url = reqwest::Url::parse(&self.base_url).context("Ungültige Link-Adresse")?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"));
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            api_bail!(InvalidInput, "Die Link-Adresse muss https:// verwenden.");
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            api_bail!(InvalidInput, "Link-Adresse ohne Pfad angeben, z. B. https://share.example.de");
        }
        self.s3.validate()
    }
}

/// Ein Freigabelink (für Liste und Antwort).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    pub id: String,
    /// Vollständiger Link inkl. Schlüssel (nur lokal angezeigt).
    pub url: String,
    pub name: String,
    pub size: u64,
    pub created_at: i64,
    pub expires_at: i64,
    pub max_downloads: Option<u32>,
    pub password: bool,
    /// 'active' | 'revoked' | 'expired'
    pub state: String,
}

/// Öffentliche, unverschlüsselte Regeln eines Links (liest der Worker).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkPolicy {
    pub v: u32,
    pub expires: i64,
    pub max_downloads: Option<u32>,
    pub password: bool,
    pub size_enc: u64,
}

fn settings_path(inner: &DaemonInner) -> PathBuf {
    inner.config.data_dir.join("links.json")
}

pub fn load_settings(data_dir: &Path) -> Result<Option<LinkSettings>> {
    let p = data_dir.join("links.json");
    match std::fs::read(&p) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).context("links.json ist beschädigt")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Speichert die Einrichtung (nur für den Benutzer lesbar).
pub(crate) fn setup(inner: &DaemonInner, settings: LinkSettings) -> Result<()> {
    settings.validate()?;
    let p = settings_path(inner);
    let tmp = p.with_extension("tmp");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&settings)?)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

fn require(inner: &DaemonInner) -> Result<LinkSettings> {
    match load_settings(&inner.config.data_dir)? {
        Some(s) => Ok(s),
        None => api_bail!(
            Unavailable,
            "Freigabelinks sind noch nicht eingerichtet: space-daemon links setup (Anleitung: gateway/cloudflare/README.md)"
        ),
    }
}

fn b64url(b: &[u8]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(b)
}

fn url_of(base: &str, id: &str, secret_b64: &str) -> String {
    format!("{}/l/{id}#{secret_b64}", base.trim_end_matches('/'))
}

/// Erstellt einen Link: Datei verschlüsseln und hochladen.
pub(crate) async fn create(
    inner: &Arc<DaemonInner>,
    path: &Path,
    days: Option<u32>,
    max_downloads: Option<u32>,
    password: Option<String>,
) -> Result<LinkInfo> {
    let settings = require(inner)?;
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let file = match std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            api_bail!(NotFound, "{} existiert nicht.", path.display())
        }
        Err(_) => api_bail!(InvalidInput, "{} ist nicht lesbar (oder ein Symlink).", path.display()),
    };
    let meta_fs = file.metadata()?;
    if !meta_fs.is_file() {
        api_bail!(InvalidInput, "{} ist keine normale Datei.", path.display());
    }
    let size = meta_fs.len();
    if size > MAX_LINK_BYTES {
        api_bail!(InvalidInput, "Datei ist zu groß für einen Link (höchstens 5 GB).");
    }
    if let Some(pw) = &password {
        if pw.chars().count() < MIN_PASSWORD {
            api_bail!(InvalidInput, "Passwort zu kurz (mindestens {MIN_PASSWORD} Zeichen).");
        }
    }
    let days = days.unwrap_or_else(|| inner.settings.link_default_days()).clamp(1, 3650);
    if max_downloads == Some(0) {
        api_bail!(InvalidInput, "Mindestens ein Download muss erlaubt sein.");
    }
    let name = crate::api::transfer::sanitize_file_name(
        &path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    );

    let mut rnd = [0u8; 48];
    getrandom::fill(&mut rnd).map_err(|e| anyhow::anyhow!("Zufallszahlen des Betriebssystems: {e}"))?;
    let id = data_encoding::HEXLOWER.encode(&rnd[..16]);
    let secret: [u8; 32] = rnd[16..].try_into().expect("32 Byte");
    let has_pw = password.is_some();
    let id2 = id.clone();
    let keys =
        Arc::new(tokio::task::spawn_blocking(move || LinkKeys::derive(&secret, &id2, password.as_deref())).await?);

    let client = S3Client::new(settings.s3.clone())?;
    let prefix = format!("links/{id}");
    // 1. Daten (verschlüsselt, gestreamt)
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(8);
    let k2 = keys.clone();
    let reader = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut file = file;
        let n = crypto::chunk_count(size);
        let mut total = 0u64;
        for i in 0..n {
            let want = ((size - total) as usize).min(CHUNK);
            let mut buf = vec![0u8; want];
            file.read_exact(&mut buf).context("Datei hat sich beim Lesen geändert")?;
            total += want as u64;
            crypto::seal_chunk(&k2, i, i == n - 1, &mut buf)?;
            if tx.blocking_send(Ok(bytes::Bytes::from(buf))).is_err() {
                anyhow::bail!("Hochladen abgebrochen");
            }
        }
        // Wurde die Datei inzwischen länger, wäre der Link unvollständig.
        let mut extra = [0u8; 1];
        if file.read(&mut extra)? != 0 {
            let _ = tx.blocking_send(Err(std::io::Error::other("Datei hat sich beim Lesen geändert")));
            anyhow::bail!("Datei hat sich beim Lesen geändert");
        }
        Ok(())
    });
    let mut rx = rx;
    let body = reqwest::Body::wrap_stream(n0_future::stream::poll_fn(move |cx| rx.poll_recv(cx)));
    let data_key = format!("{prefix}/data");
    let upload = client.put(&data_key, body, crypto::encrypted_size(size), "application/octet-stream");
    let (up, rd) = tokio::join!(upload, reader);
    rd??;
    up?;
    // 2. Metadaten (verschlüsselt), 3. Regeln zuletzt – erst dann ist der Link nutzbar.
    let meta = LinkMeta {
        v: 1,
        name: name.clone(),
        size,
        mime: crate::stream::mime_for(&name).to_string(),
        chunk: CHUNK as u32,
    };
    client.put_bytes(&format!("{prefix}/meta"), crypto::seal_meta(&keys, &meta)?, "application/octet-stream").await?;
    let now = crate::database::now_unix();
    let expires = now + i64::from(days) * 86_400;
    let policy = LinkPolicy { v: 1, expires, max_downloads, password: has_pw, size_enc: crypto::encrypted_size(size) };
    client.put_bytes(&format!("{prefix}/policy.json"), serde_json::to_vec(&policy)?, "application/json").await?;

    let secret_b64 = b64url(&secret);
    inner.db.add_link(&crate::database::LinkRow {
        id: id.clone(),
        secret: secret_b64.clone(),
        name: name.clone(),
        local_path: path.to_string_lossy().into_owned(),
        size,
        created_at: now,
        expires_at: expires,
        max_downloads,
        password: has_pw,
        state: "active".into(),
    })?;
    Ok(LinkInfo {
        url: url_of(&settings.base_url, &id, &secret_b64),
        id,
        name,
        size,
        created_at: now,
        expires_at: expires,
        max_downloads,
        password: has_pw,
        state: "active".into(),
    })
}

pub(crate) fn list(inner: &DaemonInner) -> Result<(Vec<LinkInfo>, Option<String>)> {
    let base = load_settings(&inner.config.data_dir)?.map(|s| s.base_url);
    let items = inner
        .db
        .links()?
        .into_iter()
        .map(|r| LinkInfo {
            url: base.as_deref().map(|b| url_of(b, &r.id, &r.secret)).unwrap_or_default(),
            id: r.id,
            name: r.name,
            size: r.size,
            created_at: r.created_at,
            expires_at: r.expires_at,
            max_downloads: r.max_downloads,
            password: r.password,
            state: r.state,
        })
        .collect();
    Ok((items, base))
}

async fn delete_objects(client: &S3Client, id: &str) -> Result<()> {
    // Regeln zuerst: Danach ist der Link sofort unbrauchbar, auch wenn der Rest scheitert.
    for part in ["policy.json", "meta", "data"] {
        client.delete(&format!("links/{id}/{part}")).await?;
    }
    Ok(())
}

/// Widerruft einen Link (Präfix der ID genügt, wenn eindeutig).
pub(crate) async fn revoke(inner: &DaemonInner, id_prefix: &str) -> Result<LinkInfo> {
    let settings = require(inner)?;
    let id_prefix = id_prefix.trim();
    if id_prefix.len() < 4 || !id_prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        api_bail!(InvalidInput, "Bitte mindestens 4 Zeichen der Link-ID angeben.");
    }
    let (items, _) = list(inner)?;
    let found: Vec<LinkInfo> = items.into_iter().filter(|l| l.id.starts_with(id_prefix)).collect();
    let link = match found.len() {
        0 => api_bail!(NotFound, "Kein Link mit ID {id_prefix}."),
        1 => found.into_iter().next().expect("eins"),
        _ => api_bail!(Ambiguous, "ID {id_prefix} ist nicht eindeutig."),
    };
    delete_objects(&S3Client::new(settings.s3)?, &link.id).await?;
    inner.db.set_link_state(&link.id, "revoked")?;
    Ok(LinkInfo { state: "revoked".into(), ..link })
}

/// Löscht abgelaufene Links aus dem Speicher (der Worker verweigert sie ohnehin).
pub(crate) async fn cleanup(inner: &DaemonInner) -> Result<usize> {
    let Some(settings) = load_settings(&inner.config.data_dir)? else { return Ok(0) };
    let now = crate::database::now_unix();
    let expired: Vec<_> =
        inner.db.links()?.into_iter().filter(|l| l.state == "active" && l.expires_at <= now).collect();
    if expired.is_empty() {
        return Ok(0);
    }
    let client = S3Client::new(settings.s3)?;
    let mut n = 0;
    for l in expired {
        if delete_objects(&client, &l.id).await.is_ok() {
            inner.db.set_link_state(&l.id, "expired")?;
            n += 1;
        }
    }
    Ok(n)
}

pub(crate) async fn run_cleanup(inner: Arc<DaemonInner>) {
    loop {
        if let Err(e) = cleanup(&inner).await {
            tracing::debug!("Abgelaufene Links nicht entfernt: {e:#}");
        }
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
