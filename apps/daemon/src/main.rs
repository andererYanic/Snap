use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use space_core::database::{JobState, PeerInfo, PendingJob};
use space_core::ipc::{read_frame, write_frame, Request, Response};
use space_core::{Config, Daemon, FileRecord};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tokio::net::UnixStream;

#[derive(Parser)]
#[command(
    name = "space-daemon",
    about = "P2P Virtual Distributed File System – Daemon & CLI",
    version
)]
struct Cli {
    /// Pfad zur Konfigurationsdatei (Standard: ~/.config/snap/config.toml oder $SPACE_CONFIG)
    #[arg(long, global = true, value_name = "DATEI")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Startet den Daemon im Vordergrund (beenden mit STRG+C oder SIGTERM)
    Start,
    /// Gibt eine lokale Datei frei und zeigt das Ticket an
    Publish {
        /// Pfad zur Datei
        path: PathBuf,
    },
    /// Lädt eine Datei per Ticket oder Hash herunter. Ist der Peer offline,
    /// landet der Auftrag in der Warteschlange und wird automatisch nachgeholt.
    Download {
        /// Iroh-Ticket oder Hash(-Präfix, mind. 6 Zeichen) aus `list`
        target: String,
        /// Zielpfad (Datei oder Ordner). Standard: Download-Ordner aus der Konfiguration
        #[arg(short, long, value_name = "PFAD")]
        output: Option<PathBuf>,
        /// Vorhandene Zieldatei überschreiben
        #[arg(short, long)]
        force: bool,
    },
    /// Entfernt eine Freigabe (Papierkorb). Die Datei wird sofort nicht mehr ausgeliefert.
    Delete {
        /// Hash(-Präfix) der Datei
        #[arg(long, conflicts_with = "path", required_unless_present = "path")]
        hash: Option<String>,
        /// Pfad der freigegebenen Datei
        #[arg(long)]
        path: Option<PathBuf>,
    },
    /// Listet alle freigegebenen Dateien
    List,
    /// Zeigt das vollständige Ticket einer freigegebenen Datei
    Ticket {
        /// Hash(-Präfix) der Datei
        hash: String,
    },
    /// Zeigt oder verwaltet die Offline-Warteschlange
    Queue {
        #[command(subcommand)]
        action: Option<QueueAction>,
    },
    /// Zeigt bekannte Peers mit zuletzt bekanntem Status
    Peers,
    /// Zeigt Node-ID, Pfade und Statistiken des laufenden Daemons
    Info,
}

#[derive(Subcommand)]
enum QueueAction {
    /// Entfernt einen Auftrag
    Cancel { id: i64 },
    /// Versucht einen Auftrag sofort erneut
    Retry { id: i64 },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("❌ {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let config = Config::load(cli.config.as_deref())?;

    let req = match cli.command {
        Commands::Start => {
            init_logging();
            return run_daemon(config).await.map(|_| ExitCode::SUCCESS);
        }
        Commands::Publish { path } => Request::Publish { path: absolute(&path)? },
        Commands::Download { target, output, force } => {
            let output = output.map(|o| absolute(&o)).transpose()?;
            eprintln!("📥 Lade herunter …");
            Request::Download { target, output, force }
        }
        Commands::Delete { hash, path } => Request::Delete {
            hash,
            path: path.map(|p| absolute(&p)).transpose()?,
        },
        Commands::List => Request::List,
        Commands::Ticket { hash } => Request::Ticket { hash },
        Commands::Queue { action: None } => Request::Queue,
        Commands::Queue { action: Some(QueueAction::Cancel { id }) } => Request::QueueCancel { id },
        Commands::Queue { action: Some(QueueAction::Retry { id }) } => Request::QueueRetry { id },
        Commands::Peers => Request::Peers,
        Commands::Info => Request::Info,
    };

    let res = send_request(&config.socket_path, &req).await?;
    Ok(print_response(res))
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,iroh=warn,iroh_blobs=warn,iroh_relay=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).init();
}

/// Relative Pfade werden im Verzeichnis der CLI aufgelöst – nicht in dem des Daemons.
fn absolute(p: &Path) -> Result<PathBuf> {
    std::path::absolute(p).with_context(|| format!("Pfad {} nicht auflösbar", p.display()))
}

async fn run_daemon(config: Config) -> Result<()> {
    let daemon = Daemon::start(config).await?;
    let c = daemon.config();
    println!("🆔 Node-ID:      {}", daemon.node_id());
    println!("🔌 IPC-Socket:   {}", c.socket_path.display());
    println!("💾 Daten:        {}", c.data_dir.display());
    println!("📂 Downloads:    {}", c.download_dir.display());
    if !c.use_relay {
        println!("🔒 Relais deaktiviert – nur direkte Verbindungen.");
    }
    println!("✅ Daemon aktiv. Beenden mit STRG+C.");

    wait_for_shutdown_signal().await?;
    println!("🛑 Daemon wird heruntergefahren …");
    daemon.shutdown().await;
    Ok(())
}

async fn wait_for_shutdown_signal() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r?,
        _ = term.recv() => {}
    }
    Ok(())
}

async fn send_request(socket: &Path, req: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(socket).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => anyhow::anyhow!(
            "Der Daemon läuft nicht (kein Socket unter {}).\n   Starte ihn mit: space-daemon start",
            socket.display()
        ),
        std::io::ErrorKind::PermissionDenied => anyhow::anyhow!(
            "Keine Berechtigung für den Daemon-Socket {} – läuft der Daemon unter einem anderen Benutzer?",
            socket.display()
        ),
        _ => anyhow::anyhow!("Verbindung zum Daemon fehlgeschlagen: {e}"),
    })?;
    write_frame(&mut stream, req).await?;
    read_frame(&mut stream)
        .await
        .context("Keine gültige Antwort vom Daemon (wurde die Verbindung abgelehnt?)")
}

fn print_response(res: Response) -> ExitCode {
    match res {
        Response::Success { message } => println!("✅ {message}"),
        Response::Published { ticket, hash, file_name, already_shared } => {
            if already_shared {
                println!("ℹ️  '{file_name}' war bereits freigegeben.");
            } else {
                println!("✅ '{file_name}' freigegeben.");
            }
            println!("   Hash:   {hash}");
            println!("   Ticket: {ticket}");
        }
        Response::Downloaded { path } => println!("✅ Gespeichert unter: {}", path.display()),
        Response::Queued { job_id, peer, reason, already_queued } => {
            println!("⏳ Peer {} ist gerade nicht erreichbar.", short(&peer, 10));
            println!("   Grund: {reason}");
            if already_queued {
                println!("   Auftrag {job_id} war bereits in der Warteschlange.");
            } else {
                println!("   Auftrag {job_id} wurde in die Warteschlange aufgenommen.");
            }
            println!("   Der Download startet automatisch, sobald der Peer wieder erreichbar ist.");
            println!("   Status: space-daemon queue");
        }
        Response::Ticket { ticket } => println!("{ticket}"),
        Response::FileList { files } => print_files(&files),
        Response::Queue { jobs } => print_jobs(&jobs),
        Response::Peers { peers } => print_peers(&peers),
        Response::Info(i) => {
            println!("Node-ID:        {}", i.node_id);
            println!("Daten:          {}", i.data_dir.display());
            println!("IPC-Socket:     {}", i.socket_path.display());
            println!("Downloads:      {}", i.download_dir.display());
            println!("Relais:         {}", if i.use_relay { "n0 (öffentlich)" } else { "deaktiviert" });
            println!("Freigaben:      {}", i.shared_files);
            println!("Warteschlange:  {} wartend", i.pending_jobs);
        }
        Response::Error { message } => {
            eprintln!("❌ {message}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn print_files(files: &[FileRecord]) {
    if files.is_empty() {
        println!("📭 Noch keine Dateien freigegeben. Freigeben mit: space-daemon publish <datei>");
        return;
    }
    println!("{:<14} {:>10}  {:<16}  DATEI", "HASH", "GRÖSSE", "FREIGEGEBEN");
    for f in files {
        println!(
            "{:<14} {:>10}  {:<16}  {}",
            short(&f.blake3_hash, 12),
            human_size(f.file_size),
            fmt_time(f.created_at),
            f.path
        );
    }
    println!("\nTicket anzeigen: space-daemon ticket <hash>");
}

fn print_jobs(jobs: &[PendingJob]) {
    if jobs.is_empty() {
        println!("✅ Warteschlange ist leer.");
        return;
    }
    let now = chrono::Utc::now().timestamp();
    for j in jobs {
        let state = match j.state {
            JobState::Failed => "❌ fehlgeschlagen".to_string(),
            JobState::Waiting => match j.next_attempt_at {
                Some(t) if t > now => format!("⏳ nächster Versuch in {}", human_duration(t - now)),
                _ => "⏳ wird gleich versucht".to_string(),
            },
        };
        println!("[{}] {}  von Peer {}", j.id, short(&j.target_hash, 12), short(&j.peer_id, 10));
        println!("     Ziel:     {}{}", j.output_path, if j.explicit_output { "" } else { " (Standardordner)" });
        println!("     Status:   {state} · {} Versuch(e)", j.attempts);
        if let Some(err) = &j.last_error {
            println!("     Fehler:   {err}");
        }
    }
    println!("\nAbbrechen: space-daemon queue cancel <id> · Sofort versuchen: space-daemon queue retry <id>");
}

fn print_peers(peers: &[PeerInfo]) {
    if peers.is_empty() {
        println!("Noch keine Peers bekannt. (Ein Peer wird beim ersten Download-Versuch erfasst.)");
        return;
    }
    let now = chrono::Utc::now().timestamp();
    println!("{:<12} {:<10} {:<22} NÄCHSTER VERSUCH", "PEER", "STATUS", "ZULETZT ERREICHT");
    for p in peers {
        let status = if p.status == "online" { "🟢 online" } else { "🔴 offline" };
        let seen = p.last_seen.map(fmt_time).unwrap_or_else(|| "nie".into());
        let retry = match p.next_retry_at {
            Some(t) if t > now => format!("in {}", human_duration(t - now)),
            _ => "-".into(),
        };
        println!("{:<12} {:<10} {:<22} {}", short(&p.node_id, 10), status, seen, retry);
    }
    println!("\nDer Status beruht nur auf echten Verbindungen – es werden keine Pings gesendet.");
}

fn short(s: &str, n: usize) -> &str {
    &s[..s.len().min(n)]
}

fn fmt_time(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|t| t.format("%d.%m.%Y %H:%M").to_string())
        .unwrap_or_else(|| "?".into())
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn human_duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s} s"),
        s if s < 3600 => format!("{} min", s / 60),
        s => format!("{} h {} min", s / 3600, (s % 3600) / 60),
    }
}
