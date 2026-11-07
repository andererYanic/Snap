//! Snap-Kern für Mobilgeräte, eingebettet über UniFFI (Kotlin-Anbindung für Android).
//!
//! Auf dem Telefon läuft der Kern **im Prozess der App**, nicht als eigener Dienst. Die App ruft
//! [`SnapCore::request`] mit derselben JSON-Anfrage auf, die auch über den Unix-Socket des
//! Desktop-Dienstes läuft (`space_core::ipc::Request`). Ereignisse kommen über
//! [`EventListener`].
//!
//! Sicherheit:
//! - Die Oberfläche (React Native) ist mitgelieferter Code der App und stellt Dateinamen nur als
//!   nativen Text dar; es gibt keine HTML-/Skript-Einschleusung wie in einer Web-Ansicht.
//! - Anfragen laufen als lokaler Aufrufer, wie über den Socket desselben Benutzers.
//! - Der Socket des Kerns liegt im privaten App-Ordner und ist für andere Apps unerreichbar
//!   (Android-Sandbox und Rechte 0600).

#![deny(clippy::undocumented_unsafe_blocks)]

use std::path::PathBuf;
use std::sync::Arc;

uniffi::setup_scaffolding!();

/// Fehler über die FFI-Grenze (als Kotlin-Ausnahme sichtbar).
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SnapError {
    #[error("{0}")]
    Start(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("Der Kern wurde bereits beendet.")]
    Stopped,
}

/// Wird für jedes Ereignis des Kerns aufgerufen (JSON, Feld `type`). Auf einem Hintergrund-Thread.
#[uniffi::export(with_foreign)]
pub trait EventListener: Send + Sync {
    fn on_event(&self, json: String);
}

/// Startoptionen. Alle Pfade absolut.
#[derive(uniffi::Record)]
pub struct CoreOptions {
    /// Privater App-Ordner (Datenbank, Schlüssel, Blobs, Socket), z. B. `Context.filesDir`.
    pub data_dir: String,
    /// Gemeinsamer Speicher, der indexiert und angezeigt wird, z. B. `/storage/emulated/0`.
    /// Leer = nichts indexieren.
    pub storage_root: String,
    /// Anzeigename für gekoppelte Geräte.
    pub device_name: String,
}

/// Der laufende Kern.
#[derive(uniffi::Object)]
pub struct SnapCore {
    runtime: tokio::runtime::Runtime,
    daemon: tokio::sync::RwLock<Option<space_core::daemon::Daemon>>,
}

fn absolute(p: &str, what: &str) -> Result<PathBuf, SnapError> {
    let path = PathBuf::from(p);
    if !path.is_absolute() {
        return Err(SnapError::Start(format!("{what} muss ein absoluter Pfad sein.")));
    }
    Ok(path)
}

/// Datei für Absturzberichte im privaten Datenordner.
fn crash_log_path(data_dir: &std::path::Path) -> PathBuf {
    data_dir.join("crash.log")
}

/// Schreibt Panics des Kerns (Nachricht, Ort, Thread) in `crash.log`, damit die App beim
/// nächsten Start zeigen kann, was passiert ist. Höchstens 64 KiB.
fn install_panic_log(data_dir: PathBuf) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(move || {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            let path = crash_log_path(&data_dir);
            let small = std::fs::metadata(&path).map(|m| m.len() < 64 * 1024).unwrap_or(true);
            if small {
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let thread = std::thread::current().name().unwrap_or("?").to_string();
                    let _ = writeln!(f, "[{secs}] Thread {thread}: {info}");
                }
            }
            previous(info);
        }));
    });
}

/// Absturzbericht des letzten Laufs lesen und löschen (`None`, wenn es keinen gibt).
/// Funktioniert ohne laufenden Kern.
#[uniffi::export]
pub fn take_crash_log(data_dir: String) -> Option<String> {
    let path = crash_log_path(std::path::Path::new(&data_dir));
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(8000).collect())
}

/// Konfiguration für Android aus den Startoptionen.
pub fn mobile_config(opts: &CoreOptions) -> Result<space_core::config::Config, SnapError> {
    let data = absolute(&opts.data_dir, "data_dir")?;
    let mut cfg = space_core::config::Config::portable(&data);
    cfg.socket_path = data.join("run").join("snap.sock");
    cfg.device_name = opts.device_name.trim().chars().take(64).collect();
    if cfg.device_name.is_empty() {
        cfg.device_name = "Android".into();
    }
    // Kein „Home“ auf Android: indexiert wird der gemeinsame Speicher (nach dem Start).
    cfg.index_home = false;
    cfg.index_drives = false;
    cfg.sync_interval_hours = 6.0;
    if !opts.storage_root.is_empty() {
        let storage = absolute(&opts.storage_root, "storage_root")?;
        cfg.pin_dir = storage.join("Snap");
        cfg.download_dir = storage.join("Download").join("Snap");
        cfg.trash_dir = storage.join(".Trash-snap");
    }
    Ok(cfg)
}

// Asynchrone Aufrufe laufen auf Tokio (der Kern nutzt Tokio-Dienste wie spawn_blocking).
#[uniffi::export(async_runtime = "tokio")]
impl SnapCore {
    /// Startet den Kern (blockiert, bis er bereit ist). Danach ist `request` nutzbar.
    #[uniffi::constructor]
    pub fn start(opts: CoreOptions) -> Result<Arc<Self>, SnapError> {
        let cfg = mobile_config(&opts)?;
        let _ = std::fs::create_dir_all(&cfg.data_dir);
        install_panic_log(cfg.data_dir.clone());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("snap-core")
            .build()
            .map_err(|e| SnapError::Start(e.to_string()))?;
        let storage = (!opts.storage_root.is_empty()).then(|| PathBuf::from(&opts.storage_root));
        let daemon = runtime.block_on(async {
            let d = space_core::daemon::Daemon::start(cfg).await.map_err(|e| SnapError::Start(format!("{e:#}")))?;
            if let Some(root) = storage {
                // Gemeinsamen Speicher als Ort anlegen (bereits vorhanden: kein Fehler).
                let _ = d.request(space_core::ipc::Request::IndexAdd { path: root }).await;
            }
            Ok::<_, SnapError>(d)
        })?;
        Ok(Arc::new(Self { runtime, daemon: tokio::sync::RwLock::new(Some(daemon)) }))
    }

    /// Eine Anfrage (JSON wie `space_core::ipc::Request`), Antwort als JSON
    /// (`space_core::ipc::Response`, auch Fehler als `{"Error": {...}}`).
    pub async fn request(&self, json: String) -> Result<String, SnapError> {
        if json.len() > space_core::ipc::MAX_FRAME_LEN as usize {
            return Err(SnapError::InvalidRequest("Anfrage zu groß.".into()));
        }
        let req: space_core::ipc::Request =
            serde_json::from_str(&json).map_err(|e| SnapError::InvalidRequest(e.to_string()))?;
        // Auf der Laufzeit des Kerns ausführen, egal welcher Thread aufruft.
        let guard = self.daemon.read().await;
        let daemon = guard.as_ref().ok_or(SnapError::Stopped)?;
        let res = daemon.request(req).await;
        serde_json::to_string(&res).map_err(|e| SnapError::InvalidRequest(e.to_string()))
    }

    /// Leitet alle Ereignisse an `listener` weiter, bis der Kern beendet wird.
    pub fn subscribe(&self, listener: Arc<dyn EventListener>) -> Result<(), SnapError> {
        let mut sub = self.runtime.block_on(async {
            let guard = self.daemon.read().await;
            guard.as_ref().map(|d| d.subscribe()).ok_or(SnapError::Stopped)
        })?;
        self.runtime.spawn(async move {
            while let Some(ev) = sub.next().await {
                if let Ok(json) = serde_json::to_string(&ev) {
                    listener.on_event(json);
                }
            }
        });
        Ok(())
    }

    /// Beendet den Kern (Verbindungen, Beobachter, Hintergrundaufgaben).
    pub fn stop(&self) {
        self.runtime.block_on(async {
            if let Some(d) = self.daemon.write().await.take() {
                d.shutdown().await;
            }
        });
    }

    /// API-Version des Kerns (für Kompatibilitätsprüfungen der App).
    pub fn api_version(&self) -> u32 {
        space_core::ipc::API_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Collect(Mutex<Vec<String>>);
    impl EventListener for Collect {
        fn on_event(&self, json: String) {
            self.0.lock().unwrap().push(json);
        }
    }

    /// Kosten der FFI-Schicht (JSON hin und zurück) für eine Ordnerseite mit 1 000 Einträgen.
    #[test]
    #[ignore]
    fn perf_ffi_list_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("viele");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..1000 {
            std::fs::write(dir.join(format!("datei_{i:04}.txt")), b"x").unwrap();
        }
        let core = SnapCore::start(CoreOptions {
            data_dir: tmp.path().join("data").to_string_lossy().into(),
            storage_root: String::new(),
            device_name: "Perf".into(),
        })
        .unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let req = format!(r#"{{"ListDir":{{"path":{:?},"limit":1000}}}}"#, dir.to_string_lossy());
        rt.block_on(core.request(req.clone())).unwrap();
        let n = 50;
        let t = std::time::Instant::now();
        for _ in 0..n {
            rt.block_on(core.request(req.clone())).unwrap();
        }
        let per = t.elapsed() / n;
        println!("ffi: Ordnerseite mit 1 000 Einträgen: {:.2} ms", per.as_secs_f64() * 1e3);
        let t = std::time::Instant::now();
        for _ in 0..1000 {
            rt.block_on(core.request(r#""Settings""#.into())).unwrap();
        }
        println!("ffi: kleine Anfrage (Settings): {:.1} µs", t.elapsed().as_secs_f64() * 1e3);
        assert!(per < std::time::Duration::from_millis(16));
        core.stop();
    }

    #[test]
    fn crash_log_is_read_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy().to_string();
        assert_eq!(take_crash_log(dir.clone()), None);
        std::fs::write(tmp.path().join("crash.log"), "[1] Thread snap-core: panicked at x.rs:1:1\n").unwrap();
        assert!(take_crash_log(dir.clone()).unwrap().contains("panicked"));
        assert_eq!(take_crash_log(dir), None, "nach dem Lesen gelöscht");
    }

    #[test]
    fn options_are_validated() {
        let bad = CoreOptions { data_dir: "relativ".into(), storage_root: String::new(), device_name: "x".into() };
        assert!(matches!(mobile_config(&bad), Err(SnapError::Start(_))));
        let ok = CoreOptions {
            data_dir: "/data/user/0/de.snap/files".into(),
            storage_root: "/storage/emulated/0".into(),
            device_name: "  ".into(),
        };
        let c = mobile_config(&ok).unwrap();
        assert_eq!(c.device_name, "Android");
        assert!(!c.index_home && !c.index_drives);
        assert_eq!(c.pin_dir, PathBuf::from("/storage/emulated/0/Snap"));
        assert!(c.socket_path.starts_with("/data/user/0/de.snap/files"));
    }

    /// Kern starten, Anfragen als JSON, Ereignisse, ungültige Eingaben, Beenden.
    #[test]
    fn embedded_core_end_to_end() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = tmp.path().join("storage");
        std::fs::create_dir_all(storage.join("DCIM")).unwrap();
        std::fs::write(storage.join("DCIM/bild.jpg"), b"kein echtes bild").unwrap();
        let core = SnapCore::start(CoreOptions {
            data_dir: tmp.path().join("data").to_string_lossy().into(),
            storage_root: storage.to_string_lossy().into(),
            device_name: "Testtelefon".into(),
        })
        .unwrap();
        assert_eq!(core.api_version(), space_core::ipc::API_VERSION);
        let events = Arc::new(Collect(Mutex::new(Vec::new())));
        core.subscribe(events.clone()).unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let info = rt.block_on(core.request(r#""Info""#.into())).unwrap();
        assert!(info.contains("Testtelefon") || info.contains("node_id"), "{info}");
        let dir = format!(r#"{{"ListDir":{{"path":{:?},"limit":100}}}}"#, storage.join("DCIM").to_string_lossy());
        let listing = rt.block_on(core.request(dir)).unwrap();
        assert!(listing.contains("bild.jpg"), "{listing}");
        // Ungültig: kein JSON / unbekannte Anfrage → Fehler, kein Absturz
        assert!(matches!(rt.block_on(core.request("{".into())), Err(SnapError::InvalidRequest(_))));
        assert!(matches!(rt.block_on(core.request(r#"{"Gibtsnicht":{}}"#.into())), Err(SnapError::InvalidRequest(_))));
        // Fehler des Kerns kommen als Antwort zurück
        let err = rt.block_on(core.request(r#"{"ListDir":{"path":"relativ","limit":1}}"#.into())).unwrap();
        assert!(err.contains("Error"), "{err}");
        // Ereignis auslösen: Einstellung ändern
        rt.block_on(core.request(r#"{"SettingSet":{"key":"link_default_days","value":9}}"#.into())).unwrap();
        for _ in 0..50 {
            if events.0.lock().unwrap().iter().any(|e| e.contains("SettingChanged")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(events.0.lock().unwrap().iter().any(|e| e.contains("SettingChanged")));

        core.stop();
        assert!(matches!(rt.block_on(core.request(r#""Info""#.into())), Err(SnapError::Stopped)));
    }
}
