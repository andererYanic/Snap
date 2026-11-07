# Bedrohungsmodell

Stand: 2026-11-05 · Gilt für Daemon, CLI und Protokolle in diesem Repository.

## Was Snap schützt

| Schutzgut | Ziel |
|---|---|
| Dateiinhalte | Nur Geräte/Personen mit ausdrücklicher Berechtigung erhalten Inhalte. |
| Dateilisten (Ledger) | Nur gekoppelte Geräte mit `catalog.read`; Einträge sind fälschungssicher signiert. |
| Node-Schlüssel (`secret.key`) | Verlässt das Gerät nie; Rechte 0600. |
| Lokales System | Kein Zugriff über Snap auf Dateien außerhalb freigegebener Ordner; kein Fernsteuern. |
| Verfügbarkeit | Fehlerhafte oder bösartige Gegenstellen dürfen Abgleich und Daemon nicht dauerhaft blockieren. |

## Akteure und Vertrauen

| Akteur | Vertrauen | Darf |
|---|---|---|
| Lokaler Benutzer (gleiche uid) | voll | alles über den IPC-Socket |
| Andere lokale Benutzer | keines | nichts – Socket-Ordner 0700, Socket 0600, `SO_PEERCRED` |
| Gekoppeltes Gerät | begrenzt | nur gewährte Rechte; Standard `catalog.read` |
| Fremder mit Ticket | begrenzt | genau die eine per `publish` freigegebene Datei |
| Fremder ohne Ticket | keines | nichts (auch nicht prüfen, ob ein Hash vorhanden ist) |
| Relais / Netzbeobachter | keines | sieht verschlüsselten Verkehr und Metadaten (wer mit wem, wann) |

## Mechanismen

- **Transport:** QUIC/TLS (Iroh), Identität = Ed25519-Schlüssel der Node; die Gegenseite wird
  immer aus TLS bestimmt, nie aus Nachrichteninhalten.
- **Pairing:** Einmal-Code mit 256-Bit-Token (OS-Zufall), 10 min, einmalig, nur im Speicher,
  Vergleich in konstanter Zeit. TLS beweist dem Beitretenden die Identität des Gastgebers.
- **Rechte:** eine Prüfstelle (`api/auth.rs`) für alle Operationen und Blob-Anfragen;
  ordnergebundene Rechte mit kanonisierten Pfaden, Symlink-Auflösung, Verbot von `..`/`.`;
  Snap-Daten, Socket und Systempfade sind immer gesperrt; `admin` für Geräte gesperrt.
- **Ledger:** jeder Eintrag vom Ursprungsgerät signiert; strenge Prüfung relativer Pfade;
  ungültige Einträge werden einzeln verworfen.
- **Freigaben:** Symlinks werden nie verfolgt; keine absoluten Pfade im Ledger.
- **Smart-Pinning:** Zielpfade aus fremden Einträgen werden streng geprüft (relativer Pfad,
  Freigabe- und Ordnername), es wird nie durch Symlinks geschrieben, Ziele müssen normale Dateien
  sein; lokal veränderte Kopien werden nie überschrieben oder gelöscht; Kontingent begrenzt den Platz.
  Ersatzquellen liefern nur, wenn sie selbst das Recht dazu vergeben haben.
- **Live-Verbindungen:** nur Hinweise (keine Daten), nur mit `catalog.read` in beide Richtungen,
  Frames ≤ 4 KiB, ≤ 50 Nachrichten/s, strikte Nachrichtentypen; Entzug trennt sofort.
- **Lokale Suche & Explorer:** Index und Dateisystem-Zugriff sind nur lokal nutzbar (Recht `admin`,
  für Geräte gesperrt). Der Index liegt im privaten Datenordner (0700) und wird nicht repliziert.
  Die Snap-Daten werden weder indexiert noch als Suchort angenommen, auch nicht über Symlinks.
  Symlinks werden erfasst, aber nicht verfolgt. Suchbegriffe gehen nur als Parameter in die
  Datenbank; FTS5-Syntax wird neutralisiert (Fuzz-Ziel `search_query`). Zu große Antworten werden
  als Fehler gemeldet, statt die Verbindung abzubrechen.
- **Dateioperationen:** nie stillschweigend überschreiben (`RENAME_NOREPLACE`, „ersetzen“ nur
  über den Papierkorb); Symlinks werden weder beim Kopieren noch beim Löschen verfolgt;
  `.trashinfo` gilt als fremde Eingabe (strenger Parser, nur absolute Ziele ohne `..`, Fuzz-Ziel
  `trashinfo`).
- **Vorschaubilder & Metadaten:** Dateien fremder Herkunft werden nur mit reinen Rust-Decodern
  gelesen. Abmessungen und Speicherbedarf werden *vor* dem Decodieren selbst geprüft, weil nicht
  jeder Decoder seine Grenzen einhält: Per Fuzzing wurde eine 43-Byte-GIF-Datei gefunden, die
  ~780 MB reservierte. Höchstens 2 Decodierungen laufen gleichzeitig, Panics werden abgefangen,
  `O_NOFOLLOW`. `lofty` 0.25.5 hängt bei AAC mit Frame-Länge 0 in einer Endlosschleife (ebenfalls
  per Fuzzing gefunden); für AAC wird die Dauer deshalb nicht berechnet. Fuzz-Ziel `media`.
  Restrisiko: Endlosschleifen in weiteren Parsern lassen sich ohne Prozess-Isolation nicht
  abbrechen. Für die Mobile-Einbettung wird geprüft, die Decoder in einem eigenen Prozess bzw.
  einer eigenen Erweiterung auszuführen.
- **Streaming (lokaler HTTP-Server):** nur `127.0.0.1` (zusätzlich Loopback-Prüfung je
  Verbindung), Adressen mit 256-Bit-Token aus dem Betriebssystem-Zufall (12 h Gültigkeit ohne
  Nutzung, höchstens 1 000), Host-Kopf muss `127.0.0.1:<port>`/`localhost:<port>` sein (Schutz
  vor DNS-Rebinding aus Webseiten), nur GET/HEAD, hyper als HTTP-Parser, Kopf-Zeitlimit,
  höchstens 64 Verbindungen; `nosniff`, `no-store`, `no-referrer`. Daten sind BLAKE3/bao-
  verifiziert; das liefernde Gerät prüft seine Rechte bei jeder Anfrage. Fuzz-Ziel `stream_range`.
  Restrisiko: Andere lokale Prozesse, die eine Adresse kennen (z. B. aus der Prozessliste eines
  Players), können die Datei ebenfalls abrufen.
- **Vorschaubilder fremder Freigaben (`snap/thumb/1`):** nur gekoppelt, nur mit `files.read`,
  nur innerhalb von Freigaben ohne Symlinks auf dem Weg; Anfrage ≤ 4 KiB, Antwort ≤ 1 MiB;
  empfangene Bilder werden mit den begrenzten Decodern neu codiert, bevor sie im Cache landen.
- **Hochladen (`Import` über `snap/ops/1`):** Das Ziel holt den Inhalt selbst per iroh-blobs. Ein
  Angebot gilt nur für genau dieses Gerät und diesen Hash (2 h) und wird danach zurückgezogen. Die
  Größe wird per BLAKE3/bao nachgewiesen und muss der Angabe entsprechen. Nur mit `files.write`;
  versteckte Namen sind verboten; hochgeladene Dateien sind nie ausführbar (0644) und
  überschreiben nie etwas („ersetzen“ nur mit `files.delete`, über den Papierkorb). Grenzen:
  `upload_daily_gb` je Gerät und Tag, `upload_reserve_gb` freier Platz. Hashes aus fremden
  Nachrichten werden streng geprüft (genau 64 Hex-Zeichen), denn `Hash::from_str` aus iroh-blobs
  stürzt bei Mehrbyte-Unicode ab (per Fuzzing gefunden).
- **Freigabelinks (Cloudflare Worker + R2):** Ende-zu-Ende-verschlüsselt (AES-256-GCM in
  64-KiB-Stücken, HKDF-SHA256, optional PBKDF2-SHA256 mit 600 000 Runden für ein Passwort; nur
  `ring` bzw. WebCrypto). Der Schlüssel steht nur im URL-Fragment, Speicher und Worker sehen nur
  Chiffretext. Stücknummer und Endmarke im Nonce erkennen Vertauschen und Abschneiden. Ein
  Kreuztest prüft die Browser-Implementierung gegen die Rust-Implementierung. Der Worker liefert
  nur GET/HEAD, strenge IDs, CSP `default-src 'none'`, `frame-ancestors 'none'`, keine fremden
  Skripte; Ablauf wird serverseitig durchgesetzt, abgelaufene Objekte werden gelöscht.
  Download-Grenzen über KV gelten nur näherungsweise. S3-Zugang mit SigV4 (geprüft gegen die
  AWS-Prüfwerte), nur HTTPS mit den Mozilla-Wurzelzertifikaten, Token nur für einen Bucket,
  gespeichert mit 0600; Geheimnisse werden verdeckt eingegeben, nie als Argument.
  **Restrisiken:** Wer den Link hat, hat die Datei (bis Ablauf/Widerruf). Cloudflare sieht
  Metadaten (Größe, Zeiten, IP-Adressen der Abrufer). Wer das Cloudflare-Konto übernimmt, könnte
  veränderten Seiten-Code ausliefern und Schlüssel abgreifen; dagegen hilft
  Zwei-Faktor-Anmeldung. Die Link-Geheimnisse liegen lokal in der Datenbank (privater
  Datenordner), damit Links später erneut angezeigt werden können.
- **Sicherungen:** Ende-zu-Ende-verschlüsselt (AES-256-GCM je Objekt, Objektname als AAD; IDs per
  HMAC-SHA256, damit das Ziel bekannte Dateien nicht erkennen kann); Hauptschlüssel verpackt mit
  Passwort (PBKDF2-SHA256, 600 000 Runden) und Wiederherstellungsschlüssel. Geladene Objekte werden
  gegen ihre ID geprüft; Manipulationen führen zu Fehlern statt falscher Daten (getestet).
  Wiederherstellen überschreibt nie, folgt keinen Symlinks (`create_new`), übernimmt keine
  setuid/setgid-Bits und verwirft ungültige oder doppelte Namen. Schlüsseldateien vom Ziel werden
  streng geprüft (Fuzz-Ziel `backup_keyfile`), die Inhaltsaufteilung hat ein eigenes Fuzz-Ziel.
  **Restrisiken:** Der Hauptschlüssel liegt auf dem sichernden Gerät (0600), damit Sicherungen
  ohne Eingabe laufen. Wer das Gerät übernimmt, kann die Sicherungen lesen. Wer Schreibzugriff
  auf das Ziel hat, kann Objekte löschen; Inhalte unbemerkt verändern kann er nicht.
- **Einstellungen:** nur lokal änderbar (für Geräte `admin`, also gesperrt), Wertebereiche
  geprüft; manipulierte gespeicherte Werte werden beim Laden verworfen.
- **Fernoperationen (`snap/ops/1`):** nicht gekoppelte Gegenstellen werden vor dem Lesen
  getrennt; nur relative Pfade innerhalb einer Freigabe, Symlinks auf dem Weg werden abgelehnt,
  Ziele zusätzlich kanonisiert geprüft; jede Operation läuft durch die zentrale Rechteprüfung
  mit ordnergebundenen Rechten und landet im Audit-Log. Löschen nur in den Papierkorb,
  endgültig nur mit `files.purge`. **Massenschutz** gegen kompromittierte Geräte: mehr als
  `ops_hourly_limit` Dateien pro Stunde → Bestätigung am Zielgerät (höchstens 100 offene Anfragen
  je Gerät). Frames ≤ 64 KiB, strikte Typen, Zeitlimits, ≤ 4 gleichzeitige Operationen
  (Fuzz-Ziel `ops_request`). Restrisiko: Ein Prozess *desselben Benutzers* auf dem Zielgerät
  könnte zwischen Prüfung und Ausführung einen Symlink austauschen (TOCTOU) – lokale Prozesse
  desselben Benutzers liegen außerhalb des Bedrohungsmodells.
- **Archive entpacken** (`fileops/archive.rs`, ZIP inkl. AES/ZipCrypto, tar, tar.gz): Archive
  gelten als feindlich. Entpackt wird in einen versteckten Arbeitsordner, der erst am Ende unter
  einem freien Namen sichtbar wird (Fehler/Abbruch hinterlassen nichts). Ein einziger Pfad mit
  `..`, absolutem Anfang, Laufwerksangabe, NUL-Byte, zu langem Namen oder mehr als 64 Ebenen lehnt
  das ganze Archiv ab. Verknüpfungen, Hardlinks und Gerätedateien werden nie angelegt, Rechte aus
  dem Archiv (setuid …) nie übernommen, Dateien nur mit `create_new` geschrieben. Gegen Zip-Bomben:
  ≤ 100 000 Einträge, entpackte Bytes ≤ freier Platz minus 512 MiB Reserve und ab 64 MiB höchstens
  1000-fache Archivgröße – gemessen beim Schreiben, nicht aus den Kopfdaten. Passwörter erscheinen
  nie in Protokollen. Nur reine Rust-Decoder (kein C-Code für fremde Archive). Fuzz-Ziel
  `archive_extract` prüft die Invarianten. Vorerst nur lokal; Entpacken auf anderen Geräten folgt
  mit eigenen Angriffstests. **Restrisiko:** ZipCrypto (altes ZIP-Verfahren) ist kryptografisch
  schwach – betrifft nur die Vertraulichkeit solcher Archive, nicht Snap.
- **Word-Leseansicht** (`docx.rs`): DOCX gilt als feindlich. Jeder ZIP-Teil höchstens 64 MiB
  (beim Lesen gemessen), höchstens 20 000 Blöcke und 16 MiB Text; kaputtes XML zeigt nur den
  lesbaren Anfang. XML ohne DTD und ohne eigene Entitäten (nur `&amp;` usw. und Zeichencodes) –
  kein XXE, kein „Billion Laughs“ (getestet). Bilder nur aus `word/`, nur Bildendungen, je
  ≤ 20 MiB, höchstens 100, unter eigenen Namen im App-Cache; Pfade mit `..`/absolut/URL werden
  verworfen. Links nur `http(s)`/`mailto`. Nur lokal (keine Anfrage für Geräte). Fuzz-Ziel
  `docx_read` prüft die Invarianten.
- **Eigene Schriftart (Android):** Nur auf Wunsch des Nutzers (Auswahldialog des Systems oder
  Dateimenü). Geprüft werden Größe (≤ 15 MB), Dateikopf (TrueType/OpenType) und ob Android die Datei
  lädt; erst dann ersetzt sie die bisherige (privater App-Speicher). Kaputte Schrift beim Start →
  Rückfall auf Inconsolata. **Restrisiko:** Schriften werden von Androids eigenem Schriftsystem
  ausgewertet; eine bösartige Schriftdatei könnte dort Fehler auslösen – sie kommt aber nur auf
  ausdrückliche Wahl des Nutzers in die App.
- **Eingaben von außen:** Größenlimits (IPC 1 MiB, Ledger-Seite 1000 Einträge / 8 MiB,
  Pairing 64 KiB), Timeouts, Fuzzing aller Parser (`fuzz/`).
- **Audit-Log:** abgelehnte und nicht-triviale Zugriffe fremder Geräte, entdoppelt und begrenzt.

## Desktop-App (Tauri)

- **Angreifer:** feindliche Dateinamen oder Metadaten, z. B. von gekoppelten Geräten oder aus
  Downloads, die in der Web-Ansicht landen (XSS).
- **Schutz in Schichten:**
  1. React stellt Namen nur als Text dar.
  2. Strenge CSP: nur eigene Skripte, keine Inline-Skripte, kein `eval`, keine Frames, `connect-src`
     nur zur App.
  3. Tauri-Rechte minimal: Ereignisse und Fensterleiste; keine Datei-, Shell- oder Dialogrechte.
  4. Keine allgemeine Durchreiche zum Daemon. Es gibt nur typisierte Befehle mit Eingabeprüfung
     (absolute Pfade ohne `..`, Längen, Mengen).
  5. Folgenreiches bestätigt ein **nativer** Dialog des Rust-Teils: endgültig löschen, Papierkorb
     leeren, ausführbare oder riskante Dateien öffnen, Rechte gewähren, entkoppeln,
     Fernänderungen zulassen, Sicherungen entfernen.
- Selbst ein gelungenes XSS kann daher höchstens rückgängig machbare Aktionen auslösen
  (Papierkorb, Verschieben, Umbenennen) und keine Daten nach außen senden.
- **Vorschaubild-Protokoll `thumb:`:** nur lesend, nur JPEG aus dem Cache des Dienstes.
  Pfadprüfung nach der URL-Dekodierung; kodierte Ausbrüche (`%2E%2E`) sind getestet.

## Android-App

- Der Kern läuft im App-Prozess (`apps/mobile/core-ffi`). Daten, Schlüssel und Socket liegen im
  privaten App-Ordner, geschützt durch die Android-Sandbox.
- Die Oberfläche ist mitgelieferter nativer Code (React Native). Feindliche Dateinamen werden als
  Text dargestellt, nicht als HTML. Folgenreiches bestätigt die App trotzdem in nativen Dialogen.
- Keine Google-Dienste und keine Tracker. Verteilung als signierte APK über GitHub.

## Betrieb in fremden Umgebungen (geprüft 2026-11-06)

- **Mehrbenutzer-Rechner:**
  - Datenordner, Socket-Ordner und Schlüssel sind nur für den Besitzer zugänglich (0700/0600).
  - Fremde Besitzer oder Symlinks führen zum Abbruch.
  - Der Socket nimmt nur Verbindungen derselben uid an (`SO_PEERCRED`), auch falls die Rechte
    geändert werden.
- **Kein `$XDG_RUNTIME_DIR`** (z. B. Cron, Docker, SSH ohne systemd-Sitzung): Der Socket liegt
  dann im privaten Datenordner, nie in `/tmp`.
- **Betrieb als root / in Docker:**
  - Dieselben Prüfungen gelten mit der uid 0.
  - Geschützte Pfade (Datenordner, Socket) lassen sich weder freigeben noch über Fernoperationen
    ändern.
  - Empfehlung: als eigener Benutzer betreiben, Daten als Volume einhängen.
- **Netzwerk-Dateisysteme:**
  - Werden nicht automatisch indexiert oder beobachtet.
  - Liegt der Datenordner selbst auf NFS/SMB/SSHFS, warnt der Daemon beim Start: SQLite (WAL) ist
    dort bei gleichzeitigem Zugriff mehrerer Rechner nicht sicher.
- **`unsafe`:**
  - Nur dünne libc-Aufrufe: `renameat2`, `statvfs`, `geteuid`, `syncfs`, `tcgetattr`/`tcsetattr`.
  - Jeder Block ist begründet; das erzwingt `clippy::undocumented_unsafe_blocks`.
  - Beim Audit behoben: Schlug `tcgetattr` fehl, hätte die Passwortabfrage ein genulltes
    `termios` zurückgeschrieben und damit das Terminal unbrauchbar gemacht.

## Bewusst nicht abgedeckt

- **Metadaten gegenüber Relais/Netz:** Wer wann mit wem verbunden ist, ist sichtbar
  (besonders im Modus „immer live“). Abhilfe: eigenes Relais (`relay_urls`, Docker-Vorlage in
  `gateway/relay/` mit Allowlist der eigenen Node-IDs), `use_relay = false` im LAN. Eigene Relais
  müssen `https://` verwenden und dürfen keine Zugangsdaten in der Adresse enthalten.
- **Kompromittiertes gekoppeltes Gerät:** kann alles, was ihm gewährt wurde, und seine eigenen
  Ledger-Einträge fälschen – nicht die anderer Geräte.
- **Kompromittierter lokaler Benutzer / root:** hat vollen Zugriff auf Daten und Schlüssel.
- **Daten im Ruhezustand:** Blob-Store und Datenbank sind nicht zusätzlich verschlüsselt
  (Festplattenverschlüsselung des Systems verwenden).
- **Ticket-Weitergabe:** Wer ein Ticket weitergibt, gibt die Datei weiter.

## Prüfung

- Fuzz-Ziele: Einladungscodes, Ledger-Einträge, IPC-Frames, Pfade/Namen, Live-Nachrichten.
- Angriffstests für jede Netzwerkschnittstelle (`core/tests/`), Rechte-Matrix für jede Anfrageart.
- CI: Tests, Clippy, `cargo audit`, `cargo deny`, Fuzzing (Kurzlauf je PR, 20 min nächtlich).
