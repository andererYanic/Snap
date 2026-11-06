use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use space_core::database::{AuditEntry, DeviceInfo, JobState, PeerInfo, PendingJob, RemoteFile, ShareInfo};
use space_core::events::Event;
use space_core::fileops::jobs::FileOp;
use space_core::fileops::ops::Conflict;
use space_core::index::NodeKind;
use space_core::ipc::{read_frame, write_frame, Envelope, PinInfo, Request, Response, ScanResult, SyncResult};
use space_core::remote_ops::RemoteOp;
use space_core::{Config, Daemon, FileRecord};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tokio::net::UnixStream;

#[derive(Parser)]
#[command(name = "space-daemon", about = "P2P Virtual Distributed File System – Daemon & CLI", version)]
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
    /// Zeigt Ereignisse des Daemons live an (STRG+C beendet)
    Watch,
    /// Ordner freigeben und verwalten (wie Google Drive)
    Share {
        #[command(subcommand)]
        action: ShareAction,
    },
    /// Freigaben anderer Geräte offline verfügbar halten (Smart-Pinning)
    Pin {
        #[command(subcommand)]
        action: PinAction,
    },
    /// Zeigt, was gekoppelte Geräte versucht haben (Audit-Log)
    Audit {
        #[arg(short, long, default_value_t = 50)]
        limit: u32,
    },
    /// Sucht Dateien und Ordner auf diesem Gerät (Teil des Namens, ohne Groß-/Kleinschreibung)
    Search {
        query: String,
        /// Nur diese Endungen, kommagetrennt (z. B. jpg,png)
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// Nur Ordner
        #[arg(long)]
        dirs: bool,
        /// Nur Dateien
        #[arg(long, conflicts_with = "dirs")]
        files: bool,
        /// Versteckte Dateien einbeziehen
        #[arg(long)]
        hidden: bool,
        #[arg(short, long, default_value_t = 50)]
        limit: u32,
    },
    /// Ordnerinhalt anzeigen (live vom Dateisystem)
    Ls {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Versteckte Dateien anzeigen
        #[arg(short, long)]
        all: bool,
    },
    /// Orte der lokalen Suche verwalten
    Index {
        #[command(subcommand)]
        action: IndexAction,
    },
    /// Kopieren: cp <quelle>… <zielordner>
    Cp {
        #[arg(required = true, num_args = 2..)]
        paths: Vec<PathBuf>,
        /// Wenn das Ziel existiert
        #[arg(long, value_enum, default_value_t = OnConflict::Fail)]
        on_conflict: OnConflict,
    },
    /// Verschieben: mv <quelle>… <zielordner>
    Mv {
        #[arg(required = true, num_args = 2..)]
        paths: Vec<PathBuf>,
        #[arg(long, value_enum, default_value_t = OnConflict::Fail)]
        on_conflict: OnConflict,
    },
    /// Umbenennen (im selben Ordner)
    Rename { path: PathBuf, new_name: String },
    /// Ordner anlegen
    Mkdir { path: PathBuf },
    /// In den Papierkorb verschieben (mit --permanent endgültig löschen)
    Rm {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Endgültig löschen statt Papierkorb (nicht rückgängig zu machen)
        #[arg(long)]
        permanent: bool,
    },
    /// Letzte Dateioperation rückgängig machen
    Undo,
    /// Papierkorb anzeigen und verwalten
    Trash {
        #[command(subcommand)]
        action: Option<TrashAction>,
    },
    /// Dateien in Freigaben eines gekoppelten Geräts bearbeiten (Pfade: Freigabe/pfad)
    Remote {
        /// Gerätename oder Node-ID-Präfix
        device: String,
        #[command(subcommand)]
        action: RemoteAction,
    },
    /// Angehaltene Änderungen anderer Geräte (Massenschutz) bestätigen oder ablehnen
    Ops {
        #[command(subcommand)]
        action: Option<OpsAction>,
    },
    /// Vorschaubild erzeugen und Pfad im Cache anzeigen
    Thumb {
        path: PathBuf,
        /// 1024 px statt 256 px
        #[arg(long)]
        large: bool,
    },
    /// Metadaten anzeigen (Aufnahmedatum, Kamera, GPS, Dauer, Titel …)
    Meta { path: PathBuf },
    /// Lokale Abspiel-Adresse für eine Datei (auch von anderen Geräten), z. B. `mpv $(space-daemon stream Film.mkv)`
    Stream { target: String },
    /// Hochladen auf ein gekoppeltes Gerät: upload <datei> <gerät>:<Freigabe[/ordner]>, upload list …
    Upload {
        #[command(subcommand)]
        action: UploadAction,
    },
    /// Freigabelinks für Personen ohne Snap (verschlüsselt über die eigene Domain)
    Links {
        #[command(subcommand)]
        action: Option<LinksAction>,
    },
    /// Speicheranalyse: was belegt wie viel Platz (indexierte Ordner)
    Usage {
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Anzahl Unterordner/Dateien
        #[arg(short, long, default_value_t = 20)]
        limit: u32,
    },
    /// Doppelte Dateien finden (nur anzeigen; löschen mit `rm`)
    Dupes {
        /// Nur in diesem Ordner (Standard: alle indexierten Orte)
        path: Option<PathBuf>,
        /// Mindestgröße in MB
        #[arg(long, default_value_t = 1.0)]
        min_mb: f64,
        #[arg(short, long, default_value_t = 50)]
        limit: u32,
    },
    /// Foto-Zeitachse: Bilder und Videos nach Aufnahmezeit (neueste zuerst)
    Photos {
        /// Nur in diesem Ordner (Standard: alle indexierten Orte)
        path: Option<PathBuf>,
        /// Weiter ab diesem Cursor (aus der vorherigen Ausgabe)
        #[arg(long)]
        before: Option<String>,
        #[arg(short, long, default_value_t = 50)]
        limit: u32,
        /// Nur die Anzahl je Monat anzeigen
        #[arg(long)]
        months: bool,
    },
    /// Sicherungen und Versionen (verschlüsselt, dedupliziert)
    Backup {
        #[command(subcommand)]
        action: Option<BackupAction>,
    },
    /// Grenzen anzeigen und sofort ändern (ohne Neustart)
    Settings {
        #[command(subcommand)]
        action: Option<SettingsAction>,
    },
}

#[derive(Subcommand)]
enum UploadAction {
    /// Datei(en) hochladen: send <datei>… <gerät>:<Freigabe[/ordner]>
    Send {
        #[arg(required = true, num_args = 2..)]
        args: Vec<String>,
    },
    /// Warteschlange mit Status
    List,
    /// Auftrag entfernen
    Cancel {
        id: i64,
    },
    /// Wartende/fehlgeschlagene sofort erneut versuchen
    Retry,
    /// Ordner automatisch hochladen (Kamera-Upload): rule-add <ordner> <gerät>:<Freigabe[/ordner]>
    RuleAdd {
        folder: PathBuf,
        target: String,
    },
    RuleList,
    RuleRemove {
        id: i64,
    },
}

/// "PC:Fotos/Handy" → ("PC", "Fotos", "Handy")
fn parse_upload_target(t: &str) -> Result<(String, String, String)> {
    let (dev, rest) = t.split_once(':').context("Ziel bitte als <gerät>:<Freigabe[/ordner]> angeben")?;
    let (share, dir) = split_share(rest);
    if dev.is_empty() || share.is_empty() {
        anyhow::bail!("Ziel bitte als <gerät>:<Freigabe[/ordner]> angeben");
    }
    Ok((dev.to_string(), share, dir))
}

#[derive(Subcommand)]
enum LinksAction {
    /// Links anzeigen (Standard)
    List,
    /// Link erstellen: Datei wird verschlüsselt hochgeladen
    Create {
        file: PathBuf,
        /// Gültigkeit in Tagen (Standard: Einstellung link_default_days)
        #[arg(long)]
        days: Option<u32>,
        /// Höchstzahl Downloads (Standard: unbegrenzt)
        #[arg(long)]
        max_downloads: Option<u32>,
        /// Zusätzlich ein Passwort verlangen (wird verdeckt abgefragt)
        #[arg(long)]
        password: bool,
    },
    /// Link widerrufen (ID oder Anfang der ID); löscht die verschlüsselte Kopie
    Revoke { id: String },
    /// Einrichten: Worker-Adresse und R2-Zugang (geheimer Schlüssel wird verdeckt abgefragt)
    Setup {
        /// Öffentliche Adresse des Workers, z. B. https://share.example.de
        #[arg(long)]
        url: String,
        /// R2-Endpunkt, z. B. https://<konto-id>.r2.cloudflarestorage.com
        #[arg(long)]
        endpoint: String,
        #[arg(long)]
        bucket: String,
        /// Access Key ID des R2-API-Tokens
        #[arg(long)]
        key_id: String,
    },
}

/// Liest ein Geheimnis von der Konsole, ohne es anzuzeigen (nie über die Kommandozeile).
fn read_secret(prompt: &str) -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let stdin = std::io::stdin();
    let tty = stdin.is_terminal();
    // SAFETY: termios wird vollständig von tcgetattr gefüllt und unverändert zurückgeschrieben.
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    if tty && unsafe { libc::tcgetattr(0, &mut old) } == 0 {
        let mut silent = old;
        silent.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &silent) };
    }
    let mut line = String::new();
    let res = stdin.lock().read_line(&mut line);
    if tty {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &old) };
        eprintln!();
    }
    res?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

#[derive(Subcommand)]
enum BackupAction {
    /// Sicherungen anzeigen (Standard)
    List,
    /// Einrichten: --to <ordner auf externer Platte | r2> <quellordner>… (Passwort wird verdeckt abgefragt)
    Create {
        #[arg(long)]
        to: String,
        #[arg(required = true)]
        sources: Vec<PathBuf>,
    },
    /// Bestehende Sicherung auf diesem Gerät verbinden (Passwort oder Wiederherstellungsschlüssel)
    Connect {
        #[arg(long)]
        from: String,
        /// Sicherungs-ID (32 Zeichen, steht im Ordnernamen snap-backup-…)
        #[arg(long)]
        id: String,
    },
    /// Quellordner festlegen
    Sources { id: String, sources: Vec<PathBuf> },
    /// Jetzt sichern
    Run { id: String },
    /// Gesicherte Stände
    Snapshots { id: String },
    /// Inhalt eines Ordners in einem Stand
    Ls { id: String, snapshot: String, path: PathBuf },
    /// Datei/Ordner wiederherstellen (nie überschreibend)
    Restore {
        id: String,
        snapshot: String,
        path: PathBuf,
        #[arg(long, default_value = ".")]
        to: PathBuf,
    },
    /// Prüfen (alle Bäume + Stichprobe; --all: alle Stücke)
    Check {
        id: String,
        #[arg(long)]
        all: bool,
    },
    /// Von diesem Gerät entfernen (Daten am Ziel bleiben)
    Remove { id: String },
}

fn backup_target(s: &str) -> Result<space_core::backup::repo::BackupTarget> {
    use space_core::backup::repo::BackupTarget;
    Ok(match s {
        "r2" | "s3" => BackupTarget::S3,
        p => BackupTarget::Dir { path: absolute(Path::new(p))? },
    })
}

#[derive(Subcommand)]
enum SettingsAction {
    /// Alle Einstellungen mit Wert, Standard und Bereich (Standard)
    List,
    /// Wert setzen, z. B. `settings set upload_daily_gb 50`
    Set { key: String, value: i64 },
    /// Zurück zum Standard aus config.toml
    Reset { key: String },
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Umbenennen: rename Freigabe/pfad neuer-name
    Rename { path: String, new_name: String },
    /// Verschieben innerhalb einer Freigabe: mv Freigabe/a… Freigabe/zielordner
    Mv {
        #[arg(required = true, num_args = 2..)]
        paths: Vec<String>,
        #[arg(long, value_enum, default_value_t = OnConflict::Fail)]
        on_conflict: OnConflict,
    },
    /// Kopieren innerhalb einer Freigabe
    Cp {
        #[arg(required = true, num_args = 2..)]
        paths: Vec<String>,
        #[arg(long, value_enum, default_value_t = OnConflict::Fail)]
        on_conflict: OnConflict,
    },
    /// Ordner anlegen: mkdir Freigabe/pfad/neu
    Mkdir { path: String },
    /// Vorschaubild einer Datei des Geräts (braucht dort files.read)
    Thumb {
        path: String,
        #[arg(long)]
        large: bool,
    },
    /// In den Papierkorb des Geräts (mit --permanent endgültig, braucht files.purge)
    Rm {
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(long)]
        permanent: bool,
    },
}

#[derive(Subcommand)]
enum OpsAction {
    /// Offene Anfragen anzeigen (Standard)
    Pending,
    /// Anfrage ausführen
    Approve { id: i64 },
    /// Anfrage verwerfen
    Reject { id: i64 },
}

/// "Fotos/2024/a.jpg" → ("Fotos", "2024/a.jpg"); "Fotos" → ("Fotos", "").
fn split_share(p: &str) -> (String, String) {
    let p = p.trim_matches('/');
    match p.split_once('/') {
        Some((s, r)) => (s.to_string(), r.to_string()),
        None => (p.to_string(), String::new()),
    }
}

/// Alle Pfade müssen in derselben Freigabe liegen.
fn same_share(paths: &[String]) -> Result<(String, Vec<String>)> {
    let parts: Vec<(String, String)> = paths.iter().map(|p| split_share(p)).collect();
    let share = parts.first().map(|p| p.0.clone()).unwrap_or_default();
    if parts.iter().any(|p| p.0 != share) {
        anyhow::bail!("Alle Pfade müssen in derselben Freigabe liegen.");
    }
    Ok((share, parts.into_iter().map(|p| p.1).collect()))
}

fn remote_op(action: RemoteAction) -> Result<RemoteOp> {
    Ok(match action {
        RemoteAction::Rename { path, new_name } => {
            let (share, path) = split_share(&path);
            RemoteOp::Rename { share, path, new_name }
        }
        RemoteAction::Mv { paths, on_conflict } => {
            let (share, mut rels) = same_share(&paths)?;
            let dest_dir = rels.pop().unwrap_or_default();
            RemoteOp::Move { share, paths: rels, dest_dir, conflict: on_conflict.into() }
        }
        RemoteAction::Cp { paths, on_conflict } => {
            let (share, mut rels) = same_share(&paths)?;
            let dest_dir = rels.pop().unwrap_or_default();
            RemoteOp::Copy { share, paths: rels, dest_dir, conflict: on_conflict.into() }
        }
        RemoteAction::Mkdir { path } => {
            let (share, rel) = split_share(&path);
            let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel.as_str()));
            RemoteOp::Mkdir { share, parent: parent.to_string(), name: name.to_string() }
        }
        RemoteAction::Thumb { .. } => anyhow::bail!("intern: Vorschaubild ist keine Dateioperation"),
        RemoteAction::Rm { paths, permanent } => {
            let (share, paths) = same_share(&paths)?;
            if permanent {
                RemoteOp::Purge { share, paths }
            } else {
                RemoteOp::Trash { share, paths }
            }
        }
    })
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OnConflict {
    /// Abbrechen
    Fail,
    /// Auslassen
    Skip,
    /// Beide behalten („Name (2)“)
    KeepBoth,
    /// Altes Ziel in den Papierkorb und ersetzen
    Replace,
}

impl From<OnConflict> for Conflict {
    fn from(c: OnConflict) -> Self {
        match c {
            OnConflict::Fail => Conflict::Fail,
            OnConflict::Skip => Conflict::Skip,
            OnConflict::KeepBoth => Conflict::KeepBoth,
            OnConflict::Replace => Conflict::Replace,
        }
    }
}

#[derive(Subcommand)]
enum TrashAction {
    /// Einträge anzeigen (Standard)
    List,
    /// Eintrag wiederherstellen (Nummer aus `trash list` oder Kennung)
    Restore {
        entry: String,
        /// Bei belegtem Ziel freien Namen wählen
        #[arg(long)]
        keep_both: bool,
    },
    /// Eintrag endgültig löschen
    Purge { entry: String },
    /// Papierkorb leeren (endgültig)
    Empty,
}

#[derive(Subcommand)]
enum IndexAction {
    /// Indexierte Orte mit Anzahl der Einträge
    List,
    /// Ordner zur Suche hinzufügen
    Add { path: PathBuf },
    /// Ort aus der Suche entfernen
    Remove { id: i64 },
    /// Alle Orte sofort abgleichen
    Rescan,
}

#[derive(Subcommand)]
enum PinAction {
    /// Spiegeln: "Fotos", "Fotos/2024" oder eindeutig "PC:Fotos/2024"
    Add { target: String },
    /// Pins mit Fortschritt, Konflikten und Platzbedarf
    List,
    /// Pin entfernen (gespiegelte Dateien bleiben liegen)
    Remove { id: i64 },
}

#[derive(Subcommand)]
enum ShareAction {
    /// Ordner freigeben. Ohne --devices: alle gekoppelten Geräte (auch künftige)
    Add {
        path: PathBuf,
        /// Anzeigename (Standard: Ordnername)
        #[arg(long)]
        name: Option<String>,
        /// Nur diese Geräte (Name oder Node-ID-Präfix), kommagetrennt
        #[arg(long, value_delimiter = ',')]
        devices: Option<Vec<String>>,
    },
    /// Freigaben mit Dateizahl und Größe anzeigen
    List,
    /// Freigabe beenden (Dateien bleiben auf der Platte)
    Remove { name: String },
    /// Sofort neu scannen (ohne Name: alle)
    Rescan { name: Option<String> },
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
    /// Gewährt einem Gerät ein Recht: catalog.read, files.read, files.write,
    /// pin.request, files.delete (files.* nur mit --folder)
    Grant {
        device: String,
        capability: String,
        #[arg(long, value_name = "ORDNER")]
        folder: Option<PathBuf>,
    },
    /// Entzieht ein Recht (ohne --folder: in allen Ordnern)
    Revoke {
        device: String,
        capability: String,
        #[arg(long, value_name = "ORDNER")]
        folder: Option<PathBuf>,
    },
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
        Commands::Delete { hash, path } => Request::Delete { hash, path: path.map(|p| absolute(&p)).transpose()? },
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
        Commands::Pair { action: PairAction::Grant { device, capability, folder } } => {
            Request::PairGrant { device, capability, folder: folder.map(|f| absolute(&f)).transpose()? }
        }
        Commands::Pair { action: PairAction::Revoke { device, capability, folder } } => {
            Request::PairRevoke { device, capability, folder: folder.map(|f| absolute(&f)).transpose()? }
        }
        Commands::Audit { limit } => Request::Audit { limit },
        Commands::Share { action: ShareAction::Add { path, name, devices } } => {
            Request::ShareAdd { path: absolute(&path)?, name, devices }
        }
        Commands::Share { action: ShareAction::List } => Request::ShareList,
        Commands::Pin { action: PinAction::Add { target } } => Request::PinAdd { target },
        Commands::Pin { action: PinAction::List } => Request::PinList,
        Commands::Pin { action: PinAction::Remove { id } } => Request::PinRemove { id },
        Commands::Share { action: ShareAction::Remove { name } } => Request::ShareRemove { name },
        Commands::Share { action: ShareAction::Rescan { name } } => {
            eprintln!("🔍 Scanne …");
            Request::ShareRescan { name }
        }
        Commands::Ticket { hash } => Request::Ticket { hash },
        Commands::Queue { action: None } => Request::Queue,
        Commands::Queue { action: Some(QueueAction::Cancel { id }) } => Request::QueueCancel { id },
        Commands::Queue { action: Some(QueueAction::Retry { id }) } => Request::QueueRetry { id },
        Commands::Peers => Request::Peers,
        Commands::Info => Request::Info,
        Commands::Search { query, ext, dirs, files, hidden, limit } => Request::Search {
            query,
            filter: space_core::index::SearchFilter {
                kind: if dirs {
                    Some(NodeKind::Dir)
                } else if files {
                    Some(NodeKind::File)
                } else {
                    None
                },
                extensions: ext,
                include_hidden: hidden,
                ..Default::default()
            },
            limit,
        },
        Commands::Ls { path, all } => {
            Request::ListDir { path: absolute(&path)?, show_hidden: all, offset: 0, limit: 1_000 }
        }
        Commands::Index { action: IndexAction::List } => Request::IndexRoots,
        Commands::Index { action: IndexAction::Add { path } } => Request::IndexAdd { path: absolute(&path)? },
        Commands::Index { action: IndexAction::Remove { id } } => Request::IndexRemove { id },
        Commands::Index { action: IndexAction::Rescan } => Request::IndexRescan,
        Commands::Cp { mut paths, on_conflict } => {
            let dest = absolute(&paths.pop().unwrap_or_default())?;
            let sources = paths.iter().map(|p| absolute(p)).collect::<Result<Vec<_>>>()?;
            Request::FileOp { op: FileOp::Copy { sources, dest, conflict: on_conflict.into() }, wait: true }
        }
        Commands::Mv { mut paths, on_conflict } => {
            let dest = absolute(&paths.pop().unwrap_or_default())?;
            let sources = paths.iter().map(|p| absolute(p)).collect::<Result<Vec<_>>>()?;
            Request::FileOp { op: FileOp::Move { sources, dest, conflict: on_conflict.into() }, wait: true }
        }
        Commands::Rename { path, new_name } => {
            Request::FileOp { op: FileOp::Rename { path: absolute(&path)?, new_name }, wait: true }
        }
        Commands::Mkdir { path } => {
            let path = absolute(&path)?;
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                anyhow::bail!("Ungültiger Ordnerpfad");
            };
            let op = FileOp::Mkdir { parent: parent.to_path_buf(), name: name.to_string_lossy().into_owned() };
            Request::FileOp { op, wait: true }
        }
        Commands::Rm { paths, permanent } => {
            let paths = paths.iter().map(|p| absolute(p)).collect::<Result<Vec<_>>>()?;
            let op = if permanent { FileOp::Delete { paths } } else { FileOp::Trash { paths } };
            Request::FileOp { op, wait: true }
        }
        Commands::Undo => Request::Undo,
        Commands::Trash { action: None | Some(TrashAction::List) } => Request::TrashList,
        Commands::Trash { action: Some(TrashAction::Restore { entry, keep_both }) } => {
            Request::TrashRestore { id: trash_id(&config.socket_path, &entry).await?, keep_both }
        }
        Commands::Trash { action: Some(TrashAction::Purge { entry }) } => {
            Request::TrashPurge { id: trash_id(&config.socket_path, &entry).await? }
        }
        Commands::Trash { action: Some(TrashAction::Empty) } => Request::TrashEmpty,
        Commands::Remote { device, action: RemoteAction::Thumb { path, large } } => {
            let (share, path) = split_share(&path);
            let size = if large { space_core::media::ThumbSize::Large } else { space_core::media::ThumbSize::Small };
            Request::RemoteThumbnail { device, share, path, size }
        }
        Commands::Remote { device, action } => Request::RemoteOp { device, op: remote_op(action)? },
        Commands::Ops { action: None | Some(OpsAction::Pending) } => Request::OpsPending,
        Commands::Ops { action: Some(OpsAction::Approve { id }) } => Request::OpsApprove { id },
        Commands::Ops { action: Some(OpsAction::Reject { id }) } => Request::OpsReject { id },
        Commands::Thumb { path, large } => Request::Thumbnail {
            path: absolute(&path)?,
            size: if large { space_core::media::ThumbSize::Large } else { space_core::media::ThumbSize::Small },
        },
        Commands::Meta { path } => Request::Metadata { path: absolute(&path)? },
        Commands::Stream { target } => Request::StreamUrl { target },
        Commands::Upload { action: UploadAction::Send { mut args } } => {
            let (device, share, dest_dir) = parse_upload_target(&args.pop().unwrap_or_default())?;
            for f in &args {
                let req = Request::Upload {
                    device: device.clone(),
                    share: share.clone(),
                    dest_dir: dest_dir.clone(),
                    path: absolute(Path::new(f))?,
                };
                if let r @ Response::Error { .. } = send_request(&config.socket_path, &req).await? {
                    return Ok(print_response(r));
                }
            }
            println!("⏳ {} Datei(en) eingeplant – Status: space-daemon upload list", args.len());
            return Ok(ExitCode::SUCCESS);
        }
        Commands::Upload { action: UploadAction::List } => Request::UploadList,
        Commands::Upload { action: UploadAction::Cancel { id } } => Request::UploadCancel { id },
        Commands::Upload { action: UploadAction::Retry } => Request::UploadRetry,
        Commands::Upload { action: UploadAction::RuleAdd { folder, target } } => {
            let (device, share, dest_dir) = parse_upload_target(&target)?;
            Request::UploadRuleAdd { path: absolute(&folder)?, device, share, dest_dir }
        }
        Commands::Upload { action: UploadAction::RuleList } => Request::UploadRuleList,
        Commands::Upload { action: UploadAction::RuleRemove { id } } => Request::UploadRuleRemove { id },
        Commands::Links { action: None | Some(LinksAction::List) } => Request::LinkList,
        Commands::Links { action: Some(LinksAction::Create { file, days, max_downloads, password }) } => {
            let password = if password {
                let a = read_secret("Passwort für den Link: ")?;
                let b = read_secret("Passwort wiederholen: ")?;
                if a != b {
                    anyhow::bail!("Die Passwörter stimmen nicht überein.");
                }
                Some(a)
            } else {
                None
            };
            eprintln!("⏳ Datei wird verschlüsselt hochgeladen …");
            Request::LinkCreate { path: absolute(&file)?, days, max_downloads, password }
        }
        Commands::Links { action: Some(LinksAction::Revoke { id }) } => Request::LinkRevoke { id },
        Commands::Links { action: Some(LinksAction::Setup { url, endpoint, bucket, key_id }) } => {
            let secret = read_secret("Secret Access Key des R2-Tokens: ")?;
            Request::LinkSetup(space_core::links::LinkSettings {
                base_url: url,
                s3: space_core::links::s3::S3Config {
                    endpoint,
                    bucket,
                    region: "auto".into(),
                    access_key_id: key_id,
                    secret_access_key: secret,
                },
            })
        }
        Commands::Usage { path, limit } => Request::SpaceUsage { path: absolute(&path)?, limit },
        Commands::Dupes { path, min_mb, limit } => Request::Duplicates {
            path: path.map(|p| absolute(&p)).transpose()?,
            min_size: Some((min_mb.max(0.0) * 1e6) as u64),
            limit,
        },
        Commands::Photos { path, months: true, .. } => {
            Request::TimelineMonths { path: path.map(|p| absolute(&p)).transpose()? }
        }
        Commands::Photos { path, before, limit, .. } => {
            Request::Timeline { path: path.map(|p| absolute(&p)).transpose()?, before, limit }
        }
        Commands::Backup { action: None | Some(BackupAction::List) } => Request::BackupList,
        Commands::Backup { action: Some(BackupAction::Create { to, sources }) } => {
            eprintln!("Das Passwort schützt die Sicherung außerhalb dieses Geräts (mindestens 10 Zeichen).");
            let a = read_secret("Passwort für die Sicherung: ")?;
            let b = read_secret("Passwort wiederholen: ")?;
            if a != b {
                anyhow::bail!("Die Passwörter stimmen nicht überein.");
            }
            let sources = sources.iter().map(|p| absolute(p)).collect::<Result<Vec<_>>>()?;
            Request::BackupCreate { target: backup_target(&to)?, sources, password: a }
        }
        Commands::Backup { action: Some(BackupAction::Connect { from, id }) } => {
            let secret = read_secret("Passwort oder Wiederherstellungsschlüssel: ")?;
            Request::BackupConnect { target: backup_target(&from)?, id, secret }
        }
        Commands::Backup { action: Some(BackupAction::Sources { id, sources }) } => {
            Request::BackupSources { id, sources: sources.iter().map(|p| absolute(p)).collect::<Result<Vec<_>>>()? }
        }
        Commands::Backup { action: Some(BackupAction::Run { id }) } => Request::BackupRun { id },
        Commands::Backup { action: Some(BackupAction::Snapshots { id }) } => Request::BackupSnapshots { id },
        Commands::Backup { action: Some(BackupAction::Ls { id, snapshot, path }) } => {
            Request::BackupBrowse { id, snapshot, path: absolute(&path)? }
        }
        Commands::Backup { action: Some(BackupAction::Restore { id, snapshot, path, to }) } => {
            Request::BackupRestore { id, snapshot, path: absolute(&path)?, dest: absolute(&to)? }
        }
        Commands::Backup { action: Some(BackupAction::Check { id, all }) } => {
            Request::BackupCheck { id, sample: if all { 0 } else { 200 } }
        }
        Commands::Backup { action: Some(BackupAction::Remove { id }) } => Request::BackupRemove { id },
        Commands::Settings { action: None | Some(SettingsAction::List) } => Request::Settings,
        Commands::Settings { action: Some(SettingsAction::Set { key, value }) } => {
            Request::SettingSet { key, value: Some(value) }
        }
        Commands::Settings { action: Some(SettingsAction::Reset { key }) } => Request::SettingSet { key, value: None },
        Commands::Watch => return watch(&config.socket_path).await,
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

/// Abonniert Ereignisse und gibt sie fortlaufend aus.
async fn watch(socket: &Path) -> Result<ExitCode> {
    let mut stream = connect(socket).await?;
    write_frame(&mut stream, &Envelope::new(Request::Subscribe)).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Success { .. } => {}
        other => return Ok(print_response(other)),
    }
    println!("👀 Warte auf Ereignisse … (STRG+C beendet)");
    loop {
        tokio::select! {
            frame = read_frame::<_, Response>(&mut stream) => match frame {
                Ok(Response::Event(e)) => println!("{} {}", chrono::Local::now().format("%H:%M:%S"), describe_event(&e)),
                Ok(other) => { print_response(other); }
                Err(_) => {
                    eprintln!("Verbindung zum Daemon beendet.");
                    return Ok(ExitCode::FAILURE);
                }
            },
            _ = tokio::signal::ctrl_c() => return Ok(ExitCode::SUCCESS),
        }
    }
}

fn describe_event(e: &Event) -> String {
    match e {
        Event::LocalCatalogChanged => "📁 Eigene Freigaben geändert".into(),
        Event::RemoteCatalogChanged { new_entries } => {
            format!("🔄 Dateilisten anderer Geräte geändert ({new_entries} neu)")
        }
        Event::PeerStatusChanged { node_id, status } => {
            let icon = if status == "online" { "🟢" } else { "🔴" };
            format!("{icon} Peer {} ist {status}", short(node_id, 10))
        }
        Event::JobQueued { job_id } => format!("⏳ Auftrag {job_id} in der Warteschlange"),
        Event::JobCompleted { job_id } => format!("✅ Auftrag {job_id} abgeschlossen"),
        Event::JobFailed { job_id, error } => format!("❌ Auftrag {job_id} fehlgeschlagen: {error}"),
        Event::JobRemoved { job_id } => format!("🗑️  Auftrag {job_id} entfernt"),
        Event::DevicePaired { name, .. } => format!("🔗 Gerät '{name}' gekoppelt"),
        Event::DeviceRemoved { node_id } => format!("✂️  Gerät {} entkoppelt", short(node_id, 10)),
        Event::ShareScanned { name, changed, deleted, total } => {
            format!("📂 Freigabe '{name}': {changed} neu/geändert, {deleted} gelöscht, {total} insgesamt")
        }
        Event::PinsChanged { queued, removed, conflicts } => {
            format!("📌 Pins: {queued} zu laden, {removed} entfernt, {conflicts} Konflikte")
        }
        Event::GrantsChanged { node_id } => format!("🔐 Rechte von Gerät {} geändert", short(node_id, 10)),
        Event::FileOpProgress { job, done_files, total_files, done_bytes, total_bytes } => format!(
            "⏳ Auftrag {job}: {done_files}/{total_files} Dateien, {} von {}",
            human_size(*done_bytes),
            human_size(*total_bytes)
        ),
        Event::FileOpFinished { job, ok, message } => {
            format!("{} Auftrag {job}: {message}", if *ok { "✅" } else { "❌" })
        }
        Event::ApprovalRequested { id, node_id, summary } => {
            format!("🛑 Gerät {} möchte: {summary} – bestätigen mit: space-daemon ops approve {id}", short(node_id, 10))
        }
        Event::UploadChanged { id, state, message } => format!("⬆️  Upload {id}: {state} {message}"),
        Event::BackupFinished { id, ok, message } => {
            format!("{} Sicherung {}: {message}", if *ok { "✅" } else { "❌" }, short(id, 8))
        }
        Event::DuplicateScanProgress { done_bytes, total_bytes } => {
            format!("🔍 Duplikatsuche: {} von {}", human_size(*done_bytes), human_size(*total_bytes))
        }
        Event::SettingChanged { key, value } => format!("⚙️  Einstellung {key} = {value}"),
        Event::Lagged { missed } => format!("⚠️  {missed} Ereignisse verpasst"),
    }
}

/// Papierkorb-Eintrag: Nummer aus `trash list` (1 = neuester) oder volle Kennung.
async fn trash_id(socket: &Path, entry: &str) -> Result<String> {
    let Ok(n) = entry.parse::<usize>() else { return Ok(entry.to_string()) };
    match send_request(socket, &Request::TrashList).await? {
        Response::Trash { entries } => entries
            .get(n.wrapping_sub(1))
            .map(|e| e.id.clone())
            .with_context(|| format!("Kein Eintrag Nummer {n} im Papierkorb")),
        Response::Error { message, .. } => anyhow::bail!(message),
        _ => anyhow::bail!("Unerwartete Antwort des Daemons"),
    }
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

async fn connect(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket).await.map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => anyhow::anyhow!(
            "Der Daemon läuft nicht (kein Socket unter {}).\n   Starte ihn mit: space-daemon start",
            socket.display()
        ),
        std::io::ErrorKind::PermissionDenied => anyhow::anyhow!(
            "Keine Berechtigung für den Daemon-Socket {} – läuft der Daemon unter einem anderen Benutzer?",
            socket.display()
        ),
        _ => anyhow::anyhow!("Verbindung zum Daemon fehlgeschlagen: {e}"),
    })
}

async fn send_request(socket: &Path, req: &Request) -> Result<Response> {
    let mut stream = connect(socket).await?;
    write_frame(&mut stream, &Envelope::new(req.clone())).await?;
    read_frame(&mut stream).await.context("Keine gültige Antwort vom Daemon (wurde die Verbindung abgelehnt?)")
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
        Response::Audit { entries } => print_audit(&entries),
        Response::Shares { shares } => print_shares(&shares),
        Response::ShareAdded { share, granted } => {
            println!("✅ Ordner freigegeben als '{}': {}", share.name, share.root);
            if granted.is_empty() {
                println!("   Noch kein Gerät hat Zugriff.");
            } else {
                println!("   Lesezugriff für: {}", granted.join(", "));
            }
            if share.auto_grant {
                println!("   Später gekoppelte Geräte erhalten ebenfalls Zugriff.");
            }
            println!("   Der erste Scan läuft im Hintergrund – Fortschritt: space-daemon watch");
        }
        Response::Scanned { results } => print_scanned(&results),
        Response::Pins { pins, pin_dir, used_bytes, max_bytes } => print_pins(&pins, &pin_dir, used_bytes, max_bytes),
        Response::PinAdded { pin, queued } => {
            println!(
                "📌 Pin {} angelegt: {}{} → {}",
                pin.id,
                pin.share,
                if pin.prefix.is_empty() { String::new() } else { format!("/{}", pin.prefix) },
                pin.path.display()
            );
            println!("   {queued} Datei(en) werden geladen; neue und geänderte Dateien folgen automatisch.");
        }
        Response::BrowsePage { items, total, .. } => {
            for i in &items {
                let wo = i.origin_name.clone().or_else(|| i.origin.as_deref().map(|o| short(o, 10).to_string()));
                println!(
                    "{:<14} {:>10}  {}  ({})",
                    short(&i.hash, 12),
                    human_size(i.size),
                    i.name,
                    wo.unwrap_or_else(|| "dieses Gerät".into())
                );
            }
            println!("{} von {total}", items.len());
        }
        Response::Event(e) => println!("{}", describe_event(&e)),
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
        Response::SearchResults { hits } => {
            if hits.is_empty() {
                println!("Nichts gefunden.");
            }
            for h in &hits {
                let size = if h.kind == NodeKind::File { human_size(h.size) } else { String::new() };
                println!("{} {:>10}  {}", kind_icon(h.kind), size, h.path);
            }
        }
        Response::DirListing { path, entries, total } => {
            println!("📂 {}", path.display());
            for e in &entries {
                let size = if e.kind == NodeKind::File { human_size(e.size) } else { String::new() };
                println!("{} {:>10}  {}  {}", kind_icon(e.kind), size, fmt_time(e.mtime), e.name);
            }
            if (entries.len() as u64) < total {
                println!("… {} von {total} angezeigt", entries.len());
            }
        }
        Response::IndexRoots { roots, watched_dirs } => {
            if roots.is_empty() {
                println!("Keine Orte indexiert. Hinzufügen: space-daemon index add <ordner>");
            }
            for r in &roots {
                let state = if !r.enabled {
                    "entfernt".to_string()
                } else if let Some(t) = r.last_scan {
                    format!("{} Einträge, Stand {}", r.entries, fmt_time(t))
                } else {
                    "wird indexiert …".to_string()
                };
                println!("{:>4}  {}  ({state})", r.id, r.path);
            }
            println!("{watched_dirs} Ordner werden live beobachtet.");
        }
        Response::FileOpStarted { job } => println!("⏳ Auftrag {job} gestartet."),
        Response::StreamUrl(s) => println!("{}", s.url),
        Response::BackupCreated(c) => {
            println!("✅ Sicherung {} eingerichtet.", &c.id[..8]);
            println!();
            println!("   WIEDERHERSTELLUNGSSCHLÜSSEL (jetzt notieren oder ausdrucken – wird nie wieder angezeigt):");
            println!();
            println!("   {}", c.recovery_key);
            println!();
            println!(
                "   Ohne Passwort UND ohne diesen Schlüssel ist die Sicherung auf einem neuen Gerät nicht lesbar."
            );
            println!("   Erste Sicherung jetzt: space-daemon backup run {}", &c.id[..8]);
        }
        Response::Backups { jobs } => {
            if jobs.is_empty() {
                println!("Keine Sicherungen. Einrichten: space-daemon backup create --to /media/…/Platte ~/Dokumente");
            }
            for j in &jobs {
                let target = match &j.target {
                    space_core::backup::repo::BackupTarget::Dir { path } => path.display().to_string(),
                    space_core::backup::repo::BackupTarget::S3 => "Cloudflare R2".into(),
                };
                let state = if j.running {
                    "läuft …".to_string()
                } else if let Some(e) = &j.last_error {
                    format!("❌ {e}")
                } else if let Some(t) = j.last_ok {
                    format!("✅ zuletzt {}", fmt_time(t))
                } else {
                    "noch nie".into()
                };
                println!("{}  → {target}  ({state})", &j.id[..8]);
                for s in &j.sources {
                    println!("     {}", s.display());
                }
            }
        }
        Response::Snapshot(s) => println!(
            "✅ Stand {} gesichert: {} Dateien, {} insgesamt, {} neu gespeichert",
            &s.id[..8],
            s.files,
            human_size(s.bytes),
            human_size(s.new_bytes)
        ),
        Response::Snapshots { items } => {
            for s in items.iter().rev() {
                println!(
                    "{}  {}  {:>7} Dateien  {:>10}  (+{})",
                    &s.id[..8],
                    fmt_time(s.time),
                    s.files,
                    human_size(s.bytes),
                    human_size(s.new_bytes)
                );
            }
        }
        Response::BackupEntries { entries } => {
            for e in &entries {
                let icon = if e.is_dir { "📁" } else { "📄" };
                let size = if e.is_dir { String::new() } else { human_size(e.size) };
                println!("{icon} {size:>10}  {}", e.name);
            }
        }
        Response::Restored { path, files } => {
            println!("✅ {files} Datei(en) wiederhergestellt nach {}", path.display())
        }
        Response::SpaceUsage(u) => {
            println!("📊 {}  –  {} in {} Dateien", u.path, human_size(u.total), u.files);
            let bar = |size: u64| {
                let n = if u.total == 0 { 0 } else { (size as f64 / u.total as f64 * 30.0).round() as usize };
                format!("{:<30}", "█".repeat(n))
            };
            for c in &u.children {
                println!("{} {:>10}  {}{}", bar(c.size), human_size(c.size), c.name, if c.is_dir { "/" } else { "" });
            }
            println!("\nNach Typ:");
            for t in &u.by_type {
                println!("  {:<14} {:>10}  ({} Dateien)", t.category, human_size(t.size), t.files);
            }
            println!("\nGrößte Dateien:");
            for f in u.largest.iter().take(10) {
                println!("  {:>10}  {}", human_size(f.size), f.path);
            }
        }
        Response::Duplicates(r) => {
            if r.groups.is_empty() {
                println!("Keine Duplikate gefunden ({} Kandidaten geprüft).", r.candidates);
            }
            for g in &r.groups {
                println!("🔁 {} × {}  (frei werdend: {})", g.paths.len(), human_size(g.size), human_size(g.wasted));
                for p in &g.paths {
                    println!("     {p}");
                }
                if !g.other_devices.is_empty() {
                    println!("     auch auf: {}", g.other_devices.join(", "));
                }
            }
            if !r.groups.is_empty() {
                println!(
                    "\nInsgesamt {} durch Duplikate belegt. Entfernen: space-daemon rm <pfad> (Papierkorb)",
                    human_size(r.wasted)
                );
            }
        }
        Response::Timeline(page) => {
            if page.items.is_empty() {
                println!("Keine Fotos oder Videos gefunden (die Zeitachse füllt sich nach dem Indexieren).");
            }
            let mut day = String::new();
            for it in &page.items {
                let t = chrono::DateTime::from_timestamp(it.taken, 0).unwrap_or_default();
                let d = t.format("%d.%m.%Y").to_string();
                if d != day {
                    println!("\n📅 {d}");
                    day = d;
                }
                let dims = match (it.width, it.height) {
                    (Some(w), Some(h)) => format!(" {w}×{h}"),
                    _ => String::new(),
                };
                println!(
                    "  {} {}{}  {}{}{}",
                    t.format("%H:%M"),
                    if it.video { "🎬" } else { "🖼" },
                    if it.exif { "" } else { " (Dateizeit)" },
                    it.path,
                    dims,
                    it.camera.as_deref().map(|c| format!("  · {c}")).unwrap_or_default()
                );
            }
            if let Some(next) = &page.next {
                println!("\nWeiter: space-daemon photos --before {next}");
            }
        }
        Response::TimelineMonths { months } => {
            if months.is_empty() {
                println!("Noch keine Fotos oder Videos in der Zeitachse.");
            }
            for m in &months {
                println!("  {:04}-{:02}  {:>6}", m.year, m.month, m.count);
            }
        }
        Response::Link(l) => {
            if l.state == "revoked" {
                println!("🚫 Link {} widerrufen – die verschlüsselte Kopie wurde gelöscht.", short(&l.id, 8));
            } else {
                println!("🔗 {}", l.url);
                println!(
                    "   {} ({}) · gültig bis {}{}{}",
                    l.name,
                    human_size(l.size),
                    fmt_time(l.expires_at),
                    l.max_downloads.map(|m| format!(" · höchstens {m} Downloads")).unwrap_or_default(),
                    if l.password { " · mit Passwort" } else { "" }
                );
                println!(
                    "   Wer diesen Link hat, kann die Datei laden. Widerrufen: space-daemon links revoke {}",
                    short(&l.id, 8)
                );
            }
        }
        Response::Links { items, base_url } => {
            match &base_url {
                None => println!(
                    "Noch nicht eingerichtet: space-daemon links setup … (Anleitung: gateway/cloudflare/README.md)"
                ),
                Some(b) => println!("Worker: {b}"),
            }
            for l in &items {
                let icon = match l.state.as_str() {
                    "active" => "🔗",
                    "revoked" => "🚫",
                    _ => "⌛",
                };
                println!(
                    "{icon} {}  {:<30} bis {}  {}",
                    short(&l.id, 8),
                    l.name,
                    fmt_time(l.expires_at),
                    if l.state == "active" { l.url.as_str() } else { "" }
                );
            }
        }
        Response::Uploads { items } => {
            if items.is_empty() {
                println!("Keine Uploads.");
            }
            for u in &items {
                let icon = match u.state.as_str() {
                    "done" => "✅",
                    "failed" => "❌",
                    _ => "⏳",
                };
                let target = format!("{}/{}", u.share, u.dest_dir);
                println!("{icon} {:>4}  {} → {}", u.id, u.local_path, target.trim_end_matches('/'));
                if let Some(e) = u.last_error.as_ref().filter(|_| u.state != "done") {
                    println!("         {e} (Versuche: {})", u.attempts);
                }
            }
        }
        Response::UploadRules { rules } => {
            if rules.is_empty() {
                println!("Keine Regeln. Anlegen: space-daemon upload rule-add <ordner> <gerät>:<Freigabe>");
            }
            for r in &rules {
                println!("{:>4}  {} → {}:{}/{}", r.id, r.local_dir, short(&r.device, 10), r.share, r.dest_dir);
            }
        }
        Response::Settings { items } => {
            for i in &items {
                let mark = if i.overridden { format!("  (Standard {})", i.default) } else { String::new() };
                println!("{:<20} {:>8} {:<15}{mark}", i.key, i.value, i.unit);
                println!("{:<20} {}", "", i.help);
            }
            println!("Ändern: space-daemon settings set <name> <wert>  ·  zurücksetzen: settings reset <name>");
        }
        Response::Thumbnail(t) => {
            println!("{}", t.path.display());
            println!("{} × {} px{}", t.width, t.height, if t.cached { " (aus dem Cache)" } else { "" });
        }
        Response::Metadata(m) => {
            let line = |k: &str, v: Option<String>| {
                if let Some(v) = v {
                    println!("{k:<14} {v}");
                }
            };
            line("Art:", m.kind.map(|k| format!("{k:?}")));
            line("Typ:", m.mime.clone());
            line("Größe:", Some(human_size(m.size)));
            line("Abmessungen:", m.width.zip(m.height).map(|(w, h)| format!("{w} × {h} px")));
            line("Aufgenommen:", m.taken_at.clone());
            line("Kamera:", m.camera.clone());
            line("Ort:", m.gps.map(|g| format!("{:.5}, {:.5}", g.lat, g.lon)));
            line("Dauer:", m.duration_secs.map(|d| format!("{}:{:02}", d as u64 / 60, d as u64 % 60)));
            line("Titel:", m.title.clone());
            line("Interpret:", m.artist.clone());
            line("Album:", m.album.clone());
        }
        Response::Approvals { items } => {
            if items.is_empty() {
                println!("Keine angehaltenen Anfragen.");
            }
            for a in &items {
                let who = a.device_name.clone().unwrap_or_else(|| short(&a.node_id, 10).to_string());
                println!("{:>4}  {}  {who}: {}", a.id, fmt_time(a.created_at), a.summary);
            }
            if !items.is_empty() {
                println!("Bestätigen: space-daemon ops approve <nr> · Ablehnen: space-daemon ops reject <nr>");
            }
        }
        Response::FileOpDone { outcome, .. } => {
            for d in &outcome.done {
                if d.from == d.to {
                    println!("✅ {}", d.to.display());
                } else {
                    println!("✅ {} → {}", d.from.display(), d.to.display());
                }
            }
            if !outcome.trashed.is_empty() {
                println!(
                    "🗑️  {} Eintrag/Einträge im Papierkorb (rückgängig: space-daemon undo)",
                    outcome.trashed.len()
                );
            }
            for p in &outcome.deleted {
                println!("❌ endgültig gelöscht: {}", p.display());
            }
            if outcome.done.is_empty() && outcome.trashed.is_empty() && outcome.deleted.is_empty() {
                println!("Nichts zu tun.");
            }
        }
        Response::Trash { entries } => {
            if entries.is_empty() {
                println!("Der Papierkorb ist leer.");
            }
            for (i, e) in entries.iter().enumerate() {
                let size = if e.is_dir { "Ordner".to_string() } else { human_size(e.size) };
                println!("{:>4}  {:>10}  {}  {}", i + 1, size, e.deleted_at.replace('T', " "), e.original_path);
            }
        }
        Response::Error { message, .. } => {
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
            println!("   {:<14} {:>10}  {}", short(&f.blake3_hash, 12), human_size(f.file_size), f.path);
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
            let shown = match (&f.share, &f.rel_path) {
                (Some(s), Some(r)) => format!("{s}/{r}"),
                _ => f.name.clone(),
            };
            println!("   {:<14} {:>10}  {}", short(&f.hash, 12), human_size(f.size), shown);
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
    println!(
        "Herunterladen: space-daemon download <name|hash> · 🟢 erreichbar 🔴 zuletzt nicht erreichbar ⚪ unbekannt"
    );
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
        for g in &d.grants {
            match &g.folder {
                Some(f) => println!("    🔐 {:<14} {f}", g.capability),
                None => println!("    🔐 {}", g.capability),
            }
        }
    }
    println!("\nRechte ändern: space-daemon pair grant|revoke <gerät> <recht> [--folder <ordner>]");
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

fn print_shares(shares: &[ShareInfo]) {
    if shares.is_empty() {
        println!("Noch keine Ordner freigegeben: space-daemon share add <ordner>");
        return;
    }
    println!("{:<20} {:>8} {:>10}  ORDNER", "NAME", "DATEIEN", "GRÖSSE");
    for s in shares {
        let auto = if s.auto_grant { "" } else { "  (nur ausgewählte Geräte)" };
        println!("{:<20} {:>8} {:>10}  {}{auto}", s.name, s.file_count, human_size(s.total_size), s.root);
    }
}

fn print_pins(pins: &[PinInfo], pin_dir: &Path, used: u64, max: u64) {
    if pins.is_empty() {
        println!("Noch keine Pins. Beispiel: space-daemon pin add Fotos/2024");
        return;
    }
    for p in pins {
        let from = p.origin_name.clone().unwrap_or_else(|| short(&p.origin, 10).to_string());
        let what = if p.prefix.is_empty() { p.share.clone() } else { format!("{}/{}", p.share, p.prefix) };
        println!("[{}] {from}:{what} → {}", p.id, p.path.display());
        let mut line = format!("     {} Dateien, {}", p.files_ok, human_size(p.bytes));
        if p.files_pending > 0 {
            line.push_str(&format!(", {} ausstehend", p.files_pending));
        }
        if p.conflicts > 0 {
            line.push_str(&format!(
                ", ⚠️  {} Konflikt(e) – lokal geänderte Kopien werden nicht überschrieben",
                p.conflicts
            ));
        }
        println!("{line}");
    }
    let quota = if max == 0 { "unbegrenzt".to_string() } else { human_size(max) };
    println!("\nOrdner: {} · belegt {} von {quota}", pin_dir.display(), human_size(used));
}

fn print_scanned(results: &[ScanResult]) {
    for r in results {
        println!(
            "📂 {}: {} neu/geändert, {} gelöscht, {} unverändert{}",
            r.name,
            r.added_or_changed,
            r.deleted,
            r.unchanged,
            if r.skipped > 0 { format!(", {} übersprungen", r.skipped) } else { String::new() }
        );
    }
}

fn print_audit(entries: &[AuditEntry]) {
    if entries.is_empty() {
        println!(
            "Keine Einträge. (Protokolliert werden Zugriffe gekoppelter Geräte außer dem normalen Listenabgleich.)"
        );
        return;
    }
    for e in entries {
        let icon = if e.allowed { "✅" } else { "⛔" };
        let target = e.target.as_deref().map(|t| format!(" {t}")).unwrap_or_default();
        let detail = e.detail.as_deref().map(|d| format!(" – {d}")).unwrap_or_default();
        println!("{} {icon} {} {}{target}{detail}", fmt_time(e.ts), short(&e.node_id, 10), e.operation);
    }
}

fn short(s: &str, n: usize) -> &str {
    &s[..s.len().min(n)]
}

fn fmt_time(ts: i64) -> String {
    use chrono::{Local, TimeZone};
    Local.timestamp_opt(ts, 0).single().map(|t| t.format("%d.%m.%Y %H:%M").to_string()).unwrap_or_else(|| "?".into())
}

fn kind_icon(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Dir => "📁",
        NodeKind::Symlink => "🔗",
        NodeKind::File => "📄",
    }
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
