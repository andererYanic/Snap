// Tresor (Desktop): Einrichten mit einmaligem Wiederherstellungscode, Entsperren, Inhalt,
// Aufnehmen (nativer Dialog), Öffnen/Herausholen/Löschen. Der Dienst sperrt nach 5 Minuten.
import { useCallback, useEffect, useState } from "react";
import { t } from "./i18n";
import { errorText, humanSize, isCancelled, onDaemonEvent, vault, type VaultEntry, type VaultStatus } from "./api";
import type { Toast } from "./App";

type Notify = (t: Toast) => void;

export function VaultPage({ notify }: { notify: Notify }) {
  const [st, setSt] = useState<VaultStatus | null>(null);
  const [list, setList] = useState<VaultEntry[]>([]);
  const [pw, setPw] = useState("");
  const [pw2, setPw2] = useState("");
  const [code, setCode] = useState<string | null>(null);
  const [recover, setRecover] = useState(false);
  const [rec, setRec] = useState("");
  const [busy, setBusy] = useState(false);
  const fail = (e: unknown) => {
    if (!isCancelled(e)) notify({ text: errorText(e), error: true });
  };
  const load = useCallback(async () => {
    try {
      const s = await vault.status();
      setSt(s);
      setList(s.unlocked ? await vault.list() : []);
    } catch (e) {
      fail(e);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    load();
    const sub = onDaemonEvent((ev) => ev.type === "VaultLocked" && load());
    return () => {
      sub.then((f) => f());
    };
  }, [load]);
  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
      setPw("");
      setPw2("");
    }
  };

  if (!st) return <section className="panel muted">…</section>;
  if (code) {
    return (
      <section className="panel">
        <h2>{t("Wiederherstellungscode")}</h2>
        <p className="strong mono">{code}</p>
        <p className="muted">{t("Schreib ihn jetzt auf und bewahre ihn sicher auf. Er wird nie wieder angezeigt. Ohne Passwort und ohne diesen Code ist der Inhalt des Tresors verloren.")}</p>
        <button className="btn primary" onClick={() => setCode(null)}>{t("Ich habe ihn aufgeschrieben")}</button>
      </section>
    );
  }
  if (!st.exists || recover) {
    const same = pw === pw2 && pw.length >= 8;
    return (
      <section className="panel">
        <h2>{recover ? t("Neues Passwort setzen") : t("Tresor einrichten")}</h2>
        <p className="muted">
          {recover
            ? t("Mit dem Wiederherstellungscode, den du beim Einrichten aufgeschrieben hast.")
            : t("Dateien im Tresor sind mit deinem Passwort verschlüsselt – auch Namen. Sie werden nie freigegeben, abgeglichen oder durchsucht.")}
        </p>
        <div className="col">
          {recover && <input className="field" value={rec} onChange={(e) => setRec(e.target.value)} placeholder={t("Wiederherstellungscode")} autoComplete="off" />}
          <input className="field" type="password" value={pw} onChange={(e) => setPw(e.target.value)} placeholder={t("Passwort (mind. 8 Zeichen)")} autoComplete="new-password" />
          <input className="field" type="password" value={pw2} onChange={(e) => setPw2(e.target.value)} placeholder={t("Passwort wiederholen")} autoComplete="new-password" />
          <div className="line">
            <button
              className="btn primary"
              disabled={busy || !same}
              onClick={() =>
                run(async () => {
                  if (recover) {
                    await vault.recover(rec, pw);
                    setRecover(false);
                    setRec("");
                  } else {
                    setCode((await vault.create(pw)).recovery_code);
                  }
                  await load();
                })
              }
            >
              {recover ? t("Passwort setzen") : t("Einrichten")}
            </button>
            {recover && <button className="btn" onClick={() => setRecover(false)}>{t("Abbrechen")}</button>}
          </div>
        </div>
      </section>
    );
  }
  if (!st.unlocked) {
    return (
      <section className="panel">
        <h2>{t("Tresor gesperrt")}</h2>
        {st.retry_after_secs > 0 && <p className="muted">{t("Zu viele falsche Versuche – bitte {n} s warten.", { n: st.retry_after_secs })}</p>}
        <form
          className="line"
          onSubmit={(e) => {
            e.preventDefault();
            run(async () => { await vault.unlock(pw); await load(); });
          }}
        >
          <input className="field grow" type="password" value={pw} onChange={(e) => setPw(e.target.value)} placeholder={t("Passwort")} autoFocus autoComplete="current-password" />
          <button className="btn primary" disabled={busy || !pw}>{t("Entsperren")}</button>
        </form>
        <button className="btn" onClick={() => setRecover(true)}>{t("Passwort vergessen?")}</button>
      </section>
    );
  }
  return (
    <section className="panel">
      <div className="line">
        <h2 className="grow">{t("Tresor")}</h2>
        <button className="btn" disabled={busy} onClick={() => run(async () => { const r = await vault.add(); notify({ text: r.message }); await load(); })}>{t("Dateien aufnehmen …")}</button>
        <button className="btn" onClick={() => vault.lock().then(load).catch(fail)}>{t("Sperren")}</button>
      </div>
      <p className="muted small">{t("Entsperrt · sperrt nach 5 Minuten ohne Benutzung")}</p>
      {list.length === 0 && <p className="muted">{t("Noch leer.")}</p>}
      {list.map((e) => (
        <div className="line" key={e.id}>
          <span className="grow">{e.name}</span>
          <span className="muted small">{humanSize(e.size)}</span>
          <button className="btn" onClick={() => vault.open(e.id, e.name).catch(fail)}>{t("Öffnen")}</button>
          <button className="btn" onClick={() => vault.export(e.id).then((r) => notify({ text: t("Unverschlüsselt gespeichert: {path}", { path: r.path }) })).catch(fail)}>{t("In „Download“ holen")}</button>
          <button className="btn" onClick={() => vault.remove(e.id, e.name).then(load).catch(fail)}>{t("Löschen")}</button>
        </div>
      ))}
    </section>
  );
}
