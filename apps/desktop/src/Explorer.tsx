import { locale, t } from "./i18n";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  api, baseName, errorText, formatDate, humanSize, isCancelled, isImage, isVideo, joinPath, parentOf, thumbUrl,
  type DaemonEvent, type DirEntry, type TrashEntry,
} from "./api";
import type { Clipboard, Tab, Toast } from "./App";
import { FileGlyph, Icon } from "./icons";

interface Item {
  path: string;
  name: string;
  dir: boolean;
  size: number;
  mtime: number;
  hidden: boolean;
  /** Nur im Papierkorb. */
  trash?: TrashEntry;
}

type SortKey = "name" | "mtime" | "size";

interface Props {
  tab: Tab;
  home: string;
  event: DaemonEvent | null;
  clip: Clipboard | null;
  setClip: (c: Clipboard | null) => void;
  notify: (t: Toast) => void;
  navigate: (kind: Tab["kind"], path: string) => void;
  openTab: (kind: Tab["kind"], path: string) => void;
  go: (delta: number) => void;
}

const fromEntry = (dir: string, e: DirEntry): Item => ({
  path: joinPath(dir, e.name), name: e.name, dir: e.kind === "Dir", size: e.size, mtime: e.mtime, hidden: e.hidden,
});

export default function Explorer({ tab, home, event, clip, setClip, notify, navigate, openTab, go }: Props) {
  const isTrash = tab.kind === "trash";
  const [items, setItems] = useState<Item[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [anchor, setAnchor] = useState<string | null>(null);
  const [view, setView] = useState<"details" | "grid">(() => (localStorage.getItem("view") as "grid") ?? "details");
  const [sort, setSort] = useState<{ key: SortKey; asc: boolean }>({ key: "name", asc: true });
  const [showHidden, setShowHidden] = useState(() => localStorage.getItem("hidden") === "1");
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; on: Item | null } | null>(null);
  const [editAddress, setEditAddress] = useState(false);
  const [progress, setProgress] = useState<{ job: number; pct: number } | null>(null);
  const [preview, setPreview] = useState<{ item: Item; video?: string } | null>(null);
  const [dropOver, setDropOver] = useState(false);
  const viewRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const fail = useCallback((e: unknown) => {
    if (!isCancelled(e)) notify({ text: errorText(e), error: true });
  }, [notify]);

  // ------------------------------------------------------------ Laden
  const generation = useRef(0);
  const load = useCallback(async () => {
    const gen = ++generation.current;
    const current = () => gen === generation.current;
    setError(null);
    try {
      if (searching !== null) {
        const r = await api.search(searching, showHidden);
        if (!current()) return;
        setItems(r.hits.map((h) => ({ path: h.path, name: h.name, dir: h.kind === "Dir", size: h.size, mtime: h.mtime, hidden: h.name.startsWith(".") })));
      } else if (isTrash) {
        const r = await api.trashList();
        if (!current()) return;
        setItems(r.entries.map((t) => ({
          path: t.id, name: t.name, dir: t.is_dir, size: t.size, mtime: Date.parse(t.deleted_at) / 1000 || 0, hidden: false, trash: t,
        })));
      } else {
        // Seitenweise (höchstens 1 000 je Anfrage): erste Seite sofort zeigen, Rest anhängen.
        const first = await api.listDir(tab.path, showHidden);
        if (!current()) return;
        let all = first.entries.map((e) => fromEntry(first.path, e));
        setItems(all);
        setLoading(false);
        while (all.length < first.total && first.entries.length > 0) {
          const r = await api.listDir(tab.path, showHidden, all.length);
          if (!current() || r.entries.length === 0) break;
          all = [...all, ...r.entries.map((e) => fromEntry(r.path, e))];
          setItems(all);
        }
      }
    } catch (e) {
      if (!current()) return;
      setItems([]);
      setError(errorText(e));
    } finally {
      if (current()) setLoading(false);
    }
  }, [tab.path, isTrash, showHidden, searching]);

  useEffect(() => {
    setLoading(true);
    setSel(new Set());
    load();
  }, [load]);

  useEffect(() => {
    setSearching(null);
    setQuery("");
  }, [tab.path]);

  // Abgeschlossene Dateioperationen → neu laden
  useEffect(() => {
    if (!event) return;
    if (event.type === "FileOpProgress") {
      const e = event as Extract<DaemonEvent, { type: "FileOpProgress" }>;
      const pct = e.total_bytes ? e.done_bytes / e.total_bytes : e.total_files ? e.done_files / e.total_files : 0;
      setProgress({ job: e.job, pct });
    } else if (event.type === "FileOpFinished") {
      const e = event as Extract<DaemonEvent, { type: "FileOpFinished" }>;
      setProgress(null);
      if (!e.ok) notify({ text: e.message, error: true });
      load();
    }
  }, [event, load, notify]);

  // ------------------------------------------------------------ Sortierung, Auswahl
  const sorted = useMemo(() => {
    const coll = new Intl.Collator(locale, { numeric: true, sensitivity: "base" });
    const v = [...items];
    v.sort((a, b) => {
      if (a.dir !== b.dir) return a.dir ? -1 : 1;
      const d = sort.key === "name" ? coll.compare(a.name, b.name) : sort.key === "size" ? a.size - b.size : a.mtime - b.mtime;
      return sort.asc ? d : -d;
    });
    return v;
  }, [items, sort]);

  const selected = useMemo(() => sorted.filter((i) => sel.has(i.path)), [sorted, sel]);

  const click = (e: React.MouseEvent, it: Item) => {
    viewRef.current?.focus();
    if (e.shiftKey && anchor) {
      const a = sorted.findIndex((i) => i.path === anchor);
      const b = sorted.findIndex((i) => i.path === it.path);
      const [lo, hi] = a < b ? [a, b] : [b, a];
      setSel(new Set(sorted.slice(lo, hi + 1).map((i) => i.path)));
    } else if (e.ctrlKey) {
      const n = new Set(sel);
      if (n.has(it.path)) n.delete(it.path);
      else n.add(it.path);
      setSel(n);
      setAnchor(it.path);
    } else {
      setSel(new Set([it.path]));
      setAnchor(it.path);
    }
  };

  // ------------------------------------------------------------ Aktionen
  const open = (it: Item) => {
    if (it.trash) return;
    if (it.dir) navigate("folder", it.path);
    else if (isImage(it.name)) setPreview({ item: it });
    else if (isVideo(it.name)) api.streamUrl(it.path).then((s) => setPreview({ item: it, video: s.url })).catch(fail);
    else api.open(it.path).catch(fail);
  };

  const run = (p: Promise<unknown>, done?: string) =>
    p.then(() => done && notify({ text: done })).catch(fail);

  const paste = () => {
    if (!clip || isTrash || searching !== null) return;
    run(clip.mode === "copy" ? api.copy(clip.paths, tab.path) : api.move(clip.paths, tab.path));
    if (clip.mode === "cut") setClip(null);
  };

  const trash = (list: Item[]) => {
    if (!list.length) return;
    run(api.trash(list.map((i) => i.path)), list.length === 1 ? t("„{name}“ in den Papierkorb verschoben – Strg+Z macht es rückgängig.", { name: list[0].name }) : t("{n} Elemente in den Papierkorb verschoben – Strg+Z macht es rückgängig.", { n: list.length }));
  };

  const deleteForever = (list: Item[]) => list.length && run(api.deletePermanently(list.map((i) => i.path)));

  const newFolder = async () => {
    let name = t("Neuer Ordner");
    const names = new Set(items.map((i) => i.name));
    for (let n = 2; names.has(name); n++) name = `${t("Neuer Ordner")} (${n})`;
    try {
      await api.mkdir(tab.path, name);
      setTimeout(() => {
        const p = joinPath(tab.path, name);
        setSel(new Set([p]));
        setRenaming(p);
      }, 300);
    } catch (e) {
      fail(e);
    }
  };

  const newFile = async () => {
    let name = `${t("Neue Textdatei")}.txt`;
    const names = new Set(items.map((i) => i.name));
    for (let n = 2; names.has(name); n++) name = `${t("Neue Textdatei")} (${n}).txt`;
    try {
      await api.createFile(tab.path, name);
      setTimeout(() => {
        const p = joinPath(tab.path, name);
        setSel(new Set([p]));
        setRenaming(p);
      }, 300);
    } catch (e) {
      fail(e);
    }
  };

  const commitRename = (it: Item, name: string) => {
    setRenaming(null);
    const n = name.trim();
    if (n && n !== it.name) run(api.rename(it.path, n));
  };

  const undo = () => api.undo().then((r) => notify({ text: r.message })).catch(fail);

  // ------------------------------------------------------------ Tastatur
  const onKey = (e: React.KeyboardEvent) => {
    if (renaming || editAddress) return;
    const k = e.key;
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && k.toLowerCase() === "a") { e.preventDefault(); setSel(new Set(sorted.map((i) => i.path))); }
    else if (ctrl && k.toLowerCase() === "c" && selected.length) setClip({ mode: "copy", paths: selected.map((i) => i.path) });
    else if (ctrl && k.toLowerCase() === "x" && selected.length) setClip({ mode: "cut", paths: selected.map((i) => i.path) });
    else if (ctrl && k.toLowerCase() === "v") paste();
    else if (ctrl && k.toLowerCase() === "z") undo();
    else if (ctrl && k.toLowerCase() === "f") { e.preventDefault(); searchRef.current?.focus(); }
    else if (ctrl && k.toLowerCase() === "l") { e.preventDefault(); setEditAddress(true); }
    else if (ctrl && e.shiftKey && k.toLowerCase() === "n") { e.preventDefault(); newFolder(); }
    else if (k === "Delete" && !isTrash) (e.shiftKey ? deleteForever : trash)(selected);
    else if (k === "F2" && selected.length === 1 && !isTrash) setRenaming(selected[0].path);
    else if (k === "F5") load();
    else if (k === "Enter" && selected.length === 1) open(selected[0]);
    else if ((k === "Backspace" || (e.altKey && k === "ArrowUp")) && parentOf(tab.path) && !isTrash) navigate("folder", parentOf(tab.path)!);
    else if (e.altKey && k === "ArrowLeft") go(-1);
    else if (e.altKey && k === "ArrowRight") go(1);
    else if (k === "Escape") { setSel(new Set()); setMenu(null); }
    else if ((k === "ArrowDown" || k === "ArrowUp") && sorted.length) {
      e.preventDefault();
      const i = sorted.findIndex((x) => x.path === anchor);
      const j = Math.max(0, Math.min(sorted.length - 1, i + (k === "ArrowDown" ? 1 : -1)));
      setSel(new Set([sorted[j].path]));
      setAnchor(sorted[j].path);
    }
  };

  // ------------------------------------------------------------ Ziehen und Ablegen (innerhalb der App)
  const onDragStart = (e: React.DragEvent, it: Item) => {
    const paths = sel.has(it.path) ? selected.map((i) => i.path) : [it.path];
    e.dataTransfer.setData("application/x-snap-paths", JSON.stringify(paths));
    e.dataTransfer.effectAllowed = "copyMove";
  };
  const dropInto = (e: React.DragEvent, dest: string) => {
    e.preventDefault();
    e.stopPropagation();
    setDropOver(false);
    const raw = e.dataTransfer.getData("application/x-snap-paths");
    if (!raw) return;
    const paths = (JSON.parse(raw) as string[]).filter((p) => p !== dest && parentOf(p) !== dest);
    if (paths.length) run(e.ctrlKey ? api.copy(paths, dest) : api.move(paths, dest));
  };

  // ------------------------------------------------------------ Darstellung
  const crumbs = useMemo(() => {
    if (isTrash) return [{ name: t("Papierkorb"), path: "trash:" }];
    const parts = tab.path.split("/").filter(Boolean);
    const out = [{ name: "/", path: "/" }];
    let acc = "";
    for (const p of parts) {
      acc += `/${p}`;
      out.push({ name: p, path: acc });
    }
    if (tab.path.startsWith(home) && home !== "/") {
      const hp = home.split("/").filter(Boolean).length;
      return [{ name: t("Persönlicher Ordner"), path: home }, ...out.slice(hp + 1)];
    }
    return out;
  }, [tab.path, home, isTrash]);

  const header = (key: SortKey, label: string) => (
    <th onClick={() => setSort((s) => ({ key, asc: s.key === key ? !s.asc : true }))}>
      {label} {sort.key === key ? (sort.asc ? "↑" : "↓") : ""}
    </th>
  );

  const nameNode = (it: Item) =>
    renaming === it.path ? (
      <input
        className="renameinput"
        autoFocus
        defaultValue={it.name}
        onFocus={(e) => {
          const dot = it.dir ? -1 : it.name.lastIndexOf(".");
          e.currentTarget.setSelectionRange(0, dot > 0 ? dot : it.name.length);
        }}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") commitRename(it, e.currentTarget.value);
          if (e.key === "Escape") setRenaming(null);
        }}
        onBlur={(e) => commitRename(it, e.currentTarget.value)}
        onClick={(e) => e.stopPropagation()}
      />
    ) : (
      <span className={`n${it.hidden ? " hiddenname" : ""}`}>{it.name}</span>
    );

  const rowProps = (it: Item) => ({
    onClick: (e: React.MouseEvent) => click(e, it),
    onDoubleClick: () => open(it),
    onContextMenu: (e: React.MouseEvent) => {
      if (!sel.has(it.path)) { setSel(new Set([it.path])); setAnchor(it.path); }
      setMenu({ x: e.clientX, y: e.clientY, on: it });
    },
    draggable: !isTrash && renaming !== it.path,
    onDragStart: (e: React.DragEvent) => onDragStart(e, it),
    onDragOver: it.dir ? (e: React.DragEvent) => { e.preventDefault(); e.stopPropagation(); } : undefined,
    onDrop: it.dir ? (e: React.DragEvent) => dropInto(e, it.path) : undefined,
    title: searching !== null ? it.path : undefined,
  });

  const canPaste = !!clip && !isTrash && searching === null;
  const totalSel = selected.reduce((a, i) => a + (i.dir ? 0 : i.size), 0);

  return (
    <>
      <div className="toolbar">
        <button className="iconbtn" disabled={tab.index === 0} onClick={() => go(-1)} title={t("Zurück (Alt+←)")}><Icon.back /></button>
        <button className="iconbtn" disabled={tab.index >= tab.history.length - 1} onClick={() => go(1)} title={t("Vor (Alt+→)")}><Icon.forward /></button>
        <button className="iconbtn" disabled={isTrash || !parentOf(tab.path)} onClick={() => navigate("folder", parentOf(tab.path)!)} title={t("Übergeordneter Ordner (Alt+↑)")}><Icon.up /></button>
        <button className="iconbtn" onClick={load} title={t("Aktualisieren (F5)")}><Icon.refresh /></button>
        <div className="address" onClick={() => !isTrash && setEditAddress(true)}>
          {editAddress ? (
            <input
              autoFocus
              defaultValue={tab.path}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  const v = e.currentTarget.value.trim().replace(/^~(?=\/|$)/, home);
                  setEditAddress(false);
                  if (v.startsWith("/")) navigate("folder", v);
                }
                if (e.key === "Escape") setEditAddress(false);
              }}
              onBlur={() => setEditAddress(false)}
            />
          ) : (
            <div className="crumbs">
              {crumbs.map((c, i) => (
                <span key={c.path} style={{ display: "contents" }}>
                  {i > 0 && <span className="crumbsep">›</span>}
                  <button
                    className="crumb"
                    onClick={(e) => { e.stopPropagation(); if (!isTrash) navigate("folder", c.path); }}
                    onDragOver={(e) => e.preventDefault()}
                    onDrop={(e) => dropInto(e, c.path)}
                  >
                    {c.name}
                  </button>
                </span>
              ))}
            </div>
          )}
        </div>
        <div className="searchbox">
          <Icon.search />
          <input
            ref={searchRef}
            placeholder={isTrash ? t("Suche") : t("Überall suchen")}
            value={query}
            disabled={isTrash}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") setSearching(query.trim() || null);
              if (e.key === "Escape") { setQuery(""); setSearching(null); }
            }}
          />
          {searching !== null && (
            <button className="iconbtn" style={{ width: 22, height: 22 }} onClick={() => { setQuery(""); setSearching(null); }}>
              <Icon.close size={12} />
            </button>
          )}
        </div>
      </div>

      <div className="commandbar">
        {isTrash ? (
          <>
            <button className="cmd" disabled={!selected.length} onClick={() => selected.forEach((i) => i.trash && run(api.trashRestore(i.trash.id), "Wiederhergestellt."))}>
              <Icon.restore /> Wiederherstellen
            </button>
            <button className="cmd" disabled={!items.length} onClick={() => run(api.trashEmpty(), "Papierkorb geleert.")}>
              <Icon.trash /> Papierkorb leeren
            </button>
          </>
        ) : (
          <>
            <button className="cmd" disabled={searching !== null} onClick={newFolder}><Icon.newfolder /> {t("Neu")}</button>
            <span className="cmdsep" />
            <button className="cmd" disabled={!selected.length} onClick={() => setClip({ mode: "cut", paths: selected.map((i) => i.path) })} title={t("Ausschneiden (Strg+X)")}><Icon.cut /></button>
            <button className="cmd" disabled={!selected.length} onClick={() => setClip({ mode: "copy", paths: selected.map((i) => i.path) })} title={t("Kopieren (Strg+C)")}><Icon.copy /></button>
            <button className="cmd" disabled={!canPaste} onClick={paste} title={t("Einfügen (Strg+V)")}><Icon.paste /></button>
            <button className="cmd" disabled={selected.length !== 1} onClick={() => setRenaming(selected[0]?.path)} title={t("Umbenennen (F2)")}><Icon.rename /></button>
            <button className="cmd" disabled={!selected.length} onClick={() => trash(selected)} title={t("In den Papierkorb (Entf)")}><Icon.trash /></button>
            <span className="cmdsep" />
            <button className="cmd" onClick={undo} title={t("Rückgängig (Strg+Z)")}><Icon.undo /> {t("Rückgängig")}</button>
          </>
        )}
        <span className="spacer" />
        <button className={`iconbtn${showHidden ? " on" : ""}`} onClick={() => { localStorage.setItem("hidden", showHidden ? "0" : "1"); setShowHidden(!showHidden); }} title={t("Versteckte Dateien")}>
          <Icon.hidden />
        </button>
        <button className={`iconbtn${view === "details" ? " on" : ""}`} onClick={() => { setView("details"); localStorage.setItem("view", "details"); }} title={t("Details")}><Icon.list /></button>
        <button className={`iconbtn${view === "grid" ? " on" : ""}`} onClick={() => { setView("grid"); localStorage.setItem("view", "grid"); }} title={t("Kacheln")}><Icon.gridview /></button>
      </div>

      <div
        ref={viewRef}
        className={`view${dropOver ? " drop" : ""}`}
        tabIndex={0}
        onKeyDown={onKey}
        onClick={(e) => e.target === e.currentTarget && setSel(new Set())}
        onContextMenu={(e) => { if (e.target === e.currentTarget) { setSel(new Set()); setMenu({ x: e.clientX, y: e.clientY, on: null }); } }}
        onDragOver={(e) => { if (!isTrash && searching === null) { e.preventDefault(); setDropOver(true); } }}
        onDragLeave={() => setDropOver(false)}
        onDrop={(e) => dropInto(e, tab.path)}
      >
        {error ? (
          <div className="empty"><div><div className="big">{t("Ordner nicht lesbar")}</div>{error}</div></div>
        ) : !loading && sorted.length === 0 ? (
          <div className="empty">
            <div><div className="big">{searching !== null ? t("Keine Treffer") : isTrash ? t("Der Papierkorb ist leer") : t("Dieser Ordner ist leer")}</div></div>
          </div>
        ) : view === "details" ? (
          <table className="details">
            <colgroup><col style={{ width: "52%" }} /><col style={{ width: "22%" }} /><col style={{ width: "13%" }} /><col style={{ width: "13%" }} /></colgroup>
            <thead>
              <tr>{header("name", t("Name"))}{header("mtime", isTrash ? t("Gelöscht am") : t("Geändert"))}<th>{t("Art")}</th>{header("size", t("Größe"))}</tr>
            </thead>
            <tbody>
              {sorted.map((it) => (
                <tr key={it.path} className={`row${sel.has(it.path) ? " sel" : ""}`} {...rowProps(it)}>
                  <td className="name">
                    <div className="namecell">
                      <FileGlyph name={it.name} dir={it.dir} />
                      {nameNode(it)}
                    </div>
                  </td>
                  <td>{formatDate(it.mtime)}</td>
                  <td>{it.dir ? t("Ordner") : it.name.includes(".") ? t("{ext}-Datei", { ext: it.name.slice(it.name.lastIndexOf(".") + 1).toUpperCase() }) : t("Datei")}</td>
                  <td>{it.dir ? "" : humanSize(it.size)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <div className="grid">
            {sorted.map((it) => (
              <div key={it.path} className={`tile${sel.has(it.path) ? " sel" : ""}`} {...rowProps(it)}>
                <div className="thumb">
                  {!it.dir && !it.trash && (isImage(it.name) || isVideo(it.name)) ? (
                    <img src={thumbUrl(it.path)} loading="lazy" alt="" draggable={false} onError={(e) => (e.currentTarget.style.display = "none")} />
                  ) : (
                    <FileGlyph name={it.name} dir={it.dir} size={64} />
                  )}
                </div>
                {nameNode(it)}
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="statusbar">
        <span>{sorted.length === 1 ? t("1 Element") : t("{n} Elemente", { n: sorted.length })}</span>
        {selected.length > 0 && <span>{t("{n} ausgewählt", { n: selected.length })}{totalSel ? ` · ${humanSize(totalSel)}` : ""}</span>}
        {clip && <span>{clip.mode === "copy" ? t("{n} in der Zwischenablage (kopieren)", { n: clip.paths.length }) : t("{n} in der Zwischenablage (verschieben)", { n: clip.paths.length })}</span>}
        <span className="spacer" />
        {progress && <div className="progress"><div style={{ width: `${Math.round(progress.pct * 100)}%` }} /></div>}
      </div>

      {menu && (
        <ContextMenu x={menu.x} y={menu.y} onClose={() => setMenu(null)}>
          {menu.on && !isTrash && (
            <>
              <button onClick={() => open(menu.on!)}><Icon.open /> {t("Öffnen")}</button>
              {menu.on.dir && <button onClick={() => openTab("folder", menu.on!.path)}><Icon.plus /> {t("In neuem Tab öffnen")}</button>}
              {searching !== null && <button onClick={() => navigate("folder", parentOf(menu.on!.path) ?? "/")}><Icon.up /> {t("Speicherort öffnen")}</button>}
              <hr />
              <button onClick={() => setClip({ mode: "cut", paths: selected.map((i) => i.path) })}><Icon.cut /> {t("Ausschneiden")} <span className="key">{t("Strg+X")}</span></button>
              <button onClick={() => setClip({ mode: "copy", paths: selected.map((i) => i.path) })}><Icon.copy /> {t("Kopieren")} <span className="key">{t("Strg+C")}</span></button>
              <button disabled={selected.length !== 1} onClick={() => setRenaming(menu.on!.path)}><Icon.rename /> {t("Umbenennen")} <span className="key">F2</span></button>
              <hr />
              <button onClick={() => trash(selected)}><Icon.trash /> {t("In den Papierkorb")} <span className="key">{t("Entf")}</span></button>
              <button className="danger" onClick={() => deleteForever(selected)}><Icon.trash /> {t("Endgültig löschen")} <span className="key">{t("Umschalt+Entf")}</span></button>
            </>
          )}
          {menu.on && isTrash && (
            <button onClick={() => selected.forEach((i) => i.trash && run(api.trashRestore(i.trash.id), "Wiederhergestellt."))}><Icon.restore /> {t("Wiederherstellen")}</button>
          )}
          {!menu.on && !isTrash && (
            <>
              <button disabled={searching !== null} onClick={newFolder}><Icon.newfolder /> {t("Neuer Ordner")} <span className="key">{t("Strg+Umschalt+N")}</span></button>
              <button disabled={searching !== null} onClick={newFile}><Icon.documents /> {t("Neue Textdatei")}</button>
              <button disabled={!canPaste} onClick={paste}><Icon.paste /> {t("Einfügen")} <span className="key">{t("Strg+V")}</span></button>
              <hr />
              <button onClick={undo}><Icon.undo /> {t("Rückgängig")} <span className="key">{t("Strg+Z")}</span></button>
              <button onClick={load}><Icon.refresh /> {t("Aktualisieren")} <span className="key">F5</span></button>
            </>
          )}
        </ContextMenu>
      )}

      {preview && createPortal(
        <div className="overlay" onClick={() => setPreview(null)} onKeyDown={(e) => e.key === "Escape" && setPreview(null)} tabIndex={-1} ref={(el) => el?.focus()}>
          {preview.video ? (
            <video src={preview.video} controls autoPlay onClick={(e) => e.stopPropagation()} />
          ) : (
            <img src={thumbUrl(preview.item.path, true)} alt="" />
          )}
          <div className="cap">{baseName(preview.item.path)} · {humanSize(preview.item.size)}</div>
        </div>,
        document.body,
      )}
    </>
  );
}

function ContextMenu({ x, y, onClose, children }: { x: number; y: number; onClose: () => void; children: React.ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  useEffect(() => {
    const el = ref.current;
    if (el) {
      const r = el.getBoundingClientRect();
      setPos({ x: Math.min(x, window.innerWidth - r.width - 8), y: Math.min(y, window.innerHeight - r.height - 8) });
    }
    const close = () => onClose();
    window.addEventListener("mousedown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("blur", close);
    };
  }, [x, y, onClose]);
  // Portal: `backdrop-filter` der Karte würde `position: fixed` sonst relativ zur Karte machen.
  return createPortal(
    <div
      ref={ref}
      className="menu"
      style={{ left: pos.x, top: pos.y }}
      onMouseDown={(e) => e.stopPropagation()}
      onClick={onClose}
    >
      {children}
    </div>,
    document.body,
  );
}
