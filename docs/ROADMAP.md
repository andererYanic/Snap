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
| 2026-11-06 | **Hochladen: das Zielgerät holt die Datei** – Auftrag über `snap/ops/1`, das Ziel lädt den Inhalt BLAKE3-geprüft per iroh-blobs vom Auftraggeber (Angebot gilt nur für dieses eine Gerät); nur mit `files.write`, nie überschreiben | Kein neuer Datenweg, fortsetzbar, der Inhalt ist durch den Hash garantiert. |
| 2026-11-06 | **Grenzen fürs Hochladen**: 20 GB pro Gerät und Tag (`upload_daily_gb`), mindestens 5 GB bleiben frei (`upload_reserve_gb`) | Ein kompromittiertes Gerät kann die Platte nicht füllen. |
| 2026-11-06 | **Kamera-Upload-Regeln im Back-end**: Ordner beobachten und neue Dateien automatisch zu einem Gerät/einer Freigabe hochladen (Warteschlange, ohne Duplikate) | Ersetzt Google Fotos' Sicherung; die Mobile-App nutzt dieselben Regeln. |
| 2026-11-06 | **Freigabelinks über die eigene Domain mit Cloudflare Worker + R2** (kostenloser Bereich), Ende-zu-Ende-verschlüsselt: Schlüssel nur im Link-Teil nach `#`; verschlüsselte Kopie in R2 bis Ablauf/Widerruf; Standard 7 Tage, unbegrenzt oft; optional Passwort. Rückfall: Cloudflare Tunnel/Docker auf eigenem Server | Personen ohne Snap erreichen Dateien, auch wenn die eigenen Geräte aus sind; Cloudflare sieht nur verschlüsselte Daten und Metadaten. |
| 2026-11-06 | **Sicherungen und Versionen in einer eigenen Engine (Rust)**: Stücke nach Inhalt (FastCDC), dedupliziert, Ende-zu-Ende-verschlüsselt (AES-256-GCM, ring); Ziele: externe Platte/USB und Cloudflare R2/S3 | Voll integriert (Fortschritt, einzelne Dateien wiederherstellen, App, Mobile); Ziele sehen keinen Inhalt. |
| 2026-11-06 | **Sicherungsschlüssel: Passwort + Wiederherstellungsschlüssel**; der Hauptschlüssel liegt zusätzlich auf dem sichernden Gerät (0600), damit stündliche Sicherungen ohne Eingabe laufen | Ohne Passwort und ohne Wiederherstellungsschlüssel ist eine Sicherung auf einem neuen Gerät nicht lesbar. |
| 2026-11-06 | **Zeitplan: stündlich; behalten 24 Stunden / 30 Tage / 12 Monate** (änderbar) | Feine Versionen für den Alltag, lange Aufbewahrung bei moderatem Platz. |
| 2026-11-06 | **Grenzen zur Laufzeit änderbar** (`settings set`, ohne Neustart; Standard aus `config.toml`) | Im Alltag nachjustieren, ohne den Daemon neu zu starten. |
| 2026-11-06 | **Lizenz GPL-3.0** | Freie Software; veränderte Weitergaben müssen offen bleiben. Alle Abhängigkeiten (MIT, Apache-2.0, BSD, MPL-2.0 …) sind mit GPL-3.0 verträglich. |
| 2026-11-06 | **Veröffentlichung als neues Repository** (statt Historie umschreiben) | Keine alten Datenbankdateien in der öffentlichen Historie; private Historie bleibt unverändert als Archiv. |
| 2026-11-06 | **Relais: vorerst n0, eigenes Relais vorbereitet** (`relay_urls`, `relay_fallback_n0`, Docker unter `gateway/relay/`) | Wechsel ohne Codeänderung möglich; eigenes Relais nur für eigene Geräte (Allowlist). |
| 2026-11-06 | **Nächste Phase: Desktop-App (Tauri v2 + React)** | Backend weitgehend fertig; die App macht es im Alltag nutzbar und testbar. |
| 2026-11-06 | **App-Brücke: nur typisierte Befehle, native Bestätigung für Folgenreiches** (endgültig löschen, Papierkorb leeren, ausführbare Dateien öffnen); strenge CSP, minimale Tauri-Rechte | Ein XSS über feindliche Dateinamen bekäme sonst volle Adminrechte. |
| 2026-11-06 | **App ist Client eines eigenständigen Dienstes** (systemd-Benutzerdienst), startet ihn bei Bedarf | Sicherungen, Uploads und Abgleich laufen weiter, wenn das Fenster zu ist. |
| 2026-11-07 | **Android-App ohne Google**: keine Play-Dienste, kein Firebase/FCM, keine Analyse- oder Absturzdienste; Verteilung nur als signierte APK über GitHub-Releases | Wunsch des Nutzers: keine Google-Tracker. `dl.google.com` wird nur beim Bauen für SDK/NDK genutzt. |
| 2026-11-07 | **Android: Zugriff „Alle Dateien“** (MANAGE_EXTERNAL_STORAGE) für einen vollwertigen Explorer | Ersetzt den Dateimanager des Geräts; ohne Play Store unproblematisch. |
| 2026-11-07 | **Android-Hintergrund: live, solange die App offen ist; sonst AndroidX WorkManager** (lokal, akkuschonend, ohne Dauerbenachrichtigung) | Ausgewogen zwischen Erreichbarkeit und Akku. |
| 2026-11-07 | **Android-Gestaltung: Snap-Optik mit Android-Bedienung**, Material You wahlweise in den Einstellungen; Aufbau nach UX-Grundsätzen (Daumenzone, sichtbare untere Navigation, 48-dp-Tippziele, Rückgängig statt Nachfragen) – `docs/MOBILE_UX.md` | Wiedererkennung zum Desktop, bequem einhändig bedienbar. |
| 2026-11-07 | **Android: Videos über Media3/ExoPlayer** (via `react-native-video`, ohne IMA-Werbemodul und Streaming-Formate) | Spielt fast alle Formate lokal ab, ohne Google-Dienste. |
| 2026-11-07 | **Android: Mehrfachauswahl per Halten-und-Wischen, Kontextmenü am Finger** statt Aktionsleiste unten; Suche beim Tippen mit Tippfehler-Toleranz (im Kern, Damerau-Levenshtein) | Feedback des Nutzers: Bedienung wie bei iOS, weniger „kantig“. |
| 2026-11-07 | **Android-Aufbau: Tabs Dateien, Fotos, Einstellungen**; „Geräte“ wandert in die Einstellungen (Unterkategorien „Allgemein“, „Geräte“ …); die Startseite zeigt den Speicher aller gekoppelten Geräte | Wunsch des Nutzers; erprobt im Designlabor (`apps/mobile/prototype/`). |
| 2026-11-07 | **Mehrfachauswahl: Menü am Finger nur bei genau einer Datei**, ab zwei eine Aktionsleiste unten, die offen bleibt, bis alles abgewählt ist; Menüeinträge je Format | Wunsch des Nutzers. |
| 2026-11-07 | **Dateifunktionen: Häufiges selbst, Spezielles über Android-Intents** („Bearbeiten/Öffnen mit …“), keine Sonderschnittstellen zu einzelnen Apps. Entpacken im Rust-Kern (ZIP inkl. AES-Passwort, tar/tar.gz; neuer Ordner, nichts überschreiben, kein Pfad nach außen, keine Verknüpfungen, Grenzen gegen Zip-Bomben, Rückgängig, Fuzz-Ziel); Drucken über das Android-Druckframework; Bilder (Zuschneiden, Drehen, Anpassen, Filter) und Videos (Kürzen, Ton aus, Drehen; Media3 Transformer) selbst; nach dem Bearbeiten immer fragen (Kopie vorausgewählt / Original ersetzen → Papierkorb) | Flüssig und nativ, ohne Google; Entwurf vom Nutzer freigegeben. |
| 2026-11-07 | **DOCX drucken/in PDF: zunächst Übergabe an eine Office-App** (z. B. Collabora über F-Droid); später optional Umwandlung auf eigenem Laptop/Server mit LibreOffice – dafür eigenes Sicherheitsdesign | Originalgetreue DOCX-Darstellung braucht eine Office-Engine (> 100 MB). |
| 2026-11-07 | **DOCX: eingebaute einfache Leseansicht** (Text, Überschriften, Listen, Tabellen, Bilder – lokal, ohne WebView; Original über „Mit Office-App öffnen“) **und optional ein eigener Collabora-Server** in den Einstellungen (wie bei Nextcloud) – Design dazu wird vorab vorgelegt | Wunsch des Nutzers: schlank, ohne Google-Cloud; originalgetreu über eigenen Server. |
| 2026-11-07 | **PDF, Text und Markdown in der App**: PDF über Androids `PdfRenderer`, Drucken über das Druckframework, Markdown nativ ohne HTML | Flüssig, ohne Zusatzbibliotheken. |
| 2026-11-07 | **Geräte melden ihren Speicher**: nur Summen (gesamt/frei) der Freigaben, nur an gekoppelte Geräte mit Leserecht; Angriffstests | Für die Startseite; freigegeben vom Nutzer. |
| 2026-11-07 | **Desktop zusätzlich als Flatpak** (nach 4b): `.flatpak` auf GitHub, Flathub möglich | Wunsch des Nutzers. |
| 2026-11-07 | **Mehrsprachigkeit: Deutsch als Ausgangssprache, Englisch mitgeliefert**; deutscher Text = Schlüssel, eine JSON-Datei je Sprache (`apps/desktop/locales/`), automatisch erkannt von Oberfläche und nativen Dialogen; Test auf Vollständigkeit | Wunsch des Nutzers: englische Version und einfacher Weg für eigene Sprachen. |
| 2026-11-06 | **Gestaltung: Fluent-Designsprache** (Mica-Hintergrund, Acryl, Tabs in der Titelleiste) nach dem Vorbild der Explorer-Alternative „Files“ – eigene Umsetzung ohne fremde Grafiken | Wunsch des Nutzers: klar, modern, durchscheinend. |
| 2026-11-06 | **Eigene PRs bei grüner CI selbst mergen** | Freigabe des Nutzers; Designs sicherheitsrelevanter Systeme werden weiterhin vorher vorgelegt. |
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
| **3 – Desktop-App** (3a Gerüst, sichere Brücke, Explorer, Fotos ✅ PR #21 · 3b Geräte, Sicherungen, Einstellungen ✅) | Tauri v2 + React im Explorer-Stil: Geräte mit Status, `space://`-Adressleiste, Speicher- und Pinning-Einstellungen | offen |
| **4 – Mobile-App** (4a eingebetteter Kern über UniFFI ✅ PR #23 · 4b erste App: Dateien, Fotos, Geräte, WorkManager ✅) | React Native (ohne Expo-Clouddienste, ohne Google) mit eingebettetem Rust-Core, Android-Speicherzugriff, akkuschonend, Kamera-Upload | offen |
| **5 – Medien & Teilen** (5a Streaming + Fern-Vorschaubilder ✅ PR #11 · 5b Hochladen + Kamera-Upload ✅ PR #13 · 5c Freigabelinks ✅ PR #14 · 5d Foto-Zeitachse ✅ PR #18 · eigenes Relais vorbereitet ✅) | Streaming über Byte-Bereiche, Foto-Zeitachse, Freigabelinks mit Ablauf und Widerruf, eigenes Iroh-Relais | ✅ |
| **6 – Schutz** (6a Speicheranalyse + Duplikate ✅ PR #15 · 6b-1 Sicherungs-Engine ✅ PR #16 · 6b-2 Zeitplan, Aufbewahrung, R2 ✅ PR #17) | Versionen und Snapshots, verschlüsselte Backup-Ziele, Duplikat-Bereinigung, Speicheranalyse | ✅ |
| **7 – Veröffentlichung** | Sicherheits-Checkliste unten abarbeiten, dann öffentliches Repository | offen |

## Checkliste vor der Veröffentlichung (Phase 7)

- [ ] **Git-Historie bereinigen**: Der erste Upload enthält `space.db`, `space_ledger.db` und Testdateien. Entschieden: **neues öffentliches Repository** mit sauberem ersten Commit; das private Repository bleibt als Archiv.
- [x] **Lizenz**: GPL-3.0 (`LICENSE`, `license = "GPL-3.0-only"` in allen Crates).
- [x] **`SECURITY.md`** mit vertraulichem Meldeweg; **Bedrohungsmodell** (`docs/SECURITY_MODEL.md`) – vor Veröffentlichung aktualisieren.
- [x] **CI** (GitHub Actions, auf Commit-SHAs gepinnt): `fmt`, `clippy -D warnings`, `cargo test`, `cargo audit`, `cargo deny`.
- [x] **Fuzzing** (`fuzz/`): Einladungscodes, Ledger-Einträge, IPC-Frames, Pfad-/Namensprüfung – Kurzlauf je PR, 20 min nächtlich. Neue Parser bekommen ein Ziel.
- [x] **Sichere Standardwerte** für fremde Umgebungen prüfen: Mehrbenutzer-Rechner, Betrieb als root/Docker, fehlendes `$XDG_RUNTIME_DIR`, Netzwerk-Dateisysteme (Ergebnis: `docs/SECURITY_MODEL.md`, Warnung bei Datenordner auf Netzwerk-Dateisystem).
- [x] **`unsafe`-Audit**: 6 libc-Aufrufe, alle begründet und per Lint erzwungen; ein Fehler (Terminal-Wiederherstellung) behoben.
- [x] **Abhängigkeits-Review** über `cargo audit`/`cargo deny` hinaus: `docs/DEPENDENCIES.md` (offen: `rusqlite`-Update).
- [ ] **Signierte Releases**, reproduzierbare Builds, SBOM.
- [ ] Unabhängiges **Sicherheits-Review** (zweites Augenpaar) vor Version 1.0.

