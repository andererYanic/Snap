# Snap – Android-App

Status: **4a – eingebetteter Kern (FFI)**. Die App-Oberfläche folgt in 4b; Gestaltung: [`docs/MOBILE_UX.md`](../../docs/MOBILE_UX.md).

## Grundsätze (Entscheidungen vom 2026-11-07)

- **Ohne Google:** keine Play-Dienste, kein Firebase/FCM, keine Analyse- oder Absturzdienste,
  keine Tracker. Verteilung als signierte APK über GitHub-Releases.
- **Zugriff „Alle Dateien“** (MANAGE_EXTERNAL_STORAGE): ein vollwertiger Explorer, der den
  Dateimanager des Geräts ersetzen kann.
- **Hintergrund:**
  - Solange die App offen ist, laufen Live-Verbindungen.
  - Sonst plant AndroidX WorkManager die Arbeit akkuschonend: Kamera-Upload, Abgleich,
    Sicherungen. WorkManager läuft lokal, ohne Google-Server und ohne Dauerbenachrichtigung.

## Aufbau

```
apps/mobile/
  core-ffi/   Rust: Snap-Kern als Bibliothek (UniFFI) – läuft im App-Prozess
  android/    (4b) React Native ohne Expo-Clouddienste + Kotlin-Modul, das core-ffi lädt
```

`core-ffi` stellt bereit:
- `SnapCore.start(CoreOptions)`: privater Datenordner, gemeinsamer Speicher, Gerätename;
- `request(json)`: dieselben Anfragen wie der Desktop-Dienst (`space_core::ipc::Request`);
- `subscribe(listener)`: Ereignisse als JSON;
- `stop()` und `apiVersion()`.

Kotlin-Anbindung erzeugen (nach dem Bauen der Bibliothek):

```bash
cargo run -p snap-ffi --bin uniffi-bindgen -- generate --library target/debug/libsnap_ffi.so --language kotlin --out-dir <ziel>
```

## Sicherheit

- Die Oberfläche (React Native) ist mitgelieferter Code und zeigt Dateinamen nur als nativen
  Text. Eine Einschleusung wie bei einer Web-Ansicht gibt es nicht. Folgenreiches bestätigt die App
  trotzdem in nativen Dialogen.
- Datenbank, Schlüssel und Socket liegen im privaten App-Ordner. Andere Apps erreichen sie nicht
  (Android-Sandbox, Rechte 0600).
- Ungültige Anfragen (kein JSON, unbekannte Art, zu groß) werden abgewiesen, ohne Absturz;
  getestet in `embedded_core_end_to_end`.
