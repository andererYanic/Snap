# Snap – privates P2P-VDFS

Ein Daemon mit CLI zum Teilen von Dateien zwischen eigenen Geräten über
[Iroh](https://iroh.computer) (QUIC). Gekoppelte Geräte gleichen ihre Dateilisten automatisch
ab; Downloads funktionieren per Name, Hash oder Ticket und werden bei Offline-Phasen
automatisch nachgeholt.

Ziel ist eine umfassende, sichere Alternative zu Dateiexplorer und Google Drive in einer App
(Desktop und Mobile) – siehe [`docs/ROADMAP.md`](docs/ROADMAP.md).

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

### Eigene Geräte koppeln

```bash
# Gerät A
space-daemon pair invite          # zeigt einen Einmal-Code (10 min gültig) und wartet
# Gerät B
space-daemon pair join snap1-…    # koppelt; die Dateilisten werden sofort abgeglichen
space-daemon list                 # eigene Dateien + Dateien aller gekoppelten Geräte
space-daemon download urlaub.jpg  # per Dateiname, Hash oder Ticket
```

## Befehle

| Befehl | Zweck |
|---|---|
| `start` | Daemon starten (STRG+C / SIGTERM beendet sauber) |
| `publish <datei>` | Datei freigeben, Ticket anzeigen |
| `download <ticket\|hash\|name\|Freigabe/pfad> [-o pfad] [--force]` | Herunterladen; ohne `--force` wird nie überschrieben |
| `list` | Eigene Freigaben und Dateien gekoppelter Geräte (gleicht im Hintergrund ab, wenn der Stand > 5 min alt ist) |
| `sync` | Sofort mit allen gekoppelten Geräten abgleichen |
| `share add <ordner> [--name N] [--devices a,b]` | Ordner freigeben (ohne `--devices`: alle gekoppelten, auch künftige Geräte) |
| `share list` / `share remove <name>` / `share rescan [name]` | Freigaben verwalten |
| `pin add <Freigabe[/pfad]>` (eindeutig: `Gerät:Freigabe/pfad`) | Ordner eines anderen Geräts offline verfügbar halten (`~/Snap/<Gerät>/…`) |
| `pin list` / `pin remove <id>` | Pins mit Fortschritt, Konflikten, Platzbedarf |
| `search <begriff> [--ext jpg,png] [--dirs\|--files] [--hidden]` | Dateien und Ordner auf diesem Gerät finden (Teilwort, ohne Groß-/Kleinschreibung) |
| `ls [ordner] [-a]` | Ordnerinhalt live anzeigen (Ordner zuerst) |
| `index list` / `index add <ordner>` / `index remove <id>` / `index rescan` | Orte der lokalen Suche verwalten |
| `cp <quelle>… <ziel>` / `mv <quelle>… <ziel>` `[--on-conflict fail\|skip\|keep-both\|replace]` | Kopieren/Verschieben (nie stillschweigend überschreiben; „ersetzen“ legt das alte Ziel in den Papierkorb) |
| `rename <pfad> <name>` / `mkdir <pfad>` | Umbenennen, Ordner anlegen |
| `rm <pfad>… [--permanent]` | In den Papierkorb (freedesktop) – mit `--permanent` endgültig |
| `undo` | Letzte Dateioperation rückgängig machen (auch Änderungen anderer Geräte) |
| `trash` / `trash restore <nr>` / `trash purge <nr>` / `trash empty` | Papierkorb |
| `remote <gerät> rename\|mv\|cp\|mkdir\|rm Freigabe/pfad …` | Dateien in Freigaben eines gekoppelten Geräts bearbeiten |
| `ops` / `ops approve <nr>` / `ops reject <nr>` | Vom Massenschutz angehaltene Änderungen anderer Geräte |
| `thumb <bild> [--large]` | Vorschaubild erzeugen (256 bzw. 1024 px, Cache im Datenordner) |
| `meta <datei>` | Metadaten: Art, Abmessungen, Aufnahmedatum, Kamera, Ort (GPS), Dauer, Titel/Interpret |
| `pair invite` / `pair join <code>` | Gerät koppeln (Einmal-Code, 10 min gültig) |
| `pair list` / `pair remove <gerät>` | Gekoppelte Geräte mit Rechten anzeigen / entkoppeln |
| `pair grant\|revoke <gerät> <recht> [--folder <ordner>]` | Rechte vergeben/entziehen (`catalog.read`, `files.read`, `files.write`, `files.delete`, `pin.request`) |
| `audit` | Zugriffe gekoppelter Geräte (erlaubt/abgelehnt) |
| `watch` | Ereignisse des Daemons live anzeigen |
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
(`false` = keine n0-Relais/kein n0-DNS, nur direkte Verbindungen; Tickets enthalten dann IP-Adressen),
`device_name` (Anzeigename, Standard: Hostname), `sync_interval_hours` (periodischer Abgleich,
Standard 6, ±30 % Zufall; 0 = aus).

## Architektur

```
apps/daemon      CLI + Daemon-Start (clap)
core/src
  config.rs      Konfiguration, XDG-Pfade, Umgebungsvariablen
  secure_fs.rs   private Ordner (0700), persistenter Node-Schlüssel (0600)
  ipc.rs         Protokoll CLI ↔ Daemon (JSON mit Längenpräfix, max. 1 MiB)
  daemon.rs      Start/Stopp, IPC-Server (API-Version, Abos), Aufräumen
  network.rs     Iroh-Endpoint, Blob-Store, Zugriffskontrolle, Transfers
  api/           Operationen (files, transfer, devices), Fehlercodes, Rechte (auth.rs)
  events.rs      Ereignis-Stream für Live-Updates
  worker.rs      Offline-Warteschlange
  sync.rs        Ledger-Abgleich (auf Anfrage, Hintergrund, periodisch)
  shares.rs      Ordner-Freigaben: Scanner, Watcher, Import per Referenz
  live.rs        Live-Verbindungen (snap/live/1): Hinweise, Status, Wiederverbindung
  pins.rs        Smart-Pinning: Abgleich, sichere Zielpfade, Konflikte, Kontingent
  pairing.rs     Einmal-Codes, Pairing-Protokoll (snap/pair/1)
  ledger.rs      signierte Dateilisten, Abgleich per Versionsvektor (snap/ledger/1)
  database.rs    SQLite: files, pending_pins, peer_status, devices, ledger_entries, peer_addrs
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

### Ordner-Freigaben

- `share add ~/Fotos` scannt den Ordner und beobachtet ihn danach (inotify, 2 s Bündelung).
  Beim Daemon-Start werden Änderungen nachgeholt.
- Dateien werden **per Referenz** importiert (keine Kopie). Unveränderte Dateien werden nicht
  neu gehasht; reine Zeitänderungen erzeugen keinen Abgleichverkehr.
- **Symlinks werden nie verfolgt**; temporäre und Systemdateien werden ignoriert.
- Im Ledger stehen Freigabe-Name und Pfad **relativ** zur Freigabe – nie absolute Pfade.
- Andere Geräte laden per `download Fotos/2024/strand.jpg`.

### Smart-Pinning (offline verfügbar)

- `pin add Fotos/2024` spiegelt den Ordner nach `~/Snap/<Gerät>/Fotos/2024/…` und hält ihn
  aktuell: neue, geänderte und gelöschte Dateien folgen automatisch (über die Warteschlange –
  Offline-Phasen werden nachgeholt).
- **Lokal veränderte Kopien werden nie überschrieben oder gelöscht** (Konflikt in `pin list`).
- **Mehrere Quellen:** Ist das Ursprungsgerät aus, wird jedes andere Gerät versucht, das denselben
  Inhalt anbietet – z. B. ein Server, der seinen `~/Snap`-Ordner freigibt.
- Sicherheit: Zielpfade werden streng geprüft, es wird nie durch Symlinks geschrieben.
- `pin_dir` und Kontingent `pin_max_gb` in der Konfiguration.

### Lokale Suche & Explorer

- Home-Ordner und angeschlossene Laufwerke (`/media/<user>`, `/run/media/<user>`) werden
  automatisch indexiert. Caches, `node_modules`, `.git`, Papierkorb und Ordner mit
  `CACHEDIR.TAG` werden ausgelassen, ebenso die Snap-Daten.
- Der Index liegt **auf der Platte** (`index.db`, SQLite + FTS5-Trigramm), nicht im Arbeitsspeicher.
  Bei 1 Mio. Dateien braucht der Prozess rund 33 MiB RAM, eine Suche dauert typisch 1–3 ms.
- Änderungen sind nach etwa 1 s auffindbar, weil jeder indexierte Ordner beobachtet wird
  (höchstens `index_max_watches`). Zusätzlich läuft alle 30 min ein günstiger Abgleich, der nur
  Ordner mit geänderter mtime neu liest.
- Der Index bleibt **auf dem Gerät**. Gekoppelte Geräte können weder suchen noch Ordner lesen.
- Ein entferntes Laufwerk wird übersprungen; sein Index bleibt erhalten. Ein vom Nutzer entfernter
  Ort kommt auch bei automatischer Erkennung nicht zurück.

### Dateioperationen & Papierkorb

- Kopieren, verschieben, umbenennen, Ordner anlegen, Papierkorb – als Aufträge mit Fortschritt
  (Ereignis-Stream) und Abbruch; die Suche ist danach sofort aktuell.
- **Nie stillschweigend überschreiben** (`renameat2` mit `RENAME_NOREPLACE`); Kopien entstehen
  unter temporärem Namen, ein Abbruch hinterlässt nichts Halbes; Symlinks werden als Symlinks
  kopiert; Verschieben auf ein anderes Laufwerk = kopieren, prüfen, dann löschen.
- **Papierkorb nach freedesktop-Standard**: dieselben Einträge wie im Dateimanager des Desktops;
  auf anderen Laufwerken `.Trash-<uid>` auf dem Laufwerk selbst.
- **Rückgängig** für die letzten 20 Schritte (im Speicher; endgültiges Löschen ausgenommen).

### Vorschaubilder & Metadaten

- Vorschaubilder für JPEG, PNG, GIF, WebP, BMP und TIFF, in zwei Stufen (256 und 1024 px), mit
  EXIF-Ausrichtung. Der Cache liegt im Datenordner (`thumb_cache_mb`, Standard 500) und wird bei
  geänderten Dateien automatisch erneuert.
- JPEG wird direkt verkleinert decodiert. Ein 12-MP-Foto braucht etwa 70 ms, aus dem Cache 0,02 ms.
- Metadaten: Art (nach Inhalt erkannt, nicht nach Endung), Abmessungen, Aufnahmezeit, Kamera,
  GPS, Ausrichtung; bei Musik und Videos Dauer, Titel, Interpret, Album.
- Sicherheit: nur reine Rust-Decoder, Größe und Speicher werden vor dem Decodieren geprüft
  (Schutz vor Dekompressionsbomben), höchstens 2 Decodierungen gleichzeitig, Symlinks werden
  nicht verfolgt. Die Vorschaubilder für gekoppelte Geräte folgen mit dem Streaming (5a).

### Dateien auf anderen Geräten bearbeiten

- `remote PC mv Fotos/2024/a.jpg Fotos/Urlaub` – nur innerhalb von Freigaben und nur mit den
  passenden, ordnergebundenen Rechten: `files.write` (umbenennen, verschieben, anlegen),
  `files.read` + `files.write` (kopieren), `files.delete` (Papierkorb), `files.purge` (endgültig).
- **Gelöschtes landet im Papierkorb des Zielgeräts** und lässt sich dort mit `undo` zurückholen.
- **Massenschutz:** Verändert ein Gerät mehr als `ops_hourly_limit` Dateien pro Stunde (Standard
  100), wird die Anfrage angehalten, bis sie am Zielgerät bestätigt wird (`ops approve`).

### Live-Verbindungen

- Gekoppelte Geräte halten eine stehende Verbindung (`snap/live/1`). Änderungen erscheinen nach
  ~0,3 s auf den anderen Geräten, der Online-Status sofort; kommt ein Gerät zurück, starten
  wartende Downloads sofort.
- Über die Leitung gehen **nur Hinweise** („mein Ledger-Stand ist N“) – die Daten holt der
  Empfänger per signiertem Abgleich. Nur Geräte mit `catalog.read` (in beide Richtungen geprüft).
- Schutz: Frames ≤ 4 KiB, ≤ 50 Nachrichten/s, unbekannte Nachrichten trennen, Lebenszeichen alle
  20 s, Wiederverbindung mit Backoff (2 s – 5 min, Zufallsstreuung).
- Abschaltbar mit `live_connections = false` (dann nur Abgleich bei Nutzung/periodisch).
  Hinweis: Relais/Netzbeobachter sehen, wann deine Geräte online sind.

### Pairing & Ledger-Gossip

- **Pairing:** `pair invite` erzeugt einen Code aus Node-ID und zufälligem 256-Bit-Token.
  Das neue Gerät verbindet sich damit; TLS beweist ihm die Identität des Gastgebers, das Token
  beweist dem Gastgeber die Berechtigung. Der Code gilt einmal, 10 Minuten, nur im Speicher.
  Falsche Tokens verbrauchen ihn nicht.
- **Ledger:** Jedes Gerät führt seine eigenen Einträge (Name, Größe, Hash, Löschmarkierung –
  keine Pfade) und signiert sie mit seinem Node-Schlüssel. Abgleich per Versionsvektor,
  seitenweise, in beide Richtungen über eine Verbindung. Einträge werden weitergereicht
  (z. B. über einen Server), bleiben aber dank Signatur fälschungssicher.
- **Abgleich:** bei `sync`, bei `list` (wenn älter als 5 min), nach dem Koppeln, bei eingehender
  Verbindung eines gekoppelten Geräts und periodisch.

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
- **Tickets sind Zugangsschlüssel – nur für Einzeldateien:** Wer ein Ticket einer mit `publish`
  freigegebenen Datei hat, kann sie laden. **Dateien aus Ordner-Freigaben** gehen dagegen nur an
  Geräte, die `files.read` auf den Ordner haben; die Node-ID der Gegenseite stammt aus TLS.
  Wer nur den Hash kennt, bekommt nichts.
- **Rechte pro Gerät:** Jede Operation läuft durch eine einzige Prüfstelle. Lokale Aufrufer
  (gleiche Benutzer-ID) dürfen alles; ein gekoppeltes Gerät nur, was ihm gewährt wurde –
  beim Koppeln ausschließlich `catalog.read`. Ordnerrechte gelten nur innerhalb des Ordners
  (Pfade kanonisiert, `..` verboten, Symlinks aufgelöst); Snap-Daten und Systempfade sind immer
  gesperrt. `admin` ist für Geräte gesperrt. Zugriffe fremder Geräte landen im Audit-Log
  (`audit`). Ein manipulierter Ledger-Eintrag verwirft die gesamte Seite.
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

## API für Oberflächen

Alle Oberflächen sprechen denselben Vertrag (`core/src/ipc.rs`): über den Unix-Socket
(Desktop, CLI) oder eingebettet (`Daemon::request`, `Daemon::subscribe` – Mobile per FFI).

- Jede Anfrage trägt die API-Version (`Envelope { v, req }`); Abweichung → `VersionMismatch`.
- Fehler haben maschinenlesbare Codes (`NotFound`, `Ambiguous`, `Unavailable`, `PermissionDenied` …).
- `Browse` liefert den Katalog seitenweise mit Cursor (`after`/`next`) – konstant < 1 ms pro Seite.
- `Subscribe` streamt Ereignisse (Katalog, Peer-Status, Aufträge, Geräte, Rechte).

## Tests & Sicherheitsprüfungen

```bash
cargo test
```

```bash
cargo deny check && cargo audit
```

```bash
cd fuzz && cargo +nightly fuzz run ledger_entry -- -max_total_time=60
```

Die CI prüft jeden Push/PR (Format, Clippy, Tests, Abhängigkeiten, Fuzzing-Kurzlauf) und fuzzt
nächtlich 20 Minuten pro Ziel. Sicherheitslücken bitte vertraulich melden – siehe
[`SECURITY.md`](SECURITY.md) und das [Bedrohungsmodell](docs/SECURITY_MODEL.md).

Die Integrationstests (`core/tests/`) starten mehrere echte Daemons auf 127.0.0.1, ohne Internet.
Sie prüfen:
- Zugriffskontrolle und den Ablauf „Peer offline → Warteschlange → Peer zurück → Download“,
- Pairing, Abgleich in beide Richtungen, Weiterreichen über einen Knotenpunkt,
- Angriffsfälle: fremde und entkoppelte Geräte, ersetzte, manipulierte und mehrfach genutzte Codes,
  Share-Dateien ohne Recht oder nur mit bekanntem Hash, Symlinks nach außen, Pfadausbrüche.

## Roadmap

Siehe [`docs/ROADMAP.md`](docs/ROADMAP.md).

Benötigt Rust ≥ 1.91 (Vorgabe von Iroh 1.4).
