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

## Review 7 – Lokaler Index & Suche (2026-11-06)

Messung (`local_index_million`, Release): 100 000 echte leere Dateien in 1 000 Ordnern, dazu
1 Mio. synthetische Einträge (10 000 Ordner à 100 Dateien). Gesucht wird mit frisch geöffnetem Index.

| Messung | Wert | Vergleich Spacedrive |
|---|---|---|
| **RAM des Prozesses bei 1,1 Mio. Einträgen** | **33 MiB** (Spitze 33 MiB) | 1,26–1,89 GB (Index im RAM) |
| Plattenplatz | 335 MiB (316 B/Eintrag inkl. Trigramm-Index) | – |
| Erstindexierung (walkdir + Einfügen) | 5–6 µs/Datei → 1 Mio. Dateien ≈ 6 s (+ Plattenzugriffe) | – |
| Suche, gezielt („rechnung-0042“, Nummer) | 0,6–2,6 ms | – |
| Suche, breit („laub“ bei 125 000 Treffern) | 63 ms → **1,8 ms** | – |
| Suche, 2 Zeichen („mu“) | 8 ms | – |
| Ungünstigster Fall: häufiges Wort + Filter ohne Treffer | 65–84 ms | – |
| Periodischer Abgleich ohne Änderungen | 55 → **6 µs/Ordner** (100 000 Ordner ≈ 0,6 s) | – |
| Abgleich nach einem Watcher-Ereignis (Ordner mit 100 Dateien) | 0,15 ms | – |
| Neue/gelöschte Datei → auffindbar (Daemon, Watcher) | **~1,0 s** (Bündelung 1 s) | – |
| Explorer: Ordner mit 10 000 Dateien, erste Seite | 17,9 → **5 ms** | – |

**Maßnahmen:**

- Pfade werden nicht gespeichert, sondern nur Name und Elternordner. Die Suche rekonstruiert den
  Pfad nur für die angezeigten Treffer.
- Der SQLite-Cache ist auf 8 MiB pro Verbindung begrenzt; insgesamt gibt es 3 Verbindungen.
- **Breite Anfragen** wurden zunächst komplett sortiert (63 ms). Jetzt werden höchstens 2 000
  Kandidaten gelesen und in Rust gewichtet: Namensanfang vor Treffer in der Mitte, dann kürzere Namen.
- **Abgleich:** Für Ordner mit unveränderter mtime (ns-genau) genügt ein `stat`. Zunächst wurden
  dabei alle Kinder gelesen, um die Unterordner zu finden. Ein Teilindex nur über Ordner
  (`WHERE kind = 1`, mit Stempel) macht den Abgleich 9× schneller.
- **Explorer:** Zum Sortieren reichen Name und Typ aus `readdir`. `stat` (Größe, Datum) läuft nur
  für die angeforderte Seite (3,5× schneller).
- **Watcher** je Ordner, nicht rekursiv: Ausgeschlossene Bäume wie `node_modules` belegen keine
  inotify-Plätze. Die Obergrenze liegt bei 50 000. Den Rest erledigt der periodische Abgleich.

**Gefundener Fehler (Fuzzing):** Eine Suche nach `\0` lieferte alle Dateien, weil SQLite den
Suchbegriff am Nullbyte abschnitt und nur `%` übrig blieb. Behoben: Ein Nullbyte im Suchbegriff
bedeutet keinen Treffer.

**Offen:**

- Der ungünstigste Fall (häufiges Wort, dazu ein Endungsfilter ohne Treffer) liest alle Treffer
  des Wortes. Er bleibt unter 100 ms; die Oberfläche sollte die Suche entprellt ausführen.
- Jede inotify-Beobachtung belegt Kernel-Speicher (etwa 1 KiB je Ordner, bei 50 000 Ordnern also
  bis zu ~50 MiB). Das ist Kernel- und kein Prozessspeicher, aber spürbar. Alternative für später:
  fanotify (braucht Rechte).
- Gelöschte Ordner geben ihren Platz im Watcher-Kontingent nicht frei. Bei viel Wechsel greift
  früher der periodische Abgleich.
- Sortieren nach Größe oder Datum im Explorer braucht `stat` für alle Einträge (bei Bedarf
  parallelisieren).

## Review 8 – Dateioperationen & Fernoperationen (2026-11-06)

Messung (`file_ops`, Release): Ordner mit 2 000 kleinen Dateien in 20 Unterordnern.

| Messung | Wert | Anmerkung |
|---|---|---|
| Verschieben eines Ordners (gleiches Laufwerk) | 3–5 µs/Datei (6–8 ms gesamt) | ein `rename` + Zählung für den Fortschritt |
| Kopieren | 35–415 µs/Datei | bestimmt vom Anlegen neuer Dateien auf der Platte (auf dieser VM stark schwankend) |
| In den Papierkorb, 1 000 Dateien | 176–417 µs/Datei | ebenso (eine `.trashinfo` je Eintrag) |
| Papierkorb auflisten, 1 000 Einträge | 8–13 ms | |
| Rückgängig: 1 000 Einträge wiederherstellen | 419 → **20 µs/Eintrag** | Schnellweg für den Home-Papierkorb |
| Fernoperation (Umbenennen) inkl. neuer QUIC-Verbindung | 3,0–3,3 ms (loopback) | |

**Maßnahmen:**

- **Wiederherstellen** las für jeden Eintrag `/proc/self/mounts`, um den zuständigen Papierkorb zu
  finden. Jetzt wird zuerst der Home-Papierkorb geprüft (der häufigste Fall).
- **Kein `fsync` je `.trashinfo`.** Die Spezifikation verlangt es nicht. Nach einem Absturz
  bleibt im ungünstigsten Fall eine verwaiste `.trashinfo`, die übersprungen wird.
- **Große Dateien** (ab 64 MiB) werden blockweise kopiert, mit Fortschritt und Abbruch
  innerhalb der Datei. Kleine Dateien gehen über `std::fs::copy` (`copy_file_range`, auf
  btrfs/XFS auch Reflink).
- Die Suche wird nach jeder Operation sofort für die betroffenen Ordner aktualisiert
  (0,15 ms je Ordner, siehe Review 7), statt auf den Watcher (1 s) zu warten.

**Offen:**

- Jede Fernoperation baut eine eigene QUIC-Verbindung auf. Über das Internet kostet das 1–2
  Round-Trips mehr. Die Verbindung könnte wiederverwendet werden, wie bei Downloads.
- Der Massenschutz zählt die Dateien betroffener Ordner per Verzeichnisdurchlauf. Bei sehr
  großen Ordnern kostet das Zeit, bevor die Operation beginnt; der Index könnte das schneller
  liefern.

## Review 9 – Vorschaubilder & Metadaten (2026-11-06)

Messung (`media_thumbnails`, Release): 12-MP-JPEG (4000 × 3000, 2,2 MB), Vorschaubild 256 px.

| Messung | Vorher | Nachher | Maßnahme |
|---|---|---|---|
| Vorschaubild neu erzeugen | 115 ms | **69 ms** | JPEG direkt verkleinert decodieren (DCT-Skalierung 1/8 mit `jpeg-decoder`) |
| … zwei gleichzeitig | 60 ms/Bild | **31 ms/Bild** | |
| RAM-Spitze beim Erzeugen | +38 MiB | **+6 MiB** | Es entsteht nie das volle 12-MP-Bild im Speicher |
| Vorschaubild aus dem Cache | – | **0,012–0,019 ms** | Schlüssel aus Pfad, Größe und mtime; nur `stat` und Kopf lesen |
| Metadaten (EXIF + Abmessungen) | – | 3 ms | |

Den Rest der 69 ms kostet das Entpacken der komprimierten Bilddaten; das lässt sich bei JPEG
nicht überspringen. Bei einer Galerie mit 1 000 neuen Fotos sind das etwa 35 s im Hintergrund
(2 parallel). Danach kommt jedes Vorschaubild sofort aus dem Cache.

**Gefundene Fehler (Fuzzing):**

- Eine 43-Byte-GIF-Datei mit 16 641 × 11 780 Pixeln reservierte ~780 MB, obwohl eine
  Speichergrenze gesetzt war. Jetzt prüft Snap Abmessungen und Speicherbedarf selbst, bevor
  decodiert wird.
- `lofty` (AAC) lief bei einer 245-Byte-Datei endlos. Für AAC wird die Dauer jetzt nicht
  berechnet; ein Regressionstest ist eingecheckt. Upstream ist der Fehler noch nicht gemeldet.

**Offen:**

- Vorschaubilder bereits beim Indexieren im Hintergrund erzeugen (erst ab einer Galerie in der
  Oberfläche sinnvoll; Akku auf Mobile beachten).
- Mehr parallele Decodierungen wären möglich, solange der speichersparende JPEG-Weg greift.
