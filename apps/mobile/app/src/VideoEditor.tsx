// Eingebaute Videobearbeitung: Kürzen (Anfang/Ende auf der Zeitleiste), Ton entfernen, Drehen.
// Android exportiert mit Media3 Transformer (VideoEdit.kt), hier gibt es Vorschau und Zeitleiste.
// Speichern wie bei Bildern: Kopie (vorausgewählt) oder Original ersetzen (Papierkorb, rückgängig).
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Image, type LayoutChangeEvent, Modal, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import { Gesture, GestureDetector, GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, { runOnJS } from 'react-native-reanimated';
import Video, { type VideoRef } from 'react-native-video';
import * as core from './core';
import { request, type VideoInfo, type VideoParams } from './core';
import { t } from './i18n';
import { fadeIn, m, popIn } from './motion';
import { Text } from './text';

const FRAMES = 8;
const MIN_MS = 500; // kürzester Ausschnitt
const HANDLE = 22;

// Worklet: wird auch in Gesten auf dem UI-Thread aufgerufen (sonst Absturz „Remote Function“).
const clamp = (v: number, a: number, b: number) => {
  'worklet';
  return Math.max(a, Math.min(b, v));
};

/** 75500 → „1:15“ (bzw. „1:02:03“). */
export function clock(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(s / 3600);
  const mm = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, '0');
  return h > 0 ? `${h}:${String(mm).padStart(2, '0')}:${ss}` : `${mm}:${ss}`;
}

/** Fläche (w, h) des gedrehten Videos, eingepasst in die Bühne. */
export function fitVideo(info: { width: number; height: number; rotation: number }, rot: number, stage: { w: number; h: number }) {
  const turned = (info.rotation + rot) % 180 !== 0;
  const vw = turned ? info.height : info.width;
  const vh = turned ? info.width : info.height;
  if (!vw || !vh) {
    return { w: stage.w, h: stage.h };
  }
  const s = Math.min(stage.w / vw, stage.h / vh);
  return { w: vw * s, h: vh * s };
}

export default function VideoEditor({ path, name, onClose, notify }: {
  path: string;
  name: string;
  /** `changed` = es wurde gespeichert (Liste neu laden). */
  onClose: (changed: boolean) => void;
  notify: (text: string, undo?: () => void) => void;
}) {
  const [info, setInfo] = useState<VideoInfo | null>(null);
  const [p, setP] = useState<VideoParams>({ startMs: 0, endMs: 0, mute: false, rot: 0 });
  const [frames, setFrames] = useState<(string | null)[]>([]);
  const [stage, setStage] = useState({ w: 1, h: 1 });
  const [bar, setBar] = useState(1);
  const [paused, setPaused] = useState(true);
  const [pos, setPos] = useState(0);
  const [saving, setSaving] = useState<'ask' | 'busy' | null>(null);
  const [progress, setProgress] = useState(-1);
  const [error, setError] = useState<string | null>(null);
  const video = useRef<VideoRef>(null);
  const canReplace = /\.(mp4|m4v)$/i.test(name);

  useEffect(() => {
    let live = true;
    core
      .videoInfo(path)
      .then((i) => {
        if (live) {
          setInfo(i);
          setP((x) => ({ ...x, endMs: i.durationMs }));
        }
      })
      .catch((e) => live && setError(String(e?.message ?? e)));
    core
      .videoFrames(path, FRAMES, 160)
      .then((f) => live && setFrames(f))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [path]);

  // Während des Exports den Fortschritt abfragen
  useEffect(() => {
    if (saving !== 'busy') {
      return;
    }
    const timer = setInterval(() => core.videoProgress().then(setProgress).catch(() => {}), 400);
    return () => clearInterval(timer);
  }, [saving]);

  const dur = info?.durationMs ?? 0;
  const seek = useCallback((ms: number) => {
    video.current?.seek(ms / 1000);
    setPos(ms);
  }, []);

  // Zeitleiste: Griffe für Anfang und Ende
  const startRef = useRef(0);
  const drag = useCallback(
    (kind: 'start' | 'end', dx: number, begin: boolean) => {
      if (!dur) {
        return;
      }
      if (begin) {
        setPaused(true);
        startRef.current = kind === 'start' ? p.startMs : p.endMs;
        return;
      }
      const ms = startRef.current + (dx / bar) * dur;
      if (kind === 'start') {
        const v = Math.round(clamp(ms, 0, p.endMs - MIN_MS));
        setP((x) => ({ ...x, startMs: v }));
        seek(v);
      } else {
        const v = Math.round(clamp(ms, p.startMs + MIN_MS, dur));
        setP((x) => ({ ...x, endMs: v }));
        seek(v);
      }
    },
    [dur, bar, p.startMs, p.endMs, seek],
  );
  const pan = (kind: 'start' | 'end') =>
    Gesture.Pan()
      .minDistance(0)
      .onBegin(() => runOnJS(drag)(kind, 0, true))
      .onUpdate((e) => runOnJS(drag)(kind, e.translationX, false));

  const togglePlay = () => {
    if (paused && (pos < p.startMs || pos >= p.endMs - 100)) {
      seek(p.startMs);
    }
    setPaused(!paused);
  };
  // Nur den gewählten Ausschnitt abspielen
  const onProgress = (e: { currentTime: number }) => {
    const ms = e.currentTime * 1000;
    setPos(ms);
    if (!paused && ms >= p.endMs) {
      setPaused(true);
      seek(p.startMs);
    }
  };

  const box = useMemo(() => (info ? fitVideo(info, p.rot, stage) : null), [info, p.rot, stage]);
  const turned = p.rot % 180 !== 0;
  const changed = !!dur && (p.startMs > 0 || p.endMs < dur || p.mute || p.rot !== 0);

  const save = async (mode: 'copy' | 'replace') => {
    setPaused(true);
    setProgress(-1);
    setSaving('busy');
    try {
      if (mode === 'copy') {
        const out = await core.videoSave(path, p, 'copy');
        const outName = out.slice(out.lastIndexOf('/') + 1);
        onClose(true);
        notify(t('Gespeichert als „{name}“.', { name: outName }), () => {
          request({ FileOp: { op: { Trash: { paths: [out] } }, wait: true } }).catch(() => {});
        });
      } else {
        const tmp = await core.videoSave(path, p, 'temp');
        await request({ FileOp: { op: { Replace: { path, with: tmp } }, wait: true } });
        onClose(true);
        notify(t('Gespeichert. Das Original liegt im Papierkorb.'), () => {
          request('Undo').catch(() => {});
        });
      }
    } catch (e: any) {
      setSaving(null);
      const msg = String(e?.message ?? e);
      if (msg !== 'Abgebrochen.') {
        setError(msg);
      }
    }
  };
  const cancel = () => core.videoCancel();

  const onStage = (e: LayoutChangeEvent) => setStage({ w: e.nativeEvent.layout.width, h: e.nativeEvent.layout.height });
  const x = (ms: number) => (dur ? (ms / dur) * bar : 0);

  return (
    <Modal visible animationType="fade" onRequestClose={() => (saving === 'busy' ? cancel() : saving === 'ask' ? setSaving(null) : onClose(false))} statusBarTranslucent>
      <GestureHandlerRootView style={styles.root}>
        <StatusBar barStyle="light-content" />
        <View style={styles.top}>
          <Pressable onPress={() => onClose(false)} style={styles.btn} accessibilityLabel={t('Abbrechen')}>
            <Text style={styles.btnText}>✕</Text>
          </Pressable>
          <Text style={styles.title} numberOfLines={1}>{name}</Text>
          <Pressable onPress={() => setSaving('ask')} disabled={!changed || saving === 'busy'} style={[styles.btn, !changed && styles.dim]} accessibilityLabel={t('Speichern')}>
            <Text style={styles.save}>{t('Speichern')}</Text>
          </Pressable>
        </View>

        <View style={styles.stage} onLayout={onStage}>
          {info && box ? (
            <Pressable onPress={togglePlay} style={{ width: box.w, height: box.h, justifyContent: 'center', alignItems: 'center' }} accessibilityLabel={paused ? t('Abspielen') : t('Anhalten')}>
              <Video
                ref={video}
                source={{ uri: `file://${path}` }}
                style={{ width: turned ? box.h : box.w, height: turned ? box.w : box.h, transform: [{ rotate: `${p.rot}deg` }] }}
                resizeMode="contain"
                paused={paused}
                muted={p.mute}
                progressUpdateInterval={100}
                onProgress={onProgress}
                onError={() => setError(t('Dieses Video lässt sich nicht abspielen.'))}
              />
              {paused && (
                <View style={styles.play} pointerEvents="none">
                  <Text style={styles.playIcon}>▶</Text>
                </View>
              )}
            </Pressable>
          ) : error ? (
            <Text style={styles.error}>{error}</Text>
          ) : (
            <ActivityIndicator color="#fff" />
          )}
        </View>

        {error && info && <Text style={styles.errorLine}>{error}</Text>}

        <View style={styles.tools}>
          <Text style={styles.range}>{`${clock(p.startMs)} – ${clock(p.endMs)}  ·  ${clock(p.endMs - p.startMs)}`}</Text>
          <View style={styles.timeline} onLayout={(e) => setBar(Math.max(1, e.nativeEvent.layout.width))}>
            <View style={styles.strip}>
              {Array.from({ length: FRAMES }, (_, i) =>
                frames[i] ? <Image key={i} source={{ uri: `file://${frames[i]}` }} style={styles.frame} fadeDuration={0} /> : <View key={i} style={styles.frame} />,
              )}
            </View>
            <View pointerEvents="none" style={[styles.shade, { left: 0, width: x(p.startMs) }]} />
            <View pointerEvents="none" style={[styles.shade, { left: x(p.endMs), right: 0 }]} />
            <View pointerEvents="none" style={[styles.window, { left: x(p.startMs), width: Math.max(0, x(p.endMs) - x(p.startMs)) }]} />
            {pos >= p.startMs && pos <= p.endMs && <View pointerEvents="none" style={[styles.cursor, { left: x(pos) - 1 }]} />}
            <GestureDetector gesture={pan('start')}>
              <View style={[styles.handle, { left: x(p.startMs) - HANDLE }]} accessibilityLabel={t('Anfang')} accessibilityRole="adjustable">
                <View style={styles.grip} />
              </View>
            </GestureDetector>
            <GestureDetector gesture={pan('end')}>
              <View style={[styles.handle, { left: x(p.endMs) }]} accessibilityLabel={t('Ende')} accessibilityRole="adjustable">
                <View style={styles.grip} />
              </View>
            </GestureDetector>
          </View>
          <View style={styles.chips}>
            <Pressable onPress={() => setP((v) => ({ ...v, rot: (v.rot + 270) % 360 }))} style={styles.chip} accessibilityLabel={t('Drehen')}>
              <Text style={styles.chipText}>⟲ {t('Drehen')}</Text>
            </Pressable>
            <Pressable
              onPress={() => setP((v) => ({ ...v, mute: !v.mute }))}
              disabled={!info?.audio}
              style={[styles.chip, p.mute && styles.chipOn, !info?.audio && styles.dim]}
              accessibilityRole="switch"
              accessibilityState={{ checked: p.mute, disabled: !info?.audio }}
            >
              <Text style={[styles.chipText, p.mute && styles.chipTextOn]}>{p.mute ? `🔇 ${t('Ton aus')}` : `🔊 ${t('Ton an')}`}</Text>
            </Pressable>
          </View>
        </View>

        {saving && (
          <Animated.View entering={m(fadeIn)} style={styles.scrim}>
            <Pressable style={StyleSheet.absoluteFill} onPress={() => saving === 'ask' && setSaving(null)} />
            <Animated.View entering={m(popIn)} style={styles.sheet}>
              {saving === 'busy' ? (
                <View style={styles.busy}>
                  <Text style={styles.sheetText}>
                    {t('Wird gespeichert …')}
                    {progress >= 0 ? ` ${progress} %` : ''}
                  </Text>
                  <View style={styles.progress}>
                    {progress >= 0 ? <View style={[styles.progressFill, { width: `${progress}%` }]} /> : <ActivityIndicator color="#9fbcff" />}
                  </View>
                  <Pressable onPress={cancel} style={styles.cancel} accessibilityRole="button">
                    <Text style={styles.save}>{t('Abbrechen')}</Text>
                  </Pressable>
                </View>
              ) : (
                <>
                  <Text style={styles.sheetTitle}>{t('Speichern')}</Text>
                  <Pressable onPress={() => save('copy')} style={styles.sheetRow} accessibilityRole="button">
                    <Text style={styles.sheetText}>{t('Als Kopie speichern')}</Text>
                    <Text style={styles.sheetHint}>{t('Original bleibt unverändert')}</Text>
                  </Pressable>
                  {canReplace && (
                    <Pressable onPress={() => save('replace')} style={styles.sheetRow} accessibilityRole="button">
                      <Text style={styles.sheetText}>{t('Original ersetzen')}</Text>
                      <Text style={styles.sheetHint}>{t('Original wandert in den Papierkorb')}</Text>
                    </Pressable>
                  )}
                </>
              )}
            </Animated.View>
          </Animated.View>
        )}
      </GestureHandlerRootView>
    </Modal>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#0e0f12' },
  top: { flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 4, paddingBottom: 4 },
  btn: { minWidth: 48, height: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 10 },
  btnText: { color: '#fff', fontSize: 22 },
  title: { flex: 1, color: '#f1f1f4', fontSize: 16, fontWeight: '600', textAlign: 'center' },
  save: { color: '#9fbcff', fontSize: 16, fontWeight: '700' },
  dim: { opacity: 0.4 },
  stage: { flex: 1, margin: 16, justifyContent: 'center', alignItems: 'center', overflow: 'hidden' },
  play: { position: 'absolute', width: 64, height: 64, borderRadius: 32, backgroundColor: 'rgba(0,0,0,0.5)', justifyContent: 'center', alignItems: 'center' },
  playIcon: { color: '#fff', fontSize: 26, marginLeft: 4 },
  error: { color: '#eee', textAlign: 'center', padding: 24 },
  errorLine: { color: '#ffb4a8', textAlign: 'center', paddingHorizontal: 24, paddingBottom: 8 },
  tools: { paddingHorizontal: 12 + HANDLE, paddingBottom: 18 },
  range: { color: '#b4b6be', fontSize: 13, textAlign: 'center', marginBottom: 8 },
  timeline: { height: 56, justifyContent: 'center' },
  strip: { flexDirection: 'row', height: 48, borderRadius: 8, overflow: 'hidden', backgroundColor: 'rgba(255,255,255,0.08)' },
  frame: { flex: 1, height: 48, backgroundColor: 'rgba(255,255,255,0.05)' },
  shade: { position: 'absolute', top: 4, height: 48, backgroundColor: 'rgba(0,0,0,0.65)' },
  window: { position: 'absolute', top: 2, height: 52, borderTopWidth: 2, borderBottomWidth: 2, borderColor: '#f1f1f4' },
  cursor: { position: 'absolute', top: 0, width: 2, height: 56, backgroundColor: '#9fbcff' },
  handle: { position: 'absolute', top: 2, width: HANDLE, height: 52, backgroundColor: '#f1f1f4', borderRadius: 6, justifyContent: 'center', alignItems: 'center' },
  grip: { width: 3, height: 18, borderRadius: 2, backgroundColor: '#0e0f12' },
  chips: { flexDirection: 'row', gap: 8, justifyContent: 'center', paddingTop: 14 },
  chip: { minHeight: 40, paddingHorizontal: 16, borderRadius: 20, backgroundColor: 'rgba(255,255,255,0.1)', justifyContent: 'center' },
  chipOn: { backgroundColor: '#f1f1f4' },
  chipText: { color: '#f1f1f4', fontSize: 14 },
  chipTextOn: { color: '#0e0f12', fontWeight: '700' },
  scrim: { position: 'absolute', top: 0, left: 0, right: 0, bottom: 0, backgroundColor: 'rgba(0,0,0,0.5)', justifyContent: 'flex-end' },
  sheet: { backgroundColor: '#24262e', borderTopLeftRadius: 24, borderTopRightRadius: 24, paddingVertical: 14, paddingBottom: 28 },
  sheetTitle: { color: '#b4b6be', fontSize: 13, paddingHorizontal: 20, paddingBottom: 6 },
  sheetRow: { paddingHorizontal: 20, paddingVertical: 12, minHeight: 56 },
  sheetText: { color: '#f1f1f4', fontSize: 16 },
  sheetHint: { color: '#8a8f9c', fontSize: 13, marginTop: 2 },
  busy: { padding: 20, gap: 14 },
  progress: { height: 6, borderRadius: 3, backgroundColor: 'rgba(255,255,255,0.12)', overflow: 'hidden', justifyContent: 'center' },
  progressFill: { height: 6, backgroundColor: '#9fbcff' },
  cancel: { alignSelf: 'flex-end', minHeight: 44, justifyContent: 'center', paddingHorizontal: 8 },
});
