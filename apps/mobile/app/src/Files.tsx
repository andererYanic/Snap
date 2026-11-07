// Dateien: Ordnerliste mit Brotkrumen, Zurück-Geste, Auswahl per langem Drücken,
// Aktionsleiste unten (Daumenzone), „Rückgängig“-Leiste statt Rückfragen.
import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  Alert, BackHandler, FlatList, Image, Modal, Pressable, ScrollView, StyleSheet, Text, TextInput, View,
} from 'react-native';
import { humanSize, joinPath, parentOf, request, type DirEntry } from './core';
import { locale, t } from './i18n';
import type { Theme } from './theme';
import { Glyph } from './ui';

interface Props {
  theme: Theme;
  root: string;
  notify: (text: string, undo?: () => void) => void;
  selectionChanged: (active: boolean) => void;
}

const IMAGE = /\.(jpe?g|png|gif|webp|heic|heif|bmp|avif)$/i;

export default function Files({ theme, root, notify, selectionChanged }: Props) {
  const [path, setPath] = useState(root);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [sel, setSel] = useState<Set<string>>(new Set());
  const [naming, setNaming] = useState<{ mode: 'new' | 'rename'; from?: string; value: string } | null>(null);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const s = styles(theme);

  const load = useCallback(async () => {
    try {
      const r = await request<{ entries: DirEntry[] }>({ ListDir: { path, show_hidden: false, offset: 0, limit: 1000 } });
      const coll = new Intl.Collator(locale(), { numeric: true, sensitivity: 'base' });
      r.entries.sort((a, b) => (a.kind === 'Dir') !== (b.kind === 'Dir') ? (a.kind === 'Dir' ? -1 : 1) : coll.compare(a.name, b.name));
      setEntries(r.entries);
      setError(null);
    } catch (e: any) {
      setEntries([]);
      setError(e.message);
    }
  }, [path]);

  useEffect(() => {
    setSel(new Set());
    load();
  }, [load]);

  useEffect(() => selectionChanged(sel.size > 0), [sel, selectionChanged]);

  // Zurück: erst Auswahl aufheben, dann Ordner hoch
  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', () => {
      if (sel.size) {
        setSel(new Set());
        return true;
      }
      const up = parentOf(path, root);
      if (up) {
        setPath(up);
        return true;
      }
      return false;
    });
    return () => sub.remove();
  }, [path, root, sel]);

  // Vorschaubilder für sichtbare Bilder (aus dem Cache des Kerns)
  useEffect(() => {
    const imgs = entries.filter((e) => e.kind === 'File' && IMAGE.test(e.name)).slice(0, 60);
    imgs.forEach((e) => {
      const p = joinPath(path, e.name);
      if (thumbs[p]) {
        return;
      }
      request<{ path: string }>({ Thumbnail: { path: p, size: 'Small' } })
        .then((th) => setThumbs((m) => ({ ...m, [p]: `file://${th.path}` })))
        .catch(() => {});
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entries, path]);

  const crumbs = useMemo(() => {
    const rel = path.startsWith(root) ? path.slice(root.length) : path;
    const out = [{ name: t('Telefon'), path: root }];
    let acc = root;
    for (const p of rel.split('/').filter(Boolean)) {
      acc = joinPath(acc, p);
      out.push({ name: p, path: acc });
    }
    return out;
  }, [path, root]);

  const toggle = (name: string) => {
    const n = new Set(sel);
    n.has(name) ? n.delete(name) : n.add(name);
    setSel(n);
  };

  const fileOp = async (op: unknown) => request({ FileOp: { op, wait: true } });

  const trash = async () => {
    const paths = [...sel].map((n) => joinPath(path, n));
    try {
      await fileOp({ Trash: { paths } });
      setSel(new Set());
      load();
      notify(paths.length === 1 ? t('„{name}“ in den Papierkorb verschoben.', { name: [...sel][0] }) : t('{n} Elemente in den Papierkorb verschoben.', { n: paths.length }), async () => {
        await request('Undo');
        load();
      });
    } catch (e: any) {
      notify(e.message);
    }
  };

  const deleteForever = () => {
    const paths = [...sel].map((n) => joinPath(path, n));
    Alert.alert(t('Endgültig löschen'), t('Diese {n} Elemente endgültig löschen? Das kann nicht rückgängig gemacht werden.', { n: paths.length }), [
      { text: t('Abbrechen'), style: 'cancel' },
      {
        text: t('Endgültig löschen'),
        style: 'destructive',
        onPress: async () => {
          try {
            await fileOp({ Delete: { paths } });
            setSel(new Set());
            load();
          } catch (e: any) {
            notify(e.message);
          }
        },
      },
    ]);
  };

  const commitName = async () => {
    if (!naming) {
      return;
    }
    const name = naming.value.trim();
    setNaming(null);
    if (!name) {
      return;
    }
    try {
      if (naming.mode === 'new') {
        await fileOp({ Mkdir: { parent: path, name } });
      } else if (naming.from && name !== naming.from) {
        await fileOp({ Rename: { path: joinPath(path, naming.from), new_name: name } });
        setSel(new Set());
      }
      load();
    } catch (e: any) {
      notify(e.message);
    }
  };

  const open = (e: DirEntry) => {
    if (sel.size) {
      toggle(e.name);
    } else if (e.kind === 'Dir') {
      setPath(joinPath(path, e.name));
    }
  };

  return (
    <View style={s.fill}>
      <ScrollView horizontal showsHorizontalScrollIndicator={false} style={s.crumbs} contentContainerStyle={s.crumbsInner}>
        {crumbs.map((c, i) => (
          <Pressable key={c.path} onPress={() => setPath(c.path)} style={s.crumb} accessibilityRole="button">
            <Text style={[s.crumbText, i === crumbs.length - 1 && s.crumbLast]} numberOfLines={1}>
              {i > 0 ? '› ' : ''}{c.name}
            </Text>
          </Pressable>
        ))}
      </ScrollView>
      {sel.size > 0 && (
        <View style={s.selHead}>
          <Text style={s.selText}>{t('{n} ausgewählt', { n: sel.size })}</Text>
          <Pressable onPress={() => setSel(new Set(entries.map((e) => e.name)))} style={s.headBtn}>
            <Text style={s.link}>{t('Alle')}</Text>
          </Pressable>
        </View>
      )}
      <FlatList
        data={entries}
        keyExtractor={(e) => e.name}
        contentContainerStyle={s.list}
        onRefresh={load}
        refreshing={false}
        ListEmptyComponent={<Text style={s.empty}>{error ?? t('Dieser Ordner ist leer')}</Text>}
        renderItem={({ item: e }) => {
          const p = joinPath(path, e.name);
          const selected = sel.has(e.name);
          return (
            <Pressable
              onPress={() => open(e)}
              onLongPress={() => toggle(e.name)}
              style={[s.row, selected && s.rowSel]}
              accessibilityLabel={e.name}
              accessibilityState={{ selected }}
            >
              {thumbs[p] ? <Image source={{ uri: thumbs[p] }} style={s.thumb} /> : <Glyph kind={e.kind === 'Dir' ? 'folder' : 'file'} theme={theme} />}
              <View style={s.rowText}>
                <Text style={[s.name, e.hidden && s.dim]} numberOfLines={1}>{e.name}</Text>
                <Text style={s.meta} numberOfLines={1}>
                  {e.kind === 'Dir' ? t('Ordner') : humanSize(e.size, locale())} · {new Date(e.mtime * 1000).toLocaleDateString(locale())}
                </Text>
              </View>
              {e.kind === 'Dir' && !sel.size && <Text style={s.chev}>›</Text>}
            </Pressable>
          );
        }}
      />
      {sel.size > 0 ? (
        <View style={s.actionBar}>
          <Action label={t('Umbenennen')} disabled={sel.size !== 1} onPress={() => setNaming({ mode: 'rename', from: [...sel][0], value: [...sel][0] })} theme={theme} />
          <Action label={t('In den Papierkorb')} onPress={trash} theme={theme} />
          <Action label={t('Endgültig löschen')} danger onPress={deleteForever} theme={theme} />
        </View>
      ) : (
        <Pressable style={s.fab} onPress={() => setNaming({ mode: 'new', value: t('Neuer Ordner') })} accessibilityLabel={t('Neuer Ordner')}>
          <Text style={s.fabText}>+</Text>
        </Pressable>
      )}
      <Modal visible={!!naming} transparent animationType="fade" onRequestClose={() => setNaming(null)}>
        <View style={s.modalBg}>
          <View style={s.modal}>
            <Text style={s.modalTitle}>{naming?.mode === 'new' ? t('Neuer Ordner') : t('Umbenennen')}</Text>
            <TextInput
              autoFocus
              selectTextOnFocus
              value={naming?.value}
              onChangeText={(v) => setNaming((n) => (n ? { ...n, value: v } : n))}
              onSubmitEditing={commitName}
              style={s.input}
            />
            <View style={s.modalBtns}>
              <Pressable onPress={() => setNaming(null)} style={s.headBtn}><Text style={s.link}>{t('Abbrechen')}</Text></Pressable>
              <Pressable onPress={commitName} style={s.headBtn}><Text style={s.link}>{t('Speichern')}</Text></Pressable>
            </View>
          </View>
        </View>
      </Modal>
    </View>
  );
}

function Action({ label, onPress, theme, disabled, danger }: { label: string; onPress: () => void; theme: Theme; disabled?: boolean; danger?: boolean }) {
  const s = styles(theme);
  return (
    <Pressable onPress={onPress} disabled={disabled} style={[s.action, disabled && s.dim]} accessibilityRole="button">
      <Text style={[s.actionText, danger && { color: theme.danger }]} numberOfLines={2}>{label}</Text>
    </Pressable>
  );
}

const styles = (th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    crumbs: { flexGrow: 0 },
    crumbsInner: { paddingHorizontal: 12, alignItems: 'center' },
    crumb: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 4 },
    crumbText: { color: th.text2, fontSize: 15 },
    crumbLast: { color: th.text, fontWeight: '600' },
    selHead: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 16 },
    selText: { flex: 1, color: th.text, fontSize: 16, fontWeight: '600' },
    headBtn: { minHeight: 48, minWidth: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 12 },
    link: { color: th.accent, fontSize: 16, fontWeight: '600' },
    list: { padding: 12, paddingBottom: 120 },
    empty: { color: th.text2, textAlign: 'center', marginTop: 60, fontSize: 15 },
    row: {
      flexDirection: 'row', alignItems: 'center', minHeight: 64, paddingHorizontal: 14, marginBottom: 6,
      borderRadius: 14, backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke,
    },
    rowSel: { backgroundColor: th.accentSoft, borderColor: th.accent },
    thumb: { width: 40, height: 40, borderRadius: 8 },
    rowText: { flex: 1, marginLeft: 14 },
    name: { color: th.text, fontSize: 16 },
    meta: { color: th.text2, fontSize: 13, marginTop: 2 },
    chev: { color: th.text2, fontSize: 22, marginLeft: 8 },
    dim: { opacity: 0.45 },
    actionBar: {
      position: 'absolute', left: 12, right: 12, bottom: 12, flexDirection: 'row', borderRadius: 18,
      backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke, elevation: 6,
    },
    action: { flex: 1, minHeight: 56, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 6 },
    actionText: { color: th.text, fontSize: 13, textAlign: 'center', fontWeight: '600' },
    fab: {
      position: 'absolute', right: 20, bottom: 20, width: 60, height: 60, borderRadius: 20,
      backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center', elevation: 6,
    },
    fabText: { color: th.onAccent, fontSize: 30, lineHeight: 34 },
    modalBg: { flex: 1, backgroundColor: 'rgba(0,0,0,0.45)', justifyContent: 'center', padding: 28 },
    modal: { backgroundColor: th.dark ? '#2a2c34' : '#ffffff', borderRadius: 20, padding: 20 },
    modalTitle: { color: th.text, fontSize: 18, fontWeight: '600', marginBottom: 12 },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 2, borderBottomColor: th.accent, paddingVertical: 8 },
    modalBtns: { flexDirection: 'row', justifyContent: 'flex-end', marginTop: 12 },
  });
