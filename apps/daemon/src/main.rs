use anyhow::Result;
use clap::{Parser, Subcommand};
use space_core::{Config, Daemon, ipc::{Request, Response}};
use tokio::net::UnixStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Parser)]
#[command(name = "space-daemon", about = "P2P Virtual Distributed File System Daemon & CLI", version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Startet den Daemon-Dienst dauerhaft im Vordergrund
    Start {
        /// Optionaler Pfad zur SQLite-Datenbank (überschreibt config.toml)
        #[arg(short, long, default_value = "space.db")]
        db: String,
    },
    /// Teilt eine lokale Datei über das P2P-Netzwerk
    Publish {
        /// Pfad zur lokalen Datei
        #[arg(short, long)]
        path: String,
    },
    /// Lädt eine Datei über ein P2P-Ticket herunter
    Download {
        /// Das Iroh P2P-Ticket
        #[arg(short, long, conflicts_with = "hash")]
        ticket: Option<String>,
        /// Der Blake3-Hash der Datei aus der Dateiliste
        #[arg(long, conflicts_with = "ticket")]
        hash: Option<String>,
        /// Zielpfad für die heruntergeladene Datei
        #[arg(short, long)]
        output: String,
    },
    /// Markiert eine Datei als gelöscht (Soft-Delete)
    Delete {
        #[arg(long, conflicts_with_all = ["hash", "path"])]
        id: Option<String>,
        #[arg(long, conflicts_with_all = ["id", "path"])]
        hash: Option<String>,
        #[arg(long, conflicts_with_all = ["id", "hash"])]
        path: Option<String>,
    },
    List,
}

/// Hilfsfunktion: Sendet einen IPC-Request an den laufenden Daemon über den Unix-Socket
async fn send_request_to_daemon(socket_path: &str, req: Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket_path).await
        .map_err(|_| anyhow::anyhow!("Konnte keine Verbindung zum Daemon herstellen. Läuft 'space-daemon start' im Hintergrund?"))?;
    
    let req_bytes = serde_json::to_vec(&req)?;
    stream.write_all(&req_bytes).await?;
    stream.shutdown().await?;

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let res: Response = serde_json::from_slice(&buf)?;
    Ok(res)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialisiert das Logging
    tracing_subscriber::fmt::init();

    // Zentrale Konfiguration laden oder Standard-config.toml generieren
    let config = Config::load_or_create("config.toml")?;
    let cli = Cli::parse();

    match cli.command {
        Commands::Start { db } => {
            let db_path = if db == "space.db" { config.db_path.clone() } else { db };
            
            // Startet den echten Dauerläufer-Daemon mit allen 3 Pfaden (DB, Blobs, Socket)
            let daemon = Daemon::start(&db_path, &config.blob_store_path, &config.socket_path).await?;
            
            println!("🔑 Node-ID: {}", daemon.node_id());
            println!("🔄 Daemon aktiv. Lausche auf IPC-Socket: {}", config.socket_path);
            println!("Drücke STRG+C zum Beenden.");

            tokio::signal::ctrl_c().await?;
            daemon.shutdown().await;
        }
        Commands::Publish { path } => {
            let req = Request::Publish { path };
            match send_request_to_daemon(&config.socket_path, req).await? {
                Response::Ticket { ticket } => {
                    println!("\n----------------------------------------");
                    println!("🎉 Datei erfolgreich für P2P freigegeben!");
                    println!("🎫 Ticket:\n{}", ticket);
                    println!("----------------------------------------");
                }
                Response::Error { message } => eprintln!("❌ Fehler vom Daemon: {}", message),
                _ => eprintln!("❌ Unerwartete Antwort vom Daemon."),
            }
        }
        Commands::Download { ticket, hash, output } => {
            let req = Request::Download { ticket, hash, output: Some(output) };
            match send_request_to_daemon(&config.socket_path, req).await? {
                Response::Success { message } => println!("✅ {}", message),
                Response::Error { message } => eprintln!("❌ Fehler vom Daemon: {}", message),
                _ => eprintln!("❌ Unerwartete Antwort vom Daemon."),
            }
        }
        Commands::Delete { id, hash, path } => {
            let req = Request::Delete { id, hash, path };
            match send_request_to_daemon(&config.socket_path, req).await? {
                Response::Success { message } => println!("✅ {}", message),
                Response::Error { message } => eprintln!("❌ Fehler vom Daemon: {}", message),
                _ => eprintln!("❌ Unerwartete Antwort vom Daemon."),
            }
        }
        Commands::List => {
          let req = Request::List;
          match send_request_to_daemon(&config.socket_path, req).await? {
            Response::FileList { files } => {
             if files.is_empty() {
               println!("📂 Dein Space ist aktuell leer.");
               } else {
                 println!("\n{:<25} | {:<25} | {:<10} | {:<30}", "DATEINAME", "HASH (BLAKE3)", "GRÖSSE", "TICKET");
                 println!("{}", "-".repeat(98));
                 for f in files {
                   let short_hash = if f.blake3_hash.len() > 12 { &f.blake3_hash[..12] } else { &f.blake3_hash };
                   let short_ticket = if f.ticket.len() > 25 { &f.ticket[..25] } else { &f.ticket };
                   println!("{:<25} | {:<25} | {:<10} | {:<30}...", f.file_name, format!("{}...", short_hash), f.file_size, short_ticket);
                }
                println!();
            }
        }
        Response::Error { message } => eprintln!("❌ Fehler vom Daemon: {}", message),
        _ => eprintln!("❌ Unerwartete Antwort vom Daemon."),
       }
      }
    }

    Ok(())
}
