//! Brücke zwischen Oberfläche (Web-Ansicht) und Daemon.
//!
//! Sicherheitsmodell (Entscheidung vom 2026-11-06):
//! - Die Web-Ansicht bekommt **keinen** allgemeinen Kanal zum Daemon, sondern nur die typisierten
//!   Befehle in `commands.rs`. Jeder baut genau eine Daemon-Anfrage.
//! - Nicht rückgängig zu machende oder folgenreiche Aktionen (endgültig löschen, Papierkorb leeren,
//!   ausführbare Dateien öffnen) bestätigt der Nutzer in einem **nativen** Dialog, den Skript in der
//!   Web-Ansicht weder auslösen noch wegklicken kann.
//! - Dateinamen anderer Geräte gelten als feindlich: Sie werden nur als Text dargestellt (React),
//!   die CSP verbietet fremden und eingebetteten Code.

use anyhow::{Context, Result};
use serde::Serialize;
use space_core::api::error::ErrorCode;
use space_core::ipc::{read_frame, write_frame, Envelope, Request, Response};
use std::path::{Path, PathBuf};
use tokio::net::UnixStream;

/// Fehler, wie ihn die Oberfläche sieht (Code für die Logik, Text für die Anzeige).
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct UiError {
    pub code: String,
    pub message: String,
}

impl UiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

impl From<anyhow::Error> for UiError {
    fn from(e: anyhow::Error) -> Self {
        Self::new("Internal", format!("{e:#}"))
    }
}

pub type UiResult<T> = std::result::Result<T, UiError>;

/// Verbindung zum Daemon (über den Unix-Socket desselben Benutzers).
#[derive(Clone)]
pub struct Daemon {
    pub socket: PathBuf,
}

impl Daemon {
    pub fn from_config() -> Result<Self> {
        let cfg = space_core::config::Config::load(None).context("Konfiguration nicht lesbar")?;
        Ok(Self { socket: cfg.socket_path })
    }

    pub async fn connect(&self) -> UiResult<UnixStream> {
        UnixStream::connect(&self.socket).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                UiError::new("DaemonDown", "Der Snap-Dienst läuft nicht.")
            }
            _ => UiError::new("DaemonDown", format!("Keine Verbindung zum Snap-Dienst: {e}")),
        })
    }

    /// Sendet eine Anfrage und liefert die Antwort ohne Varianten-Hülle als JSON.
    pub async fn request(&self, req: Request) -> UiResult<serde_json::Value> {
        let mut stream = self.connect().await?;
        write_frame(&mut stream, &Envelope::new(req)).await?;
        let res: Response = read_frame(&mut stream).await.context("Keine gültige Antwort vom Snap-Dienst")?;
        unwrap_response(res)
    }
}

/// `{"DirListing": {...}}` → `{...}`; Fehler werden zu [`UiError`].
pub fn unwrap_response(res: Response) -> UiResult<serde_json::Value> {
    if let Response::Error { code, message } = res {
        return Err(UiError::new(&code_name(code), message));
    }
    let v = serde_json::to_value(res).map_err(|e| UiError::new("Internal", e.to_string()))?;
    Ok(match v {
        serde_json::Value::Object(m) if m.len() == 1 => m.into_iter().next().map(|(_, v)| v).unwrap_or_default(),
        // Varianten ohne Inhalt werden als Zeichenkette serialisiert.
        other => other,
    })
}

fn code_name(code: ErrorCode) -> String {
    serde_json::to_value(code).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_else(|| "Internal".into())
}

/// Pfad aus der Oberfläche: muss absolut sein und darf keine `..`-Bestandteile haben.
pub fn ui_path(p: &str) -> UiResult<PathBuf> {
    let path = PathBuf::from(p);
    if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(UiError::new("InvalidInput", "Ungültiger Pfad."));
    }
    Ok(path)
}

/// Endungen, die beim Öffnen Code ausführen (können). Öffnen nur nach nativer Bestätigung.
const RISKY_EXT: &[&str] = &[
    "desktop",
    "sh",
    "bash",
    "zsh",
    "fish",
    "run",
    "bin",
    "appimage",
    "exe",
    "msi",
    "bat",
    "cmd",
    "com",
    "ps1",
    "jar",
    "py",
    "pl",
    "rb",
    "js",
    "mjs",
    "vbs",
    "deb",
    "rpm",
    "flatpakref",
    "snap",
    "elf",
    "out",
    "so",
    "lnk",
    "scr",
];

/// Muss vor dem Öffnen nachgefragt werden? (ausführbar oder riskante Endung)
pub fn is_risky_to_open(path: &Path, mode: u32) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    mode & 0o111 != 0 || RISKY_EXT.contains(&ext.as_str())
}

/// Dekodiert `%XX` in einer URL (für das Vorschaubild-Protokoll). Ungültige Folgen → `None`.
pub fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses_are_unwrapped() {
        let v = unwrap_response(Response::Success { message: "ok".into() }).unwrap();
        assert_eq!(v["message"], "ok");
        let e = unwrap_response(Response::error(ErrorCode::NotFound, "weg")).unwrap_err();
        assert_eq!(e, UiError::new("NotFound", "weg"));
    }

    #[test]
    fn paths_must_be_absolute_and_clean() {
        assert!(ui_path("/home/ich/Bilder").is_ok());
        for bad in ["relativ", "", "/home/../etc", "../x"] {
            assert!(ui_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn risky_files_need_confirmation() {
        assert!(is_risky_to_open(Path::new("/x/a.desktop"), 0o644));
        assert!(is_risky_to_open(Path::new("/x/Setup.EXE"), 0o644));
        assert!(is_risky_to_open(Path::new("/x/programm"), 0o755));
        assert!(!is_risky_to_open(Path::new("/x/urlaub.jpg"), 0o644));
        assert!(!is_risky_to_open(Path::new("/x/brief.pdf"), 0o600));
    }

    /// Angriffe über das `thumb:`-Protokoll: kodierte Ausbrüche und NUL-Bytes werden abgewiesen.
    #[test]
    fn thumb_uri_attacks() {
        let check = |raw: &str| percent_decode(raw).and_then(|p| ui_path(&p).ok());
        assert!(check("/home/ich/a.jpg").is_some());
        for bad in ["/home/%2E%2E/etc/shadow", "/home/ich/%2e%2e/%2e%2e/root", "relativ.jpg", "/a/b/..", "%2E%2E/x"] {
            assert!(check(bad).is_none(), "{bad}");
        }
        // NUL im Pfad: erreicht den Daemon höchstens als ungültiger Pfad (dort abgewiesen).
        assert!(check("/home/ich/a%00.jpg").is_none_or(|p| p.to_string_lossy().contains('\0')));
    }

    /// Kosten der Brücke (Socket + JSON) für eine Ordnerseite mit 2 000 Einträgen.
    /// `SNAP_SOCKET=<pfad> SNAP_DIR=<ordner>` misst gegen einen laufenden Daemon.
    #[tokio::test]
    #[ignore]
    async fn perf_bridge_roundtrip() {
        let (Ok(sock), Ok(dir)) = (std::env::var("SNAP_SOCKET"), std::env::var("SNAP_DIR")) else {
            eprintln!("SNAP_SOCKET/SNAP_DIR nicht gesetzt");
            return;
        };
        let d = Daemon { socket: sock.into() };
        let req = || Request::ListDir { path: dir.clone().into(), show_hidden: false, offset: 0, limit: 2000 };
        d.request(req()).await.unwrap();
        let n = 50;
        let t = std::time::Instant::now();
        let mut entries = 0;
        for _ in 0..n {
            entries = d.request(req()).await.unwrap()["entries"].as_array().unwrap().len();
        }
        let per = t.elapsed() / n;
        println!("brücke: Ordnerseite mit {entries} Einträgen: {:.2} ms", per.as_secs_f64() * 1e3);
        assert!(per < std::time::Duration::from_millis(16), "{per:?}");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("/home/ich/a%20b%C3%A4.jpg").as_deref(), Some("/home/ich/a bä.jpg"));
        for bad in ["%", "%2", "%zz", "%ff"] {
            assert_eq!(percent_decode(bad), None, "{bad}");
        }
    }
}
