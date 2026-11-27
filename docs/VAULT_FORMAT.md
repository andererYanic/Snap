# Tresor – Format und Sicherheit

Der Tresor liegt im privaten Datenordner unter `vault/` (Rechte 700/600). Er wird nie freigegeben,
abgeglichen, durchsucht oder für Vorschaubilder gelesen.

## Schlüssel (`vault.json`)

- Zufälliger 256-Bit-**Tresorschlüssel** (Zufall des Betriebssystems).
- Zweimal verpackt: mit dem **Passwort** und mit dem **Wiederherstellungscode**. Je Argon2id
  (Version 0x13, 64 MiB, 3 Durchläufe, 1 Spur, 16 Byte Salz) → 256-Bit-Schlüssel → AES-256-GCM
  über den Tresorschlüssel, AAD `snap-vault-v1/key/password` bzw. `…/recovery`.
- Wiederherstellungscode: 20 Zufallsbytes (160 Bit) in Base32 ohne verwechselbare Zeichen
  (`ABCDEFGHJKLMNPQRSTUVWXYZ23456789`), acht Vierergruppen. Er wird nur beim Anlegen angezeigt.
- Schlüsselparameter einer veränderten Datei werden vor dem Rechnen begrenzt (8–512 MiB, 1–10
  Durchläufe, 1–4 Spuren).

## Verzeichnis (`index`)

`Nonce (12) ‖ AES-256-GCM(Tresorschlüssel, JSON)`, AAD `snap-vault-v1/index`. Enthält je Datei
Kennung (16 Zufallsbytes, hex), Anzeigename mit Unterordnern, Größe und Zeitpunkt. Auf der Platte
stehen weder Namen noch Größen lesbar.

## Dateien (`objects/<kennung>`)

```
"SNAPVLT1" ‖ Zufall (8) ‖ Block 0 ‖ Block 1 ‖ … ‖ letzter Block
Block i  = AES-256-GCM(Tresorschlüssel, Nonce = Zufall ‖ i (u32 BE), Klartext ≤ 64 KiB)
AAD      = "snap-vault-v1/obj" ‖ Kennung (16) ‖ i (u32 BE) ‖ letzter Block (1 Byte)
```

Vertauschte, gekürzte, angehängte oder zwischen Dateien ausgetauschte Blöcke fallen beim
Entschlüsseln auf; dann wird abgebrochen und nichts Halbes bleibt liegen.

## Benutzung

- Entsperrt liegt der Tresorschlüssel nur im Speicher (`zeroize` beim Sperren). Sperre nach
  5 Minuten ohne Benutzung; die Android-App sperrt zusätzlich, sobald sie in den Hintergrund geht.
- Nach 5 falschen Versuchen wächst die Wartezeit (2 s, 4 s, … höchstens 15 Minuten).
- Zum Ansehen wird eine Datei nach `vault/open/<zufall>/` entschlüsselt und beim Sperren gelöscht.
  Andere Apps/Programme bekommen eine Tresor-Datei nur nach ausdrücklicher Rückfrage.
- Beim Aufnehmen werden die Originale nur auf Wunsch gelöscht – endgültig, nicht in den Papierkorb.

## Grenzen (ehrlich)

- Ein schwaches Passwort bleibt bei gestohlenem Gerät angreifbar; Argon2id bremst nur.
- Schadsoftware mit Root-Rechten oder ein entsperrtes Gerät in fremden Händen sieht entsperrte
  Inhalte.
- Auf Flash-Speicher lässt sich Gelöschtes nicht sicher überschreiben: Originale, die vor dem
  Aufnehmen unverschlüsselt lagen, können forensisch wiederherstellbar sein.
- Ohne Passwort und ohne Wiederherstellungscode ist der Inhalt verloren.
