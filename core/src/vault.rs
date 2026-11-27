//! Tresor-Ordner: Dateien mit Passwort verschlüsselt auf dem Gerät (Entwurf vom 2026-11-27).
//!
//! Schlüssel:
//! - Ein zufälliger 256-Bit-**Tresorschlüssel** verschlüsselt alles. Er liegt zweimal verpackt in
//!   `vault.json`: mit dem Passwort und mit dem Wiederherstellungscode (je Argon2id, 64 MiB,
//!   3 Durchläufe, eigenes Salz → AES-256-GCM).
//! - Entsperrt liegt er nur im Speicher (beim Sperren überschrieben). Sperre nach
//!   [`AUTO_LOCK_SECS`] ohne Benutzung, auf Wunsch sofort (z. B. wenn die App in den Hintergrund geht).
//!
//! Dateien (`objects/<id>`): Kopf `SNAPVLT1` + 8 Byte Zufall, danach Blöcke zu [`CHUNK`] Byte,
//! jeder mit AES-256-GCM. Nonce = Zufall ‖ Blocknummer; AAD = Kennung, Blocknummer und
//! „letzter Block“ – Vertauschen, Kürzen oder Anhängen wird erkannt. Namen und Größen stehen nur
//! im verschlüsselten Verzeichnis (`index`).
//!
//! Der Tresor liegt im privaten Datenordner (nie in Freigaben, Sync, Suche oder Vorschaubildern).
//! Geöffnete Dateien werden kurz nach `open/` entschlüsselt und beim Sperren gelöscht.

use crate::api::error::{ApiError, ErrorCode};
use crate::api_bail;
use anyhow::{Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Klartext je Block.
pub const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
const MAGIC: &[u8; 8] = b"SNAPVLT1";
const HEADER: usize = 16;
pub const AUTO_LOCK_SECS: u64 = 300;
pub const MIN_PASSWORD: usize = 8;
const ARGON_M_KIB: u32 = 64 * 1024;
const ARGON_T: u32 = 3;
const ARGON_P: u32 = 1;
/// Höchstens so viele Dateien im Tresor (Verzeichnis bleibt klein).
pub const MAX_ENTRIES: usize = 100_000;
/// Größtes verschlüsseltes Verzeichnis.
const MAX_INDEX: u64 = 64 * 1024 * 1024;

type Key = Zeroizing<[u8; 32]>;

fn b64() -> data_encoding::Encoding {
    data_encoding::BASE64
}

fn rand<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("Zufallszahlen nicht verfügbar: {e}"))?;
    Ok(b)
}

fn aead(key: &[u8; 32]) -> Result<LessSafeKey> {
    Ok(LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow::anyhow!("AES"))?))
}

fn wrong(msg: &str) -> anyhow::Error {
    ApiError::new(ErrorCode::PasswordRequired, msg.to_string()).into()
}

// ------------------------------------------------------------------ Schlüsseldatei

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Kdf {
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    pub salt: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Wrapped {
    pub kdf: Kdf,
    pub nonce: String,
    pub key: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct KeyFile {
    pub v: u32,
    pub created: i64,
    pub password: Wrapped,
    pub recovery: Wrapped,
}

fn derive(secret: &[u8], kdf: &Kdf) -> Result<Key> {
    // Grenzen gegen eine manipulierte Datei (Speicher/Zeit)
    if kdf.alg != "argon2id"
        || !(8 * 1024..=512 * 1024).contains(&kdf.m_kib)
        || !(1..=10).contains(&kdf.t)
        || !(1..=4).contains(&kdf.p)
    {
        api_bail!(InvalidInput, "Tresor-Datei beschädigt (Schlüsselparameter).");
    }
    let salt = b64().decode(kdf.salt.as_bytes()).context("Tresor-Datei beschädigt")?;
    if !(16..=64).contains(&salt.len()) {
        api_bail!(InvalidInput, "Tresor-Datei beschädigt.");
    }
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut out = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(secret, &salt, &mut out[..])
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(out)
}

fn wrap(master: &[u8; 32], secret: &[u8], label: &[u8]) -> Result<Wrapped> {
    let kdf =
        Kdf { alg: "argon2id".into(), m_kib: ARGON_M_KIB, t: ARGON_T, p: ARGON_P, salt: b64().encode(&rand::<16>()?) };
    let kek = derive(secret, &kdf)?;
    let nonce: [u8; 12] = rand()?;
    let mut buf = master.to_vec();
    aead(&kek)?
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(label), &mut buf)
        .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
    Ok(Wrapped { kdf, nonce: b64().encode(&nonce), key: b64().encode(&buf) })
}

fn unwrap(w: &Wrapped, secret: &[u8], label: &[u8], wrong_msg: &str) -> Result<Key> {
    let nonce: [u8; 12] = b64()
        .decode(w.nonce.as_bytes())
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| anyhow::anyhow!("Tresor-Datei beschädigt"))?;
    let mut buf = b64().decode(w.key.as_bytes()).context("Tresor-Datei beschädigt")?;
    if buf.len() != 32 + TAG {
        api_bail!(InvalidInput, "Tresor-Datei beschädigt.");
    }
    let kek = derive(secret, &w.kdf)?;
    let plain = aead(&kek)?
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(label), &mut buf)
        .map_err(|_| wrong(wrong_msg))?;
    let mut key = Zeroizing::new([0u8; 32]);
    key.copy_from_slice(plain);
    zeroize::Zeroize::zeroize(&mut buf);
    Ok(key)
}

const LABEL_PW: &[u8] = b"snap-vault-v1/key/password";
const LABEL_REC: &[u8] = b"snap-vault-v1/key/recovery";

/// Wiederherstellungscode: 20 Zufallsbytes, Base32 ohne verwechselbare Zeichen, in Vierergruppen.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn new_recovery_code() -> Result<String> {
    let bytes: [u8; 20] = rand()?;
    let mut bits = 0u32;
    let mut nbits = 0;
    let mut out = String::new();
    for b in bytes {
        bits = (bits << 8) | u32::from(b);
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            out.push(CODE_ALPHABET[((bits >> nbits) & 31) as usize] as char);
        }
    }
    Ok(out.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join("-"))
}

/// Eingegebenen Code vereinheitlichen (Großbuchstaben, ohne Trenner; I, O, 0 und 1 kommen nicht vor).
pub fn normalize_code(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect()
}

fn check_password(pw: &str) -> Result<()> {
    if pw.chars().count() < MIN_PASSWORD {
        api_bail!(InvalidInput, "Das Passwort braucht mindestens {MIN_PASSWORD} Zeichen.");
    }
    if pw.len() > 1024 {
        api_bail!(InvalidInput, "Das Passwort ist zu lang.");
    }
    Ok(())
}

/// Neue Schlüsseldatei: (Datei, Tresorschlüssel, Wiederherstellungscode).
pub fn create_keys(password: &str, now: i64) -> Result<(KeyFile, Key, String)> {
    check_password(password)?;
    let master: Key = Zeroizing::new(rand()?);
    let code = new_recovery_code()?;
    let file = KeyFile {
        v: 1,
        created: now,
        password: wrap(&master, password.as_bytes(), LABEL_PW)?,
        recovery: wrap(&master, normalize_code(&code).as_bytes(), LABEL_REC)?,
    };
    Ok((file, master, code))
}

pub fn unlock_password(file: &KeyFile, password: &str) -> Result<Key> {
    if file.v != 1 {
        api_bail!(InvalidInput, "Unbekannte Version des Tresors.");
    }
    unwrap(&file.password, password.as_bytes(), LABEL_PW, "Falsches Passwort.")
}

pub fn unlock_recovery(file: &KeyFile, code: &str) -> Result<Key> {
    if file.v != 1 {
        api_bail!(InvalidInput, "Unbekannte Version des Tresors.");
    }
    unwrap(&file.recovery, normalize_code(code).as_bytes(), LABEL_REC, "Falscher Wiederherstellungscode.")
}

/// Neues Passwort für einen entsperrten Tresorschlüssel.
pub fn rewrap_password(file: &mut KeyFile, master: &[u8; 32], password: &str) -> Result<()> {
    check_password(password)?;
    file.password = wrap(master, password.as_bytes(), LABEL_PW)?;
    Ok(())
}

// ------------------------------------------------------------------ Dateien

fn chunk_nonce(base: &[u8; 8], i: u32) -> Nonce {
    let mut n = [0u8; 12];
    n[..8].copy_from_slice(base);
    n[8..].copy_from_slice(&i.to_be_bytes());
    Nonce::assume_unique_for_key(n)
}

fn chunk_aad(id: &[u8; 16], i: u32, last: bool) -> Vec<u8> {
    let mut a = b"snap-vault-v1/obj".to_vec();
    a.extend_from_slice(id);
    a.extend_from_slice(&i.to_be_bytes());
    a.push(u8::from(last));
    a
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Verschlüsselt `input` vollständig nach `out`. Gibt die Klartextgröße zurück.
pub fn encrypt_stream<R: Read, W: Write>(key: &[u8; 32], id: &[u8; 16], input: &mut R, out: &mut W) -> Result<u64> {
    let base: [u8; 8] = rand()?;
    out.write_all(MAGIC)?;
    out.write_all(&base)?;
    let a = aead(key)?;
    let mut cur = vec![0u8; CHUNK];
    let mut next = vec![0u8; CHUNK];
    let mut n = read_full(input, &mut cur)?;
    let mut total = 0u64;
    let mut i: u32 = 0;
    loop {
        // Vorauslesen, um den letzten Block zu erkennen
        let m = if n == CHUNK { read_full(input, &mut next)? } else { 0 };
        let last = m == 0;
        let mut buf = cur[..n].to_vec();
        a.seal_in_place_append_tag(chunk_nonce(&base, i), Aad::from(&chunk_aad(id, i, last)[..]), &mut buf)
            .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
        out.write_all(&buf)?;
        total += n as u64;
        if last {
            break;
        }
        i = i.checked_add(1).context("Datei zu groß")?;
        std::mem::swap(&mut cur, &mut next);
        n = m;
    }
    zeroize::Zeroize::zeroize(&mut cur[..]);
    zeroize::Zeroize::zeroize(&mut next[..]);
    Ok(total)
}

/// Entschlüsselt eine Tresor-Datei der Länge `len` nach `out`. Jeder Block wird vor der Ausgabe
/// geprüft; bei einem Fehler (falscher Schlüssel, Manipulation, gekürzt) bricht es ab.
pub fn decrypt_stream<R: Read, W: Write>(
    key: &[u8; 32],
    id: &[u8; 16],
    input: &mut R,
    len: u64,
    out: &mut W,
) -> Result<u64> {
    let damaged = || -> anyhow::Error {
        ApiError::new(ErrorCode::InvalidInput, "Tresor-Datei beschädigt oder verändert.").into()
    };
    if len < (HEADER + TAG) as u64 {
        return Err(damaged());
    }
    let mut head = [0u8; HEADER];
    input.read_exact(&mut head).map_err(|_| damaged())?;
    if &head[..8] != MAGIC {
        return Err(damaged());
    }
    let base: [u8; 8] = head[8..].try_into().expect("8 Byte");
    let body = len - HEADER as u64;
    let block = (CHUNK + TAG) as u64;
    let chunks = body.div_ceil(block);
    let last_len = body - (chunks - 1) * block;
    if last_len < TAG as u64 || chunks > u64::from(u32::MAX) {
        return Err(damaged());
    }
    let a = aead(key)?;
    let mut buf = vec![0u8; CHUNK + TAG];
    let mut total = 0u64;
    for i in 0..chunks {
        let last = i + 1 == chunks;
        let size = if last { last_len as usize } else { CHUNK + TAG };
        input.read_exact(&mut buf[..size]).map_err(|_| damaged())?;
        let plain = a
            .open_in_place(
                chunk_nonce(&base, i as u32),
                Aad::from(&chunk_aad(id, i as u32, last)[..]),
                &mut buf[..size],
            )
            .map_err(|_| damaged())?;
        out.write_all(plain)?;
        total += plain.len() as u64;
    }
    zeroize::Zeroize::zeroize(&mut buf[..]);
    Ok(total)
}

// ------------------------------------------------------------------ Verzeichnis

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VaultEntry {
    /// Zufällige Kennung (32 Hex-Zeichen), zugleich Dateiname unter `objects/`.
    pub id: String,
    /// Anzeigename mit Unterordnern, z. B. `Ausweise/Pass.pdf`.
    pub name: String,
    pub size: u64,
    pub added: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
struct Index {
    entries: Vec<VaultEntry>,
}

const LABEL_INDEX: &[u8] = b"snap-vault-v1/index";

fn seal_index(key: &[u8; 32], idx: &Index) -> Result<Vec<u8>> {
    let nonce: [u8; 12] = rand()?;
    let mut buf = serde_json::to_vec(idx)?;
    aead(key)?
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(LABEL_INDEX), &mut buf)
        .map_err(|_| anyhow::anyhow!("Verschlüsselung fehlgeschlagen"))?;
    Ok([&nonce[..], &buf].concat())
}

fn open_index(key: &[u8; 32], data: &[u8]) -> Result<Index> {
    if data.len() < 12 + TAG {
        api_bail!(InvalidInput, "Tresor-Verzeichnis beschädigt.");
    }
    let nonce: [u8; 12] = data[..12].try_into().expect("12 Byte");
    let mut buf = data[12..].to_vec();
    let plain = aead(key)?
        .open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(LABEL_INDEX), &mut buf)
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Tresor-Verzeichnis beschädigt oder verändert."))?;
    let idx: Index = serde_json::from_slice(plain).context("Tresor-Verzeichnis beschädigt")?;
    Ok(idx)
}

/// Kennung als 16 Byte (nur gültige, von uns erzeugte Kennungen).
fn id_bytes(id: &str) -> Result<[u8; 16]> {
    if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        api_bail!(InvalidInput, "Ungültige Tresor-Kennung.");
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&id[i * 2..i * 2 + 2], 16).map_err(|_| anyhow::anyhow!("Kennung"))?;
    }
    Ok(out)
}

fn new_id() -> Result<(String, [u8; 16])> {
    let b: [u8; 16] = rand()?;
    Ok((b.iter().map(|x| format!("{x:02x}")).collect(), b))
}

/// Anzeigename bereinigen: relative Teile ohne `..`, Steuerzeichen und versteckte Namen.
fn clean_name(name: &str) -> Option<String> {
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() || parts.len() > 32 {
        return None;
    }
    for p in &parts {
        if *p == "." || *p == ".." || p.chars().any(|c| c.is_control()) || p.len() > 255 {
            return None;
        }
    }
    Some(parts.join("/"))
}

// ------------------------------------------------------------------ Dienst

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct VaultStatus {
    pub exists: bool,
    pub unlocked: bool,
    /// Nur entsperrt bekannt.
    pub entries: Option<u64>,
    pub auto_lock_secs: u64,
    /// Nach zu vielen falschen Versuchen: so lange warten.
    pub retry_after_secs: u64,
}

struct Unlocked {
    key: Key,
    last_use: Instant,
}

/// Zustand des Tresors (Schlüssel nur im Speicher).
pub struct Vault {
    dir: PathBuf,
    state: Mutex<Option<Unlocked>>,
    failures: Mutex<(u32, Option<Instant>)>,
    /// Ein Schreibvorgang auf das Verzeichnis zurzeit.
    write: Mutex<()>,
}

impl Vault {
    pub fn new(data_dir: &Path) -> Self {
        let v = Self {
            dir: data_dir.join("vault"),
            state: Mutex::new(None),
            failures: Mutex::new((0, None)),
            write: Mutex::new(()),
        };
        // Reste geöffneter Dateien (z. B. nach einem Absturz) entfernen
        let _ = std::fs::remove_dir_all(v.open_dir());
        v
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn key_file(&self) -> PathBuf {
        self.dir.join("vault.json")
    }
    fn index_file(&self) -> PathBuf {
        self.dir.join("index")
    }
    fn objects(&self) -> PathBuf {
        self.dir.join("objects")
    }
    pub fn open_dir(&self) -> PathBuf {
        self.dir.join("open")
    }

    pub fn exists(&self) -> bool {
        self.key_file().is_file()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, Option<Unlocked>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Schlüssel holen und Benutzung vermerken (sonst Fehler „gesperrt“).
    fn key(&self) -> Result<Key> {
        let mut s = self.lock_state();
        match s.as_mut() {
            Some(u) if u.last_use.elapsed() < Duration::from_secs(AUTO_LOCK_SECS) => {
                u.last_use = Instant::now();
                Ok(Zeroizing::new(*u.key))
            }
            _ => {
                drop(s);
                self.lock();
                Err(ApiError::new(ErrorCode::PermissionDenied, "Der Tresor ist gesperrt.").into())
            }
        }
    }

    pub fn status(&self) -> VaultStatus {
        let unlocked =
            self.lock_state().as_ref().is_some_and(|u| u.last_use.elapsed() < Duration::from_secs(AUTO_LOCK_SECS));
        let entries = if unlocked { self.list().ok().map(|e| e.len() as u64) } else { None };
        VaultStatus {
            exists: self.exists(),
            unlocked,
            entries,
            auto_lock_secs: AUTO_LOCK_SECS,
            retry_after_secs: self.retry_after(),
        }
    }

    fn retry_after(&self) -> u64 {
        let f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        match (f.0, f.1) {
            (n, Some(t)) if n >= 5 => {
                // 2 s, 4 s, 8 s … höchstens 15 Minuten
                let wait = Duration::from_secs((2u64 << (n - 5).min(10)).min(900));
                let left = wait.saturating_sub(t.elapsed());
                left.as_secs() + u64::from(left.subsec_nanos() > 0)
            }
            _ => 0,
        }
    }

    fn note_attempt(&self, ok: bool) {
        let mut f = self.failures.lock().unwrap_or_else(|e| e.into_inner());
        *f = if ok { (0, None) } else { (f.0.saturating_add(1), Some(Instant::now())) };
    }

    fn read_keys(&self) -> Result<KeyFile> {
        let data = std::fs::read(self.key_file())
            .map_err(|_| ApiError::new(ErrorCode::NotFound, "Es gibt noch keinen Tresor."))?;
        if data.len() > 64 * 1024 {
            api_bail!(InvalidInput, "Tresor-Datei beschädigt.");
        }
        serde_json::from_slice(&data).context("Tresor-Datei beschädigt")
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> Result<()> {
        let tmp = path.with_extension("tmp");
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        }
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Tresor anlegen. Gibt den Wiederherstellungscode zurück (nur jetzt sichtbar).
    pub fn create(&self, password: &str, now: i64) -> Result<String> {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        if self.exists() {
            api_bail!(AlreadyExists, "Es gibt schon einen Tresor.");
        }
        crate::secure_fs::ensure_private_dir(&self.dir)?;
        crate::secure_fs::ensure_private_dir(&self.objects())?;
        let (file, key, code) = create_keys(password, now)?;
        self.write_atomic(&self.index_file(), &seal_index(&key, &Index::default())?)?;
        self.write_atomic(&self.key_file(), &serde_json::to_vec_pretty(&file)?)?;
        *self.lock_state() = Some(Unlocked { key, last_use: Instant::now() });
        Ok(code)
    }

    fn check_throttle(&self) -> Result<()> {
        let wait = self.retry_after();
        if wait > 0 {
            api_bail!(Unavailable, "Zu viele falsche Versuche – bitte {wait} s warten.");
        }
        Ok(())
    }

    pub fn unlock(&self, password: &str) -> Result<()> {
        self.check_throttle()?;
        let file = self.read_keys()?;
        let res = unlock_password(&file, password);
        self.note_attempt(res.is_ok());
        let key = res?;
        open_index(&key, &std::fs::read(self.index_file())?)?;
        *self.lock_state() = Some(Unlocked { key, last_use: Instant::now() });
        Ok(())
    }

    /// Mit dem Wiederherstellungscode entsperren und ein neues Passwort setzen.
    pub fn recover(&self, code: &str, new_password: &str) -> Result<()> {
        self.check_throttle()?;
        check_password(new_password)?;
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read_keys()?;
        let res = unlock_recovery(&file, code);
        self.note_attempt(res.is_ok());
        let key = res?;
        rewrap_password(&mut file, &key, new_password)?;
        self.write_atomic(&self.key_file(), &serde_json::to_vec_pretty(&file)?)?;
        *self.lock_state() = Some(Unlocked { key, last_use: Instant::now() });
        Ok(())
    }

    pub fn change_password(&self, old: &str, new: &str) -> Result<()> {
        self.check_throttle()?;
        check_password(new)?;
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = self.read_keys()?;
        let res = unlock_password(&file, old);
        self.note_attempt(res.is_ok());
        let key = res?;
        rewrap_password(&mut file, &key, new)?;
        self.write_atomic(&self.key_file(), &serde_json::to_vec_pretty(&file)?)
    }

    /// Sperren: Schlüssel vergessen (überschrieben) und geöffnete Dateien löschen.
    pub fn lock(&self) {
        *self.lock_state() = None;
        let _ = std::fs::remove_dir_all(self.open_dir());
    }

    /// Automatisch sperren, wenn lange unbenutzt. `true`, wenn gerade gesperrt wurde.
    pub fn auto_lock(&self) -> bool {
        let expired =
            self.lock_state().as_ref().is_some_and(|u| u.last_use.elapsed() >= Duration::from_secs(AUTO_LOCK_SECS));
        if expired {
            self.lock();
        }
        expired
    }

    fn read_index(&self, key: &[u8; 32]) -> Result<Index> {
        let f = self.index_file();
        if std::fs::metadata(&f)?.len() > MAX_INDEX {
            api_bail!(InvalidInput, "Tresor-Verzeichnis beschädigt.");
        }
        open_index(key, &std::fs::read(f)?)
    }

    pub fn list(&self) -> Result<Vec<VaultEntry>> {
        let key = self.key()?;
        let mut e = self.read_index(&key)?.entries;
        e.sort_by_key(|a| a.name.to_lowercase());
        Ok(e)
    }

    /// Dateien (und Ordner, rekursiv) aufnehmen. `remove`: Originale danach löschen (nicht in den
    /// Papierkorb – dort lägen sie unverschlüsselt). Gibt die Zahl aufgenommener Dateien zurück.
    pub fn add(&self, paths: &[PathBuf], remove: bool, now: i64) -> Result<usize> {
        let key = self.key()?;
        let mut files = Vec::new();
        for p in paths {
            let meta = std::fs::symlink_metadata(p).with_context(|| format!("{} nicht gefunden", p.display()))?;
            let base = p.parent().unwrap_or(Path::new("/"));
            if meta.is_dir() {
                for e in walkdir::WalkDir::new(p).follow_links(false).into_iter().flatten() {
                    if e.file_type().is_file() {
                        files.push((e.path().to_path_buf(), base.to_path_buf()));
                    }
                }
            } else if meta.is_file() {
                files.push((p.clone(), base.to_path_buf()));
            }
        }
        if files.len() > MAX_ENTRIES {
            api_bail!(InvalidInput, "Zu viele Dateien auf einmal.");
        }
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut idx = self.read_index(&key)?;
        if idx.entries.len() + files.len() > MAX_ENTRIES {
            api_bail!(InvalidInput, "Der Tresor ist voll (höchstens {MAX_ENTRIES} Dateien).");
        }
        let mut added = Vec::new();
        for (path, base) in &files {
            let rel = path.strip_prefix(base).unwrap_or(path).to_string_lossy().into_owned();
            let Some(name) = clean_name(&rel) else { continue };
            let (id, idb) = new_id()?;
            let target = self.objects().join(&id);
            let tmp = self.objects().join(format!("{id}.tmp"));
            let res = (|| -> Result<u64> {
                let mut input = std::io::BufReader::new(std::fs::File::open(path)?);
                let mut out =
                    std::io::BufWriter::new(std::fs::OpenOptions::new().create_new(true).write(true).open(&tmp)?);
                let n = encrypt_stream(&key, &idb, &mut input, &mut out)?;
                out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
                std::fs::rename(&tmp, &target)?;
                Ok(n)
            })();
            match res {
                Ok(size) => {
                    idx.entries.push(VaultEntry { id, name, size, added: now });
                    added.push(path.clone());
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp);
                    // Verzeichnis mit dem bisher Aufgenommenen sichern, dann abbrechen
                    self.write_atomic(&self.index_file(), &seal_index(&key, &idx)?)?;
                    return Err(e.context(format!("{} nicht aufgenommen", path.display())));
                }
            }
        }
        self.write_atomic(&self.index_file(), &seal_index(&key, &idx)?)?;
        if remove {
            for p in &added {
                let _ = std::fs::remove_file(p);
            }
        }
        Ok(added.len())
    }

    fn entry(&self, key: &[u8; 32], id: &str) -> Result<VaultEntry> {
        id_bytes(id)?;
        self.read_index(key)?
            .entries
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound, "Diese Datei gibt es im Tresor nicht.").into())
    }

    fn decrypt_to(&self, key: &[u8; 32], e: &VaultEntry, target: &Path) -> Result<()> {
        let src = self.objects().join(&e.id);
        let len = std::fs::symlink_metadata(&src).context("Tresor-Datei fehlt")?.len();
        let tmp = target.with_file_name(format!(".{}.snap-vault", fastrand::u64(..)));
        let res = (|| -> Result<()> {
            let mut input = std::io::BufReader::new(std::fs::File::open(&src)?);
            let mut out = std::io::BufWriter::new(std::fs::OpenOptions::new().create_new(true).write(true).open(&tmp)?);
            std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
            decrypt_stream(key, &id_bytes(&e.id)?, &mut input, len, &mut out)?;
            out.flush()?;
            Ok(())
        })();
        if let Err(err) = res {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        crate::fileops::ops::rename_noreplace(&tmp, target).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })?;
        Ok(())
    }

    fn base_name(e: &VaultEntry) -> String {
        e.name.rsplit('/').next().filter(|n| !n.is_empty() && !n.starts_with('.')).unwrap_or("Datei").to_string()
    }

    /// Zum Ansehen entschlüsseln (privater Ordner, beim Sperren gelöscht). Gibt den Pfad zurück.
    pub fn open(&self, id: &str) -> Result<PathBuf> {
        let key = self.key()?;
        let e = self.entry(&key, id)?;
        crate::secure_fs::ensure_private_dir(&self.open_dir())?;
        let dir = self.open_dir().join(format!("{:016x}", fastrand::u64(..)));
        crate::secure_fs::ensure_private_dir(&dir)?;
        let target = dir.join(Self::base_name(&e));
        self.decrypt_to(&key, &e, &target)?;
        Ok(target)
    }

    /// Aus dem Tresor holen: entschlüsselt in `dest_dir` (freier Name), bleibt im Tresor.
    pub fn export(&self, id: &str, dest_dir: &Path) -> Result<PathBuf> {
        let key = self.key()?;
        let e = self.entry(&key, id)?;
        if !dest_dir.is_absolute() || !dest_dir.is_dir() {
            api_bail!(InvalidInput, "Zielordner gibt es nicht.");
        }
        if dest_dir.starts_with(&self.dir) {
            api_bail!(InvalidInput, "Ungültiger Zielordner.");
        }
        let mut target = dest_dir.join(Self::base_name(&e));
        if std::fs::symlink_metadata(&target).is_ok() {
            target = crate::fileops::ops::unique_path(&target);
        }
        self.decrypt_to(&key, &e, &target)?;
        Ok(target)
    }

    /// Datei aus dem Tresor löschen (endgültig).
    pub fn remove(&self, id: &str) -> Result<()> {
        let key = self.key()?;
        id_bytes(id)?;
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut idx = self.read_index(&key)?;
        let before = idx.entries.len();
        idx.entries.retain(|e| e.id != id);
        if idx.entries.len() == before {
            api_bail!(NotFound, "Diese Datei gibt es im Tresor nicht.");
        }
        self.write_atomic(&self.index_file(), &seal_index(&key, &idx)?)?;
        let _ = std::fs::remove_file(self.objects().join(id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn roundtrip(len: usize) {
        let key = [7u8; 32];
        let id = [1u8; 16];
        let data: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let mut enc = Vec::new();
        assert_eq!(encrypt_stream(&key, &id, &mut Cursor::new(&data), &mut enc).unwrap(), len as u64);
        let mut dec = Vec::new();
        decrypt_stream(&key, &id, &mut Cursor::new(&enc), enc.len() as u64, &mut dec).unwrap();
        assert_eq!(dec, data, "Länge {len}");
    }

    #[test]
    fn verschluesseln_und_zurueck() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 17] {
            roundtrip(len);
        }
    }

    #[test]
    fn manipulation_wird_erkannt() {
        let key = [7u8; 32];
        let id = [1u8; 16];
        let data = vec![42u8; 3 * CHUNK + 5];
        let mut enc = Vec::new();
        encrypt_stream(&key, &id, &mut Cursor::new(&data), &mut enc).unwrap();
        let dec = |b: &[u8], k: &[u8; 32], i: &[u8; 16]| {
            decrypt_stream(k, i, &mut Cursor::new(b), b.len() as u64, &mut Vec::new())
        };
        // Ein Bit gekippt
        let mut bad = enc.clone();
        bad[HEADER + 100] ^= 1;
        assert!(dec(&bad, &key, &id).is_err());
        // An einer Blockgrenze gekürzt (die letzten Blöcke fehlen)
        let cut = &enc[..HEADER + 2 * (CHUNK + TAG)];
        assert!(dec(cut, &key, &id).is_err());
        // Blöcke vertauscht
        let b = CHUNK + TAG;
        let mut swapped = enc.clone();
        let (x, y) = (HEADER, HEADER + b);
        let first: Vec<u8> = swapped[x..x + b].to_vec();
        swapped.copy_within(y..y + b, x);
        swapped[y..y + b].copy_from_slice(&first);
        assert!(dec(&swapped, &key, &id).is_err());
        // Anderer Schlüssel bzw. andere Kennung (Dateien vertauscht)
        assert!(dec(&enc, &[8u8; 32], &id).is_err());
        assert!(dec(&enc, &key, &[2u8; 16]).is_err());
        // Unsinn
        assert!(dec(b"SNAPVLT1", &key, &id).is_err());
        assert!(dec(&[], &key, &id).is_err());
    }

    #[test]
    fn schluessel_passwort_und_code() {
        let (mut file, master, code) = create_keys("geheim123", 1).unwrap();
        assert_eq!(code.len(), 39, "32 Zeichen in 8 Gruppen: {code}");
        assert_eq!(*unlock_password(&file, "geheim123").unwrap(), *master);
        assert!(unlock_password(&file, "falsch!!!").is_err());
        // Code auch klein und ohne Bindestriche
        assert_eq!(*unlock_recovery(&file, &code.to_lowercase().replace('-', " ")).unwrap(), *master);
        assert!(unlock_recovery(&file, "AAAA-BBBB").is_err());
        rewrap_password(&mut file, &master, "neues-passwort").unwrap();
        assert!(unlock_password(&file, "geheim123").is_err());
        assert_eq!(*unlock_password(&file, "neues-passwort").unwrap(), *master);
        assert!(create_keys("kurz", 1).is_err());
        // Manipulierte Schlüsselparameter (riesiger Speicher) werden abgelehnt, ohne zu rechnen
        file.password.kdf.m_kib = 4 * 1024 * 1024;
        assert!(unlock_password(&file, "neues-passwort").is_err());
    }

    #[test]
    fn namen_und_kennungen() {
        assert_eq!(clean_name("a/b.txt").as_deref(), Some("a/b.txt"));
        assert_eq!(clean_name("/a//b").as_deref(), Some("a/b"));
        assert!(clean_name("../x").is_none() && clean_name("a/./b").is_none() && clean_name("").is_none());
        assert!(id_bytes(&"a".repeat(32)).is_ok());
        assert!(id_bytes("../../etc/passwd").is_err() && id_bytes(&"A".repeat(32)).is_err());
    }
}
