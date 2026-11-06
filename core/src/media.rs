//! Vorschaubilder und Metadaten (Fotos, Musik, Videos).
//!
//! Sicherheit: Dateien können von fremden Geräten oder aus dem Internet stammen.
//! - Nur reine Rust-Decoder (kein C-Code), Bildgröße und Speicher begrenzt (Schutz vor
//!   Dekompressionsbomben), höchstens 2 Decodierungen gleichzeitig, Panics werden abgefangen.
//! - Symlinks werden nicht verfolgt; nur normale Dateien bis [`MAX_INPUT_BYTES`].
//! - Der Cache liegt im privaten Datenordner; Schlüssel = BLAKE3(Pfad, Größe, mtime, Stufe),
//!   geänderte Dateien bekommen also automatisch neue Vorschaubilder.

use crate::api_bail;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Seek};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Größte Datei, die für ein Vorschaubild decodiert wird.
pub const MAX_INPUT_BYTES: u64 = 200 * 1024 * 1024;
const MAX_DIMENSION: u32 = 20_000;
/// Speichergrenze je Decodierung (z. B. 64 MP als RGBA).
const MAX_ALLOC: u64 = 256 * 1024 * 1024;
const JPEG_QUALITY: u8 = 80;

/// Stufe eines Vorschaubilds.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThumbSize {
    /// Längste Kante 256 px (Raster, Listen).
    #[default]
    Small,
    /// Längste Kante 1024 px (Vorschau, Galerie).
    Large,
}

impl ThumbSize {
    pub fn edge(self) -> u32 {
        match self {
            ThumbSize::Small => 256,
            ThumbSize::Large => 1024,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Document,
    Other,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Gps {
    pub lat: f64,
    pub lon: f64,
}

/// Metadaten einer Datei. Alle Felder optional – was die Datei nicht hergibt, bleibt leer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct MediaInfo {
    pub kind: Option<MediaKind>,
    pub mime: Option<String>,
    pub size: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Aufnahmezeit laut EXIF (`YYYY-MM-DD hh:mm:ss`).
    pub taken_at: Option<String>,
    pub camera: Option<String>,
    /// EXIF-Ausrichtung (1–8).
    pub orientation: Option<u16>,
    pub gps: Option<Gps>,
    pub duration_secs: Option<f64>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

/// Erkennt die Art anhand der ersten Bytes (Inhalt), ersatzweise der Endung.
pub fn detect(head: &[u8], name: &str) -> (MediaKind, Option<&'static str>) {
    let starts = |sig: &[u8]| head.starts_with(sig);
    let at = |off: usize, sig: &[u8]| head.get(off..off + sig.len()) == Some(sig);
    let by_magic = if starts(b"\xFF\xD8\xFF") {
        Some((MediaKind::Image, "image/jpeg"))
    } else if starts(b"\x89PNG\r\n\x1a\n") {
        Some((MediaKind::Image, "image/png"))
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some((MediaKind::Image, "image/gif"))
    } else if starts(b"RIFF") && at(8, b"WEBP") {
        Some((MediaKind::Image, "image/webp"))
    } else if starts(b"BM") && head.len() >= 14 {
        Some((MediaKind::Image, "image/bmp"))
    } else if starts(b"II*\0") || starts(b"MM\0*") {
        Some((MediaKind::Image, "image/tiff"))
    } else if at(4, b"ftypheic") || at(4, b"ftypheix") || at(4, b"ftypmif1") {
        Some((MediaKind::Image, "image/heic"))
    } else if at(4, b"ftyp") {
        Some((MediaKind::Video, "video/mp4"))
    } else if starts(b"\x1A\x45\xDF\xA3") {
        Some((MediaKind::Video, "video/x-matroska"))
    } else if starts(b"RIFF") && at(8, b"AVI ") {
        Some((MediaKind::Video, "video/x-msvideo"))
    } else if starts(b"ID3") || starts(b"\xFF\xFB") || starts(b"\xFF\xF3") {
        Some((MediaKind::Audio, "audio/mpeg"))
    } else if starts(b"fLaC") {
        Some((MediaKind::Audio, "audio/flac"))
    } else if starts(b"OggS") {
        Some((MediaKind::Audio, "audio/ogg"))
    } else if starts(b"RIFF") && at(8, b"WAVE") {
        Some((MediaKind::Audio, "audio/wav"))
    } else if starts(b"%PDF-") {
        Some((MediaKind::Document, "application/pdf"))
    } else {
        None
    };
    if let Some((k, m)) = by_magic {
        return (k, Some(m));
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let kind = match ext.as_str() {
        "m4a" | "aac" | "opus" | "wma" | "aiff" | "ape" => MediaKind::Audio,
        "mov" | "webm" | "m4v" | "wmv" | "3gp" => MediaKind::Video,
        "doc" | "docx" | "odt" | "txt" | "md" | "rtf" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" => {
            MediaKind::Document
        }
        _ => MediaKind::Other,
    };
    (kind, None)
}

/// Öffnet eine normale Datei (keine Symlinks, Geräte, FIFOs) bis zur Größengrenze.
fn open_regular(path: &Path) -> Result<(std::fs::File, std::fs::Metadata)> {
    if !path.is_absolute() {
        api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
    }
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => api_bail!(NotFound, "{} existiert nicht.", path.display()),
    };
    if !meta.is_file() {
        api_bail!(InvalidInput, "{} ist keine normale Datei.", path.display());
    }
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW: wurde der Pfad inzwischen durch einen Symlink ersetzt, schlägt das Öffnen fehl.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| format!("{} nicht lesbar", path.display()))?;
    Ok((file, meta))
}

/// Liest Metadaten (EXIF, Audio-/Video-Tags). Blockierend.
pub fn metadata(path: &Path) -> Result<MediaInfo> {
    let (mut file, meta) = open_regular(path)?;
    let mut head = [0u8; 32];
    let n = file.read(&mut head)?;
    file.rewind()?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (kind, mime) = detect(&head[..n], &name);
    let mut info =
        MediaInfo { kind: Some(kind), mime: mime.map(str::to_string), size: meta.len(), ..Default::default() };
    match kind {
        MediaKind::Image => {
            read_exif(&mut BufReader::new(&file), &mut info);
            file.rewind()?;
            if let Ok(reader) = image::ImageReader::new(BufReader::new(&file)).with_guessed_format() {
                if let Ok((w, h)) = reader.into_dimensions() {
                    info.width = Some(w);
                    info.height = Some(h);
                }
            }
        }
        MediaKind::Audio | MediaKind::Video => read_tags(&mut file, &mut info),
        _ => {}
    }
    Ok(info)
}

/// EXIF aus einem beliebigen Datenstrom (auch für das Fuzz-Ziel).
pub fn read_exif<R: std::io::BufRead + Seek>(reader: &mut R, info: &mut MediaInfo) {
    let Ok(exif) = exif::Reader::new().read_from_container(reader) else { return };
    let text = |tag| {
        exif.get_field(tag, exif::In::PRIMARY).map(|f| {
            f.display_value()
                .to_string()
                .trim_matches('"')
                .chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect::<String>()
        })
    };
    info.taken_at = text(exif::Tag::DateTimeOriginal).or_else(|| text(exif::Tag::DateTime));
    let camera = [text(exif::Tag::Make), text(exif::Tag::Model)].into_iter().flatten().collect::<Vec<_>>().join(" ");
    info.camera = (!camera.trim().is_empty()).then(|| camera.trim().to_string());
    info.orientation = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|o| (1..=8).contains(o))
        .map(|o| o as u16);
    let coord = |tag, reftag, max: f64| -> Option<f64> {
        let f = exif.get_field(tag, exif::In::PRIMARY)?;
        let exif::Value::Rational(ref v) = f.value else { return None };
        if v.len() < 3 || v.iter().any(|r| r.denom == 0) {
            return None;
        }
        let deg = v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0;
        let r = exif.get_field(reftag, exif::In::PRIMARY)?.display_value().to_string();
        let signed = if r.contains('S') || r.contains('W') { -deg } else { deg };
        (signed.is_finite() && signed.abs() <= max).then_some(signed)
    };
    if let (Some(lat), Some(lon)) = (
        coord(exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef, 90.0),
        coord(exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef, 180.0),
    ) {
        info.gps = Some(Gps { lat, lon });
    }
}

/// Audio-/Video-Tags (ohne Coverbilder – spart Speicher).
pub fn read_tags<R: Read + Seek>(reader: &mut R, info: &mut MediaInfo) {
    use lofty::file::{AudioFile, TaggedFileExt};
    use lofty::tag::Accessor;
    let Ok(probe) = lofty::probe::Probe::new(reader).guess_file_type() else { return };
    // lofty 0.25.5 läuft bei ADTS-AAC mit einer Frame-Länge von 0 beim Berechnen der Dauer in
    // eine Endlosschleife (gefunden per Fuzzing, aac/read.rs). Für AAC daher nur Tags lesen.
    let is_aac = probe.file_type() == Some(lofty::file::FileType::Aac);
    let opts = lofty::config::ParseOptions::new().read_cover_art(false).read_properties(!is_aac);
    let probe = probe.options(opts);
    let Ok(tagged) = probe.read() else { return };
    let d = tagged.properties().duration().as_secs_f64();
    if d.is_finite() && d > 0.0 {
        info.duration_secs = Some(d);
    }
    let clean = |s: std::borrow::Cow<'_, str>| -> Option<String> {
        let s: String = s.chars().filter(|c| !c.is_control()).take(256).collect();
        (!s.trim().is_empty()).then_some(s)
    };
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        info.title = tag.title().and_then(clean);
        info.artist = tag.artist().and_then(clean);
        info.album = tag.album().and_then(clean);
    }
}

/// Decodiert ein Bild aus Bytes mit Grenzen und verkleinert es (auch für das Fuzz-Ziel).
/// Die EXIF-Ausrichtung wird angewendet.
pub fn render_thumbnail<R: std::io::BufRead + Seek>(reader: R, edge: u32) -> Result<image::RgbImage> {
    render_thumbnail_limited(reader, edge, MAX_ALLOC)
}

/// Wie [`render_thumbnail`] mit eigener Speichergrenze (das Fuzz-Ziel nutzt eine kleine, damit
/// winzige Dateien mit riesigen, aber erlaubten Abmessungen den Lauf nicht ausbremsen).
pub fn render_thumbnail_limited<R: std::io::BufRead + Seek>(
    mut reader: R,
    edge: u32,
    max_alloc: u64,
) -> Result<image::RgbImage> {
    let start = reader.stream_position()?;
    let mut magic = [0u8; 3];
    let is_jpeg = reader.read_exact(&mut magic).is_ok() && magic == [0xFF, 0xD8, 0xFF];
    reader.seek(std::io::SeekFrom::Start(start))?;
    // Schnellweg JPEG: beim Decodieren um 1/2 … 1/8 verkleinern (DCT-Skalierung).
    let (img, orientation) = match is_jpeg.then(|| jpeg_scaled(&mut reader, edge, max_alloc)) {
        Some(Ok(r)) => r,
        _ => {
            reader.seek(std::io::SeekFrom::Start(start))?;
            decode_generic(reader, max_alloc)?
        }
    };
    let mut img = img;
    // Erst verkleinern, dann drehen (billiger).
    if img.width() > edge || img.height() > edge {
        img = img.thumbnail(edge, edge);
    }
    img.apply_orientation(orientation);
    Ok(img.to_rgb8())
}

fn decode_generic<R: std::io::BufRead + Seek>(
    reader: R,
    max_alloc: u64,
) -> Result<(image::DynamicImage, image::metadata::Orientation)> {
    use image::ImageDecoder;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(max_alloc);
    let mut reader = image::ImageReader::new(reader).with_guessed_format()?;
    reader.limits(limits);
    let mut decoder = reader.into_decoder()?;
    // Eigene Prüfung vor dem Decodieren: Nicht jeder Decoder hält sich an `max_alloc`
    // (gefunden per Fuzzing: eine 43-Byte-GIF-Datei mit 16 641 × 11 780 Pixeln reservierte ~780 MB).
    let (w, h) = decoder.dimensions();
    if w > MAX_DIMENSION || h > MAX_DIMENSION || decoder.total_bytes() > max_alloc {
        anyhow::bail!("Bild ist zu groß ({w} × {h} Pixel)");
    }
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    Ok((image::DynamicImage::from_decoder(decoder)?, orientation))
}

/// JPEG direkt verkleinert decodieren. Unbekannte Pixelformate → Fehler (dann allgemeiner Weg).
fn jpeg_scaled<R: std::io::BufRead + Seek>(
    reader: &mut R,
    edge: u32,
    max_alloc: u64,
) -> Result<(image::DynamicImage, image::metadata::Orientation)> {
    let start = reader.stream_position()?;
    let mut info = MediaInfo::default();
    read_exif(reader, &mut info);
    let orientation = info
        .orientation
        .and_then(|o| image::metadata::Orientation::from_exif(o as u8))
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    reader.seek(std::io::SeekFrom::Start(start))?;
    let mut d = jpeg_decoder::Decoder::new(&mut *reader);
    d.set_max_decoding_buffer_size(usize::try_from(max_alloc).unwrap_or(usize::MAX));
    d.read_info()?;
    let i = d.info().context("kein JPEG-Kopf")?;
    if u32::from(i.width) > MAX_DIMENSION || u32::from(i.height) > MAX_DIMENSION {
        anyhow::bail!("Bild ist zu groß");
    }
    let req = edge.min(u32::from(u16::MAX)) as u16;
    let (w, h) = d.scale(req, req)?;
    let px = d.decode()?;
    let (w, h) = (u32::from(w), u32::from(h));
    let img = match d.info().context("kein JPEG-Kopf")?.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => image::RgbImage::from_raw(w, h, px).map(image::DynamicImage::ImageRgb8),
        jpeg_decoder::PixelFormat::L8 => image::GrayImage::from_raw(w, h, px).map(image::DynamicImage::ImageLuma8),
        _ => None,
    };
    Ok((img.context("Pixelformat nicht unterstützt")?, orientation))
}

/// Ergebnis eines Vorschaubilds.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// JPEG im Cache (nur lokal lesbar).
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Aus dem Cache (ohne Decodierung).
    pub cached: bool,
}

/// Cache für Vorschaubilder im privaten Datenordner.
pub struct ThumbCache {
    dir: PathBuf,
    max_bytes: AtomicU64,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    writes: AtomicU64,
}

impl ThumbCache {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            dir,
            max_bytes: AtomicU64::new(max_bytes),
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
            writes: AtomicU64::new(0),
        }
    }

    /// Neue Höchstgröße (Einstellung `thumb_cache_mb`); räumt sofort auf.
    pub fn set_max(&self, max_bytes: u64) {
        self.max_bytes.store(max_bytes, Ordering::Relaxed);
        let dir = self.dir.clone();
        std::thread::spawn(move || prune(&dir, max_bytes));
    }

    fn key(path: &Path, meta: &std::fs::Metadata, size: ThumbSize) -> String {
        use std::os::unix::ffi::OsStrExt;
        let mut h = blake3::Hasher::new();
        h.update(b"snap-thumb-v1\0");
        h.update(path.as_os_str().as_bytes());
        h.update(&[0]);
        h.update(&meta.len().to_le_bytes());
        h.update(&meta.mtime().to_le_bytes());
        h.update(&meta.mtime_nsec().to_le_bytes());
        h.update(&size.edge().to_le_bytes());
        h.finalize().to_hex().to_string()
    }

    fn cache_path(&self, key: &str) -> PathBuf {
        self.dir.join(&key[..2]).join(format!("{key}.jpg"))
    }

    /// Liefert das Vorschaubild (aus dem Cache oder neu erzeugt).
    pub async fn get(&self, path: &Path, size: ThumbSize) -> Result<Thumbnail> {
        if !path.is_absolute() {
            api_bail!(InvalidInput, "Pfad muss absolut sein: {}", path.display());
        }
        let p = path.to_path_buf();
        let meta = match std::fs::symlink_metadata(&p) {
            Ok(m) if m.is_file() => m,
            Ok(_) => api_bail!(InvalidInput, "{} ist keine normale Datei.", p.display()),
            Err(_) => api_bail!(NotFound, "{} existiert nicht.", p.display()),
        };
        let key = Self::key(&p, &meta, size);
        let target = self.cache_path(&key);
        if let Ok((w, h)) = image::image_dimensions(&target) {
            return Ok(Thumbnail { path: target, width: w, height: h, cached: true });
        }
        let _permit = self.slots.acquire().await?;
        let dir = self.dir.clone();
        let t2 = target.clone();
        let (w, h) = tokio::task::spawn_blocking(move || -> Result<(u32, u32)> {
            let (file, meta) = open_regular(&p)?;
            if meta.len() > MAX_INPUT_BYTES {
                api_bail!(InvalidInput, "Datei ist zu groß für ein Vorschaubild.");
            }
            let img = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                render_thumbnail(BufReader::new(file), size.edge())
            }))
            .map_err(|_| anyhow::anyhow!("Decoder abgestürzt"))?
            .map_err(|e| {
                crate::api::error::ApiError::new(
                    crate::api::error::ErrorCode::InvalidInput,
                    format!("Kein Vorschaubild möglich: {e}"),
                )
            })?;
            crate::secure_fs::ensure_private_dir(&dir)?;
            let parent = t2.parent().context("Cache-Pfad")?;
            std::fs::create_dir_all(parent)?;
            let tmp = parent.join(format!(".{}.tmp", std::process::id() as u64 ^ fastrand::u64(..)));
            {
                let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&img)?;
            }
            std::fs::rename(&tmp, &t2)?;
            Ok((img.width(), img.height()))
        })
        .await??;
        // Gelegentlich aufräumen (älteste zuerst), damit der Cache begrenzt bleibt.
        if self.writes.fetch_add(1, Ordering::Relaxed) % 200 == 199 {
            let (dir, max) = (self.dir.clone(), self.max_bytes.load(Ordering::Relaxed));
            tokio::task::spawn_blocking(move || prune(&dir, max));
        }
        Ok(Thumbnail { path: target, width: w, height: h, cached: false })
    }
}

/// Löscht die ältesten Vorschaubilder, bis der Cache unter `max_bytes` liegt.
pub fn prune(dir: &Path, max_bytes: u64) -> u64 {
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = walkdir::WalkDir::new(dir)
        .max_depth(2)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.into_path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    let mut removed = 0;
    for (_, len, p) in files {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 255) as u8, (y % 255) as u8, 128]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&img).unwrap();
        out
    }

    #[test]
    fn detects_by_content_not_name() {
        assert_eq!(detect(&jpeg(4, 4), "foto.txt").0, MediaKind::Image);
        assert_eq!(detect(b"%PDF-1.7", "x").1, Some("application/pdf"));
        assert_eq!(detect(b"fLaC\0\0", "x").0, MediaKind::Audio);
        assert_eq!(detect(b"", "lied.m4a").0, MediaKind::Audio);
        assert_eq!(detect(b"hallo", "x.bin").0, MediaKind::Other);
    }

    #[tokio::test]
    async fn thumbnails_are_cached_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("foto.jpg");
        std::fs::write(&src, jpeg(2000, 1000)).unwrap();
        let cache = ThumbCache::new(dir.path().join("thumbs"), 10 * 1024 * 1024);
        let t = cache.get(&src, ThumbSize::Small).await.unwrap();
        assert_eq!((t.width, t.height, t.cached), (256, 128, false));
        let again = cache.get(&src, ThumbSize::Small).await.unwrap();
        assert!(again.cached);
        assert_eq!(again.path, t.path);
        let large = cache.get(&src, ThumbSize::Large).await.unwrap();
        assert_eq!((large.width, large.height), (1024, 512));
        // Kleine Bilder werden nicht vergrößert
        std::fs::write(dir.path().join("klein.jpg"), jpeg(40, 20)).unwrap();
        let k = cache.get(&dir.path().join("klein.jpg"), ThumbSize::Small).await.unwrap();
        assert_eq!((k.width, k.height), (40, 20));
        // Geänderte Datei → neues Vorschaubild
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&src, jpeg(1000, 1000)).unwrap();
        let changed = cache.get(&src, ThumbSize::Small).await.unwrap();
        assert!(!changed.cached);
        assert_eq!((changed.width, changed.height), (256, 256));

        // Kaputte Datei, Symlink, Ordner, fehlend
        std::fs::write(dir.path().join("kaputt.jpg"), b"\xFF\xD8\xFF\xE0kaputt").unwrap();
        assert!(cache.get(&dir.path().join("kaputt.jpg"), ThumbSize::Small).await.is_err());
        std::os::unix::fs::symlink(&src, dir.path().join("link.jpg")).unwrap();
        assert!(cache.get(&dir.path().join("link.jpg"), ThumbSize::Small).await.is_err());
        assert!(cache.get(dir.path(), ThumbSize::Small).await.is_err());
        assert!(cache.get(&dir.path().join("fehlt.jpg"), ThumbSize::Small).await.is_err());

        // Aufräumen hält die Grenze
        let removed = prune(&dir.path().join("thumbs"), 0);
        assert!(removed >= 3);
    }

    #[test]
    fn oversized_images_are_rejected() {
        use image::ImageEncoder;
        let png = |w: u32, h: u32| {
            let mut buf = Vec::new();
            image::codecs::png::PngEncoder::new(&mut buf)
                .write_image(&image::RgbImage::new(w, h), w, h, image::ExtendedColorType::Rgb8)
                .unwrap();
            buf
        };
        // Grenze greift beim Lesen des Kopfs, vor jeder großen Speicheranforderung.
        assert!(render_thumbnail(std::io::Cursor::new(png(MAX_DIMENSION + 1, 1)), 256).is_err());
        assert!(render_thumbnail(std::io::Cursor::new(png(10, 10)), 256).is_ok());
        assert!(render_thumbnail(std::io::Cursor::new(b"kein bild".to_vec()), 256).is_err());
        // Winzige GIF-Datei, die eine riesige Fläche behauptet (Fuzzing-Fund): abgelehnt, bevor
        // Speicher reserviert wird.
        let gif_bomb: &[u8] =
            b"GIF89a\x01A\x04.\x80\x00\x00\x00\x01\x00\x00\x00\x00,\x00\x00-\x00\x01\x00\x01\x00\x00\x02\x02)\x01\xfb;";
        let t = std::time::Instant::now();
        assert!(render_thumbnail_limited(std::io::Cursor::new(gif_bomb), 64, 16 << 20).is_err());
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn metadata_of_files() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.jpg");
        std::fs::write(&p, jpeg(64, 32)).unwrap();
        let m = metadata(&p).unwrap();
        assert_eq!(m.kind, Some(MediaKind::Image));
        assert_eq!((m.width, m.height), (Some(64), Some(32)));
        assert_eq!(m.gps, None);
        let t = dir.path().join("t.txt");
        std::fs::write(&t, b"hallo").unwrap();
        assert_eq!(metadata(&t).unwrap().kind, Some(MediaKind::Document));
        assert!(metadata(Path::new("relativ")).is_err());
    }

    /// JPEG mit EXIF-Block (APP1) erzeugen.
    pub(crate) fn jpeg_with_exif(w: u32, h: u32) -> Vec<u8> {
        use exif::{experimental::Writer, Field, In, Rational, Tag, Value};
        let f = |tag, value| Field { tag, ifd_num: In::PRIMARY, value };
        let fields = [
            f(Tag::Make, Value::Ascii(vec![b"Fairphone".to_vec()])),
            f(Tag::Model, Value::Ascii(vec![b"FP5".to_vec()])),
            f(Tag::Orientation, Value::Short(vec![6])),
            f(Tag::DateTimeOriginal, Value::Ascii(vec![b"2024:07:14 18:30:00".to_vec()])),
            f(Tag::GPSLatitudeRef, Value::Ascii(vec![b"N".to_vec()])),
            f(
                Tag::GPSLatitude,
                Value::Rational(vec![Rational::from((52, 1)), Rational::from((31, 1)), Rational::from((12, 1))]),
            ),
            f(Tag::GPSLongitudeRef, Value::Ascii(vec![b"E".to_vec()])),
            f(
                Tag::GPSLongitude,
                Value::Rational(vec![Rational::from((13, 1)), Rational::from((24, 1)), Rational::from((0, 1))]),
            ),
        ];
        let mut w8 = Writer::new();
        for field in &fields {
            w8.push_field(field);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        w8.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();
        let plain = jpeg(w, h);
        let mut out = plain[..2].to_vec(); // SOI
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&tiff);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[tokio::test]
    async fn exif_metadata_and_orientation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("urlaub.jpg");
        std::fs::write(&p, jpeg_with_exif(400, 200)).unwrap();
        let m = metadata(&p).unwrap();
        assert_eq!(m.camera.as_deref(), Some("Fairphone FP5"));
        assert_eq!(m.taken_at.as_deref(), Some("2024-07-14 18:30:00"));
        assert_eq!(m.orientation, Some(6));
        let gps = m.gps.unwrap();
        assert!((gps.lat - 52.52).abs() < 0.001 && (gps.lon - 13.4).abs() < 0.001, "{gps:?}");
        // Ausrichtung 6 = 90° gedreht → Hochformat
        let cache = ThumbCache::new(dir.path().join("thumbs"), 1 << 20);
        let t = cache.get(&p, ThumbSize::Small).await.unwrap();
        assert_eq!((t.width, t.height), (128, 256));
    }
}
#[cfg(test)]
mod regression {
    /// Fuzzing-Fund: diese 245 Bytes ließen lofty (AAC) endlos laufen.
    #[test]
    fn aac_zero_length_frame_does_not_hang() {
        let data: &[u8] = include_bytes!("../tests/data/lofty-aac-hang.bin");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut info = super::MediaInfo::default();
            super::read_tags(&mut std::io::Cursor::new(data), &mut info);
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok(), "read_tags hängt");
    }
}
