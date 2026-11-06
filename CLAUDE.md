# Arbeitsregeln für dieses Repository

- Sprache: Deutsch (Code-Kommentare, Commits, Doku, Antworten).
- Vision, Entscheidungen und Phasen stehen in `docs/ROADMAP.md` – vor größeren Änderungen lesen
  und neue Grundsatzentscheidungen dort mit Datum eintragen.
- **Sicherheit:** Vor der Umsetzung sicherheitsrelevanter Systeme (Netzwerkprotokolle, Pairing,
  Rechte, Kryptografie) Design und Risiken vorlegen und nachfragen. Jede neue
  Netzwerkschnittstelle bekommt Angriffstests (fremde, entkoppelte, manipulierte Gegenstellen).
- **Performance:** Nach jedem neuen System ein Effizienz-Review mit Messung
  (`core/tests/perf.rs`, Release-Build) und Eintrag in `docs/PERFORMANCE.md`.
  Maßstab: keine GUI-Abfrage blockiert länger als 16 ms.
- **Öffentliche Veröffentlichung geplant:** Code muss für fremde Nutzer sicher sein – sichere
  Standardwerte, keine Geheimnisse oder persönlichen Daten im Repo, alle Eingaben von außen
  (Netzwerk, IPC, Dateisystem) als feindlich behandeln. Checkliste in `docs/ROADMAP.md` (Phase 7).
- Neue Parser für Eingaben von außen bekommen ein Fuzz-Ziel in `fuzz/` (Invarianten prüfen,
  nicht nur „kein Absturz“). Neue Abhängigkeiten: `cargo deny check` und `cargo audit` grün.
- Arbeit auf Feature-Branches, Pull Requests nach `main`; vor dem Commit `cargo fmt`,
  `cargo test` und `cargo clippy --all-targets -- -D warnings` (so prüft auch die CI).
- **Mergen (Freigabe des Nutzers vom 2026-11-06):** Eigene Pull Requests selbst nach `main`
  mergen, sobald die CI auf dem aktuellen Stand vollständig grün ist (Merge-Commit, keine
  Umschreibung der Historie). Die Pflicht, sicherheitsrelevante Designs vorher vorzulegen,
  bleibt davon unberührt.
- **Arbeitsweise (Wunsch des Nutzers vom 2026-11-06):** Selbstständig Phase für Phase weiterarbeiten,
  bis eine Entscheidung des Nutzers nötig ist (z. B. sicherheitsrelevantes Design); dann gebündelt fragen.
