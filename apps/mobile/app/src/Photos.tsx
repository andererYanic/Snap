// Fotos: Zeitachse nach Monaten. Vorschaubilder erzeugt Android (schnell, mit Cache) und blendet
// sie ein; Antippen öffnet die eingebaute Anzeige (Zoomen, Wischen, Videos).
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Dimensions, FlatList, Pressable, StyleSheet, View } from 'react-native';
import { Text } from './text';
import Animated, { FadeIn } from 'react-native-reanimated';
import * as core from './core';
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

export default function Photos({ theme, notify, onDrop }: {
  theme: Theme;
  notify?: (text: string, undo?: () => void) => void;
  /** Ausgewählte Fotos per Snap Drop senden. */
  onDrop?: (paths: string[]) => void;
}) {
  const [items, setItems] = useState<TimelineItem[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [viewer, setViewer] = useState<number | null>(null);
  const [editing, setEditing] = useState<{ path: string; name: string } | null>(null);
  // Auswahl: lange drücken beginnt, danach Antippen fügt hinzu bzw. entfernt
  const [sel, setSel] = useState<Set<string>>(new Set());
  const toggle = (path: string) => {
    const n = new Set(sel);
    n.has(path) ? n.delete(path) : n.add(path);
    setSel(n);
    core.haptic('tick');
  };
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
                  onPress={() => (sel.size ? toggle(it.path) : setViewer(r.offset + k))}
                  onLongPress={() => toggle(it.path)}
                  style={({ pressed }) => [s.cell, { width: size, height: size, opacity: pressed ? 0.8 : 1 }]}
                  accessibilityLabel={shortDate(it.taken, locale(), true)}
                  accessibilityState={{ selected: sel.has(it.path) }}
                >
                  <Thumb path={it.path} edge={256} style={s.img} />
                  {sel.has(it.path) && (
                    <View style={s.picked}>
                      <View style={s.check}>
                        <Icon name="check" size={14} color="#fff" strokeWidth={2.4} />
                      </View>
                    </View>
                  )}
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
      {sel.size > 0 && (
        <Animated.View entering={m(FadeIn.duration(150))} style={s.selBar}>
          <Pressable onPress={() => setSel(new Set())} style={s.selBtn} accessibilityLabel={t('Auswahl aufheben')}>
            <Icon name="close" size={22} color={theme.text as string} />
          </Pressable>
          <Text style={s.selText}>{t('{n} ausgewählt', { n: sel.size })}</Text>
          <Pressable onPress={() => core.shareFiles([...sel]).catch(() => {})} style={s.selBtn} accessibilityLabel={t('Teilen')}>
            <Icon name="share" size={22} color={theme.text as string} />
          </Pressable>
          {onDrop && (
            <Pressable
              onPress={() => {
                onDrop([...sel]);
                setSel(new Set());
              }}
              style={s.selBtn}
              accessibilityLabel={t('In der Nähe senden')}
            >
              <Icon name="nearby" size={22} color={theme.text as string} />
            </Pressable>
          )}
        </Animated.View>
      )}
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
    picked: { position: 'absolute', left: 0, right: 0, top: 0, bottom: 0, borderWidth: 3, borderColor: th.accent, backgroundColor: 'rgba(0,0,0,0.18)' },
    check: { position: 'absolute', right: 6, top: 6, width: 22, height: 22, borderRadius: 11, backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center' },
    selBar: { position: 'absolute', left: 12, right: 12, bottom: 12, flexDirection: 'row', alignItems: 'center', gap: 4, paddingHorizontal: 6, minHeight: 56, borderRadius: 28, backgroundColor: th.surface, borderWidth: StyleSheet.hairlineWidth, borderColor: th.line, elevation: 6 },
    selText: { flex: 1, color: th.text, fontSize: 15, fontWeight: '600', paddingLeft: 4 },
    selBtn: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    badge: { position: 'absolute', right: 6, bottom: 6, width: 22, height: 22, borderRadius: 11, backgroundColor: 'rgba(0,0,0,0.45)', justifyContent: 'center', alignItems: 'center' },
  }));
