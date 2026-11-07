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

- **Auswahl:** Langes Drücken startet den Auswahlmodus. Die untere Leiste wird dann zur
  Aktionsleiste (Kopieren, Verschieben, Teilen, Papierkorb, Mehr); oben stehen Anzahl und „Alle“.
- **Dateien:**
  - Start ist der gemeinsame Speicher.
  - Oben eine kleine Ortsauswahl: Telefon, SD-Karte, gekoppelte Geräte.
  - Liste oder Raster umschaltbar. Zuletzt geöffnete Dateien als horizontale Leiste über der Liste.
- **Fotos:** Zeitachse nach Monaten mit Schnellsprung am rechten Rand (Daumen). Antippen öffnet
  Vollbild mit Wischen zwischen Bildern.
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
