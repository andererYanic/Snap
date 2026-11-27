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

## Als Zentrale für andere Geräte

Ordner eines anderen Geräts auf dem Server vorhalten (vollständige Kopie unter `/data/Snap`):

```bash
docker exec snap space-daemon pin add Fotos
```

Der Platz dafür ist standardmäßig unbegrenzt (`pin_max_gb = 0` in `/data/config.toml`).

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
