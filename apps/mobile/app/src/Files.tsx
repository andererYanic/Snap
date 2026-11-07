// Dateien – Aufbau nach docs/MOBILE_UX.md (gelernt u. a. von Spacedrive):
// - Startseite statt Dateisystem-Wurzel: Suche, Speicher mit Belegung, Standardordner.
// - Explorer: Kopfzeile mit Zurück, Ordnername, Ansicht (Liste/Raster) und „⋯“.
// - Flache Zeilen mit Trennlinie, Symbol nach Dateityp, Metazeile „Größe • Datum“.
// - Antippen öffnet (Ordner hier, Dateien mit der passenden App), langes Drücken wählt aus.
// - Aktionen unten (Daumenzone); Verschieben/Kopieren über eine „Hier einfügen“-Leiste.
// - Papierkorb mit „Rückgängig“ statt Rückfragen; nur Endgültiges wird bestätigt.
import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Alert, BackHandler, Dimensions, FlatList, Image, Modal, Pressable, ScrollView, StyleSheet, Text, TextInput, View,
} from 'react-native';
import * as core from './core';
import { humanSize, joinPath, parentOf, request, type DirEntry } from './core';
import { locale, t } from './i18n';
import type { Theme } from './theme';
import { BottomSheet, Glyph, Tap, type SheetAction } from './ui';

interface Props {
  theme: Theme;
  root: string;
  notify: (text: string, undo?: () => void) => void;
  selectionChanged: (active: boolean) => void;
}

interface Item extends DirEntry {
  path: string;
}

type Sort = 'name' | 'date' | 'size';
const IMAGE = /\.(jpe?g|png|gif|webp|heic|heif|bmp|avif)$/i;
const FOLDERS: [string, string][] = [
  ['Download', 'Downloads'],
  ['DCIM', 'Kamera'],
  ['Pictures', 'Bilder'],
  ['Documents', 'Dokumente'],
  ['Music', 'Musik'],
  ['Movies', 'Videos'],
];

export default function Files({ theme, root, notify, selectionChanged }: Props) {
  const [path, setPath] = useState<string | null>(null); // null = Startseite
  const [items, setItems] = useState<Item[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [clip, setClip] = useState<{ mode: 'copy' | 'move'; paths: string[] } | null>(null);
  const [naming, setNaming] = useState<{ mode: 'folder' | 'file' | 'rename'; from?: string; value: string } | null>(null);
  const [sheet, setSheet] = useState<'create' | 'more' | 'selection' | null>(null);
  const [view, setView] = useState<'list' | 'grid'>('list');
  const [sort, setSort] = useState<Sort>('name');
  const [hidden, setHidden] = useState(false);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [query, setQuery] = useState('');
  const [hits, setHits] = useState<Item[] | null>(null);
  const [storage, setStorage] = useState<{ total: number; free: number } | null>(null);
  const [folders, setFolders] = useState<[string, string][]>([]);
  const s = styles(theme);

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
    load();
  }, [load]);

  // Startseite: Speicherbelegung und vorhandene Standardordner
  useEffect(() => {
    if (path !== null) {
      return;
    }
    core.storageInfo().then(setStorage).catch(() => {});
    request<{ entries: DirEntry[] }>({ ListDir: { path: root, show_hidden: false, offset: 0, limit: 1000 } })
      .then((r) => {
        const names = new Set(r.entries.filter((e) => e.kind === 'Dir').map((e) => e.name));
        setFolders(FOLDERS.filter(([dir]) => names.has(dir)));
      })
      .catch(() => {});
  }, [path, root]);

  useEffect(() => selectionChanged(sel.size > 0 || clip !== null), [sel, clip, selectionChanged]);

  // Zurück: Auswahl aufheben → Suche schließen → Ordner hoch → Startseite
  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', () => {
      if (sel.size) {
        setSel(new Set());
        return true;
      }
      if (hits) {
        setHits(null);
        setQuery('');
        return true;
      }
      if (path !== null) {
        setPath(parentOf(path, root) ?? null);
        return true;
      }
      return false;
    });
    return () => sub.remove();
  }, [path, root, sel, hits]);

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

  // Vorschaubilder für Bilder (aus dem Cache des Kerns)
  useEffect(() => {
    shown.filter((e) => e.kind === 'File' && IMAGE.test(e.name)).slice(0, 80).forEach((e) => {
      if (thumbs[e.path] !== undefined) {
        return;
      }
      setThumbs((m) => ({ ...m, [e.path]: '' }));
      request<{ path: string }>({ Thumbnail: { path: e.path, size: 'Small' } })
        .then((th) => setThumbs((m) => ({ ...m, [e.path]: `file://${th.path}` })))
        .catch(() => {});
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [shown]);

  // ------------------------------------------------------------ Aktionen
  const fail = (e: any) => notify(e?.message ?? String(e));
  const fileOp = (op: unknown) => request({ FileOp: { op, wait: true } });
  const selected = shown.filter((e) => sel.has(e.path));

  const toggle = (p: string) => {
    const n = new Set(sel);
    n.has(p) ? n.delete(p) : n.add(p);
    setSel(n);
  };

  const open = (e: Item) => {
    if (sel.size) {
      toggle(e.path);
    } else if (e.kind === 'Dir') {
      setHits(null);
      setQuery('');
      setPath(e.path);
    } else {
      core.openFile(e.path).catch(fail);
    }
  };

  const search = async () => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    try {
      const r = await request<{ hits: { path: string; name: string; kind: DirEntry['kind']; size: number; mtime: number }[] }>({
        Search: { query: q, filter: {}, limit: 300 },
      });
      setHits(r.hits.map((h) => ({ ...h, hidden: h.name.startsWith('.') })));
    } catch (e) {
      fail(e);
    }
  };

  const trash = async () => {
    const paths = selected.map((e) => e.path);
    try {
      await fileOp({ Trash: { paths } });
      setSel(new Set());
      load();
      notify(
        paths.length === 1 ? t('„{name}“ in den Papierkorb verschoben.', { name: selected[0].name }) : t('{n} Elemente in den Papierkorb verschoben.', { n: paths.length }),
        async () => {
          await request('Undo');
          load();
        },
      );
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
      const op = clip.mode === 'copy' ? { Copy: { sources: clip.paths, dest: path, conflict: 'KeepBoth' } } : { Move: { sources: clip.paths, dest: path, conflict: 'KeepBoth' } };
      await fileOp(op);
      notify(clip.mode === 'copy' ? t('{n} Elemente kopiert.', { n: clip.paths.length }) : t('{n} Elemente verschoben.', { n: clip.paths.length }), async () => {
        await request('Undo');
        load();
      });
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

  // ------------------------------------------------------------ Startseite
  if (path === null && !hits) {
    const used = storage ? storage.total - storage.free : 0;
    return (
      <ScrollView contentContainerStyle={s.landing} keyboardShouldPersistTaps="handled">
        <Text style={s.bigTitle}>{t('Dateien')}</Text>
        <SearchField theme={theme} value={query} onChange={setQuery} onSubmit={search} />
        <Text style={s.section}>{t('Speicher')}</Text>
        <Tap theme={theme} onPress={() => setPath(root)} style={s.card} label={t('Interner Speicher')}>
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
                  <View style={[s.usageFill, { backgroundColor: theme.accent, width: `${Math.min(100, (used / storage.total) * 100)}%` }]} />
                </View>
              )}
            </View>
            <Text style={s.chev}>›</Text>
          </View>
        </Tap>
        {folders.length > 0 && <Text style={s.section}>{t('Ordner')}</Text>}
        <View style={s.folderGrid}>
          {folders.map(([dir, label]) => (
            <Tap key={dir} theme={theme} onPress={() => setPath(joinPath(root, dir))} style={s.folderTile} label={t(label)}>
              <Glyph name={dir} dir theme={theme} size={44} />
              <Text style={s.folderLabel} numberOfLines={1}>{t(label)}</Text>
            </Tap>
          ))}
        </View>
      </ScrollView>
    );
  }

  // ------------------------------------------------------------ Explorer / Suchergebnisse
  const title = hits ? t('Suche: „{q}“', { q: query.trim() }) : path === root ? t('Interner Speicher') : path!.slice(path!.lastIndexOf('/') + 1);
  const cols = 3;
  const tile = Math.floor((Dimensions.get('window').width - 32 - (cols - 1) * 8) / cols);

  const moreActions: SheetAction[] = [
    { label: view === 'list' ? t('Als Raster anzeigen') : t('Als Liste anzeigen'), onPress: () => setView(view === 'list' ? 'grid' : 'list') },
    { label: t('Nach Name sortieren'), onPress: () => setSort('name'), disabled: sort === 'name' },
    { label: t('Nach Datum sortieren'), onPress: () => setSort('date'), disabled: sort === 'date' },
    { label: t('Nach Größe sortieren'), onPress: () => setSort('size'), disabled: sort === 'size' },
    { label: hidden ? t('Versteckte Dateien ausblenden') : t('Versteckte Dateien anzeigen'), onPress: () => setHidden(!hidden) },
  ];
  const selectionActions: SheetAction[] = [
    { label: t('Kopieren nach …'), onPress: () => { setClip({ mode: 'copy', paths: selected.map((e) => e.path) }); setSel(new Set()); } },
    { label: t('Umbenennen'), disabled: selected.length !== 1, onPress: () => setNaming({ mode: 'rename', from: selected[0].name, value: selected[0].name }) },
    { label: t('Endgültig löschen'), danger: true, onPress: deleteForever },
  ];

  return (
    <View style={s.fill}>
      {/* Kopfzeile: Zurück, Titel, Ansicht, Mehr – oder Auswahlmodus */}
      {sel.size > 0 ? (
        <View style={s.header}>
          <Tap theme={theme} onPress={() => setSel(new Set())} style={s.iconBtn} label={t('Auswahl aufheben')}>
            <Text style={s.iconText}>✕</Text>
          </Tap>
          <Text style={s.headerTitle}>{t('{n} ausgewählt', { n: sel.size })}</Text>
          <Tap theme={theme} onPress={() => setSel(new Set(shown.map((e) => e.path)))} style={s.textBtn}>
            <Text style={s.link}>{t('Alle')}</Text>
          </Tap>
        </View>
      ) : (
        <View style={s.header}>
          <Tap theme={theme} onPress={() => (hits ? (setHits(null), setQuery('')) : setPath(parentOf(path!, root) ?? null))} style={s.iconBtn} label={t('Zurück')}>
            <Text style={s.iconText}>‹</Text>
          </Tap>
          <Text style={s.headerTitle} numberOfLines={1}>{title}</Text>
          {!hits && (
            <Tap theme={theme} onPress={() => setView(view === 'list' ? 'grid' : 'list')} style={s.iconBtn} label={view === 'list' ? t('Als Raster anzeigen') : t('Als Liste anzeigen')}>
              <Text style={s.iconSmall}>{view === 'list' ? '▦' : '☰'}</Text>
            </Tap>
          )}
          {!hits && (
            <Tap theme={theme} onPress={() => setSheet('more')} style={s.iconBtn} label={t('Mehr')}>
              <Text style={s.iconText}>⋯</Text>
            </Tap>
          )}
        </View>
      )}

      <FlatList
        key={view}
        data={shown}
        numColumns={view === 'grid' && !hits ? cols : 1}
        keyExtractor={(e) => e.path}
        contentContainerStyle={view === 'grid' && !hits ? s.gridList : s.list}
        columnWrapperStyle={view === 'grid' && !hits ? s.gridRow : undefined}
        onRefresh={load}
        refreshing={false}
        ListEmptyComponent={
          <View style={s.empty}>
            <View style={[s.emptyIcon, { backgroundColor: theme.accentSoft }]}><Text style={[s.emptyPlus, { color: theme.accent }]}>{hits ? '∅' : '+'}</Text></View>
            <Text style={s.emptyTitle}>{error ? t('Ordner nicht lesbar') : hits ? t('Keine Treffer') : t('Dieser Ordner ist leer')}</Text>
            <Text style={s.meta}>{error ?? (hits ? '' : t('Mit „+“ legst du hier einen Ordner oder eine Datei an.'))}</Text>
          </View>
        }
        renderItem={({ item: e }) => {
          const on = sel.has(e.path);
          if (view === 'grid' && !hits) {
            return (
              <Tap theme={theme} onPress={() => open(e)} onLongPress={() => toggle(e.path)} style={[s.tile, { width: tile }, on && s.tileOn]} label={e.name} selected={on}>
                <View style={[s.tileThumb, { width: tile - 12, height: tile - 12 }]}>
                  {thumbs[e.path] ? <Image source={{ uri: thumbs[e.path] }} style={s.tileImg} /> : <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} size={56} />}
                </View>
                <Text style={s.tileName} numberOfLines={2}>{e.name}</Text>
              </Tap>
            );
          }
          return (
            <Tap theme={theme} onPress={() => open(e)} onLongPress={() => toggle(e.path)} style={[s.row, on && s.rowOn]} label={e.name} selected={on}>
              {on ? (
                <View style={[s.check, { backgroundColor: theme.accent }]}><Text style={{ color: theme.onAccent }}>✓</Text></View>
              ) : thumbs[e.path] ? (
                <Image source={{ uri: thumbs[e.path] }} style={s.thumb} />
              ) : (
                <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} />
              )}
              <View style={s.rowText}>
                <Text style={[s.name, e.hidden && s.dim]} numberOfLines={1}>{e.name}</Text>
                <Text style={s.meta} numberOfLines={1}>
                  {hits ? e.path : `${e.kind === 'Dir' ? t('Ordner') : humanSize(e.size, locale())} • ${new Date(e.mtime * 1000).toLocaleDateString(locale())}`}
                </Text>
              </View>
              {e.kind === 'Dir' && !sel.size && <Text style={s.chev}>›</Text>}
            </Tap>
          );
        }}
      />

      {/* Unten: Auswahl-Aktionen, „Hier einfügen“ oder „+“ */}
      {sel.size > 0 ? (
        <View style={s.bar}>
          <BarBtn theme={theme} label={t('Teilen')} disabled={selected.some((e) => e.kind === 'Dir')} onPress={() => core.shareFiles(selected.map((e) => e.path)).catch(fail)} />
          <BarBtn theme={theme} label={t('Verschieben')} onPress={() => { setClip({ mode: 'move', paths: selected.map((e) => e.path) }); setSel(new Set()); }} />
          <BarBtn theme={theme} label={t('Papierkorb')} onPress={trash} />
          <BarBtn theme={theme} label={t('Mehr')} onPress={() => setSheet('selection')} />
        </View>
      ) : clip ? (
        <View style={s.bar}>
          <Text style={s.clipText} numberOfLines={2}>
            {clip.mode === 'copy' ? t('{n} zum Kopieren ausgewählt', { n: clip.paths.length }) : t('{n} zum Verschieben ausgewählt', { n: clip.paths.length })}
          </Text>
          <BarBtn theme={theme} label={t('Abbrechen')} onPress={() => setClip(null)} />
          <BarBtn theme={theme} label={t('Hier einfügen')} strong disabled={!!hits} onPress={paste} />
        </View>
      ) : !hits ? (
        <Pressable style={s.fab} onPress={() => setSheet('create')} android_ripple={{ color: 'rgba(255,255,255,0.3)', borderless: false }} accessibilityLabel={t('Neu')}>
          <Text style={s.fabText}>+</Text>
        </Pressable>
      ) : null}

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
      <BottomSheet visible={sheet === 'selection'} theme={theme} onClose={() => setSheet(null)} actions={selectionActions} />

      <Modal visible={!!naming} transparent animationType="fade" onRequestClose={() => setNaming(null)}>
        <View style={s.modalBg}>
          <View style={s.modal}>
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
          </View>
        </View>
      </Modal>
    </View>
  );
}

function SearchField({ theme, value, onChange, onSubmit }: { theme: Theme; value: string; onChange: (v: string) => void; onSubmit: () => void }) {
  const s = styles(theme);
  return (
    <View style={s.search}>
      <Text style={s.searchIcon}>⌕</Text>
      <TextInput
        value={value}
        onChangeText={onChange}
        onSubmitEditing={onSubmit}
        placeholder={t('Überall suchen')}
        placeholderTextColor={theme.text2 as string}
        returnKeyType="search"
        style={s.searchInput}
        accessibilityLabel={t('Überall suchen')}
      />
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
      flexDirection: 'row', alignItems: 'center', minHeight: 48, paddingHorizontal: 14, borderRadius: 16,
      backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke,
    },
    searchIcon: { color: th.text2, fontSize: 20, marginRight: 8 },
    searchInput: { flex: 1, color: th.text, fontSize: 16, paddingVertical: 10 },
    section: { color: th.text2, fontSize: 13, fontWeight: '700', marginTop: 24, marginBottom: 8, marginLeft: 4, letterSpacing: 0.3 },
    card: { borderRadius: 18, backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke, overflow: 'hidden' },
    cardRow: { flexDirection: 'row', alignItems: 'center', padding: 16 },
    cardTitle: { color: th.text, fontSize: 16, fontWeight: '600' },
    driveIcon: { width: 44, height: 44, borderRadius: 12, justifyContent: 'flex-end', padding: 8, marginRight: 14 },
    driveBar: { height: 6, borderRadius: 3 },
    usage: { height: 6, borderRadius: 3, marginTop: 8, overflow: 'hidden' },
    usageFill: { height: 6, borderRadius: 3 },
    folderGrid: { flexDirection: 'row', flexWrap: 'wrap', gap: 10 },
    folderTile: {
      width: '31%', alignItems: 'center', paddingVertical: 14, borderRadius: 16, overflow: 'hidden',
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
    row: { flexDirection: 'row', alignItems: 'center', minHeight: 64, paddingHorizontal: 16 },
    rowOn: { backgroundColor: th.accentSoft },
    rowText: {
      flex: 1, marginLeft: 14, alignSelf: 'stretch', justifyContent: 'center',
      borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: th.stroke,
    },
    thumb: { width: 40, height: 40, borderRadius: 8 },
    check: { width: 40, height: 40, borderRadius: 20, justifyContent: 'center', alignItems: 'center' },
    name: { color: th.text, fontSize: 16 },
    meta: { color: th.text2, fontSize: 13, marginTop: 2 },
    chev: { color: th.text2, fontSize: 22, marginLeft: 8 },
    dim: { opacity: 0.45 },
    gridList: { padding: 16, paddingBottom: 120 },
    gridRow: { gap: 8, marginBottom: 8 },
    tile: { alignItems: 'center', padding: 6, borderRadius: 14, overflow: 'hidden' },
    tileOn: { backgroundColor: th.accentSoft },
    tileThumb: { justifyContent: 'center', alignItems: 'center', borderRadius: 10, overflow: 'hidden' },
    tileImg: { width: '100%', height: '100%' },
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
    fab: {
      position: 'absolute', right: 20, bottom: 20, width: 60, height: 60, borderRadius: 20, overflow: 'hidden',
      backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center', elevation: 6,
    },
    fabText: { color: th.onAccent, fontSize: 30, lineHeight: 34 },
    modalBg: { flex: 1, backgroundColor: 'rgba(0,0,0,0.45)', justifyContent: 'center', padding: 28 },
    modal: { backgroundColor: th.dark ? '#2a2c34' : '#ffffff', borderRadius: 24, padding: 22 },
    modalTitle: { color: th.text, fontSize: 18, fontWeight: '600', marginBottom: 12 },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 2, borderBottomColor: th.accent, paddingVertical: 8 },
    modalBtns: { flexDirection: 'row', justifyContent: 'flex-end', marginTop: 12 },
  });
