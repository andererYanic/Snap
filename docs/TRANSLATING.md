# Übersetzen / Translating

**English summary:**
1. Copy `apps/desktop/locales/en.json` to `apps/desktop/locales/<code>.json` (e.g. `fr.json`,
   `pt-br.json`).
2. Set `name` (the language's own name, e.g. "Français") and `locale` (e.g. `fr-FR`).
3. Translate the values; never change the German keys or the `{placeholders}`.
4. Run `cargo test` in `apps/desktop/src-tauri` and open a pull request.

No code changes are needed: the app and its native dialogs pick up every file in that folder
automatically.

---

## So funktioniert es

- **Ausgangssprache ist Deutsch.** Der deutsche Text im Code ist zugleich der Schlüssel, wie bei
  gettext. Die Oberfläche schreibt `t("Papierkorb leeren")`, der Rust-Teil für native Dialoge
  `tr("Papierkorb leeren", &[])`.
- **Eine Datei je Sprache:** `apps/desktop/locales/<kürzel>.json`. Beide Teile lesen dieselben
  Dateien:
  - die Oberfläche über `import.meta.glob`;
  - die nativen Dialoge über `build.rs`, das alle Dateien einbettet.
- **Aufbau:**

  ```json
  {
    "name": "English",
    "locale": "en-GB",
    "strings": {
      "Papierkorb leeren": "Empty trash",
      "{n} Elemente": "{n} items"
    }
  }
  ```

  - `name`: Name der Sprache in ihr selbst. So erscheint sie in den Einstellungen.
  - `locale`: BCP-47-Tag für Datums- und Zahlenformate, Sortierung und Monatsnamen.
  - `strings`: deutscher Text → Übersetzung. Platzhalter wie `{n}`, `{name}` oder `{date}` müssen
    erhalten bleiben; ihre Reihenfolge darf sich ändern.
- **Sprachwahl:** Zuerst gilt die in den Einstellungen gewählte Sprache. Sonst die Systemsprache
  (exakt, z. B. `pt-br`, dann der Grundteil `pt`). Sonst Deutsch. Fehlt ein einzelner Text, wird
  er deutsch angezeigt.

## Neue Sprache hinzufügen

1. `apps/desktop/locales/en.json` nach `apps/desktop/locales/<kürzel>.json` kopieren. Das Kürzel
   ist klein geschrieben, z. B. `fr`, `es` oder `pt-br`.
2. `name` und `locale` anpassen, alle Werte übersetzen.
3. Prüfen:
   ```bash
   cd apps/desktop/src-tauri && cargo test
   ```
   Der Test `every_language_is_complete` meldet fehlende Übersetzungen, verlorene Platzhalter
   und ungültiges JSON.
4. Die App bauen bzw. neu starten und unter **Einstellungen → Sprache** auswählen.

## Für Entwickler: neue Texte

- Jeden sichtbaren Text in `t("…")` (Oberfläche) bzw. `tr("…", &[…])` (Rust-Dialoge) setzen.
  Werte gehören als Platzhalter in den Text, nicht zusammengesetzt:
  `t("{n} ausgewählt", { n })` statt `n + " ausgewählt"`.
- Danach alle Sprachdateien ergänzen. Der Test im Rust-Teil findet neue Schlüssel automatisch
  und schlägt fehl, bis jede Datei sie enthält.
- Texte, die vom Dienst kommen, lassen sich ebenfalls übersetzen, wenn die Oberfläche sie durch
  `t()` reicht: Ortsnamen, Beschreibungen und Einheiten der Einstellungen. Sie stehen zusätzlich in
  den Sprachdateien, werden aber nicht vom Test erfasst.
- **Noch nicht übersetzt:** Fehler- und Statusmeldungen des Dienstes, z. B. „Rückgängig gemacht: …“,
  und die Kommandozeile. Sie erscheinen vorerst auf Deutsch.
