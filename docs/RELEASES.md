# Releases: signiert, nachprüfbar, mit Stückliste

Ein Release entsteht durch ein Versions-Tag (`git tag v0.2.0 && git push origin v0.2.0`). Der
Workflow `.github/workflows/release.yml` baut und veröffentlicht:

| Datei | Inhalt |
|---|---|
| `snap-<version>.apk` | Android-App, mit dem eigenen Release-Schlüssel signiert (arm64, armv7, x86_64) |
| `snap-daemon-<version>-linux-x86_64.tar.gz` | Dienst und CLI `space-daemon` |
| `snap-<version>.flatpak` | Desktop-App mit Dienst (Flatpak-Bündel) |
| `sbom-*.cdx.json` | Stücklisten (CycloneDX) aller Rust- und npm-Abhängigkeiten |
| `SHA256SUMS` | Prüfsummen aller Dateien |

Zu jeder Datei gibt es einen **Herkunftsnachweis** (GitHub-Attestierung, Sigstore): Er belegt,
dass die Datei von diesem Workflow aus genau diesem Commit gebaut wurde.

## Einmalig: APK-Schlüssel anlegen (Nutzer, lokal)

Der Schlüssel entsteht auf deinem Rechner und liegt danach **nur** als GitHub-Secret vor – nie im
Repository. Bewahre `snap-release.keystore` und die Passwörter zusätzlich offline auf: Geht der
Schlüssel verloren, können installierte Apps keine Updates mehr bekommen.

```bash
keytool -genkeypair -v -keystore snap-release.keystore -alias snap -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Snap"
```

```bash
base64 -w0 snap-release.keystore | gh secret set SNAP_KEYSTORE_B64 --repo andererYanic/snap
```

Die drei Werte fragt `gh` verdeckt ab (nicht als Argument übergeben):

```bash
gh secret set SNAP_KEYSTORE_PASSWORD --repo andererYanic/snap
```

```bash
gh secret set SNAP_KEY_ALIAS --repo andererYanic/snap
```

```bash
gh secret set SNAP_KEY_PASSWORD --repo andererYanic/snap
```

Ohne diese Secrets bricht der Release-Workflow ab; mit dem Debug-Schlüssel wird nie veröffentlicht.

## Prüfen (Nutzer der Releases)

```bash
sha256sum -c SHA256SUMS
```

```bash
gh attestation verify snap-v0.2.0.apk --repo andererYanic/snap
```

APK-Zertifikat anzeigen und mit dem im README veröffentlichten Fingerabdruck vergleichen:

```bash
apksigner verify --print-certs snap-v0.2.0.apk
```

## Reproduzierbare Builds

- Feste Werkzeuge: Rust-Version im Workflow (`RUST_VERSION`), `--locked` für Cargo, `npm ci`,
  feste NDK-, SDK- und `cargo-ndk`-Versionen.
- Keine Bauumgebung im Ergebnis: `--remap-path-prefix` für Quell- und Cargo-Pfade,
  `SOURCE_DATE_EPOCH` aus dem Commit, Archive mit fester Reihenfolge, Zeit und Besitzer
  (`tar --sort=name --mtime … | gzip -n`).
- Nachbauen des Dienstes: dieselben Befehle wie im Job `linux` auf einem x86_64-Linux mit der
  genannten Rust-Version ausführen; die Prüfsumme des Archivs muss übereinstimmen.
- APK: Signaturblock unterscheidet sich naturgemäß. Vergleich ohne Signatur, z. B. mit
  `apksigcopier compare` oder `diffoscope` auf den entpackten Inhalt.
- **Offen:** automatischer Zweitbau in der CI mit Vergleich (geplant mit dem öffentlichen
  Repository).
