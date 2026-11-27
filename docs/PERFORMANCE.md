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

## Review 10 – Streaming (2026-11-06)

Messung (`streaming`, Release, zwei Daemons über Loopback-QUIC): 64-MiB-Videodatei auf dem
PC, abgespielt vom Handy über den lokalen HTTP-Server.

| Messung | Wert |
|---|---|
| Spulen in die Mitte → erstes Byte (nicht im Cache) | **6–7 ms** |
| Abspielen am Stück, nicht im Cache | 101 → **111 MB/s** (≈ 900 Mbit/s; 4K-Video braucht ~6 MB/s) |
| Abspielen am Stück, aus dem Streaming-Cache | **322–327 MB/s**, erstes Byte < 1 ms |
| Übertragung beim Spulen | nur der angefragte Bereich (im Test < 50 % der Datei) |

**Maßnahme:** adaptives Vorausladen. Das erste Fenster ist 1 MiB groß, damit Spulen schnell
startet; bei durchgehendem Abspielen verdoppelt es sich bis 16 MiB. Auf Loopback bringt das
wenig, weil dort die Übertragung selbst begrenzt (QUIC, bao-Prüfung, Schreiben in den Store).
Über das Internet spart es je 16 MiB fünfzehn Umläufe.

**Offen:**

- Die Cache-Grenze wird alle 10 min durchgesetzt; ein sehr großer Film kann sie bis dahin
  kurz überschreiten.
- Die Größe zu bestimmen kostet beim ersten Abruf einen Umlauf. Bei Dateien aus dem Katalog
  wäre sie bekannt, muss aber trotzdem verifiziert werden.

## Review 11 – Hochladen & Kamera-Upload (2026-11-06)

Messung (`upload`, Release, zwei Daemons über Loopback-QUIC).

| Messung | Vorher | Nachher |
|---|---|---|
| 64-MiB-Video hochladen | – | **0,3 s** (≈ 200 MB/s) |
| 100 Fotos à 3 MB | > 5 min | **1,7 s** (17 ms/Foto, 172 MB/s) |
| Neues Kamerafoto → auf dem PC | – | ~3,5 s (davon 3 s gewollte Wartezeit, bis die Kamera fertig geschrieben hat) |

**Gefundener Fehler:** Die Warteschlange schlief nach jedem Stapel von 8 Aufträgen bis zu 30 s,
obwohl noch Aufträge fällig waren. Jetzt holt sie sofort weitere und lädt bis zu 4 Dateien
parallel hoch.

**Offen:**

- Das Ziel kopiert die Datei aus dem Blob-Store an den Zielort, der Inhalt liegt also kurz
  doppelt auf der Platte. Ein Export per Referenz würde das vermeiden.
- Vor jedem Versuch wird die Datei neu gehasht (bei großen Videos merklich). Hash, Größe und
  mtime ließen sich zwischenspeichern.

## Review 12 – Freigabelinks (2026-11-06)

Messung (`share_links`, Release; Speicher durch einen lokalen S3-Ersatz ersetzt, damit nur Snap
gemessen wird).

| Messung | Wert |
|---|---|
| Schlüssel mit Passwort ableiten (PBKDF2, 600 000 Runden) | 176 ms (im Browser je nach Gerät ~0,2–1 s) |
| 64 MiB verschlüsseln (AES-256-GCM, ring) | 71 ms (≈ 950 MB/s) |
| Link für 64 MiB erstellen (lesen, verschlüsseln, hochladen) | 106 ms (≈ 630 MB/s) |

Die Verschlüsselung läuft gestreamt: Gelesen, verschlüsselt und hochgeladen wird in 64-KiB-Stücken
mit höchstens 8 Stücken im Puffer. Dadurch braucht Snap unabhängig von der Dateigröße nur
etwa 0,5 MiB zusätzlichen Speicher. In der Praxis begrenzt der Upload ins Internet.

**Gefundener Fehler (Worker-Test):** Bei einer Download-Grenze wurden nach dem Start des letzten
erlaubten Downloads dessen weitere Stücke abgelehnt; der Download wäre nie fertig geworden.
Jetzt werden nur neue Downloads abgewiesen.

**Offen:**

- Dateien über 5 GB brauchen mehrteiliges Hochladen (S3 Multipart).
- Browser ohne direktes Schreiben auf die Platte (Firefox, Safari) halten große Dateien im
  Speicher. Ein Service Worker könnte das streamen.

## Review 13 – Speicheranalyse & Duplikate (2026-11-06)

Messung (`storage_analysis`, Release).

| Messung | Vorher | Nachher | Maßnahme |
|---|---|---|---|
| Speicheranalyse über 1,1 Mio. Einträge | 1,85 s | **0,57 s** | Rekursion nur über Ordner (Teilindex), Dateien je Ordner direkt |
| Speicheranalyse eines Ordners mit 10 000 Dateien | 109 ms | **43 ms** | ebenso |
| Duplikate: 1 000 Dateien à 256 KB, erster Lauf | – | 0,38 s (850 MB/s gelesen) | Größe → Anfang (64 KiB) → BLAKE3; nur Kandidaten werden gelesen |
| Duplikate: zweiter Lauf | – | 0,12 s, 0 Bytes gelesen | Hash-Cache im Index (gültig, solange Größe und mtime passen) |

Die Speicheranalyse ist keine Abfrage pro Tastendruck. Die Oberfläche sollte sie trotzdem im
Hintergrund mit Ladeanzeige ausführen, denn bei 1 Mio. Dateien liegt sie über der 16-ms-Grenze.

**Offen:** Ordnergrößen könnten beim Indexieren fortlaufend mitgeführt werden; dann wäre die
Analyse sofort da, kostet aber Schreibarbeit bei jeder Änderung.

## Review 14 – Sicherungen (2026-11-06)

Messung (`backup_engine`, Release, Ziel: Ordner auf derselben Platte): 210 MB in vier großen
Dateien und 5 000 kleinen.

| Messung | Vorher | Nachher | Maßnahme |
|---|---|---|---|
| Erster Lauf | 6,4 s (33 MB/s) | **3,8 s (55 MB/s)** | Kein `fsync` je Objekt; ein `syncfs` vor dem Katalog |
| Zweiter Lauf ohne Änderung | – | **26 ms** | Unveränderte Dateien (Größe + mtime) werden nicht gelesen |
| 1 MB in einem 50-MB-Video geändert | – | 165 ms, **4,8 MB** neu gespeichert | Stücke nach Inhalt (FastCDC) |
| Alles wiederherstellen | – | 0,8 s (256 MB/s) | |
| RAM-Spitze | – | 53 MiB (Test inkl. Daten) | Stücke höchstens 4 MiB, gestreamt |

**Offen:**

- Kleine Dateien erzeugen je ein eigenes Objekt (5 000 Objekte für 5 000 Notizen). Pakete mit
  vielen Stücken würden den ersten Lauf und vor allem R2 (Anzahl der Schreibzugriffe)
  beschleunigen.
- Verschlüsselt und hochgeladen wird nacheinander. Parallel ginge es bei langsamen Zielen (R2)
  deutlich schneller.

## Review 15 – Sicherungen: Zeitplan, Aufbewahrung, Ausdünnen (6b-2, 2026-11-06)

Messung `core/tests/perf.rs::backup_engine` (Release):

| Fall | Zeit |
|---|---|
| Erster Lauf 210 MB, 5 004 Dateien | 3,7 s (57 MB/s) |
| Zweiter Lauf ohne Änderung | 24 ms |
| 1 MB in einem Video geändert | 137 ms (4,8 MB neu) |
| Alles wiederherstellen | 594 ms (353 MB/s) |

- **Schutz vor groben Zeitstempeln:** Dateien, deren mtime weniger als 2 s vor dem Beginn des
  vorherigen Laufs liegt, werden erneut gelesen. Im Alltag (stündlich) betrifft das praktisch keine
  Datei; der Lauf ohne Änderung bleibt bei 24 ms.
- **Zeitplaner:** prüft einmal pro Minute nur die Auftragsliste in SQLite (Mikrosekunden); Läufe
  laufen im Hintergrund, nie im Pfad einer GUI-Abfrage.
- **Ausdünnen:** liest den Katalog und die Bäume der verbleibenden Stände (Dateidaten werden nicht
  gelesen) und löscht nur, was der lokale Bekannt-Zwischenspeicher als vorhanden führt.

## Review 16 – Foto-Zeitachse (5d, 2026-11-06)

Messung `photo_timeline` (Release): 1 Mio. Index-Einträge, davon 375 000 Bilder/Videos.

| Messung | Vorher | Nachher | Maßnahme |
|---|---|---|---|
| Erste Seite (200) | – | 1,1 ms | Index `(hidden, taken, node_id)`, Cursor statt OFFSET |
| Seite 502 | – | 0,6 ms | ebenso (Kosten unabhängig von der Tiefe) |
| Erste Seite eines Ordners (3 750 Fotos) | 142 ms | **3,3 ms** | `find_dir` ohne Zählung (s. u.), Ordnerindex mit `INDEXED BY` für kleine Bereiche |
| Monate eines Ordners | 424 ms | **1,0 ms** | ebenso |
| Monate gesamt (375 000) | 21 ms | **0 ms** (Zwischenspeicher) | vom Aktualisierer im Hintergrund vorberechnet; ohne Zwischenspeicher 20 ms |
| Erstes Füllen | – | 16 µs je Datei (ohne Lesen) | EXIF wird nur für sichtbare Dateien gelesen |
| Prüfung ohne Änderung | 370 ms | **211 ms** (Hintergrund) | Teilindex auf Bild-/Video-Endungen; Waisen nur alle 10 min |

- **Nebenbefund:** `LocalIndex::find_dir` rief `roots()` auf, das jeden Ort komplett zählt
  (`COUNT(*)` über 1 Mio. Einträge, ~30 ms). Jetzt nur noch die Pfade der Orte. Davon profitieren
  auch Speicheranalyse und Duplikatsuche eines Ordners: 43 ms → **8 ms** (10 000 Dateien).
- Große Bereiche (über 1 000 Unterordner) nutzen den Zeitindex mit Ordnerbedingung; dort sind die
  Treffer dicht, die Abfrage endet nach einer Seite.
- **Offen:** Die Prüfung im Hintergrund (211 ms bei 1 Mio. Einträgen) läuft nach jeder
  Indexänderung (gebündelt, 5 s). Eine Warteschlange geänderter Knoten aus dem Indexer würde sie
  auf die Änderungen beschränken.

## Review 17 – Desktop-App, Brücke zum Dienst (3a, 2026-11-06)

Messung `apps/desktop/src-tauri` → `bridge::tests::perf_bridge_roundtrip` (Release-App gegen
Release-Daemon, Ordner mit 2 000 Dateien):

| Messung | Ergebnis |
|---|---|
| Ordnerseite mit 1 000 Einträgen (Daemon liest live, Socket, JSON, Umwandlung) | **3,1 ms** |
| Gleiche Messung gegen Debug-Daemon (nur zum Vergleich) | 10,5 ms |
| Oberfläche (JS + CSS, gzip) | 82 KB + 3 KB |

- Große Ordner werden seitenweise geladen (je 1 000). Die erste Seite erscheint sofort, der Rest
  wird angehängt. Ein Generationszähler verwirft Seiten, wenn inzwischen woanders hin navigiert
  wurde.
- Vorschaubilder lädt die Web-Ansicht träge (`loading="lazy"`) über das `thumb:`-Protokoll. Der
  Daemon liefert sie aus seinem Cache (Review 9). Die Antwort trägt `Cache-Control`, damit die
  Web-Ansicht sie nicht erneut anfragt.
- **Offen:** Jede Ordnerseite liest den Ordner im Daemon neu ein. Bei sehr großen Ordnern
  (> 10 000 Einträge) wäre ein Cursor im Daemon günstiger. Eine virtualisierte Liste in der
  Oberfläche folgt, sobald Ordner dieser Größe im Alltag auffallen.

## Review 18 – Mobiler Kern über FFI (4a, 2026-11-07)

Messung `apps/mobile/core-ffi` → `tests::perf_ffi_list_dir` (Release):

| Messung | Ergebnis |
|---|---|
| Ordnerseite mit 1 000 Einträgen (JSON-Anfrage → Kern → JSON-Antwort) | **1,8 ms** |
| Kleine Anfrage (`Settings`) | **6 µs** |

- Auf dem Telefon läuft der Kern im App-Prozess. Es gibt keinen Socket-Umweg; JSON ist die
  einzige Umwandlung.
- Telefone sind langsamer als die Messmaschine (grob Faktor 3–5). Eine Ordnerseite bleibt auch dort
  deutlich unter 16 ms.

## Review 19 – Tippfehler-tolerante Suche, native Vorschaubilder (4c, 2026-11-07)

Messung `core/tests/perf.rs` (Release, Index mit 1 Mio. Einträgen, kalter Cache):

| Anfrage | Ergebnis |
|---|---|
| Tippfehler „rechnnug“, „projket“ (nur ähnliche Treffer) | **5–6 ms** |
| „vertrrag-0001“, „urlaub-00005“ (lange Anfragen, bis 3 Fehler) | **5–6 ms** |
| „vertrag“ mit Filter „nur PDF“ ohne Treffer | **87 ms** |

- Erst läuft die exakte Trigramm-Suche. Nur wenn sie weniger als 5 Treffer liefert und die Anfrage
  mindestens 6 Zeichen hat, sucht der Kern ähnliche Namen. Nach dem Schubfachprinzip muss ein
  Teilstück der Anfrage wörtlich vorkommen; diese Kandidaten liefert der FTS-Index (höchstens 20 000
  Indexzeilen, 4 000 Kandidaten), danach entscheidet der Damerau-Levenshtein-Abstand.
- Die 87 ms im letzten Fall stammen aus der **exakten** Suche, wenn ein Filter alles ausschließt.
  Das bestand schon vorher (bisher nicht gemessen) und liegt unter dem Suchbudget von 100 ms. Die
  App sucht beim Tippen mit 160 ms Pause und verwirft veraltete Antworten, die Oberfläche blockiert
  nie. ~~Offen: Filter in die FTS-Abfrage ziehen~~ – erledigt in Review 30 (87 → 24 ms).
- Vorschaubilder auf Android erzeugt jetzt das System (`ThumbnailUtils`, nutzt eingebettete
  EXIF-Vorschauen und Hardware-Dekoder) statt der Kern. Höchstens 4 gleichzeitig, Ergebnis als JPEG
  im App-Cache (begrenzt auf 200 MB). Ein zweiter Aufruf liest nur noch die Cache-Datei; die App
  hält zusätzlich die URIs im Speicher und lädt die ersten 40 Bilder der Zeitachse vorab.
- Animationen laufen über Reanimated auf dem UI-Thread, also unabhängig von JS-Last.

## Review 20 – Archive entpacken (4e, 2026-11-07)

Messung `core/tests/perf.rs` → `archive_extract` (Release):

| Messung | Ergebnis |
|---|---|
| ZIP mit 5 000 Dateien à 2 KB (50 Ordner) | **81 ms** (16 µs je Datei) |
| ZIP mit einer Datei, 200 MB (schwach komprimierbar) | **1,0 s** (≈ 206 MB/s) |

- Entpacken läuft als Auftrag im Hintergrund (`spawn_blocking`) mit Fortschritt und Abbruch;
  die Oberfläche wartet nie darauf. Prüfungen (Pfade, Grenzen) kosten neben dem Schreiben kaum
  etwas.
- Auf Telefonen rechnen wir grob mit einem Drittel bis einem Fünftel – also etwa 40–70 MB/s.
- Geschrieben wird in 64-KiB-Blöcken; der Speicherbedarf bleibt unabhängig von der Archivgröße.

## Review 21 – Android: Ruckler und träge Menüs (4h, 2026-11-07)

Anlass: Der Nutzer meldete starke Bildeinbrüche und träge Menüs. Neue Messung im Emulator-CI
(`apps/mobile/app/e2e/perf.sh`: Ordner mit 460 Dateien, Scrollen, Halten-und-Wischen, Menü;
`dumpsys gfxinfo`).

| Bildzeit | vorher | nachher |
|---|---|---|
| 50. Perzentil | 26 ms | 23 ms |
| 90. / 99. Perzentil | 32 / 48 ms | 32 / 48 ms |
| langsame UI-Thread-Bilder | 12 | 12 |

- **Einordnung:** Der CI-Emulator rendert ohne Grafikkarte (SwiftShader); seine Bildzeiten sind
  vom Zeichnen bestimmt und zeigen JS-Verbesserungen kaum. Auf dem Telefon des Nutzers ist es nach
  dessen Rückmeldung „deutlich flüssiger“. Die Messung bleibt als Schutz vor Rückschritten.
- Ursachen und Abhilfe: Bei jedem Renderlauf entstanden neue Themen- und Stilobjekte
  (`StyleSheet.create` in jeder Zeile) → jetzt einmal je Thema. Beim Markieren wurde die ganze
  Liste neu gezeichnet → memoisierte Zeilen. Jede Zeile war für eine Layout-Animation angemeldet,
  jedes Vorschaubild hatte eine eigene Animation → entfernt bzw. native Einblendung. Datum und
  Größe liefen bei jedem Zeichnen durch `toLocale…` (in Hermes teuer) → zwischengespeicherte
  `Intl`-Formate. Scroll-Ereignisse 60-mal pro Sekunde in JS → nur am Ende einer Bewegung.
- Suchfeld: ein einziges Eingabefeld für Startseite und Suche – die Tastatur bleibt beim ersten
  Buchstaben offen (vorher schloss sie kurz).
- **Offen:** Messung auf echter Hardware  wäre aussagekräftiger;
  bis dahin zählt die Rückmeldung vom Gerät.

## Review 22 – Eingebaute Anzeigen: PDF, Text/Markdown, DOCX (2026-11-07)

| Messung | Ergebnis |
|---|---|
| DOCX lesen: 646 KB XML, 3 250 Blöcke (Überschriften, Absätze, 50 Tabellen) | **5,5 ms** (Release) |

- DOCX liest der Kern in `spawn_blocking`; die App zeigt die Blöcke in einer virtualisierten
  Liste. Auch auf dem Telefon (Faktor 3–5) bleibt ein langes Dokument deutlich unter 50 ms.
- PDF: Android rendert Seiten bei Bedarf (nur sichtbare ± 2), als PNG im Cache (150 MB); eine
  Seite in Bildschirmbreite braucht auf Mittelklasse-Telefonen typischerweise 20–60 ms und läuft
  außerhalb des UI-Threads.
- Text/Markdown: höchstens 2 MB werden gelesen; lange Texte in Abschnitten zu 60 Zeilen
  (virtualisierte Liste), Markdown auf höchstens 5 000 Blöcke begrenzt.

## Review 23 – Speichermeldung der Geräte (4f, 2026-11-07)

- Der eigene Speicher wird beim Verbindungsaufbau und danach alle 5 Minuten bestimmt (`statvfs`
  je Laufwerk der Freigaben, Mikrosekunden) und nur bei spürbarer Änderung (> 0,5 % bzw. > 100 MB)
  gemeldet – eine Nachricht von < 100 Byte. Kein messbarer Einfluss auf Akku oder Netz.
- `pair list` liest den zuletzt gemeldeten Wert aus dem Speicher (keine Netzabfrage) – die
  Startseite blockiert nicht.

## Review 24 – Bildbearbeitung (Android, 2026-11-07)

- Vorschau: Android dekodiert das Bild verkleinert (höchstens ~1080–2048 px Kante, 4 MP), wendet
  Drehung und Farbmatrix auf der Grafik-Pipeline an und legt das Ergebnis als JPEG im Cache ab
  (80 MB begrenzt). Regler sind entprellt (90 ms), veraltete Vorschauen werden verworfen; alles
  läuft außerhalb des UI-Threads, die Oberfläche bleibt bedienbar.
- Speichern in voller Größe (höchstens 24 MP, sonst verkleinert), ebenfalls im Hintergrund.
- **Offen:** Messung auf echter Hardware (Vorschau-Zeit je Reglerschritt) nachholen.

## Review 25 – Videobearbeitung (Android, 2026-11-07)

- Zeitleiste: 8 Einzelbilder (160 px) über `MediaMetadataRetriever` am nächsten Schlüsselbild,
  im Hintergrund erzeugt und im Bearbeitungs-Cache (80 MB, gemeinsam mit der Bildbearbeitung)
  abgelegt; beim zweiten Öffnen sofort da.
- Ziehen der Griffe aktualisiert nur Zahlen und springt die Vorschau an (kein Dekodieren eigener
  Bilder); Export läuft in Media3 (Hardware-Codecs), der UI-Thread fragt nur alle 400 ms den
  Fortschritt ab (eine Zahl über die Brücke).
- Ein Export zur Zeit (Sperre); Abbrechen löscht die halbfertige Datei.
- **Offen:** Exportzeit je Minute Video auf echter Hardware messen.

## Review 26 – Ordneransicht anderer Geräte (2026-11-07)

Neue Abfrage `RemoteDir` (Startseite → Gerät). Statt alle Einträge unter einem Ordner zu lesen und
in Rust zu gruppieren, springt sie im neuen Teilindex `idx_ledger_tree (origin, share, rel_path)
WHERE deleted = 0` von Kind zu Kind: ein Unterordner kostet einen Indexzugriff, egal wie groß er ist.

Messung (`perf.rs::remote_dir_tree`, Release, 105k Katalogeinträge eines Geräts):

| Abfrage | vorher (Obergrenze als `?4 IS NULL OR …`) | jetzt |
|---|---|---|
| Wurzel (Freigaben) | 0,05 ms | 0,05 ms |
| Freigabe: 20 Ordner + 180 Dateien | 0,68 ms | 0,69 ms |
| Unterordner mit 3 Monatsordnern | 3,0 ms | 0,013 ms |
| Blattordner, Seite mit 200 Dateien | 0,95 ms | 0,84 ms |

Lehre: Die Obergrenze muss als echte Bereichsbedingung in der Abfrage stehen; mit `?4 IS NULL OR`
nutzt SQLite sie nicht für den Indexbereich und liest nach dem letzten Kind alle folgenden
Einträge. Alle Seiten liegen weit unter 16 ms.

## Review 27 – Desktop: Links, Speicheranalyse, Schriftart (2026-11-07)

- Speicheranalyse und Duplikatsuche nutzen die bereits gemessenen Abfragen (Review 13: Ordner mit
  10 000 Dateien 8 ms, 1,1 Mio. Einträge 0,57 s). Die Seite rechnet asynchron mit Ladeanzeige,
  Duplikate erst auf Knopfdruck; die Oberfläche blockiert nie.
- Eigene Schrift: einmal beim Start bzw. nach der Wahl als Bytes über `tauri::ipc::Response`
  (kein JSON, kein Base64) und `FontFace`; Inconsolata liegt gebündelt bei (2 × 105 kB).
- Linkliste: eine Abfrage beim Öffnen der Seite; Erstellen lädt verschlüsselt im Dienst hoch, die
  Oberfläche wartet nur auf das Ergebnis.

## Review 28 – Entpacken auf anderen Geräten (2026-11-07)

- Vor dem Entpacken zählt das Zielgerät die Einträge für den Massenschutz: bei ZIP aus dem
  Inhaltsverzeichnis (ein Lesezugriff am Dateiende, µs), bei TAR/TAR.GZ durch Lesen der
  Kopfzeilen (so teuer wie einmal Lesen bzw. Entpacken ohne Schreiben; bricht bei 100 001 ab).
  Läuft in `spawn_blocking`, blockiert weder Netz noch Oberfläche.
- Das Entpacken selbst ist der bestehende Dateiauftrag (Review 20), höchstens einer je Gerät.
- Die App fragt nur einmal (Fernoperation) und wartet auf das Ergebnis; nichts läuft im UI-Thread.

## Review 29 – rusqlite 0.32 → 0.41 (SQLite 3.5x, 2026-11-07)

Ohne Codeänderung; Messung `perf.rs::db_hot_paths` (Release) nach dem Wechsel:

| Messung | vorher | nachher |
|---|---|---|
| `is_shared` | 7–9 µs | 4,0 µs |
| Lesen während Schreiblast p50 / p99 / max | – / 0,06 ms / 0,8 ms | 4,1 µs / 0,02 ms / 1,0 ms |
| `apply_remote_entries` (10k) | – | 11,4 µs je Eintrag |
| `entries_since` (Seite 1000) | – | 1,5 ms |

Keine Verschlechterung; `cargo deny check` und `cargo audit` grün.

## Review 30 – Endungsfilter im Suchindex (2026-11-07)

Der FTS5-Index hat jetzt eine zweite Spalte `ext`. Ein Endungsfilter („nur PDF“) wird in die
Index-Abfrage gezogen (`name : "vertrag" AND ext : ("pdf")`), statt alle Namenstreffer zu lesen und
danach zu filtern. Endungen unter 3 Zeichen (Trigramme) fallen auf die genaue Prüfung zurück, die
ohnehin immer läuft. Bestehende Indizes werden beim ersten Öffnen einmalig neu aufgebaut
(1 Mio. Einträge: einige Sekunden, nur einmal).

Messung (`perf.rs::local_index_million`, Release, 1 Mio. Einträge):

| Anfrage | vorher | nachher |
|---|---|---|
| „vertrag“ + nur PDF, keine Treffer (ungünstigster Fall) | 87 ms | **24 ms** |
| übrige Suchen (exakt, Tippfehler) | 1–9 ms | 1–9 ms (unverändert) |
| 1 Mio. Einträge einfügen + Index | – | 8,2 s |

## Review 31 – Große Ordner: Cursor und Zwischenspeicher (2026-11-07)

`ListDir` kennt jetzt einen Cursor (`after`/`next`): Seiten bleiben stabil, auch wenn sich der
Ordner zwischen zwei Seiten ändert. Gemessen zeigte sich, dass bei sehr großen Ordnern nicht das
Sortieren, sondern `readdir` die Zeit kostet. Daher werden Ordner ab 5 000 (bis 200 000) Einträgen
sortiert zwischengespeichert (höchstens 4), gültig solange die Änderungszeit des Ordners gleich
bleibt; frisch geänderte Ordner (< 3 s, grobe Zeitstempel z. B. auf FAT) werden nicht gespeichert.

Messung (`perf.rs::explorer_paging_large_folder`, Release, 100 000 Dateien):

| Seite | vorher | nachher |
|---|---|---|
| erste Seite (kalt) | 56 ms | 49–73 ms (readdir, schwankt mit dem Datenträger) |
| Seite ab 50 000, per Position | 44 ms | 0,13 ms aus dem Zwischenspeicher |
| Folgeseite per Cursor | – | **0,13 ms** |

Die erste Seite eines riesigen Ordners bleibt eine asynchrone Abfrage im Hintergrund (die
Oberfläche blockiert nicht); erneutes Öffnen und Weiterblättern sind sofort da.

## Review 32 – PDF: Textmarker, Signieren, Prüfen (2026-11-09)

Messung (`perf.rs::pdf_mark_sign_verify`, Release, PDF mit 200 Seiten, 0,8 MB):

| Schritt | Zeit |
|---|---|
| Textmarker übernehmen (20 Striche, 1 000 Punkte) | 1,6 ms |
| Signieren (Update + CMS, ohne Entsperren) | 1,9 ms |
| Signaturen prüfen | 1,3 ms |
| Digitale ID entsperren (Argon2id 64 MiB, t=3) | 104 ms (PC; auf Telefonen bis ≈ 1 s) |

- Alles läuft im Kern in `spawn_blocking`; die Oberfläche zeigt währenddessen einen Hinweis und
  blockiert nicht. Argon2 ist absichtlich teuer (Schutz des Schlüssels gegen Raten).
- Live-Zeichnen: ein SVG-Pfad je Strich, Punkte ausgedünnt (Mindestabstand 0,2 % der Fläche);
  Prüfen beim Öffnen läuft parallel zum Laden der Seiten.

## Review 33 – Gestaltung: SVG-Symbole in Listen (2026-11-25)

- Neu je Listenzeile: ein Ordner- bzw. Dateisymbol (`FolderShape`/`Glyph`) und bei Ordnern ein
  Chevron, beide als `react-native-svg` mit einem bis zwei Pfaden. Die Komponenten sind mit
  `memo` umhüllt; Zeilen haben feste Höhe (64 dp, `getItemLayout`), Neuzeichnen beim Scrollen
  entfällt.
- Entfernt: Farbverlaufs-Ebene hinter allen Seiten und die halbtransparenten Karten – weniger
  Überzeichnung (Overdraw) je Bild.
- Prüfung mit `apps/mobile/app/e2e/perf.sh` (400 Dateien, Wischen, Ziehen-Auswählen, `gfxinfo`)
  auf dem Gerät steht aus; der Kern ist unverändert (keine neuen Abfragen, kein Einfluss auf die
  16-ms-Grenze der Kernaufrufe).

## Review 34 – Snap Drop, Kern (2026-11-26)

Messung (`perf.rs::drop_transfer`, Release, 127.0.0.1):

| Schritt | Zeit |
|---|---|
| Oberflächen-Abfragen `status` + `nearby` + `offers` | 0,6 µs |
| Angebot mit 1 000 Dateien prüfen (Namen, Summen, Hashes) | 0,26 ms |
| Probe: Verbindung + Abgleich mit 512 Kontakten (Höchstzahl) | 1,7 ms |
| Übertragung 256 MiB inkl. BLAKE3 auf beiden Seiten | 314 MB/s |

- Keine Oberflächen-Abfrage berührt Netz oder Festplatte (nur Speicher unter kurzen Sperren).
- Hashen beim Sender läuft in `spawn_blocking`; Empfang schreibt in 256-KiB-Blöcken und hasht
  dabei mit, ohne die Datei ein zweites Mal zu lesen.
- Kontakt-Abgleich: ein BLAKE3-Keyed-Hash je Kontakt und ein Mengen-Nachschlag je Tag.
- Fortschritt höchstens 4× pro Sekunde; mDNS-Dienst läuft nur, solange man sichtbar ist oder sucht.

## Review 35 – Zoomen, PDF-Scrollen, Textauswahl (2026-11-26)

- Zoom, Verschieben, Blättern und Schwung laufen vollständig als Worklets auf dem UI-Thread
  (Reanimated); während einer Geste gibt es keinen Umweg über JavaScript und kein neues Layout.
  Nach dem Loslassen wird nur die Auflösung der sichtbaren Seiten angepasst (Stufen 1/1,5/2/3/4/5).
- PDF: Seiten sind absolut positioniert; gezeichnet werden nur die sichtbaren Seiten ± 1. Die
  sichtbaren Seiten berechnet eine Reaktion auf dem UI-Thread (Binärsuche über die Seitenkanten)
  und meldet nur Änderungen an die Oberfläche.
- Bildanzeige: höchstens drei Bilder gleichzeitig (aktuelles und Nachbarn).
- Textauswahl: höchstens eine Abfrage an Android gleichzeitig, immer mit dem neuesten Fingerpunkt;
  zum Schluss genau eine abschließende Abfrage.
- Messung auf dem Gerät (Bildrate beim Zoomen großer PDFs, `gfxinfo`) steht aus.

## Review 36 – Formate: Tabellen, Präsentationen, Bücher, Archive, Audio, APKs (2026-11-26)

Messung `core/tests/perf.rs::formats_read`, Release-Build:

| Fall | Zeit je Datei |
|---|---|
| XLSX, 5 000 × 12 Zellen (2 MB XML, größte erlaubte Ansicht) | 27,5 ms |
| EPUB, 300 Kapitel → 12 300 Blöcke | 12,1 ms |
| ZIP mit 10 000 Einträgen auflisten (Obergrenze) | 8,5 ms |

- Lesen läuft im Kern in `spawn_blocking`; die Oberfläche wartet asynchron mit Ladeanzeige und
  blockiert nie. Feste Obergrenzen (Zeilen, Spalten, Zellenlänge, Folien, Kapitel, Einträge)
  begrenzen Zeit und Speicher auch bei feindlichen Dateien.
- Tabellen zeichnet die App höchstens 400 Zeilen auf einmal (Rest als Hinweis); CSV wird in einem
  Durchgang ohne reguläre Ausdrücke zerlegt.
- Archivansicht: Ordnerebenen werden aus der flachen Liste im Speicher abgeleitet (eine Schleife
  je Ebene, `FlatList` virtualisiert).
- Audio: ein unsichtbarer Player je sichtbarer Seite; Cover über `MediaMetadataRetriever`
  (≤ 10 MB, verkleinert dekodiert, im Vorschaubild-Cache).
- App-Pakete: Split-APKs werden in 256-KiB-Blöcken direkt aus dem ZIP in die Installer-Sitzung
  kopiert (kein Zwischenentpacken); für die App-Infos wird nur die Haupt-APK kurz in den Cache
  kopiert und danach gelöscht.

## Review 37 – Entkoppeln in beide Richtungen (2026-11-26)

Messung `core/tests/perf.rs::unpair_cleanup` (Release) und `core/tests/unpair.rs`:

| Fall | Zeit |
|---|---|
| Gegenseite erfährt das Entkoppeln (Live-Verbindung, Loopback) | 1,6 ms |
| Gegenseite war offline: übernommen nach dem Wiederkommen | 52 ms nach dem ersten Verbindungsversuch |
| Gerät mit 200 000 Katalogeinträgen aus der Datenbank löschen | 1,3 s |
| `was_unpaired` (beim Abweisen einer Verbindung) | 0,4 ms |
| 10 000 fremde Einträge übernehmen, mit Entkoppelt-Prüfung | 77 ms (vorher 79 ms) |
| 10 000 Einträge eines entkoppelten Geräts verwerfen | 0,5 ms |

- Das Löschen läuft in `spawn_blocking` (über die API und beim Entkoppeln durch die Gegenseite);
  Lesezugriffe der Oberfläche laufen über eigene WAL-Leseverbindungen weiter und warten nicht.
- Die Entkoppelt-Prüfung beim Übernehmen kostet eine indizierte Abfrage je Herkunft und Seite.
- Offline-Kopien und Vorschaubilder räumt die Pin-Schleife danach im Hintergrund auf; die
  Vorschaubilder liegen je Gerät in einem eigenen Ordner und verschwinden mit einem Aufruf.

## Review 38 – Snap Drop im Hintergrund (2026-11-27)

Messung `core/tests/perf.rs::drop_wake` (Release):

| Fall | Zeit |
|---|---|
| Weck-Tags prüfen: 500 Kontakte × 3 Epochen (Rust; Android rechnet dasselbe mit `javax.crypto`) | 0,3 ms |
| Weck-Schlüssel-Datei für 500 Kontakte schreiben (bei jeder Kontaktänderung, 0600, atomar) | 1,2 ms |

- **Akku, Standard (Bluetooth-Wecken):** Snap läuft nicht. Die Suche filtert der Bluetooth-Chip
  (Hardware-Filter, `SCAN_MODE_LOW_POWER`, wo möglich ein Treffer je Gerät statt je Paket).
  Geweckt wird nur bei passendem Tag; der Dienst läuft dann gut 3 Minuten und endet von selbst.
  Drosselung: höchstens alle 20 s, 30-mal je Stunde.
- **Akku, „Auch über WLAN“:** dauerhafter Vordergrunddienst mit Multicast-Sperre und
  Bluetooth-Werbung (`ADVERTISE_MODE_LOW_POWER`) – bewusst als „braucht mehr Akku“ gekennzeichnet.
- **Sender:** Weck-Werbung nur nach Antippen eines Kontakts (90 s) bzw. beim Öffnen des
  Sende-Fensters ohne Tag (30 s), danach Ende.
- Messung auf dem Gerät (Akkuverbrauch über 24 h mit eingeschaltetem Wecken) steht aus.

## Review 39 – Snap Drop von überall (2026-11-27)

Messung `core/tests/perf.rs::drop_remote` (Release, Loopback):

| Fall | Zeit |
|---|---|
| Erster Tausch in der Nähe inkl. Start des Fern-Endpunkts | 6,4 ms |
| Senden an einen Kontakt über seinen Fern-Endpunkt (Verbindung, Probe, Angebot, Annahme, Datei) | 2,4 ms je Sendung |

- Der Fern-Endpunkt startet erst, wenn es Kontakte gibt bzw. beim ersten Tausch, und hält dann eine
  Relais-Verbindung (wie der Geräte-Endpunkt). Auf Android läuft er nur, solange der Kern läuft
  (App offen oder „Auch über WLAN“) – keine zusätzliche Dauerlast.
- Zusätzliche Prüfung je Probe: Abgleich der Kontakt-Tags wie in der Nähe (gemessen in Review 34).

## Review 40 – Snap Drop: Abbrechen, Verlauf, Mitteilungen (2026-11-27)

- **Verlauf:** eine Zeile je empfangener Sendung (höchstens 200, ohne Textinhalt); Einfügen mit
  einem Aufräum-`DELETE` über den Primärschlüssel, Abfrage `ORDER BY id DESC LIMIT 50`. Beim
  Anzeigen wird nur die erste Datei je Eintrag auf Existenz geprüft (≤ 50 `stat`).
- **Abbrechen:** schließt die QUIC-Verbindung (Code 5); kein Polling, beide Seiten erfahren es
  sofort. Halbe Dateien werden wie bei Verbindungsverlust verworfen (Test `abbrechen_und_verlauf`,
  64 MiB, Abbruch nach dem ersten Fortschritt).
- **Desktop-Mitteilungen:** ein Abo auf den Ereignisbus im Dienst; D-Bus-Aufrufe nur bei Angeboten.
  Ohne Sitzungsbus endet die Aufgabe sofort.
- **Android:** ein Ereignis-Empfänger je Prozess (`DropNotify`); Fortschritts-Mitteilungen höchstens
  einmal je Sekunde.

## Review 41 – Snap Drop: Ordner, Gruppen, Fotoauswahl (2026-11-27)

- **Ordner:** Packen (`compress_into`) und Entpacken (`extract` mit Grenzen) laufen in
  `spawn_blocking`; Durchsatz wie in den bestehenden Archiv-Messungen (`archive_extract`). Bereits
  komprimierte Formate (JPEG, MP4 …) werden nur gespeichert, nicht erneut komprimiert. Das
  Arbeits-ZIP liegt im privaten Datenordner und wird nach dem Senden gelöscht (auch bei Fehlern).
- **Gruppen:** Sendungen an mehrere Empfänger laufen gleichzeitig; jede hat ihre eigene Verbindung
  und Kennung, die Dateien werden je Empfänger einmal gelesen (BLAKE3 beim Vorbereiten je Sendung).
- **Fotoauswahl:** nur Zustand in der Oberfläche (Menge der Pfade), kein zusätzliches Laden.

## Review 42 – Zwischenablage zwischen eigenen Geräten (2026-11-27)

- Eine kurze QUIC-Verbindung je Sendung (wie die Fernoperationen), ein Rahmen ≤ 64 KB; die
  Rechteprüfung ist eine indizierte Abfrage (gemessen in Review „auth_checks“).
- Kein Dauerbetrieb: Android liest die Zwischenablage nur beim Öffnen der App; der Desktop prüft
  höchstens alle 3 s und nur, solange das Fenster im Vordergrund ist und ein Gerät gewählt wurde.
- Drosselung je Gerät: eine kleine Liste von Zeitpunkten (≤ 30) im Speicher.
- Fuzz-Ziel `clip_request`: 4 Mio. Läufe in 60 s ohne Befund.

## Review 43 – „Alle Dateien“ (2026-11-27)

- Erster Scan erfasst und hasht den ganzen Speicher einmal (parallel bis zur Kernzahl, wie bei
  jeder Freigabe; Durchsatz siehe Review „share_scan“). Danach nur noch geänderte Dateien
  (Größe/Zeitstempel), Beobachtung per Dateisystem-Ereignissen.
- Die Auslass-Regeln wirken schon beim Durchlaufen (`filter_entry`): ausgelassene Ordner werden
  nicht betreten – `Android/data` mit oft Hunderttausenden Cache-Dateien kostet damit nichts.
- Katalog mit sehr vielen Einträgen: getestet bis 1 Mio. Einträge (Review „local_index_million“).

## Review 44 – Gerät finden (2026-11-27)

- Eine kurze QUIC-Verbindung je Anfrage mit einem Rahmen ≤ 256 Byte; die Rechteprüfung ist dieselbe
  indizierte Abfrage wie bei der Zwischenablage (Review „auth_checks“). Gemessen (Release, Loopback, `core/tests/ring.rs`):
  Rundlauf Anfrage → Ereignis beim klingelnden Gerät 2,3 ms, Stoppen 1,8 ms – jeweils mit neuem
  Verbindungsaufbau.
- Kein Dauerbetrieb und kein Abfragen: Der Ton läuft nur während eines Klingelns (höchstens 30 s,
  danach wird alles freigegeben). Die Oberfläche fragt nichts ab – alle Aufrufe sind asynchron,
  keine GUI-Abfrage blockiert.
- Drosselung je Gerät: höchstens 6 Zeitpunkte im Speicher; abgelaufene Einträge werden bei jeder
  Anfrage entfernt (kein Wachstum durch viele Geräte).

## Review 45 – Zwei-Wege-Sync (2026-11-27)

Gemessen (Release, Loopback, `core/tests/perf.rs` › `pin_mirror`, 2000 Dateien in 20 Ordnern):

| Vorgang | Zeit |
|---|---|
| Änderung am Handy → beim PC angekommen | 4,7 s (davon 3 s bewusstes Abwarten, ob die Datei noch geschrieben wird) |
| Abgleich ohne Änderungen (inkl. Suche nach neuen Dateien) | 29 ms je Durchlauf |

- Der Abgleich läuft in `spawn_blocking`, nie im GUI-Pfad; Oberflächen lesen nur `PinList`.
- Ausgelöst wird er durch Dateisystem-Ereignisse im Pin-Ordner (gebündelt, 300 ms) und höchstens
  alle 5 Minuten zum Nachholen; die Beobachtung läuft nur, solange es Zwei-Wege-Pins gibt.
- Vor dem Senden wird einmal je Ursprung der Katalog aufgefrischt (eine Verbindung, nur neue
  Einträge), dann werden die Änderungen nacheinander gesendet – nie zwei Läufe gleichzeitig.
- Hochrechnung: bei 100 000 Dateien etwa 1,5 s je Abgleich im Hintergrund. Falls das stört, wäre der
  nächste Schritt, nur die vom Dateisystem gemeldeten Pfade zu prüfen.

## Review 46 – RAW, SVG, GIF, Untertitel (2026-11-27)

Gemessen (Release, `core/tests/perf.rs` › `raw_preview`, 25-MB-Datei mit 12-MP-JPEG-Vorschau):

| Vorgang | Zeit |
|---|---|
| Eingebettete Vorschau finden | 9 µs |
| RAW → Vorschaubild 256 px | 48 ms |
| RAW → Anzeige 2560 px | 325 ms |

- Das Finden liest nur die IFD-Einträge (wenige KB), nie die Rohdaten. Decodiert wird nur die
  Vorschau, beim kleinen Vorschaubild mit DCT-Verkleinerung. Alles läuft in `spawn_blocking` mit
  höchstens zwei gleichzeitigen Vorschauen und landet im Vorschau-Cache (danach ohne Decodierung).
- SVG: Bereinigung per regulären Ausdrücken über höchstens 2 MB Text, einmal je Öffnen.
- Untertitel: eine Ordnerabfrage beim Öffnen eines Videos (höchstens 2000 Einträge), keine
  Dauerarbeit. GIF-Animation decodiert Fresco nur, solange das Bild sichtbar ist.

## Review 47 – Texterkennung (OCR) (2026-11-27)

Gemessen mit denselben Commits von Tesseract/Leptonica, für den Linux-Host gebaut (4 Kerne, ein
Thread), Deutsch + Englisch, LSTM (`tessdata_fast`):

| Eingabe | Zeit | Ergebnis |
|---|---|---|
| A4-Seite, 300 dpi (2480 × 3508), 30 Textzeilen | 1,25 s | alle Zeilen korrekt, inkl. ä/ö/ü/ß und € |

- Auf Handys ist mit etwa dem 2- bis 3-Fachen zu rechnen. Die Erkennung läuft nur auf Knopfdruck,
  in `Dispatchers.Default` (nie im UI-Thread), höchstens eine gleichzeitig; die Oberfläche zeigt
  einen Fortschritt und bleibt bedienbar.
- Bilder werden vor der Erkennung auf höchstens 3000 px verkleinert (begrenzt Zeit und Speicher);
  PDF-Seiten werden mit 2400 px Breite gezeichnet.
- Größe: `libsnap_ocr.so` 2,5 MB je Architektur (gestrippt), Sprachdaten 5,4 MB (einmal in der APK).

## Review 49 – Ablaufende Sendungen (2026-11-27)

- Beim Empfang: ein `INSERT` je Datei in `drop_expiry` (eine Transaktion), gemessen im Test
  `core/tests/drop.rs` › `ablaufende_sendung_vorschlag_und_eigene_frist` ohne messbaren Unterschied
  zur Übertragung ohne Frist (< 1 ms).
- Aufräumen alle 5 Minuten in `spawn_blocking`: eine indizierte Abfrage (`idx_drop_expiry`) nach
  höchstens 500 fälligen Pfaden; ohne fällige Einträge kein Plattenzugriff außer der Abfrage.
