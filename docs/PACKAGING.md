# Pakete

## Flatpak (Desktop)

Manifest: `packaging/flatpak/de.snap.desktop.yml` (App-ID `de.snap.desktop`, GNOME-Laufzeit 48).
Das Paket enthält die Desktop-App und den Dienst `space-daemon`; die App startet den Dienst bei
Bedarf selbst (innerhalb der Sandbox, `systemctl --user` gibt es dort nicht).

```bash
flatpak-builder --force-clean --user --install build packaging/flatpak/de.snap.desktop.yml
```

```bash
flatpak run de.snap.desktop
```

Die CI (`.github/workflows/flatpak.yml`) baut bei Änderungen an der Desktop-App oder an der
Paketierung ein Bündel `snap.flatpak` als Artefakt. Installieren:

```bash
flatpak install --user snap.flatpak
```

### Rechte (finish-args) und warum

| Recht | Grund |
|---|---|
| `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc`, `--device=dri` | Fenster und Grafik |
| `--share=network` | Verbindungen zu den eigenen Geräten (Iroh/QUIC), Sicherungen, Freigabelinks |
| `--filesystem=host` | Dateimanager: Zugriff auf die Dateien des Nutzers (wie andere Dateimanager) |
| `--talk-name=org.freedesktop.Notifications` | Benachrichtigungen |

Keine Rechte für Geräte außer der Grafik, keine Session-Bus-Namen außer Benachrichtigungen, kein
`--filesystem=home/.ssh` o. ä. Daten und Schlüssel liegen in `~/.var/app/de.snap.desktop/`
(getrennt von einer Installation außerhalb von Flatpak).

### Offen

- **Flathub:** Der Bau lädt Crates und npm-Pakete während des Bauens (`--share=network`). Flathub
  verlangt vorab erzeugte Quellen: `flatpak-cargo-generator.py` (beide `Cargo.lock`) und
  `flatpak-node-generator` (`apps/desktop/package-lock.json`), dann `--share=network` entfernen.
- **Hintergrund:** Autostart des Dienstes nach der Anmeldung über das Background-Portal.
- **Signierte Releases** mit Bündel als Release-Anhang (Phase „Signierte Releases“).
