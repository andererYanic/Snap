# Eigenes Iroh-Relais (optional)

Standardmäßig nutzt Snap die öffentlichen Relais von n0. Relais helfen beim Verbindungsaufbau
zwischen Geräten in verschiedenen Netzen und leiten Daten weiter, wenn keine direkte Verbindung
möglich ist.

- **Was ein Relais sieht:** welche Geräte (Node-IDs) wann verbunden sind und wie viele Daten
  fließen.
- **Was es nicht sieht:** Inhalte. Die Verbindungen sind Ende-zu-Ende-verschlüsselt (QUIC/TLS
  zwischen den Geräten).

Ein eigenes Relais hält auch diese Metadaten bei dir.

## Einrichtung (Docker)

Voraussetzungen:
- ein Server mit öffentlicher IP;
- eine (Sub-)Domain, z. B. `relay.deine-domain.de`, mit A/AAAA-Eintrag auf den Server;
- offene Ports 80/tcp, 443/tcp und 7842/udp.

1. In diesem Ordner `relay.toml.example` nach `relay.toml` kopieren. Dann `hostname` und
   `contact` anpassen und unter `[access]` die Node-IDs deiner Geräte eintragen
   (`space-daemon info` auf jedem Gerät).
2. Starten:
   ```bash
   docker compose up -d --build
   ```
   Das Zertifikat kommt beim ersten Aufruf automatisch von Let's Encrypt.
3. Prüfen: `https://relay.deine-domain.de` sollte im Browser antworten.
4. Auf jedem Gerät in `config.toml` eintragen und den Daemon neu starten:
   ```toml
   relay_urls = ["https://relay.deine-domain.de"]
   relay_fallback_n0 = true   # n0 als Ausweich, falls dein Server ausfällt
   ```
   Wer gar keine n0-Relais mehr nutzen will, setzt `relay_fallback_n0 = false`.

**Neues Gerät koppeln:** Seine Node-ID in `relay.toml` ergänzen, dann
`docker compose restart` ausführen. Bis dahin hilft der n0-Ausweich.

## Sicherheit

- Der Container läuft ohne root, mit schreibgeschütztem Dateisystem und ohne
  Linux-Capabilities.
- Nur Geräte auf der Liste dürfen das Relais nutzen.
- Verbindungen und Bandbreite je Gerät sind begrenzt (`[limits]`).
- Die Kennzahlen-Schnittstelle ist aus.
- Version 1.4.1 passt zum Daemon. Relais und Daemon gemeinsam aktualisieren.
- Die Adressauflösung (welches Relais ein Gerät nutzt) läuft weiter über n0-DNS. Deshalb
  genügt im Ticket die Node-ID.
