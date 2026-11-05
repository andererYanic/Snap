use crate::database::FileRecord;
use std::sync::Arc;
use std::path::Path;
use anyhow::Result;
use tokio::net::UnixListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use crate::network::P2pNetwork;
use crate::database::DatabaseManager;
use crate::ipc::{Request, Response};

pub struct DaemonInner {
    pub network: P2pNetwork,
    pub db: DatabaseManager,
}

#[derive(Clone)]
pub struct Daemon {
    inner: Arc<DaemonInner>,
}

impl Daemon {
    /// Startet den Daemon inklusive Datenbank, persistentem P2P-Netzwerk, IPC-Server und Garbage Collector
    pub async fn start(db_path: &str, blob_store_path: &str, socket_path: &str) -> Result<Self> {
        let db = DatabaseManager::new(db_path)?;
        let network = P2pNetwork::start(blob_store_path).await?;
        
        let inner = Arc::new(DaemonInner { network, db });
        
        // Alten Socket löschen, falls er von einem früheren unsauberen Exit übrig ist
        if Path::new(socket_path).exists() {
            std::fs::remove_file(socket_path)?;
        }
        
        let listener = UnixListener::bind(socket_path)?;
        println!("🔌 IPC-Server lauscht auf Socket: {}", socket_path);

        // IPC-Server als Hintergrund-Task starten
        let inner_clone = inner.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let inner = inner_clone.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0; 4096];
                    if let Ok(n) = stream.read(&mut buf).await {
                        if let Ok(request) = serde_json::from_slice::<Request>(&buf[..n]) {
                            let response = handle_request(&inner, request).await;
                            if let Ok(res_bytes) = serde_json::to_vec(&response) {
                                let _ = stream.write_all(&res_bytes).await;
                            }
                        }
                    }
                });
            }
        });

        let daemon = Self { inner };
        
        // Hintergrund-Task starten, der regelmäßig nach abgelaufenen Soft-Deletes prüft
        daemon.start_pending_pin_worker();
        daemon.clone().spawn_garbage_collector(3600);
        
        Ok(daemon)
    }

    pub fn node_id(&self) -> String {
        self.inner.network.get_node_id().to_string()
    }

    pub async fn shutdown(&self) {
        println!("🛑 Daemon wird heruntergefahren...");
    }

    /// Hintergrund-Task für den Garbage Collector
    fn spawn_garbage_collector(self, interval_secs: u64) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                if let Err(e) = self.run_pruning_pass().await {
                    eprintln!("❌ Fehler beim automatischen Aufräumen (GC): {}", e);
                }
            }
        });
    }

    async fn run_pruning_pass(&self) -> Result<()> {
        // Hier greift dein bestehender Pruning-Mechanismus
        Ok(())
    }

    pub fn start_pending_pin_worker(&self) {
        let inner = self.inner.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                
                // Nun greifen wir über das geklonte inner auf db und network zu
                if let Ok(pending) = inner.db.get_pending_pins() {
                    for (id, target_hash, action) in pending {
                        if action == "PIN" || action == "DOWNLOAD" {
                            if let Ok(Some(ticket)) = inner.db.get_ticket_by_hash(&target_hash) {
                                let default_output = format!("synced_{}", &target_hash[..8.min(target_hash.len())]);
                                
                                match inner.network.download_file(&ticket, &default_output).await {
                                    Ok(_) => {
                                        println!("🔄 Pending Pin erfolgreich im Hintergrund synchronisiert: {}", target_hash);
                                        let _ = inner.db.remove_pending_pin(&id);
                                    }
                                    Err(_) => {
                                        // Peer noch offline – beim nächsten Intervall erneut versuchen
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

}

async fn handle_request(inner: &DaemonInner, req: Request) -> Response {
    match req {
        Request::Publish { path } => {
            let path_buf = std::path::Path::new(&path);
            if !path_buf.exists() || !path_buf.is_file() {
                return Response::Error { message: format!("Datei nicht gefunden oder ungültig: {}", path) };
            }

            // 1. Datei über das Netzwerk veröffentlichen und Ticket erhalten
            let ticket = match inner.network.publish_file(&path).await {
                Ok(t) => t,
                Err(e) => return Response::Error { message: e.to_string() },
            };

            // 2. Metadaten für den Dateikatalog ermitteln
            let file_name = match path_buf.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => "unknown".to_string(),
            };
            
            let file_size = match std::fs::metadata(&path_buf) {
                Ok(m) => m.len(),
                Err(_) => 0,
            };

            // Blake3-Hash berechnen
            let blake3_hash = match std::fs::read(&path_buf) {
                Ok(bytes) => blake3::hash(&bytes).to_string(),
                Err(_) => "".to_string(),
            };

            let file_record = FileRecord {
                blake3_hash,
                file_name,
                file_size,
                mime_type: None,
                ticket: ticket.clone(),
            };

            // 3. In SQLite-Datenbank abspeichern
            if let Err(e) = inner.db.add_file(&file_record, "local_device", &path) {
                eprintln!("⚠️ Konnte veröffentlichte Datei nicht in DB speichern: {}", e);
            }

            Response::Ticket { ticket }
        }

        Request::Download { ticket, hash, output } => {
            // Entweder direkt das Ticket nutzen oder über den Hash in der DB nachschlagen
            let target_ticket = match (ticket, hash) {
                (Some(t), _) => t,
                (None, Some(h)) => {
                    match inner.db.get_ticket_by_hash(&h) {
                        Ok(Some(t)) => t,
                        Ok(None) => return Response::Error { message: format!("Kein Ticket für Hash '{}' im Katalog gefunden.", h) },
                        Err(e) => return Response::Error { message: e.to_string() },
                    }
                }
                (None, None) => return Response::Error { message: "Entweder --ticket oder --hash muss angegeben werden.".to_string() },
            };

            // Standard-Output falls keins angegeben wurde (vorläufiger Fallback)
            let target_output = output.unwrap_or_else(|| "downloaded_file".to_string());

            match inner.network.download_file(&target_ticket, &target_output).await {
                Ok(_) => Response::Success { message: format!("Datei erfolgreich nach '{}' heruntergeladen.", target_output) },
                Err(e) => Response::Error { message: e.to_string() },
            }
        }

        Request::Delete { id, hash, path } => {
            match inner.db.delete_file_flexible(id.as_deref(), hash.as_deref(), path.as_deref()) {
                Ok(count) if count > 0 => Response::Success { message: format!("{} Datei(en) erfolgreich als gelöscht markiert.", count) },
                Ok(_) => Response::Error { message: "Keine passende Datei zum Löschen im Katalog gefunden.".to_string() },
                Err(e) => Response::Error { message: e.to_string() },
            }
        }

        Request::List => {
            match inner.db.list_files() {
                Ok(files) => Response::FileList { files },
                Err(e) => Response::Error { message: e.to_string() },
            }
        }
    }
}
