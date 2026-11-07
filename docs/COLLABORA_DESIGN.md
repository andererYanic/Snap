# Collabora-Anbindung: Entwurf und Angriffsfläche (zur Freigabe, 2026-11-07)

Ziel: Office-Dateien (DOCX, XLSX, PPTX, ODF) mit dem **eigenen** Collabora-Server des Nutzers
originalgetreu ansehen und bearbeiten – wie in Nextcloud. Zwei Wege, in den Einstellungen wählbar:

- **A – über ein eigenes Snap-Gerät** (Laptop/Server mit Snap, Collabora im selben Netz).
- **B – Handy direkt** (Collabora ist vom Handy aus erreichbar: Heimnetz oder öffentlich).

Nichts davon ist eingeschaltet, solange der Nutzer keinen Server einträgt.

## So funktioniert es (WOPI)

Collabora speichert keine Dateien. Es braucht einen **WOPI-Host**, der drei Dinge anbietet:
Datei-Infos (`CheckFileInfo`), Inhalt lesen (`GetFile`) und Inhalt schreiben (`PutFile`).
Collabora holt die Datei dort ab und schreibt Änderungen zurück; der Editor selbst läuft als
Webseite von Collabora.

| | Weg A (über Snap-Gerät) | Weg B (Handy direkt) |
|---|---|---|
| WOPI-Host | Snap-Daemon auf dem Laptop/Server | Snap-Kern auf dem Handy |
| Wer erreicht wen | Collabora → Snap-Gerät (lokales Netz); Handy → Snap-Gerät (bestehende P2P-Verbindung) | Collabora → Handy; Handy → Collabora |
| Editor im Handy | Webansicht, geladen über das Snap-Gerät | Webansicht direkt von Collabora |

**Problem bei Weg B:** Collabora muss das Handy erreichen können. Handys haben meist keine
erreichbare Adresse (Mobilfunk, wechselnde WLANs). Vorschlag: Bei Weg B lädt das Handy die Datei
für die Sitzung **in Collabora hoch** – über einen kleinen, nur für diese Sitzung gültigen
WOPI-Endpunkt auf dem Handy, den Collabora **nur im selben WLAN** erreicht. Unterwegs geht dann
nur Weg A. (Alternative: Weg B nur im Heimnetz anbieten.)

## Neue Angriffsfläche

### 1. WOPI-Endpunkt (neu, HTTP)

Ein neuer HTTP-Dienst, den Collabora aufruft. Das ist die größte neue Fläche.

| Risiko | Gegenmaßnahme |
|---|---|
| Jemand im Netz ruft den Endpunkt auf und liest/überschreibt Dateien | Je Sitzung **ein Zugriffstoken für genau eine Datei** (256 Bit Zufall, nur im Speicher), Gültigkeit 8 h bzw. bis zum Schließen; ohne gültiges Token nur `401`. Keine Pfade in der URL, nur eine zufällige Datei-Kennung. |
| Gefälschte Anfragen, die nicht von Collabora kommen | **WOPI Proof Keys**: Collabora signiert jede Anfrage; Snap prüft die Signatur mit dem öffentlichen Schlüssel aus Collabora (`/hosting/discovery`) und den Zeitstempel (max. 20 min alt). |
| Token abgegriffen (z. B. aus Logs) | Token nie protokollieren; nur über HTTPS bzw. lokales Netz; an die Datei-Kennung gebunden; widerrufbar (Sitzung beenden). |
| Endpunkt aus dem Internet erreichbar | Bindet standardmäßig nur an die Adresse im lokalen Netz bzw. an `127.0.0.1` (wenn Collabora auf demselben Rechner läuft); öffentliches Lauschen nur nach ausdrücklicher Einstellung. |
| Überlastung / große Uploads | Größenlimit für `PutFile` (z. B. 200 MB), Zeitlimits, höchstens wenige gleichzeitige Sitzungen, Rate-Limit je Token. |
| Manipulierte Antworten auf `PutFile` überschreiben das Original | Schreiben erst in eine Temp-Datei, dann atomar ersetzen; das **alte Original wandert in den Papierkorb** (rückgängig machbar); Sperren (`Lock`/`Unlock`) gegen gleichzeitige Schreiber. |
| Zugriff auf andere Dateien über Pfad-Tricks | Endpunkt kennt nur die eine freigegebene Datei je Token; es gibt keine Pfadauflösung aus der Anfrage. |

### 2. Webansicht (WebView) im Handy

Collaboras Editor ist eine Webseite. Das Handy braucht dafür eine eingebettete Webansicht.

| Risiko | Gegenmaßnahme |
|---|---|
| Webansicht ruft fremde Seiten auf | Nur die eingetragene Collabora-Adresse; jede andere Navigation wird blockiert bzw. im Browser geöffnet. |
| Seite greift auf das Handy zu | Kein JavaScript-Brückenobjekt zur App, keine Dateizugriffe (`file://` aus), keine Standort-/Kamera-Rechte; Cookies nur für diese Webansicht. |
| Kompromittierter Collabora-Server | Er sieht die Dateien, die man öffnet (wie bei jedem Editor) – aber keine anderen Dateien und keine Snap-Schlüssel. |
| Android-Webansicht veraltet | Sie kommt vom System (auf Geräten ohne Google z. B. von Vanadium/Bromite/LineageOS). Snap zeigt eine Warnung, wenn sie sehr alt ist. |

### 3. Weg A: Snap-Gerät als Vermittler

| Risiko | Gegenmaßnahme |
|---|---|
| Fremdes Gerät öffnet Dateien über den Laptop | Nur **gekoppelte** Geräte mit Leserecht (`files.read`) für den Ordner; Bearbeiten nur mit Schreibrecht (`files.write`) – die bestehende Rechteprüfung. |
| Neue Anfragen im Snap-Protokoll | Eigene, strikt typisierte Anfrage „Editor-Sitzung öffnen“; Angriffstests (fremd, entkoppelt, manipuliert, Flut) wie bei den anderen Netzschnittstellen. |
| Editor-Webseite über P2P weiterleiten | Snap leitet nur Anfragen an die eingetragene Collabora-Adresse weiter (kein offener Proxy), mit Größen- und Zeitlimits. |

### 4. Zugangsdaten und Verbindung zu Collabora

| Risiko | Gegenmaßnahme |
|---|---|
| Abgehört / falscher Server | Nur **HTTPS** mit Zertifikatsprüfung; reines HTTP nur für Adressen im lokalen Netz mit Warnung. Optional: Zertifikat beim ersten Verbinden merken und Änderungen melden. |
| Zugangsdaten (falls Collabora geschützt ist) | Nur auf dem Gerät gespeichert (0600 bzw. Android-Keystore), nie in Logs oder Antworten der API. |
| Manipulierte Antworten (`discovery`) | Strikte Größen- und Formatprüfung; Fuzz-Ziel für den Discovery-Parser. |

## Was sich nicht ändert

- Ohne eingetragenen Server gibt es keinen neuen Endpunkt und keine Webansicht.
- Dateien verlassen das Gerät nur auf ausdrücklichen Tipp („Mit Collabora bearbeiten“).
- Die Peer-zu-Peer-Verschlüsselung und die Rechteprüfung bleiben unverändert die Grundlage.

## Tests (vor dem Merge)

- WOPI: ohne/abgelaufenes/fremdes Token, falsche Signatur, alter Zeitstempel, zu großer Upload,
  gleichzeitige Schreiber, Abbruch mitten im Schreiben (Original bleibt heil).
- Weg A: fremde, entkoppelte und manipulierte Geräte; Gerät ohne Schreibrecht.
- Webansicht: Navigation auf fremde Adressen wird blockiert.
- Fuzz-Ziele für WOPI-Anfragen und Discovery.

## Offene Fragen an den Nutzer

1. Weg B: nur im selben WLAN anbieten (empfohlen) – oder auch mit öffentlich erreichbarem Handy?
2. Darf der WOPI-Endpunkt des Snap-Geräts im lokalen Netz lauschen (nötig, wenn Collabora auf
   einem anderen Rechner läuft), oder läuft Collabora bei dir auf demselben Rechner wie Snap?
