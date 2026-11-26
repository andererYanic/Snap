// Snap Drop am Desktop: Sichtbarkeit, Kontakte, Senden an Geräte in der Nähe und Annahme
// eingehender Angebote. Senden bestätigt der Nutzer zusätzlich im nativen Dialog (commands.rs).
// Entwurf und Risiken: docs/DROP.md.
import { t } from "./i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { baseName, drop, errorText, formatDate, humanSize, isCancelled, onDaemonEvent, type DropBlocked, type DropContact, type DropMode, type DropStatus, type IncomingOffer, type NearbyPeer } from "./api";
import type { Toast } from "./App";
import { Icon } from "./icons";

type Notify = (t: Toast) => void;
const fail = (notify: Notify) => (e: unknown) => {
  if (!isCancelled(e)) notify({ text: errorText(e), error: true });
};
const filesLabel = (n: number) => (n === 1 ? t("1 Datei") : t("{n} Dateien", { n }));

export function Avatar({ name, contact, size = 36 }: { name: string; contact?: boolean; size?: number }) {
  return (
    <span className={`avatar${contact ? " contact" : ""}`} style={{ width: size, height: size, fontSize: size * 0.42 }} aria-hidden="true">
      {(name.trim()[0] ?? "?").toUpperCase()}
    </span>
  );
}

// ================================================================ Seite

export function DropPage({ notify }: { notify: Notify }) {
  const [st, setSt] = useState<DropStatus | null>(null);
  const [people, setPeople] = useState<DropContact[]>([]);
  const [blocked, setBlocked] = useState<DropBlocked[]>([]);
  const [now, setNow] = useState(Date.now() / 1000);

  const load = useCallback(() => {
    drop.status().then(setSt).catch(fail(notify));
    drop.contacts().then(setPeople).catch(() => {});
    drop.blocked().then(setBlocked).catch(() => {});
  }, [notify]);

  useEffect(() => {
    load();
    const sub = onDaemonEvent((ev) => {
      if (ev.type === "DropVisibilityChanged" || (ev.type === "DropFinished" && ev.ok)) load();
    });
    const tick = window.setInterval(() => setNow(Date.now() / 1000), 15000);
    return () => {
      sub.then((f) => f());
      window.clearInterval(tick);
    };
  }, [load]);

  const choose = (mode: DropMode) => drop.setVisibility(mode).then(setSt).catch(fail(notify));
  const minutes = st?.until ? Math.max(1, Math.ceil((st.until - now) / 60)) : 0;
  const explain = !st
    ? ""
    : st.mode === "off"
      ? t("Niemand sieht dieses Gerät. Senden geht trotzdem.")
      : st.mode === "contacts"
        ? t("Nur Kontakte sehen dich – wenn sie dich ebenfalls gespeichert haben.")
        : t("Alle in der Nähe sehen „{name}“ – noch {min} Min.", { name: st.name, min: minutes });

  return (
    <div className="page">
      <h1>{t("Snap Drop")}</h1>
      <p className="muted">{t("Dateien direkt an Menschen in der Nähe senden – ohne Cloud, Ende-zu-Ende-verschlüsselt. Rechtsklick auf Dateien → „In der Nähe senden …“.")}</p>

      <section className="panel">
        <h2>{t("Sichtbarkeit")}</h2>
        <div className="seg" role="radiogroup">
          {(["off", "contacts", "everyone"] as DropMode[]).map((m) => (
            <button key={m} role="radio" aria-checked={st?.mode === m} className={st?.mode === m ? "on" : ""} onClick={() => choose(m)}>
              {m === "off" ? t("Aus") : m === "contacts" ? t("Kontakte") : t("Alle")}
            </button>
          ))}
        </div>
        <p className="muted small explain">{explain}</p>
        {st?.code && (
          <div className="line">
            <span className="muted small grow">{t("Für Geräte, die nicht in der Nähe sind")}</span>
            <button
              className="btn"
              onClick={() => navigator.clipboard.writeText(st.code!).then(() => notify({ text: t("Code kopiert.") }), fail(notify))}
            >
              <Icon.copy /> {t("Code kopieren")}
            </button>
          </div>
        )}
      </section>

      <section className="panel">
        <h2>{t("Dieses Gerät")}</h2>
        <div className="line">
          <div className="grow">
            <div className="strong">{st?.name}</div>
            <div className="muted small">{t("Fingerabdruck {fp}", { fp: st?.fingerprint ?? "" })}</div>
          </div>
        </div>
        <div className="muted small ablage">{t("Ablage")}: {st?.dir}</div>
      </section>

      <section className="panel">
        <h2>{t("Kontakte")}</h2>
        {people.length === 0 && (
          <p className="muted small">{t("Nach einem Tausch kannst du das Gegenüber als Kontakt speichern. Kontakte erkennt Snap am Schlüssel, nicht am Namen.")}</p>
        )}
        {people.map((c) => (
          <div key={c.key} className="line person">
            <Avatar name={c.name} contact />
            <div className="grow">
              <div className="strong">{c.name}</div>
              <div className="muted small mono">{c.fingerprint}</div>
            </div>
            <button className="btn" onClick={() => drop.removeContact(c.key).then(() => { notify({ text: t("Kontakt entfernt.") }); load(); }).catch(fail(notify))}>
              {t("Entfernen")}
            </button>
          </div>
        ))}
      </section>

      {blocked.length > 0 && (
        <section className="panel">
          <h2>{t("Blockiert")}</h2>
          {blocked.map((b) => (
            <div key={b.key} className="line person">
              <div className="grow">
                <div className="strong">{b.name}</div>
                <div className="muted small">{t("seit {date}", { date: formatDate(b.blocked_at) })}</div>
              </div>
              <button className="btn" onClick={() => drop.unblock(b.key).then(load).catch(fail(notify))}>{t("Freigeben")}</button>
            </div>
          ))}
        </section>
      )}

      <p className="muted small">
        {t("Übertragungen sind Ende-zu-Ende-verschlüsselt. Über Bluetooth gehen nur Verbindungsdaten, nie Dateien. Nichts wird ohne deine Zustimmung gespeichert.")}
      </p>
    </div>
  );
}

// ================================================================ Senden

export function DropSendDialog({ paths, notify, onClose }: { paths: string[]; notify: Notify; onClose: () => void }) {
  const [peers, setPeers] = useState<NearbyPeer[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [code, setCode] = useState("");

  useEffect(() => {
    let alive = true;
    const refresh = () => drop.nearby().then((p) => alive && setPeers(p.filter((x) => !x.offline))).catch(() => {});
    drop.browse(true).catch(() => {});
    refresh();
    const timer = window.setInterval(refresh, 1500);
    const sub = onDaemonEvent((ev) => {
      if (ev.type === "DropNearbyChanged") refresh();
      if (ev.type === "DropProgress" && !ev.incoming) setProgress({ done: Number(ev.done_bytes), total: Number(ev.total_bytes) });
    });
    return () => {
      alive = false;
      window.clearInterval(timer);
      sub.then((f) => f());
      drop.browse(false).catch(() => {});
    };
  }, []);

  const send = (p: NearbyPeer) => {
    setBusy(p.id);
    setProgress(null);
    drop
      .send(p.id, paths)
      .then((r) => {
        if (r.ok) onClose();
        else notify({ text: r.error ?? t("Fehlgeschlagen"), error: true });
      })
      .catch(fail(notify))
      .finally(() => setBusy(null));
  };

  const connect = () =>
    drop
      .addCode(code.trim())
      .then((p) => { setCode(""); setPeers((old) => [p, ...old.filter((x) => x.id !== p.id)]); })
      .catch(fail(notify));

  return (
    <div className="overlay dim" onClick={() => !busy && onClose()} onKeyDown={(e) => e.key === "Escape" && !busy && onClose()} tabIndex={-1}>
      <div className="dialog drop" onClick={(e) => e.stopPropagation()}>
        <h2>{t("In der Nähe senden")}</h2>
        <p className="muted small">{paths.length === 1 ? baseName(paths[0]) : filesLabel(paths.length)}</p>
        <div className="peers">
          {peers.length === 0 ? (
            <div className="empty">
              <Icon.nearby size={28} />
              <div className="strong">{t("Suche nach Geräten …")}</div>
              <div className="muted small">{t("Auf dem anderen Gerät Snap öffnen und unter Einstellungen › Snap Drop „Alle“ wählen.")}</div>
            </div>
          ) : (
            peers.map((p) => {
              const name = p.contact ?? p.name ?? t("Snap-Gerät");
              const mine = busy === p.id;
              return (
                <button key={p.id} className="peer" disabled={!!busy} onClick={() => send(p)} aria-label={t("An {name} senden", { name })}>
                  <Avatar name={name} contact={!!p.contact} size={40} />
                  <span className="grow">
                    <span className="strong">{name}</span>
                    <span className="muted small">
                      {mine
                        ? progress && progress.total > 0
                          ? `${humanSize(progress.done)} / ${humanSize(progress.total)}`
                          : t("Wartet auf Annahme …")
                        : p.contact
                          ? t("Kontakt")
                          : t("In der Nähe")}
                    </span>
                    {mine && progress && progress.total > 0 && (
                      <span className="progress"><span style={{ width: `${Math.round((progress.done / progress.total) * 100)}%` }} /></span>
                    )}
                  </span>
                </button>
              );
            })
          )}
        </div>
        <div className="line">
          <input className="field grow" placeholder={t("Code (snapdrop1-…)")} value={code} onChange={(e) => setCode(e.target.value)} onKeyDown={(e) => e.key === "Enter" && code.trim() && connect()} />
          <button className="btn" disabled={!code.trim()} onClick={connect}>{t("Verbinden")}</button>
          <button className="btn" disabled={!!busy} onClick={onClose}>{t("Schließen")}</button>
        </div>
      </div>
    </div>
  );
}

// ================================================================ Empfangen

export function OfferDialog({ offer, onDone }: { offer: IncomingOffer; onDone: () => void }) {
  const [png, setPng] = useState<string | null>(null);
  const [reveal, setReveal] = useState(false);
  const answered = useRef(false);
  useEffect(() => {
    if (offer.preview) drop.preview(offer.id).then(setPng).catch(() => {});
  }, [offer.id, offer.preview]);
  const answer = (accept: boolean, block = false) => {
    if (answered.current) return;
    answered.current = true;
    drop.respond(offer.id, accept, block).catch(() => {});
    onDone();
  };
  const name = offer.contact ?? offer.name;
  const shown = offer.files.slice(0, 4);
  return (
    <div className="overlay dim" onKeyDown={(e) => e.key === "Escape" && answer(false)} tabIndex={-1}>
      <div className="dialog drop" role="alertdialog" aria-label={t("Snap Drop")}>
        <div className="line offerhead">
          <Avatar name={name} contact={!!offer.contact} size={48} />
          <div className="grow">
            <div className="offername">{name}</div>
            <div className={`small ${offer.contact ? "accent" : "muted"}`}>
              {offer.contact ? t("Kontakt") : t("Unbekannt – nur annehmen, wenn du die Person kennst.")}
            </div>
          </div>
          {png && (
            <button className="previewbtn" onClick={() => setReveal(true)} aria-label={t("Vorschau anzeigen")}>
              <img src={`data:image/png;base64,${png}`} alt="" className={reveal ? "" : "blur"} />
            </button>
          )}
        </div>
        <p>{t("{name} möchte dir {files} senden ({size}).", { name, files: filesLabel(offer.files.length), size: humanSize(offer.total) })}</p>
        <div className="offerfiles">
          {shown.map((f, i) => (
            <div key={i} className="line">
              <span className="grow ellipsis">{f.name}</span>
              <span className="muted small num">{humanSize(f.size)}</span>
            </div>
          ))}
          {offer.files.length > shown.length && <div className="muted small">{t("und {n} weitere", { n: offer.files.length - shown.length })}</div>}
        </div>
        <div className="muted small">{t("Prüfcode")} <span className="checkcode">{offer.code}</span></div>
        <div className="line">
          {!offer.contact && <button className="btn link danger" onClick={() => answer(false, true)}>{t("Ablehnen und blockieren")}</button>}
          <span className="grow" />
          <button className="btn" onClick={() => answer(false)}>{t("Ablehnen")}</button>
          <button className="btn primary" autoFocus onClick={() => answer(true)}>{t("Annehmen")}</button>
        </div>
      </div>
    </div>
  );
}
