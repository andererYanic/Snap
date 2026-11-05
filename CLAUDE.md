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
- Arbeit auf Feature-Branches, Pull Requests nach `main`; vor dem Commit `cargo test` und
  `cargo clippy --all-targets` ohne Warnungen.
