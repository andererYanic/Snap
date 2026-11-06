# Freigabelinks über die eigene Domain (Cloudflare Worker + R2)

Damit kannst du Dateien mit Personen teilen, die kein Snap haben, z. B. als
`https://share.deine-domain.de/l/…#…`. Snap verschlüsselt auf deinem Gerät; Cloudflare speichert
und liefert nur verschlüsselte Daten. Der Schlüssel steht im Link nach `#` und wird vom Browser
nie an Cloudflare geschickt. Links funktionieren auch, wenn deine Geräte aus sind.

**Kosten:** Der kostenlose Bereich von R2 (Stand 2026: 10 GB Speicher pro Monat, Downloads ohne
Gebühr) und von Workers (100 000 Anfragen pro Tag) reicht für private Nutzung. Für R2 verlangt
Cloudflare unter Umständen eine hinterlegte Zahlungsmethode. Aktuelle Preise:
<https://developers.cloudflare.com/r2/pricing/>.

## Einrichtung (einmalig, ca. 15 Minuten)

1. **Domain bei Cloudflare:** Deine Domain muss Cloudflare als DNS nutzen (Dashboard → „Website
   hinzufügen“).
2. **R2 aktivieren und Bucket anlegen:** Dashboard → R2 → „Bucket erstellen“, Name z. B.
   `snap-links`. Keinen öffentlichen Zugriff aktivieren.
3. **API-Token nur für diesen Bucket:** R2 → „API-Tokens verwalten“ → „API-Token erstellen“:
   - Berechtigung: **Objekt lesen und schreiben**
   - Gilt für: **nur den Bucket `snap-links`**
   - Notiere **Access Key ID**, **Secret Access Key** und den **Endpunkt**
     (`https://<konto-id>.r2.cloudflarestorage.com`).
4. **Worker veröffentlichen** (auf einem Rechner mit Node.js):
   ```bash
   cd gateway/cloudflare
   cp wrangler.toml.example wrangler.toml   # Domain und Bucket-Namen eintragen
   npx wrangler login
   npx wrangler deploy
   ```
   Optional für Download-Grenzen: `npx wrangler kv namespace create COUNTS` und die ausgegebene ID
   in `wrangler.toml` eintragen (Abschnitt `kv_namespaces`), dann erneut `deploy`.
5. **Snap verbinden** (der Secret Access Key wird verdeckt abgefragt, nicht als Argument):
   ```bash
   space-daemon links setup --url https://share.deine-domain.de \
     --endpoint https://<konto-id>.r2.cloudflarestorage.com --bucket snap-links --key-id <access-key-id>
   ```
6. **Ausprobieren:**
   ```bash
   space-daemon links create ~/Videos/urlaub.mp4             # 7 Tage, unbegrenzt oft
   space-daemon links create vertrag.pdf --days 1 --password # mit Passwort
   space-daemon links                                        # alle Links
   space-daemon links revoke <id>                            # widerrufen (löscht die Kopie)
   ```

## Sicherheit in Kürze

- **Wer den Link hat, hat die Datei**, bis er abläuft oder du ihn widerrufst. Für E-Mail oder
  Chats lohnt sich `--password`; das Passwort teilst du dann über einen anderen Weg.
- **Cloudflare sieht** Größe, Zeitpunkte und die IP-Adressen der Abrufer, aber weder Inhalt noch
  Dateinamen.
- **Die Seite kommt von deinem Worker.** Wer deinen Cloudflare-Zugang hätte, könnte veränderten
  Code ausliefern. Schütze dein Cloudflare-Konto deshalb mit Zwei-Faktor-Anmeldung.
- **Der API-Token** darf nur auf diesen einen Bucket zugreifen. Snap speichert ihn in
  `links.json` im privaten Datenordner (nur für dich lesbar).
- **Download-Grenzen** werden über KV nur näherungsweise gezählt. Gleichzeitige Abrufe können
  die Grenze knapp überschreiten.
- **Größe:** bis 5 GB pro Datei. Im Browser werden Dateien über 200 MB direkt auf die Platte
  geschrieben, wo das unterstützt ist (Chromium); sonst entstehen sie im Speicher.

## Rückfall: Cloudflare Tunnel / eigener Server

Wenn R2 nicht in Frage kommt, kann ein eigener Server (Docker) dieselbe Rolle übernehmen. Das
ist für später vorgesehen; das Format (`docs/LINK_FORMAT.md`) bleibt gleich.

## Tests

```bash
node --test gateway/cloudflare/test/worker.test.mjs   # Worker mit nachgebautem R2/KV
cargo test -p space-core --test links                  # Snap ↔ Browser-Kreuztest
```
