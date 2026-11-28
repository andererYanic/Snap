// Snap als Dateiauswahl für andere Apps („Datei auswählen“ → Snap). Eigene schlanke Ansicht in
// `PickerActivity`: Ordner durchblättern, Datei antippen → an die aufrufende App. Herausgegeben
// wird nur, was hier angetippt wird; der Kern prüft jede Datei (gemeinsamer Speicher, nie Snaps
// eigene Ordner), Android gibt sie nur lesend und nur für diesen Aufruf frei.
import { useCallback, useEffect, useMemo, useState } from 'react';
import { ActivityIndicator, BackHandler, FlatList, Pressable, StatusBar, StyleSheet, useColorScheme, View } from 'react-native';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';
import * as core from './core';
import { humanSize, request, type DirEntry } from './core';
import { initLanguage, locale, t } from './i18n';
import { Icon } from './Icon';
import { Text } from './text';
import { snapTheme, type Theme } from './theme';
import { Glyph, themed } from './ui';

/** Art einer Datei nach Endung (nur zum Vorfiltern; maßgeblich prüft Android beim Abschluss). */
const TYPES: [RegExp, string][] = [
  [/\.(jpe?g|png|gif|webp|heic|heif|bmp|avif|svg|dng|cr2|nef|arw)$/i, 'image/'],
  [/\.(mp4|mkv|mov|webm|3gp|m4v|avi)$/i, 'video/'],
  [/\.(mp3|flac|m4a|ogg|oga|opus|wav|aac)$/i, 'audio/'],
  [/\.(txt|md|csv|tsv|html?|css|xml|json|vcf|ics|log)$/i, 'text/'],
  [/\.pdf$/i, 'application/pdf'],
  [/\.zip$/i, 'application/zip'],
  [/\.apk$/i, 'application/vnd.android.package-archive'],
  [/\.(docx?|odt|xlsx?|ods|pptx?|odp|epub|rtf)$/i, 'application/'],
];

/** Kann die Datei zu den gewünschten Arten passen? (leer = alles) */
export function mayMatch(name: string, mimes: string[]): boolean {
  if (mimes.length === 0) {
    return true;
  }
  const guess = TYPES.find(([re]) => re.test(name))?.[1];
  if (!guess) {
    return false;
  }
  return mimes.some((want) => {
    const w = want.toLowerCase();
    if (w === '*/*') return true;
    if (w.endsWith('/*')) return guess.startsWith(w.slice(0, -1)) || (guess.endsWith('/') && w.startsWith(guess));
    return guess.endsWith('/') ? w.startsWith(guess) : w === guess;
  });
}

const join = (dir: string, name: string) => (dir.endsWith('/') ? dir + name : `${dir}/${name}`);

export default function Picker() {
  const dark = useColorScheme() === 'dark';
  const theme = snapTheme(dark);
  const s = styles(theme);
  const [state, setState] = useState<'start' | 'ok' | 'noaccess'>('start');
  const [req, setReq] = useState<{ mimes: string[]; multiple: boolean }>({ mimes: [], multiple: false });
  const [root, setRoot] = useState('');
  const [path, setPath] = useState('');
  const [items, setItems] = useState<DirEntry[] | null>(null);
  const [chosen, setChosen] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      await initLanguage().catch(() => {});
      const r = await core.pickRequest().catch(() => null);
      if (r) setReq(r);
      const top = await core.storageRoot();
      setRoot(top);
      setPath(top);
      if (!(await core.hasAllFilesAccess())) {
        setState('noaccess');
        return;
      }
      await core.start();
      setState('ok');
    })().catch((e) => setError(String(e?.message ?? e)));
  }, []);

  useEffect(() => {
    if (state !== 'ok' || !path) return;
    setItems(null);
    request<{ entries: DirEntry[] }>({ ListDir: { path, show_hidden: false, offset: 0, limit: 2000 } })
      .then((r) => setItems(r.entries))
      .catch((e) => {
        setItems([]);
        setError(String(e?.message ?? e));
      });
  }, [state, path]);

  const shown = useMemo(() => {
    const coll = new Intl.Collator(locale(), { numeric: true, sensitivity: 'base' });
    return (items ?? [])
      .filter((e) => e.kind === 'Dir' || (e.kind === 'File' && mayMatch(e.name, req.mimes)))
      .sort((a, b) => (a.kind === b.kind ? coll.compare(a.name, b.name) : a.kind === 'Dir' ? -1 : 1));
  }, [items, req.mimes]);

  const up = useCallback(() => {
    if (path && path !== root) {
      setPath(path.slice(0, path.lastIndexOf('/')) || root);
      return true;
    }
    return false;
  }, [path, root]);

  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', () => {
      if (!up()) core.pickCancel();
      return true;
    });
    return () => sub.remove();
  }, [up]);

  const finish = (paths: string[]) =>
    core.pickFinish(paths).catch((e) => setError(String(e?.message ?? e)));

  const tap = (e: DirEntry) => {
    const p = join(path, e.name);
    setError(null);
    if (e.kind === 'Dir') {
      setPath(p);
    } else if (req.multiple) {
      setChosen((c) => (c.includes(p) ? c.filter((x) => x !== p) : [...c, p]));
    } else {
      finish([p]);
    }
  };

  const title = req.multiple ? t('Dateien auswählen') : t('Datei auswählen');
  const where = path === root ? t('Interner Speicher') : path.slice(path.lastIndexOf('/') + 1);
  return (
    <SafeAreaProvider>
      <SafeAreaView style={s.root}>
        <StatusBar barStyle={dark ? 'light-content' : 'dark-content'} />
        <View style={s.top}>
          <Pressable onPress={() => core.pickCancel()} style={s.btn} accessibilityRole="button" accessibilityLabel={t('Abbrechen')}>
            <Icon name="close" color={theme.text as string} />
          </Pressable>
          <View style={s.grow}>
            <Text style={s.title} numberOfLines={1}>{title}</Text>
            <Text style={s.meta} numberOfLines={1}>{where}</Text>
          </View>
        </View>
        {error && <Text style={s.error}>{error}</Text>}
        {state === 'noaccess' ? (
          <Text style={s.note}>{t('Snap hat noch keinen Zugriff auf deine Dateien. Öffne Snap einmal und erlaube den Zugriff.')}</Text>
        ) : state === 'start' || items === null ? (
          <ActivityIndicator style={s.loading} color={theme.accent as string} />
        ) : (
          <FlatList
            data={path !== root ? [null, ...shown] : shown}
            keyExtractor={(e) => (e ? e.name : '..')}
            ListEmptyComponent={<Text style={s.note}>{t('Hier ist nichts Passendes.')}</Text>}
            renderItem={({ item: e }) =>
              e === null ? (
                <Pressable onPress={up} style={s.row} android_ripple={{ color: theme.accentSoft as string }} accessibilityRole="button" accessibilityLabel={t('Zurück')}>
                  <Icon name="back" size={22} color={theme.text2 as string} />
                  <Text style={s.text}>{t('Zurück')}</Text>
                </Pressable>
              ) : (
                <Pressable
                  onPress={() => tap(e)}
                  style={[s.row, chosen.includes(join(path, e.name)) && { backgroundColor: theme.accentSoft as string }]}
                  android_ripple={{ color: theme.accentSoft as string }}
                  accessibilityRole="button"
                  accessibilityState={{ selected: chosen.includes(join(path, e.name)) }}
                  accessibilityLabel={e.name}
                >
                  <Glyph name={e.name} dir={e.kind === 'Dir'} theme={theme} size={36} />
                  <View style={s.grow}>
                    <Text style={s.text} numberOfLines={1}>{e.name}</Text>
                    {e.kind === 'File' && <Text style={s.meta}>{humanSize(e.size, locale())}</Text>}
                  </View>
                </Pressable>
              )
            }
          />
        )}
        {req.multiple && chosen.length > 0 && (
          <Pressable onPress={() => finish(chosen)} style={s.done} accessibilityRole="button" accessibilityLabel={t('Auswählen ({n})', { n: chosen.length })}>
            <Text style={s.doneText}>{t('Auswählen ({n})', { n: chosen.length })}</Text>
          </Pressable>
        )}
      </SafeAreaView>
    </SafeAreaProvider>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    root: { flex: 1, backgroundColor: th.bg },
    top: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 4, paddingVertical: 8, gap: 4 },
    btn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
    grow: { flex: 1, minWidth: 0 },
    title: { color: th.text, fontSize: 20, lineHeight: 26, fontWeight: '700' },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
    note: { color: th.text2, fontSize: 15, lineHeight: 22, padding: 24 },
    error: { color: th.danger, fontSize: 14, lineHeight: 20, paddingHorizontal: 16, paddingBottom: 8 },
    loading: { marginTop: 48 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 14, paddingHorizontal: 16, minHeight: 60 },
    text: { color: th.text, fontSize: 15, lineHeight: 20 },
    done: { margin: 16, minHeight: 52, borderRadius: 26, backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center' },
    doneText: { color: th.onAccent, fontSize: 16, fontWeight: '700' },
  }),
);
