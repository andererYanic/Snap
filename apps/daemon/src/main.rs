use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use space_core::database::{DeviceInfo, JobState, PeerInfo, PendingJob, RemoteFile};
use space_core::ipc::{read_frame, write_frame, Request, Response, SyncResult};
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
    /// Lädt eine Datei per Ticket, Hash oder Dateiname herunter. Ist das Gerät offline,
    /// landet der Auftrag in der Warteschlange und wird automatisch nachgeholt.
    Download {
        /// Ticket, Hash(-Präfix, mind. 6 Zeichen) oder Dateiname aus `list`
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
    /// Zeigt eigene Freigaben und die Dateien gekoppelter Geräte
    List,
    /// Gleicht die Dateilisten sofort mit allen gekoppelten Geräten ab
    Sync,
    /// Geräte koppeln und verwalten
    Pair {
        #[command(subcommand)]
        action: PairAction,
    },
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
enum PairAction {
    /// Erzeugt einen Einmal-Code (10 min gültig) und wartet auf das neue Gerät
    Invite,
    /// Koppelt dieses Gerät mit einem Einladungscode
    Join { code: String },
    /// Zeigt gekoppelte Geräte
    List,
    /// Entkoppelt ein Gerät (Name oder Node-ID-Präfix)
    Remove { device: String },
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
        Commands::Sync => {
            eprintln!("🔄 Gleiche mit gekoppelten Geräten ab …");
            Request::Sync
        }
        Commands::Pair { action: PairAction::Invite } => return pair_invite(&config.socket_path).await,
        Commands::Pair { action: PairAction::Join { code } } => {
            eprintln!("🔗 Verbinde …");
            Request::PairJoin { code }
        }
        Commands::Pair { action: PairAction::List } => Request::PairList,
        Commands::Pair { action: PairAction::Remove { device } } => Request::PairRemove { device },
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

/// Zeigt den Einladungscode und wartet, bis sich ein Gerät damit koppelt.
async fn pair_invite(socket: &Path) -> Result<ExitCode> {
    let (code, expires_at) = match send_request(socket, &Request::PairInvite).await? {
        Response::PairInvite { code, expires_at } => (code, expires_at),
        other => return Ok(print_response(other)),
    };
    println!("🔑 Einladungscode (einmalig, gültig bis {}):\n", fmt_time(expires_at));
    println!("   {code}\n");
    println!("Auf dem neuen Gerät ausführen:");
    println!("   space-daemon pair join {code}\n");
    println!("⚠️  Den Code nur über einen sicheren Weg übertragen – wer ihn hat, kann sich koppeln.");
    println!("⏳ Warte auf das neue Gerät … (STRG+C bricht das Warten ab, der Code bleibt bis zum Ablauf gültig)");
    let res = send_request(socket, &Request::PairInviteWait).await?;
    Ok(print_response(res))
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
        Response::Catalog { local, remote, devices, peers, syncing } => {
            print_catalog(&local, &remote, &devices, &peers, syncing)
        }
        Response::SyncReport { results } => print_sync(&results),
        Response::PairInvite { code, .. } => println!("{code}"),
        Response::Paired { device } => {
            println!("✅ Gekoppelt mit '{}' ({})", device.name, short(&device.node_id, 10));
            println!("   Die Dateilisten werden jetzt im Hintergrund abgeglichen. Anzeigen: space-daemon list");
        }
        Response::Devices { devices } => print_devices(&devices),
        Response::Queue { jobs } => print_jobs(&jobs),
        Response::Peers { peers } => print_peers(&peers),
        Response::Info(i) => {
            println!("Node-ID:        {}", i.node_id);
            println!("Daten:          {}", i.data_dir.display());
            println!("IPC-Socket:     {}", i.socket_path.display());
            println!("Downloads:      {}", i.download_dir.display());
            println!("Gerätename:     {}", i.device_name);
            println!("Relais:         {}", if i.use_relay { "n0 (öffentlich)" } else { "deaktiviert" });
            println!("Gekoppelt:      {} Gerät(e)", i.paired_devices);
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

fn print_catalog(
    local: &[FileRecord],
    remote: &[RemoteFile],
    devices: &[DeviceInfo],
    peers: &[PeerInfo],
    syncing: bool,
) {
    let now = chrono::Utc::now().timestamp();
    println!("📁 Dieses Gerät");
    if local.is_empty() {
        println!("   (noch nichts freigegeben – space-daemon publish <datei>)");
    } else {
        for f in local {
            println!(
                "   {:<14} {:>10}  {}",
                short(&f.blake3_hash, 12),
                human_size(f.file_size),
                f.path
            );
        }
    }

    // Einträge nach Ursprungsgerät gruppieren (Reihenfolge kommt sortiert vom Daemon).
    let mut groups: Vec<(&str, Vec<&RemoteFile>)> = Vec::new();
    for f in remote {
        match groups.last_mut() {
            Some((origin, list)) if *origin == f.origin => list.push(f),
            _ => groups.push((&f.origin, vec![f])),
        }
    }
    for (origin, files) in &groups {
        let device = devices.iter().find(|d| d.node_id == *origin);
        let name = device.map(|d| d.name.clone()).unwrap_or_else(|| format!("Gerät {}", short(origin, 10)));
        let status = match peers.iter().find(|p| p.node_id == *origin).map(|p| p.status.as_str()) {
            Some("online") => "🟢",
            Some(_) => "🔴",
            None => "⚪",
        };
        let via = if device.is_none() { " · über anderes Gerät" } else { "" };
        println!("\n{status} {name}{via}");
        for f in files {
            println!("   {:<14} {:>10}  {}", short(&f.hash, 12), human_size(f.size), f.name);
        }
    }
    // Gekoppelte Geräte ohne Dateien trotzdem anzeigen
    for d in devices.iter().filter(|d| !groups.iter().any(|(o, _)| *o == d.node_id)) {
        println!("\n⚪ {} – keine freigegebenen Dateien bekannt", d.name);
    }

    if !devices.is_empty() {
        let oldest = devices.iter().map(|d| d.last_sync_at).min().flatten();
        let stand = match oldest {
            Some(t) => format!("Stand: vor {}", human_duration(now - t)),
            None => "noch nicht abgeglichen".into(),
        };
        if syncing {
            println!("\n🔄 {stand} – Abgleich läuft im Hintergrund, gleich erneut `list` aufrufen.");
        } else {
            println!("\n{stand}");
        }
    } else {
        println!("\nNoch keine Geräte gekoppelt: space-daemon pair invite");
    }
    println!("Herunterladen: space-daemon download <name|hash> · 🟢 erreichbar 🔴 zuletzt nicht erreichbar ⚪ unbekannt");
}

fn print_sync(results: &[SyncResult]) {
    if results.is_empty() {
        println!("Noch keine Geräte gekoppelt: space-daemon pair invite");
        return;
    }
    for r in results {
        let name = if r.name.is_empty() { short(&r.node_id, 10).to_string() } else { r.name.clone() };
        if r.ok {
            println!("✅ {name}: {} neue Einträge", r.new_entries);
        } else {
            println!("🔴 {name}: {}", r.error.as_deref().unwrap_or("Fehler"));
        }
    }
}

fn print_devices(devices: &[DeviceInfo]) {
    if devices.is_empty() {
        println!("Noch keine Geräte gekoppelt. Neues Gerät koppeln: space-daemon pair invite");
        return;
    }
    let now = chrono::Utc::now().timestamp();
    println!("{:<20} {:<12} {:<18} LETZTER ABGLEICH", "NAME", "NODE-ID", "GEKOPPELT");
    for d in devices {
        let sync = d.last_sync_at.map(|t| format!("vor {}", human_duration(now - t))).unwrap_or_else(|| "nie".into());
        println!("{:<20} {:<12} {:<18} {}", d.name, short(&d.node_id, 10), fmt_time(d.paired_at), sync);
    }
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
