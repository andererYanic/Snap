// Dateien – Aufbau nach docs/MOBILE_UX.md:
// - Startseite: Suche (Ergebnisse schon beim Tippen, mit Tippfehler-Toleranz), Speicher, Ordner.
// - Explorer: Kopfzeile, flache Zeilen oder Raster, Übergänge beim Ordnerwechsel.
// - Markieren: lang halten – und ohne Loslassen über weitere Dateien wischen. Beim Loslassen
//   erscheint das Menü dort, wo der Finger zuerst lag (wie bei iOS).
// - Bilder und Videos öffnen in der eingebauten Anzeige, alles andere mit der passenden App.
import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Alert, BackHandler, Dimensions, FlatList, Modal, Pressable, ScrollView, StyleSheet, View } from 'react-native';
import { Text, TextInput } from './text';
import { Gesture, GestureDetector } from 'react-native-gesture-handler';
import Animated, { runOnJS } from 'react-native-reanimated';
import ContextMenu, { type MenuAction } from './ContextMenu';
import * as core from './core';
import { humanSize, joinPath, parentOf, relativeDate, request, type DirEntry } from './core';
import { locale, t } from './i18n';
import type { Theme } from './theme';
import { isAudio, isMedia, isVideo, Thumb } from './thumbs';
import { BottomSheet, Glyph, SectionLabel, Tap, themed, type SheetAction } from './ui';
import { Icon } from './Icon';
import Editor from './Editor';
import RemoteBrowser, { type RemoteDevice } from './RemoteBrowser';
import PdfViewer from './PdfViewer';
import TextViewer, { isText } from './TextViewer';
import { ApkSheet, isApk } from './Apk';
import ArchiveViewer, { isArchiveListable } from './ArchiveViewer';
import SvgViewer, { isSvg } from './SvgViewer';
import Viewer, { type MediaItem } from './Viewer';
import { fadeIn, LONG_PRESS, m, pageIn, popIn, popOut, toastIn } from './motion';

interface Props {
  theme: Theme;
  root: string;
  notify: (text: string, undo?: () => void) => void;
  /** Snap Drop: Dateien an Geräte in der Nähe senden. */
  onDrop?: (paths: string[]) => void;
  selectionChanged: (active: boolean) => void;
  /**
   * Platz der unteren Leiste (feste Höhe): Files stellt dort die Aktionsleiste bzw. „Hier
   * einfügen“ hinein; `null` = Hauptmenü. So ändert sich die Höhe der Liste nie.
   */
  bottomSlot?: (node: React.ReactNode) => void;
  /** Schrift wurde geändert (Name oder null = Inconsolata). */
  onFont?: (name: string | null) => void;
  /** Zähler: ändert er sich, geht es zurück zur Startansicht (erneutes Antippen von „Dateien“). */
  home?: number;
}

interface Item extends DirEntry {
  path: string;
  fuzzy?: boolean;
}

type Sort = 'name' | 'date' | 'size' | 'type';
/** Ausführbare Dateien anderer Systeme (auf Android nicht startbar). */
const FOREIGN_PROGRAM = /\.(exe|msi|msix|appx|com|scr|deb|rpm|appimage|flatpak|flatpakref|snap|run|bin|dmg|pkg|app|jar)$/i;
const ROW = 64;
const FOLDERS: [string, string][] = [
  ['Download', 'Downloads'],
  ['DCIM', 'Kamera'],
  ['Pictures', 'Bilder'],
  ['Documents', 'Dokumente'],
  ['Music', 'Musik'],
  ['Movies', 'Videos'],
];

/** Endung in Kleinbuchstaben („“ ohne Endung). */
export function extOf(name: string): string {
  const i = name.lastIndexOf('.');
  return i > 0 && i < name.length - 1 ? name.slice(i + 1).toLowerCase() : '';
}

/**
 * Reihenfolge der Einträge: Ordner immer zuerst. „type“ gruppiert nach Dateiformat (Endung,
 * alphabetisch; ohne Endung zuletzt) und sortiert innerhalb jeder Gruppe nach Datum, neueste zuerst.
 */
export function entryOrder(sort: 'name' | 'date' | 'size' | 'type', coll: Intl.Collator) {
  return (a: { name: string; kind: string; mtime: number; size: number }, b: { name: string; kind: string; mtime: number; size: number }) => {
    if ((a.kind === 'Dir') !== (b.kind === 'Dir')) {
      return a.kind === 'Dir' ? -1 : 1;
    }
    if (sort === 'type') {
      const ea = a.kind === 'Dir' ? '' : extOf(a.name);
      const eb = b.kind === 'Dir' ? '' : extOf(b.name);
      if (ea !== eb) {
        if (!ea || !eb) {
          return ea ? -1 : 1;
        }
        return coll.compare(ea, eb);
      }
      return b.mtime - a.mtime || coll.compare(a.name, b.name);
    }
    if (sort === 'date') {
      return b.mtime - a.mtime;
    }
    if (sort === 'size') {
      return b.size - a.size;
    }
    return coll.compare(a.name, b.name);
  };
}

export default function Files({ theme, root, notify, selectionChanged, bottomSlot, onFont, onDrop, home = 0 }: Props) {
  const [path, setPath] = useState<string | null>(null); // null = Startseite
  const [dir, setDir] = useState<'forward' | 'back'>('forward');
  const [items, setItems] = useState<Item[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  // Ab zwei markierten Dateien: Aktionsleiste unten statt Menü am Finger. Einmal offen, bleibt sie,
  // bis alles abgewählt ist.
  const [barOn, setBarOn] = useState(false);
  const [clip, setClip] = useState<{ mode: 'copy' | 'move'; paths: string[] } | null>(null);
  // Laufender langer Vorgang (Entpacken/Packen) und Passwortabfrage für verschlüsselte Archive
  const [busy, setBusy] = useState<string | null>(null);
  const [pwAsk, setPwAsk] = useState<{ archive: string; rest: string[]; wrong: boolean; value: string } | null>(null);
  const [naming, setNaming] = useState<{ mode: 'folder' | 'file' | 'rename'; from?: string; value: string } | null>(null);
  const [sheet, setSheet] = useState<'create' | 'more' | 'selection' | null>(null);
  const [view, setView] = useState<'list' | 'grid'>('list');
  const [sort, setSortState] = useState<Sort>('name');
  // Gewählte Sortierung bleibt über Neustarts erhalten
  useEffect(() => {
    core.getPref('sort').then((v: string | null) => {
      if (v === 'name' || v === 'date' || v === 'size' || v === 'type') {
        setSortState(v);
      }
    }).catch(() => {});
  }, []);
  const setSort = (v: Sort) => {
    setSortState(v);
    core.setPref('sort', v);
  };
  const [hidden, setHidden] = useState(false);
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<Item[] | null>(null);
  const [searching, setSearching] = useState(false); // Anfrage läuft noch
  const [apk, setApk] = useState<{ path: string; name: string; size: number } | null>(null);
  const [archive, setArchive] = useState<{ path: string; name: string } | null>(null);
  const [pdf, setPdf] = useState<{ path: string; name: string; mode?: 'view' | 'mark' | 'sign' } | null>(null);
  const [editing, setEditing] = useState<{ path: string; name: string } | null>(null);
  const [textFile, setTextFile] = useState<{ path: string; name: string } | null>(null);
  const [svg, setSvg] = useState<{ path: string; name: string } | null>(null);
  const [viewer, setViewer] = useState<{ items: MediaItem[]; start: number } | null>(null);
  // Geöffnetes anderes Gerät (Startseite → Gerät antippen)
  const [remoteDev, setRemoteDev] = useState<RemoteDevice | null>(null);
  const [storage, setStorage] = useState<{ total: number; free: number } | null>(null);
  const [folders, setFolders] = useState<[string, string][]>([]);
  // Gekoppelte Geräte mit gemeldetem Speicher (Startseite)
  const [remote, setRemote] = useState<{ node_id: string; name: string; online?: boolean; storage?: { total: number; free: number } }[]>([]);
  const [scrollEnabled, setScrollEnabled] = useState(true);
  const listRef = useRef<FlatList<Item>>(null);
  const scrollY = useRef(0);
  const listH = useRef(0);
  const searchGen = useRef(0);
  const s = styles(theme);
  const cols = 3;
  const W = Dimensions.get('window').width;
  const tile = Math.floor((W - 32 - (cols - 1) * 8) / cols);
  const tileH = tile + 40;

  const go = useCallback((next: string | null, direction: 'forward' | 'back') => {
    setDir(direction);
    setPath(next);
  }, []);

  const homeSeen = useRef(home);
  useEffect(() => {
    if (home === homeSeen.current) {
      return;
    }
    homeSeen.current = home;
    setQuery('');
    setSel(new Set());
    setMenu(null);
    setClip(null);
    setRemoteDev(null);
    setPath((p) => {
      if (p !== null) {
        setDir('back');
      }
      return null;
    });
  }, [home]);

  // ------------------------------------------------------------ Laden
  const load = useCallback(async () => {
    if (path === null) {
      return;
    }
    try {
      const r = await request<{ entries: DirEntry[] }>({ ListDir: { path, show_hidden: hidden, offset: 0, limit: 1000 } });
      setItems(r.entries.map((e) => ({ ...e, path: joinPath(path, e.name) })));
      setError(null);
    } catch (e: any) {
      setItems([]);
      setError(e.message);
    }
  }, [path, hidden]);

  useEffect(() => {
    setSel(new Set());
    setMenu(null);
    scrollY.current = 0;
    load();
  }, [load]);

  useEffect(() => {
    if (path !== null) {
      return;
    }
    core.storageInfo().then(setStorage).catch(() => {});
    request<{ devices: typeof remote }>('PairList').then((r) => setRemote(r.devices ?? [])).catch(() => {});
    request<{ entries: DirEntry[] }>({ ListDir: { path: root, show_hidden: false, offset: 0, limit: 1000 } })
      .then((r) => {
        const names = new Set(r.entries.filter((e) => e.kind === 'Dir').map((e) => e.name));
        setFolders(FOLDERS.filter(([d]) => names.has(d)));
      })
      .catch(() => {});
  }, [path, root]);

  useEffect(() => selectionChanged(sel.size > 0 || clip !== null), [sel, clip, selectionChanged]);

  useEffect(() => {
    if (sel.size === 0) {
      setBarOn(false);
      setMenu(null);
    } else if (sel.size > 1) {
      setBarOn(true);
      setMenu(null);
    }
  }, [sel]);
  const showBar = barOn && sel.size > 0;

  // Suche beim Tippen (entprellt; veraltete Antworten werden verworfen)
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      ++searchGen.current;
      setHits(null);
      setSearching(false);
      return;
    }
    // Sofort in die Suchansicht wechseln; bis die erste Antwort da ist, bleibt die Liste leer.
    setHits((h) => h ?? []);
    setSearching(true);
    const gen = ++searchGen.current;
    const timer = setTimeout(async () => {
      try {
        const r = await request<{ hits: { path: string; name: string; kind: DirEntry['kind']; size: number; mtime: number; fuzzy?: boolean }[] }>({
          Search: { query: q, filter: {}, limit: 200 },
        });
        if (gen === searchGen.current) {
          setHits(r.hits.map((h) => ({ ...h, hidden: h.name.startsWith('.') })));
          setSearching(false);
        }
      } catch {
        // Suche fehlgeschlagen: alte Treffer stehen lassen
        if (gen === searchGen.current) {
          setSearching(false);
        }
      }
    }, 160);
    return () => clearTimeout(timer);
  }, [query]);

  // Zurück: Menü → Auswahl → Suche → Ordner hoch → Startseite
  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', () => {
      if (menu) {
        setMenu(null);
        return true;
      }
      if (sel.size) {
        setSel(new Set());
        return true;
      }
      if (hits) {
        setQuery('');
        return true;
      }
      if (path !== null) {
        go(parentOf(path, root) ?? null, 'back');
        return true;
      }
      return false;
    });
    return () => sub.remove();
  }, [path, root, sel, hits, menu, go]);

  const sorted = useMemo(() => {
    const v = [...items];
    v.sort(entryOrder(sort, new Intl.Collator(locale(), { numeric: true, sensitivity: 'base' })));
    return v;
  }, [items, sort]);

  const shown = hits ?? sorted;
  const grid = view === 'grid' && !hits;
  const selected = shown.filter((e) => sel.has(e.path));

  // ------------------------------------------------------------ Aktionen
  const fail = (e: any) => notify(e?.message ?? String(e));
  const fileOp = (op: unknown) => request({ FileOp: { op, wait: true } });
  /** Programme anderer Systeme: verständlicher Hinweis statt „keine App gefunden“. */
  const openFailed = (name: string, err: unknown) =>
    FOREIGN_PROGRAM.test(name) ? notify(t('„{name}“ ist ein Programm für Windows, Linux oder macOS und lässt sich auf Android nicht ausführen.', { name })) : fail(err);

  const open = (e: Item) => {
    if (sel.size) {
      const n = new Set(sel);
      n.has(e.path) ? n.delete(e.path) : n.add(e.path);
      setSel(n);
      if (n.size === 0) {
        setMenu(null);
      }
      core.haptic('tick');
    } else if (e.kind === 'Dir') {
      setQuery('');
      go(e.path, 'forward');
    } else if (isSvg(e.name)) {
      setSvg({ path: e.path, name: e.name });
    } else if (isText(e.name)) {
      // Text und Markdown zeigt die App selbst an
      setTextFile({ path: e.path, name: e.name });
    } else if (/\.pdf$/i.test(e.name)) {
      // PDFs zeigt die App selbst (Android zeichnet die Seiten)
      setPdf({ path: e.path, name: e.name });
    } else if (isMedia(e.name) || isAudio(e.name)) {
      // Alle Dateien des Ordners bzw. der Treffer: Wischen führt zur nächsten/vorigen Datei
      const files = shown.filter((x) => x.kind === 'File');
      setViewer({ items: files, start: Math.max(0, files.findIndex((x) => x.path === e.path)) });
    } else if (isApk(e.name)) {
      setApk({ path: e.path, name: e.name, size: e.size ?? 0 });
    } else if (isArchiveListable(e.name)) {
      setArchive({ path: e.path, name: e.name });
    } else {
      core.openFile(e.path).catch((err) => openFailed(e.name, err));
    }
  };

  /** Datei (z. B. von einem anderen Gerät heruntergeladen) mit den eingebauten Anzeigen öffnen. */
  const openPath = (path: string, name: string) => {
    if (isSvg(name)) {
      setSvg({ path, name });
    } else if (isText(name)) {
      setTextFile({ path, name });
    } else if (/\.pdf$/i.test(name)) {
      setPdf({ path, name });
    } else if (isMedia(name) || isAudio(name)) {
      setViewer({ items: [{ path, name }], start: 0 });
    } else if (isApk(name)) {
      setApk({ path, name, size: 0 });
    } else if (isArchiveListable(name)) {
      setArchive({ path, name });
    } else {
      core.openFile(path).catch((err) => openFailed(name, err));
    }
  };

  const undoable = (text: string) =>
    notify(text, async () => {
      await request('Undo');
      load();
    });

  const trash = async () => {
    const list = selected;
    try {
      await fileOp({ Trash: { paths: list.map((e) => e.path) } });
      setSel(new Set());
      load();
      undoable(list.length === 1 ? t('„{name}“ in den Papierkorb verschoben.', { name: list[0].name }) : t('{n} Elemente in den Papierkorb verschoben.', { n: list.length }));
    } catch (e) {
      fail(e);
    }
  };

  // ------------------------------------------------------------ Archive
  const ARCHIVE = /\.(zip|tar|tgz|tar\.gz)$/i;
  const fileName = (p: string) => p.slice(p.lastIndexOf('/') + 1);

  /** Entpackt Archive nacheinander; bei Passwortschutz wird gefragt und dort weitergemacht. */
  const extractAll = async (archives: string[], password?: string) => {
    setSel(new Set());
    let done = 0;
    let last = '';
    for (let i = 0; i < archives.length; i++) {
      const a = archives[i];
      setBusy(t('„{name}“ wird entpackt …', { name: fileName(a) }));
      try {
        const r = await fileOp({ Extract: { archive: a, ...(i === 0 && password ? { password } : {}) } });
        last = fileName(r?.outcome?.done?.[0]?.to ?? '');
        done++;
      } catch (e: any) {
        setBusy(null);
        if (e?.code === 'PasswordRequired') {
          setPwAsk({ archive: a, rest: archives.slice(i + 1), wrong: i === 0 && !!password, value: '' });
          if (done) {
            load();
          }
          return;
        }
        load();
        fail(e);
        return;
      }
    }
    setBusy(null);
    load();
    if (done === 1) {
      undoable(t('Entpackt nach „{name}“.', { name: last }));
    } else if (done > 1) {
      notify(t('{n} Archive entpackt.', { n: done }));
    }
  };

  const compressSel = async () => {
    const paths = selected.map((e) => e.path);
    setSel(new Set());
    setBusy(t('{n} Einträge werden gepackt …', { n: paths.length }));
    try {
      const r = await fileOp({ Compress: { sources: paths } });
      setBusy(null);
      load();
      undoable(t('„{name}“ erstellt.', { name: fileName(r?.outcome?.done?.[0]?.to ?? '') }));
    } catch (e) {
      setBusy(null);
      load();
      fail(e);
    }
  };

  const deleteForever = () => {
    const paths = selected.map((e) => e.path);
    Alert.alert(t('Endgültig löschen'), t('Diese {n} Elemente endgültig löschen? Das kann nicht rückgängig gemacht werden.', { n: paths.length }), [
      { text: t('Abbrechen'), style: 'cancel' },
      {
        text: t('Endgültig löschen'),
        style: 'destructive',
        onPress: () => fileOp({ Delete: { paths } }).then(() => { setSel(new Set()); load(); }).catch(fail),
      },
    ]);
  };

  const paste = async () => {
    if (!clip || path === null) {
      return;
    }
    try {
      const body = { sources: clip.paths, dest: path, conflict: 'KeepBoth' };
      await fileOp(clip.mode === 'copy' ? { Copy: body } : { Move: body });
      undoable(clip.mode === 'copy' ? t('{n} Elemente kopiert.', { n: clip.paths.length }) : t('{n} Elemente verschoben.', { n: clip.paths.length }));
      setClip(null);
      load();
    } catch (e) {
      fail(e);
    }
  };

  const commitName = async () => {
    if (!naming || path === null) {
      return;
    }
    const name = naming.value.trim();
    const mode = naming.mode;
    setNaming(null);
    if (!name) {
      return;
    }
    try {
      if (mode === 'folder') {
        await fileOp({ Mkdir: { parent: path, name } });
      } else if (mode === 'file') {
        await fileOp({ CreateFile: { parent: path, name } });
      } else if (naming.from && name !== naming.from) {
        await fileOp({ Rename: { path: joinPath(path, naming.from), new_name: name } });
        setSel(new Set());
      }
      load();
    } catch (e) {
      fail(e);
    }
  };

  // ------------------------------------------------------------ Markieren durch Wischen
  // Index des Elements unter (x, y) im Listenbereich
  const indexAt = useCallback(
    (x: number, y: number) => {
      const yy = y + scrollY.current - (grid ? 16 : 0);
      if (grid) {
        const row = Math.floor(yy / (tileH + 8));
        const col = Math.min(cols - 1, Math.max(0, Math.floor((x - 16) / (tile + 8))));
        return row * cols + col;
      }
      return Math.floor(yy / ROW);
    },
    [grid, tile, tileH],
  );

  const drag = useRef<{ start: number; base: Set<string>; last: number; x: number; y: number; size: number } | null>(null);
  const autoScroll = useRef<ReturnType<typeof setInterval> | null>(null);

  const dragStart = useCallback(
    (x: number, y: number, ax: number, ay: number) => {
      const i = indexAt(x, y);
      if (i < 0 || i >= shown.length) {
        return;
      }
      setScrollEnabled(false);
      setMenu(null);
      const base = new Set(sel);
      base.add(shown[i].path);
      drag.current = { start: i, base, last: i, x: ax, y: ay, size: base.size };
      setSel(new Set(base));
      core.haptic('select');
    },
    [indexAt, shown, sel],
  );

  const dragMove = useCallback(
    (x: number, y: number) => {
      const d = drag.current;
      if (!d) {
        return;
      }
      // Am Rand automatisch weiterscrollen
      const edge = 72;
      const speed = y < edge ? -(edge - y) / 3 : y > listH.current - edge ? (y - (listH.current - edge)) / 3 : 0;
      if (speed !== 0 && !autoScroll.current) {
        autoScroll.current = setInterval(() => {
          scrollY.current = Math.max(0, scrollY.current + speed);
          listRef.current?.scrollToOffset({ offset: scrollY.current, animated: false });
        }, 16);
      } else if (speed === 0 && autoScroll.current) {
        clearInterval(autoScroll.current);
        autoScroll.current = null;
      }
      const i = Math.max(0, Math.min(shown.length - 1, indexAt(x, y)));
      if (i === d.last) {
        return;
      }
      d.last = i;
      const n = new Set(d.base);
      for (let k = Math.min(d.start, i); k <= Math.max(d.start, i); k++) {
        n.add(shown[k].path);
      }
      d.size = n.size;
      setSel(n);
      core.haptic('tick');
    },
    [indexAt, shown],
  );

  const dragEnd = useCallback(() => {
    if (autoScroll.current) {
      clearInterval(autoScroll.current);
      autoScroll.current = null;
    }
    setScrollEnabled(true);
    const d = drag.current;
    drag.current = null;
    // Menü am Finger nur, wenn genau eine Datei markiert ist; sonst übernimmt die Leiste unten
    if (d && d.size === 1) {
      setMenu({ x: d.x, y: d.y });
    }
  }, []);

  const selectGesture = Gesture.Pan()
    .withTestId('files-select')
    .activateAfterLongPress(LONG_PRESS)
    .enabled(!clip)
    .onStart((e) => runOnJS(dragStart)(e.x, e.y, e.absoluteX, e.absoluteY))
    .onUpdate((e) => runOnJS(dragMove)(e.x, e.y))
    .onFinalize(() => runOnJS(dragEnd)());

  // Zeilen sind memoisiert: Beim Markieren wird nur die geänderte Zeile neu gezeichnet.
  // Freigegebene Ordner (für gekoppelte Geräte sichtbar)
  const [shared, setShared] = useState<Map<string, string>>(new Map());
  const loadShares = useCallback(() => {
    request<{ shares: { name: string; root: string }[] }>('ShareList')
      .then((r) => setShared(new Map((r.shares ?? []).map((x) => [x.root, x.name]))))
      .catch(() => {});
  }, []);
  useEffect(() => {
    loadShares();
  }, [loadShares]);
  const shareFolder = (e: Item) => {
    setSel(new Set());
    request({ ShareAdd: { path: e.path, name: null, devices: null } })
      .then(() => {
        notify(t('„{name}“ ist jetzt für deine gekoppelten Geräte sichtbar.', { name: e.name }));
        loadShares();
      })
      .catch(fail);
  };
  const unshareFolder = (e: Item) => {
    const name = shared.get(e.path);
    setSel(new Set());
    if (!name) {
      return;
    }
    request({ ShareRemove: { name } })
      .then(() => {
        notify(t('„{name}“ ist nicht mehr freigegeben.', { name: e.name }));
        loadShares();
      })
      .catch(fail);
  };

  const openRef = useRef(open);
  openRef.current = open;
  const onOpen = useCallback((e: Item) => openRef.current(e), []);
  const selecting = sel.size > 0;
  const renderItem = useCallback(
    ({ item }: { item: Item }) => (
      <FileRow
        e={item}
        on={sel.has(item.path)}
        shared={item.kind === 'Dir' && shared.has(item.path)}
        grid={grid}
        tile={tile}
        tileH={tileH}
        theme={theme}
        inSearch={!!hits}
        selecting={selecting}
        onOpen={onOpen}
      />
    ),
    [sel, shared, grid, tile, tileH, theme, hits, selecting, onOpen],
  );

  // ------------------------------------------------------------ Startseite
  // Startseite und Suche teilen sich EIN Suchfeld an derselben Stelle im Baum: Beim ersten
  // Buchstaben wechselt nur der Inhalt darunter – die Tastatur bleibt offen.
  const onLanding = path === null && !hits;
  const used = storage ? storage.total - storage.free : 0;
  const landingBody = !onLanding ? null : (
      <Animated.View entering={m(dir === 'back' ? pageIn('back') : fadeIn)} style={s.fill}>
        <ScrollView contentContainerStyle={s.landing} keyboardShouldPersistTaps="handled">
          <SectionLabel theme={theme} style={s.section}>{t('Speicher')}</SectionLabel>
          <View style={s.group}>
            <Tap theme={theme} onPress={() => go(root, 'forward')} style={s.placeRow} label={t('Interner Speicher')}>
              <Icon name="phone" size={24} color={theme.accent as string} />
              <View style={s.placeText}>
                <View style={s.placeTop}>
                  <Text style={s.cardTitle} numberOfLines={1}>{t('Interner Speicher')}</Text>
                  {storage && <Text style={s.placeNum}>{t('{free} frei', { free: humanSize(storage.free, locale()) })}</Text>}
                </View>
                {storage && (
                  <View style={s.usage}>
                    <View style={[s.usageFill, { width: `${Math.min(100, (used / storage.total) * 100)}%` }]} />
                  </View>
                )}
                {storage && (
                  <Text style={s.meta}>{t('{used} von {total} belegt', { used: humanSize(used, locale()), total: humanSize(storage.total, locale()) })}</Text>
                )}
              </View>
            </Tap>
            {remote.map((d) => {
              const dUsed = d.storage ? d.storage.total - d.storage.free : 0;
              return (
                <View key={d.node_id}>
                  <View style={s.groupLine} />
                  <Tap theme={theme} onPress={() => setRemoteDev(d)} style={s.placeRow} label={d.name}>
                    <Icon name={/tablet/i.test(d.name) ? 'tablet' : /handy|phone|pixel/i.test(d.name) ? 'phone' : 'laptop'} size={24} color={(d.online ? theme.accent : theme.text3) as string} />
                    <View style={s.placeText}>
                      <View style={s.placeTop}>
                        <Text style={[s.cardTitle, !d.online && s.offline]} numberOfLines={1}>{d.name}</Text>
                        {d.online && d.storage ? <Text style={s.placeNum}>{t('{free} frei', { free: humanSize(d.storage.free, locale()) })}</Text> : null}
                      </View>
                      {d.online && d.storage ? (
                        <>
                          <View style={s.usage}>
                            <View style={[s.usageFill, { width: `${Math.min(100, (dUsed / d.storage.total) * 100)}%` }]} />
                          </View>
                          <Text style={s.meta}>{t('{used} von {total} belegt', { used: humanSize(dUsed, locale()), total: humanSize(d.storage.total, locale()) })}</Text>
                        </>
                      ) : (
                        <View style={s.status}>
                          <View style={[s.onlineDot, { backgroundColor: (d.online ? theme.ok : theme.text3) as string }]} />
                          <Text style={s.meta}>{d.online ? t('Verbunden') : t('Nicht verbunden')}</Text>
                        </View>
                      )}
                    </View>
                  </Tap>
                </View>
              );
            })}
          </View>
          {folders.length > 0 && <SectionLabel theme={theme} style={s.section}>{t('Standardordner')}</SectionLabel>}
          <View style={s.folderGrid}>
            {folders.map(([d, label]) => (
              <View key={d} style={s.folderCell}>
                <Tap theme={theme} onPress={() => go(joinPath(root, d), 'forward')} style={s.folderTile} label={t(label)}>
                  <Glyph name={d} dir theme={theme} size={44} />
                  <Text style={s.folderLabel} numberOfLines={1}>{t(label)}</Text>
                </Tap>
              </View>
            ))}
          </View>
        </ScrollView>
      </Animated.View>
  );

  // ------------------------------------------------------------ Explorer / Suche
  const title = hits || path === null ? '' : path === root ? t('Interner Speicher') : path.slice(path.lastIndexOf('/') + 1);
  const single = selected.length === 1 ? selected[0] : null;
  const zips = selected.filter((e) => e.kind === 'File' && ARCHIVE.test(e.name)).map((e) => e.path);
  const menuActions: MenuAction[] = [
    // Je nach Dateiformat
    ...(single && single.kind === 'File' ? formatActions(single) : []),
    ...(single && single.kind === 'Dir' && !hits
      ? [shared.has(single.path)
        ? { icon: 'close' as const, label: t('Freigabe beenden'), onPress: () => unshareFolder(single) }
        : { icon: 'devices' as const, label: t('Für Geräte freigeben'), onPress: () => shareFolder(single) }]
      : []),
    { icon: 'share', label: t('Teilen'), disabled: selected.some((e) => e.kind === 'Dir'), onPress: () => core.shareFiles(selected.map((e) => e.path)).catch(fail) },
    // Ordner werden für die Übertragung gepackt und beim Empfänger wieder entpackt
    ...(onDrop ? [{ icon: 'nearby' as const, label: t('In der Nähe senden'), onPress: () => { onDrop(selected.map((e) => e.path)); setSel(new Set()); } }] : []),
    { icon: 'copy', label: t('Kopieren nach …'), onPress: () => { setClip({ mode: 'copy', paths: selected.map((e) => e.path) }); setSel(new Set()); } },
    { icon: 'move', label: t('Verschieben nach …'), onPress: () => { setClip({ mode: 'move', paths: selected.map((e) => e.path) }); setSel(new Set()); } },
    { icon: 'rename', label: t('Umbenennen'), disabled: !single || !!hits, onPress: () => single && setNaming({ mode: 'rename', from: single.name, value: single.name }) },
    { icon: 'trash', label: t('In den Papierkorb'), separate: true, onPress: trash },
    { icon: 'close', label: t('Endgültig löschen'), danger: true, onPress: deleteForever },
  ];
  /** Aktionen, die nur für bestimmte Formate passen (Menü am Finger, eine Datei). */
  function formatActions(e: Item): MenuAction[] {
    const done = () => setSel(new Set());
    if (ARCHIVE.test(e.name)) {
      return [{ icon: 'extract', label: t('Entpacken'), onPress: () => extractAll([e.path]) }];
    }
    if (/\.(jpe?g|png|webp|heic|heif|bmp)$/i.test(e.name)) {
      return [
        { icon: 'edit', label: t('Bearbeiten'), onPress: () => { done(); setEditing({ path: e.path, name: e.name }); } },
        { icon: 'print', label: t('Drucken'), onPress: () => { done(); core.printFile(e.path).catch(fail); } },
      ];
    }
    if (isVideo(e.name)) {
      return [{ icon: 'edit', label: t('Bearbeiten'), onPress: () => { done(); setEditing({ path: e.path, name: e.name }); } }];
    }
    if (/\.pdf$/i.test(e.name)) {
      return [
        { icon: 'sign', label: t('Signieren'), onPress: () => { done(); setPdf({ path: e.path, name: e.name, mode: 'sign' }); } },
        { icon: 'highlight', label: t('Markieren'), onPress: () => { done(); setPdf({ path: e.path, name: e.name, mode: 'mark' }); } },
        { icon: 'print', label: t('Drucken'), onPress: () => { done(); core.printFile(e.path).catch(fail); } },
      ];
    }
    if (/\.snapid$/i.test(e.name)) {
      return [{
        icon: 'key',
        label: t('Als digitale ID importieren'),
        onPress: () => {
          done();
          request<{ id: { name: string } | null }>({ SignIdImport: { path: e.path } })
            .then((r) => notify(t('Digitale ID „{name}“ importiert.', { name: r.id?.name ?? '' })))
            .catch(fail);
        },
      }];
    }
    if (/\.pdf$/i.test(e.name) || /\.gif$/i.test(e.name)) {
      return [{ icon: 'print', label: t('Drucken'), onPress: () => { done(); core.printFile(e.path).catch(fail); } }];
    }
    if (/\.(ttf|otf)$/i.test(e.name)) {
      return [{
        icon: 'font',
        label: t('Als Schriftart verwenden'),
        onPress: () => {
          done();
          core
            .setFontFromFile(e.path)
            .then((name) => {
              onFont?.(name);
              notify(t('Schriftart „{name}“ wird verwendet.', { name: name ?? e.name }));
            })
            .catch(fail);
        },
      }];
    }
    if (/\.(docx?|odt|rtf|xlsx?|ods|pptx?|odp)$/i.test(e.name)) {
      return [{ icon: 'open', label: t('Mit Office-App öffnen'), onPress: () => { done(); core.openFile(e.path).catch(fail); } }];
    }
    return [];
  }

  const toClip = (mode: 'copy' | 'move') => {
    setClip({ mode, paths: selected.map((e) => e.path) });
    setSel(new Set());
  };
  const barActions = [
    { icon: 'share' as const, label: t('Teilen'), disabled: selected.some((e) => e.kind === 'Dir'), onPress: () => core.shareFiles(selected.map((e) => e.path)).catch(fail) },
    { icon: 'copy' as const, label: t('Kopieren'), onPress: () => toClip('copy') },
    { icon: 'move' as const, label: t('Verschieben'), onPress: () => toClip('move') },
    { icon: 'trash' as const, label: t('Papierkorb'), danger: true, onPress: trash },
    { icon: 'more' as const, label: t('Mehr'), onPress: () => setSheet('selection') },
  ];
  const selectionActions: SheetAction[] = [
    ...(onDrop && !selected.some((e) => e.kind === 'Dir')
      ? [{ icon: 'nearby' as const, label: t('In der Nähe senden'), onPress: () => { onDrop(selected.map((e) => e.path)); setSel(new Set()); } }]
      : []),
    ...(zips.length ? [{ label: zips.length === 1 ? t('Entpacken') : t('{n} Archive entpacken', { n: zips.length }), onPress: () => extractAll(zips) }] : []),
    { label: t('Als ZIP packen'), disabled: !!hits, onPress: compressSel },
    { label: t('Umbenennen'), disabled: !single || !!hits, onPress: () => single && setNaming({ mode: 'rename', from: single.name, value: single.name }) },
    { label: t('Endgültig löschen'), danger: true, onPress: deleteForever },
  ];
  const moreActions: SheetAction[] = [
    { label: view === 'list' ? t('Als Raster anzeigen') : t('Als Liste anzeigen'), onPress: () => setView(view === 'list' ? 'grid' : 'list') },
    { label: t('Nach Name sortieren'), onPress: () => setSort('name'), disabled: sort === 'name' },
    { label: t('Nach Datum sortieren'), onPress: () => setSort('date'), disabled: sort === 'date' },
    { label: t('Nach Größe sortieren'), onPress: () => setSort('size'), disabled: sort === 'size' },
    { label: t('Nach Dateiformat sortieren'), onPress: () => setSort('type'), disabled: sort === 'type' },
    { label: hidden ? t('Versteckte Dateien ausblenden') : t('Versteckte Dateien anzeigen'), onPress: () => setHidden(!hidden) },
  ];

  // Untere Leiste: Aktionsleiste (ab zwei Dateien) oder „Hier einfügen“ im festen Platz der App
  const bottomNode = onLanding ? null : showBar ? (
    <View style={s.actionBar} accessibilityRole="toolbar">
      {barActions.map((a) => (
        <Tap key={a.label} theme={theme} onPress={a.onPress} disabled={a.disabled} style={[s.actionBtn, a.disabled && s.dim]} label={a.label}>
          <Icon name={a.icon} size={22} color={(a.danger ? theme.danger : theme.text) as string} strokeWidth={a.icon === 'more' ? 2.4 : 1.6} />
          <Text style={[s.actionText, a.danger && { color: theme.danger }]} numberOfLines={1}>{a.label}</Text>
        </Tap>
      ))}
    </View>
  ) : clip ? (
    <View style={s.bar}>
      <Text style={s.clipText} numberOfLines={2}>
        {clip.mode === 'copy' ? t('{n} zum Kopieren ausgewählt', { n: clip.paths.length }) : t('{n} zum Verschieben ausgewählt', { n: clip.paths.length })}
      </Text>
      <BarBtn theme={theme} label={t('Abbrechen')} onPress={() => setClip(null)} />
      <BarBtn theme={theme} label={t('Hier einfügen')} strong disabled={!!hits || path === null} onPress={paste} />
    </View>
  ) : null;
  const slotRef = useRef(bottomSlot);
  slotRef.current = bottomSlot;
  // Nur neu setzen, wenn sich Auswahl, Ablage oder Ansicht ändern (sonst Endlosschleife über die App)
  const nodeRef = useRef(bottomNode);
  nodeRef.current = bottomNode;
  useEffect(() => {
    slotRef.current?.(nodeRef.current);
  }, [showBar, clip, onLanding, sel, hits, path, theme, shown]);
  useEffect(() => () => slotRef.current?.(null), []);

  return (
    <View style={s.fill}>
      {/* Kopfzeile */}
      {sel.size > 0 ? (
        <Animated.View entering={m(fadeIn)} style={s.header}>
          <Tap theme={theme} onPress={() => { setSel(new Set()); setMenu(null); }} style={s.iconBtn} label={t('Auswahl aufheben')}>
            <Icon name="close" color={theme.text as string} />
          </Tap>
          <Text style={s.headerTitle}>{t('{n} ausgewählt', { n: sel.size })}</Text>
          <Tap theme={theme} onPress={() => setSel(new Set(shown.map((e) => e.path)))} style={s.textBtn}>
            <Text style={s.link}>{t('Alle')}</Text>
          </Tap>
          {showBar ? (
            <View style={s.iconBtn} />
          ) : (
            <Tap theme={theme} onPress={() => setMenu({ x: W - 24, y: 96 })} style={s.iconBtn} label={t('Aktionen')}>
              <Icon name="more" color={theme.text as string} strokeWidth={2.4} />
            </Tap>
          )}
        </Animated.View>
      ) : path === null || hits ? (
        <View style={hits ? s.searchTopActive : s.searchTop}>
          {!hits && <Text style={s.bigTitle}>{t('Dateien')}</Text>}
          <View style={s.searchRow}>
            {hits ? (
              <Tap theme={theme} onPress={() => setQuery('')} style={s.iconBtn} label={t('Zurück')}>
                <Icon name="back" color={theme.text as string} />
              </Tap>
            ) : null}
            <View style={s.grow}>
              <SearchField theme={theme} value={query} onChange={setQuery} />
            </View>
          </View>
        </View>
      ) : (
        <View style={s.header}>
          <Tap theme={theme} onPress={() => go(parentOf(path!, root) ?? null, 'back')} style={s.iconBtn} label={t('Zurück')}>
            <Icon name="back" color={theme.text as string} />
          </Tap>
          <View style={s.headerTitles}>
            <Text style={[s.headerTitle, s.noFlex]} numberOfLines={1}>{title}</Text>
            {path !== root && <Text style={s.crumb} numberOfLines={1} ellipsizeMode="head">{crumbOf(path!, root, t('Interner Speicher'))}</Text>}
          </View>
          <Tap theme={theme} onPress={() => setView(view === 'list' ? 'grid' : 'list')} style={s.iconBtn} label={view === 'list' ? t('Als Raster anzeigen') : t('Als Liste anzeigen')}>
            <Icon name={view === 'list' ? 'grid' : 'list'} size={22} color={theme.text2 as string} />
          </Tap>
          <Tap theme={theme} onPress={() => setSheet('more')} style={s.iconBtn} label={t('Mehr')}>
            <Icon name="more" color={theme.text as string} strokeWidth={2.4} />
          </Tap>
        </View>
      )}

      {landingBody ?? (
      <GestureDetector gesture={selectGesture}>
        <Animated.View
          key={`${path}|${view}|${hits ? 'q' : ''}`}
          entering={m(hits ? fadeIn : pageIn(dir))}
          style={s.fill}
          onLayout={(e) => (listH.current = e.nativeEvent.layout.height)}
        >
          <FlatList
            ref={listRef}
            data={shown}
            numColumns={grid ? cols : 1}
            key={grid ? 'grid' : 'list'}
            keyExtractor={(e: Item) => e.path}
            contentContainerStyle={grid ? s.gridList : s.list}
            columnWrapperStyle={grid ? s.gridRow : undefined}
            getItemLayout={grid ? undefined : (_: unknown, i: number) => ({ length: ROW, offset: ROW * i, index: i })}
            scrollEnabled={scrollEnabled}
            // Scrollstand nur am Ende einer Bewegung merken (statt 60 JS-Aufrufe pro Sekunde)
            onScrollEndDrag={(e) => (scrollY.current = e.nativeEvent.contentOffset.y)}
            onMomentumScrollEnd={(e) => (scrollY.current = e.nativeEvent.contentOffset.y)}
            onScrollBeginDrag={() => setMenu(null)}
            initialNumToRender={14}
            maxToRenderPerBatch={8}
            windowSize={7}
            removeClippedSubviews
            onRefresh={hits ? undefined : load}
            refreshing={false}
            keyboardShouldPersistTaps="handled"
            ListEmptyComponent={
              hits && searching ? undefined : <Animated.View entering={m(fadeIn)} style={s.empty}>
                <View style={s.emptyIcon}>
                  <Icon name={error ? 'warning' : hits ? 'search' : 'folder'} size={28} color={theme.text3 as string} />
                </View>
                <Text style={s.emptyTitle}>{error ? t('Ordner nicht lesbar') : hits ? t('Keine Treffer') : t('Dieser Ordner ist leer')}</Text>
                <Text style={[s.meta, s.center]}>{error ?? (hits ? t('Anders schreiben oder weniger Buchstaben versuchen.') : t('Mit „+“ legst du hier einen Ordner oder eine Datei an.'))}</Text>
              </Animated.View>
            }
            renderItem={renderItem}
          />
        </Animated.View>
      </GestureDetector>
      )}

      {/* Unten: Aktionsleiste (ab zwei Dateien), „Hier einfügen“ oder „+“ */}
      {onLanding || showBar || clip ? null : !hits && sel.size === 0 ? (        <Animated.View entering={m(popIn)} exiting={m(popOut)} style={s.fabBox}>
          <Pressable style={s.fab} onPress={() => setSheet('create')} android_ripple={{ color: 'rgba(255,255,255,0.3)' }} accessibilityLabel={t('Neu')}>
            <Icon name="plus" size={26} color={theme.onAccent as string} strokeWidth={2} />
          </Pressable>
        </Animated.View>
      ) : null}

      {menu && sel.size > 0 && <ContextMenu
          x={menu.x}
          y={menu.y}
          theme={theme}
          actions={menuActions}
          onClose={() => {
            setMenu(null);
            // Nur zum Öffnen des Menüs markiert → beim Schließen wieder abwählen
            if (!showBar) {
              setSel(new Set());
            }
          }}
        />}

      <BottomSheet
        visible={sheet === 'create'}
        title={t('Neu')}
        theme={theme}
        onClose={() => setSheet(null)}
        actions={[
          { icon: 'folder', label: t('Neuer Ordner'), onPress: () => setNaming({ mode: 'folder', value: t('Neuer Ordner') }) },
          { icon: 'document', label: t('Neue Textdatei'), onPress: () => setNaming({ mode: 'file', value: `${t('Neue Textdatei')}.txt` }) },
        ]}
      />
      <BottomSheet visible={sheet === 'more'} theme={theme} onClose={() => setSheet(null)} actions={moreActions} />
      <BottomSheet visible={sheet === 'selection'} title={t('{n} ausgewählt', { n: sel.size })} theme={theme} onClose={() => setSheet(null)} actions={selectionActions} />

      {busy && (
        <Animated.View entering={m(toastIn)} style={s.busy} accessibilityRole="progressbar" accessibilityLabel={busy}>
          <ActivityIndicator color={theme.accent as string} />
          <Text style={s.busyText} numberOfLines={2}>{busy}</Text>
        </Animated.View>
      )}

      <Modal visible={!!pwAsk} transparent animationType="none" statusBarTranslucent onRequestClose={() => setPwAsk(null)}>
        <View style={s.modalBg}>
          <Animated.View entering={m(popIn)} style={s.modal}>
            <Text style={s.modalTitle}>{t('Passwort')}</Text>
            <Text style={[s.meta, s.pwHint]}>
              {pwAsk?.wrong ? t('Falsches Passwort.') : t('„{name}“ ist mit einem Passwort geschützt.', { name: pwAsk ? fileName(pwAsk.archive) : '' })}
            </Text>
            <TextInput
              autoFocus
              secureTextEntry
              autoCorrect={false}
              autoCapitalize="none"
              value={pwAsk?.value}
              onChangeText={(v) => setPwAsk((x) => (x ? { ...x, value: v } : x))}
              onSubmitEditing={() => pwAsk && (setPwAsk(null), extractAll([pwAsk.archive, ...pwAsk.rest], pwAsk.value))}
              style={s.input}
              accessibilityLabel={t('Passwort')}
            />
            <View style={s.modalBtns}>
              <BarBtn theme={theme} label={t('Abbrechen')} onPress={() => setPwAsk(null)} />
              <BarBtn theme={theme} label={t('Entpacken')} strong disabled={!pwAsk?.value} onPress={() => pwAsk && (setPwAsk(null), extractAll([pwAsk.archive, ...pwAsk.rest], pwAsk.value))} />
            </View>
          </Animated.View>
        </View>
      </Modal>

      <Modal visible={!!naming} transparent animationType="fade" onRequestClose={() => setNaming(null)}>
        <View style={s.modalBg}>
          <Animated.View entering={m(popIn)} style={s.modal}>
            <Text style={s.modalTitle}>
              {naming?.mode === 'folder' ? t('Neuer Ordner') : naming?.mode === 'file' ? t('Neue Textdatei') : t('Umbenennen')}
            </Text>
            <TextInput
              autoFocus
              selectTextOnFocus
              value={naming?.value}
              onChangeText={(v) => setNaming((n) => (n ? { ...n, value: v } : n))}
              onSubmitEditing={commitName}
              style={s.input}
            />
            <View style={s.modalBtns}>
              <Tap theme={theme} onPress={() => setNaming(null)} style={s.textBtn}><Text style={s.link}>{t('Abbrechen')}</Text></Tap>
              <Tap theme={theme} onPress={commitName} style={s.textBtn}><Text style={s.link}>{t('Speichern')}</Text></Tap>
            </View>
          </Animated.View>
        </View>
      </Modal>

      {remoteDev && (
        <View style={StyleSheet.absoluteFill}>
          <RemoteBrowser
            device={remoteDev}
            theme={theme}
            notify={notify}
            onClose={() => setRemoteDev(null)}
            onOpen={openPath}
            onStream={(uri, name) => setViewer({ items: [{ path: '', name, uri }], start: 0 })}
          />
        </View>
      )}
      {svg && (
        <SvgViewer
          path={svg.path}
          name={svg.name}
          theme={theme}
          onClose={() => setSvg(null)}
          onSource={() => { setSvg(null); setTextFile(svg); }}
        />
      )}
      {textFile && <TextViewer path={textFile.path} name={textFile.name} theme={theme} onClose={() => setTextFile(null)} />}
      {apk && <ApkSheet path={apk.path} name={apk.name} size={apk.size} theme={theme} notify={notify} onClose={() => setApk(null)} />}
      {archive && (
        <ArchiveViewer
          path={archive.path}
          name={archive.name}
          theme={theme}
          onClose={() => setArchive(null)}
          onExtract={() => {
            const a = archive.path;
            setArchive(null);
            extractAll([a]);
          }}
        />
      )}
      {pdf && <PdfViewer path={pdf.path} name={pdf.name} initialMode={pdf.mode} notify={notify} onChanged={load} onClose={() => setPdf(null)} />}
      {viewer && (
        <Viewer
          items={viewer.items}
          start={viewer.start}
          onClose={() => setViewer(null)}
          onEdit={(it) => {
            setViewer(null);
            setEditing({ path: it.path, name: it.name });
          }}
        />
      )}
      {editing && (
        <Editor
          path={editing.path}
          name={editing.name}
          notify={(text, undo) => notify(text, undo ? () => { undo(); setTimeout(load, 300); } : undefined)}
          onClose={(changed) => {
            setEditing(null);
            if (changed) {
              load();
            }
          }}
        />
      )}
    </View>
  );
}

/** Übergeordneter Ordner eines Pfads (für Suchtreffer). */
function parentPath(p: string): string {
  const i = p.lastIndexOf('/');
  return i > 0 ? p.slice(0, i).replace(/^\/storage\/emulated\/0/, '~') : p;
}

/** Brotkrumen unter dem Titel: „Interner Speicher › DCIM“ (ohne den aktuellen Ordner). */
function crumbOf(path: string, root: string, rootName: string): string {
  const rel = path.startsWith(root) ? path.slice(root.length) : path;
  const parts = rel.split('/').filter(Boolean).slice(0, -1);
  return [rootName, ...parts].join(' › ');
}

function SearchField({ theme, value, onChange, autoFocus }: { theme: Theme; value: string; onChange: (v: string) => void; autoFocus?: boolean }) {
  const s = styles(theme);
  return (
    <View style={s.search}>
      <Icon name="search" size={20} color={theme.text2 as string} />
      <TextInput
        value={value}
        onChangeText={onChange}
        autoFocus={autoFocus}
        placeholder={t('Überall suchen')}
        placeholderTextColor={theme.text2 as string}
        returnKeyType="search"
        autoCorrect={false}
        style={s.searchInput}
        accessibilityLabel={t('Überall suchen')}
      />
      {value.length > 0 && (
        <Pressable onPress={() => onChange('')} style={s.clear} accessibilityLabel={t('Suche leeren')}>
          <Icon name="close" size={18} color={theme.text2 as string} />
        </Pressable>
      )}
    </View>
  );
}

interface RowProps {
  e: Item;
  on: boolean;
  /** Ordner ist für gekoppelte Geräte freigegeben. */
  shared?: boolean;
  grid: boolean;
  tile: number;
  tileH: number;
  theme: Theme;
  inSearch: boolean;
  selecting: boolean;
  onOpen: (e: Item) => void;
}

const noop = () => {};

const FileRow = memo(function FileRow({ e, on, shared, grid, tile, tileH, theme, inSearch, selecting, onOpen }: RowProps) {
  const s = styles(theme);
  const press = () => onOpen(e);
  if (grid) {
    return (
      <View style={{ width: tile, height: tileH }}>
        <Tap theme={theme} onPress={press} onLongPress={noop} delayLongPress={LONG_PRESS} style={[s.tile, on && s.tileOn]} label={e.name} selected={on}>
          <View style={[s.tileThumb, { width: tile - 12, height: tile - 12 }]}>
            <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} size={56} />
            {e.kind === 'File' && (isMedia(e.name) || isAudio(e.name)) && <Thumb path={e.path} edge={320} style={s.fillImg} />}
            {on && (
              <Animated.View entering={m(popIn)} exiting={m(popOut)} style={[s.tileCheck, { backgroundColor: theme.accent }]}>
                <Icon name="check" size={16} color={theme.onAccent as string} strokeWidth={2.4} />
              </Animated.View>
            )}
          </View>
          <Text style={s.tileName} numberOfLines={2}>{e.name}</Text>
        </Tap>
      </View>
    );
  }
  const loc = locale();
  return (
    <Tap theme={theme} onPress={press} onLongPress={noop} delayLongPress={LONG_PRESS} style={[s.row, on && s.rowOn]} label={e.name} selected={on}>
      <View style={s.iconBox}>
        <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} />
        {e.kind === 'File' && (isMedia(e.name) || isAudio(e.name)) && <Thumb path={e.path} edge={160} style={s.thumbImg} />}
        {on && (
          <Animated.View entering={m(popIn)} exiting={m(popOut)} style={[s.check, { backgroundColor: theme.accent }]}>
            <Icon name="check" size={20} color={theme.onAccent as string} strokeWidth={2.4} />
          </Animated.View>
        )}
      </View>
      <View style={s.rowText}>
        <View style={s.rowMain}>
          <Text style={[s.name, e.hidden && s.dim]} numberOfLines={1}>{e.name}</Text>
          <Text style={s.meta} numberOfLines={1} ellipsizeMode={inSearch ? 'head' : 'tail'}>
            {e.fuzzy ? `${t('ähnlich')} · ` : ''}
            {shared ? `${t('Freigegeben')} · ` : ''}
            {inSearch ? parentPath(e.path) : relativeDate(e.mtime, loc, t('gestern'))}
          </Text>
        </View>
        {e.kind === 'Dir' ? (
          !selecting && <Icon name="chevron" size={20} color={theme.text3 as string} />
        ) : (
          <Text style={s.size}>{humanSize(e.size, loc)}</Text>
        )}
      </View>
    </Tap>
  );
});

function BarBtn({ label, onPress, theme, disabled, strong }: { label: string; onPress: () => void; theme: Theme; disabled?: boolean; strong?: boolean }) {
  const s = styles(theme);
  return (
    <Tap theme={theme} onPress={onPress} disabled={disabled} style={[s.barBtn, disabled && s.dim]} label={label}>
      <Text style={[s.barText, strong && { color: theme.accent }]} numberOfLines={1}>{label}</Text>
    </Tap>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1 },
    center: { textAlign: 'center' },
    landing: { paddingHorizontal: 16, paddingBottom: 40 },
    bigTitle: { color: th.text, fontSize: 26, lineHeight: 32, fontWeight: '700', marginTop: 12, marginBottom: 16 },
    search: { flexDirection: 'row', alignItems: 'center', minHeight: 48, paddingLeft: 16, gap: 12, borderRadius: 24, backgroundColor: th.surface2 },
    searchInput: { flex: 1, color: th.text, fontSize: 16, paddingVertical: 10 },
    clear: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    searchTop: { paddingHorizontal: 16, paddingTop: 4, paddingBottom: 4 },
    searchTopActive: { paddingRight: 16, paddingVertical: 4 },
    searchRow: { flexDirection: 'row', alignItems: 'center' },
    section: { marginTop: 28, marginBottom: 10, marginLeft: 4 },
    group: { borderRadius: 12, backgroundColor: th.surface, overflow: 'hidden' },
    groupLine: { height: StyleSheet.hairlineWidth, backgroundColor: th.line, marginLeft: 56 },
    placeRow: { flexDirection: 'row', alignItems: 'flex-start', gap: 16, paddingHorizontal: 16, paddingVertical: 14 },
    placeText: { flex: 1 },
    placeTop: { flexDirection: 'row', alignItems: 'baseline', gap: 8 },
    placeNum: { color: th.text2, fontSize: 13 },
    cardTitle: { flex: 1, color: th.text, fontSize: 16, lineHeight: 22, fontWeight: '600' },
    offline: { color: th.text2, fontWeight: '400' },
    usage: { height: 4, borderRadius: 2, marginTop: 8, marginBottom: 6, overflow: 'hidden', backgroundColor: th.surface2 },
    usageFill: { height: 4, borderRadius: 2, backgroundColor: th.accent },
    status: { flexDirection: 'row', alignItems: 'center', gap: 8, marginTop: 2 },
    onlineDot: { width: 7, height: 7, borderRadius: 4 },
    folderGrid: { flexDirection: 'row', flexWrap: 'wrap', marginHorizontal: -4 },
    folderCell: { width: '33.33%', padding: 4 },
    folderTile: { alignItems: 'center', paddingTop: 12, paddingBottom: 10, borderRadius: 12, overflow: 'hidden' },
    folderLabel: { color: th.text, fontSize: 13, lineHeight: 18, marginTop: 6 },
    header: { flexDirection: 'row', alignItems: 'center', minHeight: 60, paddingHorizontal: 4 },
    headerTitles: { flex: 1, marginLeft: 4 },
    noFlex: { flex: 0 },
    headerTitle: { flex: 1, color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    crumb: { color: th.text3, fontSize: 12, lineHeight: 16 },
    iconBtn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center', overflow: 'hidden' },
    textBtn: { minHeight: 48, minWidth: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 12 },
    link: { color: th.accent, fontSize: 15, fontWeight: '700' },
    list: { paddingBottom: 120 },
    row: { flexDirection: 'row', alignItems: 'center', height: ROW, paddingLeft: 16 },
    rowOn: { backgroundColor: th.accentSoft },
    rowText: {
      flex: 1, flexDirection: 'row', alignItems: 'center', marginLeft: 16, paddingRight: 16, alignSelf: 'stretch', gap: 12,
      borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: th.line,
    },
    rowMain: { flex: 1 },
    iconBox: { width: 40, height: 40, justifyContent: 'center', alignItems: 'center' },
    thumbImg: { width: 40, height: 40, borderRadius: 6 },
    check: { position: 'absolute', width: 40, height: 40, borderRadius: 20, justifyContent: 'center', alignItems: 'center' },
    name: { color: th.text, fontSize: 16, lineHeight: 22 },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
    size: { color: th.text3, fontSize: 13, minWidth: 56, textAlign: 'right' },
    dim: { opacity: 0.45 },
    gridList: { padding: 12, paddingBottom: 120 },
    gridRow: { gap: 8, marginBottom: 8 },
    tile: { flex: 1, alignItems: 'center', padding: 6, borderRadius: 12, overflow: 'hidden' },
    tileOn: { backgroundColor: th.accentSoft },
    tileThumb: { justifyContent: 'center', alignItems: 'center', borderRadius: 8, overflow: 'hidden', backgroundColor: th.surface },
    fillImg: { position: 'absolute', width: '100%', height: '100%' },
    tileCheck: { position: 'absolute', top: 6, right: 6, width: 24, height: 24, borderRadius: 12, justifyContent: 'center', alignItems: 'center' },
    tileName: { color: th.text, fontSize: 12, lineHeight: 16, textAlign: 'center', marginTop: 6 },
    empty: { alignItems: 'center', marginTop: 96, paddingHorizontal: 40 },
    emptyIcon: { width: 64, height: 64, borderRadius: 32, justifyContent: 'center', alignItems: 'center', marginBottom: 16, backgroundColor: th.surface2 },
    emptyTitle: { color: th.text, fontSize: 17, lineHeight: 24, fontWeight: '700', marginBottom: 6, textAlign: 'center' },
    bar: { flex: 1, flexDirection: 'row', alignItems: 'center' },
    barBtn: { flex: 1, minHeight: 56, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 6 },
    barText: { color: th.text, fontSize: 14, fontWeight: '600' },
    clipText: { flex: 1.6, color: th.text2, fontSize: 13, lineHeight: 18, paddingLeft: 16 },
    actionBar: { flex: 1, flexDirection: 'row', paddingHorizontal: 4, paddingVertical: 6 },
    actionBtn: { flex: 1, justifyContent: 'center', alignItems: 'center', gap: 4, borderRadius: 12, overflow: 'hidden' },
    actionText: { color: th.text, fontSize: 12, lineHeight: 16 },
    busy: {
      position: 'absolute', left: 12, right: 12, bottom: 12, minHeight: 52, flexDirection: 'row', alignItems: 'center', gap: 14,
      paddingHorizontal: 16, borderRadius: 10, backgroundColor: th.dark ? '#EDEBE6' : '#2A2A27', elevation: 6,
    },
    busyText: { flex: 1, color: th.dark ? '#1A1A18' : '#F4F2ED', fontSize: 14, lineHeight: 20 },
    pwHint: { marginBottom: 8 },
    fabBox: { position: 'absolute', right: 16, bottom: 16 },
    fab: { width: 56, height: 56, borderRadius: 16, overflow: 'hidden', backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center', elevation: 3 },
    modalBg: { flex: 1, backgroundColor: 'rgba(10,10,9,0.42)', justifyContent: 'center', padding: 24 },
    modal: { backgroundColor: th.surface, borderRadius: 16, paddingHorizontal: 24, paddingTop: 22, paddingBottom: 12 },
    modalTitle: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700', marginBottom: 12 },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 2, borderBottomColor: th.accent, paddingVertical: 8 },
    modalBtns: { flexDirection: 'row', justifyContent: 'flex-end', gap: 4, marginTop: 16 },
  }));
