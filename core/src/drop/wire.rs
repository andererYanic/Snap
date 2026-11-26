//! Nachrichten von `snap/drop/1`, Grenzen und Prüfungen.
//!
//! Alles hier kommt von fremden Geräten und wird als feindlich behandelt: feste Obergrenzen,
//! `deny_unknown_fields`, Namensbereinigung, keine Pfade. Fuzz-Ziele: `fuzz/fuzz_targets/drop_*`.

use crate::api::error::{ApiError, ErrorCode};
use crate::api_bail;
use anyhow::Result;
use iroh::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

pub const DROP_ALPN: &[u8] = b"snap/drop/1";
pub const VERSION: u32 = 1;
/// Größter Rahmen (Probe, Karte, Angebot, Antwort).
pub const MAX_FRAME: u32 = 256 * 1024;
pub const MAX_FILES: usize = 1000;
pub const MAX_TAGS: usize = 512;
/// Anzeigename in Byte.
pub const MAX_DISPLAY_NAME: usize = 64;
pub const MAX_FILE_NAME: usize = 255;
pub const MAX_MIME: usize = 100;
/// Vorschaubild (JPEG) in Byte und Kantenlänge.
pub const MAX_PREVIEW: usize = 8 * 1024;
pub const MAX_PREVIEW_EDGE: u32 = 64;

/// Gerätetyp für das Symbol beim Gegenüber.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Phone,
    Tablet,
    #[default]
    Laptop,
    Desktop,
}

/// Erste Nachricht jeder Verbindung (Sender → Empfänger). Verrät nichts über den Sender.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub v: u32,
    /// Kontakt-Tags (je 16 Hex-Zeichen), höchstens [`MAX_TAGS`].
    pub tags: Vec<String>,
}

impl Probe {
    pub fn tag_bytes(&self) -> Result<Vec<[u8; 8]>> {
        if self.v != VERSION {
            api_bail!(VersionMismatch, "Drop-Version {} wird nicht unterstützt", self.v);
        }
        if self.tags.len() > MAX_TAGS {
            api_bail!(InvalidInput, "Zu viele Kontakt-Tags");
        }
        self.tags.iter().map(|t| hex_array::<8>(t)).collect()
    }
}

/// Beweis, dass das Gegenüber den Drop-Schlüssel in dieser Verbindung besitzt.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Proof {
    /// Drop-Schlüssel (64 Hex-Zeichen).
    pub key: String,
    /// Ed25519-Signatur über den Sitzungswert (128 Hex-Zeichen).
    pub sig: String,
}

impl Proof {
    pub fn new(key: &PublicKey, sig: &Signature) -> Self {
        Self { key: hex(key.as_bytes()), sig: hex(&sig.to_bytes()) }
    }

    /// Prüft den Beweis und gibt den Schlüssel zurück.
    pub fn verify(&self, session: &[u8; 32]) -> Result<PublicKey> {
        let key = PublicKey::from_bytes(&hex_array::<32>(&self.key)?)
            .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Drop-Schlüssel"))?;
        let sig = Signature::from_bytes(&hex_array::<64>(&self.sig)?);
        if !super::identity::verify_session(&key, session, &sig) {
            api_bail!(PermissionDenied, "Signatur des Gegenübers ist ungültig");
        }
        Ok(key)
    }
}

/// Antwort auf die Probe.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "lowercase", deny_unknown_fields)]
pub enum Card {
    /// Sichtbar. `proof` nur, wenn die Probe zu einem gespeicherten Kontakt passte.
    Visible { name: String, kind: DeviceKind, proof: Option<Proof> },
    /// Nicht sichtbar für diesen Sender (Kontakte-Modus, kein Kontakt).
    Hidden,
}

/// Eine angebotene Datei.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfferFile {
    pub name: String,
    pub size: u64,
    /// BLAKE3 (64 Hex-Zeichen).
    pub hash: String,
    pub mime: String,
}

/// Angebot (Sender → Empfänger, nach der Karte).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub name: String,
    pub kind: DeviceKind,
    pub proof: Proof,
    pub files: Vec<OfferFile>,
    /// Kleines JPEG (Base64), wird beim Empfänger nur unscharf gezeigt.
    pub preview: Option<String>,
    /// Geteilter Text oder Link (statt oder zusätzlich zu Dateien), höchstens [`MAX_TEXT`] Byte.
    #[serde(default)]
    pub text: Option<String>,
}

/// Geprüfte Fassung eines Angebots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidOffer {
    pub sender: PublicKey,
    pub name: String,
    pub kind: DeviceKind,
    pub files: Vec<ValidFile>,
    pub total: u64,
    /// Vorschaubild, geprüft und neu als PNG codiert (nie die Originalbytes).
    pub preview_png: Option<Vec<u8>>,
    /// Bereinigter Text (ohne Steuerzeichen außer Zeilenumbruch und Tabulator).
    pub text: Option<String>,
    /// Ist der Text genau ein Link (nur `http`/`https`), steht er hier normalisiert.
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidFile {
    /// Bereinigter Dateiname (ohne Pfad, ohne Steuerzeichen).
    pub name: String,
    pub size: u64,
    pub hash: [u8; 32],
    pub mime: String,
}

impl Offer {
    /// Prüft alle Felder gegen die Grenzen. `session` bindet den Beweis an die Verbindung.
    pub fn validate(&self, session: &[u8; 32]) -> Result<ValidOffer> {
        let sender = self.proof.verify(session)?;
        let text = match &self.text {
            Some(t) if t.len() > MAX_TEXT => api_bail!(InvalidInput, "Text ist zu lang"),
            Some(t) => Some(clean_text(t)).filter(|t| !t.trim().is_empty()),
            None => None,
        };
        let (files, total) =
            if self.files.is_empty() && text.is_some() { (Vec::new(), 0) } else { validate_files(&self.files)? };
        let preview_png = match &self.preview {
            Some(b64) => Some(decode_preview(b64)?),
            None => None,
        };
        let link = text.as_deref().and_then(as_link);
        Ok(ValidOffer {
            sender,
            name: display_name(&self.name),
            kind: self.kind,
            files,
            total,
            preview_png,
            text,
            link,
        })
    }
}

/// Prüft die Dateiliste (ohne Beweis; auch für die Kurzanfrage über Bluetooth genutzt).
pub fn validate_files(files: &[OfferFile]) -> Result<(Vec<ValidFile>, u64)> {
    if files.is_empty() || files.len() > MAX_FILES {
        api_bail!(InvalidInput, "Angebot enthält {} Dateien (erlaubt 1–{MAX_FILES})", files.len());
    }
    let mut total: u64 = 0;
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let name = sanitize_file_name(&f.name)
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Dateiname im Angebot"))?;
        total =
            total.checked_add(f.size).ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Angebot ist zu groß"))?;
        let mime =
            if f.mime.len() <= MAX_MIME && f.mime.bytes().all(|b| b.is_ascii_alphanumeric() || b"/+-.".contains(&b)) {
                f.mime.clone()
            } else {
                "application/octet-stream".into()
            };
        out.push(ValidFile { name, size: f.size, hash: hex_array::<32>(&f.hash)?, mime });
    }
    Ok((out, total))
}

/// Antwort des Empfängers auf das Angebot.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub accepted: bool,
    /// Grund bei Ablehnung: `declined`, `busy`, `space`, `invalid`, `timeout`.
    pub reason: Option<String>,
}

/// Abschluss (Empfänger → Sender) nach allen Dateien. Mit Beweis, damit der Sender den Empfänger
/// als Kontakt speichern kann – erst nach dem Annehmen, nie vorher.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Received {
    pub ok: bool,
    pub proof: Option<Proof>,
}

/// Größter geteilter Text (Byte).
pub const MAX_TEXT: usize = 64 * 1024;

/// Text bereinigen: Steuer- und unsichtbare Formatzeichen entfernen (außer Zeilenumbruch und
/// Tabulator), Zeilenenden vereinheitlichen.
pub fn clean_text(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || (!c.is_control() && !is_format_char(*c)))
        .collect()
}

/// Genau ein Link mit `http`/`https` (ohne Zugangsdaten, mit Rechnername) → normalisierte Adresse.
/// Alles andere (`javascript:`, `intent:`, `file:`, Text mit Leerzeichen …) ist kein Link.
pub fn as_link(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.len() > 4096 || t.chars().any(char::is_whitespace) {
        return None;
    }
    let url = reqwest::Url::parse(t).ok()?;
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|h| !h.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    ok.then(|| url.to_string())
}

/// Anzeigename bereinigen: Steuer- und Formatzeichen weg, Länge begrenzt, nie leer.
pub fn display_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cut = truncate_utf8(&cleaned, MAX_DISPLAY_NAME);
    if cut.is_empty() {
        "Unbenannt".into()
    } else {
        cut.to_string()
    }
}

/// Unsichtbare Formatzeichen (Richtungswechsel, Null-Breite), mit denen sich Namen und
/// Dateiendungen verschleiern lassen (z. B. U+202E vor „fdp.exe“ zeigt „exe.pdf“).
fn is_format_char(c: char) -> bool {
    matches!(c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x2069 | 0xFEFF | 0xAD)
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Dateiname aus einem Angebot bereinigen. `None`, wenn nichts Brauchbares übrig bleibt.
///
/// Invarianten (Fuzz-geprüft): kein `/`, `\`, `:`, keine Steuer- oder Formatzeichen, nicht `.`
/// oder `..`, beginnt nicht mit `.`, höchstens [`MAX_FILE_NAME`] Byte, nicht leer.
pub fn sanitize_file_name(raw: &str) -> Option<String> {
    // Nur der letzte Pfadteil zählt (beide Trennzeichen).
    let last = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = last
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c) && *c != ':' && *c != '/' && *c != '\\')
        .collect();
    s = tidy(&s).to_string();
    if s.is_empty() {
        return None;
    }
    // Versteckte Dateien sichtbar machen (kein ".bashrc" im Download-Ordner).
    if s.starts_with('.') {
        s = format!("_{s}");
    }
    if s.len() > MAX_FILE_NAME {
        // Endung möglichst behalten.
        let (stem, ext) = match s.rfind('.') {
            Some(i) if s.len() - i <= 16 && i > 0 => (&s[..i], &s[i..]),
            _ => (s.as_str(), ""),
        };
        s = format!("{}{ext}", truncate_utf8(stem, MAX_FILE_NAME - ext.len()));
        s = tidy(&s).to_string();
    }
    Some(s)
}

/// Leerraum vorn sowie Punkte und Leerraum hinten entfernen (in einem Schritt, damit das
/// Ergebnis stabil ist: „8. .“ → „8“).
fn tidy(s: &str) -> &str {
    s.trim_start().trim_end_matches(|c: char| c == '.' || c.is_whitespace())
}

/// Vorschaubild prüfen: Base64 → JPEG ≤ 8 KiB, ≤ 64 × 64 px, mit Speichergrenze decodiert und
/// als PNG neu codiert.
pub fn decode_preview(b64: &str) -> Result<Vec<u8>> {
    use data_encoding::BASE64;
    if b64.len() > MAX_PREVIEW * 4 / 3 + 4 {
        api_bail!(InvalidInput, "Vorschaubild ist zu groß");
    }
    let bytes = BASE64
        .decode(b64.as_bytes())
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Vorschaubild ist beschädigt"))?;
    if bytes.len() > MAX_PREVIEW || !bytes.starts_with(&[0xFF, 0xD8]) {
        api_bail!(InvalidInput, "Vorschaubild ist kein kleines JPEG");
    }
    let mut dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(&bytes))
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Vorschaubild ist beschädigt"))?;
    use image::ImageDecoder;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_EDGE);
    limits.max_image_height = Some(MAX_PREVIEW_EDGE);
    limits.max_alloc = Some(u64::from(MAX_PREVIEW_EDGE * MAX_PREVIEW_EDGE) * 4 * 4);
    dec.set_limits(limits).map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Vorschaubild ist zu groß"))?;
    let (w, h) = dec.dimensions();
    if w == 0 || h == 0 || w > MAX_PREVIEW_EDGE || h > MAX_PREVIEW_EDGE {
        api_bail!(InvalidInput, "Vorschaubild ist zu groß");
    }
    let img = image::DynamicImage::from_decoder(dec)
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Vorschaubild ist beschädigt"))?
        .to_rgb8();
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// Vorschaubild für ein eigenes Angebot erzeugen (Bilder; sonst `None`).
pub fn make_preview(path: &std::path::Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let img = crate::media::render_thumbnail_limited(std::io::BufReader::new(file), 48, 64 * 1024 * 1024).ok()?;
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 50);
    enc.encode_image(&img).ok()?;
    (out.len() <= MAX_PREVIEW).then(|| data_encoding::BASE64.encode(&out))
}

// --- Verbindungsdaten für Bluetooth (GATT) und QR-Code ---

const CONNECT_VERSION: u8 = 1;
const MAX_ADDRS: usize = 8;
pub const TOKEN_LEN: usize = 8;

/// Wie ein Empfänger erreichbar ist (über Bluetooth gelesen oder im QR-Code).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectInfo {
    /// Transport-Kennung der Sitzung (nicht der Drop-Schlüssel).
    pub endpoint: PublicKey,
    pub token: [u8; TOKEN_LEN],
    /// Kann einem Hotspot beitreten (Offline-Weg).
    pub can_join_hotspot: bool,
    /// Über Relais erreichbar (QR-Code über das Internet).
    pub relay: bool,
    pub addrs: Vec<SocketAddr>,
    /// Anzeigename – nur im Modus „Alle“ (für Geräte, die nur über Bluetooth erreichbar sind).
    pub name: Option<String>,
}

impl ConnectInfo {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(48 + self.addrs.len() * 19);
        b.push(CONNECT_VERSION);
        b.push(u8::from(self.can_join_hotspot) | (u8::from(self.relay) << 1));
        b.extend_from_slice(self.endpoint.as_bytes());
        b.extend_from_slice(&self.token);
        let addrs: Vec<&SocketAddr> = self.addrs.iter().take(MAX_ADDRS).collect();
        b.push(addrs.len() as u8);
        for a in addrs {
            match a.ip() {
                IpAddr::V4(ip) => {
                    b.push(4);
                    b.extend_from_slice(&ip.octets());
                }
                IpAddr::V6(ip) => {
                    b.push(6);
                    b.extend_from_slice(&ip.octets());
                }
            }
            b.extend_from_slice(&a.port().to_be_bytes());
        }
        put_str(&mut b, self.name.as_deref().map_or("", |n| truncate_utf8(n, MAX_DISPLAY_NAME)));
        b
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader(buf);
        let info = Self::read(&mut r)?;
        if !r.0.is_empty() {
            api_bail!(InvalidInput, "Verbindungsdaten sind beschädigt");
        }
        Ok(info)
    }

    fn read(r: &mut Reader<'_>) -> Result<Self> {
        if r.u8()? != CONNECT_VERSION {
            api_bail!(VersionMismatch, "Verbindungsdaten stammen von einer anderen Version");
        }
        let flags = r.u8()?;
        if flags & !0b11 != 0 {
            api_bail!(InvalidInput, "Verbindungsdaten sind beschädigt");
        }
        let endpoint = PublicKey::from_bytes(&r.array::<32>()?)
            .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Verbindungsdaten sind beschädigt"))?;
        let token = r.array::<TOKEN_LEN>()?;
        let n = r.u8()? as usize;
        if n > MAX_ADDRS {
            api_bail!(InvalidInput, "Verbindungsdaten sind beschädigt");
        }
        let mut addrs = Vec::with_capacity(n);
        for _ in 0..n {
            let ip = match r.u8()? {
                4 => IpAddr::V4(Ipv4Addr::from(r.array::<4>()?)),
                6 => IpAddr::V6(Ipv6Addr::from(r.array::<16>()?)),
                _ => api_bail!(InvalidInput, "Verbindungsdaten sind beschädigt"),
            };
            let port = u16::from_be_bytes(r.array::<2>()?);
            if port == 0 || ip.is_unspecified() || ip.is_multicast() {
                api_bail!(InvalidInput, "Verbindungsdaten enthalten eine ungültige Adresse");
            }
            addrs.push(SocketAddr::new(ip, port));
        }
        let raw = r.str(MAX_DISPLAY_NAME)?;
        // Nur kanonische Namen (sonst wäre die Kodierung nicht eindeutig).
        let name = match raw {
            "" => None,
            n if display_name(n) == n => Some(n.to_string()),
            _ => api_bail!(InvalidInput, "Verbindungsdaten enthalten einen ungültigen Namen"),
        };
        Ok(Self { endpoint, token, can_join_hotspot: flags & 1 != 0, relay: flags & 2 != 0, addrs, name })
    }

    /// Als Text für QR-Codes und zum Abtippen.
    pub fn to_code(&self) -> String {
        format!("snapdrop1-{}", data_encoding::BASE32_NOPAD.encode(&self.encode()).to_ascii_lowercase())
    }

    pub fn from_code(code: &str) -> Result<Self> {
        let body = code
            .trim()
            .strip_prefix("snapdrop1-")
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidInput, "Kein Snap-Drop-Code"))?;
        if body.len() > 1024 {
            api_bail!(InvalidInput, "Snap-Drop-Code ist zu lang");
        }
        let raw = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Snap-Drop-Code ist beschädigt"))?;
        Self::decode(&raw)
    }
}

/// Kurzanfrage über Bluetooth für den Offline-Weg: Der Sender hat einen Hotspot geöffnet und
/// bittet den Empfänger beizutreten. Alles darin ist unbestätigt (Bluetooth ist nicht
/// authentisiert) und wird später gegen das echte Angebot geprüft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotspotRequest {
    pub name: String,
    pub files: u32,
    pub total: u64,
    pub ssid: String,
    pub passphrase: String,
    pub sender: ConnectInfo,
}

const HOTSPOT_VERSION: u8 = 1;

impl HotspotRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = vec![HOTSPOT_VERSION];
        put_str(&mut b, truncate_utf8(&self.name, MAX_DISPLAY_NAME));
        b.extend_from_slice(&self.files.to_be_bytes());
        b.extend_from_slice(&self.total.to_be_bytes());
        put_str(&mut b, &self.ssid);
        put_str(&mut b, &self.passphrase);
        b.extend_from_slice(&self.sender.encode());
        b
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader(buf);
        if r.u8()? != HOTSPOT_VERSION {
            api_bail!(VersionMismatch, "Anfrage stammt von einer anderen Version");
        }
        let name = display_name(r.str(MAX_DISPLAY_NAME)?);
        let files = u32::from_be_bytes(r.array::<4>()?);
        let total = u64::from_be_bytes(r.array::<8>()?);
        if files == 0 || files as usize > MAX_FILES {
            api_bail!(InvalidInput, "Anfrage ist ungültig");
        }
        let ssid = r.str(32)?.to_string();
        let passphrase = r.str(63)?.to_string();
        // WPA2: SSID 1–32 Byte, Passphrase 8–63 druckbare ASCII-Zeichen.
        if ssid.is_empty()
            || ssid.chars().any(char::is_control)
            || passphrase.len() < 8
            || !passphrase.bytes().all(|c| (0x20..0x7f).contains(&c))
        {
            api_bail!(InvalidInput, "Hotspot-Daten sind ungültig");
        }
        let sender = ConnectInfo::read(&mut r)?;
        if !r.0.is_empty() {
            api_bail!(InvalidInput, "Anfrage ist beschädigt");
        }
        Ok(Self { name, files, total, ssid, passphrase, sender })
    }
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    let s = truncate_utf8(s, 255);
    b.push(s.len() as u8);
    b.extend_from_slice(s.as_bytes());
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.0.len() < n {
            api_bail!(InvalidInput, "Daten sind unvollständig");
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    fn str(&mut self, max: usize) -> Result<&'a str> {
        let n = self.u8()? as usize;
        if n > max {
            api_bail!(InvalidInput, "Text ist zu lang");
        }
        std::str::from_utf8(self.take(n)?)
            .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Text ist kein UTF-8").into())
    }
}

pub fn hex(b: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(b)
}

pub fn hex_array<const N: usize>(s: &str) -> Result<[u8; N]> {
    if s.len() != N * 2 {
        api_bail!(InvalidInput, "Ungültiger Hex-Wert");
    }
    let v = data_encoding::HEXLOWER_PERMISSIVE
        .decode(s.as_bytes())
        .map_err(|_| ApiError::new(ErrorCode::InvalidInput, "Ungültiger Hex-Wert"))?;
    let mut a = [0u8; N];
    a.copy_from_slice(&v);
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dateinamen_werden_bereinigt() {
        assert_eq!(sanitize_file_name("foto.jpg").as_deref(), Some("foto.jpg"));
        assert_eq!(sanitize_file_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(sanitize_file_name("C:\\Windows\\evil.exe").as_deref(), Some("evil.exe"));
        assert_eq!(sanitize_file_name(".bashrc").as_deref(), Some("_.bashrc"));
        assert_eq!(sanitize_file_name("rechnung\u{202E}fdp.exe").as_deref(), Some("rechnungfdp.exe"));
        assert_eq!(sanitize_file_name("a\nb\0c.txt").as_deref(), Some("abc.txt"));
        assert_eq!(sanitize_file_name("name...").as_deref(), Some("name"));
        assert_eq!(sanitize_file_name("\u{10}8. .").as_deref(), Some("8"), "Fuzz-Fund: stabil");
        for bad in ["", ".", "..", "/", "a/", "...", "  ", "\u{202E}"] {
            assert_eq!(sanitize_file_name(bad), None, "{bad:?}");
        }
        let long = format!("{}.jpeg", "ä".repeat(300));
        let s = sanitize_file_name(&long).unwrap();
        assert!(s.len() <= MAX_FILE_NAME && s.ends_with(".jpeg"));
    }

    #[test]
    fn text_und_links() {
        assert_eq!(clean_text("a\r\nb\u{0}\u{202E}c\td"), "a\nbc\td");
        assert_eq!(as_link(" https://example.org/x?y=1 ").as_deref(), Some("https://example.org/x?y=1"));
        assert_eq!(as_link("http://Example.ORG").as_deref(), Some("http://example.org/"));
        for bad in [
            "javascript:alert(1)",
            "intent://x#Intent;end",
            "file:///etc/passwd",
            "https://user:pw@example.org",
            "siehe https://example.org",
            "ftp://example.org",
            "https://",
            "",
        ] {
            assert_eq!(as_link(bad), None, "{bad}");
        }
    }

    #[test]
    fn anzeigenamen() {
        assert_eq!(display_name("  Annas\n  Telefon "), "Annas Telefon");
        assert_eq!(display_name("\u{202E}\u{200B}"), "Unbenannt");
        assert!(display_name(&"x".repeat(500)).len() <= MAX_DISPLAY_NAME);
    }

    fn info() -> ConnectInfo {
        ConnectInfo {
            endpoint: iroh::SecretKey::generate().public(),
            token: [9; 8],
            can_join_hotspot: true,
            relay: false,
            addrs: vec!["192.168.1.20:4433".parse().unwrap(), "[fe80::1]:9".parse().unwrap()],
            name: Some("Annas Telefon".into()),
        }
    }

    #[test]
    fn verbindungsdaten_hin_und_zurueck() {
        let i = info();
        assert_eq!(ConnectInfo::decode(&i.encode()).unwrap(), i);
        assert_eq!(ConnectInfo::from_code(&i.to_code()).unwrap(), i);
        let mut b = i.encode();
        b.push(0);
        assert!(ConnectInfo::decode(&b).is_err());
        for n in 0..i.encode().len() {
            assert!(ConnectInfo::decode(&i.encode()[..n]).is_err());
        }
        let mut b = i.encode();
        b[1] = 0xff;
        assert!(ConnectInfo::decode(&b).is_err());
        let ohne = ConnectInfo { name: None, ..i.clone() };
        assert_eq!(ConnectInfo::decode(&ohne.encode()).unwrap(), ohne);
        // Nicht kanonischer Name (Steuerzeichen) wird abgewiesen.
        let mut b = ohne.encode();
        b.pop();
        b.extend_from_slice(&[2, b'a', b'\n']);
        assert!(ConnectInfo::decode(&b).is_err());
    }

    #[test]
    fn hotspot_anfrage() {
        let r = HotspotRequest {
            name: "Anna".into(),
            files: 3,
            total: 1234,
            ssid: "AndroidShare_1234".into(),
            passphrase: "geheim123".into(),
            sender: info(),
        };
        assert_eq!(HotspotRequest::decode(&r.encode()).unwrap(), r);
        let bad = HotspotRequest { passphrase: "kurz".into(), ..r.clone() };
        assert!(HotspotRequest::decode(&bad.encode()).is_err());
        let bad = HotspotRequest { files: 0, ..r };
        assert!(HotspotRequest::decode(&bad.encode()).is_err());
    }

    #[test]
    fn vorschau_grenzen() {
        // Gültiges kleines JPEG.
        let jpeg = crate::media::tests::jpeg(48, 32);
        let png = decode_preview(&data_encoding::BASE64.encode(&jpeg)).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        // Zu große Kantenlänge.
        let big = crate::media::tests::jpeg(200, 20);
        assert!(decode_preview(&data_encoding::BASE64.encode(&big)).is_err());
        // Kein JPEG / kein Base64.
        assert!(decode_preview(&data_encoding::BASE64.encode(b"GIF89a....")).is_err());
        assert!(decode_preview("@@@").is_err());
    }

    #[test]
    fn angebot_pruefen() {
        let id = super::super::identity::DropIdentity::ephemeral();
        let session = [3u8; 32];
        let file = OfferFile { name: "a/b.txt".into(), size: 5, hash: "00".repeat(32), mime: "text/plain".into() };
        let offer = Offer {
            name: "Bob".into(),
            kind: DeviceKind::Phone,
            proof: Proof::new(&id.public(), &id.sign_session(&session)),
            files: vec![file.clone()],
            preview: None,
            text: None,
        };
        let v = offer.validate(&session).unwrap();
        assert_eq!(v.sender, id.public());
        assert_eq!(v.files[0].name, "b.txt");
        assert_eq!(v.total, 5);
        // Beweis aus einer anderen Sitzung.
        assert!(offer.validate(&[4u8; 32]).is_err());
        // Überlauf der Gesamtgröße.
        let big = OfferFile { size: u64::MAX, ..file.clone() };
        let o = Offer { files: vec![big.clone(), big], ..offer.clone() };
        assert!(o.validate(&session).is_err());
        // Zu viele Dateien / keine Dateien.
        let o = Offer { files: vec![file; MAX_FILES + 1], ..offer.clone() };
        assert!(o.validate(&session).is_err());
        let o = Offer { files: vec![], ..offer.clone() };
        assert!(o.validate(&session).is_err());
        // Nur Text: erlaubt, Link erkannt; zu langer Text abgewiesen
        let o = Offer { files: vec![], text: Some("https://example.org".into()), ..offer.clone() };
        let v = o.validate(&session).unwrap();
        assert_eq!((v.files.len(), v.link.as_deref()), (0, Some("https://example.org/")));
        let o = Offer { files: vec![], text: Some("x".repeat(MAX_TEXT + 1)), ..offer.clone() };
        assert!(o.validate(&session).is_err());
        let o = Offer { files: vec![], text: Some(" \n ".into()), ..offer };
        assert!(o.validate(&session).is_err(), "leerer Text zählt nicht");
    }
}
