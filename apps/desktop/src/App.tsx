import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, baseName, errorText, onDaemonEvent, onDaemonStatus, type DaemonEvent, type Place } from "./api";
import { Icon } from "./icons";
import Explorer from "./Explorer";
import Photos from "./Photos";

/** Ein Tab: Ordner (mit Verlauf), Suche, Fotos oder Papierkorb. */
export interface Tab {
  id: number;
  kind: "folder" | "photos" | "trash";
  path: string;
  history: string[];
  index: number;
}

export interface Clipboard {
  mode: "copy" | "cut";
  paths: string[];
}

export interface Toast {
  text: string;
  error?: boolean;
}

let nextId = 1;
export const newTab = (kind: Tab["kind"], path: string): Tab => ({ id: nextId++, kind, path, history: [path], index: 0 });

function tabTitle(t: Tab, home: string): string {
  if (t.kind === "photos") return "Fotos";
  if (t.kind === "trash") return "Papierkorb";
  if (t.path === home) return "Persönlicher Ordner";
  return baseName(t.path);
}

export default function App() {
  const [up, setUp] = useState<boolean | null>(null);
  const [places, setPlaces] = useState<Place[]>([]);
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [active, setActive] = useState(0);
  const [clip, setClip] = useState<Clipboard | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [events, setEvents] = useState<DaemonEvent | null>(null);
  const [starting, setStarting] = useState(false);
  const toastTimer = useRef<number>(0);
  const home = places.find((p) => p.kind === "home")?.path ?? "/";

  const notify = useCallback((t: Toast) => {
    setToast(t);
    window.clearTimeout(toastTimer.current);
    toastTimer.current = window.setTimeout(() => setToast(null), t.error ? 7000 : 3500);
  }, []);

  // Dienststatus und Ereignisse
  useEffect(() => {
    const subs = [onDaemonStatus(setUp), onDaemonEvent(setEvents)];
    api.places().then(setPlaces).catch(() => {});
    api.places()
      .then(() => api.listDir("/", false))
      .then(() => setUp(true))
      .catch((e) => setUp(e?.code === "DaemonDown" ? false : true));
    return () => subs.forEach((p) => p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (tabs.length === 0 && places.length > 0) setTabs([newTab("folder", home)]);
  }, [places, home, tabs.length]);

  const tab = tabs[Math.min(active, tabs.length - 1)];

  const updateTab = useCallback((id: number, f: (t: Tab) => Tab) => {
    setTabs((ts) => ts.map((t) => (t.id === id ? f(t) : t)));
  }, []);

  /** Navigation im aktuellen Tab (mit Verlauf). */
  const navigate = useCallback(
    (kind: Tab["kind"], path: string) => {
      if (!tab) return;
      updateTab(tab.id, (t) => {
        if (t.kind === kind && t.path === path) return t;
        const history = [...t.history.slice(0, t.index + 1), path];
        return { ...t, kind, path, history, index: history.length - 1 };
      });
    },
    [tab, updateTab],
  );

  const openTab = useCallback((kind: Tab["kind"], path: string) => {
    setTabs((ts) => {
      setActive(ts.length);
      return [...ts, newTab(kind, path)];
    });
  }, []);

  const closeTab = useCallback(
    (i: number) => {
      setTabs((ts) => {
        if (ts.length === 1) {
          getCurrentWindow().close();
          return ts;
        }
        const n = ts.filter((_, j) => j !== i);
        setActive((a) => Math.max(0, a > i || a === n.length ? a - 1 : a));
        return n;
      });
    },
    [],
  );

  // Tastenkürzel für Tabs
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.key.toLowerCase() === "t") {
        e.preventDefault();
        openTab("folder", tab?.kind === "folder" ? tab.path : home);
      } else if (e.ctrlKey && e.key.toLowerCase() === "w") {
        e.preventDefault();
        closeTab(active);
      } else if (e.ctrlKey && e.key === "Tab") {
        e.preventDefault();
        setActive((a) => (a + (e.shiftKey ? tabs.length - 1 : 1)) % tabs.length);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openTab, closeTab, active, tab, home, tabs.length]);

  const win = getCurrentWindow();

  if (up === false) {
    return (
      <div className="app">
        <TitleBar win={win}>{null}</TitleBar>
        <div className="body center">
          <div className="card">
            <Icon.drive size={36} />
            <h2>Der Snap-Dienst läuft nicht</h2>
            <p>
              Der Dienst hält Index, Sicherungen und Verbindungen zu deinen Geräten im Hintergrund aktuell –
              auch wenn dieses Fenster geschlossen ist.
            </p>
            <button
              className="btn primary"
              disabled={starting}
              onClick={() => {
                setStarting(true);
                api.startDaemon()
                  .then(() => setUp(true))
                  .catch((e) => notify({ text: errorText(e), error: true }))
                  .finally(() => setStarting(false));
              }}
            >
              {starting ? "Startet …" : "Dienst starten"}
            </button>
          </div>
        </div>
        {toast && <div className={`toast${toast.error ? " err" : ""}`}>{toast.text}</div>}
      </div>
    );
  }

  return (
    <div className="app">
      <TitleBar win={win}>
        <div className="tabs">
          {tabs.map((t, i) => (
            <div
              key={t.id}
              className={`tab${i === active ? " active" : ""}`}
              onMouseDown={(e) => {
                if (e.button === 1) closeTab(i);
                else setActive(i);
              }}
              title={t.kind === "folder" ? t.path : undefined}
            >
              {t.kind === "photos" ? <Icon.photos /> : t.kind === "trash" ? <Icon.bin /> : <Icon.home />}
              <span className="label">{tabTitle(t, home)}</span>
              <button
                className="close"
                onMouseDown={(e) => e.stopPropagation()}
                onClick={() => closeTab(i)}
                aria-label="Tab schließen"
              >
                <Icon.close size={12} />
              </button>
            </div>
          ))}
        </div>
        <button className="newtab" onClick={() => openTab("folder", home)} title="Neuer Tab (Strg+T)">
          <Icon.plus />
        </button>
      </TitleBar>
      <div className="body">
        <Sidebar places={places} tab={tab} onGo={navigate} />
        <div className="main">
          {tab?.kind === "photos" ? (
            <Photos notify={notify} />
          ) : tab ? (
            <Explorer
              key={tab.id}
              tab={tab}
              home={home}
              event={events}
              clip={clip}
              setClip={setClip}
              notify={notify}
              navigate={navigate}
              openTab={openTab}
              go={(delta) =>
                updateTab(tab.id, (t) => {
                  const index = Math.max(0, Math.min(t.history.length - 1, t.index + delta));
                  const path = t.history[index];
                  return { ...t, index, path, kind: path === "trash:" ? "trash" : path === "photos:" ? "photos" : "folder" };
                })
              }
            />
          ) : null}
        </div>
      </div>
      {toast && <div className={`toast${toast.error ? " err" : ""}`}>{toast.text}</div>}
    </div>
  );
}

function TitleBar({ win, children }: { win: ReturnType<typeof getCurrentWindow>; children: React.ReactNode }) {
  return (
    <div className="titlebar" onDoubleClick={(e) => e.target === e.currentTarget && win.toggleMaximize()}>
      {children}
      <div className="drag" data-tauri-drag-region onDoubleClick={() => win.toggleMaximize()} />
      <div className="winbtns">
        <button onClick={() => win.minimize()} aria-label="Minimieren"><Icon.min /></button>
        <button onClick={() => win.toggleMaximize()} aria-label="Maximieren"><Icon.max size={14} /></button>
        <button className="closewin" onClick={() => win.close()} aria-label="Schließen"><Icon.close /></button>
      </div>
    </div>
  );
}

function Sidebar({ places, tab, onGo }: { places: Place[]; tab?: Tab; onGo: (k: Tab["kind"], p: string) => void }) {
  const is = (kind: Tab["kind"], path: string) => tab?.kind === kind && (kind !== "folder" || tab.path === path);
  return (
    <nav className="sidebar">
      {places.map((p) => {
        const I = Icon[p.kind] ?? Icon.home;
        return (
          <button key={p.path} className={`navitem${is("folder", p.path) ? " active" : ""}`} onClick={() => onGo("folder", p.path)}>
            <I size={16} />
            <span>{p.name}</span>
          </button>
        );
      })}
      <h4>Snap</h4>
      <button className={`navitem${is("photos", "") ? " active" : ""}`} onClick={() => onGo("photos", "photos:")}>
        <Icon.photos size={16} />
        <span>Fotos</span>
      </button>
      <button className={`navitem${is("trash", "") ? " active" : ""}`} onClick={() => onGo("trash", "trash:")}>
        <Icon.bin size={16} />
        <span>Papierkorb</span>
      </button>
      <h4>Dieser Computer</h4>
      <button className={`navitem${is("folder", "/") ? " active" : ""}`} onClick={() => onGo("folder", "/")}>
        <Icon.drive size={16} />
        <span>Dateisystem</span>
      </button>
    </nav>
  );
}
