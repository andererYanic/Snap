// Eingebaute PDF-Anzeige (wie bei Google Drive, aber lokal): Android zeichnet die Seiten selbst
// (`PdfRenderer`, Teil des Systems). Seiten erscheinen beim Scrollen. Zoomen mit zwei Fingern um
// den Punkt zwischen ihnen (live per Transformation, danach werden die Seiten in der neuen Größe
// scharf neu gezeichnet), Doppeltippen wechselt zwischen 100 % und 250 %. Gezoomt lässt sich in
// beide Richtungen scrollen.
import { memo, useCallback, useEffect, useRef, useState } from 'react';
import { ActivityIndicator, Dimensions, Image, Modal, PixelRatio, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import { Text } from './text';
import { FlatList, Gesture, GestureDetector, GestureHandlerRootView, ScrollView } from 'react-native-gesture-handler';
import Animated, { runOnJS, useAnimatedStyle, useSharedValue } from 'react-native-reanimated';
import * as core from './core';
import { t } from './i18n';
import { fadeIn, m } from './motion';

const { width: W } = Dimensions.get('window');
const GAP = 8;
const PAD = 8;
const PAGE_W = W - 2 * PAD;
export const MIN_ZOOM = 1;
export const MAX_ZOOM = 5;

const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

/** Auflösungsstufe für eine Zoomstufe (nicht bei jedem Zwischenwert neu zeichnen). */
export function renderStep(scale: number): number {
  return [1, 1.5, 2, 3, 4, 5].find((st) => st >= scale - 0.01) ?? 5;
}

/**
 * Neue Scrollposition, damit der Punkt unter den Fingern (`focal`, im Ausschnitt) beim Zoomen um
 * den Faktor `k` an derselben Stelle bleibt.
 */
export function keepFocal(offset: number, focal: number, k: number): number {
  return Math.max(0, (offset + focal) * k - focal);
}

const PdfPage = memo(function PdfPage({ path, index, ratio, scale }: { path: string; index: number; ratio: number; scale: number }) {
  const [uri, setUri] = useState<string | null>(null);
  const step = renderStep(scale);
  useEffect(() => {
    let alive = true;
    const px = Math.min(4096, Math.round(PAGE_W * step * PixelRatio.get()));
    // Altes (unschärferes) Bild bleibt stehen, bis das neue fertig ist
    core.pdfPage(path, index, px).then((f) => alive && f && setUri(`file://${f}`)).catch(() => {});
    return () => {
      alive = false;
    };
  }, [path, index, step]);
  const w = PAGE_W * scale;
  return (
    <View style={[styles.page, { width: w, height: w * ratio }]} accessibilityLabel={t('Seite {n}', { n: index + 1 })}>
      {uri ? <Image source={{ uri }} style={styles.fill} fadeDuration={0} /> : <ActivityIndicator style={styles.fill} color="#8a8f9c" />}
    </View>
  );
});

export default function PdfViewer({ path, name, onClose }: { path: string; name: string; onClose: () => void }) {
  const [info, setInfo] = useState<{ pages: number; ratios: number[] } | null | 'error'>(null);
  const [page, setPage] = useState(0);
  const [scale, setScale] = useState(1);
  const [scrolling, setScrolling] = useState(true);
  const hScroll = useRef<any>(null);
  const vList = useRef<any>(null);
  const pos = useRef({ x: 0, y: 0 });
  // Live-Zoom während der Geste (Transformation um den Fingerpunkt), danach übernommen
  const live = useSharedValue(1);
  const ox = useSharedValue(0);
  const oy = useSharedValue(0);

  useEffect(() => {
    core.pdfInfo(path).then((i) => setInfo(i && i.pages > 0 ? i : 'error')).catch(() => setInfo('error'));
  }, [path]);

  /** Zoom übernehmen: neue Größe, Scrollposition so, dass der Fingerpunkt bleibt. */
  const commit = useCallback(
    (target: number, fx: number, fy: number) => {
      const next = clamp(target, MIN_ZOOM, MAX_ZOOM);
      const k = next / scale;
      const x = next === 1 ? 0 : keepFocal(pos.current.x, fx, k);
      const y = keepFocal(pos.current.y, fy, k);
      setScale(next);
      setScrolling(true);
      requestAnimationFrame(() => {
        hScroll.current?.scrollTo?.({ x, animated: false });
        vList.current?.scrollToOffset?.({ offset: y, animated: false });
        pos.current = { x, y };
        live.value = 1;
      });
    },
    [scale, live],
  );

  const pinch = Gesture.Pinch()
    .onStart((e) => {
      ox.value = e.focalX;
      oy.value = e.focalY;
      runOnJS(setScrolling)(false);
    })
    .onUpdate((e) => {
      live.value = clamp(e.scale, MIN_ZOOM / scale, MAX_ZOOM / scale);
    })
    .onEnd(() => {
      runOnJS(commit)(scale * live.value, ox.value, oy.value);
    });
  const doubleTap = Gesture.Tap()
    .numberOfTaps(2)
    .onEnd((e) => {
      runOnJS(commit)(scale > 1.2 ? 1 : 2.5, e.x, e.y);
    });
  const gestures = Gesture.Simultaneous(pinch, doubleTap);
  const liveStyle = useAnimatedStyle(() => ({
    transform: [{ translateX: ox.value }, { translateY: oy.value }, { scale: live.value }, { translateX: -ox.value }, { translateY: -oy.value }],
  }));

  const ok = info && info !== 'error' ? info : null;
  const pageW = PAGE_W * scale;
  const offsets = ok ? ok.ratios.reduce<number[]>((acc, r, i) => (acc.push(i === 0 ? 0 : acc[i - 1] + pageW * ok.ratios[i - 1] + GAP), acc), []) : [];

  return (
    <Modal visible transparent={false} animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <GestureHandlerRootView style={styles.root}>
        <StatusBar barStyle="light-content" />
        <View style={styles.top}>
          <Pressable onPress={onClose} style={styles.btn} accessibilityLabel={t('Schließen')}>
            <Text style={styles.btnText}>✕</Text>
          </Pressable>
          <View style={styles.titleBox}>
            <Text style={styles.title} numberOfLines={1}>{name}</Text>
            {ok && <Text style={styles.counter}>{t('Seite {n} von {total}', { n: page + 1, total: ok.pages })}{scale > 1.01 ? `  ·  ${Math.round(scale * 100)} %` : ''}</Text>}
          </View>
          <Pressable onPress={() => core.printFile(path).catch(() => {})} style={styles.btn} accessibilityLabel={t('Drucken')}>
            <Text style={styles.small}>{t('Drucken')}</Text>
          </Pressable>
          <Pressable onPress={() => core.openFile(path).catch(() => {})} style={styles.btn} accessibilityLabel={t('Öffnen mit …')}>
            <Text style={styles.btnText}>⋯</Text>
          </Pressable>
        </View>
        {info === null ? (
          <ActivityIndicator style={styles.fill} color="#fff" />
        ) : info === 'error' ? (
          <Animated.View entering={m(fadeIn)} style={styles.error}>
            <Text style={styles.errorText}>{t('Dieses PDF lässt sich nicht anzeigen.')}</Text>
            <Pressable onPress={() => core.openFile(path).catch(() => {})} style={styles.errorBtn}>
              <Text style={styles.small}>{t('Mit anderer App öffnen')}</Text>
            </Pressable>
          </Animated.View>
        ) : (
          <GestureDetector gesture={gestures}>
            <Animated.View style={[styles.fill, styles.clip]} accessibilityHint={t('Mit zwei Fingern zoomen, doppelt tippen für 250 %')}>
              <Animated.View style={[styles.fill, liveStyle]}>
                <ScrollView
                  ref={hScroll}
                  horizontal
                  scrollEnabled={scrolling && scale > 1.01}
                  showsHorizontalScrollIndicator={false}
                  onScroll={(e) => (pos.current.x = e.nativeEvent.contentOffset.x)}
                  scrollEventThrottle={32}
                  contentContainerStyle={{ width: pageW + 2 * PAD }}
                >
                  <FlatList
                    ref={vList}
                    style={{ width: pageW + 2 * PAD }}
                    data={Array.from({ length: info.pages }, (_, i) => i)}
                    keyExtractor={(i) => String(i)}
                    extraData={scale}
                    scrollEnabled={scrolling}
                    contentContainerStyle={styles.list}
                    getItemLayout={(_, i) => ({ length: pageW * info.ratios[i] + GAP, offset: offsets[i], index: i })}
                    initialNumToRender={3}
                    maxToRenderPerBatch={3}
                    windowSize={5}
                    removeClippedSubviews
                    onScroll={(e) => (pos.current.y = e.nativeEvent.contentOffset.y)}
                    scrollEventThrottle={32}
                    onViewableItemsChanged={({ viewableItems }) => viewableItems[0] && setPage(viewableItems[0].index ?? 0)}
                    viewabilityConfig={{ itemVisiblePercentThreshold: 40 }}
                    renderItem={({ item: i }) => <PdfPage path={path} index={i} ratio={info.ratios[i]} scale={scale} />}
                  />
                </ScrollView>
              </Animated.View>
            </Animated.View>
          </GestureDetector>
        )}
      </GestureHandlerRootView>
    </Modal>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#1c1d22' },
  fill: { flex: 1 },
  top: { flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 8, paddingBottom: 8, backgroundColor: '#25262c' },
  btn: { minWidth: 48, height: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 8 },
  btnText: { color: '#fff', fontSize: 22 },
  small: { color: '#fff', fontSize: 15, fontWeight: '600' },
  titleBox: { flex: 1, marginHorizontal: 4 },
  title: { color: '#fff', fontSize: 15, fontWeight: '600' },
  counter: { color: '#bbb', fontSize: 12 },
  list: { padding: PAD },
  clip: { overflow: 'hidden' },
  page: { marginBottom: GAP, backgroundColor: '#fff', borderRadius: 2, overflow: 'hidden' },
  error: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 24 },
  errorText: { color: '#eee', fontSize: 15, textAlign: 'center', marginBottom: 12 },
  errorBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: 'rgba(255,255,255,0.15)' },
});
