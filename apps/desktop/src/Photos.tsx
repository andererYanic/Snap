import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, errorText, humanSize, thumbUrl, type MonthCount, type TimelineItem } from "./api";
import type { Toast } from "./App";

const MONTHS = ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"];

/** Aufnahmezeit ist lokale Uhrzeit ohne Zeitzone (als UTC gespeichert) → immer in UTC anzeigen. */
const monthKey = (t: number) => new Date(t * 1000).toISOString().slice(0, 7);
const monthLabel = (t: number) => {
  const d = new Date(t * 1000);
  return `${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
};
const dayLabel = (t: number) =>
  new Date(t * 1000).toLocaleDateString("de-DE", { weekday: "long", day: "numeric", month: "long", year: "numeric", timeZone: "UTC" });

export default function Photos({ notify }: { notify: (t: Toast) => void }) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [months, setMonths] = useState<MonthCount[]>([]);
  const [loading, setLoading] = useState(false);
  const [done, setDone] = useState(false);
  const [preview, setPreview] = useState<{ item: TimelineItem; video?: string } | null>(null);
  const scroller = useRef<HTMLDivElement>(null);

  const more = useCallback(
    async (cursor: string | null, reset = false) => {
      if (loading) return;
      setLoading(true);
      try {
        const p = await api.timeline(null, cursor);
        setItems((v) => (reset ? p.items : [...v, ...p.items]));
        setNext(p.next);
        setDone(!p.next);
      } catch (e) {
        notify({ text: errorText(e), error: true });
      } finally {
        setLoading(false);
      }
    },
    [loading, notify],
  );

  useEffect(() => {
    more(null, true);
    api.timelineMonths(null).then((m) => setMonths(m.months)).catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const onScroll = () => {
    const el = scroller.current;
    if (el && !done && !loading && el.scrollTop + el.clientHeight > el.scrollHeight - 1500) more(next);
  };

  const days = useMemo(() => {
    const out: { key: string; label: string; items: TimelineItem[] }[] = [];
    for (const it of items) {
      const k = monthKey(it.taken);
      if (out.length === 0 || out[out.length - 1].key !== k) out.push({ key: k, label: monthLabel(it.taken), items: [] });
      out[out.length - 1].items.push(it);
    }
    return out;
  }, [items]);

  /** Zu einem Monat springen: nachladen, bis er erreicht ist. */
  const jump = async (m: MonthCount) => {
    const key = `${m.year}-${String(m.month).padStart(2, "0")}`;
    const find = () => scroller.current?.querySelector<HTMLElement>(`[data-month="${key}"]`);
    let cursor = next;
    let guard = 0;
    while (!find() && cursor && guard++ < 50) {
      const p = await api.timeline(null, cursor);
      setItems((v) => [...v, ...p.items]);
      cursor = p.next;
      setNext(p.next);
      setDone(!p.next);
      await new Promise((r) => setTimeout(r, 30));
    }
    find()?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  const open = (it: TimelineItem) => {
    if (it.video) api.streamUrl(it.path).then((s) => setPreview({ item: it, video: s.url })).catch((e) => notify({ text: errorText(e), error: true }));
    else setPreview({ item: it });
  };

  let lastYear = 0;
  return (
    <div className="photos">
      <div className="photoscroll" ref={scroller} onScroll={onScroll}>
        {days.length === 0 && !loading ? (
          <div className="empty">
            <div>
              <div className="big">Noch keine Fotos</div>
              Bilder und Videos aus deinen indexierten Ordnern erscheinen hier, sortiert nach Aufnahmedatum.
            </div>
          </div>
        ) : (
          days.map((d) => (
            <section key={d.key} data-month={d.key}>
              <div className="dayhead">{d.label}</div>
              <div className="photogrid">
                {d.items.map((it) => (
                  <div key={it.path} className="photo" onDoubleClick={() => open(it)} onClick={() => open(it)} title={`${dayLabel(it.taken)}\n${it.path}`}>
                    <img src={thumbUrl(it.path)} loading="lazy" alt="" draggable={false} onError={(e) => (e.currentTarget.style.visibility = "hidden")} />
                    {it.video && <span className="badge">▶ Video</span>}
                  </div>
                ))}
              </div>
            </section>
          ))
        )}
      </div>
      {months.length > 0 && (
        <div className="months">
          {months.map((m) => {
            const head = m.year !== lastYear;
            lastYear = m.year;
            return (
              <div key={`${m.year}-${m.month}`}>
                {head && <div className="y">{m.year}</div>}
                <button onClick={() => jump(m)} title={`${m.count} Fotos/Videos`}>{MONTHS[m.month - 1].slice(0, 3)}</button>
              </div>
            );
          })}
        </div>
      )}
      {preview && createPortal(
        <div className="overlay" onClick={() => setPreview(null)} onKeyDown={(e) => e.key === "Escape" && setPreview(null)} tabIndex={-1} ref={(el) => el?.focus()}>
          {preview.video ? (
            <video src={preview.video} controls autoPlay onClick={(e) => e.stopPropagation()} />
          ) : (
            <img src={thumbUrl(preview.item.path, true)} alt="" />
          )}
          <div className="cap">
            {dayLabel(preview.item.taken)} · {humanSize(preview.item.size)}
            {preview.item.camera ? ` · ${preview.item.camera}` : ""}
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}
