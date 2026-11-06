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
- **Eingaben von außen:** Größenlimits (IPC 1 MiB, Ledger-Seite 1000 Einträge / 8 MiB,
  Pairing 64 KiB), Timeouts, Fuzzing aller Parser (`fuzz/`).
- **Audit-Log:** abgelehnte und nicht-triviale Zugriffe fremder Geräte, entdoppelt und begrenzt.

## Bewusst nicht abgedeckt

- **Metadaten gegenüber Relais/Netz:** Wer wann mit wem verbunden ist, ist sichtbar
  (besonders im Modus „immer live“). Abhilfe: eigenes Relais, `use_relay = false` im LAN.
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
