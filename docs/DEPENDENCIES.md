# Abhängigkeiten (Review 2026-11-06)

Automatisch geprüft in der CI: `cargo audit` (bekannte Lücken) und `cargo deny` (Lizenzen, Quellen,
gesperrte Crates). Dieses Dokument ergänzt das um eine manuelle Einschätzung der **direkten**
Abhängigkeiten von `space-core`. Insgesamt umfasst der Daemon rund 460 Crates im Baum, die meisten
davon über Iroh.

| Crate | Zweck | Einschätzung |
|---|---|---|
| `iroh`, `iroh-blobs`, `n0-future` | P2P-Verbindungen (QUIC), Blob-Übertragung | Kern des Projekts. Aktiv gepflegt (n0). Größter Teil des Baums. |
| `tokio` | Laufzeit | Standard, sehr gut gepflegt |
| `rusqlite` (bundled SQLite) | Datenbank, Index, FTS5 | Gut gepflegt. SQLite wird mitgebaut, damit Versionsstand und FTS5 garantiert sind. Update von 0.32 möglich (bringt `hashbrown`-Duplikat weg). |
| `ring` | AES-GCM, HKDF, HMAC, PBKDF2, SHA-256 | Etabliert und geprüft. Bewusst keine eigene Kryptografie. |
| `rustls`, `webpki-roots`, `reqwest` | HTTPS zu S3/R2 | Standard. Nur HTTPS, eigene Wurzelzertifikate (kein System-Store). |
| `hyper`, `hyper-util`, `http-body-util` | Lokaler Streaming-Server (127.0.0.1) | Standard |
| `blake3` | Inhalts-Hashes (Duplikate, Blobs) | Offizielle Implementierung |
| `image`, `jpeg-decoder` | Vorschaubilder | Pure Rust. Größenprüfung vor dem Dekodieren (eigene, s. `media.rs`). |
| `kamadak-exif` | EXIF (Aufnahmezeit, Ausrichtung, GPS) | Klein, pure Rust. Fuzz-Ziel `media`. |
| `lofty` | Audio-/Video-Tags | Pure Rust. Endlosschleife bei ADTS-AAC (per Fuzzing gefunden) umgangen: für AAC nur Tags, keine Dauer. Fuzz-Ziel `media`. |
| `notify` | Dateisystem-Beobachtung (inotify) | Standard |
| `walkdir` | Verzeichnisdurchlauf (Sicherungen) | Klein, stabil |
| `serde`, `serde_json`, `toml` | Serialisierung, Konfiguration | Standard |
| `chrono` | Zeitangaben (Aufbewahrung, Zeitachse) | Standard. Nur Rechnen, keine Zeitzonen-Datenbank. |
| `anyhow`, `tracing` | Fehler, Protokoll | Standard |
| `data-encoding`, `fastrand`, `getrandom`, `bytes` | Hex/Base32, Zufall (nicht kryptografisch bzw. Betriebssystem), Puffer | Klein, stabil. Geheimnisse nur aus `getrandom`. |
| `directories` | XDG-Pfade | Klein. Update auf v6 möglich. |
| `libc` | 6 Systemaufrufe (s. `SECURITY_MODEL.md`, `unsafe`) | Standard |

## Ergebnisse

- Aktualisiert: `getrandom` 0.2 → 0.4 und `toml` 0.8 → 1. Das entfernt Doppelversionen im Baum
  (`getrandom` 0.2 bleibt nur noch über `ring`).
- Bekannte, erlaubte Warnungen (`deny.toml`): `atomic-polyfill`, `paste` und `proc-macro-error`
  sind ungepflegt. Sie kommen nur indirekt (über Iroh bzw. Build-Makros) und sind ohne bekannte
  Lücke.
- **Offen:** `rusqlite` auf die aktuelle Version heben (Schema-Tests decken das ab).
  Doppelversionen der Netlink-Crates entstehen innerhalb von Iroh und lassen sich nur dort lösen.
