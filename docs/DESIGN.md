# Gestaltung

Snap soll sich anfühlen wie ein gut gemachtes Werkzeug: ruhig, genau, ohne Effekthascherei. Die
Schrift Inconsolata ist der Charakter der App – alles andere tritt zurück. Dieses Dokument gilt für
Android und Desktop; neue Oberflächen richten sich danach.

## Grundsätze

1. **Inhalt vor Rahmen.** Dateien, Fotos und Namen sind das Wichtigste. Keine Karte um jedes
   Element, keine Schatten als Dekoration. Gruppen entstehen durch Abstand und feine Linien.
2. **Farbe trägt Bedeutung.** Eine Akzentfarbe (Tinte, ein ruhiges Grün-Blau) für das, was man
   antippen soll oder was ausgewählt ist. Gelb nur für Ordner, Rot nur für Gefahr. Keine Verläufe,
   kein Leuchten, keine Glasflächen.
3. **Ein Symbolsatz.** Alle Symbole stammen aus `Icon` (eigene SVG-Linien, 24er-Raster, Strich 1,6,
   runde Enden). Keine Emoji und keine Unicode-Zeichen als Symbole – sie sehen auf jedem Gerät
   anders aus und wirken zusammengewürfelt.
4. **Rangfolge durch Ton, nicht durch Größe.** Hauptzeile in voller Textfarbe, Zusatzangaben
   gedämpft (`text2`), Nebensächliches noch leiser (`text3`). Schriftgrößen nur aus der Skala.
5. **Die Monospace-Schrift nutzen.** Zahlen stehen von selbst untereinander: Größen rechtsbündig,
   Daten kurz („gestern“, „14:30“, „3. Nov.“). Abschnittstitel in kleinen Versalien mit Abstand.
6. **Plattform respektieren.** Android: Titel links, Zurück-Pfeil, ⋮ für weitere Aktionen, untere
   Navigation mit Markierung, schwebende Taste für „Neu“, 48-dp-Tippziele. Desktop: dichte Listen,
   Tastatur zuerst, Kontextmenüs.
7. **Ränder mitgestalten.** Leere Ordner, keine Geräte, kein Netz, Laden und Fehler bekommen einen
   eigenen, freundlichen Zustand mit einem klaren nächsten Schritt.
8. **Text wie ein Mensch.** Kurz, konkret, ohne Ausrufezeichen und ohne Marketing. Knöpfe sagen,
   was passiert („Entpacken“, nicht „OK“).

## Farben

| Rolle | Hell | Dunkel |
|---|---|---|
| Hintergrund `bg` | `#F4F2ED` (Papier) | `#121211` |
| Fläche `surface` (Listen, Blätter) | `#FFFFFF` | `#1C1C1A` |
| Fläche 2 `surface2` (Felder, gedrückt) | `#EAE7E0` | `#272725` |
| Text `text` | `#1A1A18` | `#EDEBE6` |
| Text 2 `text2` | `#66655F` | `#A3A19A` |
| Text 3 `text3` | `#9C9A92` | `#6F6D67` |
| Linie `line` | `rgba(26,26,24,0.09)` | `rgba(237,235,230,0.09)` |
| Akzent `accent` | `#1F6B5C` | `#72C7B2` |
| Akzent weich `accentSoft` | `rgba(31,107,92,0.12)` | `rgba(114,199,178,0.16)` |
| Ordner `folder` | `#E2B04A` | `#C99A3C` |
| Gefahr `danger` | `#B3261E` | `#F2B8B5` |
| Hinweis `warn` | `#8A5A00` | `#E9C46A` |
| In Ordnung `ok` | `#2E7D4F` | `#7FD19B` |

Material You (wahlweise) ersetzt Hintergrund, Flächen und Akzent durch die Systemfarben.

## Schrift (Inconsolata)

| Stufe | Größe / Zeilenhöhe | Verwendung |
|---|---|---|
| Titel | 26 / 32, fett | Bildschirmtitel |
| Überschrift | 18 / 24, halbfett | Blatt- und Dialogtitel |
| Text | 16 / 22 | Listenzeilen, Fließtext |
| Meta | 13 / 18, `text2` | Größe, Datum, Erklärungen |
| Etikett | 12 / 16, Versalien, Laufweite 1 | Abschnittstitel, `text3` |

## Maße

- Raster 4: 4, 8, 12, 16, 24, 32. Seitenrand 16.
- Zeilen: 56 (eine Zeile) bzw. 64 (mit Meta). Tippziele mindestens 48.
- Radien: 12 für Gruppen und Blätter, 8 für Vorschaubilder und Felder, rund für Chips und Markierung
  in der Navigation, 16 für die schwebende Taste.
- Linien: Haarlinie in `line`, eingerückt bis zum Text (nicht unter dem Symbol).

## Nicht tun

Verläufe und unscharfe Farbkleckse im Hintergrund · Karten mit Schatten um alles · Emoji als
Symbole · zentrierte Titel auf Android · farbige Kacheln hinter jedem Symbol · fünf Akzentfarben ·
„Erfolgreich!“-Meldungen mit Ausrufezeichen · gleiche Abstände überall.
