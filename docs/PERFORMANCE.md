# Performance-Reviews

Nach jedem neuen System wird geprüft, wie der Code effizienter werden kann, damit die Apps
(Desktop/Mobile) später flüssig laufen. Maßstab: **Eine GUI-Abfrage darf nie länger als ein
Frame (16 ms) blockieren**, auch nicht während Abgleichen oder Transfers.

Messungen: `cargo test --release -p space-core --test perf -- --ignored --nocapture`
(4 Kerne, Release-Build, SQLite im WAL-Modus auf Datei).

## Review 1 – Core + Ledger-Gossip (2026-11-05)

| Hot Path | Vorher | Nachher | Maßnahme |
|---|---|---|---|
| Lesen während Schreiblast (p99 / max) | 19,2 ms / 61 ms | **0,06 ms / 0,8 ms** | Ein Schreiber + Pool aus 4 Leseverbindungen (WAL) |
| Fremde Ledger-Einträge übernehmen | 106 µs/Eintrag | **8,5–11 µs** | Eine Transaktion pro Seite statt Autocommit je Zeile |
| Signaturprüfung (Seite à 1000) | 65 ms, auf dem Runtime-Thread | **27 ms, außerhalb der Runtime** | Parallel auf allen Kernen in `spawn_blocking` |
| `entries_since` (Antwortseite) | auf dem Runtime-Thread | in `spawn_blocking` | blockiert IPC/Transfers nicht mehr |
| `is_shared` (pro Blob-Anfrage) | 7 µs | 7–9 µs | unkritisch |
| Statements | jedes Mal neu vorbereitet | `prepare_cached` (64 pro Verbindung) | |
| Hintergrund-Abgleich bei `list` | konnte sich überlappen, wartete bei jedem Aufruf auf Timeouts offline Geräte | einmal gleichzeitig, Geräte im Backoff übersprungen, parallel | |

### Offene Punkte (in späteren Phasen)

| Punkt | Wirkung | Wann |
|---|---|---|
| ~~`remote_files` lädt alle Einträge~~ | erledigt in Review 2 (`Browse` mit Cursor) | ✅ |
| Publish 260–290 µs/Datei (2 Commits + Signatur) | Beim Ordner-Import Stapelverarbeitung in einer Transaktion | Phase 1 |
| ~~Publish kopiert die Datei in den Blob-Store~~ | Freigaben importieren per Referenz (Review 4); Einzel-`publish` kopiert weiterhin (sicherer, da nicht beobachtet) | ✅ teilweise |
| ~~Jeder Download öffnet eine neue QUIC-Verbindung~~ | Verbindungen je Gerät werden wiederverwendet (Review 6) | ✅ |
| ~~Warteschlangen-Worker arbeitet seriell~~ | bis zu 8 Aufträge parallel (Review 6) | ✅ |
| Export kopiert den Blob ein zweites Mal | Reflink/Hardlink wo möglich | Phase 1 |
| Hashes als 64-Zeichen-Text in SQLite | 2× Platz ggü. BLOB(32); erst bei sehr großen Katalogen relevant | Phase 2 |
| Synchrone SQLite-Aufrufe in async-Handlern | bei kurzen Abfragen unkritisch; lange Abfragen in `spawn_blocking` | laufend |

## Review 2 – API-Schicht: Browse & Ereignisse (2026-11-05)

Messung mit 100 000 Einträgen eines anderen Geräts + 5 000 eigenen Dateien.

| Hot Path | Erste Version | Nachher | Maßnahme |
|---|---|---|---|
| Erste Seite (100 Einträge) | 167 ms | **0,97 ms** | Getrennte Abfragen je Tabelle über den Namensindex, Zusammenführen in Rust, Gesamtzahl zwischengespeichert |
| Tiefe Seite (~Eintrag 90 000) | 340 ms (`OFFSET`) | **0,2 ms** (Cursor) | Keyset-Pagination mit indexfähiger Bedingung (`>=`) |
| Tiefe Seite per `OFFSET` | 340 ms | 140 ms | bleibt O(Offset) – API begrenzt Offset auf 100 000 und verweist auf `after` |
| Suche (Teilstring) | 30 ms | 22 ms | `LIKE '%…%'` nutzt keinen Index → FTS5 (Trigram) in Phase 2 |
| Gesamtzahl | 9–13 ms je Abfrage | einmal je Katalogänderung | Cache, geleert bei `LocalCatalogChanged`/`RemoteCatalogChanged` |
| Ereignisse | – | ~3 µs pro übernommenem Eintrag | unkritisch |

**Ursache der ersten Version:** `UNION ALL` + `COUNT(*) OVER ()` und die Wahl des
Lösch-Index (`deleted = 0`) zwangen SQLite, alle Zeilen in einem temporären B-Baum zu
sortieren. `+l.deleted` / `+deleted_at` schaltet diesen Index für die Seitenabfrage ab;
der Namensindex liefert die Reihenfolge direkt (`EXPLAIN QUERY PLAN` geprüft).

**Kosten:** Die zusätzlichen Indizes verteuern das Schreiben leicht – Veröffentlichen
~270 → ~360 µs/Datei, Übernahme fremder Einträge unverändert ~11 µs. Lesen während
Schreiblast bleibt bei p99 ≈ 70 µs.

**Maßstab präzisiert:** Blättern/Scrollen < 16 ms, Suche < 100 ms (läuft asynchron und
blockiert die Oberfläche nicht).

## Review 3 – Rechte & Audit (2026-11-05)

| Hot Path | Erste Version | Nachher | Maßnahme |
|---|---|---|---|
| Prüfung lokaler Aufrufer | 0 µs + Vektor-Allokation je Anfrage | **≈ 0 µs** | geschützte Pfade nur für fremde Geräte berechnen |
| Gerät mit `catalog.read` (Ledger-Abgleich) | 1,7 µs | 1,7 µs | unkritisch, erlaubtes Lesen wird nicht protokolliert |
| Gerät mit Ordnerrecht (Pfad kanonisieren + Audit) | 36 µs | **6,4 µs** | gleiche Audit-Einträge innerhalb von 60 s nur einmal schreiben |

Das Entdoppeln ist auch eine Sicherheitsmaßnahme: Ohne es könnte ein Gerät mit vielen
abgelehnten Anfragen das Audit-Log und damit die Platte fluten. Das Log ist zusätzlich auf
10 000 Einträge begrenzt.

## Review 4 – Ordner-Freigaben (2026-11-05)

Messung: 10 000 Dateien à 4 KiB in 100 Ordnern.

| Messung | Erste Version | Nachher | Maßnahme |
|---|---|---|---|
| Erstscan (hashen + importieren) | 2,4 s (doppelt importiert) | **1,7 s** (173 µs/Datei) | ein Scan pro Freigabe gleichzeitig |
| Rescan ohne Änderungen (Daemon-Start) | 45–78 ms | 45–78 ms (5–8 µs/Datei) | nur `stat`, kein Hashen bei gleicher Größe + mtime |
| Änderung → Katalog | – | **2,02 s** | 2 s Bündelung + 25 ms Arbeit, nur betroffener Unterordner |
| Watcher nach dem Scan | scannte erneut (eigene Lesezugriffe) | **ruhig** | `Access`-Ereignisse werden ignoriert |
| Nur mtime geändert, Inhalt gleich | neue Ledger-Version → Abgleichverkehr | **nur mtime aktualisiert** | `ScanChange::Touch` |

**Gefundene Fehler:** (1) Der Watcher reagierte auf Lesezugriffe – der Import löste so
selbst neue Scans aus. (2) Zwei gleichzeitige Scans derselben Freigabe importierten alles
doppelt und verdoppelten die Ledger-Versionen (und damit den Verkehr zu allen Geräten).

**Weitere Eigenschaften:** Import per Referenz (keine Kopie), parallel auf allen Kernen,
Datenbank-Übernahme in Stapeln à 500 in einer Transaktion.

**Offen:** Pro Datei werden Temp-Tag und Tag einzeln gesetzt (~170 µs/Datei bei kleinen
Dateien) – bei sehr vielen kleinen Dateien ließe sich das bündeln (Batch-API von iroh-blobs).

## Review 5 – Live-Verbindungen (2026-11-05)

| Messung | Wert | Anmerkung |
|---|---|---|
| Änderung auf A → in B's Katalog | **~330 ms** | 300 ms Bündelung (fasst Scan-Stapel zusammen) + ~30 ms Abgleich |
| Gerät fährt herunter → „offline“ beim anderen | **sofort** (µs) | sauberes QUIC-Schließen; bei Abbruch ohne Schließen spätestens 60 s |
| Gerät wieder da → wartender Download fertig | **0,1–0,4 s** | Live-Verbindung weckt die Warteschlange sofort (vorher bis zum Backoff, bis 1 h) |
| Ruhelast pro gekoppeltem Gerät | 1 Frame / 20 s | Lebenszeichen; Hinweise nur bei Änderungen |
| Verbindungen pro Gerätepaar | genau 1 | Duplikate deterministisch aufgelöst |

**Gefundener Fehler:** Nach Entzug von `catalog.read` baute die *eigene* Seite die Live-Verbindung
wieder auf und hätte weiter Ledger-Hinweise an das Gerät ohne Recht geschickt. Jetzt gilt die
Rechteprüfung auch für ausgehende Verbindungen.

**Offen:** Die Bündelung (300 ms) könnte für Einzeländerungen kürzer sein (adaptiv); der Abgleich
selbst öffnet noch eine eigene Ledger-Verbindung statt die Live-Verbindung mitzunutzen.

## Review 6 – Smart-Pinning (2026-11-05)

Messung: Freigabe mit 2 000 Dateien (~300 B–1 KB) in 20 Ordnern, Handy pinnt die ganze Freigabe.

| Messung | Erste Version | Nachher | Maßnahme |
|---|---|---|---|
| Erste Spiegelung | 4,9 s (2,5 ms/Datei) | **1,3 s** (0,66 ms/Datei) | Verbindung je Gerät wiederverwenden, 8 Aufträge parallel |
| Einzeländerung auf dem PC → Kopie auf dem Handy | 3,3 s | **2,65 s** | Pin-Bündelung 1 s → 300 ms (2 s davon: Watcher-Bündelung beim PC) |

**Warum das über das Internet noch wichtiger ist:** Vorher kostete jede Datei einen eigenen
QUIC-Verbindungsaufbau (über Relais/WAN je ein bis mehrere Round-Trips). Jetzt teilen sich alle
Dateien eines Geräts eine Verbindung.

**Offen:** Der Pin-Abgleich prüft bei jeder Katalogänderung alle Einträge der Pins (Datenbank +
`stat`); bei sehr großen Pins (100 000+ Dateien) wäre ein inkrementeller Abgleich nur der
geänderten Einträge sinnvoll.

## Review 7 – Lokaler Index & Suche, Prototyp (2026-11-06)

Messung (`local_index_million`, Release): 100 000 echte leere Dateien gescannt, dazu 1 Mio.
synthetische Einträge (10 000 Ordner à 100 Dateien); Suche mit frisch geöffnetem Index.

| Messung | Wert | Vergleich Spacedrive |
|---|---|---|
| **RAM des Prozesses bei 1,1 Mio. Einträgen** | **32 MiB** (Spitze 32 MiB) | 1,26–1,89 GB (Index im RAM) |
| Plattenplatz | 322 MiB (304 B/Eintrag inkl. Trigramm-Index) | – |
| Echter Scan (walkdir + Einfügen) | 5 µs/Datei → 1 Mio. Dateien ≈ 5 s (+ Plattenzugriffe) | – |
| Einfügen + Suchindex, 1 Mio. | 4,7 s | – |
| Suche, gezielt („rechnung-0042“, Nummer) | 0,6–2,6 ms | – |
| Suche, breit („laub“ bei 125 000 Treffern) | 63 ms → **2,3 ms** | – |
| Suche, 2 Zeichen („mu“) | 14 ms | – |
| Ungünstigster Fall: häufiges Wort + Filter ohne Treffer | 84 ms | – |

**Maßnahmen:**

- Pfade werden nicht gespeichert, sondern nur Name und Elternordner. Die Suche rekonstruiert den
  Pfad nur für die angezeigten Treffer.
- Der SQLite-Cache ist auf 8 MiB pro Verbindung begrenzt; insgesamt gibt es 3 Verbindungen.
- Breite Anfragen wurden zunächst komplett sortiert (63 ms). Jetzt werden höchstens 2 000
  Kandidaten gelesen und in Rust gewichtet (Namensanfang vor Treffer in der Mitte, dann kürzere
  Namen).

**Offen:**

- Der ungünstigste Fall (häufiges Wort, dazu ein Endungsfilter ohne Treffer) liest alle Treffer
  des Wortes. Er bleibt unter 100 ms, aber die Oberfläche sollte die Suche entprellt und
  abbrechbar ausführen.
- Inkrementeller Scan und Watcher folgen. Gemessen wird dann die Zeit bis eine Änderung
  auffindbar ist, und der Leerlauf bei 1 Mio. Dateien.
