# Snap im Docker-Container (eigener Server)

Der Container ist ein **Snap-Gerät ohne Oberfläche**: Er ist immer erreichbar, empfängt
Foto-Sicherungen, hält Ordner deiner anderen Geräte vor und gibt Änderungen weiter – auch wenn
deine Geräte nie gleichzeitig online sind (Zentrale). Bedient wird er über die Kommandozeile
(`docker exec`) und von deinen anderen Geräten aus.

## Schnellstart

```bash
git clone https://github.com/andererYanic/Snap.git && cd Snap
docker compose -f packaging/docker/compose.yml up -d --build
```

Name des Geräts und Ordner des Servers stehen in `packaging/docker/compose.yml`
(`SNAP_NAME`, Abschnitt `volumes`). Beim ersten Start legt der Container
`/data/config.toml` an; spätere Änderungen dort bleiben erhalten.

## Koppeln

Auf dem Server einen Code erzeugen …

```bash
docker exec -it snap space-daemon pair invite
```

… und am Handy oder Laptop unter *Einstellungen › Geräte* eingeben. Beide zeigen danach einen
**Prüfcode**; der Server fragt im Terminal, ob er übereinstimmt (`j` bestätigt). Ohne Terminal
(z. B. aus einem Skript):

```bash
docker exec snap space-daemon pair confirm   # oder: pair reject
```

Den Einladungscode nur über einen sicheren Weg weitergeben – auch mit Prüfcode gilt: Wer den Code
hat, kann sich zumindest melden.

## Ordner des Servers freigeben

Ordner in `compose.yml` einbinden (z. B. `/srv/fotos:/shares/Fotos`) und freigeben:

```bash
docker exec snap space-daemon share add /shares/Fotos
```

## Fotos vom Handy sichern

Ordner einbinden (z. B. `/srv/fotos:/shares/Fotos`), freigeben und dem Handy das Schreiben dort
erlauben (Schreibrechte vergibt Snap nie von selbst):

```bash
docker exec snap space-daemon share add /shares/Fotos
docker exec snap space-daemon pair grant Handy files.write --folder /shares/Fotos
```

Am Handy unter *Einstellungen › Geräte › Server* „Fotos hierhin sichern“ einschalten. Neue Fotos und
Videos der Kamera landen dann in `Fotos/<Name des Handys>/` – nie überschreibend, auch nach
Offline-Phasen (die App gleicht etwa stündlich im Hintergrund ab).

## Gerät finden

Damit der Server ein Handy klingeln lassen darf, muss das Handy es erlauben (*Geräte › Server ›
„Darf mein Gerät klingeln lassen“*). Dann:

```bash
docker exec snap space-daemon ring Handy          # 30 Sekunden laut
docker exec snap space-daemon ring Handy --stop
```

## Als Zentrale für andere Geräte

Ordner eines anderen Geräts auf dem Server vorhalten (vollständige Kopie unter `/data/Snap`):

```bash
docker exec snap space-daemon pin add Fotos
```

Der Platz dafür ist standardmäßig unbegrenzt (`pin_max_gb = 0` in `/data/config.toml`).

### Ordner zwischen allen Geräten synchron halten

Der Server hält die Ordner in **beide Richtungen** abgeglichen und reicht Änderungen weiter – auch
wenn Handy und PC nie gleichzeitig online sind:

```bash
# 1. Auf dem PC dem Server erlauben, im Ordner zu ändern und zu löschen (Papierkorb):
space-daemon pair grant Server files.write --folder ~/Dokumente
space-daemon pair grant Server files.delete --folder ~/Dokumente
# 2. Auf dem Server den Ordner in beide Richtungen spiegeln und den Spiegel freigeben:
docker exec snap space-daemon pin add PC:Dokumente --two-way
docker exec snap space-daemon share add /data/Snap --name Kopien
docker exec snap space-daemon pair grant Handy files.write --folder /data/Snap
docker exec snap space-daemon pair grant Handy files.delete --folder /data/Snap
```

Am Handy dann im Server unter *Kopien › PC › Dokumente* oben rechts „Ordner synchron halten“ ›
„Beide Richtungen“. Regeln: Neuestes gewinnt (die ältere Fassung landet im Papierkorb), Gelöschtes
landet im Papierkorb, und bei vielen Löschungen auf einmal (mehr als 20) fragt Snap nach:

```bash
docker exec snap space-daemon pin list              # ⇄ = beide Richtungen, angehaltene Löschungen
docker exec snap space-daemon pin deletes 1 yes     # oder: no (Dateien behalten)
```

## Netzwerk

- **Keine offenen Ports nötig:** Verbindungen laufen ausgehend, notfalls über ein Relais, das nur
  verschlüsselte Daten sieht.
- **Schneller mit Port:** `11204/udp` freigeben (in `compose.yml` schon eingetragen) ermöglicht
  direkte Verbindungen.
- Eigenes Relais: `relay_urls` in `/data/config.toml` (siehe Kommentar in der Vorlage).

## Sicherheit

- Läuft als Benutzer `snap` (UID 10001), mit schreibgeschütztem Abbild, ohne Zusatzrechte
  (`cap_drop: ALL`, `no-new-privileges`).
- Das Volume `/data` enthält die **Schlüssel des Geräts** und die Datenbank: privat halten, nicht
  mit anderen Containern teilen, regelmäßig sichern.
- Der Steuer-Socket liegt in `/data/state` (nur für den Benutzer `snap`); bedienen lässt sich der
  Dienst nur mit `docker exec` – nie über das Netz.

## Wartung

```bash
docker exec snap space-daemon info        # Zustand
docker exec snap space-daemon pair list   # gekoppelte Geräte
docker compose -f packaging/docker/compose.yml up -d --build   # Aktualisieren
```
