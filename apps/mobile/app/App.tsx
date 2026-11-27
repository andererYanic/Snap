// Snap für Android: Start (Berechtigung, Kern), untere Navigation, Rückgängig-Leiste.
// Gestaltung: docs/MOBILE_UX.md (Snap-Optik, Android-Bedienung, Material You wahlweise).
import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, AppState, Pressable, ScrollView, StatusBar, StyleSheet, useColorScheme, View } from 'react-native';
import { FontContext, Text } from './src/text';
import { GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, { FadeIn } from 'react-native-reanimated';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';
import * as core from './src/core';
import Files from './src/Files';
import { initLanguage, t } from './src/i18n';
import Settings from './src/Settings';
import Photos from './src/Photos';
import { materialTheme, snapTheme, type Theme } from './src/theme';
import { NavIcon, themed } from './src/ui';
import { fadeIn, initMotion, m, toastIn, toastOut } from './src/motion';
import * as drop from './src/drop';
import * as clip from './src/clipboard';
import { DropSendSheet, HotspotSheet, OfferSheet, ReceiveBar } from './src/Drop';

type Tab = 'files' | 'photos' | 'settings';

export default function App() {
  const dark = useColorScheme() === 'dark';
  const [ready, setReady] = useState<'start' | 'permission' | 'ok' | string>('start');
  const [root, setRoot] = useState('/storage/emulated/0');
  const [tab, setTab] = useState<Tab>('files');
  // Erneutes Antippen von „Dateien“ führt zur Startansicht
  const [filesHome, setFilesHome] = useState(0);
  const [settingsHome, setSettingsHome] = useState(0);
  const [material, setMaterialState] = useState(false);
  const [materialAvailable, setMaterialAvailable] = useState(false);
  const [crash, setCrash] = useState<string | null>(null);
  const [toast, setToast] = useState<{ text: string; undo?: () => void; action?: { label: string; run: () => void } } | null>(null);
  // Snap Drop: Senden (gewählte Dateien), eingehendes Angebot, Kurzanfrage für den Offline-Weg
  const [dropShare, setDropShare] = useState<{ paths: string[]; text: string | null } | null>(null);
  const setDropPaths = useCallback((paths: string[] | null) => setDropShare(paths ? { paths, text: null } : null), []);
  const [incoming, setIncoming] = useState<drop.IncomingOffer | null>(null);
  const [hotspot, setHotspot] = useState<{ data: string; offer: drop.HotspotOffer } | null>(null);
  const [, setSelecting] = useState(false);
  // Schrift: Name der eigenen Schrift (null = Inconsolata); Version erzwingt Neuzeichnen
  const [fontName, setFontName] = useState<string | null>(null);
  const [fontVersion, setFontVersion] = useState(0);
  const fontChanged = useCallback((name: string | null) => {
    setFontName(name);
    // Neuer Schriftname → alle Texte übernehmen die Schrift, ohne dass man die Seite verlässt
    setFontVersion((v) => {
      const next = v + 1;
      core.applyFont(`SnapFont-${next}`).catch(() => {});
      return next;
    });
  }, []);
  const fontFamily = fontVersion === 0 ? 'SnapFont' : `SnapFont-${fontVersion}`;
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
      // Zwischenablage automatisch an die gewählten eigenen Geräte (nur im Vordergrund lesbar)
      if (st === 'active' && ready === 'ok') {
        clip.syncOnForeground().catch(() => {});
      }
      // Ein offenes Angebot könnte inzwischen per Mitteilung beantwortet worden sein
      if (st === 'active' && ready === 'ok') {
        drop
          .offers()
          .then((list) => setIncoming((cur) => (cur && !list.some((o) => o.id === cur.id) ? null : cur)))
          .catch(() => {});
      }
    });
    return () => sub.remove();
  }, [boot, ready]);

  const notify = useCallback((text: string, undo?: () => void) => {
    setToast({ text, undo });
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setToast(null), undo ? 6000 : 3500);
  }, []);

  const notifyAction = useCallback((text: string, label: string, run: () => void) => {
    setToast({ text, action: { label, run } });
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setToast(null), 8000);
  }, []);

  // Snap Drop: Ereignisse aus dem Kern und Bluetooth
  useEffect(() => {
    if (ready !== 'ok') {
      return;
    }
    drop.listen();
    drop.onHotspotRequest((data, offer) => setHotspot({ data, offer }));
    // Aus dem „Teilen“-Menü einer anderen App: Snap-Drop-Blatt mit den Inhalten öffnen
    const pickShare = () =>
      core.takeShare().then((sh) => {
        if (sh && (sh.files.length > 0 || sh.text)) {
          setDropShare({ paths: sh.files, text: sh.text });
        }
      }).catch(() => {});
    pickShare();
    const offShare = core.onShare(pickShare);
    drop.syncRadio().catch(() => {});
    drop.initBackground().catch(() => {});
    const saveContact = (id: number) =>
      drop
        .saveContact(id)
        .then((c) => notify(t('{name} ist jetzt ein Kontakt.', { name: c.name })))
        .catch((e) => notify(String(e?.message ?? e)));
    const off = core.onEvent((ev) => {
      if ((ev.type === 'DropProgress' || ev.type === 'DropFinished') && ev.incoming) {
        // Schon über Mitteilung oder Popup beantwortet
        setIncoming((cur) => (cur?.id === ev.id ? null : cur));
      }
      if (ev.type === 'DropOffer') {
        drop
          .offers()
          .then((list) => setIncoming((cur) => cur ?? list.find((o) => o.id === ev.id) ?? null))
          .catch(() => {});
      } else if (ev.type === 'DropOfferClosed') {
        setIncoming((cur) => {
          if (cur?.id === ev.id) {
            notify(t('Das Angebot wurde zurückgezogen.'));
            return null;
          }
          return cur;
        });
      } else if (ev.type === 'DropFinished') {
        const files: string[] = ev.files ?? [];
        const count = files.length === 1 ? t('1 Datei') : t('{n} Dateien', { n: files.length });
        if (ev.incoming && ev.ok && ev.text) {
          // Text immer in die Zwischenablage; Links lassen sich direkt öffnen
          drop.leaveHotspot();
          core.copyText(ev.text).catch(() => {});
          if (ev.link) {
            const link: string = ev.link;
            notifyAction(t('Link von {name} – in die Zwischenablage kopiert.', { name: ev.peer }), t('Öffnen'), () =>
              core.openLink(link).catch((e) => notify(String(e?.message ?? e))),
            );
          } else if (ev.can_save_contact) {
            notifyAction(t('Text von {name} in die Zwischenablage kopiert.', { name: ev.peer }), t('Als Kontakt speichern'), () => saveContact(ev.id));
          } else {
            notify(t('Text von {name} in die Zwischenablage kopiert.', { name: ev.peer }));
          }
        } else if (ev.incoming) {
          drop.leaveHotspot();
          if (!ev.ok) {
            notify(t('Empfang fehlgeschlagen: {error}', { error: ev.error ?? '' }));
          } else if (ev.can_save_contact) {
            notifyAction(t('{files} von {name} gespeichert.', { files: count, name: ev.peer }), t('Als Kontakt speichern'), () => saveContact(ev.id));
          } else if (files.length === 1) {
            notifyAction(t('{files} von {name} gespeichert.', { files: count, name: ev.peer }), t('Öffnen'), () => core.openFile(files[0]).catch(() => {}));
          } else {
            notify(t('{files} von {name} gespeichert.', { files: count, name: ev.peer }));
          }
        } else if (ev.ok) {
          if (ev.can_save_contact) {
            notifyAction(t('An {name} gesendet.', { name: ev.peer }), t('Als Kontakt speichern'), () => saveContact(ev.id));
          } else {
            notify(t('An {name} gesendet.', { name: ev.peer }));
          }
        }
      } else if (ev.type === 'DropVisibilityChanged') {
        drop.syncRadio(ev.mode).catch(() => {});
      } else if (ev.type === 'PairConfirm') {
        // Kopplung erst nach Vergleich des Prüfcodes (ein weitergegebener Code allein reicht nicht)
        Alert.alert(
          t('Gerät koppeln?'),
          t('„{name}“ möchte sich koppeln.\n\nPrüfcode: {code}\n\nNur bestätigen, wenn auf dem anderen Gerät derselbe Code steht.', { name: ev.name, code: ev.code }),
          [
            { text: t('Ablehnen'), style: 'cancel', onPress: () => core.request({ PairConfirm: { accept: false } }).catch(() => {}) },
            { text: t('Bestätigen'), onPress: () => core.request({ PairConfirm: { accept: true } }).catch(() => {}) },
          ],
          { cancelable: false },
        );
      } else if (ev.type === 'ClipboardReceived') {
        // Android hat den Text schon übernommen (DropNotify); hier nur merken und melden
        clip.remember(ev.text);
        notify(t('Zwischenablage von „{name}“ übernommen.', { name: ev.name }));
      } else if (ev.type === 'DeviceRemoved') {
        // Geöffnete Dateien des Geräts sofort unzugänglich machen (Cache ist nur ein Cache)
        core.clearRemoteCache().catch(() => {});
      } else if (ev.type === 'DeviceUnpairedByPeer') {
        notify(t('„{name}“ hat die Kopplung aufgehoben. Seine Dateien sind hier nicht mehr verfügbar.', { name: ev.name }));
      }
    });
    return () => {
      off();
      offShare();
      drop.onHotspotRequest(null);
    };
  }, [ready, notify, notifyAction]);

  const answerHotspot = async (accept: boolean) => {
    const h = hotspot;
    setHotspot(null);
    if (!h || !accept) {
      return;
    }
    try {
      notify(t('Verbinde direkt mit {name} …', { name: h.offer.name }));
      await drop.acceptHotspot(h.data, h.offer);
      // Falls der Sender nicht nachkommt: nach 3 Minuten zurück ins normale Netz.
      setTimeout(() => drop.leaveHotspot(), 180_000);
    } catch (e: any) {
      drop.leaveHotspot();
      notify(String(e?.message ?? e));
    }
  };

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
    { key: 'settings', label: t('Einstellungen') },
  ];

  return (
    <GestureHandlerRootView style={{ flex: 1 }}>
    <SafeAreaProvider>
      <FontContext.Provider value={fontFamily}>
      <View style={s.bg}>
        {bar}
        <SafeAreaView style={s.fill} edges={['top', 'left', 'right']}>
          {tab === 'photos' && <Text style={s.header}>{tabs.find((x) => x.key === tab)?.label}</Text>}
          <Animated.View key={tab} entering={m(FadeIn.duration(180))} style={s.fill}>
            {tab === 'files' && <Files theme={theme} root={root} notify={notify} selectionChanged={setSelecting} bottomSlot={setBottomBar} home={filesHome} onFont={fontChanged} onDrop={setDropPaths} />}
            {tab === 'photos' && <Photos theme={theme} notify={notify} onDrop={setDropPaths} />}
            {tab === 'settings' && (
              <Settings theme={theme} notify={notify} material={material} setMaterial={setMaterial} materialAvailable={materialAvailable} fontName={fontName} onFont={fontChanged} home={settingsHome} />
            )}
          </Animated.View>
          {toast && (
            <Animated.View entering={m(toastIn)} exiting={m(toastOut)} style={s.toast}>
              <Text style={s.toastText}>{toast.text}</Text>
              {toast.undo && (
                <Pressable onPress={() => { toast.undo?.(); setToast(null); }} style={s.toastBtn}>
                  <Text style={s.link}>{t('Rückgängig')}</Text>
                </Pressable>
              )}
              {toast.action && (
                <Pressable onPress={() => { toast.action?.run(); setToast(null); }} style={s.toastBtn} accessibilityRole="button">
                  <Text style={s.link}>{toast.action.label}</Text>
                </Pressable>
              )}
            </Animated.View>
          )}
          <ReceiveBar theme={theme} />
          {dropShare && <DropSendSheet paths={dropShare.paths} text={dropShare.text} theme={theme} notify={notify} onClose={() => setDropShare(null)} />}
          {incoming && <OfferSheet offer={incoming} theme={theme} onDone={() => setIncoming(null)} />}
          {hotspot && <HotspotSheet offer={hotspot.offer} theme={theme} onAnswer={answerHotspot} />}
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
                <Pressable key={x.key} style={s.navItem} onPress={() => { if (tab !== x.key) { core.haptic('tick'); } else if (x.key === 'files') { setFilesHome((n) => n + 1); } else if (x.key === 'settings') { setSettingsHome((n) => n + 1); } setTab(x.key); }} accessibilityRole="tab" accessibilityState={{ selected: on }}>
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
      </FontContext.Provider>
    </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    bg: { flex: 1, backgroundColor: th.bg },
    fill: { flex: 1 },
    center: { flex: 1, justifyContent: 'center', padding: 24 },
    card: { paddingVertical: 28, paddingHorizontal: 24, borderRadius: 16, backgroundColor: th.surface },
    title: { color: th.text, fontSize: 26, lineHeight: 32, fontWeight: '700', marginBottom: 12 },
    muted: { color: th.text2, fontSize: 15, lineHeight: 22 },
    error: { color: th.danger, fontSize: 15 },
    report: { maxHeight: 260, marginTop: 12, padding: 12, borderRadius: 8, backgroundColor: th.surface2 },
    reportText: { color: th.text, fontSize: 12 },
    btn: { minHeight: 48, justifyContent: 'center', alignItems: 'center', borderRadius: 24, backgroundColor: th.accent, marginTop: 24, paddingHorizontal: 24 },
    btnText: { color: th.onAccent, fontSize: 16, fontWeight: '600' },
    header: { color: th.text, fontSize: 26, lineHeight: 32, fontWeight: '700', paddingHorizontal: 16, paddingTop: 16, paddingBottom: 8 },
    nav: { flexDirection: 'row', backgroundColor: th.surface, borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: th.line },
    slot: { flex: 1, flexDirection: 'row', minHeight: 68 },
    navItem: { flex: 1, alignItems: 'center', paddingTop: 10, paddingBottom: 10, minHeight: 68 },
    navPill: { width: 56, height: 30, borderRadius: 15, justifyContent: 'center', alignItems: 'center' },
    navPillOn: { backgroundColor: th.accentSoft },
    navLabel: { color: th.text2, fontSize: 12, lineHeight: 16, marginTop: 4 },
    navLabelOn: { color: th.text, fontWeight: '700' },
    toast: {
      position: 'absolute', left: 12, right: 12, bottom: 12, flexDirection: 'row', alignItems: 'center', minHeight: 48,
      paddingLeft: 16, borderRadius: 10, backgroundColor: th.dark ? '#EDEBE6' : '#2A2A27', elevation: 6,
    },
    toastText: { flex: 1, color: th.dark ? '#1A1A18' : '#F4F2ED', fontSize: 14, lineHeight: 20, paddingVertical: 12 },
    toastBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 16 },
    link: { color: th.dark ? '#1F6B5C' : '#8FD8C5', fontSize: 15, fontWeight: '700' },
  }));
