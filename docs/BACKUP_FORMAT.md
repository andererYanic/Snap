# Format der Sicherungen (v1)

Implementiert in `core/src/backup/` (Rust, `ring`). Eine Ablage ist ein Ordner
`snap-backup-<id>/` auf dem Ziel (externe Platte) bzw. ein Präfix `backups/<id>/` (S3/R2).

## Schlüssel

- **Hauptschlüssel** `M` (32 Byte Zufall). Abgeleitet per HKDF-SHA256 (salt `snap-backup-v1`):
  `K_data` (AES-256-GCM, Info `data`) und `K_id` (HMAC-SHA256, Info `id`).
- `keys/password`: `M` verpackt mit `PBKDF2-HMAC-SHA256(passwort, salt, 600 000)`.
- `keys/recovery`: `M` verpackt mit `HKDF(R, salt, "snap-backup-recovery")`. `R` ist der
  Wiederherstellungsschlüssel (32 Byte, Base32 in Vierergruppen angezeigt).
- Verpackt wird mit AES-256-GCM, AAD `snap-backup-v1/key/<art>`.
- Auf dem sichernden Gerät liegt `M` in `<daten>/backup/<id>.key` (0600), damit Sicherungen ohne
  Eingabe laufen. Am Ziel liegt `M` nie unverpackt.

## Objekte

| Name | Inhalt |
|---|---|
| `config` | unverschlüsselt: `{"v":1,"id":…,"created":…,"chunker":"fastcdc-v1"}` |
| `keys/password`, `keys/recovery` | verpackter Hauptschlüssel (JSON) |
| `catalog` | verschlüsselt: Liste der Stände (Zeit, Quellordner → Wurzelbaum, Statistik) |
| `trees/<xx>/<id>` | verschlüsselt: Ordnerinhalt als JSON (Name, Art, Größe, mtime, Rechte, Stücke bzw. Unterbaum bzw. Symlink-Ziel) |
| `data/<xx>/<id>` | verschlüsselt: ein Datenstück |

- **ID** = HMAC-SHA256(`K_id`, Klartext), hexadezimal. Gleiche Inhalte haben dieselbe ID
  (Deduplizierung). Ohne Schlüssel lässt sich nicht erkennen, ob eine bekannte Datei enthalten ist.
- **Verschlüsselung** jedes Objekts: `nonce (12 Byte Zufall) ‖ AES-256-GCM(K_data, klartext)`, AAD =
  `snap-backup-v1/<objektname>`. Vertauschte oder veränderte Objekte fallen auf. Beim Laden wird
  zusätzlich die ID gegen den Inhalt geprüft.
- **Stücke**: FastCDC mit Normalisierung (min 256 KiB, Ziel 1 MiB, max 4 MiB) und fester
  Gear-Tabelle. Einfügungen verschieben nur wenige Schnittpunkte.
- **Unveränderte Dateien** (gleiche Größe und mtime wie im vorherigen Stand) werden nicht gelesen,
  ihre Stückliste wird übernommen. Liegt die mtime weniger als 2 s vor dem Beginn des vorherigen Laufs, wird
  trotzdem gelesen (grobe Zeitstempel könnten sonst eine Änderung direkt nach dem Lesen verbergen).
- **Ausdünnen**: Erst wird der Katalog ohne die entfernten Stände geschrieben, dann werden Objekte
  gelöscht, die kein verbliebener Stand mehr referenziert. Ein Abbruch lässt nur Reste zurück.
- **Mehrere Geräte**: Ein Ziel wird jeweils von einem Gerät beschrieben (gleichzeitiges Sichern
  zweier Geräte in dasselbe Ziel wird nicht unterstützt). Übernimmt ein anderes Gerät (`connect`),
  erkennt das bisherige Gerät am neuesten Stand im Katalog, dass es nicht mehr der letzte Schreiber
  war, und baut seinen lokalen Zwischenspeicher der vorhandenen Objekte neu auf.
- **Reihenfolge**: Erst Daten und Bäume schreiben, dann das Laufwerk synchronisieren, dann den
  Katalog schreiben. Ein Abbruch hinterlässt höchstens unbenutzte Objekte, nie einen kaputten Stand.

## Was das Ziel sieht

Anzahl und ungefähre Größe der Objekte sowie den Zeitpunkt der Sicherungen. Nicht sichtbar sind
Inhalte, Dateinamen, Ordnerstruktur und Anzahl der Dateien.
