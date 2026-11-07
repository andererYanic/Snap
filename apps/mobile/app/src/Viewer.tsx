// Eingebaute Anzeige für Bilder und Videos.
// - Bilder: Zoomen mit zwei Fingern, Doppeltippen, im Zoom verschieben.
// - Wischen nach links/rechts: nächste/vorige Datei im Ordner bzw. in der Zeitachse. Andere
//   Dateien (PDF, Text …) erscheinen als Karte mit „Öffnen mit …“.
// - Nach unten wischen schließt; Videos laufen über Media3/ExoPlayer (lokal, ohne Google-Dienste).
import { useCallback, useEffect, useRef, useState } from 'react';
import { Dimensions, FlatList, Image, Modal, Pressable, StatusBar, StyleSheet, Text, View } from 'react-native';
import { Gesture, GestureDetector, GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, {
  FadeIn, FadeOut, runOnJS, useAnimatedStyle, useSharedValue, withTiming,
} from 'react-native-reanimated';
import Video from 'react-native-video';
import * as core from './core';
import { t } from './i18n';
import { isMedia, isVideo, useThumb } from './thumbs';
import { EASE_OUT } from './motion';

/** Weiche, klare Bewegung ohne Nachfedern (statt Feder). */
const ease = (v: number) => {
  'worklet';
  return withTiming(v, { duration: 220, easing: EASE_OUT });
};
import { m } from './motion';

export interface MediaItem {
  path: string;
  name: string;
}

const { width: W, height: H } = Dimensions.get('window');

export default function Viewer({ items, start, onClose }: { items: MediaItem[]; start: number; onClose: () => void }) {
  const [index, setIndex] = useState(start);
  const [chrome, setChrome] = useState(true);
  const [zoomed, setZoomed] = useState(false);
  const list = useRef<FlatList<MediaItem>>(null);
  const item = items[index];

  // Nach unten wischen zum Schließen (nur ohne Zoom)
  const dragY = useSharedValue(0);
  const dismiss = Gesture.Pan()
    .enabled(!zoomed)
    .activeOffsetY([-20, 20])
    .failOffsetX([-20, 20])
    .onUpdate((e) => {
      dragY.value = Math.max(0, e.translationY);
    })
    .onEnd((e) => {
      if (e.translationY > 140 || e.velocityY > 1200) {
        dragY.value = withTiming(H, { duration: 180 }, () => runOnJS(onClose)());
      } else {
        dragY.value = ease(0);
      }
    });
  const pageStyle = useAnimatedStyle(() => ({
    transform: [{ translateY: dragY.value }, { scale: 1 - Math.min(dragY.value / H, 0.3) * 0.4 }],
  }));
  const backdrop = useAnimatedStyle(() => ({ opacity: 1 - Math.min(dragY.value / (H * 0.6), 1) }));

  return (
    <Modal visible transparent statusBarTranslucent animationType="fade" onRequestClose={onClose}>
      <GestureHandlerRootView style={styles.fill}>
        <StatusBar hidden={!chrome} barStyle="light-content" />
        <Animated.View style={[StyleSheet.absoluteFill, styles.black, backdrop]} />
        <GestureDetector gesture={dismiss}>
          <Animated.View style={[styles.fill, pageStyle]}>
            <FlatList
              ref={list}
              data={items}
              horizontal
              pagingEnabled
              scrollEnabled={!zoomed}
              initialScrollIndex={start}
              getItemLayout={(_, i) => ({ length: W, offset: W * i, index: i })}
              keyExtractor={(m) => m.path}
              showsHorizontalScrollIndicator={false}
              windowSize={3}
              onMomentumScrollEnd={(e) => setIndex(Math.round(e.nativeEvent.contentOffset.x / W))}
              renderItem={({ item: m, index: i }) =>
                !isMedia(m.name) ? (
                  <FilePage item={m} />
                ) : isVideo(m.name) ? (
                  <VideoPage item={m} active={i === index} />
                ) : (
                  <ZoomImage item={m} onZoom={setZoomed} onTap={() => setChrome((c) => !c)} />
                )
              }
            />
          </Animated.View>
        </GestureDetector>
        {chrome && item && (
          <Animated.View entering={m(FadeIn.duration(150))} exiting={m(FadeOut.duration(150))} style={styles.top}>
            <Pressable onPress={onClose} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Schließen')}>
              <Text style={styles.btnText}>✕</Text>
            </Pressable>
            <View style={styles.titleBox}>
              <Text style={styles.title} numberOfLines={1}>{item.name}</Text>
              <Text style={styles.counter}>{`${index + 1} / ${items.length}`}</Text>
            </View>
            <Pressable onPress={() => core.shareFiles([item.path]).catch(() => {})} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Teilen')}>
              <Text style={styles.btnSmall}>{t('Teilen')}</Text>
            </Pressable>
            <Pressable onPress={() => core.openFile(item.path).catch(() => {})} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Öffnen mit …')}>
              <Text style={styles.btnSmall}>⋯</Text>
            </Pressable>
          </Animated.View>
        )}
      </GestureHandlerRootView>
    </Modal>
  );
}

/** Bild mit Zoom (zwei Finger, Doppeltippen) und Verschieben im Zoom. */
function ZoomImage({ item, onZoom, onTap }: { item: MediaItem; onZoom: (z: boolean) => void; onTap: () => void }) {
  const preview = useThumb(item.path, 1024);
  const [loaded, setLoaded] = useState(false);
  const scale = useSharedValue(1);
  const saved = useSharedValue(1);
  const tx = useSharedValue(0);
  const ty = useSharedValue(0);
  const sx = useSharedValue(0);
  const sy = useSharedValue(0);

  const [isZoomed, setIsZoomed] = useState(false);
  const report = useCallback(
    (z: boolean) => {
      setIsZoomed(z);
      onZoom(z);
    },
    [onZoom],
  );

  const pinch = Gesture.Pinch()
    .onUpdate((e) => {
      scale.value = Math.min(6, Math.max(1, saved.value * e.scale));
    })
    .onEnd(() => {
      saved.value = scale.value;
      if (scale.value <= 1.01) {
        tx.value = ease(0);
        ty.value = ease(0);
        sx.value = 0;
        sy.value = 0;
      }
      runOnJS(report)(scale.value > 1.01);
    });
  // Verschieben nur im Zoom: Ohne Zoom würde die Geste das Wischen zur nächsten Datei schlucken.
  const pan = Gesture.Pan()
    .withTestId('viewer-pan')
    .enabled(isZoomed)
    .averageTouches(true)
    .onUpdate((e) => {
      if (scale.value > 1.01) {
        const maxX = (W * (scale.value - 1)) / 2;
        const maxY = (H * (scale.value - 1)) / 2;
        tx.value = Math.min(maxX, Math.max(-maxX, sx.value + e.translationX));
        ty.value = Math.min(maxY, Math.max(-maxY, sy.value + e.translationY));
      }
    })
    .onEnd(() => {
      sx.value = tx.value;
      sy.value = ty.value;
    });
  const doubleTap = Gesture.Tap()
    .numberOfTaps(2)
    .onEnd((e) => {
      const zoomIn = scale.value <= 1.01;
      const target = zoomIn ? 2.5 : 1;
      scale.value = ease(target);
      saved.value = target;
      // Auf die getippte Stelle zoomen
      const nx = zoomIn ? (W / 2 - e.x) * (target - 1) : 0;
      const ny = zoomIn ? (H / 2 - e.y) * (target - 1) : 0;
      tx.value = ease(nx);
      ty.value = ease(ny);
      sx.value = nx;
      sy.value = ny;
      runOnJS(report)(zoomIn);
    });
  const singleTap = Gesture.Tap().requireExternalGestureToFail(doubleTap).onEnd(() => runOnJS(onTap)());
  const gesture = Gesture.Simultaneous(pinch, pan, Gesture.Exclusive(doubleTap, singleTap));

  const style = useAnimatedStyle(() => ({
    transform: [{ translateX: tx.value }, { translateY: ty.value }, { scale: scale.value }],
  }));

  return (
    <GestureDetector gesture={gesture}>
      <Animated.View style={[styles.page, style]}>
        {preview && !loaded && <Image source={{ uri: preview }} style={styles.media} resizeMode="contain" fadeDuration={0} />}
        <Image
          source={{ uri: `file://${item.path}` }}
          style={[styles.media, !loaded && styles.hidden]}
          resizeMode="contain"
          onLoad={() => setLoaded(true)}
          fadeDuration={150}
        />
      </Animated.View>
    </GestureDetector>
  );
}

/** Karte für Dateien, die die Anzeige nicht selbst darstellt. */
function FilePage({ item }: { item: MediaItem }) {
  const dot = item.name.lastIndexOf('.');
  const ext = dot > 0 ? item.name.slice(dot + 1).toUpperCase().slice(0, 5) : '';
  return (
    <View style={styles.page}>
      <View style={styles.fileBadge}>
        <Text style={styles.fileExt}>{ext || '…'}</Text>
      </View>
      <Text style={styles.fileName} numberOfLines={2}>{item.name}</Text>
      <Pressable onPress={() => core.openFile(item.path).catch(() => {})} style={styles.errorBtn} accessibilityRole="button">
        <Text style={styles.btnSmall}>{t('Öffnen mit …')}</Text>
      </Pressable>
    </View>
  );
}

function VideoPage({ item, active }: { item: MediaItem; active: boolean }) {
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setError(null), [item.path]);
  return (
    <View style={styles.page}>
      {error ? (
        <View style={styles.errorBox}>
          <Text style={styles.errorText}>{t('Dieses Video lässt sich nicht abspielen.')}</Text>
          <Pressable onPress={() => core.openFile(item.path).catch(() => {})} style={styles.errorBtn}>
            <Text style={styles.btnSmall}>{t('Mit anderer App öffnen')}</Text>
          </Pressable>
        </View>
      ) : (
        <Video
          source={{ uri: `file://${item.path}` }}
          style={styles.media}
          resizeMode="contain"
          paused={!active}
          controls
          onError={(e: any) => setError(String(e?.error?.errorString ?? 'error'))}
        />
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  fill: { flex: 1 },
  black: { backgroundColor: '#000' },
  page: { width: W, height: H, justifyContent: 'center', alignItems: 'center' },
  media: { width: W, height: H, position: 'absolute' },
  hidden: { opacity: 0 },
  top: {
    position: 'absolute', top: 0, left: 0, right: 0, paddingTop: 36, paddingHorizontal: 8, paddingBottom: 8,
    flexDirection: 'row', alignItems: 'center', backgroundColor: 'rgba(0,0,0,0.45)',
  },
  btn: { minWidth: 48, height: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 8 },
  btnText: { color: '#fff', fontSize: 22 },
  btnSmall: { color: '#fff', fontSize: 15, fontWeight: '600' },
  titleBox: { flex: 1, marginHorizontal: 4 },
  title: { color: '#fff', fontSize: 15, fontWeight: '600' },
  counter: { color: '#bbb', fontSize: 12 },
  fileBadge: { width: 120, height: 120, borderRadius: 28, backgroundColor: 'rgba(255,255,255,0.12)', justifyContent: 'center', alignItems: 'center', marginBottom: 18 },
  fileExt: { color: '#fff', fontSize: 26, fontWeight: '700' },
  fileName: { color: '#fff', fontSize: 17, fontWeight: '600', textAlign: 'center', marginHorizontal: 32, marginBottom: 18 },
  errorBox: { alignItems: 'center', padding: 24 },
  errorText: { color: '#eee', fontSize: 15, textAlign: 'center', marginBottom: 12 },
  errorBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: 'rgba(255,255,255,0.15)' },
});
