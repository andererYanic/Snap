// APK-Dateien: App-Infos zeigen und an den Installer des Systems übergeben (mit Warnung).
// Android fragt beim ersten Mal selbst, ob Snap Apps installieren darf.
import { useEffect, useState } from 'react';
import { ActivityIndicator, Image, Modal, Pressable, StyleSheet, View } from 'react-native';
import Animated from 'react-native-reanimated';
import * as core from './core';
import { locale, t } from './i18n';
import { Icon } from './Icon';
import { fadeIn, m, sheetIn } from './motion';
import { Text } from './text';
import type { Theme } from './theme';
import { themed } from './ui';

/** Installierbare Android-Apps: einzelne APK oder App-Paket aus mehreren APKs. */
export const isApk = (name: string) => /\.(apk|apks|xapk|apkm)$/i.test(name);

export function ApkSheet({ path, name, size, theme, onClose, notify }: {
  path: string; name: string; size: number; theme: Theme; onClose: () => void; notify: (text: string) => void;
}) {
  const s = styles(theme);
  const [info, setInfo] = useState<core.ApkInfo | null | 'error' | undefined>(undefined);
  useEffect(() => {
    core.apkInfo(path).then((i) => setInfo(i ?? 'error')).catch(() => setInfo('error'));
  }, [path]);

  const [busy, setBusy] = useState(false);
  const install = async () => {
    if (busy) {
      return;
    }
    setBusy(true);
    try {
      const r = await core.installApk(path);
      if (r === 'permission') {
        notify(t('Erlaube Snap, Apps zu installieren, und tippe dann erneut auf „Installieren“.'));
      } else {
        if (r === 'installed' && info && info !== 'error') {
          notify(t('„{name}“ installiert.', { name: info.label }));
        }
        onClose();
      }
    } catch (e: any) {
      notify(String(e?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  const ok = info && info !== 'error' ? info : null;
  const state = !ok
    ? null
    : ok.installedCode === undefined
      ? t('Nicht installiert')
      : ok.installedCode < ok.versionCode
        ? t('Aktualisierung von {from} auf {to}', { from: ok.installedVersion ?? '', to: ok.version })
        : ok.installedCode === ok.versionCode
          ? t('Diese Version ist bereits installiert')
          : t('Ältere Version als installiert ({v})', { v: ok.installedVersion ?? '' });

  return (
    <Modal visible transparent animationType="none" statusBarTranslucent onRequestClose={onClose}>
      <Animated.View entering={m(fadeIn)} style={s.scrim}>
        <Pressable style={s.fill} onPress={onClose} accessibilityLabel={t('Schließen')} />
      </Animated.View>
      <Animated.View entering={m(sheetIn)} style={s.sheet}>
        <View style={s.grabber} />
        {info === undefined ? (
          <ActivityIndicator style={s.loading} color={theme.accent as string} />
        ) : !ok ? (
          <View style={s.body}>
            <Text style={s.title}>{name}</Text>
            <Text style={s.meta}>{t('Diese APK-Datei ist beschädigt oder unvollständig.')}</Text>
          </View>
        ) : (
          <View style={s.body}>
            <View style={s.head}>
              {ok.icon ? <Image source={{ uri: `file://${ok.icon}` }} style={s.icon} /> : <View style={[s.icon, s.iconEmpty]}><Icon name="phone" size={28} color={theme.text3 as string} /></View>}
              <View style={s.grow}>
                <Text style={s.title} numberOfLines={2}>{ok.label}</Text>
                <Text style={s.meta} numberOfLines={1}>{t('Version {v}', { v: ok.version })}{size > 0 ? ` · ${core.humanSize(size, locale())}` : ''}</Text>
                <Text style={s.meta} numberOfLines={1}>{ok.package}</Text>
                {(ok.splits ?? 1) > 1 && <Text style={s.meta} numberOfLines={1}>{t('App-Paket aus {n} Teilen', { n: ok.splits ?? 1 })}</Text>}
              </View>
            </View>
            <Text style={s.state}>{state}</Text>
            {ok.obb && <Text style={s.meta}>{t('Zusatzdaten (OBB) im Paket werden nicht übertragen; manche Spiele laden sie beim Start selbst nach.')}</Text>}
            <View style={s.warn}>
              <Icon name="warning" size={20} color={theme.warn as string} />
              <Text style={s.warnText}>{t('Apps außerhalb eines App-Stores können schaden. Installiere nur, wenn du der Quelle vertraust.')}</Text>
            </View>
            <View style={s.actions}>
              <Pressable onPress={onClose} style={[s.tonal, s.grow]} accessibilityRole="button">
                <Text style={s.tonalText}>{t('Abbrechen')}</Text>
              </Pressable>
              <Pressable onPress={install} style={[s.primary, s.grow]} accessibilityRole="button" disabled={busy}>
                {busy ? <ActivityIndicator color={theme.onAccent as string} /> : <Text style={s.primaryText}>{ok.installedCode !== undefined && ok.installedCode < ok.versionCode ? t('Aktualisieren') : t('Installieren')}</Text>}
              </Pressable>
            </View>
          </View>
        )}
      </Animated.View>
    </Modal>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1, minWidth: 0 },
    scrim: { flex: 1, backgroundColor: 'rgba(10,10,9,0.42)' },
    sheet: { paddingBottom: 24, paddingTop: 10, borderTopLeftRadius: 20, borderTopRightRadius: 20, backgroundColor: th.surface },
    grabber: { alignSelf: 'center', width: 32, height: 4, borderRadius: 2, marginBottom: 10, opacity: 0.5, backgroundColor: th.text3 },
    loading: { padding: 32 },
    body: { paddingHorizontal: 20, gap: 14 },
    head: { flexDirection: 'row', alignItems: 'center', gap: 14 },
    icon: { width: 56, height: 56, borderRadius: 14 },
    iconEmpty: { backgroundColor: th.surface2, justifyContent: 'center', alignItems: 'center' },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
    state: { color: th.text, fontSize: 15, lineHeight: 22 },
    warn: { flexDirection: 'row', gap: 10, alignItems: 'flex-start', padding: 12, borderRadius: 12, backgroundColor: th.surface2 },
    warnText: { flex: 1, color: th.text2, fontSize: 13, lineHeight: 19 },
    actions: { flexDirection: 'row', gap: 12 },
    primary: { minHeight: 48, borderRadius: 24, backgroundColor: th.accent, justifyContent: 'center', alignItems: 'center' },
    primaryText: { color: th.onAccent, fontSize: 16, fontWeight: '700' },
    tonal: { minHeight: 48, borderRadius: 24, backgroundColor: th.surface2, justifyContent: 'center', alignItems: 'center' },
    tonalText: { color: th.text, fontSize: 16, fontWeight: '600' },
  }));
