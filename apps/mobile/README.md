# Snap – Android-App

Status: **4b – erste lauffähige App**: Dateien, Fotos, Geräte, Mehr; Hintergrundarbeit über WorkManager. Gestaltung: [`docs/MOBILE_UX.md`](../../docs/MOBILE_UX.md).

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
  app/        React Native 0.88 ohne Expo-Clouddienste
    App.tsx, src/   Oberfläche (Snap-Optik, Material You wahlweise), Übersetzungen aus apps/desktop/locales
    android/        Kotlin: SnapCoreModule (Brücke), SyncWorker (WorkManager), Gradle-Aufgaben für Rust
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

## Bauen

Voraussetzungen:
- Android-SDK mit `platforms;android-37.0`, `build-tools;37.0.0` und `ndk;27.3.13750724`;
- JDK 17, Node 22;
- `rustup target add aarch64-linux-android`, `cargo install cargo-ndk`.

```bash
cd apps/mobile/app && npm ci
```

```bash
cd apps/mobile/app/android && ./gradlew assembleRelease -PreactNativeArchitectures=arm64-v8a
```

Gradle baut dabei den Rust-Kern (`cargo ndk`) und erzeugt die Kotlin-Anbindung automatisch. Die APK
liegt unter `android/app/build/outputs/apk/release/`.

**Signierung:** Noch mit dem Debug-Schlüssel. Für Veröffentlichungen auf GitHub folgt ein eigener
Release-Schlüssel als GitHub-Secret.

## Geprüft

- Bildschirm-Tests (jest, nachgebildeter Kern): Liste, Auswahl per langem Drücken, Papierkorb mit
  Rückgängig, Berechtigungsbildschirm, Sprachwahl nach Systemsprache.
- CI prüft, dass die APK keine Google Play Services und kein Firebase enthält.
- **Noch offen:** Test auf einem echten Gerät bzw. Emulator. In dieser Build-Umgebung gibt es keine
  Virtualisierung (KVM).
