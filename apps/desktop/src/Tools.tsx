// Freigabelinks und Speicheranalyse (Phase 5). Links: Ende-zu-Ende-verschlüsselt über den eigenen
// Cloudflare-Worker; Erstellen bestätigt der Nutzer zusätzlich im nativen Dialog (commands.rs).
import { t } from "./i18n";
import { useCallback, useEffect, useState } from "react";
import { api5, baseName, errorText, formatDate, humanSize, isCancelled, type DuplicateReport, type LinkInfo, type SpaceUsage } from "./api";
import type { Toast } from "./App";
import { Icon } from "./icons";

type Notify = (t: Toast) => void;
const fail = (notify: Notify) => (e: unknown) => {
  if (!isCancelled(e)) notify({ text: errorText(e), error: true });
};

function copy(text: string, notify: Notify) {
  navigator.clipboard.writeText(text).then(
    () => notify({ text: t("Link kopiert.") }),
    () => notify({ text: t("Kopieren nicht möglich."), error: true }),
  );
}

// ================================================================ Links

export function LinksPage({ notify }: { notify: Notify }) {
  const [data, setData] = useState<{ items: LinkInfo[]; base_url: string | null } | null>(null);
  const load = useCallback(() => {
    api5.links().then(setData).catch(fail(notify));
  }, [notify]);
  useEffect(load, [load]);

  const revoke = (l: LinkInfo) => {
    if (!window.confirm(t("Link für „{name}“ widerrufen? Die verschlüsselte Kopie wird gelöscht.", { name: l.name }))) return;
    api5.linkRevoke(l.id).then(() => { notify({ text: t("Link widerrufen.") }); load(); }).catch(fail(notify));
  };

  return (
    <div className="page">
      <h1>{t("Freigabelinks")}</h1>
      <p className="muted">{t("Für Personen ohne Snap: Die Datei wird auf diesem Computer verschlüsselt und in deinen eigenen Speicher geladen. Der Schlüssel steht nur im Link.")}</p>
      {data && !data.base_url && <LinkSetup notify={notify} done={load} />}
      {data?.base_url && (
        <section className="panel">
          <h2>{t("Links")}</h2>
          {data.items.length === 0 && <p className="muted">{t("Noch keine Links. Rechtsklick auf eine Datei → „Link erstellen …“.")}</p>}
          {data.items.map((l) => (
            <div key={l.id} className="line">
              <div className="grow">
                <div className="strong">{l.name}</div>
                <div className="muted small">
                  {humanSize(l.size)} · {t("bis {date}", { date: formatDate(l.expires_at) })}
                  {l.max_downloads ? ` · ${t("höchstens {n} Downloads", { n: String(l.max_downloads) })}` : ""}
                  {l.password ? ` · ${t("mit Passwort")}` : ""}
                  {l.state !== "active" ? ` · ${l.state === "expired" ? t("abgelaufen") : t("widerrufen")}` : ""}
                </div>
              </div>
              {l.state === "active" && (
                <>
                  <button className="btn" onClick={() => copy(l.url, notify)}><Icon.copy /> {t("Kopieren")}</button>
                  <button className="btn" onClick={() => revoke(l)}>{t("Widerrufen")}</button>
                </>
              )}
            </div>
          ))}
          <p className="muted small">{t("Adresse: {url}", { url: data.base_url })}</p>
        </section>
      )}
    </div>
  );
}

function LinkSetup({ notify, done }: { notify: Notify; done: () => void }) {
  const [f, setF] = useState({ base: "", endpoint: "", bucket: "", key: "", secret: "" });
  const [busy, setBusy] = useState(false);
  const set = (k: keyof typeof f) => (e: React.ChangeEvent<HTMLInputElement>) => setF({ ...f, [k]: e.target.value });
  const ok = f.base.startsWith("https://") && f.endpoint.startsWith("https://") && f.bucket && f.key && f.secret;
  const save = () => {
    setBusy(true);
    api5
      .linkSetup(f.base, f.endpoint, f.bucket, f.key, f.secret)
      .then(() => { notify({ text: t("Freigabelinks eingerichtet.") }); setF({ ...f, secret: "" }); done(); })
      .catch(fail(notify))
      .finally(() => setBusy(false));
  };
  return (
    <section className="panel">
      <h2>{t("Einrichten")}</h2>
      <p className="muted small">{t("Anleitung: gateway/cloudflare/README.md. Der geheime Schlüssel bleibt im Snap-Dienst und wird nie wieder angezeigt.")}</p>
      <div className="form">
        <label>{t("Link-Adresse (Worker)")}<input className="field" placeholder="https://share.example.de" value={f.base} onChange={set("base")} /></label>
        <label>{t("S3-Endpunkt (R2)")}<input className="field" placeholder="https://<konto>.r2.cloudflarestorage.com" value={f.endpoint} onChange={set("endpoint")} /></label>
        <label>{t("Bucket")}<input className="field" value={f.bucket} onChange={set("bucket")} /></label>
        <label>{t("Zugangsschlüssel-ID")}<input className="field" value={f.key} onChange={set("key")} autoComplete="off" /></label>
        <label>{t("Geheimer Schlüssel")}<input className="field" type="password" value={f.secret} onChange={set("secret")} autoComplete="new-password" /></label>
      </div>
      <button className="btn primary" disabled={!ok || busy} onClick={save}>{busy ? t("Richte ein …") : t("Einrichten")}</button>
    </section>
  );
}

/** Dialog „Link erstellen“ für eine Datei (aus dem Kontextmenü). */
export function LinkDialog({ path, notify, onClose }: { path: string; notify: Notify; onClose: () => void }) {
  const [days, setDays] = useState("7");
  const [max, setMax] = useState("");
  const [pw, setPw] = useState("");
  const [busy, setBusy] = useState(false);
  const [made, setMade] = useState<LinkInfo | null>(null);
  const d = Number(days);
  const m = max.trim() === "" ? null : Number(max);
  const valid = Number.isInteger(d) && d >= 1 && d <= 365 && (m === null || (Number.isInteger(m) && m >= 1 && m <= 10000)) && (pw === "" || pw.length >= 8);
  const create = () => {
    setBusy(true);
    api5
      .linkCreate(path, d, m, pw || null)
      .then((l) => { setMade(l); copy(l.url, notify); })
      .catch((e) => { fail(notify)(e); if (isCancelled(e)) onClose(); })
      .finally(() => setBusy(false));
  };
  return (
    <div className="overlay dim" onClick={onClose} onKeyDown={(e) => e.key === "Escape" && onClose()} tabIndex={-1}>
      <div className="dialog" onClick={(e) => e.stopPropagation()}>
        <h2>{t("Link erstellen")}</h2>
        <p className="strong">{baseName(path)}</p>
        {made ? (
          <>
            <input className="field" readOnly value={made.url} onFocus={(e) => e.target.select()} />
            <div className="line">
              <span className="muted small grow">{t("Der Link wurde kopiert. Wer ihn hat, kann die Datei herunterladen.")}</span>
              <button className="btn" onClick={() => copy(made.url, notify)}><Icon.copy /> {t("Kopieren")}</button>
              <button className="btn primary" onClick={onClose}>{t("Fertig")}</button>
            </div>
          </>
        ) : (
          <>
            <div className="form">
              <label>{t("Gültig (Tage)")}<input className="field num" value={days} onChange={(e) => setDays(e.target.value)} /></label>
              <label>{t("Höchstens Downloads (leer = unbegrenzt)")}<input className="field num" value={max} onChange={(e) => setMax(e.target.value)} /></label>
              <label>{t("Passwort (optional, mindestens 8 Zeichen)")}<input className="field" type="password" value={pw} onChange={(e) => setPw(e.target.value)} autoComplete="new-password" /></label>
            </div>
            <div className="line">
              <span className="grow" />
              <button className="btn" onClick={onClose}>{t("Abbrechen")}</button>
              <button className="btn primary" disabled={!valid || busy} onClick={create}>{busy ? t("Wird verschlüsselt hochgeladen …") : t("Link erstellen")}</button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// ================================================================ Speicheranalyse

export function StoragePage({ path, notify, onOpen }: { path: string; notify: Notify; onOpen: (dir: string) => void }) {
  const [usage, setUsage] = useState<SpaceUsage | null>(null);
  const [dups, setDups] = useState<DuplicateReport | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setUsage(null);
    setDups(null);
    setBusy(true);
    api5.spaceUsage(path).then(setUsage).catch(fail(notify)).finally(() => setBusy(false));
  }, [path, notify]);
  const findDups = () => {
    setBusy(true);
    api5.duplicates(path).then(setDups).catch(fail(notify)).finally(() => setBusy(false));
  };
  const max = usage?.children[0]?.size || 1;
  return (
    <div className="page">
      <h1>{t("Speicher")}: {baseName(path)}</h1>
      {!usage ? (
        <p className="muted">{busy ? t("Wird berechnet …") : ""}</p>
      ) : (
        <>
          <p className="muted">{t("{size} in {n} Dateien", { size: humanSize(usage.total), n: usage.files.toLocaleString() })}</p>
          <section className="panel">
            <h2>{t("Nach Art")}</h2>
            <div className="typebar" role="img" aria-label={t("Nach Art")}>
              {usage.by_type.map((ty, i) => (
                <span key={ty.category} className={`seg c${i % 6}`} style={{ flexGrow: ty.size }} title={`${t(ty.category)}: ${humanSize(ty.size)}`} />
              ))}
            </div>
            <div className="legend">
              {usage.by_type.map((ty, i) => (
                <span key={ty.category}><i className={`dot c${i % 6}`} /> {t(ty.category)} · {humanSize(ty.size)}</span>
              ))}
            </div>
          </section>
          <section className="panel">
            <h2>{t("Größte Einträge")}</h2>
            {usage.children.map((c) => (
              <div key={c.name} className={`usage${c.is_dir ? " link" : ""}`} onClick={() => c.is_dir && onOpen(`${usage.path.replace(/\/$/, "")}/${c.name}`)}>
                <span className="grow name">{c.is_dir ? "📁 " : ""}{c.name}</span>
                <span className="bar"><span style={{ width: `${(c.size / max) * 100}%` }} /></span>
                <span className="muted small size">{humanSize(c.size)}</span>
              </div>
            ))}
          </section>
          {usage.largest.length > 0 && (
            <section className="panel">
              <h2>{t("Größte Dateien")}</h2>
              {usage.largest.map((f) => (
                <div key={f.path} className="line">
                  <span className="grow small" title={f.path}>{f.path}</span>
                  <span className="muted small">{humanSize(f.size)}</span>
                </div>
              ))}
            </section>
          )}
          <section className="panel">
            <h2>{t("Doppelte Dateien")}</h2>
            {!dups ? (
              <button className="btn" disabled={busy} onClick={findDups}>{busy ? t("Wird gesucht …") : t("Doppelte Dateien suchen (ab 1 MB)")}</button>
            ) : dups.groups.length === 0 ? (
              <p className="muted">{t("Keine doppelten Dateien gefunden.")}</p>
            ) : (
              <>
                <p className="muted">{t("{size} ließen sich sparen.", { size: humanSize(dups.wasted) })}</p>
                {dups.groups.map((g) => (
                  <div key={g.hash} className="panel inner">
                    <div className="strong">{humanSize(g.size)} × {g.paths.length}{g.other_devices.length ? ` · ${t("auch auf: {list}", { list: g.other_devices.join(", ") })}` : ""}</div>
                    {g.paths.map((p) => <div key={p} className="small" title={p}>{p}</div>)}
                  </div>
                ))}
              </>
            )}
          </section>
        </>
      )}
    </div>
  );
}
