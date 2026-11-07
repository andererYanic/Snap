// Fotos: Zeitachse nach Monaten, Raster mit Vorschaubildern, Vollbild beim Antippen.
import { useCallback, useEffect, useState } from 'react';
import { Dimensions, FlatList, Image, Modal, Pressable, StyleSheet, Text, View } from 'react-native';
import { request, type TimelineItem } from './core';
import { locale, t } from './i18n';
import type { Theme } from './theme';

const COLS = 4;

export default function Photos({ theme }: { theme: Theme }) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [full, setFull] = useState<{ item: TimelineItem; uri: string } | null>(null);
  const size = Math.floor((Dimensions.get('window').width - 24 - (COLS - 1) * 3) / COLS);
  const s = styles(theme);

  const more = useCallback(async (cursor: string | null) => {
    const p = await request<{ items: TimelineItem[]; next: string | null }>({ Timeline: { path: null, before: cursor, limit: 200 } });
    setItems((v) => (cursor ? [...v, ...p.items] : p.items));
    setNext(p.next);
  }, []);

  useEffect(() => {
    more(null).catch(() => {});
  }, [more]);

  const thumb = (it: TimelineItem) => {
    if (thumbs[it.path] !== undefined) {
      return;
    }
    setThumbs((m) => ({ ...m, [it.path]: '' }));
    request<{ path: string }>({ Thumbnail: { path: it.path, size: 'Small' } })
      .then((th) => setThumbs((m) => ({ ...m, [it.path]: `file://${th.path}` })))
      .catch(() => {});
  };

  const open = async (it: TimelineItem) => {
    try {
      const th = await request<{ path: string }>({ Thumbnail: { path: it.path, size: 'Large' } });
      setFull({ item: it, uri: `file://${th.path}` });
    } catch {
      // nicht darstellbar
    }
  };

  // Monatsüberschriften als eigene Zeilen
  const rows: ({ head: string } | { cells: TimelineItem[] })[] = [];
  let month = '';
  let cur: TimelineItem[] = [];
  for (const it of items) {
    const d = new Date(it.taken * 1000);
    const m = d.toLocaleDateString(locale(), { month: 'long', year: 'numeric', timeZone: 'UTC' });
    if (m !== month) {
      if (cur.length) {
        rows.push({ cells: cur });
      }
      cur = [];
      rows.push({ head: m });
      month = m;
    }
    cur.push(it);
    if (cur.length === COLS) {
      rows.push({ cells: cur });
      cur = [];
    }
  }
  if (cur.length) {
    rows.push({ cells: cur });
  }

  return (
    <View style={s.fill}>
      <FlatList
        data={rows}
        keyExtractor={(r, i) => ('head' in r ? `h${r.head}` : `r${i}`)}
        contentContainerStyle={s.list}
        onEndReached={() => next && more(next).catch(() => {})}
        onEndReachedThreshold={1.5}
        ListEmptyComponent={<Text style={s.empty}>{t('Noch keine Fotos')}</Text>}
        renderItem={({ item: r }) =>
          'head' in r ? (
            <Text style={s.head}>{r.head}</Text>
          ) : (
            <View style={s.row}>
              {r.cells.map((it) => {
                thumb(it);
                return (
                  <Pressable key={it.path} onPress={() => open(it)} style={[s.cell, { width: size, height: size }]}>
                    {thumbs[it.path] ? <Image source={{ uri: thumbs[it.path] }} style={s.img} /> : null}
                    {it.video && <Text style={s.badge}>▶</Text>}
                  </Pressable>
                );
              })}
            </View>
          )
        }
      />
      <Modal visible={!!full} transparent onRequestClose={() => setFull(null)} animationType="fade">
        <Pressable style={s.full} onPress={() => setFull(null)}>
          {full && <Image source={{ uri: full.uri }} style={s.fullImg} resizeMode="contain" />}
          {full && <Text style={s.cap}>{new Date(full.item.taken * 1000).toLocaleString(locale(), { timeZone: 'UTC' })}</Text>}
        </Pressable>
      </Modal>
    </View>
  );
}

const styles = (th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    list: { padding: 12, paddingBottom: 40 },
    empty: { color: th.text2, textAlign: 'center', marginTop: 60, fontSize: 15 },
    head: { color: th.text, fontSize: 17, fontWeight: '600', marginTop: 14, marginBottom: 8 },
    row: { flexDirection: 'row', gap: 3, marginBottom: 3 },
    cell: { borderRadius: 6, overflow: 'hidden', backgroundColor: th.accentSoft },
    img: { width: '100%', height: '100%' },
    badge: { position: 'absolute', right: 6, bottom: 4, color: '#fff', fontSize: 14 },
    full: { flex: 1, backgroundColor: '#000', justifyContent: 'center' },
    fullImg: { width: '100%', height: '85%' },
    cap: { color: '#ddd', textAlign: 'center', marginTop: 8 },
  });
