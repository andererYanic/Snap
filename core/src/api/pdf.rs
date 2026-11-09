//! Anfragen zum Markieren, Signieren und Prüfen von PDFs sowie zur digitalen ID (nur lokal).
//! Rechenintensives (Argon2, PDF lesen) läuft in `spawn_blocking`.

use crate::api_bail;
use crate::daemon::DaemonInner;
use crate::fileops::archive::Password;
use crate::ipc::{PdfLayer, Response};
use crate::pdfsign::{self, id, pdf::Layer, Placement};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

fn data_dir(inner: &DaemonInner) -> PathBuf {
    inner.config.data_dir.clone()
}

/// Eine vorhandene, reguläre PDF-Datei (absolut, ohne `..`, nicht zu groß).
fn pdf_file(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        api_bail!(InvalidInput, "Pfad muss absolut sein.");
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| {
        crate::api::error::ApiError::new(crate::api::error::ErrorCode::NotFound, "Datei nicht gefunden.")
    })?;
    if !meta.is_file() {
        api_bail!(InvalidInput, "Nur Dateien lassen sich bearbeiten.");
    }
    if meta.len() as usize > pdfsign::pdf::MAX_PDF_BYTES {
        api_bail!(InvalidInput, "Die PDF-Datei ist zu groß (höchstens 256 MB).");
    }
    Ok(std::fs::read(path)?)
}

/// Schreibt das Ergebnis neben das Original, ohne etwas zu überschreiben.
/// `copy`: „<Name> (<suffix>).pdf“ (bei Bedarf mit Nummer); `temp`: versteckte Datei für „Ersetzen“.
fn write_next_to(path: &Path, bytes: &[u8], mode: &str, suffix: &str) -> Result<PathBuf> {
    use std::io::Write;
    let dir = path.parent().context("Datei ohne Ordner")?;
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Dokument".into());
    let candidates: Box<dyn Iterator<Item = PathBuf>> = match mode {
        "temp" => Box::new(std::iter::once(dir.join(format!(".snap-edit-{:016x}.pdf", fastrand::u64(..))))),
        "copy" => Box::new(
            std::iter::once(dir.join(format!("{stem} ({suffix}).pdf")))
                .chain((2..1000).map(|n| dir.join(format!("{stem} ({suffix} {n}).pdf")))),
        ),
        _ => api_bail!(InvalidInput, "Unbekannte Speicherart."),
    };
    for c in candidates {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&c) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
                    let _ = std::fs::remove_file(&c);
                    return Err(e).context("Speichern fehlgeschlagen");
                }
                return Ok(c);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context("Speichern fehlgeschlagen"),
        }
    }
    api_bail!(AlreadyExists, "Kein freier Dateiname.")
}

fn color(hex: &str) -> Result<[f32; 3]> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        api_bail!(InvalidInput, "Ungültige Farbe.");
    }
    let v = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).map(|x| x as f32 / 255.0).unwrap_or(0.0);
    Ok([v(0), v(2), v(4)])
}

pub(crate) fn id_info(inner: &DaemonInner) -> Result<Response> {
    let dd = data_dir(inner);
    let id = pdfsign::load_id(&dd)?.map(|f| id::check(&f)).transpose()?;
    let has_signature = pdfsign::load_signature(&dd)?.is_some_and(|s| !s.strokes.is_empty());
    Ok(Response::SignId { id, has_signature })
}

pub(crate) async fn id_create(inner: &DaemonInner, name: String, password: Password) -> Result<Response> {
    let dd = data_dir(inner);
    if pdfsign::load_id(&dd)?.is_some() {
        api_bail!(AlreadyExists, "Es gibt schon eine digitale ID. Erst löschen, dann neu erstellen.");
    }
    let now = crate::database::now_unix();
    let file = tokio::task::spawn_blocking(move || id::create(&name, &password.0, now)).await??;
    pdfsign::store_id(&dd, &file)?;
    id_info(inner)
}

pub(crate) fn id_export(inner: &DaemonInner, dest: &Path) -> Result<Response> {
    if !dest.is_absolute() || !dest.is_dir() {
        api_bail!(InvalidInput, "Zielordner ungültig.");
    }
    let file = pdfsign::load_id(&data_dir(inner))?.context("Keine digitale ID vorhanden.")?;
    let name: String =
        file.name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let bytes = serde_json::to_vec_pretty(&file)?;
    // Datei ist verschlüsselt; trotzdem nie überschreiben
    let target = (1..1000)
        .map(|n| {
            if n == 1 {
                dest.join(format!("Snap-ID {name}.snapid"))
            } else {
                dest.join(format!("Snap-ID {name} {n}.snapid"))
            }
        })
        .find(|p| !p.exists())
        .context("Kein freier Dateiname.")?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
        .context("Export fehlgeschlagen")?;
    Ok(Response::PdfWritten { path: target })
}

pub(crate) fn id_import(inner: &DaemonInner, path: &Path) -> Result<Response> {
    let dd = data_dir(inner);
    if pdfsign::load_id(&dd)?.is_some() {
        api_bail!(AlreadyExists, "Es gibt schon eine digitale ID. Erst löschen, dann importieren.");
    }
    let meta = std::fs::metadata(path).context("Datei nicht gefunden")?;
    if !meta.is_file() || meta.len() > 64 * 1024 {
        api_bail!(InvalidInput, "Keine ID-Datei.");
    }
    let file: id::IdFile = serde_json::from_slice(&std::fs::read(path)?).map_err(|_| {
        crate::api::error::ApiError::new(crate::api::error::ErrorCode::InvalidInput, "Keine gültige ID-Datei.")
    })?;
    pdfsign::store_id(&dd, &file)?;
    id_info(inner)
}

pub(crate) fn id_delete(inner: &DaemonInner) -> Result<Response> {
    pdfsign::delete_id(&data_dir(inner))?;
    id_info(inner)
}

pub(crate) fn signature_save(inner: &DaemonInner, strokes: Vec<pdfsign::pdf::Stroke>) -> Result<Response> {
    pdfsign::store_signature(&data_dir(inner), &pdfsign::SavedSignature { strokes: strokes.clone() })?;
    Ok(Response::Signature { strokes })
}

pub(crate) fn signature_get(inner: &DaemonInner) -> Result<Response> {
    let strokes = pdfsign::load_signature(&data_dir(inner))?.map(|s| s.strokes).unwrap_or_default();
    Ok(Response::Signature { strokes })
}

pub(crate) async fn mark(path: PathBuf, layers: Vec<PdfLayer>, mode: String) -> Result<Response> {
    if layers.len() > 1000 {
        api_bail!(InvalidInput, "Zu viele Markierungen.");
    }
    let layers: Vec<Layer> = layers
        .into_iter()
        .map(|l| {
            Ok(Layer {
                page: l.page,
                strokes: l.strokes,
                color: color(&l.color)?,
                width: l.width,
                opacity: 0.38,
                multiply: true,
            })
        })
        .collect::<Result<_>>()?;
    let out = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let orig = pdf_file(&path)?;
        let bytes = pdfsign::pdf::draw(&orig, &layers)?;
        write_next_to(&path, &bytes, &mode, "markiert")
    })
    .await??;
    Ok(Response::PdfWritten { path: out })
}

pub(crate) async fn sign(
    inner: &DaemonInner,
    path: PathBuf,
    placement: Placement,
    password: Password,
    mode: String,
) -> Result<Response> {
    let file = pdfsign::load_id(&data_dir(inner))?.context("Erst eine digitale ID erstellen.")?;
    let out = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let orig = pdf_file(&path)?;
        let unlocked = id::unlock(&file, &password.0)?;
        let bytes = pdfsign::sign(&orig, &placement, &unlocked, &file.name, crate::database::now_unix())?;
        write_next_to(&path, &bytes, &mode, "signiert")
    })
    .await??;
    Ok(Response::PdfWritten { path: out })
}

pub(crate) async fn verify(inner: &DaemonInner, path: PathBuf) -> Result<Response> {
    let own = pdfsign::load_id(&data_dir(inner))?.and_then(|f| id::check(&f).ok()).map(|i| i.fingerprint);
    let items = tokio::task::spawn_blocking(move || -> Result<_> {
        let bytes = pdf_file(&path)?;
        pdfsign::verify(&bytes, own.as_deref())
    })
    .await??;
    Ok(Response::PdfSignatures { items })
}
