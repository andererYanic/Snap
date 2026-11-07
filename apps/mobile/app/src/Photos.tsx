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
import { loadThumb, Thumb } from './thumbs';
import Viewer from './Viewer';
import { m } from './motion';

const COLS = 4;

type Row = { head: string } | { cells: TimelineItem[]; offset: number };

export default function Photos({ theme }: { theme: Theme }) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [viewer, setViewer] = useState<number | null>(null);
  const size = Math.floor((Dimensions.get('window').width - 24 - (COLS - 1) * 3) / COLS);
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
          out.push({ head: m });
          month = m;
        }
        cur = [];
        start = i;
      }
      cur.push(it);
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
        ListEmptyComponent={loading ? undefined : <Text style={s.empty}>{t('Noch keine Fotos')}</Text>}
        renderItem={({ item: r }) =>
          'head' in r ? (
            <Animated.View entering={m(FadeIn.duration(200))}><Text style={s.head}>{r.head}</Text></Animated.View>
          ) : (
            <View style={s.row}>
              {r.cells.map((it, k) => (
                <Pressable
                  key={it.path}
                  onPress={() => setViewer(r.offset + k)}
                  style={({ pressed }) => [s.cell, { width: size, height: size, transform: [{ scale: pressed ? 0.96 : 1 }] }]}
                  accessibilityLabel={shortDate(it.taken, locale(), true)}
                >
                  <Thumb path={it.path} edge={256} style={s.img} />
                  {it.video && <Text style={s.badge}>▶</Text>}
                </Pressable>
              ))}
            </View>
          )
        }
      />
      {viewer !== null && <Viewer items={media} start={viewer} onClose={() => setViewer(null)} />}
    </View>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    list: { padding: 12, paddingBottom: 40 },
    empty: { color: th.text2, textAlign: 'center', marginTop: 60, fontSize: 15 },
    head: { color: th.text, fontSize: 17, fontWeight: '600', marginTop: 14, marginBottom: 8 },
    row: { flexDirection: 'row', gap: 3, marginBottom: 3 },
    cell: { borderRadius: 6, overflow: 'hidden', backgroundColor: th.accentSoft },
    img: { width: '100%', height: '100%' },
    badge: { position: 'absolute', right: 6, bottom: 4, color: '#fff', fontSize: 14, textShadowColor: '#000', textShadowRadius: 4 },
  }));
