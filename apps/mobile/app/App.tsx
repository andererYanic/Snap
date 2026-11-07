// Snap für Android: Start (Berechtigung, Kern), untere Navigation, Rückgängig-Leiste.
// Gestaltung: docs/MOBILE_UX.md (Snap-Optik, Android-Bedienung, Material You wahlweise).
import { useCallback, useEffect, useRef, useState } from 'react';
import { AppState, Pressable, ScrollView, StatusBar, StyleSheet, useColorScheme, View } from 'react-native';
import { Text } from './src/text';
import { GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, { FadeIn } from 'react-native-reanimated';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';
import * as core from './src/core';
import Files from './src/Files';
import { initLanguage, t } from './src/i18n';
import { Devices, More } from './src/Pages';
import Photos from './src/Photos';
import { materialTheme, snapTheme, type Theme } from './src/theme';
import { NavIcon, themed } from './src/ui';
import { fadeIn, initMotion, m, toastIn, toastOut } from './src/motion';

type Tab = 'files' | 'photos' | 'devices' | 'more';

export default function App() {
  const dark = useColorScheme() === 'dark';
  const [ready, setReady] = useState<'start' | 'permission' | 'ok' | string>('start');
  const [root, setRoot] = useState('/storage/emulated/0');
  const [tab, setTab] = useState<Tab>('files');
  // Erneutes Antippen von „Dateien“ führt zur Startansicht
  const [filesHome, setFilesHome] = useState(0);
  const [material, setMaterialState] = useState(false);
  const [materialAvailable, setMaterialAvailable] = useState(false);
  const [crash, setCrash] = useState<string | null>(null);
  const [toast, setToast] = useState<{ text: string; undo?: () => void } | null>(null);
  const [, setSelecting] = useState(false);
  // Schrift: Name der eigenen Schrift (null = Inconsolata); Version erzwingt Neuzeichnen
  const [fontName, setFontName] = useState<string | null>(null);
  const [fontVersion, setFontVersion] = useState(0);
  const fontChanged = useCallback((name: string | null) => {
    setFontName(name);
    setFontVersion((v) => v + 1);
  }, []);
  // Inhalt des unteren Platzes von „Dateien“ (Aktionsleiste, „Hier einfügen“); null = Hauptmenü
  const [bottomBar, setBottomBar] = useState<React.ReactNode>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const theme: Theme = material && materialAvailable ? materialTheme(dark) : snapTheme(dark);

  const boot = useCallback(async (force = false) => {
    try {
      const [, , font] = await Promise.all([initLanguage(), initMotion(), core.applyFont().catch(() => null)]);
      setFontName(font);
      const sdk = await core.sdkInt();
      setMaterialAvailable(sdk >= 31);
      setMaterialState((await core.getPref('material')) === '1');
      setRoot(await core.storageRoot());
      if (!(await core.hasAllFilesAccess())) {
        setReady('permission');
        return;
      }
      // Ist der letzte Start abgestürzt? Dann Bericht zeigen statt erneut abzustürzen.
      const report = force ? null : await core.lastCrash();
      if (report) {
        setCrash(report);
        setReady('crash');
        return;
      }
      setReady('start');
      await core.start();
      setReady('ok');
    } catch (e: any) {
      setReady(String(e?.message ?? e));
    }
  }, []);

  // Einmal beim Öffnen starten
  useEffect(() => {
    boot();
  }, [boot]);

  // Nach Rückkehr aus den Systemeinstellungen erneut prüfen
  useEffect(() => {
    const sub = AppState.addEventListener('change', (st) => {
      if (st === 'active' && ready === 'permission') {
        boot();
      }
    });
    return () => sub.remove();
  }, [boot, ready]);

  const notify = useCallback((text: string, undo?: () => void) => {
    setToast({ text, undo });
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setToast(null), undo ? 6000 : 3500);
  }, []);

  const setMaterial = (v: boolean) => {
    setMaterialState(v);
    core.setPref('material', v ? '1' : '0');
  };

  const s = styles(theme);
  const bar = <StatusBar barStyle={dark ? 'light-content' : 'dark-content'} />;

  if (ready !== 'ok') {
    return (
      <GestureHandlerRootView style={{ flex: 1 }}>
    <SafeAreaProvider>
        <View style={s.bg}>
          {bar}
          <View style={s.tint} />
          <SafeAreaView style={s.center}>
            <View style={s.card}>
              <Text style={s.title}>Snap</Text>
              {ready === 'start' ? (
                <Text style={s.muted}>{t('Startet …')}</Text>
              ) : ready === 'permission' ? (
                <>
                  <Text style={s.muted}>
                    {t('Snap ist ein Dateimanager und braucht dafür Zugriff auf alle Dateien. Deine Dateien bleiben auf deinen Geräten – ohne Cloud, ohne Google-Dienste.')}
                  </Text>
                  <Pressable style={s.btn} onPress={core.requestAllFilesAccess} accessibilityRole="button">
                    <Text style={s.btnText}>{t('Zugriff erlauben')}</Text>
                  </Pressable>
                </>
              ) : ready === 'crash' ? (
                <>
                  <Text style={s.muted}>{t('Snap wurde beim letzten Start unerwartet beendet. Bericht:')}</Text>
                  <ScrollView style={s.report}>
                    <Text selectable style={s.reportText}>{crash}</Text>
                  </ScrollView>
                  <Pressable style={s.btn} onPress={() => boot(true)} accessibilityRole="button">
                    <Text style={s.btnText}>{t('Erneut versuchen')}</Text>
                  </Pressable>
                </>
              ) : (
                <Text selectable style={s.error}>{ready}</Text>
              )}
            </View>
          </SafeAreaView>
        </View>
      </SafeAreaProvider>
    </GestureHandlerRootView>
    );
  }

  const tabs: { key: Tab; label: string }[] = [
    { key: 'files', label: t('Dateien') },
    { key: 'photos', label: t('Fotos') },
    { key: 'devices', label: t('Geräte') },
    { key: 'more', label: t('Mehr') },
  ];

  return (
    <GestureHandlerRootView style={{ flex: 1 }}>
    <SafeAreaProvider>
      <View style={s.bg} key={`font-${fontVersion}`}>
        {bar}
        <View style={s.tint} />
        <SafeAreaView style={s.fill} edges={['top', 'left', 'right']}>
          {tab !== 'files' && <Text style={s.header}>{tabs.find((x) => x.key === tab)?.label}</Text>}
          <Animated.View key={tab} entering={m(FadeIn.duration(180))} style={s.fill}>
            {tab === 'files' && <Files theme={theme} root={root} notify={notify} selectionChanged={setSelecting} bottomSlot={setBottomBar} home={filesHome} onFont={fontChanged} />}
            {tab === 'photos' && <Photos theme={theme} />}
            {tab === 'devices' && <Devices theme={theme} notify={notify} />}
            {tab === 'more' && <More theme={theme} notify={notify} material={material} setMaterial={setMaterial} materialAvailable={materialAvailable} fontName={fontName} onFont={fontChanged} />}
          </Animated.View>
          {toast && (
            <Animated.View entering={m(toastIn)} exiting={m(toastOut)} style={s.toast}>
              <Text style={s.toastText}>{toast.text}</Text>
              {toast.undo && (
                <Pressable onPress={() => { toast.undo?.(); setToast(null); }} style={s.toastBtn}>
                  <Text style={s.link}>{t('Rückgängig')}</Text>
                </Pressable>
              )}
            </Animated.View>
          )}
        </SafeAreaView>
        {/* Fester Platz unten: Hauptmenü oder – beim Markieren – die Leiste von „Dateien“.
            Gleiche Höhe in beiden Fällen, damit die Liste darüber nie springt. */}
        <SafeAreaView edges={['bottom']} style={s.nav}>
          {tab === 'files' && bottomBar ? (
            <Animated.View key="bar" entering={m(fadeIn)} style={s.slot}>{bottomBar}</Animated.View>
          ) : (
            tabs.map((x) => {
              const on = tab === x.key;
              return (
                <Pressable key={x.key} style={s.navItem} onPress={() => { if (tab !== x.key) { core.haptic('tick'); } else if (x.key === 'files') { setFilesHome((n) => n + 1); } setTab(x.key); }} accessibilityRole="tab" accessibilityState={{ selected: on }}>
                  <View style={[s.navPill, on && s.navPillOn]}>
                    <NavIcon name={x.key} color={(on ? theme.accent : theme.text2) as string} />
                  </View>
                  <Text style={[s.navLabel, on && s.navLabelOn]}>{x.label}</Text>
                </Pressable>
              );
            })
          )}
        </SafeAreaView>
      </View>
    </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    bg: { flex: 1, backgroundColor: th.bg },
    // Weicher Farbverlauf der Snap-Optik (zweite Farbe als Schleier oben)
    tint: { position: 'absolute', top: 0, left: 0, right: 0, height: '55%', backgroundColor: th.bgTint, opacity: 0.6, borderBottomLeftRadius: 400, borderBottomRightRadius: 200 },
    fill: { flex: 1 },
    center: { flex: 1, justifyContent: 'center', padding: 24 },
    card: { padding: 24, borderRadius: 24, backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke },
    title: { color: th.text, fontSize: 28, fontWeight: '700', marginBottom: 12 },
    muted: { color: th.text2, fontSize: 15, lineHeight: 22 },
    error: { color: th.danger, fontSize: 15 },
    report: { maxHeight: 260, marginTop: 12, padding: 10, borderRadius: 10, backgroundColor: th.accentSoft },
    reportText: { color: th.text, fontSize: 12, fontFamily: 'monospace' },
    btn: { minHeight: 52, justifyContent: 'center', alignItems: 'center', borderRadius: 16, backgroundColor: th.accent, marginTop: 20 },
    btnText: { color: th.onAccent, fontSize: 16, fontWeight: '600' },
    header: { color: th.text, fontSize: 26, fontWeight: '700', paddingHorizontal: 16, paddingTop: 12, paddingBottom: 4 },
    nav: { flexDirection: 'row', backgroundColor: th.card, borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: th.stroke },
    slot: { flex: 1, flexDirection: 'row', minHeight: 64 },
    navItem: { flex: 1, alignItems: 'center', paddingTop: 10, paddingBottom: 8, minHeight: 64 },
    navPill: { width: 60, height: 32, borderRadius: 16, justifyContent: 'center', alignItems: 'center' },
    navPillOn: { backgroundColor: th.accentSoft },
    navLabel: { color: th.text2, fontSize: 12, marginTop: 4 },
    navLabelOn: { color: th.text, fontWeight: '600' },
    toast: {
      position: 'absolute', left: 12, right: 12, bottom: 12, flexDirection: 'row', alignItems: 'center', minHeight: 52,
      paddingLeft: 16, borderRadius: 14, backgroundColor: th.dark ? '#e8e8ee' : '#2b2d35', elevation: 8,
    },
    toastText: { flex: 1, color: th.dark ? '#1b1b1f' : '#f1f1f4', fontSize: 14 },
    toastBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 16 },
    link: { color: th.dark ? '#3b6fe0' : '#9db8ff', fontSize: 15, fontWeight: '700' },
  }));
