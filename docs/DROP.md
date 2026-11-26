# Snap Drop – Dateien an Menschen in der Nähe senden

Stand: 2026-11-25. Entscheidungen des Nutzers: siehe `ROADMAP.md` (2026-11-25).

Snap Drop sendet Dateien an andere Personen in der Nähe, ähnlich wie AirDrop. Es ist **getrennt vom
Koppeln**: Drop gibt nie Rechte auf Freigaben oder Ordner, es überträgt nur ausdrücklich angebotene
und angenommene Dateien.

## Entscheidungen

| Thema | Entscheidung |
|---|---|
| Finden | Gleiches WLAN (mDNS), QR-/Kurzcode über das Relais, Bluetooth LE |
| Offline | In Version 1: Bluetooth findet, ein kurzzeitiger Android-Hotspot überträgt |
| Sichtbarkeit | Standard **Aus**; „Nur Kontakte“; „Alle für 10 Minuten“ |
| Kontakte | Nach einem Tausch auf beiden Seiten speicherbar, erkannt am Schlüssel |
| Vorschau | Winziges Vorschaubild, beim Empfänger unscharf, scharf erst auf Tippen |
| Plattformen | Android und Desktop |

## Identitäten

- **Drop-Schlüssel** (Ed25519, `drop_identity.key`, 0600): dauerhaft, **getrennt vom Geräteschlüssel**.
  Wer mit mir tauscht, lernt nie den Schlüssel, mit dem meine Geräte gekoppelt sind.
- **Transport-Schlüssel**: Für jede Sichtbarkeits-Sitzung und jeden Sendevorgang startet ein eigener
  Iroh-Endpunkt mit Zufallsschlüssel. Ein Gerät ist dadurch über verschiedene Orte und Zeiten nicht
  an seiner Transport-Kennung wiederzuerkennen.
- **Sitzungsbindung**: Beide Seiten leiten aus der TLS-Verbindung (`export_keying_material`) einen
  Sitzungswert ab. Daraus entstehen
  - der **Prüfcode** (6 Ziffern, auf beiden Bildschirmen gleich; erkennt einen Mittelsmann),
  - das **Kontakt-Geheimnis** (32 Byte, wird nur gespeichert, wenn der Nutzer den Kontakt speichert),
  - die Signatur des Drop-Schlüssels (beweist bei Kontakten, wer sendet, und ist nicht auf eine
    andere Verbindung übertragbar).

## Finden

- **mDNS** (`_snapdrop._udp.local`): Instanzname ist ein Zufallstoken, TXT enthält nur Version und
  Transport-Kennung. **Kein Name, kein Drop-Schlüssel** im Rundsenden.
- **Bluetooth LE**: Werbung mit Dienst-UUID und 8-Byte-Zufallstoken (wechselt alle 15 Minuten und mit
  jeder Sitzung). Die Verbindungsdaten (Transport-Kennung, Adressen, Hotspot-Fähigkeit) liest der
  Sender per GATT. Das Format ist ein eigener, versionierter Binärcode, geparst im Kern (Fuzz-Ziel).
- **QR/Kurzcode**: enthält Transport-Kennung und Einmal-Token, Verbindung über Relais.
- Gesendet wird nur, solange man sichtbar ist. Bei „Aus“ läuft weder mDNS noch Bluetooth noch ein
  Endpunkt.

## Protokoll `snap/drop/1`

JSON-Rahmen mit Längenpräfix und Größengrenze (wie die übrigen Protokolle).

1. **Probe** (Sender → Empfänger, wenn der Sender das Teilen-Menü öffnet):
   `{ v, tags: [8-Byte-HMAC(Kontakt-Geheimnis, nonce ‖ Empfänger-Kennung)] }` für jeden eigenen Kontakt
   (höchstens 512). Der Sender verrät dabei **nichts über sich**: Ohne das Geheimnis sind die Tags
   zufällig und nicht verknüpfbar.
2. **Karte** (Empfänger → Sender):
   - Modus „Alle“: Name und Gerätetyp. Drop-Schlüssel und Signatur nur, wenn ein Tag zu einem
     eigenen Kontakt passt (dann erscheint beim Sender „Kontakt“). Fremde lernen den Schlüssel nicht.
   - Modus „Kontakte“: nur wenn ein Tag zu einem eigenen Kontakt passt, sonst `Verborgen` und Ende.
     Ein Fremder erfährt nur, dass dort ein Snap-Gerät im Kontakte-Modus ist.
3. **Angebot** (Sender → Empfänger, nach Auswahl):
   `{ name, Drop-Schlüssel, Signatur, Dateien: [{name, size, blake3, mime}], vorschau? }`.
4. Der Empfänger prüft die Grenzen und zeigt die Anfrage mit Absender (bekannter Kontakt oder
   „Unbekannt“), Prüfcode, Anzahl, Größe und Typen. Antwort: `Annehmen` oder `Ablehnen`
   (Zeitlimit 2 Minuten).
5. **Übertragung**: Je Datei ein QUIC-Strom. Der Empfänger schreibt in eine `.snap-part`-Datei,
   prüft Größe und BLAKE3 und benennt erst dann um. Bei Abweichung wird gelöscht und abgebrochen.
6. **Kontakt speichern**: Danach fragen beide Seiten „Als Kontakt speichern?“. Gespeichert werden
   Drop-Schlüssel, Name und Kontakt-Geheimnis. Der Kontakte-Modus funktioniert für **gegenseitige**
   Kontakte.

## Offline über Hotspot (Android)

1. Der Sender findet den Empfänger per Bluetooth und liest dessen Verbindungsdaten.
2. Gibt es keinen gemeinsamen Weg (Verbindungsversuch scheitert nach 3 s), öffnet der Sender einen
   **LocalOnlyHotspot** (Android-System, ohne Google) mit Zufalls-SSID und -Passwort.
3. Der Sender schreibt per GATT eine Kurzanfrage an den Empfänger: Name, Anzahl, Größe und die
   Hotspot-Zugangsdaten.
4. Der Empfänger sieht **eine** Snap-Anfrage. Erst nach „Annehmen“ verbindet er sich mit dem Hotspot.
   Android fragt dabei noch einmal selbst nach. Danach läuft das normale Protokoll ab Schritt 1. Das
   endgültige Angebot muss zur Kurzanfrage passen, sonst wird abgebrochen.
5. Der Hotspot schließt nach der Übertragung, spätestens nach 10 Minuten.

Der Desktop kann einem Android-Hotspot beitreten (NetworkManager über D-Bus, nie per
Kommandozeile mit Passwort). Desktop ↔ Desktop offline gibt es in Version 1 nicht.

## Ablage beim Empfänger

- `Downloads/Snap Drop/` (einstellbar). Nichts wird automatisch geöffnet.
- Dateinamen: nur der letzte Pfadteil, Steuerzeichen und `/ \ :` entfernt, höchstens 255 Byte,
  versteckte Namen bekommen ein sichtbares Präfix, Kollisionen werden nummeriert.
- Ausführbare Rechte werden nie gesetzt (Modus 0644).

## Grenzen

| Grenze | Wert |
|---|---|
| Rahmen (Probe, Karte, Angebot) | 256 KiB |
| Dateien je Angebot | 1 000 |
| Gesamtgröße | freier Speicher minus 1 GiB Reserve |
| Vorschaubild | 8 KiB, höchstens 64 × 64 px, Dekodieren mit Pixelgrenze |
| Offene Anfragen | 1 je Absender, 3 insgesamt |
| Proben / Angebote | 30 / 6 pro Minute, darüber verworfen |
| Abgelehnte Fremd-Angebote | nach 3 in einer Sitzung schaltet „Alle“ ab |
| Sichtbarkeit „Alle“ | 10 Minuten, dann automatisch „Aus“ bzw. „Kontakte“ |

## Risiken und Gegenmaßnahmen

| Risiko | Gegenmaßnahme | Rest-Risiko |
|---|---|---|
| Unerwünschte Bilder | Vorschau nur winzig und unscharf; Ablehnen/Blockieren mit einem Tipp | gering |
| Gefälschter Name | Kontakte am Drop-Schlüssel erkannt (Signatur), Fremde als „Unbekannt“ markiert, Prüfcode | Fremde können jeden Namen wählen (wie bei AirDrop) |
| Mittelsmann bei Fremden (z. B. über Bluetooth) | E2E-verschlüsselt mit Prüfcode auf beiden Seiten | nur erkennbar, wenn man den Code vergleicht |
| Verfolgung | Keine festen Kennungen im Rundsenden; Transport-Schlüssel je Sitzung; nur bei Sichtbarkeit | Bluetooth-Hardware-Adresse rotiert das Betriebssystem |
| Ausspähen der Kontakte | Proben mit HMAC-Tags, nur gegenseitige Kontakte erkennen sich | – |
| Spam / Überlastung | Rate-Limits, offene Anfragen begrenzt, Auto-Aus, Speicherprüfung | gering |
| Manipulierte Dateien/Namen | Namensbereinigung, BLAKE3-Prüfung, kein Ausführen, Fuzz-Ziele | – |
| Hotspot-Zugangsdaten mitgelesen | Hotspot nur kurz, Daten trotzdem E2E-verschlüsselt mit festen Kennungen | Fremder kann dem leeren Hotspot beitreten |
| Alte Android-Versionen (< 12) | Bluetooth-Suche verlangt dort die Standortberechtigung (Systemvorgabe); ab Android 12 „neverForLocation“ | nur Android 7–11 |

## Stufen

1. **Kern**: Identität, Kontakte, Sichtbarkeit, Endpunkt je Sitzung, mDNS, Protokoll, Ablage,
   Grenzen, API, Codec für Bluetooth/QR, Angriffstests, Fuzz-Ziele, Messung.
2. **Android**: Oberfläche (Teilen → In der Nähe, Annahme, Sichtbarkeit, Kontakte), Bluetooth LE
   (Kotlin), Multicast-Sperre für mDNS, LocalOnlyHotspot.
3. **Desktop**: Oberfläche, Bluetooth über BlueZ (wenn vorhanden), Hotspot-Beitritt über
   NetworkManager, Flatpak-Rechte.
