// Dateien eines anderen Geräts durchsuchen (von der Startseite aus): Freigaben als Ordner, darin
// Unterordner und Dateien aus dem lokal abgeglichenen Katalog – das Blättern braucht kein Netz.
// Öffnen lädt die Datei in den Cache und zeigt sie mit den eingebauten Anzeigen; Videos werden
// gestreamt. „Herunterladen“ legt eine Kopie in den Download-Ordner.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, BackHandler, FlatList, Pressable, StyleSheet, View } from 'react-native';
import Animated from 'react-native-reanimated';
import * as core from './core';
import { humanSize, request } from './core';
import { locale, t } from './i18n';
import { m, pageIn } from './motion';
import { Text } from './text';
import type { Theme } from './theme';
import { isVideo } from './thumbs';
import { Glyph, Tap, themed } from './ui';

export interface RemoteDevice {
  node_id: string;
  name: string;
  online?: boolean;
}
interface CatalogItem {
  hash: string;
  name: string;
  size: number;
  share?: string | null;
  rel_path?: string | null;
}
interface Page {
  dirs: string[];
  files: CatalogItem[];
  next: string | null;
}
type Row = { kind: 'dir'; name: string } | { kind: 'file'; item: CatalogItem };

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

/** Dateiname aus einem fremden Katalog als sicherer, einzelner Pfadteil. */
export function safeName(name: string): string {
  const n = name.replace(/[/\\\u0000-\u001f]/g, '_').trim();
  return !n || n === '.' || n === '..' ? 'Datei' : n.slice(0, 200);
}

/** Zielpfad im Cache für eine Datei eines anderen Geräts (`null` bei ungültigem Hash). */
export function cachePath(cache: string, item: { hash: string; name: string }): string | null {
  if (!/^[0-9a-f]{64}$/.test(item.hash)) {
    return null;
  }
  return `${cache}/remote/${item.hash.slice(0, 32)}/${safeName(item.name)}`;
}

export default function RemoteBrowser({ device, theme, notify, onClose, onOpen, onStream }: {
  device: RemoteDevice;
  theme: Theme;
  notify: (text: string) => void;
  onClose: () => void;
  /** Heruntergeladene Datei mit den eingebauten Anzeigen öffnen. */
  onOpen: (path: string, name: string) => void;
  /** Video über die lokale Streaming-Adresse abspielen. */
  onStream: (uri: string, name: string) => void;
}) {
  const s = styles(theme);
  // [] = Wurzel des Geräts; [Freigabe, Ordner, Unterordner …]
  const [trail, setTrail] = useState<string[]>([]);
  const [dir, setDir] = useState<'forward' | 'back'>('forward');
  const [rows, setRows] = useState<Row[] | null>(null);
  const [next, setNext] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const gen = useRef(0);

  const query = useCallback(
    (after: string | null) =>
      request<Page>({
        RemoteDir: {
          device: device.node_id,
          share: trail[0] ?? null,
          dir: trail.slice(1).join('/'),
          after,
          limit: 300,
        },
      }),
    [device.node_id, trail],
  );
  const toRows = (p: Page): Row[] => [
    ...[...p.dirs].sort(collator.compare).map((name): Row => ({ kind: 'dir', name })),
    ...[...p.files].sort((a, b) => collator.compare(a.name, b.name)).map((item): Row => ({ kind: 'file', item })),
  ];

  useEffect(() => {
    const g = ++gen.current;
    setRows(null);
    setError(null);
    query(null)
      .then((p) => {
        if (g === gen.current) {
          setRows(toRows(p));
          setNext(p.next);
        }
      })
      .catch((e) => g === gen.current && setError(String(e?.message ?? e)));
  }, [query]);

  const more = () => {
    if (!next || !rows) {
      return;
    }
    const g = gen.current;
    const after = next;
    setNext(null);
    query(after)
      .then((p) => {
        if (g === gen.current) {
          setRows((r) => [...(r ?? []), ...toRows(p)]);
          setNext(p.next);
        }
      })
      .catch(() => {});
  };

  const back = useCallback(() => {
    if (trail.length === 0) {
      onClose();
    } else {
      setDir('back');
      setTrail((x) => x.slice(0, -1));
    }
    return true;
  }, [trail.length, onClose]);
  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', back);
    return () => sub.remove();
  }, [back]);

  const enter = (name: string) => {
    setDir('forward');
    setTrail((x) => [...x, name]);
  };

  const fetchTo = async (item: CatalogItem, keep: boolean): Promise<string> => {
    if (keep) {
      const r = await request<{ path?: string; job_id?: number }>({ Download: { target: item.hash, output: null, force: false } });
      if (!r?.path) {
        throw new Error(t('Gerät nicht erreichbar – die Datei wird geladen, sobald es wieder da ist.'));
      }
      return r.path;
    }
    const out = cachePath(await core.cacheDir(), item);
    if (!out) {
      throw new Error(t('Ungültiger Katalogeintrag.'));
    }
    const r = await request<{ path?: string }>({ Download: { target: item.hash, output: out, force: true } });
    if (!r?.path) {
      throw new Error(t('Gerät nicht erreichbar.'));
    }
    return r.path;
  };

  const openFile = async (item: CatalogItem) => {
    setBusy(item.hash);
    try {
      if (isVideo(item.name)) {
        const r = await request<{ url: string }>({ StreamUrl: { target: item.hash } });
        onStream(r.url, item.name);
      } else {
        onOpen(await fetchTo(item, false), item.name);
      }
    } catch (e: any) {
      notify(String(e?.message ?? e));
    } finally {
      setBusy(null);
    }
  };
  const download = async (item: CatalogItem) => {
    setBusy(item.hash);
    try {
      const path = await fetchTo(item, true);
      notify(t('Heruntergeladen: {name}', { name: path.slice(path.lastIndexOf('/') + 1) }));
    } catch (e: any) {
      notify(String(e?.message ?? e));
    } finally {
      setBusy(null);
    }
  };

  const title = trail.length ? trail[trail.length - 1] : device.name;
  const crumb = trail.length ? [device.name, ...trail.slice(0, -1)].join(' › ') : device.online ? t('Verbunden') : t('Nicht verbunden');
  const empty = useMemo(() => rows !== null && rows.length === 0, [rows]);

  return (
    <Animated.View key={trail.join('/')} entering={m(pageIn(dir))} style={s.fill}>
      <View style={s.head}>
        <Pressable onPress={back} style={s.backBtn} accessibilityLabel={t('Zurück')} android_ripple={{ color: theme.accentSoft as string, borderless: true }}>
          <Text style={s.backIcon}>‹</Text>
        </Pressable>
        <View style={s.grow}>
          <Text style={s.title} numberOfLines={1}>{title}</Text>
          <Text style={s.crumb} numberOfLines={1}>{crumb}</Text>
        </View>
      </View>
      {error ? (
        <Text style={s.note}>{error}</Text>
      ) : rows === null ? (
        <ActivityIndicator style={s.loading} color={theme.accent as string} />
      ) : empty ? (
        <Text style={s.note}>{trail.length ? t('Dieser Ordner ist leer.') : t('Dieses Gerät teilt noch keine Dateien mit dir.')}</Text>
      ) : (
        <FlatList
          data={rows}
          keyExtractor={(r) => (r.kind === 'dir' ? `d:${r.name}` : `f:${r.item.hash}:${r.item.rel_path ?? r.item.name}`)}
          onEndReached={more}
          onEndReachedThreshold={0.5}
          renderItem={({ item: r }) =>
            r.kind === 'dir' ? (
              <Tap theme={theme} onPress={() => enter(r.name)} style={s.row} label={r.name}>
                <Glyph name={r.name} dir theme={theme} />
                <Text style={[s.name, s.grow]} numberOfLines={1}>{r.name}</Text>
                <Text style={s.chev}>›</Text>
              </Tap>
            ) : (
              <Tap theme={theme} onPress={() => openFile(r.item)} onLongPress={() => download(r.item)} style={s.row} label={r.item.name}>
                <Glyph name={r.item.name} dir={false} theme={theme} />
                <View style={s.grow}>
                  <Text style={s.name} numberOfLines={1}>{r.item.name}</Text>
                  <Text style={s.meta}>{humanSize(r.item.size, locale())}</Text>
                </View>
                {busy === r.item.hash ? (
                  <ActivityIndicator color={theme.accent as string} />
                ) : (
                  <Pressable onPress={() => download(r.item)} style={s.dl} accessibilityLabel={t('Herunterladen')} hitSlop={8}>
                    <Text style={s.dlIcon}>⤓</Text>
                  </Pressable>
                )}
              </Tap>
            )
          }
        />
      )}
    </Animated.View>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1, backgroundColor: th.bg },
    grow: { flex: 1 },
    head: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 4, paddingTop: 8, paddingBottom: 8 },
    backBtn: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    backIcon: { color: th.text, fontSize: 30, marginTop: -4 },
    title: { color: th.text, fontSize: 20, fontWeight: '700' },
    crumb: { color: th.text2, fontSize: 13 },
    loading: { marginTop: 40 },
    note: { color: th.text2, fontSize: 15, textAlign: 'center', padding: 32 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 14, paddingHorizontal: 16, minHeight: 64 },
    name: { color: th.text, fontSize: 16 },
    meta: { color: th.text2, fontSize: 13, marginTop: 2 },
    chev: { color: th.text2, fontSize: 22 },
    dl: { width: 40, height: 40, justifyContent: 'center', alignItems: 'center' },
    dlIcon: { color: th.accent, fontSize: 20 },
  }),
);
