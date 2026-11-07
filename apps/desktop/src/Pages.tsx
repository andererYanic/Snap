// Geräte, Sicherungen und Einstellungen (Phase 3b).
import { LANGUAGES, lang, locale, setLanguage, t } from "./i18n";
import { useCallback, useEffect, useState } from "react";
import { api3, api5, applyFont, errorText, formatDate, humanSize, isCancelled, type Approval, type BackupJob, type Device, type Peer, type Setting, type Snapshot } from "./api";
import type { Toast } from "./App";
import { Icon } from "./icons";

type Notify = (t: Toast) => void;

function useLoader<T>(f: () => Promise<T>, notify: Notify): [T | null, () => void] {
  const [v, setV] = useState<T | null>(null);
  const load = useCallback(() => {
    f().then(setV).catch((e) => notify({ text: errorText(e), error: true }));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(load, [load]);
  return [v, load];
}

const fail = (notify: Notify) => (e: unknown) => {
  if (!isCancelled(e)) notify({ text: errorText(e), error: true });
};

const CAPS: Record<string, string> = {
  "catalog.read": t("Dateiliste sehen"),
  "files.read": t("Dateien lesen"),
  "files.write": t("Dateien ändern"),
  "pin.request": t("Kopien anfordern"),
  "files.delete": t("In den Papierkorb"),
  "files.purge": t("Endgültig löschen"),
  admin: t("Verwalten (alles)"),
};
const FOLDER_CAPS = ["files.read", "files.write", "files.delete", "files.purge"];

// ================================================================ Geräte

export function Devices({ notify }: { notify: Notify }) {
  const [devs, reload] = useLoader(() => api3.devices().then((r) => r.devices), notify);
  const [peers, reloadPeers] = useLoader(() => api3.peers().then((r) => r.peers), notify);
  const [pending, reloadPending] = useLoader(() => api3.opsPending().then((r) => r.items), notify);
  const [invite, setInvite] = useState<{ code: string; expires_at: number } | null>(null);
  const [code, setCode] = useState("");
  const [grantFor, setGrantFor] = useState<Device | null>(null);
  const status = (id: string) => (peers ?? []).find((p: Peer) => p.node_id === id)?.status ?? "offline";

  const startInvite = async () => {
    try {
      const inv = await api3.pairInvite();
      setInvite(inv);
      const r = await api3.pairInviteWait();
      setInvite(null);
      notify({ text: t("„{name}“ ist jetzt gekoppelt. Rechte vergibst du einzeln.", { name: r.device.name }) });
      reload();
      reloadPeers();
    } catch (e) {
      setInvite(null);
      fail(notify)(e);
    }
  };

  const join = () =>
    api3.pairJoin(code)
      .then((r) => {
        setCode("");
        notify({ text: t("Mit „{name}“ gekoppelt.", { name: r.device.name }) });
        reload();
      })
      .catch(fail(notify));

  return (
    <div className="page">
      <h1>{t("Geräte")}</h1>
      {(pending ?? []).length > 0 && (
        <section className="panel warn">
          <h2>{t("Wartende Anfragen anderer Geräte")}</h2>
          {(pending ?? []).map((a: Approval) => (
            <div className="line" key={a.id}>
              <span className="grow">{String(a.device_name ?? a.node_id.slice(0, 10))}: {a.summary}</span>
              <button className="btn" onClick={() => api3.opsReject(a.id).then(reloadPending).catch(fail(notify))}>{t("Ablehnen")}</button>
              <button className="btn primary" onClick={() => api3.opsApprove(a.id, String(a.summary ?? "")).then(reloadPending).catch(fail(notify))}>{t("Zulassen …")}</button>
            </div>
          ))}
        </section>
      )}
      <section className="panel">
        <h2>{t("Gekoppelte Geräte")}</h2>
        {devs && devs.length === 0 && <p className="muted">{t("Noch keine Geräte gekoppelt.")}</p>}
        {(devs ?? []).map((d: Device) => (
          <div className="device" key={d.node_id}>
            <div className="line">
              <span className={`dot ${status(d.node_id) === "online" ? "on" : ""}`} />
              <div className="grow">
                <div className="strong">{d.name}</div>
                <div className="muted small">
                  {status(d.node_id) === "online" ? t("Verbunden") : t("Nicht verbunden")} · {t("gekoppelt am {date}", { date: formatDate(d.paired_at) })}
                  {d.last_sync_at ? ` · ${t("zuletzt abgeglichen {date}", { date: formatDate(d.last_sync_at) })}` : ""}
                  {d.storage ? ` · ${t("{free} von {total} frei", { free: humanSize(d.storage.free), total: humanSize(d.storage.total) })}` : ""}
                </div>
              </div>
              <button className="btn" onClick={() => setGrantFor(grantFor?.node_id === d.node_id ? null : d)}>{t("Recht geben …")}</button>
              <button className="btn" onClick={() => api3.pairRemove(d.node_id, d.name).then(reload).catch(fail(notify))}>{t("Entkoppeln …")}</button>
            </div>
            <div className="chips">
              {d.grants.length === 0 && <span className="muted small">{t("Keine Rechte")}</span>}
              {d.grants.map((g) => (
                <span className="chip" key={g.capability + (g.folder ?? "")}>
                  {CAPS[g.capability] ?? g.capability}
                  {g.folder ? ` · ${g.folder}` : ""}
                  <button
                    title={t("Recht entziehen")}
                    onClick={() => api3.pairRevoke(d.node_id, g.capability, g.folder).then(reload).catch(fail(notify))}
                  >
                    <Icon.close size={10} />
                  </button>
                </span>
              ))}
            </div>
            {grantFor?.node_id === d.node_id && <GrantForm device={d} notify={notify} done={() => { setGrantFor(null); reload(); }} />}
          </div>
        ))}
      </section>
      <section className="panel">
        <h2>{t("Neues Gerät koppeln")}</h2>
        <div className="cols">
          <div>
            <p className="muted">{t("Auf diesem Gerät einen Code erzeugen und am neuen Gerät eingeben.")}</p>
            {invite ? (
              <>
                <div className="code">{invite.code}</div>
                <p className="muted small">Gültig bis {formatDate(invite.expires_at)} · wartet auf das andere Gerät …</p>
              </>
            ) : (
              <button className="btn primary" onClick={startInvite}>{t("Code erzeugen")}</button>
            )}
          </div>
          <div>
            <p className="muted">{t("Oder den Code eines anderen Geräts hier eingeben.")}</p>
            <div className="line">
              <input className="field grow" value={code} onChange={(e) => setCode(e.target.value)} placeholder={t("Einladungscode")} spellCheck={false} />
              <button className="btn primary" disabled={!code.trim()} onClick={join}>{t("Koppeln")}</button>
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}

function GrantForm({ device, notify, done }: { device: Device; notify: Notify; done: () => void }) {
  const [cap, setCap] = useState("files.read");
  const [folder, setFolder] = useState("");
  const needsFolder = FOLDER_CAPS.includes(cap);
  return (
    <div className="line form">
      <select className="field" value={cap} onChange={(e) => setCap(e.target.value)}>
        {Object.entries(CAPS).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
      </select>
      {needsFolder && <input className="field grow" placeholder={t("Ordner, z. B. /home/ich/Bilder")} value={folder} onChange={(e) => setFolder(e.target.value)} />}
      <button
        className="btn primary"
        disabled={needsFolder && !folder.startsWith("/")}
        onClick={() => api3.pairGrant(device.node_id, device.name, cap, needsFolder ? folder : null).then(done).catch(fail(notify))}
      >
        Gewähren …
      </button>
    </div>
  );
}

// ================================================================ Sicherungen

export function Backups({ notify }: { notify: Notify }) {
  const [jobs, reload] = useLoader(() => api3.backups().then((r) => r.jobs), notify);
  const [creating, setCreating] = useState(false);
  const [recovery, setRecovery] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [snaps, setSnaps] = useState<Snapshot[]>([]);

  const show = (id: string) => {
    if (open === id) return setOpen(null);
    setOpen(id);
    api3.backupSnapshots(id).then((r) => setSnaps(r.items)).catch(fail(notify));
  };

  return (
    <div className="page">
      <h1>{t("Sicherungen")}</h1>
      {recovery && (
        <section className="panel warn">
          <h2>{t("Wiederherstellungsschlüssel – jetzt aufschreiben")}</h2>
          <p>{t("Mit Passwort oder diesem Schlüssel lässt sich die Sicherung auf einem neuen Gerät öffnen. Er wird nur dieses eine Mal angezeigt.")}</p>
          <div className="code selectable">{recovery}</div>
          <button className="btn" onClick={() => setRecovery(null)}>{t("Ich habe ihn sicher aufbewahrt")}</button>
        </section>
      )}
      <section className="panel">
        <div className="line">
          <h2 className="grow">{t("Eingerichtete Sicherungen")}</h2>
          <button className="btn primary" onClick={() => setCreating(!creating)}>{t("Neue Sicherung")}</button>
        </div>
        {creating && <BackupForm notify={notify} done={(key) => { setCreating(false); setRecovery(key); reload(); }} />}
        {jobs && jobs.length === 0 && !creating && <p className="muted">{t("Noch keine Sicherung eingerichtet. Empfohlen: externe Platte und Cloudflare R2.")}</p>}
        {(jobs ?? []).map((j: BackupJob) => (
          <div className="device" key={j.id}>
            <div className="line">
              <span className={`dot ${j.last_error ? "err" : j.last_ok ? "on" : ""}`} />
              <div className="grow">
                <div className="strong">{j.target.kind === "S3" ? "Cloudflare R2" : j.target.path}</div>
                <div className="muted small">
                  {j.sources.join(", ") || t("keine Ordner")} · {j.running ? t("läuft gerade …") : j.last_ok ? t("zuletzt erfolgreich {date}", { date: formatDate(j.last_ok) }) : t("noch nie gesichert")}
                </div>
                {j.last_error && <div className="errtext small">{j.last_error}</div>}
              </div>
              <button className="btn" onClick={() => show(j.id)}>{t("Versionen")}</button>
              <button
                className="btn"
                disabled={j.running}
                onClick={() => {
                  notify({ text: t("Sicherung läuft …") });
                  api3.backupRun(j.id)
                    .then((s) => notify({ text: t("Gesichert: {files} Dateien, {size} neu.", { files: s.files, size: humanSize(s.new_bytes) }) }))
                    .catch(fail(notify))
                    .finally(reload);
                }}
              >
                Jetzt sichern
              </button>
              <button className="btn" onClick={() => api3.backupRemove(j.id).then(reload).catch(fail(notify))}>{t("Entfernen …")}</button>
            </div>
            {open === j.id && (
              <table className="details small-table">
                <thead><tr><th>{t("Stand")}</th><th>{t("Dateien")}</th><th>{t("Größe")}</th><th>{t("Neu")}</th><th /></tr></thead>
                <tbody>
                  {snaps.map((s) => (
                    <tr className="row" key={s.id}>
                      <td>{formatDate(s.time)}</td>
                      <td>{s.files.toLocaleString(locale)}</td>
                      <td>{humanSize(s.bytes)}</td>
                      <td>{humanSize(s.new_bytes)}</td>
                      <td>
                        <button className="btn" onClick={() => restoreAll(j, s, notify)}>{t("Wiederherstellen")}</button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        ))}
      </section>
    </div>
  );
}

/** Ganzen Stand eines Quellordners nach „<Quelle> (wiederhergestellt)“ daneben zurückholen – nie überschreiben. */
function restoreAll(j: BackupJob, s: Snapshot, notify: Notify) {
  for (const src of s.sources) {
    const parent = src.path.slice(0, src.path.lastIndexOf("/")) || "/";
    api3.backupRestore(j.id, s.id, src.path, parent)
      .then((r) => notify({ text: t("{n} Dateien wiederhergestellt nach {path}", { n: r.files, path: r.path }) }))
      .catch(fail(notify));
  }
}

function BackupForm({ notify, done }: { notify: Notify; done: (recoveryKey: string) => void }) {
  const [kind, setKind] = useState<"dir" | "r2">("dir");
  const [target, setTarget] = useState("");
  const [sources, setSources] = useState("");
  const [pw, setPw] = useState("");
  const [pw2, setPw2] = useState("");
  const [busy, setBusy] = useState(false);
  const list = sources.split("\n").map((s) => s.trim()).filter(Boolean);
  const ok = list.length > 0 && list.every((s) => s.startsWith("/")) && pw.length >= 10 && pw === pw2 && (kind === "r2" || target.startsWith("/"));
  return (
    <div className="form panel inner">
      <div className="line">
        <label><input type="radio" checked={kind === "dir"} onChange={() => setKind("dir")} /> {t("Externe Platte / Ordner")}</label>
        <label><input type="radio" checked={kind === "r2"} onChange={() => setKind("r2")} /> {t("Cloudflare R2 (Zugang wie Freigabelinks)")}</label>
      </div>
      {kind === "dir" && <input className="field" placeholder={t("Ziel, z. B. /media/ich/Platte")} value={target} onChange={(e) => setTarget(e.target.value)} />}
      <textarea className="field" rows={3} placeholder={"Zu sichernde Ordner, einer pro Zeile\n/home/ich/Dokumente\n/home/ich/Bilder"} value={sources} onChange={(e) => setSources(e.target.value)} />
      <div className="line">
        <input className="field grow" type="password" placeholder={t("Passwort (mindestens 10 Zeichen)")} value={pw} onChange={(e) => setPw(e.target.value)} autoComplete="new-password" />
        <input className="field grow" type="password" placeholder={t("Passwort wiederholen")} value={pw2} onChange={(e) => setPw2(e.target.value)} autoComplete="new-password" />
      </div>
      {pw2 && pw !== pw2 && <div className="errtext small">{t("Die Passwörter stimmen nicht überein.")}</div>}
      <div className="line">
        <span className="muted small grow">{t("Verschlüsselt (AES-256-GCM) und dedupliziert. Stündlich automatisch; Aufbewahrung in den Einstellungen.")}</span>
        <button
          className="btn primary"
          disabled={!ok || busy}
          onClick={() => {
            setBusy(true);
            api3.backupCreate(kind === "r2" ? null : target, list, pw)
              .then((r) => { setPw(""); setPw2(""); done(r.recovery_key); })
              .catch(fail(notify))
              .finally(() => setBusy(false));
          }}
        >
          {busy ? t("Richte ein …") : t("Einrichten")}
        </button>
      </div>
    </div>
  );
}

// ================================================================ Einstellungen

export function SettingsPage({ notify }: { notify: Notify }) {
  const [items, reload] = useLoader(() => api3.settings().then((r) => r.items), notify);
  return (
    <div className="page">
      <h1>{t("Einstellungen")}</h1>
      <section className="panel">
        <h2>{t("Sprache")}</h2>
        <div className="line">
          <select className="field" value={lang} onChange={(e) => setLanguage(e.target.value)}>
            {Object.entries(LANGUAGES).map(([code, l]) => <option key={code} value={code}>{l.name}</option>)}
          </select>
          <span className="muted small grow">{t("Eigene Sprache hinzufügen: eine Datei in apps/desktop/locales/ ablegen – Anleitung in docs/TRANSLATING.md.")}</span>
        </div>
      </section>
      <FontPanel notify={notify} />
      <section className="panel">
        <h2>{t("Grenzen und Regeln")}</h2>
        <p className="muted">{t("Änderungen wirken sofort, ohne Neustart. „Standard“ setzt den Wert aus der Konfiguration zurück.")}</p>
        {(items ?? []).map((s: Setting) => (
          <SettingRow key={s.key} s={s} notify={notify} reload={reload} />
        ))}
      </section>
    </div>
  );
}

function SettingRow({ s, notify, reload }: { s: Setting; notify: Notify; reload: () => void }) {
  const [v, setV] = useState(String(s.value));
  useEffect(() => setV(String(s.value)), [s.value]);
  const n = Number(v);
  const valid = v.trim() !== "" && Number.isInteger(n) && n >= s.min && n <= s.max;
  const save = (value: number | null) => api3.settingSet(s.key, value).then(() => { notify({ text: t("Gespeichert.") }); reload(); }).catch(fail(notify));
  return (
    <div className="line setting">
      <div className="grow">
        <div className="strong">{t(s.help)}</div>
        <div className="muted small">{s.key} · {t("Standard")} {s.default} {t(s.unit)} · {s.min}–{s.max}</div>
      </div>
      <input className={`field num${valid ? "" : " invalid"}`} value={v} onChange={(e) => setV(e.target.value)} onKeyDown={(e) => e.key === "Enter" && valid && save(n)} />
      <span className="muted small unit">{t(s.unit)}</span>
      <button className="btn" disabled={!valid || n === s.value} onClick={() => save(n)}>{t("Speichern")}</button>
      <button className="btn" disabled={!s.overridden} onClick={() => save(null)}>{t("Standard")}</button>
    </div>
  );
}

function FontPanel({ notify }: { notify: Notify }) {
  const [name, setName] = useState<string | null>(null);
  useEffect(() => {
    api5.fontName().then(setName).catch(() => setName(""));
  }, []);
  const pick = () =>
    api5.fontPick().then(async (n) => { setName(n); await applyFont(); notify({ text: t("Schriftart „{name}“ wird verwendet.", { name: n }) }); }).catch(fail(notify));
  const reset = () => api5.fontReset().then(async () => { setName(""); await applyFont(); }).catch(fail(notify));
  return (
    <section className="panel">
      <h2>{t("Schriftart")}</h2>
      <div className="line">
        <span className="grow">{name ? name : t("Inconsolata (Standard)")}</span>
        <button className="btn" onClick={pick}>{t("Schriftdatei wählen …")}</button>
        <button className="btn" disabled={!name} onClick={reset}>{t("Zurück zu Inconsolata")}</button>
      </div>
      <p className="muted small">{t("TrueType- oder OpenType-Datei (.ttf, .otf), höchstens 16 MB.")}</p>
    </section>
  );
}
