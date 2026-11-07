#!/usr/bin/env bash
# Ruckel-Messung im Emulator: großer Ordner, Scrollen, Halten-und-Wischen, Kontextmenü.
# Liest danach die Bildstatistik von Android (dumpsys gfxinfo). Nur zum Vergleichen zwischen
# Ständen – der Emulator rendert in Software und ist absolut gesehen langsamer als ein Telefon.
set -u
PKG=de.snap.mobile
OUT="$(dirname "$0")/out"
mkdir -p "$OUT"

# Testordner: 400 Textdateien und 60 Bilder
adb shell 'mkdir -p /sdcard/Download/perf && cd /sdcard/Download/perf && for i in $(seq -w 1 400); do echo "Datei $i" > "notiz-$i.txt"; done'
for i in $(seq -w 1 60); do adb shell screencap -p "/sdcard/Download/perf/bild-$i.png"; done

ui() { adb shell uiautomator dump /sdcard/ui.xml > /dev/null 2>&1; adb shell cat /sdcard/ui.xml; }
# Mitte des Elements mit Text bzw. Beschreibung $1 antippen
tap_text() {
  local xml b
  xml="$(ui)"
  b="$(echo "$xml" | tr '>' '\n' | grep -E "(text|content-desc)=\"$1\"" | head -1 | grep -oE 'bounds="\[[0-9]+,[0-9]+\]\[[0-9]+,[0-9]+\]"' | grep -oE '[0-9]+')"
  if [ -z "$b" ]; then echo "nicht gefunden: $1"; return 1; fi
  set -- $b
  adb shell input tap $(( ($1 + $3) / 2 )) $(( ($2 + $4) / 2 ))
}

adb shell am force-stop "$PKG"
adb shell am start -W -n "$PKG/.MainActivity" > /dev/null
sleep 20
tap_text "Interner Speicher" || tap_text "Internal storage"
sleep 3
tap_text "Download"; sleep 3
tap_text "perf"; sleep 5
adb exec-out screencap -p > "$OUT/perf-ordner.png"
echo "---- Sichtbar vor der Messung ----"; ui | grep -oE 'text="[^"]+"' | sed 's/text=//' | head -12 | tr '\n' ' '; echo

W="$(adb shell wm size | grep -oE '[0-9]+x[0-9]+' | tail -1)"; WX="${W%x*}"; WY="${W#*x}"
X=$(( WX / 2 )); TOP=$(( WY * 30 / 100 )); BOT=$(( WY * 80 / 100 ))

adb shell dumpsys gfxinfo "$PKG" reset > /dev/null
# Scrollen: 8 Wischbewegungen nach oben und unten
for k in 1 2 3 4; do
  adb shell input swipe $X $BOT $X $TOP 250; sleep 0.6
done
for k in 1 2 3 4; do
  adb shell input swipe $X $TOP $X $BOT 250; sleep 0.6
done
# Halten und über Einträge wischen (Mehrfachauswahl), dann loslassen
adb shell input motionevent DOWN $X $TOP; sleep 0.5
for y in $(seq $TOP 40 $BOT); do adb shell input motionevent MOVE $X $y; done
adb shell input motionevent UP $X $BOT; sleep 1
adb exec-out screencap -p > "$OUT/perf-auswahl.png"
# Auswahl aufheben, eine Datei lange drücken (Menü am Finger), Menü schließen
adb shell input keyevent KEYCODE_BACK; sleep 1
adb shell input swipe $X $TOP $X $TOP 600; sleep 1
adb exec-out screencap -p > "$OUT/perf-menue.png"
adb shell input keyevent KEYCODE_BACK; sleep 0.5
adb shell input keyevent KEYCODE_BACK; sleep 1

adb shell dumpsys gfxinfo "$PKG" > "$OUT/gfxinfo.txt"
echo "---- Bildstatistik (Messung Ruckeln) ----"
grep -E "Total frames rendered|Janky frames|50th percentile|90th percentile|95th percentile|99th percentile|Number Slow UI thread|Number Slow bitmap|Number Slow issue|Number Frame deadline missed" "$OUT/gfxinfo.txt" | head -12
