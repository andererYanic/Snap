# Android-App: Gestaltung und Bedienung

Entscheidung vom 2026-11-07: **Snap-Optik mit Android-Bedienung**, also dieselbe Designsprache wie
die Desktop-App (weicher Farbverlauf, durchscheinende Karten, eigene Symbole, hell/dunkel nach
System). **Material You** (Farben aus dem Hintergrundbild, Android 12+) lässt sich in den
Einstellungen einschalten.

## Grundsätze (aus UX-Forschung und Plattform-Richtlinien)

1. **Daumenzone:**
   - Das Telefon wird meist einhändig gehalten; das untere Drittel ist mühelos erreichbar, oben ist
     es schwer (Steven Hoober, „Thumb Zone“).
   - Häufige Aktionen gehören nach unten: Navigation, Auswahlaktionen, „+“-Knopf, Rückgängig.
   - Oben steht nur, was man liest oder selten antippt: Ort, Suche, Menü.
2. **Sichtbare Navigation statt Hamburger:**
   - Versteckte Menüs werden schlechter gefunden (Nielsen Norman Group).
   - Deshalb eine untere Leiste mit 4 Zielen: Material empfiehlt 3–5; ab 5 wird es eng.
3. **Tippziele ≥ 48 dp** (Material) bzw. 44 pt (Apple), mit ≥ 8 dp Abstand. Listenzeilen sind
   56–64 dp hoch.
4. **Schrittweise Offenlegung:**
   - Der Alltag (Dateien, Fotos, Geräte) ist sofort da.
   - Seltenes liegt unter „Mehr“: Sicherungen, Papierkorb, Freigabelinks, Speicheranalyse,
     Einstellungen.
5. **Fehler verzeihen statt nachfragen:**
   - Papierkorb und Verschieben lassen sich mit „Rückgängig“ in einer Leiste am unteren Rand
     zurücknehmen (wie am Desktop mit Strg+Z).
   - Nur endgültiges Löschen und sicherheitsrelevante Aktionen bestätigt ein nativer Dialog.
6. **Gesten nur zusätzlich:**
   - Alles ist auch über sichtbare Knöpfe erreichbar; Wischgesten sind schwer zu entdecken.
   - Kein Wischen zum Löschen.
   - Die Zurück-Geste von Android (vorausschauend) navigiert in der Ordnerhierarchie.
7. **Barrierefreiheit:**
   - Systemschriftgröße wird respektiert, Kontrast nach WCAG AA.
   - TalkBack-Beschriftungen für alle Symbolknöpfe.
   - Keine Information nur über Farbe.
8. **Gleiches Verhalten wie am Desktop:** dieselben Begriffe und Übersetzungen (`locales/`), dieselben
   Symbole und Sicherheitsregeln. Wer eine App kennt, kennt die andere.

## Gelernt von Spacedrive (2026-11-07)

Spacedrive ist ein quelloffener Dateimanager. Übernommen wurden Muster, kein Code und keine Grafiken:

- **Startseite statt Dateisystem-Wurzel:** Suchfeld oben, darunter gruppierte Einstiege. Bei Snap
  sind das der Speicher mit Belegungsbalken und die vorhandenen Standardordner (Download, Kamera,
  Bilder …). Der Explorer öffnet sich erst danach.
- **Explorer-Kopfzeile:** Zurück-Pfeil, Ordnername mittig, Ansicht wechseln (Liste/Raster) und „⋯“
  für Seltenes (Sortierung, versteckte Dateien).
- **Flache Zeilen statt Karten:**
  - Symbol 40 dp, Name, Metazeile „Größe • Datum“, „›“ bei Ordnern.
  - Trennlinie ab dem Text, wie in Android-Einstellungen.
  - Ruhiger und dichter als Karten.
- **Großer Titel** auf der Startseite. Im Explorer übernimmt die Kopfzeile.
- **Leerzustände** mit Symbol, Titel und Erklärung, was man hier tun kann.
- **Rückmeldung bei jeder Berührung:** Android-Welle auf allen tippbaren Flächen.

Eigene Ergänzungen:
- Dateien öffnen mit der passenden App (FileProvider, nur Lesezugriff für die gewählte App).
- Teilen über das Android-Menü.
- „+“ öffnet eine Auswahl von unten: neuer Ordner, neue Textdatei.
- Verschieben/Kopieren: Elemente auswählen, zum Ziel navigieren, dann „Hier einfügen“ in der
  Leiste unten.
- Dateisymbole mit farbigem Typ-Etikett (PDF, IMG, VID …).

## Aufbau

```
┌──────────────────────────────┐
│ ◀  Dokumente          🔍  ⋮  │  Kopfzeile: Ort (antippen → Pfad), Suche, Menü
│ Home › Dokumente             │  Brotkrumen, waagrecht scrollbar
├──────────────────────────────┤
│ ╭──────────────────────────╮ │
│ │ 📁 Steuer              › │ │  Karten auf dem Farbverlauf
│ │ 📄 Mietvertrag.pdf 410 KB│ │  Zeilen 56–64 dp, Vorschaubilder
│ ╰──────────────────────────╯ │
│                        ( + ) │  Neu: Ordner, Foto aufnehmen, hochladen
├──────────────────────────────┤
│  📁      🖼       📱      ⋯  │  Untere Leiste (immer sichtbar)
│ Dateien Fotos  Geräte  Mehr  │
└──────────────────────────────┘
```

- **Auswahl:** Lang drücken (190 ms) und, ohne den Finger zu heben, über weitere Einträge wischen –
  alles Überstrichene wird markiert (am Rand scrollt die Liste mit, Haptik bei jeder neuen Markierung).
  - **Genau eine Datei:** Beim Loslassen öffnet sich das Kontextmenü dort, wo der Finger zuerst
    aufgesetzt hat, mit leichter Abdunkelung; Antippen daneben schließt nur das Menü.
  - **Ab zwei Dateien:** Statt des Menüs erscheint unten – an der Stelle der Hauptleiste – eine
    Aktionsleiste (Teilen, Kopieren, Verschieben, Papierkorb, Mehr). Sie bleibt, bis alle Dateien
    abgewählt sind.
- **Dateien:**
  - Start ist der gemeinsame Speicher.
  - Oben eine kleine Ortsauswahl: Telefon, SD-Karte, gekoppelte Geräte.
  - Liste oder Raster umschaltbar. Zuletzt geöffnete Dateien als horizontale Leiste über der Liste.
- **Fotos:** Zeitachse nach Monaten mit Schnellsprung am rechten Rand (Daumen). Antippen öffnet
  die eingebaute Anzeige.
- **Untere Leiste: Dateien, Fotos, Einstellungen.** Die Geräte stehen in den Einstellungen
  (Unterkategorien: Allgemein – Sprache, Material You, Schriftart; Geräte – Status, Speicher,
  Koppeln; Papierkorb; Über Snap). Die Startseite zeigt unter „Interner Speicher“ alle
  gekoppelten Geräte mit ihrem gemeldeten Speicher. Schrift: Inconsolata, eigene TTF/OTF wählbar.
- **Unten „Dateien“ erneut antippen** führt aus jedem Ordner und jeder Suche zurück zur Startansicht.
- **Eingebaute Anzeige:** Bilder und Videos ohne fremde App. Zwei-Finger-Zoom, Doppeltippen zoomt
  auf die getippte Stelle, Wischen links/rechts blättert, nach unten wischen schließt. Erst
  erscheint sofort ein 1024-px-Vorschaubild, dann das Original. Gewischt wird durch alle Dateien des
  Ordners; was die Anzeige nicht darstellt (PDF, Text …), erscheint als Karte mit „Öffnen mit …“. Videos über Media3/ExoPlayer; was
  nicht abspielbar ist, bietet „Mit anderer App öffnen“ an.
- **Bilder bearbeiten:** im Menü am Finger und in der Bildanzeige. Zuschneiden (frei oder 1:1, 4:3,
  3:4, 16:9, Ecken ziehen, Drittel-Raster), Drehen, Helligkeit/Kontrast/Sättigung/Wärme, Filter. Android
  rechnet (Hardware), die Vorschau folgt den Reglern nach kurzer Pause. Speichern fragt: Kopie
  (Original bleibt) oder Original ersetzen (Original in den Papierkorb, „Rückgängig“). Aufnahmedatum,
  Kamera und Ort bleiben erhalten.
- **Suche:** Wechsel in die Suchansicht schon beim ersten Buchstaben, Ergebnisse beim Tippen (160 ms Pause), Tippfehler werden toleriert; solche
  Treffer sind mit „ähnlich“ markiert.
- **Bewegung:** Federnde Übergänge statt harter Wechsel (Ordner gleiten seitlich herein, Einträge
  blenden gestaffelt ein, Vorschaubilder blenden auf, Menüs zoomen vom Finger aus auf). Kurze
  Dauern (120–250 ms), damit nichts träge wirkt. **Keine Federn, die nachschwingen.** Werte aus dem
  Designlabor (`apps/mobile/prototype/`, mit dem Nutzer abgestimmt): Kurve `cubic-bezier(0.32, 0.72, 0, 1)`,
  Menüs/„+“/Dialoge 180 ms von 92 % auf 100 %, Ordnerwechsel 140 ms „Gleiten + Blende“ (30 % Versatz),
  keine gestaffelt einblendenden Listen. Alles zentral in `src/motion.ts`. Ist „Animationen entfernen“ eingeschaltet, entfallen
  alle Ein- und Ausblendungen (`src/motion.ts`).
- **Geräte:** Status, Koppeln per Code oder QR-Code (Kamera), Rechte. Kamera-Upload-Regel pro Gerät.
- **Mehr:** Sicherungen, Papierkorb, Freigabelinks, Speicheranalyse, Einstellungen (inkl. Sprache und
  Material You).
- **Tablet/Querformat:** Die untere Leiste wird zur Seitenleiste, Ordner und Inhalt stehen
  nebeneinander, wie am Desktop.

## Quellen

- [Nielsen Norman Group: Hamburger Menus and Hidden Navigation](https://www.nngroup.com/articles/hamburger-menus/)
- [Nielsen Norman Group: Find Navigation on Mobile, Even Behind a Hamburger](https://www.nngroup.com/articles/find-navigation-mobile-even-hamburger/)
- [Thumb Zone (Hoober) – Übersicht](https://www.digitalthriveai.com/en-ca/resources/web-design/the-thumb-zone-designing-for-mobile-users/)
- [Tippziele: Material 48 dp, Apple 44 pt](https://blog.openreplay.com/improving-tap-targets-mobile-ux/)
- [IxDF: Tappability Affordances](https://ixdf.org/literature/article/how-to-use-tappability-affordances)
