// Sicherheitsübersicht (Desktop): Hinweise, Rechte je Gerät, Sichtbarkeit. Nur lesend.
import { useEffect, useState } from "react";
import { t } from "./i18n";
import { errorText, securityOverview, type SecurityReport } from "./api";
import type { Toast } from "./App";

const RIGHTS: Record<string, string> = {
  "catalog.read": "Dateiliste sehen",
  "files.read": "Dateien lesen",
  "files.write": "Dateien ändern",
  "files.delete": "In den Papierkorb",
  "files.purge": "Endgültig löschen",
  "pin.request": "Kopien anfordern",
  "clipboard.write": "Zwischenablage füllen",
  "device.ring": "Klingeln lassen",
  admin: "Verwalten (alles)",
};

function hintText(h: SecurityReport["hints"][number]): string {
  switch (h.code) {
    case "purge": return t("„{name}“ darf Dateien endgültig löschen (ohne Papierkorb).", { name: h.name ?? "" });
    case "all_files": return t("„Alle Dateien“ ist eingeschaltet: Deine gekoppelten Geräte sehen den ganzen Speicher.");
    case "links": return t("{n} Freigabelink(s) sind aktiv – wer den Link hat, kann die Datei laden.", { n: h.count ?? 0 });
    case "drop_everyone": return t("Snap Drop ist gerade für alle in der Nähe sichtbar (läuft von selbst ab).");
    case "vault_unlocked": return t("Der Tresor ist entsperrt (sperrt sich nach 5 Minuten ohne Benutzung).");
    case "denied": return t("{n} abgewiesene Zugriffe in den letzten 7 Tagen – Einzelheiten im Protokoll.", { n: h.count ?? 0 });
    case "no_devices": return t("Noch kein Gerät gekoppelt.");
    default: return h.text;
  }
}

export function SecurityPage({ notify }: { notify: (t: Toast) => void }) {
  const [r, setR] = useState<SecurityReport | null>(null);
  useEffect(() => {
    securityOverview().then(setR).catch((e) => notify({ text: errorText(e), error: true }));
  }, [notify]);
  if (!r) return <section className="panel muted">…</section>;
  const right = (x: string) => {
    const i = x.indexOf(": ");
    const cap = i < 0 ? x : x.slice(0, i);
    return t(RIGHTS[cap] ?? cap) + (i < 0 ? "" : ` · ${x.slice(i + 2)}`);
  };
  return (
    <>
      {r.hints.length > 0 && (
        <section className="panel">
          {r.hints.map((h, i) => (
            <p key={i} className={h.level === "warn" ? "strong" : "muted"}>{h.level === "warn" ? "⚠ " : "ℹ "}{hintText(h)}</p>
          ))}
        </section>
      )}
      <section className="panel">
        <h2>{t("Gekoppelte Geräte")}</h2>
        {r.devices.length === 0 && <p className="muted">{t("Noch kein Gerät gekoppelt.")}</p>}
        {r.devices.map((d) => (
          <div key={d.node_id}>
            <div className="strong">{d.name}</div>
            <div className="muted small">{d.rights.map(right).join(" · ")}</div>
          </div>
        ))}
      </section>
      <section className="panel">
        <h2>{t("Sichtbarkeit")}</h2>
        <div className="line"><span className="grow">{t("Freigegebene Ordner")}</span><span>{r.shares.length}</span></div>
        <div className="line"><span className="grow">{t("Alle Dateien sichtbar")}</span><span>{r.all_files ? t("Ja") : t("Nein")}</span></div>
        <div className="line"><span className="grow">{t("Aktive Freigabelinks")}</span><span>{r.links_active}</span></div>
        <div className="line"><span className="grow">{t("Snap Drop")}</span><span>{r.drop_mode === "Everyone" ? t("Für alle sichtbar") : r.drop_mode === "Contacts" ? t("Für Kontakte sichtbar") : t("Unsichtbar")}</span></div>
        <div className="line"><span className="grow">{t("Tresor")}</span><span>{!r.vault_exists ? t("Nicht eingerichtet") : r.vault_unlocked ? t("Entsperrt") : t("Gesperrt")}</span></div>
        <div className="line"><span className="grow">{t("Abgewiesene Zugriffe (7 Tage)")}</span><span>{r.denied_7d}</span></div>
        <div className="line"><span className="grow">{t("Relais")}</span><span>{r.relay_public ? t("Öffentlich – sieht nur Verschlüsseltes") : t("Eigenes oder aus")}</span></div>
      </section>
    </>
  );
}
