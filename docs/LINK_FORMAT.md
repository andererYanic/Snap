# Format der Freigabelinks (v1)

Implementiert in `core/src/links/crypto.rs` (Snap, Rust/ring) und
`gateway/cloudflare/public/snap-crypto.js` (Browser, WebCrypto). Ein Kreuztest
(`core/tests/links.rs`) entschlüsselt die Ausgabe von Rust mit dem Browser-Code.

## Link

```
https://<worker>/l/<id>#<geheimnis>
```

- `id`: 16 Byte Zufall, hexadezimal (32 Zeichen). Sie ist öffentlich und dient als Name der
  Objekte im Speicher.
- `geheimnis` `S`: 32 Byte Zufall, Base64url ohne Auffüllung (43 Zeichen). Es steht im
  Fragment, das Browser **nie** an Server senden.

## Schlüssel

```
P   = PBKDF2-HMAC-SHA256(passwort, "snap-link-v1/pw/" ‖ id, 600 000 Runden, 32 Byte)   (nur mit Passwort)
IKM = S ‖ P   bzw.   S
PRK = HKDF-Extract(SHA-256, salt = "snap-link-v1/" ‖ id, IKM)
K_data = HKDF-Expand(PRK, "data", 32),   K_meta = HKDF-Expand(PRK, "meta", 32)
```

## Objekte im Speicher (`links/<id>/…`)

| Objekt | Inhalt |
|---|---|
| `data` | Datei in Stücken à 65 536 Byte, je AES-256-GCM mit `K_data` (Tag 16 Byte angehängt). Nonce = 3 Null-Byte ‖ Stücknummer (8 Byte, big endian) ‖ Endmarke (1 = letztes Stück). Eine leere Datei hat ein leeres letztes Stück. Größe: `n + ⌈n/65536⌉·16` (mindestens 16). |
| `meta` | JSON `{"v":1,"name":…,"size":…,"mime":…,"chunk":65536}`, AES-256-GCM mit `K_meta`, Nonce = 12 Null-Byte (der Schlüssel wird nur einmal verwendet). |
| `policy.json` | **Unverschlüsselt** (liest der Worker): `{"v":1,"expires":<unix>,"max_downloads":null\|n,"password":bool,"size_enc":…}` |

Vertauschte, weggelassene oder abgeschnittene Stücke fallen durch Stücknummer und Endmarke im
Nonce auf; jede Änderung lässt die Authentifizierung scheitern.

## Was der Server sieht

Größe (ungefähr die der Datei), Erstell- und Ablaufzeit, ob ein Passwort gesetzt ist, Zeitpunkte
und IP-Adressen der Abrufe. Nicht sichtbar sind Inhalt und Dateiname.
