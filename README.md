# Snap – privates P2P-VDFS

Ein Daemon mit CLI zum Teilen von Dateien zwischen eigenen Geräten über
[Iroh](https://iroh.computer) (QUIC). Der Daemon verträgt Offline-Phasen, sendet keine Pings
und ermittelt den Peer-Status rein reaktiv.

## Schnellstart

```bash
cargo build --release
./target/release/space-daemon start            # Daemon im Vordergrund
./target/release/space-daemon publish foto.jpg # gibt Hash + Ticket aus
```

Auf einem anderen Gerät:

```bash
space-daemon download <TICKET>                 # → ~/Downloads/snap/
space-daemon download <TICKET> -o ~/foto.jpg   # eigener Zielpfad
```

Ist der Peer offline, landet der Auftrag automatisch in der Warteschlange und wird
nachgeholt, sobald der Peer wieder erreichbar ist.

## Befehle

| Befehl | Zweck |
|---|---|
| `start` | Daemon starten (STRG+C / SIGTERM beendet sauber) |
| `publish <datei>` | Datei freigeben, Ticket anzeigen |
| `download <ticket\|hash> [-o pfad] [--force]` | Herunterladen; ohne `--force` wird nie überschrieben |
| `list` | Freigegebene Dateien |
| `ticket <hash>` | Vollständiges Ticket einer Freigabe |
| `delete --hash <h>` / `delete --path <p>` | Freigabe entfernen (sofort nicht mehr abrufbar) |
| `queue` / `queue retry <id>` / `queue cancel <id>` | Offline-Warteschlange |
| `peers` | Bekannte Peers mit zuletzt bekanntem Status |
| `info` | Node-ID, Pfade, Statistik |

Hashes können abgekürzt werden (mindestens 6 Hexzeichen, müssen eindeutig sein).

## Konfiguration & Pfade

| Was | Standard | Überschreiben |
|---|---|---|
| Konfiguration | `~/.config/snap/config.toml` (wird beim ersten Start angelegt) | `--config`, `SPACE_CONFIG` |
| Daten (DB, Blobs, `secret.key`) | `~/.local/share/snap/` | `data_dir`, `SPACE_DATA_DIR` |
| IPC-Socket | `$XDG_RUNTIME_DIR/snap/snap.sock` | `socket_path`, `SPACE_SOCKET` |
| Downloads ohne Zielpfad | `~/Downloads/snap/` | `download_dir`, `SPACE_DOWNLOAD_DIR` |

Weitere Optionen (siehe kommentierte `config.toml`): `retention_days` (Papierkorb, Standard 30),
`connect_timeout_secs` (10), `bind_port` (fester UDP-Port, z. B. für Docker), `use_relay`
(`false` = keine n0-Relais/kein n0-DNS, nur direkte Verbindungen; Tickets enthalten dann IP-Adressen).

## Architektur

```
apps/daemon      CLI + Daemon-Start (clap)
core/src
  config.rs      Konfiguration, XDG-Pfade, Umgebungsvariablen
  secure_fs.rs   private Ordner (0700), persistenter Node-Schlüssel (0600)
  ipc.rs         Protokoll CLI ↔ Daemon (JSON mit Längenpräfix, max. 1 MiB)
  daemon.rs      IPC-Server, Request-Handling, Warteschlangen-Worker, Aufräumen
  network.rs     Iroh-Endpoint, Blob-Store, Zugriffskontrolle, Transfers
  database.rs    SQLite: files, pending_pins, peer_status
  backoff.rs     30 s → 2 min → 10 min → 1 h, ±20 % Jitter
```

### Offline-Warteschlange (Hybrid-Worker)

- **Warteschlange leer:** Der Worker schläft unbegrenzt (`Notify`).
- **Aufträge vorhanden:** Er schläft bis zum nächsten fälligen Versuch oder bis er geweckt wird.
- **Er wird geweckt durch:**
  - einen neuen Auftrag,
  - `queue retry`,
  - einen bekannten Peer, der sich **von sich aus** bei uns verbindet. Diese Gelegenheit wird genutzt, ohne dass zusätzlicher Verkehr entsteht.
- **Backoff gilt pro Peer:** Ist ein Peer nicht erreichbar, werden seine übrigen Aufträge in dieser Runde übersprungen.
- **Fehlerarten:**
  - Verbindungsfehler und Abbrüche → Warteschlange
  - Ablehnung durch den Peer (Datei nicht oder nicht mehr freigegeben) und lokale Fehler → sofortige Fehlermeldung bzw. Job-Status „fehlgeschlagen“

### Peer-Status

`online` bzw. `offline` mit `last_seen`. Der Status ändert sich nur durch echte Verbindungen:
- ein erfolgreicher Download,
- ein fehlgeschlagener Verbindungsaufbau,
- eine eingehende Verbindung eines bereits bekannten Peers.

Unbekannte Node-IDs werden nicht gespeichert.

## Sicherheitsmodell

- **Zugriffskontrolle auf Blob-Ebene:** Ausgeliefert werden nur Hashes, die mit `publish`
  freigegeben und nicht gelöscht sind. Heruntergeladene oder gelöschte Dateien sind nicht
  abrufbar. Die Frage „besitzt du Hash X?“ (Observe) wird für alles andere abgelehnt.
  `get_many` und `push` sind deaktiviert.
- **Tickets sind Zugangsschlüssel:** Wer ein Ticket einer freigegebenen Datei hat, kann sie laden.
- **IPC:**
  - Der Socket liegt in einem Ordner mit den Rechten 0700, der Socket selbst hat 0600.
  - Zusätzlich muss jede Verbindung von derselben Benutzer-ID kommen (`SO_PEERCRED`).
  - Nachrichten sind auf 1 MiB begrenzt, mit 10 s Timeout.
- **Identität:** Der Node-Schlüssel liegt in `secret.key` (0600). Die Node-ID und damit alle
  Tickets bleiben über Neustarts gleich.
- **Dateisystem:**
  - Downloads werden erst in eine temporäre Datei geschrieben und dann ohne Überschreiben
    verlinkt. `--force` ersetzt atomar.
  - Dateinamen werden bereinigt (kein `../`).
- **Löschen:** Eine gelöschte Freigabe wird sofort nicht mehr ausgeliefert. Nach `retention_days`
  wird ihr Tag entfernt, und die Garbage Collection des Blob-Stores gibt den Speicher frei.
- **Bekannte Metadaten-Lücke:** Mit `use_relay = true` veröffentlicht der Node seine Node-ID und sein
  Heim-Relais bei n0-DNS (keine IP-Adressen). Verbindungen können über n0-Relais laufen; die
  Inhalte bleiben dabei durch QUIC/TLS Ende-zu-Ende verschlüsselt. Geplant ist ein eigenes Relais.

## Tests

```bash
cargo test
```

Die Integrationstests (`core/tests/two_nodes.rs`) starten mehrere echte Daemons auf 127.0.0.1,
ohne Internet. Sie prüfen:
- die Zugriffskontrolle,
- den Ablauf „Peer offline → Warteschlange → Peer zurück → Download“,
- das Aufwecken des Workers durch eingehende Verbindungen.

## Roadmap

1. **Ledger-Gossip:** automatischer Austausch der Dateilisten zwischen gekoppelten Geräten
   (Pairing, eigenes ALPN, Pull statt Push).
2. Tauri-GUI, Android.
3. Eigenes Iroh-Relais (Docker), Erweiterung von `peer_status` um `connection_type`.

Benötigt Rust ≥ 1.91 (Vorgabe von Iroh 1.4).
