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
