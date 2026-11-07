// Dateien – Aufbau nach docs/MOBILE_UX.md:
// - Startseite: Suche (Ergebnisse schon beim Tippen, mit Tippfehler-Toleranz), Speicher, Ordner.
// - Explorer: Kopfzeile, flache Zeilen oder Raster, Übergänge beim Ordnerwechsel.
// - Markieren: lang halten – und ohne Loslassen über weitere Dateien wischen. Beim Loslassen
//   erscheint das Menü dort, wo der Finger zuerst lag (wie bei iOS).
// - Bilder und Videos öffnen in der eingebauten Anzeige, alles andere mit der passenden App.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Alert, BackHandler, Dimensions, FlatList, Modal, Pressable, ScrollView, StyleSheet, Text, TextInput, View,
} from 'react-native';
import { Gesture, GestureDetector } from 'react-native-gesture-handler';
import Animated, {
  LinearTransition, runOnJS,
} from 'react-native-reanimated';
import ContextMenu, { type MenuAction } from './ContextMenu';
import * as core from './core';
import { humanSize, joinPath, parentOf, request, type DirEntry } from './core';
import { locale, t } from './i18n';
import type { Theme } from './theme';
import { isMedia, Thumb } from './thumbs';
import { BottomSheet, Glyph, Tap, type SheetAction } from './ui';
import Viewer from './Viewer';
import { DUR, EASE_OUT, fadeIn, LONG_PRESS, m, pageIn, popIn, popOut, toastIn } from './motion';

interface Props {
  theme: Theme;
  root: string;
  notify: (text: string, undo?: () => void) => void;
  selectionChanged: (active: boolean) => void;
  /** Zähler: ändert er sich, geht es zurück zur Startansicht (erneutes Antippen von „Dateien“). */
  home?: number;
}

interface Item extends DirEntry {
  path: string;
  fuzzy?: boolean;
}

type Sort = 'name' | 'date' | 'size';
const ROW = 64;
const FOLDERS: [string, string][] = [
  ['Download', 'Downloads'],
  ['DCIM', 'Kamera'],
  ['Pictures', 'Bilder'],
  ['Documents', 'Dokumente'],
  ['Music', 'Musik'],
  ['Movies', 'Videos'],
];

export default function Files({ theme, root, notify, selectionChanged, home = 0 }: Props) {
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
  const [naming, setNaming] = useState<{ mode: 'folder' | 'file' | 'rename'; from?: string; value: string } | null>(null);
  const [sheet, setSheet] = useState<'create' | 'more' | 'selection' | null>(null);
  const [view, setView] = useState<'list' | 'grid'>('list');
  const [sort, setSort] = useState<Sort>('name');
  const [hidden, setHidden] = useState(false);
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<Item[] | null>(null);
  const [searching, setSearching] = useState(false); // Anfrage läuft noch
  const [viewer, setViewer] = useState<{ items: Item[]; start: number } | null>(null);
  const [storage, setStorage] = useState<{ total: number; free: number } | null>(null);
  const [folders, setFolders] = useState<[string, string][]>([]);
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
    const coll = new Intl.Collator(locale(), { numeric: true, sensitivity: 'base' });
    const v = [...items];
    v.sort((a, b) => {
      if ((a.kind === 'Dir') !== (b.kind === 'Dir')) {
        return a.kind === 'Dir' ? -1 : 1;
      }
      if (sort === 'date') {
        return b.mtime - a.mtime;
      }
      if (sort === 'size') {
        return b.size - a.size;
      }
      return coll.compare(a.name, b.name);
    });
    return v;
  }, [items, sort]);

  const shown = hits ?? sorted;
  const grid = view === 'grid' && !hits;
  const selected = shown.filter((e) => sel.has(e.path));

  // ------------------------------------------------------------ Aktionen
  const fail = (e: any) => notify(e?.message ?? String(e));
  const fileOp = (op: unknown) => request({ FileOp: { op, wait: true } });

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
    } else if (isMedia(e.name)) {
      // Alle Dateien des Ordners bzw. der Treffer: Wischen führt zur nächsten/vorigen Datei
      const files = shown.filter((x) => x.kind === 'File');
      setViewer({ items: files, start: Math.max(0, files.findIndex((x) => x.path === e.path)) });
    } else {
      core.openFile(e.path).catch(fail);
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

  // ------------------------------------------------------------ Startseite
  if (path === null && !hits) {
    const used = storage ? storage.total - storage.free : 0;
    return (
      <Animated.View entering={m(dir === 'back' ? pageIn('back') : fadeIn)} style={s.fill}>
        <ScrollView contentContainerStyle={s.landing} keyboardShouldPersistTaps="handled">
          <Text style={s.bigTitle}>{t('Dateien')}</Text>
          <SearchField theme={theme} value={query} onChange={setQuery} />
          <Text style={s.section}>{t('Speicher')}</Text>
          <Tap theme={theme} onPress={() => go(root, 'forward')} style={s.card} label={t('Interner Speicher')}>
            <View style={s.cardRow}>
              <View style={[s.driveIcon, { backgroundColor: theme.accentSoft }]}>
                <View style={[s.driveBar, { backgroundColor: theme.accent }]} />
              </View>
              <View style={s.grow}>
                <Text style={s.cardTitle}>{t('Interner Speicher')}</Text>
                {storage && (
                  <Text style={s.meta}>
                    {t('{used} von {total} belegt', { used: humanSize(used, locale()), total: humanSize(storage.total, locale()) })}
                  </Text>
                )}
                {storage && (
                  <View style={[s.usage, { backgroundColor: theme.accentSoft }]}>
                    <Animated.View
                      entering={m(fadeIn)}
                      style={[s.usageFill, { backgroundColor: theme.accent, width: `${Math.min(100, (used / storage.total) * 100)}%` }]}
                    />
                  </View>
                )}
              </View>
              <Text style={s.chev}>›</Text>
            </View>
          </Tap>
          {folders.length > 0 && <Text style={s.section}>{t('Standardordner')}</Text>}
          <View style={s.folderGrid}>
            {folders.map(([d, label]) => (
              <Animated.View key={d} style={s.folderCell}>
                <Tap theme={theme} onPress={() => go(joinPath(root, d), 'forward')} style={s.folderTile} label={t(label)}>
                  <Glyph name={d} dir theme={theme} size={44} />
                  <Text style={s.folderLabel} numberOfLines={1}>{t(label)}</Text>
                </Tap>
              </Animated.View>
            ))}
          </View>
        </ScrollView>
      </Animated.View>
    );
  }

  // ------------------------------------------------------------ Explorer / Suche
  const title = hits ? '' : path === root ? t('Interner Speicher') : path!.slice(path!.lastIndexOf('/') + 1);
  const single = selected.length === 1 ? selected[0] : null;
  const menuActions: MenuAction[] = [
    ...(single && single.kind === 'File' ? [{ label: t('Öffnen'), onPress: () => open({ ...single }) }] : []),
    { label: t('Teilen'), disabled: selected.some((e) => e.kind === 'Dir'), onPress: () => core.shareFiles(selected.map((e) => e.path)).catch(fail) },
    { label: t('Kopieren nach …'), onPress: () => { setClip({ mode: 'copy', paths: selected.map((e) => e.path) }); setSel(new Set()); } },
    { label: t('Verschieben nach …'), onPress: () => { setClip({ mode: 'move', paths: selected.map((e) => e.path) }); setSel(new Set()); } },
    { label: t('Umbenennen'), disabled: !single || !!hits, onPress: () => single && setNaming({ mode: 'rename', from: single.name, value: single.name }) },
    { label: t('In den Papierkorb'), separate: true, onPress: trash },
    { label: t('Endgültig löschen'), danger: true, onPress: deleteForever },
  ];
  const toClip = (mode: 'copy' | 'move') => {
    setClip({ mode, paths: selected.map((e) => e.path) });
    setSel(new Set());
  };
  const barActions = [
    { icon: '⇪', label: t('Teilen'), disabled: selected.some((e) => e.kind === 'Dir'), onPress: () => core.shareFiles(selected.map((e) => e.path)).catch(fail) },
    { icon: '⧉', label: t('Kopieren'), onPress: () => toClip('copy') },
    { icon: '➜', label: t('Verschieben'), onPress: () => toClip('move') },
    { icon: '🗑', label: t('Papierkorb'), danger: true, onPress: trash },
    { icon: '⋯', label: t('Mehr'), onPress: () => setSheet('selection') },
  ];
  const selectionActions: SheetAction[] = [
    { label: t('Umbenennen'), disabled: !single || !!hits, onPress: () => single && setNaming({ mode: 'rename', from: single.name, value: single.name }) },
    { label: t('Endgültig löschen'), danger: true, onPress: deleteForever },
  ];
  const moreActions: SheetAction[] = [
    { label: view === 'list' ? t('Als Raster anzeigen') : t('Als Liste anzeigen'), onPress: () => setView(view === 'list' ? 'grid' : 'list') },
    { label: t('Nach Name sortieren'), onPress: () => setSort('name'), disabled: sort === 'name' },
    { label: t('Nach Datum sortieren'), onPress: () => setSort('date'), disabled: sort === 'date' },
    { label: t('Nach Größe sortieren'), onPress: () => setSort('size'), disabled: sort === 'size' },
    { label: hidden ? t('Versteckte Dateien ausblenden') : t('Versteckte Dateien anzeigen'), onPress: () => setHidden(!hidden) },
  ];

  const renderItem = ({ item: e }: { item: Item }) => {
    const on = sel.has(e.path);
    if (grid) {
      return (
        <Animated.View style={{ width: tile, height: tileH }}>
          <Tap theme={theme} onPress={() => open(e)} onLongPress={() => {}} delayLongPress={LONG_PRESS} style={[s.tile, on && s.tileOn]} label={e.name} selected={on}>
            <View style={[s.tileThumb, { width: tile - 12, height: tile - 12 }]}>
              <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} size={56} />
              {e.kind === 'File' && isMedia(e.name) && <Thumb path={e.path} edge={320} style={s.fillImg} />}
              {on && (
                <Animated.View entering={m(popIn)} exiting={m(popOut)} style={[s.tileCheck, { backgroundColor: theme.accent }]}>
                  <Text style={{ color: theme.onAccent }}>✓</Text>
                </Animated.View>
              )}
            </View>
            <Text style={s.tileName} numberOfLines={2}>{e.name}</Text>
          </Tap>
        </Animated.View>
      );
    }
    return (
      <Animated.View>
        <Tap theme={theme} onPress={() => open(e)} onLongPress={() => {}} delayLongPress={LONG_PRESS} style={[s.row, on && s.rowOn]} label={e.name} selected={on}>
          <View style={s.iconBox}>
            <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} />
            {e.kind === 'File' && isMedia(e.name) && <Thumb path={e.path} edge={160} style={s.thumbImg} />}
            {on && (
              <Animated.View entering={m(popIn)} exiting={m(popOut)} style={[s.check, { backgroundColor: theme.accent }]}>
                <Text style={{ color: theme.onAccent }}>✓</Text>
              </Animated.View>
            )}
          </View>
          <View style={s.rowText}>
            <Text style={[s.name, e.hidden && s.dim]} numberOfLines={1}>{e.name}</Text>
            <Text style={s.meta} numberOfLines={1}>
              {e.fuzzy ? `${t('ähnlich')} · ` : ''}
              {hits ? e.path : `${e.kind === 'Dir' ? t('Ordner') : humanSize(e.size, locale())} • ${new Date(e.mtime * 1000).toLocaleDateString(locale())}`}
            </Text>
          </View>
          {e.kind === 'Dir' && !sel.size && <Text style={s.chev}>›</Text>}
        </Tap>
      </Animated.View>
    );
  };

  return (
    <View style={s.fill}>
      {/* Kopfzeile */}
      {sel.size > 0 ? (
        <Animated.View entering={m(fadeIn)} style={s.header}>
          <Tap theme={theme} onPress={() => { setSel(new Set()); setMenu(null); }} style={s.iconBtn} label={t('Auswahl aufheben')}>
            <Text style={s.iconText}>✕</Text>
          </Tap>
          <Text style={s.headerTitle}>{t('{n} ausgewählt', { n: sel.size })}</Text>
          <Tap theme={theme} onPress={() => setSel(new Set(shown.map((e) => e.path)))} style={s.textBtn}>
            <Text style={s.link}>{t('Alle')}</Text>
          </Tap>
          {showBar ? (
            <View style={s.iconBtn} />
          ) : (
            <Tap theme={theme} onPress={() => setMenu({ x: W - 24, y: 96 })} style={s.iconBtn} label={t('Aktionen')}>
              <Text style={s.iconText}>⋯</Text>
            </Tap>
          )}
        </Animated.View>
      ) : hits ? (
        <View style={s.searchHeader}>
          <Tap theme={theme} onPress={() => setQuery('')} style={s.iconBtn} label={t('Zurück')}>
            <Text style={s.iconText}>‹</Text>
          </Tap>
          <View style={s.grow}>
            <SearchField theme={theme} value={query} onChange={setQuery} autoFocus />
          </View>
        </View>
      ) : (
        <View style={s.header}>
          <Tap theme={theme} onPress={() => go(parentOf(path!, root) ?? null, 'back')} style={s.iconBtn} label={t('Zurück')}>
            <Text style={s.iconText}>‹</Text>
          </Tap>
          <Text style={s.headerTitle} numberOfLines={1}>{title}</Text>
          <Tap theme={theme} onPress={() => setView(view === 'list' ? 'grid' : 'list')} style={s.iconBtn} label={view === 'list' ? t('Als Raster anzeigen') : t('Als Liste anzeigen')}>
            <Text style={s.iconSmall}>{view === 'list' ? '▦' : '☰'}</Text>
          </Tap>
          <Tap theme={theme} onPress={() => setSheet('more')} style={s.iconBtn} label={t('Mehr')}>
            <Text style={s.iconText}>⋯</Text>
          </Tap>
        </View>
      )}

      <GestureDetector gesture={selectGesture}>
        <Animated.View
          key={`${path}|${view}|${hits ? 'q' : ''}`}
          entering={m(hits ? fadeIn : pageIn(dir))}
          style={s.fill}
          onLayout={(e) => (listH.current = e.nativeEvent.layout.height)}
        >
          <Animated.FlatList
            ref={listRef as any}
            data={shown}
            numColumns={grid ? cols : 1}
            key={grid ? 'grid' : 'list'}
            keyExtractor={(e: Item) => e.path}
            contentContainerStyle={grid ? s.gridList : s.list}
            columnWrapperStyle={grid ? s.gridRow : undefined}
            getItemLayout={grid ? undefined : (_: unknown, i: number) => ({ length: ROW, offset: ROW * i, index: i })}
            itemLayoutAnimation={m(LinearTransition.duration(DUR.page).easing(EASE_OUT))}
            scrollEnabled={scrollEnabled}
            onScroll={(e: any) => (scrollY.current = e.nativeEvent.contentOffset.y)}
            scrollEventThrottle={16}
            onScrollBeginDrag={() => setMenu(null)}
            onRefresh={hits ? undefined : load}
            refreshing={false}
            keyboardShouldPersistTaps="handled"
            ListEmptyComponent={
              hits && searching ? undefined : <Animated.View entering={m(fadeIn)} style={s.empty}>
                <View style={[s.emptyIcon, { backgroundColor: theme.accentSoft }]}>
                  <Text style={[s.emptyPlus, { color: theme.accent }]}>{hits ? '∅' : '+'}</Text>
                </View>
                <Text style={s.emptyTitle}>{error ? t('Ordner nicht lesbar') : hits ? t('Keine Treffer') : t('Dieser Ordner ist leer')}</Text>
                <Text style={s.meta}>{error ?? (hits ? '' : t('Mit „+“ legst du hier einen Ordner oder eine Datei an.'))}</Text>
              </Animated.View>
            }
            renderItem={renderItem}
          />
        </Animated.View>
      </GestureDetector>

      {/* Unten: Aktionsleiste (ab zwei Dateien), „Hier einfügen“ oder „+“ */}
      {showBar ? (
        <Animated.View entering={m(toastIn)} style={s.actionBar} accessibilityRole="toolbar">
          {barActions.map((a) => (
            <Tap key={a.label} theme={theme} onPress={a.onPress} disabled={a.disabled} style={[s.actionBtn, a.disabled && s.dim]} label={a.label}>
              <Text style={[s.actionIcon, a.danger && { color: theme.danger }]}>{a.icon}</Text>
              <Text style={[s.actionText, a.danger && { color: theme.danger }]} numberOfLines={1}>{a.label}</Text>
            </Tap>
          ))}
        </Animated.View>
      ) : clip ? (
        <Animated.View entering={m(toastIn)} style={s.bar}>
          <Text style={s.clipText} numberOfLines={2}>
            {clip.mode === 'copy' ? t('{n} zum Kopieren ausgewählt', { n: clip.paths.length }) : t('{n} zum Verschieben ausgewählt', { n: clip.paths.length })}
          </Text>
          <BarBtn theme={theme} label={t('Abbrechen')} onPress={() => setClip(null)} />
          <BarBtn theme={theme} label={t('Hier einfügen')} strong disabled={!!hits || path === null} onPress={paste} />
        </Animated.View>
      ) : !hits && sel.size === 0 ? (
        <Animated.View entering={m(popIn)} exiting={m(popOut)} style={s.fabBox}>
          <Pressable style={s.fab} onPress={() => setSheet('create')} android_ripple={{ color: 'rgba(255,255,255,0.3)' }} accessibilityLabel={t('Neu')}>
            <Text style={s.fabText}>+</Text>
          </Pressable>
        </Animated.View>
      ) : null}

      {menu && sel.size > 0 && <ContextMenu x={menu.x} y={menu.y} theme={theme} actions={menuActions} onClose={() => setMenu(null)} />}

      <BottomSheet
        visible={sheet === 'create'}
        title={t('Neu')}
        theme={theme}
        onClose={() => setSheet(null)}
        actions={[
          { label: t('Neuer Ordner'), onPress: () => setNaming({ mode: 'folder', value: t('Neuer Ordner') }) },
          { label: t('Neue Textdatei'), onPress: () => setNaming({ mode: 'file', value: `${t('Neue Textdatei')}.txt` }) },
        ]}
      />
      <BottomSheet visible={sheet === 'more'} theme={theme} onClose={() => setSheet(null)} actions={moreActions} />
      <BottomSheet visible={sheet === 'selection'} title={t('{n} ausgewählt', { n: sel.size })} theme={theme} onClose={() => setSheet(null)} actions={selectionActions} />

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

      {viewer && <Viewer items={viewer.items} start={viewer.start} onClose={() => setViewer(null)} />}
    </View>
  );
}

function SearchField({ theme, value, onChange, autoFocus }: { theme: Theme; value: string; onChange: (v: string) => void; autoFocus?: boolean }) {
  const s = styles(theme);
  return (
    <View style={s.search}>
      <Text style={s.searchIcon}>⌕</Text>
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
          <Text style={s.searchIcon}>✕</Text>
        </Pressable>
      )}
    </View>
  );
}

function BarBtn({ label, onPress, theme, disabled, strong }: { label: string; onPress: () => void; theme: Theme; disabled?: boolean; strong?: boolean }) {
  const s = styles(theme);
  return (
    <Tap theme={theme} onPress={onPress} disabled={disabled} style={[s.barBtn, disabled && s.dim]} label={label}>
      <Text style={[s.barText, strong && { color: theme.accent }]} numberOfLines={1}>{label}</Text>
    </Tap>
  );
}

const styles = (th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1 },
    landing: { padding: 16, paddingBottom: 40 },
    bigTitle: { color: th.text, fontSize: 30, fontWeight: '700', marginTop: 4, marginBottom: 14 },
    search: {
      flexDirection: 'row', alignItems: 'center', minHeight: 48, paddingLeft: 14, borderRadius: 16,
      backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke,
    },
    searchIcon: { color: th.text2, fontSize: 18, marginRight: 8 },
    searchInput: { flex: 1, color: th.text, fontSize: 16, paddingVertical: 10 },
    clear: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    searchHeader: { flexDirection: 'row', alignItems: 'center', paddingRight: 12, paddingVertical: 4 },
    section: { color: th.text2, fontSize: 13, fontWeight: '700', marginTop: 24, marginBottom: 8, marginLeft: 4, letterSpacing: 0.3 },
    card: { borderRadius: 18, backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke, overflow: 'hidden' },
    cardRow: { flexDirection: 'row', alignItems: 'center', padding: 16 },
    cardTitle: { color: th.text, fontSize: 16, fontWeight: '600' },
    driveIcon: { width: 44, height: 44, borderRadius: 12, justifyContent: 'flex-end', padding: 8, marginRight: 14 },
    driveBar: { height: 6, borderRadius: 3 },
    usage: { height: 6, borderRadius: 3, marginTop: 8, overflow: 'hidden' },
    usageFill: { height: 6, borderRadius: 3 },
    folderGrid: { flexDirection: 'row', flexWrap: 'wrap', gap: 10 },
    folderCell: { width: '31%' },
    folderTile: {
      alignItems: 'center', paddingVertical: 14, borderRadius: 16, overflow: 'hidden',
      backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke,
    },
    folderLabel: { color: th.text, fontSize: 13, marginTop: 6 },
    header: { flexDirection: 'row', alignItems: 'center', height: 56, paddingHorizontal: 4 },
    headerTitle: { flex: 1, color: th.text, fontSize: 18, fontWeight: '600', textAlign: 'center' },
    iconBtn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center', overflow: 'hidden' },
    iconText: { color: th.text, fontSize: 26, lineHeight: 30 },
    iconSmall: { color: th.text, fontSize: 18 },
    textBtn: { minHeight: 48, minWidth: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 12 },
    link: { color: th.accent, fontSize: 16, fontWeight: '600' },
    list: { paddingBottom: 120 },
    row: { flexDirection: 'row', alignItems: 'center', height: ROW, paddingHorizontal: 16 },
    rowOn: { backgroundColor: th.accentSoft },
    rowText: {
      flex: 1, marginLeft: 14, alignSelf: 'stretch', justifyContent: 'center',
      borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: th.stroke,
    },
    iconBox: { width: 40, height: 40, justifyContent: 'center', alignItems: 'center' },
    thumbImg: { width: 40, height: 40, borderRadius: 8 },
    check: { position: 'absolute', width: 40, height: 40, borderRadius: 20, justifyContent: 'center', alignItems: 'center' },
    name: { color: th.text, fontSize: 16 },
    meta: { color: th.text2, fontSize: 13, marginTop: 2 },
    chev: { color: th.text2, fontSize: 22, marginLeft: 8 },
    dim: { opacity: 0.45 },
    gridList: { padding: 16, paddingBottom: 120 },
    gridRow: { gap: 8, marginBottom: 8 },
    tile: { flex: 1, alignItems: 'center', padding: 6, borderRadius: 14, overflow: 'hidden' },
    tileOn: { backgroundColor: th.accentSoft },
    tileThumb: { justifyContent: 'center', alignItems: 'center', borderRadius: 10, overflow: 'hidden' },
    fillImg: { width: '100%', height: '100%' },
    tileCheck: { position: 'absolute', top: 6, right: 6, width: 26, height: 26, borderRadius: 13, justifyContent: 'center', alignItems: 'center' },
    tileName: { color: th.text, fontSize: 12, textAlign: 'center', marginTop: 4 },
    empty: { alignItems: 'center', marginTop: 80, paddingHorizontal: 32 },
    emptyIcon: { width: 64, height: 64, borderRadius: 32, justifyContent: 'center', alignItems: 'center', marginBottom: 14 },
    emptyPlus: { fontSize: 30 },
    emptyTitle: { color: th.text, fontSize: 18, fontWeight: '700', marginBottom: 6, textAlign: 'center' },
    bar: {
      position: 'absolute', left: 12, right: 12, bottom: 12, flexDirection: 'row', alignItems: 'center', borderRadius: 18,
      backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke, elevation: 6, overflow: 'hidden',
    },
    barBtn: { flex: 1, minHeight: 56, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 6 },
    barText: { color: th.text, fontSize: 13, fontWeight: '600' },
    clipText: { flex: 1.6, color: th.text2, fontSize: 13, paddingLeft: 16 },
    actionBar: {
      position: 'absolute', left: 0, right: 0, bottom: 0, height: 72, flexDirection: 'row', paddingHorizontal: 8, paddingTop: 6, paddingBottom: 10,
      backgroundColor: th.dark ? '#2b2d36' : '#ffffff', borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: th.stroke, elevation: 12,
    },
    actionBtn: { flex: 1, justifyContent: 'center', alignItems: 'center', borderRadius: 14, overflow: 'hidden' },
    actionIcon: { color: th.text, fontSize: 19, lineHeight: 24 },
    actionText: { color: th.text, fontSize: 12, marginTop: 2 },
    fabBox: { position: 'absolute', right: 20, bottom: 20 },
    fab: {
      width: 60, height: 60, borderRadius: 20, overflow: 'hidden',
      backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center', elevation: 6,
    },
    fabText: { color: th.onAccent, fontSize: 30, lineHeight: 34 },
    modalBg: { flex: 1, backgroundColor: 'rgba(0,0,0,0.45)', justifyContent: 'center', padding: 28 },
    modal: { backgroundColor: th.dark ? '#2a2c34' : '#ffffff', borderRadius: 24, padding: 22 },
    modalTitle: { color: th.text, fontSize: 18, fontWeight: '600', marginBottom: 12 },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 2, borderBottomColor: th.accent, paddingVertical: 8 },
    modalBtns: { flexDirection: 'row', justifyContent: 'flex-end', marginTop: 12 },
  });
