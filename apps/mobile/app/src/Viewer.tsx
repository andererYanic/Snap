// Eingebaute Anzeige für Bilder und Videos.
// - Bilder: Zoomen mit zwei Fingern live um den Punkt zwischen den Fingern (gleichzeitig
//   verschieben), Gummiband an den Grenzen, Doppeltippen zoomt auf die Stelle, Verschieben mit
//   Schwung. Rechnung: `zoom.ts`.
// - Wischen nach links/rechts: nächste/vorige Datei (eigenes Blättern auf dem UI-Thread, damit
//   Zoom und Blättern sich nicht gegenseitig die Finger wegnehmen). Andere Dateien (PDF, Text …)
//   erscheinen als Karte mit „Öffnen mit …“.
// - Nach unten wischen schließt; Videos laufen über Media3/ExoPlayer (lokal, ohne Google-Dienste).
import { useCallback, useEffect, useRef, useState } from 'react';
import { Dimensions, Image, Modal, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import { Text } from './text';
import { Gesture, GestureDetector, GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, {
  cancelAnimation, FadeIn, FadeOut, runOnJS, useAnimatedStyle, useSharedValue, withDecay, withTiming,
  type SharedValue,
} from 'react-native-reanimated';
import Video from 'react-native-video';
import * as core from './core';
import { t } from './i18n';
import { isAudio, isMedia, isVideo, useThumb } from './thumbs';
import { m } from './motion';
import { Icon } from './Icon';
import { clampV, pinchBase, pinchStep, range, rubber, settle, settleTarget, type PinchState } from './zoom';

export interface MediaItem {
  path: string;
  name: string;
  /** Statt `file://path` (z. B. gestreamtes Video eines anderen Geräts). */
  uri?: string;
}

const { width: W, height: H } = Dimensions.get('window');
export const IMAGE_MAX_ZOOM = 6;
export const DOUBLE_TAP_ZOOM = 2.5;
/** Ab hier gilt ein Bild als gezoomt (Blättern aus, Verschieben an). */
const ZOOMED = 1.01;

type Box = { x: number; y: number; w: number; h: number };
const FULL: Box = { x: 0, y: 0, w: W, h: H };

/** Fläche, die ein Bild mit `contain` auf dem Bildschirm einnimmt. */
export function containBox(iw: number, ih: number): Box {
  if (!(iw > 0 && ih > 0)) {
    return FULL;
  }
  const k = Math.min(W / iw, H / ih);
  const w = iw * k;
  const h = ih * k;
  return { x: (W - w) / 2, y: (H - h) / 2, w, h };
}

export default function Viewer({ items, start, onClose, onEdit }: {
  items: MediaItem[]; start: number; onClose: () => void;
  /** Bild bearbeiten (eingebauter Editor); fehlt er, gibt es keinen Knopf. */
  onEdit?: (item: MediaItem) => void;
}) {
  const [index, setIndexState] = useState(start);
  const [chrome, setChrome] = useState(true);
  const item = items[index];
  const image = !!item && isMedia(item.name) && !isVideo(item.name);
  const boxes = useRef(new Map<string, Box>());

  // Zoom des aktuellen Bilds
  const s = useSharedValue(1);
  const tx = useSharedValue(0);
  const ty = useSharedValue(0);
  const box = useSharedValue<Box>(FULL);
  // Blättern und Schließen
  const base = useSharedValue(-start * W);
  const drag = useSharedValue(0);
  const dragY = useSharedValue(0);
  const idx = useSharedValue(start);
  const count = useSharedValue(items.length);
  // Geste: 0 offen, 1 blättern, 2 schließen, 3 Bild verschieben, 4 nichts
  const mode = useSharedValue(0);
  const start0 = useSharedValue({ s: 1, x: 0, y: 0, px: 0, py: 0 });
  const pz = useSharedValue<PinchState>({ s: 1, px: 0, py: 0, base: 1, n: 2, fx: 0, fy: 0 });
  // Während eines Zooms ruht das Verschieben (kein Mitlaufen, kein Schwung danach)
  const pinching = useSharedValue(false);
  const pinchedInPan = useSharedValue(false);

  useEffect(() => {
    count.value = items.length;
  }, [items.length, count]);

  const setIndex = useCallback((n: number) => {
    setIndexState(n);
    const it = items[n];
    box.value = (it && boxes.current.get(it.path)) ?? FULL;
  }, [items, box]);

  const onBox = useCallback((path: string, b: Box) => {
    boxes.current.set(path, b);
    if (items[index]?.path === path) {
      box.value = b;
    }
  }, [items, index, box]);

  const toggleChrome = useCallback(() => setChrome((c) => !c), []);

  const resetZoom = () => {
    'worklet';
    s.value = settle(1);
    tx.value = settle(0);
    ty.value = settle(0);
  };

  const pinch = Gesture.Pinch()
    .withTestId('viewer-pinch')
    .enabled(image)
    .onStart((e) => {
      cancelAnimation(tx);
      cancelAnimation(ty);
      cancelAnimation(s);
      pinching.value = true;
      pinchedInPan.value = true;
      pz.value = pinchBase(s.value, tx.value, ty.value, e.focalX, e.focalY, e.scale, e.numberOfPointers);
      mode.value = 4;
      drag.value = settle(0);
    })
    .onUpdate((e) => {
      const r = pinchStep(pz.value, { s: s.value, x: tx.value, y: ty.value }, e.scale, e.focalX, e.focalY, e.numberOfPointers, 1, IMAGE_MAX_ZOOM);
      pz.value = r.st;
      s.value = r.s;
      tx.value = r.x;
      ty.value = r.y;
    })
    .onEnd(() => {
      // Letzter gültiger Fingerpunkt (nicht der Punkt beim Abheben des ersten Fingers)
      const b = box.value;
      const st = pz.value;
      const to = settleTarget(s.value, tx.value, ty.value, st.fx, st.fy, 1, IMAGE_MAX_ZOOM, W, H, b.x, b.y, b.w, b.h);
      s.value = settle(to.s);
      tx.value = settle(to.x);
      ty.value = settle(to.y);
    })
    .onFinalize(() => {
      pinching.value = false;
    });

  const pan = Gesture.Pan()
    .withTestId('viewer-pan')
    .maxPointers(1)
    .onBegin(() => {
      cancelAnimation(tx);
      cancelAnimation(ty);
      start0.value = { s: s.value, x: tx.value, y: ty.value, px: 0, py: 0 };
      mode.value = s.value > ZOOMED ? 3 : 0;
      pinchedInPan.value = false;
    })
    .onUpdate((e) => {
      if (pinching.value || pinchedInPan.value) {
        return;
      }
      if (mode.value === 0) {
        if (Math.abs(e.translationX) < 10 && Math.abs(e.translationY) < 10) {
          return;
        }
        mode.value = Math.abs(e.translationX) > Math.abs(e.translationY) ? 1 : e.translationY > 0 ? 2 : 4;
      }
      if (mode.value === 3) {
        const b = box.value;
        const sc = s.value;
        const [x0, x1] = range(sc, W, b.x, b.w);
        const [y0, y1] = range(sc, H, b.y, b.h);
        tx.value = rubber(start0.value.x + e.translationX, x0, x1);
        ty.value = rubber(start0.value.y + e.translationY, y0, y1);
      } else if (mode.value === 1) {
        const atStart = idx.value === 0 && e.translationX > 0;
        const atEnd = idx.value === count.value - 1 && e.translationX < 0;
        drag.value = atStart || atEnd ? e.translationX * 0.3 : e.translationX;
      } else if (mode.value === 2) {
        dragY.value = Math.max(0, e.translationY);
      }
    })
    .onEnd((e) => {
      if (pinchedInPan.value) {
        return; // nach einem Zoom kein Schwung
      }
      if (mode.value === 3) {
        const b = box.value;
        const sc = s.value;
        const [x0, x1] = range(sc, W, b.x, b.w);
        const [y0, y1] = range(sc, H, b.y, b.h);
        tx.value = withDecay({ velocity: e.velocityX, clamp: [x0, x1], rubberBandEffect: true });
        ty.value = withDecay({ velocity: e.velocityY, clamp: [y0, y1], rubberBandEffect: true });
      } else if (mode.value === 1) {
        let n = idx.value;
        if ((e.translationX < -W * 0.22 || e.velocityX < -700) && n < count.value - 1) {
          n += 1;
        } else if ((e.translationX > W * 0.22 || e.velocityX > 700) && n > 0) {
          n -= 1;
        }
        base.value = base.value + drag.value;
        drag.value = 0;
        if (n !== idx.value) {
          idx.value = n;
          s.value = 1;
          tx.value = 0;
          ty.value = 0;
          runOnJS(setIndex)(n);
        }
        base.value = withTiming(-n * W, { duration: 240 });
      } else if (mode.value === 2) {
        if (e.translationY > 140 || e.velocityY > 1200) {
          dragY.value = withTiming(H, { duration: 180 }, () => runOnJS(onClose)());
        } else {
          dragY.value = settle(0);
        }
      }
    })
    .onFinalize((_, ok) => {
      // Abgebrochen (z. B. zweiter Finger): Blättern und Schließen zurücksetzen
      if (!ok) {
        if (mode.value === 1) {
          base.value = withTiming(-idx.value * W, { duration: 200 });
          drag.value = 0;
        }
        if (mode.value === 2) {
          dragY.value = settle(0);
        }
      }
    });

  const doubleTap = Gesture.Tap()
    .numberOfTaps(2)
    .enabled(image)
    .onEnd((e) => {
      if (s.value > ZOOMED) {
        resetZoom();
        return;
      }
      const b = box.value;
      const ns = DOUBLE_TAP_ZOOM;
      const [x0, x1] = range(ns, W, b.x, b.w);
      const [y0, y1] = range(ns, H, b.y, b.h);
      s.value = settle(ns);
      tx.value = settle(clampV(e.x - e.x * ns, x0, x1));
      ty.value = settle(clampV(e.y - e.y * ns, y0, y1));
    });
  const singleTap = Gesture.Tap()
    .enabled(image)
    .requireExternalGestureToFail(doubleTap)
    .onEnd(() => runOnJS(toggleChrome)());
  const gesture = Gesture.Simultaneous(pinch, pan, Gesture.Exclusive(doubleTap, singleTap));

  const rowStyle = useAnimatedStyle(() => ({ transform: [{ translateX: base.value + drag.value }] }));
  const pageStyle = useAnimatedStyle(() => ({
    transform: [{ translateY: dragY.value }, { scale: 1 - Math.min(dragY.value / H, 0.3) * 0.4 }],
  }));
  const backdrop = useAnimatedStyle(() => ({ opacity: 1 - Math.min(dragY.value / (H * 0.6), 1) }));

  // Nur die aktuelle und die Nachbarseiten zeichnen
  const window = [index - 1, index, index + 1].filter((i) => i >= 0 && i < items.length);

  return (
    <Modal visible transparent statusBarTranslucent animationType="fade" onRequestClose={onClose}>
      <GestureHandlerRootView style={styles.fill}>
        <StatusBar hidden={!chrome} barStyle="light-content" />
        <Animated.View style={[StyleSheet.absoluteFill, styles.black, backdrop]} />
        <GestureDetector gesture={gesture}>
          <Animated.View style={[styles.fill, pageStyle]} collapsable={false}>
            <Animated.View style={[styles.row, { width: Math.max(1, items.length) * W }, rowStyle]}>
              {window.map((i) => {
                const it = items[i];
                return (
                  <View key={it.path} style={[styles.slot, { left: i * W }]}>
                    {isAudio(it.name) ? (
                      <AudioPage item={it} active={i === index} />
                    ) : !isMedia(it.name) ? (
                      <FilePage item={it} />
                    ) : isVideo(it.name) ? (
                      <VideoPage item={it} active={i === index} />
                    ) : (
                      <ZoomImage item={it} active={i === index} s={s} tx={tx} ty={ty} onBox={onBox} />
                    )}
                  </View>
                );
              })}
            </Animated.View>
          </Animated.View>
        </GestureDetector>
        {chrome && item && (
          <Animated.View entering={m(FadeIn.duration(150))} exiting={m(FadeOut.duration(150))} style={styles.top}>
            <Pressable onPress={onClose} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Schließen')}>
              <Icon name="back" color="#fff" />
            </Pressable>
            <View style={styles.titleBox}>
              <Text style={styles.title} numberOfLines={1}>{item.name}</Text>
              <Text style={styles.counter}>{`${index + 1} / ${items.length}`}</Text>
            </View>
            {onEdit && !item.uri && (
              <Pressable onPress={() => onEdit(item)} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Bearbeiten')}>
                <Icon name="edit" size={22} color="#fff" />
              </Pressable>
            )}
            {!item.uri && (
              <Pressable onPress={() => core.shareFiles([item.path]).catch(() => {})} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Teilen')}>
                <Icon name="share" size={22} color="#fff" />
              </Pressable>
            )}
            <Pressable onPress={() => core.openFile(item.path).catch(() => {})} style={styles.btn} android_ripple={{ color: 'rgba(255,255,255,0.2)', borderless: true }} accessibilityLabel={t('Öffnen mit …')}>
              <Icon name="more" size={22} color="#fff" strokeWidth={2.4} />
            </Pressable>
          </Animated.View>
        )}
      </GestureHandlerRootView>
    </Modal>
  );
}

/** Bild; das aktuelle folgt dem Zoom (gemeinsame Werte der Anzeige), die Nachbarn stehen still. */
function ZoomImage({ item, active, s, tx, ty, onBox }: {
  item: MediaItem; active: boolean; s: SharedValue<number>; tx: SharedValue<number>; ty: SharedValue<number>;
  onBox: (path: string, b: Box) => void;
}) {
  const preview = useThumb(item.path, 1024);
  const [loaded, setLoaded] = useState(false);
  const style = useAnimatedStyle(() =>
    active
      ? { transform: [{ translateX: tx.value }, { translateY: ty.value }, { scale: s.value }] }
      : { transform: [{ translateX: 0 }, { translateY: 0 }, { scale: 1 }] },
  );
  return (
    <Animated.View style={[styles.page, styles.origin, style]}>
      {preview && !loaded && <Image source={{ uri: preview }} style={styles.media} resizeMode="contain" fadeDuration={0} />}
      <Image
        source={{ uri: `file://${item.path}` }}
        style={[styles.media, !loaded && styles.hidden]}
        resizeMode="contain"
        onLoad={(e: any) => {
          setLoaded(true);
          const src = e?.nativeEvent?.source;
          if (src?.width && src?.height) {
            onBox(item.path, containBox(src.width, src.height));
          }
        }}
        fadeDuration={150}
      />
    </Animated.View>
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

const fmtTime = (s: number) => {
  const t0 = Math.max(0, Math.floor(s));
  const h = Math.floor(t0 / 3600);
  const m = Math.floor((t0 % 3600) / 60);
  const sec = String(t0 % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
};

/** Audio: Cover (falls eingebettet), Titel/Interpret, großer Abspielknopf, Zeitleiste zum Springen. */
function AudioPage({ item, active }: { item: MediaItem; active: boolean }) {
  const cover = useThumb(item.uri ? null : item.path, 720);
  const player = useRef<any>(null);
  const [playing, setPlaying] = useState(false);
  const [pos, setPos] = useState(0);
  const [dur, setDur] = useState(0);
  const [meta, setMeta] = useState<{ title?: string | null; artist?: string | null; album?: string | null }>({});
  const [error, setError] = useState(false);
  const trackW = useRef(1);
  useEffect(() => {
    if (!active) {
      setPlaying(false);
    }
  }, [active]);
  useEffect(() => {
    if (!item.uri) {
      core.request<any>({ Metadata: { path: item.path } }).then((m) => setMeta(m ?? {})).catch(() => {});
    }
  }, [item.path, item.uri]);
  const seekTo = (x: number) => {
    if (dur > 0) {
      const t0 = Math.max(0, Math.min(1, x / trackW.current)) * dur;
      player.current?.seek(t0);
      setPos(t0);
    }
  };
  const scrub = Gesture.Pan().minDistance(0).onBegin((e) => runOnJS(seekTo)(e.x)).onUpdate((e) => runOnJS(seekTo)(e.x));
  const title = meta.title || item.name.replace(/\.[^.]+$/, '');
  const sub = [meta.artist, meta.album].filter(Boolean).join(' · ');
  return (
    <View style={styles.page}>
      <Video
        ref={player}
        source={{ uri: item.uri ?? `file://${item.path}` }}
        paused={!playing || !active}
        playInBackground={false}
        onLoad={(e: any) => setDur(e?.duration ?? 0)}
        onProgress={(e: any) => setPos(e?.currentTime ?? 0)}
        onEnd={() => { setPlaying(false); setPos(0); player.current?.seek(0); }}
        onError={() => setError(true)}
        style={styles.hiddenPlayer}
      />
      <View style={styles.cover}>
        {cover ? <Image source={{ uri: cover }} style={styles.coverImg} /> : <Icon name="music" size={96} color="rgba(255,255,255,0.55)" strokeWidth={1.2} />}
      </View>
      <Text style={styles.audioTitle} numberOfLines={2}>{title}</Text>
      {sub ? <Text style={styles.audioSub} numberOfLines={1}>{sub}</Text> : null}
      {error ? (
        <Text style={styles.errorText}>{t('Diese Audiodatei lässt sich nicht abspielen.')}</Text>
      ) : (
        <>
          <GestureDetector gesture={scrub}>
            <View style={styles.track} onLayout={(e) => (trackW.current = e.nativeEvent.layout.width || 1)} accessibilityRole="adjustable" accessibilityLabel={t('Position')}>
              <View style={styles.trackBg} />
              <View style={[styles.trackOn, { width: `${dur > 0 ? Math.min(100, (pos / dur) * 100) : 0}%` }]} />
              <View style={[styles.knob, { left: `${dur > 0 ? Math.min(100, (pos / dur) * 100) : 0}%` }]} />
            </View>
          </GestureDetector>
          <View style={styles.times}>
            <Text style={styles.time}>{fmtTime(pos)}</Text>
            <Text style={styles.time}>{dur > 0 ? fmtTime(dur) : '–:––'}</Text>
          </View>
          <Pressable onPress={() => setPlaying((p) => !p)} style={styles.playBtn} accessibilityRole="button" accessibilityLabel={playing ? t('Pause') : t('Abspielen')}>
            <Icon name={playing ? 'pause' : 'play'} size={34} color="#0B1F1A" fill={playing ? undefined : '#0B1F1A'} strokeWidth={playing ? 3 : 1.6} />
          </Pressable>
        </>
      )}
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
          source={{ uri: item.uri ?? `file://${item.path}` }}
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
  row: { width: W, height: H },
  slot: { position: 'absolute', top: 0, width: W, height: H },
  origin: { transformOrigin: 'left top' },
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
  hiddenPlayer: { width: 0, height: 0, position: 'absolute' },
  cover: { width: W * 0.68, height: W * 0.68, borderRadius: 18, backgroundColor: 'rgba(255,255,255,0.08)', justifyContent: 'center', alignItems: 'center', overflow: 'hidden', marginBottom: 28 },
  coverImg: { width: '100%', height: '100%' },
  audioTitle: { color: '#fff', fontSize: 20, lineHeight: 26, fontWeight: '700', textAlign: 'center', marginHorizontal: 32 },
  audioSub: { color: '#bbb', fontSize: 14, marginTop: 6, marginHorizontal: 32, textAlign: 'center' },
  track: { width: W - 64, height: 32, justifyContent: 'center', marginTop: 28 },
  trackBg: { position: 'absolute', left: 0, right: 0, height: 4, borderRadius: 2, backgroundColor: 'rgba(255,255,255,0.22)' },
  trackOn: { position: 'absolute', left: 0, height: 4, borderRadius: 2, backgroundColor: '#72C7B2' },
  knob: { position: 'absolute', width: 14, height: 14, borderRadius: 7, marginLeft: -7, backgroundColor: '#fff' },
  times: { width: W - 64, flexDirection: 'row', justifyContent: 'space-between' },
  time: { color: '#bbb', fontSize: 12, fontVariant: ['tabular-nums'] },
  playBtn: { width: 72, height: 72, borderRadius: 36, backgroundColor: '#72C7B2', justifyContent: 'center', alignItems: 'center', marginTop: 20 },
  errorBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: 'rgba(255,255,255,0.15)' },
});
