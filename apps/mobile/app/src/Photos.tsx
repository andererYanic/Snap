// Fotos: Zeitachse nach Monaten. Vorschaubilder erzeugt Android (schnell, mit Cache) und blendet
// sie ein; Antippen öffnet die eingebaute Anzeige (Zoomen, Wischen, Videos).
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Dimensions, FlatList, Pressable, StyleSheet, View } from 'react-native';
import { Text } from './text';
import Animated, { FadeIn } from 'react-native-reanimated';
import { monthYear, request, shortDate, type TimelineItem } from './core';
import { locale, t } from './i18n';
import type { Theme } from './theme';
import { themed } from './ui';
import { Icon } from './Icon';
import { loadThumb, Thumb } from './thumbs';
import Editor from './Editor';
import Viewer from './Viewer';
import { m } from './motion';

const COLS = 4;

type Row = { head: string; count: number } | { cells: TimelineItem[]; offset: number };

export default function Photos({ theme, notify }: { theme: Theme; notify?: (text: string, undo?: () => void) => void }) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [viewer, setViewer] = useState<number | null>(null);
  const [editing, setEditing] = useState<{ path: string; name: string } | null>(null);
  const size = Math.floor((Dimensions.get('window').width - 4 - (COLS - 1) * 2) / COLS);
  const s = styles(theme);

  const more = useCallback(async (cursor: string | null) => {
    const p = await request<{ items: TimelineItem[]; next: string | null }>({ Timeline: { path: null, before: cursor, limit: 200 } });
    setItems((v) => (cursor ? [...v, ...p.items] : p.items));
    setNext(p.next);
    // Die ersten Vorschaubilder schon vorab anstoßen
    p.items.slice(0, 40).forEach((it) => loadThumb(it.path, 256));
  }, []);

  useEffect(() => {
    more(null).catch(() => {}).finally(() => setLoading(false));
  }, [more]);

  const rows = useMemo(() => {
    const out: Row[] = [];
    let month = '';
    let cur: TimelineItem[] = [];
    let start = 0;
    items.forEach((it, i) => {
      const m = monthYear(it.taken, locale());
      if (m !== month || cur.length === COLS) {
        if (cur.length) {
          out.push({ cells: cur, offset: start });
        }
        if (m !== month) {
          out.push({ head: m, count: 0 });
          month = m;
        }
        cur = [];
        start = i;
      }
      cur.push(it);
      for (let j = out.length - 1; j >= 0; j--) {
        const r = out[j];
        if ('head' in r) {
          r.count++;
          break;
        }
      }
    });
    if (cur.length) {
      out.push({ cells: cur, offset: start });
    }
    return out;
  }, [items]);

  const media = useMemo(() => items.map((it) => ({ path: it.path, name: it.path.slice(it.path.lastIndexOf('/') + 1) })), [items]);

  return (
    <View style={s.fill}>
      <FlatList
        data={rows}
        keyExtractor={(r, i) => ('head' in r ? `h${r.head}` : `r${r.offset}-${i}`)}
        contentContainerStyle={s.list}
        onEndReached={() => next && more(next).catch(() => {})}
        onEndReachedThreshold={2}
        windowSize={9}
        initialNumToRender={12}
        ListEmptyComponent={loading ? undefined : (
          <View style={s.emptyBox}>
            <View style={s.emptyIcon}>
              <Icon name="photos" size={28} color={theme.text3 as string} />
            </View>
            <Text style={s.emptyTitle}>{t('Noch keine Fotos')}</Text>
            <Text style={s.empty}>{t('Fotos und Videos aus Kamera und Bilderordnern erscheinen hier nach Aufnahmedatum.')}</Text>
          </View>
        )}
        renderItem={({ item: r }) =>
          'head' in r ? (
            <Animated.View entering={m(FadeIn.duration(200))} style={s.headRow}>
              <Text style={s.head}>{r.head}</Text>
              <Text style={s.count}>{r.count}</Text>
            </Animated.View>
          ) : (
            <View style={s.row}>
              {r.cells.map((it, k) => (
                <Pressable
                  key={it.path}
                  onPress={() => setViewer(r.offset + k)}
                  style={({ pressed }) => [s.cell, { width: size, height: size, opacity: pressed ? 0.8 : 1 }]}
                  accessibilityLabel={shortDate(it.taken, locale(), true)}
                >
                  <Thumb path={it.path} edge={256} style={s.img} />
                  {it.video && (
                    <View style={s.badge}>
                      <Icon name="play" size={12} color="#fff" fill="#fff" strokeWidth={1.2} />
                    </View>
                  )}
                </Pressable>
              ))}
            </View>
          )
        }
      />
      {viewer !== null && (
        <Viewer
          items={media}
          start={viewer}
          onClose={() => setViewer(null)}
          onEdit={notify ? (it) => { setViewer(null); setEditing({ path: it.path, name: it.name }); } : undefined}
        />
      )}
      {editing && notify && (
        <Editor
          path={editing.path}
          name={editing.name}
          notify={notify}
          onClose={(changed) => {
            setEditing(null);
            if (changed) {
              more(null).catch(() => {});
            }
          }}
        />
      )}
    </View>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    list: { paddingHorizontal: 2, paddingBottom: 40 },
    emptyBox: { alignItems: 'center', paddingTop: 96, paddingHorizontal: 40, gap: 6 },
    emptyIcon: { width: 64, height: 64, borderRadius: 32, justifyContent: 'center', alignItems: 'center', marginBottom: 10, backgroundColor: th.surface2 },
    emptyTitle: { color: th.text, fontSize: 17, lineHeight: 24, fontWeight: '700' },
    empty: { color: th.text2, textAlign: 'center', fontSize: 13, lineHeight: 18 },
    headRow: { flexDirection: 'row', alignItems: 'baseline', justifyContent: 'space-between', paddingHorizontal: 14, paddingTop: 20, paddingBottom: 10 },
    head: { color: th.text, fontSize: 16, lineHeight: 22, fontWeight: '700' },
    count: { color: th.text3, fontSize: 13 },
    row: { flexDirection: 'row', gap: 2, marginBottom: 2 },
    cell: { overflow: 'hidden', backgroundColor: th.surface2 },
    img: { width: '100%', height: '100%' },
    badge: { position: 'absolute', right: 6, bottom: 6, width: 22, height: 22, borderRadius: 11, backgroundColor: 'rgba(0,0,0,0.45)', justifyContent: 'center', alignItems: 'center' },
  }));
