#!/usr/bin/env bash
# Emulator-Test: App installieren, „Alle Dateien“ erlauben (wie ein Nutzer), starten und
# prüfen, ob sie nach dem Start des Kerns weiterläuft. Ergebnisse landen in e2e/out/.
set -u
APK="$1"
PKG=de.snap.mobile
OUT="$(dirname "$0")/out"
mkdir -p "$OUT"

adb wait-for-device
adb shell 'while [ "$(getprop sys.boot_completed)" != "1" ]; do sleep 2; done'
adb install -r -g "$APK"
# Entspricht dem Schalter „Zugriff auf alle Dateien erlauben“ in den Systemeinstellungen
adb shell appops set "$PKG" MANAGE_EXTERNAL_STORAGE allow
# Etwas zum Anzeigen
adb shell 'mkdir -p /sdcard/Documents /sdcard/DCIM && echo hallo > /sdcard/Documents/test.txt'
adb logcat -c

adb shell am start -W -n "$PKG/.MainActivity"
sleep 45
adb exec-out screencap -p > "$OUT/screen-1.png"

# Zweiter Start (wie „lässt sich nicht mehr öffnen“)
adb shell am force-stop "$PKG"
adb shell am start -W -n "$PKG/.MainActivity"
sleep 30
adb exec-out screencap -p > "$OUT/screen-2.png"

# Sichtbare Texte (Bildschirminhalt) für das Log
adb shell uiautomator dump /sdcard/ui.xml > /dev/null 2>&1
adb shell cat /sdcard/ui.xml > "$OUT/ui.xml" 2>/dev/null
grep -oE 'text="[^"]+"' "$OUT/ui.xml" | sed 's/text=//' | tr '\n' ' ' > "$OUT/ui-texts.txt"
echo "---- Sichtbare Texte ----"; cat "$OUT/ui-texts.txt"; echo

PID="$(adb shell pidof "$PKG" | tr -d '\r')"
adb logcat -d > "$OUT/logcat.txt"
adb logcat -d -b crash > "$OUT/logcat-crash.txt"
adb shell run-as "$PKG" cat files/snap/crash.log > "$OUT/crash.log" 2>/dev/null || true
adb shell run-as "$PKG" ls -la files files/snap > "$OUT/files.txt" 2>&1 || true

echo "---- Absturzpuffer ----"; cat "$OUT/logcat-crash.txt"
echo "---- crash.log ----"; cat "$OUT/crash.log" 2>/dev/null
echo "---- Snap/Rust im Log ----"; grep -iE "snap|rust|panic|FATAL|tombstone|DEBUG +:" "$OUT/logcat.txt" | tail -80

if [ -z "$PID" ]; then
  echo "FEHLER: Die App läuft nach dem Start nicht mehr."
  exit 1
fi
if grep -q "FATAL EXCEPTION\|Fatal signal" "$OUT/logcat.txt"; then
  echo "FEHLER: Absturz im Log."
  exit 1
fi
# Dateiansicht mit Inhalt statt Fehler- oder Absturzbildschirm (Deutsch oder Englisch)
if ! grep -qE "Documents" "$OUT/ui-texts.txt" || ! grep -qE "Dateien|Files" "$OUT/ui-texts.txt"; then
  echo "FEHLER: Die Dateiansicht wird nicht angezeigt."
  exit 1
fi
echo "OK: App läuft (PID $PID) und zeigt die Dateien."
