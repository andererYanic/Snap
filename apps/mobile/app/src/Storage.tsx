// Speicherübersicht wie bei Android: Belegung nach Art (Bilder, Videos, Musik, Dokumente, Apps,
// Archive, Sonstiges), größte Dateien und größte Ordner – mit Drill-down in Ordner. Grundlage ist
// der lokale Suchindex (kein erneutes Durchsuchen des Speichers). Löschen nur über den Papierkorb.
import { useEffect, useState } from 'react';
import { ActivityIndicator, Alert, BackHandler, Modal, Pressable, ScrollView, StatusBar, StyleSheet, View } from 'react-native';
import * as core from './core';
import { humanSize, request } from './core';
import { locale, t } from './i18n';
import { Icon, type IconName } from './Icon';
import { Text } from './text';
import type { Theme } from './theme';
import { Glyph, SectionLabel, themed } from './ui';

interface Usage {
  path: string;
  total: number;
  files: number;
  children: { name: string; is_dir: boolean; size: number; files: number }[];
  by_type: { category: string; size: number; files: number }[];
  largest: { path: string; size: number }[];
}

const CAT: Record<string, { label: string; icon: IconName; color: string }> = {
  Bilder: { label: 'Bilder', icon: 'photos', color: '#3F8F6A' },
  Videos: { label: 'Videos', icon: 'video', color: '#C2543F' },
  Musik: { label: 'Musik', icon: 'music', color: '#8A5CC2' },
  Dokumente: { label: 'Dokumente', icon: 'document', color: '#3F6FC2' },
  Apps: { label: 'Apps', icon: 'grid', color: '#C29A3F' },
  Archive: { label: 'Archive', icon: 'archive', color: '#7A7A72' },
  Programmcode: { label: 'Programmcode', icon: 'file', color: '#5C8FA8' },
  Sonstiges: { label: 'Sonstiges', icon: 'file', color: '#9A9A92' },
};

export default function StoragePage({ root, theme, onClose, onOpenFolder, notify }: {
  root: string;
  theme: Theme;
  onClose: () => void;
  /** Ordner in der Dateiansicht öffnen. */
  onOpenFolder: (path: string) => void;
  notify: (m: string) => void;
}) {
  const s = styles(theme);
  const [trail, setTrail] = useState<string[]>([root]);
  const [usage, setUsage] = useState<Usage | null | 'error'>(null);
  const [gen, setGen] = useState(0);
  const path = trail[trail.length - 1];

  useEffect(() => {
    setUsage(null);
    request<Usage>({ SpaceUsage: { path, limit: 25 } }).then(setUsage).catch(() => setUsage('error'));
  }, [path, gen]);

  const back = () => (trail.length > 1 ? setTrail(trail.slice(0, -1)) : onClose());
  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', () => {
      back();
      return true;
    });
    return () => sub.remove();
  });

  const trash = (p: string) =>
    Alert.alert(t('In den Papierkorb'), p.slice(p.lastIndexOf('/') + 1), [
      { text: t('Abbrechen'), style: 'cancel' },
      {
        text: t('In den Papierkorb'),
        style: 'destructive',
        onPress: () =>
          request({ FileOp: { op: { Trash: { paths: [p] } }, wait: true } })
            .then(() => setGen((g) => g + 1))
            .catch((e) => notify(String(e?.message ?? e))),
      },
    ]);

  const u = usage && usage !== 'error' ? usage : null;
  const name = path === root ? t('Interner Speicher') : path.slice(path.lastIndexOf('/') + 1);
  const cats = u ? [...u.by_type].sort((a, b) => b.size - a.size) : [];
  const folders = u ? u.children.filter((c) => c.is_dir && c.size > 0).slice(0, 12) : [];
  return (
    <Modal visible animationType="slide" onRequestClose={back} statusBarTranslucent>
      <View style={s.root}>
        <StatusBar barStyle={theme.dark ? 'light-content' : 'dark-content'} />
        <View style={s.top}>
          <Pressable onPress={back} style={s.btn} accessibilityLabel={t('Zurück')}>
            <Icon name="back" color={theme.text as string} />
          </Pressable>
          <View style={s.grow}>
            <Text style={s.title} numberOfLines={1}>{name}</Text>
            {u && <Text style={s.meta}>{t('{size} in {n} Dateien', { size: humanSize(u.total, locale()), n: u.files.toLocaleString(locale()) })}</Text>}
          </View>
        </View>
        {usage === null ? (
          <ActivityIndicator style={s.loading} color={theme.accent as string} />
        ) : usage === 'error' ? (
          <Text style={s.note}>{t('Der Speicher wird noch erfasst – gleich noch einmal versuchen.')}</Text>
        ) : (
          <ScrollView contentContainerStyle={s.page}>
            {/* Balken nach Art, wie in den Android-Einstellungen */}
            <View style={s.bar}>
              {cats.filter((c) => c.size > 0).map((c) => (
                <View key={c.category} style={{ flex: Math.max(c.size, 1), backgroundColor: (CAT[c.category] ?? CAT.Sonstiges).color }} />
              ))}
            </View>
            <View style={s.card}>
              {cats.map((c, i) => {
                const k = CAT[c.category] ?? CAT.Sonstiges;
                return (
                  <View key={c.category}>
                    {i > 0 && <View style={s.sep} />}
                    <View style={s.row}>
                      <View style={[s.dot, { backgroundColor: k.color }]} />
                      <Icon name={k.icon} size={20} color={theme.text2 as string} />
                      <Text style={[s.text, s.grow]}>{t(k.label)}</Text>
                      <Text style={s.size}>{humanSize(c.size, locale())}</Text>
                    </View>
                  </View>
                );
              })}
            </View>

            {u!.largest.length > 0 && <SectionLabel theme={theme} style={s.section}>{t('Größte Dateien')}</SectionLabel>}
            <View style={s.card}>
              {u!.largest.slice(0, 15).map((f, i) => {
                const fname = f.path.slice(f.path.lastIndexOf('/') + 1);
                return (
                  <View key={f.path}>
                    {i > 0 && <View style={s.sep} />}
                    <Pressable onLongPress={() => trash(f.path)} onPress={() => core.openFile(f.path).catch(() => {})} style={s.row} android_ripple={{ color: theme.accentSoft as string }} accessibilityLabel={fname} accessibilityHint={t('Lange drücken: in den Papierkorb')}>
                      <Glyph name={fname} dir={false} theme={theme} />
                      <View style={s.grow}>
                        <Text style={s.text} numberOfLines={1}>{fname}</Text>
                        <Text style={s.meta} numberOfLines={1}>{f.path.slice(0, f.path.lastIndexOf('/')).replace(root, '') || '/'}</Text>
                      </View>
                      <Text style={s.size}>{humanSize(f.size, locale())}</Text>
                    </Pressable>
                  </View>
                );
              })}
            </View>

            {folders.length > 0 && <SectionLabel theme={theme} style={s.section}>{t('Größte Ordner')}</SectionLabel>}
            <View style={s.card}>
              {folders.map((c, i) => (
                <View key={c.name}>
                  {i > 0 && <View style={s.sep} />}
                  <Pressable
                    onPress={() => setTrail([...trail, `${path}/${c.name}`])}
                    onLongPress={() => onOpenFolder(`${path}/${c.name}`)}
                    style={s.row}
                    android_ripple={{ color: theme.accentSoft as string }}
                    accessibilityLabel={c.name}
                    accessibilityHint={t('Lange drücken: Ordner öffnen')}
                  >
                    <Glyph name={c.name} dir theme={theme} />
                    <View style={s.grow}>
                      <Text style={s.text} numberOfLines={1}>{c.name}</Text>
                      <View style={s.track}>
                        <View style={[s.fill, { width: `${Math.max(2, Math.min(100, (c.size / Math.max(1, u!.total)) * 100))}%` }]} />
                      </View>
                    </View>
                    <Text style={s.size}>{humanSize(c.size, locale())}</Text>
                  </Pressable>
                </View>
              ))}
            </View>
          </ScrollView>
        )}
      </View>
    </Modal>
  );
}

const styles = themed((th) =>
  StyleSheet.create({
    root: { flex: 1, backgroundColor: th.bg },
    top: { flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 4, gap: 4 },
    btn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
    grow: { flex: 1, minWidth: 0 },
    title: { color: th.text, fontSize: 20, lineHeight: 26, fontWeight: '700' },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
    note: { color: th.text2, fontSize: 15, lineHeight: 22, padding: 24 },
    loading: { marginTop: 48 },
    page: { padding: 16, paddingBottom: 48 },
    bar: { flexDirection: 'row', height: 14, borderRadius: 7, overflow: 'hidden', backgroundColor: th.surface2, marginBottom: 16 },
    card: { borderRadius: 14, backgroundColor: th.surface, overflow: 'hidden' },
    section: { marginTop: 20, marginBottom: 8 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 16, minHeight: 56 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.surface2, marginLeft: 16 },
    dot: { width: 10, height: 10, borderRadius: 5 },
    text: { color: th.text, fontSize: 15, lineHeight: 20 },
    size: { color: th.text2, fontSize: 14, fontVariant: ['tabular-nums'] },
    track: { height: 4, borderRadius: 2, backgroundColor: th.surface2, marginTop: 6, overflow: 'hidden' },
    fill: { height: 4, borderRadius: 2, backgroundColor: th.accent },
  }),
);
