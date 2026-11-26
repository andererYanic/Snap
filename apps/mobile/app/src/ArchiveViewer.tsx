// Inhalt eines Archivs ansehen (ZIP, TAR, TAR.GZ), ohne zu entpacken. Ordner lassen sich
// aufklappen; „Entpacken“ nutzt die bestehende Dateioperation (mit Passwortabfrage, Rückgängig).
import { useEffect, useMemo, useState } from 'react';
import { ActivityIndicator, FlatList, Modal, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import * as core from './core';
import { locale, t } from './i18n';
import { Icon } from './Icon';
import { Text } from './text';
import type { Theme } from './theme';
import { Glyph, themed } from './ui';

interface Entry { name: string; size: number; dir: boolean }
interface Listing { entries: Entry[]; total_size: number; truncated: boolean; encrypted: boolean }

export const isArchiveListable = (name: string) => /\.(zip|tar|tgz|tar\.gz)$/i.test(name);

export default function ArchiveViewer({ path, name, theme, onClose, onExtract }: {
  path: string; name: string; theme: Theme; onClose: () => void; onExtract: () => void;
}) {
  const s = styles(theme);
  const [list, setList] = useState<Listing | null | 'error'>(null);
  const [dir, setDir] = useState('');
  useEffect(() => {
    core.request<Listing>({ ArchiveList: { path } }).then(setList).catch(() => setList('error'));
  }, [path]);

  // Einträge der aktuellen Ebene (Ordner aus Pfaden abgeleitet, auch ohne eigene Ordnereinträge)
  const rows = useMemo(() => {
    if (!list || list === 'error') {
      return [];
    }
    const prefix = dir ? `${dir}/` : '';
    const dirs = new Map<string, number>();
    const files: Entry[] = [];
    for (const e of list.entries) {
      if (!e.name.startsWith(prefix)) {
        continue;
      }
      const rest = e.name.slice(prefix.length);
      const slash = rest.indexOf('/');
      if (slash >= 0) {
        const d = rest.slice(0, slash);
        dirs.set(d, (dirs.get(d) ?? 0) + e.size);
      } else if (e.dir) {
        if (rest) {
          dirs.set(rest, dirs.get(rest) ?? 0);
        }
      } else if (rest) {
        files.push({ ...e, name: rest });
      }
    }
    const coll = new Intl.Collator(locale(), { numeric: true, sensitivity: 'base' });
    return [
      ...Array.from(dirs.entries()).sort((a, b) => coll.compare(a[0], b[0])).map(([n, size]) => ({ name: n, size, dir: true })),
      ...files.sort((a, b) => coll.compare(a.name, b.name)),
    ];
  }, [list, dir]);

  const ok = list && list !== 'error' ? list : null;
  return (
    <Modal visible animationType="fade" onRequestClose={() => (dir ? setDir(dir.includes('/') ? dir.slice(0, dir.lastIndexOf('/')) : '') : onClose())} statusBarTranslucent>
      <View style={s.root}>
        <StatusBar barStyle={theme.dark ? 'light-content' : 'dark-content'} />
        <View style={s.top}>
          <Pressable
            onPress={() => (dir ? setDir(dir.includes('/') ? dir.slice(0, dir.lastIndexOf('/')) : '') : onClose())}
            style={s.btn}
            accessibilityLabel={t('Zurück')}
          >
            <Icon name="back" color={theme.text as string} />
          </Pressable>
          <View style={s.grow}>
            <Text style={s.title} numberOfLines={1}>{dir ? dir.slice(dir.lastIndexOf('/') + 1) : name}</Text>
            {ok && <Text style={s.meta}>{t('{n} Einträge · {size} entpackt', { n: ok.entries.length, size: core.humanSize(ok.total_size, locale()) })}</Text>}
          </View>
        </View>
        {list === null ? (
          <ActivityIndicator style={s.grow} color={theme.accent as string} />
        ) : list === 'error' ? (
          <Text style={s.note}>{t('Dieses Archiv lässt sich nicht lesen.')}</Text>
        ) : (
          <FlatList
            data={rows}
            keyExtractor={(e) => (e.dir ? 'd:' : 'f:') + e.name}
            ListHeaderComponent={
              list.encrypted ? <Text style={s.note}>{t('Verschlüsselt – beim Entpacken wird nach dem Passwort gefragt.')}</Text> : undefined
            }
            ListFooterComponent={list.truncated ? <Text style={s.note}>{t('Nur die ersten 10 000 Einträge werden angezeigt.')}</Text> : undefined}
            renderItem={({ item }) => (
              <Pressable
                onPress={item.dir ? () => setDir(dir ? `${dir}/${item.name}` : item.name) : undefined}
                style={s.row}
                android_ripple={item.dir ? { color: theme.accentSoft as string } : undefined}
                accessibilityRole={item.dir ? 'button' : undefined}
              >
                <Glyph name={item.name} dir={item.dir} theme={theme} />
                <Text style={s.name} numberOfLines={1}>{item.name}</Text>
                {item.dir ? <Icon name="chevron" size={20} color={theme.text3 as string} /> : <Text style={s.size}>{core.humanSize(item.size, locale())}</Text>}
              </Pressable>
            )}
          />
        )}
        <View style={s.bar}>
          <Pressable onPress={onExtract} style={s.primary} accessibilityRole="button">
            <Icon name="extract" size={20} color={theme.onAccent as string} />
            <Text style={s.primaryText}>{t('Entpacken')}</Text>
          </Pressable>
        </View>
      </View>
    </Modal>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    root: { flex: 1, backgroundColor: th.bg, paddingTop: 28 },
    grow: { flex: 1, minWidth: 0 },
    top: { flexDirection: 'row', alignItems: 'center', minHeight: 60, paddingHorizontal: 4 },
    btn: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    meta: { color: th.text2, fontSize: 12, lineHeight: 16 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 12, minHeight: 56, paddingHorizontal: 16 },
    name: { flex: 1, color: th.text, fontSize: 15 },
    size: { color: th.text2, fontSize: 13, fontVariant: ['tabular-nums'] },
    note: { color: th.text2, fontSize: 13, lineHeight: 19, padding: 16 },
    bar: { padding: 12, paddingBottom: 20, backgroundColor: th.surface, borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: th.line },
    primary: { minHeight: 48, borderRadius: 24, backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center', flexDirection: 'row', gap: 8 },
    primaryText: { color: th.onAccent, fontSize: 16, fontWeight: '700' },
  }));
