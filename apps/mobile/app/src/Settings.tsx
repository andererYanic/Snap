// Einstellungen mit Unterkategorien (Allgemein, Geräte, Papierkorb, Über Snap). Die Geräte sind
// hierher gewandert (vorher eigener Tab). Unterseiten gleiten herein, „Zurück“ führt zur Liste.
import { useEffect, useState } from 'react';
import { BackHandler, Pressable, ScrollView, StyleSheet, View } from 'react-native';
import Animated from 'react-native-reanimated';
import { request, storageRoot } from './core';
import { SignIdPage } from './SignId';
import { t } from './i18n';
import { m, pageIn } from './motion';
import { Devices, General, TrashPage, type Device } from './Pages';
import { Text } from './text';
import type { Theme } from './theme';
import { themed } from './ui';

type Sub = 'general' | 'devices' | 'sign' | 'trash' | 'about';

interface Props {
  theme: Theme;
  notify: (t: string) => void;
  material: boolean;
  setMaterial: (v: boolean) => void;
  materialAvailable: boolean;
  fontName: string | null;
  onFont: (name: string | null) => void;
  /** Zähler: ändert er sich (erneutes Antippen des Tabs), geht es zur Liste zurück. */
  home?: number;
}

export default function Settings(p: Props) {
  const s = styles(p.theme);
  const [sub, setSub] = useState<Sub | null>(null);
  const [dir, setDir] = useState<'forward' | 'back'>('forward');
  const [devices, setDevices] = useState<Device[] | null>(null);

  useEffect(() => {
    request<{ devices?: Device[] }>('PairList').then((r) => setDevices(r?.devices ?? [])).catch(() => setDevices([]));
  }, [sub]);
  useEffect(() => {
    setDir('back');
    setSub(null);
  }, [p.home]);
  useEffect(() => {
    const h = BackHandler.addEventListener('hardwareBackPress', () => {
      if (sub) {
        setDir('back');
        setSub(null);
        return true;
      }
      return false;
    });
    return () => h.remove();
  }, [sub]);

  const open = (x: Sub) => {
    setDir('forward');
    setSub(x);
  };
  const title = { general: t('Allgemein'), devices: t('Geräte'), sign: t('Digitale ID'), trash: t('Papierkorb'), about: t('Über Snap') };
  const online = devices?.filter((d) => d.online).length ?? 0;

  if (sub) {
    return (
      <Animated.View key={sub} entering={m(pageIn('forward'))} style={s.fill}>
        <View style={s.header}>
          <Pressable onPress={() => { setDir('back'); setSub(null); }} style={s.back} accessibilityLabel={t('Zurück')}>
            <Text style={s.backText}>‹</Text>
          </Pressable>
          <Text style={s.headerTitle}>{title[sub]}</Text>
          <View style={s.back} />
        </View>
        {sub === 'general' && (
          <General theme={p.theme} notify={p.notify} material={p.material} setMaterial={p.setMaterial} materialAvailable={p.materialAvailable} fontName={p.fontName} onFont={p.onFont} />
        )}
        {sub === 'devices' && <Devices theme={p.theme} notify={p.notify} />}
        {sub === 'sign' && <SignIdPage theme={p.theme} notify={p.notify} downloads={async () => `${await storageRoot()}/Download`} />}
        {sub === 'trash' && <TrashPage theme={p.theme} notify={p.notify} />}
        {sub === 'about' && (
          <ScrollView contentContainerStyle={s.page}>
            <View style={s.card}>
              <Text style={s.title}>Snap</Text>
              <Text style={s.meta}>{t('Version {v}', { v: '0.1.0' })} · GPL-3.0</Text>
              <Text style={[s.meta, s.para]}>
                {t('Privates Dateisystem für die eigenen Geräte – ohne Cloud, ohne Google-Dienste.')}
              </Text>
              <Text style={[s.meta, s.para]}>{t('Schrift: Inconsolata (SIL Open Font License).')}</Text>
            </View>
          </ScrollView>
        )}
      </Animated.View>
    );
  }

  const Row = ({ k, icon, label, meta }: { k: Sub; icon: string; label: string; meta: string }) => (
    <Pressable onPress={() => open(k)} android_ripple={{ color: p.theme.accentSoft as string }} style={s.row} accessibilityRole="button" accessibilityLabel={label}>
      <View style={[s.icon, { backgroundColor: p.theme.accentSoft }]}>
        <Text style={[s.iconText, { color: p.theme.accent }]}>{icon}</Text>
      </View>
      <View style={s.grow}>
        <Text style={s.label}>{label}</Text>
        <Text style={s.meta} numberOfLines={1}>{meta}</Text>
      </View>
      <Text style={s.chev}>›</Text>
    </Pressable>
  );

  return (
    <Animated.View key="root" entering={m(dir === 'back' ? pageIn('back') : undefined)} style={s.fill}>
      <ScrollView contentContainerStyle={s.page}>
        <Text style={s.bigTitle}>{t('Einstellungen')}</Text>
        <View style={s.card}>
          <Row k="general" icon="⚙" label={t('Allgemein')} meta={t('Sprache, Erscheinung, Schriftart')} />
          <View style={s.sep} />
          <Row
            k="devices"
            icon="⌁"
            label={t('Geräte')}
            meta={devices === null ? '' : t('{n} gekoppelt · {online} verbunden', { n: devices.length, online })}
          />
          <View style={s.sep} />
          <Row k="sign" icon="✍" label={t('Digitale ID')} meta={t('PDFs signieren, Unterschrift')} />
        </View>
        <View style={s.card}>
          <Row k="trash" icon="🗑" label={t('Papierkorb')} meta={t('Gelöschte Dateien wiederherstellen')} />
          <View style={s.sep} />
          <Row k="about" icon="ℹ" label={t('Über Snap')} meta={t('Version {v}', { v: '0.1.0' })} />
        </View>
      </ScrollView>
    </Animated.View>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1, minWidth: 0 },
    page: { padding: 16, paddingBottom: 40, gap: 14 },
    bigTitle: { color: th.text, fontSize: 30, fontWeight: '700', marginTop: 4, marginBottom: 2 },
    card: { borderRadius: 18, backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke, overflow: 'hidden' },
    row: { flexDirection: 'row', alignItems: 'center', gap: 14, minHeight: 64, paddingHorizontal: 16, paddingVertical: 10 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.stroke, marginLeft: 66 },
    icon: { width: 36, height: 36, borderRadius: 10, justifyContent: 'center', alignItems: 'center' },
    iconText: { fontSize: 17 },
    label: { color: th.text, fontSize: 16, fontWeight: '500' },
    meta: { color: th.text2, fontSize: 13, marginTop: 2 },
    chev: { color: th.text2, fontSize: 22 },
    header: { flexDirection: 'row', alignItems: 'center', height: 56, paddingHorizontal: 4 },
    back: { width: 48, height: 48, justifyContent: 'center', alignItems: 'center' },
    backText: { color: th.text, fontSize: 26 },
    headerTitle: { flex: 1, textAlign: 'center', color: th.text, fontSize: 18, fontWeight: '600' },
    title: { color: th.text, fontSize: 22, fontWeight: '700', padding: 16, paddingBottom: 0 },
    para: { paddingHorizontal: 16, paddingBottom: 12 },
  }));
