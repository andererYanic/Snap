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
| **0b** | **API-Schicht**: Fehlercodes, API-Version, Ereignis-Stream, seitenweiser Katalog, Rechte pro Gerät mit Audit-Log, Modulaufteilung | ✅ PR #3 |
| **1 – Drive-Kern** | Ordner freigeben und beobachten (Watcher), relative Pfade im Ledger, **Smart-Pinning mit mehreren Quellen** (Server springt ein), kein doppelter Speicher (`TryReference`), Anzeige der Kopien, **Live-Verbindungen** zwischen gekoppelten Geräten | offen |
| **2 – Explorer-Kern** | Index des lokalen Dateisystems, FTS5-Suche, Dateioperationen mit Papierkorb, Vorschaubilder, Metadaten | offen |
| **3 – Desktop-App** | Tauri v2 + React im Explorer-Stil: Geräte mit Status, `space://`-Adressleiste, Speicher- und Pinning-Einstellungen | offen |
| **4 – Mobile-App** | React Native (Expo) mit eingebettetem Rust-Core, Android-Speicherzugriff, akkuschonend, Kamera-Upload | offen |
| **5 – Medien & Teilen** | Streaming über Byte-Bereiche, Foto-Zeitachse, Freigabelinks mit Ablauf und Widerruf, eigenes Iroh-Relais | offen |
| **6 – Schutz** | Versionen und Snapshots, verschlüsselte Backup-Ziele, Duplikat-Bereinigung, Speicheranalyse | offen |
