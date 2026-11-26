// Einstellungen mit Unterkategorien (Allgemein, Geräte, Papierkorb, Über Snap). Die Geräte sind
// hierher gewandert (vorher eigener Tab). Unterseiten gleiten herein, „Zurück“ führt zur Liste.
import { useEffect, useState } from 'react';
import { BackHandler, Pressable, ScrollView, StyleSheet, View } from 'react-native';
import Animated from 'react-native-reanimated';
import { request, storageRoot } from './core';
import { SignIdPage } from './SignId';
import { DropPage } from './Drop';
import * as drop from './drop';
import { t } from './i18n';
import { m, pageIn } from './motion';
import { Devices, General, TrashPage, type Device } from './Pages';
import { Text } from './text';
import type { Theme } from './theme';
import { themed } from './ui';
import { Icon, type IconName } from './Icon';

type Sub = 'general' | 'devices' | 'drop' | 'sign' | 'trash' | 'about';

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
  const [dropMode, setDropMode] = useState<drop.Mode | null>(null);

  useEffect(() => {
    request<{ devices?: Device[] }>('PairList').then((r) => setDevices(r?.devices ?? [])).catch(() => setDevices([]));
    drop.status().then((st) => setDropMode(st.mode)).catch(() => {});
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
  const title = { general: t('Allgemein'), devices: t('Geräte'), drop: t('Snap Drop'), sign: t('Digitale ID'), trash: t('Papierkorb'), about: t('Über Snap') };
  const dropMeta = dropMode === 'everyone' ? t('Für alle sichtbar') : dropMode === 'contacts' ? t('Für Kontakte sichtbar') : t('Unsichtbar · Senden an Geräte in der Nähe');
  const online = devices?.filter((d) => d.online).length ?? 0;

  if (sub) {
    return (
      <Animated.View key={sub} entering={m(pageIn('forward'))} style={s.fill}>
        <View style={s.header}>
          <Pressable onPress={() => { setDir('back'); setSub(null); }} style={s.back} accessibilityLabel={t('Zurück')}>
            <Icon name="back" color={p.theme.text as string} />
          </Pressable>
          <Text style={s.headerTitle}>{title[sub]}</Text>
        </View>
        {sub === 'general' && (
          <General theme={p.theme} notify={p.notify} material={p.material} setMaterial={p.setMaterial} materialAvailable={p.materialAvailable} fontName={p.fontName} onFont={p.onFont} />
        )}
        {sub === 'devices' && <Devices theme={p.theme} notify={p.notify} />}
        {sub === 'drop' && <DropPage theme={p.theme} notify={p.notify} />}
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

  const Row = ({ k, icon, label, meta }: { k: Sub; icon: IconName; label: string; meta: string }) => (
    <Pressable onPress={() => open(k)} android_ripple={{ color: p.theme.accentSoft as string }} style={s.row} accessibilityRole="button" accessibilityLabel={label}>
      <Icon name={icon} size={22} color={p.theme.text2 as string} />
      <View style={s.grow}>
        <Text style={s.label}>{label}</Text>
        <Text style={s.meta} numberOfLines={1}>{meta}</Text>
      </View>
      <Icon name="chevron" size={20} color={p.theme.text3 as string} />
    </Pressable>
  );

  return (
    <Animated.View key="root" entering={m(dir === 'back' ? pageIn('back') : undefined)} style={s.fill}>
      <ScrollView contentContainerStyle={s.page}>
        <Text style={s.bigTitle}>{t('Einstellungen')}</Text>
        <View style={s.card}>
          <Row k="general" icon="settings" label={t('Allgemein')} meta={t('Sprache, Erscheinung, Schriftart')} />
          <View style={s.sep} />
          <Row
            k="devices"
            icon="devices"
            label={t('Geräte')}
            meta={devices === null ? '' : t('{n} gekoppelt · {online} verbunden', { n: devices.length, online })}
          />
          <View style={s.sep} />
          <Row k="drop" icon="nearby" label={t('Snap Drop')} meta={dropMeta} />
          <View style={s.sep} />
          <Row k="sign" icon="sign" label={t('Digitale ID')} meta={t('PDFs signieren, Unterschrift')} />
        </View>
        <View style={s.card}>
          <Row k="trash" icon="trash" label={t('Papierkorb')} meta={t('Gelöschte Dateien wiederherstellen')} />
          <View style={s.sep} />
          <Row k="about" icon="info" label={t('Über Snap')} meta={t('Version {v}', { v: '0.1.0' })} />
        </View>
      </ScrollView>
    </Animated.View>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1, minWidth: 0 },
    page: { paddingHorizontal: 16, paddingBottom: 40, gap: 16 },
    bigTitle: { color: th.text, fontSize: 26, lineHeight: 32, fontWeight: '700', marginTop: 12, marginBottom: 4 },
    card: { borderRadius: 12, backgroundColor: th.surface, overflow: 'hidden' },
    row: { flexDirection: 'row', alignItems: 'center', gap: 20, minHeight: 64, paddingHorizontal: 16, paddingVertical: 10 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.line, marginLeft: 58 },
    label: { color: th.text, fontSize: 16, lineHeight: 22 },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18, marginTop: 1 },
    header: { flexDirection: 'row', alignItems: 'center', minHeight: 60, paddingHorizontal: 4 },
    back: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
    headerTitle: { flex: 1, color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700', marginLeft: 4 },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700', padding: 16, paddingBottom: 4 },
    para: { paddingHorizontal: 16, paddingBottom: 14, lineHeight: 20 },
  }));
