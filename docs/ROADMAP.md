# Snap – Vision & Roadmap

## Vision

Eine umfassende App für Desktop und Mobile, die **Dateiexplorer und Google Drive vereint** und
dabei möglichst viele weitere Apps ersetzt – ohne fremde Server als Pflicht und ohne
Abstriche bei der Sicherheit.

| Ersetzt | Funktion in Snap |
|---|---|
| Dateiexplorer | Lokale Ordner durchsuchen, kopieren, verschieben, umbenennen, Papierkorb |
| Google Drive / Dropbox / Syncthing | Ordner auf allen Geräten, offline anpinnen, Dateien anderer Geräte direkt öffnen |
| Google Fotos | Galerie, Zeitachse, automatischer Kamera-Upload vom Handy |
| VLC / Plex (teilweise) | Videos und Musik von anderen Geräten streamen (Byte-Bereiche statt Voll-Download) |
| WeTransfer / Freigabelinks | Teilen mit anderen, schreibgeschützt, mit Ablaufdatum und Widerruf |
| Backup-Tool | Versionen, Snapshots, verschlüsselte Kopien auf NAS oder S3 |
| Duplikat-Finder, Speicheranalyse | Duplikate und Platzverbrauch über alle Geräte |
| Suche (Everything, Spotlight) | Volltextsuche über alle Geräte, auch offline |

## Getroffene Entscheidungen

| Datum | Entscheidung | Begründung / Folgen |
|---|---|---|
| 2026-11-05 | **Umfassendes Produkt statt Nische** | Spacedrive als Anstoß; Snap soll möglichst viele Apps ersetzen. |
| 2026-11-05 | **Immer live**: gekoppelte Geräte halten Verbindungen, Änderungen erscheinen sofort | Drive-ähnliches Erlebnis. Löst das frühere Prinzip „keine Heartbeats“ ab. Anwesenheit der Geräte ist für Relais und Netzbeobachter sichtbar; ein eigenes Relais mildert das. |
| 2026-11-05 | **Desktop: Tauri v2 + React · Mobile: React Native (Expo) · gemeinsamer Rust-Core** | Mobile fühlt sich nativer an; Core per FFI eingebettet. Verlangt eine saubere, typisierte API-Schicht im Core. |
| 2026-11-05 | **Nach Phase 0 zuerst der Drive-Kern** | Baut direkt auf Ledger, Pairing und Warteschlange auf. |
| 2026-11-05 | Pairing per langem Einmal-Code, signiertes Weiterreichen von Ledger-Einträgen | Siehe README „Pairing & Ledger-Gossip“. |
| 2026-11-05 | **Rechte pro Gerät**: Standard beim Koppeln nur `catalog.read`; weitere Rechte (`files.read/write/delete` mit Ordner, `pin.request`) ausdrücklich per `pair grant` | Ein kompromittiertes Gerät kann wenig anrichten. Eine einzige Prüfstelle für alle Operationen. |
| 2026-11-05 | **`admin` (Fernverwaltung) vorerst gesperrt**, später nur mit Bestätigung am Zielgerät | Verhindert, dass ein Gerät sich selbst weitere Rechte oder Kopplungen verschafft. |
| 2026-11-05 | **Ordner-Freigaben**: neue Freigaben gelten für alle gekoppelten Geräte (auch später gekoppelte), einschränkbar per `--devices`; Zugriff auf Share-Dateien nur mit `files.read` auf den Ordner – Einzeldateien per `publish` behalten „Ticket reicht“ | Drive-Erlebnis ohne den Hash als einzigen Schlüssel. |
| 2026-11-05 | **Symlinks in Freigaben werden nicht verfolgt** | Verhindert versehentliches Teilen von Dateien außerhalb des Ordners (z. B. `~/.ssh`). |
| 2026-11-05 | **Gepinnte Dateien sichtbar unter `~/Snap/<Gerät>/<Freigabe>/…`**, Pfad und Kontingent einstellbar | Für alle Programme nutzbar. |
| 2026-11-05 | **Öffentliche Veröffentlichung auf GitHub geplant**, sobald fertig – andere sollen Snap für ihre Geräte nutzen | Der Code muss für fremde Nutzer und Umgebungen sicher sein (sichere Standardwerte, Mehrbenutzer-Rechner, Docker/root). Checkliste: Phase 7. |
| 2026-11-06 | **Erst das komplette Back-end, dann die Oberflächen**; Reihenfolge 2a Index & Suche → 2b Dateioperationen → 2c Vorschaubilder/Metadaten → 5a Streaming → 5b Hochladen → 5c Freigabelinks → 6 Schutz → 4a Einbettung (Mobile) | Snap soll die Standard-Dateimanager der Geräte vollständig ersetzen. |
| 2026-11-06 | **Lokaler Index auf der Platte (SQLite + FTS5-Trigramm), nicht im RAM**; Ziel: Daemon < 150 MB RAM bei 1 Mio. Dateien | Spacedrive misst 1,26–1,89 GB RAM für 1 Mio. Dateien (Index im Arbeitsspeicher) – für Desktop spürbar, für Mobile untragbar. |
| 2026-11-06 | Explorer liest jeden Ordner **live**; indexiert (für die Suche) werden Home und angeschlossene Laufwerke automatisch, mit Standard-Ausschlüssen (Caches, `node_modules`, Papierkorb …); weitere Orte hinzufügbar | Dateimanager-Ersatz ohne Einrichtungsaufwand. |
| 2026-11-06 | Der lokale Index bleibt **auf dem Gerät** (nicht an gekoppelte Geräte repliziert) | Gekoppelte Geräte sehen weiter nur Freigaben, nicht die gesamte Ordnerstruktur. |
| 2026-11-06 | Löschen über den **System-Papierkorb** (Freedesktop), Rückfallordner auf Laufwerken ohne eigenen | Zusammenspiel mit anderen Programmen und Wiederherstellung. |
| 2026-11-06 | **Dateioperationen lokal und über Geräte in einem Schritt** (2b): kopieren, verschieben, umbenennen, Ordner anlegen, Papierkorb, Rückgängig; gekoppelte Geräte nur innerhalb von Freigaben und nur mit ordnergebundenen Rechten | Drive-Erlebnis: Ordnung halten von jedem Gerät aus. |
| 2026-11-06 | **Fernlöschen landet im Papierkorb**; endgültiges Löschen nur mit dem eigenen Recht `files.purge` (nie Standard) | Ein kompromittiertes Gerät kann nichts unwiederbringlich zerstören, ohne dass es ausdrücklich erlaubt wurde. |
| 2026-11-06 | **Massenschutz**: Mehr als 100 verändernde Fernoperationen (Dateien) pro Gerät und Stunde werden angehalten, bis sie am Zielgerät bestätigt werden (`ops_hourly_limit`) | Schutz vor Ransomware-artigem Verhalten eines kompromittierten Geräts. |
| 2026-11-06 | **Vorschaubilder nur mit reinen Rust-Decodern** (JPEG, PNG, GIF, WebP, BMP, TIFF); Video-, HEIC- und PDF-Vorschauen später, mit isolierten Decodern | Fremde Dateien sind Angriffsfläche; C-Bibliotheken wie ffmpeg oder libheif nur abgeschottet. |
| 2026-11-06 | **Streaming über einen lokalen HTTP-Server** (nur 127.0.0.1, Einmal-Adressen mit 256-Bit-Token, Host-Prüfung, nur GET/HEAD) | Funktioniert mit jedem Player (VLC, mpv, Browser) und der späteren App; die Rechte prüft weiterhin das Ursprungsgerät. |
| 2026-11-06 | **Teilweise gestreamte Dateien bleiben im Cache** (`stream_cache_mb`, Standard 2 GB, älteste zuerst) | Weiterschauen ohne erneutes Laden. |
| 2026-11-06 | **Vorschaubilder fremder Freigaben** für gekoppelte Geräte mit `files.read` (`snap/thumb/1`) | Galerie-Ansicht ohne die Originale zu laden. |
| 2026-11-05 | E2EE für eigene Geräte nicht nötig (QUIC/TLS) | **Wird wieder relevant** für Cloud-Backup-Ziele und Freigaben an Dritte (Phase 5/6). |

## Sicherheitsprinzipien

1. **Fähigkeiten statt Pauschalvertrauen.** Ein gekoppeltes Gerät bekommt einzelne Rechte
   (lesen, pinnen, schreiben, verwalten), nie automatisch die gesamte API.
2. **Jede Kopplung und jede Freigabe ist sichtbar, begrenzbar und widerrufbar.**
3. **Verschlüsselung dort, wo Daten fremde Hardware berühren** (Cloud, Dritte).
4. **Riskante Operationen** (Löschen, Verschieben, Deduplizieren) laufen als
   Vorschau → Ausführung → Prüfung; Gelöschtes landet im Papierkorb.
5. **Geheimnisse nie ungeprüft vertrauen:** Identitäten kommen aus TLS, nicht aus
   behaupteten Nachrichteninhalten; Pairing-Geheimnisse werden kryptografisch nachgewiesen.
6. **Jede neue Netzwerkschnittstelle bekommt Angriffstests** (fremde, entkoppelte,
   manipulierte Gegenstellen) bevor sie gemergt wird.

## Phasen

| Phase | Inhalt | Status |
|---|---|---|
| **0a** | Core härten (PR #1), Pairing & Ledger-Gossip (PR #2) | ✅ |
| **0b** | **API-Schicht**: Fehlercodes, API-Version, Ereignis-Stream, seitenweiser Katalog, Rechte pro Gerät mit Audit-Log, Modulaufteilung | ✅ |
| **1 – Drive-Kern** (1a Shares + Rechte ✅ PR #4 · 1b Live ✅ PR #6 · 1c Pinning ✅ PR #7) | Ordner freigeben und beobachten (Watcher), relative Pfade im Ledger, **Smart-Pinning mit mehreren Quellen** (Server springt ein), kein doppelter Speicher (`TryReference`), Anzeige der Kopien, **Live-Verbindungen** zwischen gekoppelten Geräten | offen |
| **2 – Explorer-Kern** (2a Index & Suche ✅ PR #8 · 2b Dateioperationen ✅ PR #9 · 2c Vorschaubilder & Metadaten ✅ PR #10) | Index des lokalen Dateisystems, FTS5-Suche, Ordner live lesen, Dateioperationen mit Papierkorb, Vorschaubilder, Metadaten | offen |
| **3 – Desktop-App** | Tauri v2 + React im Explorer-Stil: Geräte mit Status, `space://`-Adressleiste, Speicher- und Pinning-Einstellungen | offen |
| **4 – Mobile-App** | React Native (Expo) mit eingebettetem Rust-Core, Android-Speicherzugriff, akkuschonend, Kamera-Upload | offen |
| **5 – Medien & Teilen** (5a Streaming + Fern-Vorschaubilder ✅ PR #11) | Streaming über Byte-Bereiche, Foto-Zeitachse, Freigabelinks mit Ablauf und Widerruf, eigenes Iroh-Relais | offen |
| **6 – Schutz** | Versionen und Snapshots, verschlüsselte Backup-Ziele, Duplikat-Bereinigung, Speicheranalyse | offen |
| **7 – Veröffentlichung** | Sicherheits-Checkliste unten abarbeiten, dann öffentliches Repository | offen |

## Checkliste vor der Veröffentlichung (Phase 7)

- [ ] **Git-Historie bereinigen**: Der erste Upload enthält `space.db`, `space_ledger.db` und Testdateien (Tickets sind wertlos, trotzdem entfernen) – neues Repository oder `git filter-repo`.
- [ ] **Lizenz** festlegen (z. B. Apache-2.0 oder MIT/Apache dual).
- [x] **`SECURITY.md`** mit vertraulichem Meldeweg; **Bedrohungsmodell** (`docs/SECURITY_MODEL.md`) – vor Veröffentlichung aktualisieren.
- [x] **CI** (GitHub Actions, auf Commit-SHAs gepinnt): `fmt`, `clippy -D warnings`, `cargo test`, `cargo audit`, `cargo deny`.
- [x] **Fuzzing** (`fuzz/`): Einladungscodes, Ledger-Einträge, IPC-Frames, Pfad-/Namensprüfung – Kurzlauf je PR, 20 min nächtlich. Neue Parser bekommen ein Ziel.
- [ ] **Sichere Standardwerte** für fremde Umgebungen prüfen: Mehrbenutzer-Rechner, Betrieb als root/Docker, fehlendes `$XDG_RUNTIME_DIR`, Netzwerk-Dateisysteme.
- [ ] **`unsafe`-Audit** (derzeit nur `libc::geteuid`) und Abhängigkeits-Review.
- [ ] **Signierte Releases**, reproduzierbare Builds, SBOM.
- [ ] Unabhängiges **Sicherheits-Review** (zweites Augenpaar) vor Version 1.0.

