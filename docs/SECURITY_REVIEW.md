# Auftrag für ein unabhängiges Sicherheits-Review

Dieses Dokument ist die Arbeitsgrundlage für externe Prüfer. Es beschreibt Umfang, Angriffsflächen,
Annahmen und wie sich alles lokal nachstellen lässt. Das Bedrohungsmodell steht in
[`SECURITY_MODEL.md`](SECURITY_MODEL.md), Meldewege in [`../SECURITY.md`](../SECURITY.md).

## Ziel

Snap verbindet die eigenen Geräte eines Nutzers direkt (Peer-to-Peer, Iroh/QUIC mit TLS 1.3 und
Ed25519-Geräteschlüsseln). Geprüft werden soll, ob **ein fremdes oder ein gekoppeltes, aber
böswilliges Gerät** mehr erreichen kann als vorgesehen, und ob **lokale Eingaben** (Dateien,
Archive, Dokumente, Medien, Dateinamen) den Dienst oder die Oberflächen angreifen können.

## Umfang (nach Priorität)

1. **Netzwerkprotokolle** (alle über Iroh-Endpunkte, Identität aus TLS):
   | ALPN | Zweck | Code |
   |---|---|---|
   | `snap/pair/1` | Kopplung mit Einladungscode | `core/src/pairing.rs` |
   | `snap/ledger/1` | signierter Katalog (Abgleich) | `core/src/ledger.rs`, `core/src/sync.rs` |
   | `snap/live/1` | Live-Ereignisse, Speichermeldung | `core/src/live.rs` |
   | `snap/ops/1` | Fernoperationen inkl. Hochladen und Entpacken | `core/src/remote_ops.rs`, `core/src/upload.rs` |
   | `snap/thumb/1` | Vorschaubilder fremder Freigaben | `core/src/remote_thumbs.rs` |
   | iroh-blobs | Dateiinhalte (BLAKE3/bao-verifiziert) | `core/src/network.rs` |
2. **Rechteprüfung**: `core/src/api/auth.rs` (`required_capability`, `authorize`), ordnergebundene
   Rechte, Massenschutz und Bestätigungen (`OpsPending/Approve`).
3. **Parser für Eingaben von außen**: Archive (`core/src/fileops/archive.rs`), DOCX
   (`core/src/docx.rs`), Medien/Metadaten (`core/src/media.rs`), Papierkorb-Infos, Einladungscodes,
   IPC-Rahmen, HTTP-Range (Streaming).
4. **Lokale Schnittstellen**: IPC-Socket mit `SO_PEERCRED` (`core/src/ipc.rs`), Streaming-Server auf
   127.0.0.1 (`core/src/stream.rs`), Tauri-Brücke mit typisierten Befehlen und nativen
   Bestätigungen (`apps/desktop/src-tauri/src/`), Android-Modul (`apps/mobile/app/android/…/SnapCoreModule.kt`,
   `ImageEdit.kt`, `VideoEdit.kt`).
5. **Freigabelinks**: Ende-zu-Ende-Verschlüsselung im Client (`core/src/links/crypto.rs`, Format in
   [`LINK_FORMAT.md`](LINK_FORMAT.md)) und Cloudflare-Worker (`gateway/cloudflare/worker.js`).
6. **Sicherungen**: Verschlüsselung, Schlüsseldatei, Wiederherstellung (`core/src/backup/`,
   [`BACKUP_FORMAT.md`](BACKUP_FORMAT.md)).
7. **Lieferkette**: `deny.toml`, gepinnte Actions, Release-Workflow ([`RELEASES.md`](RELEASES.md)).

## Annahmen (bewusst außerhalb)

- Das eigene Gerät und das Benutzerkonto sind vertrauenswürdig (kein Schutz gegen Root/Malware).
- Gekoppelte Geräte sind *teilweise* vertrauenswürdig: nur im Rahmen ihrer Rechte.
- Siehe „Bewusst nicht abgedeckt“ in [`SECURITY_MODEL.md`](SECURITY_MODEL.md).

## Konkrete Fragen an die Prüfer

- Kann ein nicht gekoppeltes Gerät irgendeine Antwort außer dem Verbindungsabbau erhalten?
- Kann ein gekoppeltes Gerät außerhalb einer Freigabe lesen/schreiben (Pfade, Symlinks, TOCTOU,
  Unicode, Groß-/Kleinschreibung, Hardlinks)?
- Lassen sich Rechte oder Bestätigungen umgehen (Massenschutz, angehaltene Anfragen, Neustart)?
- Ressourcen: Speicher/CPU/Datenträger erschöpfen (Archive, Katalog, Vorschaubilder, Uploads)?
- Kryptografie: Kopplung (Code-Entropie, Replay), Katalogsignaturen, Link- und Backup-Format.
- Oberflächen: XSS in der Tauri-Ansicht trotz CSP; Missbrauch der typisierten Befehle.

## Nachstellen

```bash
cargo test --workspace
```

```bash
cargo test -p space-core --test remote_ops
```

```bash
cargo deny check && cargo audit
```

Fuzzing (Nightly-Rust, `cargo-fuzz`); Ziele in `fuzz/fuzz_targets/` prüfen Invarianten, nicht nur
Abstürze:

```bash
cd fuzz && cargo +nightly fuzz run ops_request -- -max_total_time=600
```

Leistungsgrenzen (relevant für DoS-Fragen): `core/tests/perf.rs`, Ergebnisse in
[`PERFORMANCE.md`](PERFORMANCE.md).

## Vorhandene Angriffstests (Auswahl)

- `core/tests/remote_ops.rs`: fremde/entkoppelte Gegenstellen, manipulierte Rahmen, Pfadtricks,
  Symlinks, Massenschutz, Entpacken (Zip-Slip, Passwort-Raten, Passwort nie gespeichert).
- `core/tests/two_nodes.rs`, `core/tests/live.rs`, `core/tests/upload.rs`, `core/tests/stream.rs`,
  `core/tests/links.rs`, `gateway/cloudflare/test/`.

## Ergebnis

Bitte Befunde mit Schweregrad (CVSS oder hoch/mittel/niedrig), betroffenem Commit und Schritten zum
Nachstellen über die private Meldefunktion (siehe `SECURITY.md`) melden. Behobene Befunde werden mit
Regressionstest (Angriffstest bzw. Fuzz-Korpus) geschlossen und in `SECURITY_MODEL.md` vermerkt.
