// Dateien eines anderen Geräts durchsuchen (von der Startseite aus): Freigaben als Ordner, darin
// Unterordner und Dateien aus dem lokal abgeglichenen Katalog – das Blättern braucht kein Netz.
// Öffnen lädt die Datei in den Cache und zeigt sie mit den eingebauten Anzeigen; Videos werden
// gestreamt. „Herunterladen“ legt eine Kopie in den Download-Ordner.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Alert, BackHandler, FlatList, Modal, Pressable, StyleSheet, View } from 'react-native';
import Animated from 'react-native-reanimated';
import * as core from './core';
import { humanSize, request } from './core';
import { locale, t } from './i18n';
import { m, pageIn } from './motion';
import { Text, TextInput } from './text';
import type { Theme } from './theme';
import { isVideo } from './thumbs';
import { Glyph, Tap, themed } from './ui';
import { Icon } from './Icon';

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

const ARCHIVE = /\.(zip|tar|tar\.gz|tgz)$/i;
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
  const [pw, setPw] = useState<{ item: CatalogItem; wrong: boolean; value: string } | null>(null);
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

  /** Archiv direkt auf dem anderen Gerät entpacken (Fernoperation, braucht dort files.write). */
  const extractThere = async (item: CatalogItem, password?: string) => {
    if (!item.share || !item.rel_path) {
      notify(t('Nur Dateien in Freigaben lassen sich dort entpacken.'));
      return;
    }
    setBusy(item.hash);
    try {
      await request({
        RemoteOp: { device: device.node_id, op: { op: 'Extract', share: item.share, archive: item.rel_path, ...(password ? { password } : {}) } },
      });
      notify(t('Auf {device} entpackt. Der Ordner erscheint nach dem nächsten Abgleich.', { device: device.name }));
    } catch (e: any) {
      if (e?.code === 'PasswordRequired') {
        setPw({ item, wrong: !!password, value: '' });
      } else {
        notify(String(e?.message ?? e));
      }
    } finally {
      setBusy(null);
    }
  };

  const tapFile = (item: CatalogItem) => {
    if (!ARCHIVE.test(item.name)) {
      openFile(item);
      return;
    }
    Alert.alert(item.name, undefined, [
      { text: t('Abbrechen'), style: 'cancel' },
      { text: t('Herunterladen'), onPress: () => download(item) },
      { text: t('Auf {device} entpacken', { device: device.name }), onPress: () => extractThere(item) },
    ]);
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
          <Icon name="back" color={theme.text as string} />
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
        <Text style={s.note}>{trail.length ? t('Dieser Ordner ist leer.') : t('Dieses Gerät gibt noch keine Ordner frei. Auf dem anderen Gerät: in „Dateien“ einen Ordner lange drücken › „Für Geräte freigeben“ – oder dort unter Einstellungen › Geräte auswählen.')}</Text>
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
                <Icon name="chevron" size={20} color={theme.text3 as string} />
              </Tap>
            ) : (
              <Tap theme={theme} onPress={() => tapFile(r.item)} onLongPress={() => download(r.item)} style={s.row} label={r.item.name}>
                <Glyph name={r.item.name} dir={false} theme={theme} />
                <View style={s.grow}>
                  <Text style={s.name} numberOfLines={1}>{r.item.name}</Text>
                  <Text style={s.meta}>{humanSize(r.item.size, locale())}</Text>
                </View>
                {busy === r.item.hash ? (
                  <ActivityIndicator color={theme.accent as string} />
                ) : (
                  <Pressable onPress={() => download(r.item)} style={s.dl} accessibilityLabel={t('Herunterladen')} hitSlop={8}>
                    <Icon name="download" size={22} color={theme.accent as string} />
                  </Pressable>
                )}
              </Tap>
            )
          }
        />
      )}
      <Modal visible={!!pw} transparent animationType="fade" statusBarTranslucent onRequestClose={() => setPw(null)}>
        <View style={s.modalBg}>
          <View style={s.modal}>
            <Text style={s.title}>{t('Passwort')}</Text>
            <Text style={s.meta}>{pw?.wrong ? t('Falsches Passwort.') : t('„{name}“ ist mit einem Passwort geschützt.', { name: pw?.item.name ?? '' })}</Text>
            <TextInput
              autoFocus
              secureTextEntry
              autoCorrect={false}
              autoCapitalize="none"
              value={pw?.value}
              onChangeText={(v) => setPw((x) => (x ? { ...x, value: v } : x))}
              style={s.input}
              accessibilityLabel={t('Passwort')}
            />
            <View style={s.modalBtns}>
              <Pressable onPress={() => setPw(null)} style={s.modalBtn} accessibilityRole="button">
                <Text style={s.modalBtnText}>{t('Abbrechen')}</Text>
              </Pressable>
              <Pressable
                onPress={() => pw && pw.value && (setPw(null), extractThere(pw.item, pw.value))}
                disabled={!pw?.value}
                style={s.modalBtn}
                accessibilityRole="button"
              >
                <Text style={[s.modalBtnText, s.strong]}>{t('Entpacken')}</Text>
              </Pressable>
            </View>
          </View>
        </View>
      </Modal>
    </Animated.View>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1, backgroundColor: th.bg },
    grow: { flex: 1 },
    head: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 4, minHeight: 60 },
    backBtn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    crumb: { color: th.text3, fontSize: 12, lineHeight: 16 },
    loading: { marginTop: 48 },
    note: { color: th.text2, fontSize: 14, lineHeight: 20, textAlign: 'center', padding: 40 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 16, paddingLeft: 16, paddingRight: 8, minHeight: 64 },
    name: { color: th.text, fontSize: 16, lineHeight: 22 },
    meta: { color: th.text3, fontSize: 13, lineHeight: 18 },
    dl: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    modalBg: { flex: 1, backgroundColor: 'rgba(10,10,9,0.42)', justifyContent: 'center', padding: 24 },
    modal: { backgroundColor: th.surface, borderRadius: 16, paddingHorizontal: 24, paddingTop: 22, paddingBottom: 12, gap: 10 },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 1.5, borderBottomColor: th.accent, paddingVertical: 8 },
    modalBtns: { flexDirection: 'row', justifyContent: 'flex-end', gap: 4, marginTop: 6 },
    modalBtn: { minHeight: 48, paddingHorizontal: 14, justifyContent: 'center' },
    modalBtnText: { color: th.accent, fontSize: 15 },
    strong: { fontWeight: '700' },
  }),
);
