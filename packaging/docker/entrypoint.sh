#!/bin/sh
# Startet den Snap-Dienst im Container. Beim ersten Start wird eine Konfiguration mit sicheren
# Werten angelegt (danach gilt nur noch $SPACE_CONFIG – Änderungen dort bleiben erhalten).
set -eu
umask 077
data="${SNAP_DATA:-/data}"
mkdir -p "$data/state" "$data/Snap" "$data/Downloads"
if [ ! -f "$SPACE_CONFIG" ]; then
  name="${SNAP_NAME:-Server}"
  port="${SNAP_PORT:-11204}"
  # Nur druckbare, harmlose Zeichen im Namen (landet in TOML)
  name="$(printf '%s' "$name" | tr -cd 'A-Za-z0-9 ._-' | cut -c1-40)"
  case "$port" in ''|*[!0-9]*) port=11204 ;; esac
  cat > "$SPACE_CONFIG" <<CONF
# Snap im Container – Werte siehe docs/DOCKER.md
device_name = "${name:-Server}"
bind_port = ${port}
# Der Server ist immer erreichbar: als Zentrale für die anderen Geräte
live_connections = true
sync_interval_hours = 1
# Keine lokale Suche über das Container-Dateisystem
index_enabled = false
index_drives = false
# Spiegel anderer Geräte: 0 = ohne Grenze (Platz auf dem Server beachten)
pin_max_gb = 0
CONF
fi
exec space-daemon start
