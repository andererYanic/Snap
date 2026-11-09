// Eingebaute Bildbearbeitung: Zuschneiden (frei oder festes Seitenverhältnis), Drehen, Anpassen
// (Helligkeit, Kontrast, Sättigung, Wärme) und Filter. Android rechnet (ImageEdit.kt), hier gibt es
// die Vorschau, den Zuschnittrahmen und die Regler. Speichern fragt: Kopie (vorausgewählt) oder
// Original ersetzen (Original in den Papierkorb, rückgängig machbar).
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Image, type LayoutChangeEvent, Modal, PixelRatio, Pressable, ScrollView, StatusBar, StyleSheet, View } from 'react-native';
import { Gesture, GestureDetector, GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, { runOnJS } from 'react-native-reanimated';
import * as core from './core';
import { request, type EditParams } from './core';
import { t } from './i18n';
import { fadeIn, m, popIn } from './motion';
import { Text } from './text';

type Rect = { x: number; y: number; w: number; h: number };
const FULL: Rect = { x: 0, y: 0, w: 1, h: 1 };
const MIN = 0.08; // kleinster Zuschnitt (Anteil)
const ASPECTS: [string, number | null][] = [['Frei', null], ['1:1', 1], ['4:3', 4 / 3], ['3:4', 3 / 4], ['16:9', 16 / 9]];
const FILTERS: [string, string][] = [['', 'Original'], ['klar', 'Klar'], ['warm', 'Warm'], ['kuehl', 'Kühl'], ['sw', 'S/W'], ['matt', 'Matt']];
const PX = Math.round(1080 * Math.min(2, PixelRatio.get() / 1.5));

// Worklet: wird auch in Gesten auf dem UI-Thread aufgerufen (sonst Absturz „Remote Function“).
const clamp = (v: number, a: number, b: number) => {
  'worklet';
  return Math.max(a, Math.min(b, v));
};

/** Größter zentrierter Ausschnitt mit Seitenverhältnis `a` (Pixelverhältnis des Bildes `img`). */
export function fitAspect(a: number | null, img: number): Rect {
  if (!a) {
    return FULL;
  }
  // a = Breite/Höhe in Pixeln; im normierten Raum: w/h = a / img
  const r = a / img;
  return r >= 1 ? { x: 0, y: (1 - 1 / r) / 2, w: 1, h: 1 / r } : { x: (1 - r) / 2, y: 0, w: r, h: 1 };
}

/** Regler von -1 bis 1 (angezeigt -100 … 100), per Ziehen oder Tippen. */
function Slider({ label, value, onChange }: { label: string; value: number; onChange: (v: number) => void }) {
  const [width, setWidth] = useState(1);
  const set = useCallback((x: number) => onChange(clamp((x / width) * 2 - 1, -1, 1)), [onChange, width]);
  const pan = Gesture.Pan().minDistance(0).onBegin((e) => runOnJS(set)(e.x)).onUpdate((e) => runOnJS(set)(e.x));
  const pos = ((value + 1) / 2) * width;
  return (
    <View style={styles.slider}>
      <Text style={styles.sliderLabel}>{label}</Text>
      <GestureDetector gesture={pan}>
        <View style={styles.track} onLayout={(e) => setWidth(Math.max(1, e.nativeEvent.layout.width))} accessibilityRole="adjustable" accessibilityLabel={label} accessibilityValue={{ min: -100, max: 100, now: Math.round(value * 100) }}>
          <View style={styles.rail} />
          <View style={[styles.fill, { left: Math.min(pos, width / 2), width: Math.abs(pos - width / 2) }]} />
          <View style={[styles.thumb, { left: pos - 11 }]} />
        </View>
      </GestureDetector>
      <Pressable onPress={() => onChange(0)} style={styles.sliderValue} accessibilityLabel={t('Zurücksetzen')}>
        <Text style={styles.sliderValueText}>{Math.round(value * 100)}</Text>
      </Pressable>
    </View>
  );
}

export default function ImageEditor({ path, name, onClose, notify }: {
  path: string;
  name: string;
  /** `changed` = es wurde gespeichert (Liste neu laden). */
  onClose: (changed: boolean) => void;
  notify: (text: string, undo?: () => void) => void;
}) {
  const [p, setP] = useState<EditParams>({ rot: 0, crop: null, light: 0, contrast: 0, sat: 0, warm: 0, filter: '' });
  const [tab, setTab] = useState<'crop' | 'adjust' | 'filter'>('crop');
  const [aspect, setAspect] = useState<number | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [imgSize, setImgSize] = useState<{ w: number; h: number } | null>(null);
  const [stage, setStage] = useState<{ w: number; h: number }>({ w: 1, h: 1 });
  const [filterThumbs, setFilterThumbs] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState<'ask' | 'busy' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const crop = p.crop ?? FULL;

  // Vorschau ohne Zuschnitt (der Rahmen liegt darüber); kurz entprellt, damit Regler flüssig bleiben
  const look = useMemo(() => ({ ...p, crop: null }), [p]);
  const gen = useRef(0);
  useEffect(() => {
    const g = ++gen.current;
    const timer = setTimeout(() => {
      core
        .editPreview(path, look, PX)
        .then((f) => {
          if (g === gen.current) {
            setPreview(`file://${f}`);
            Image.getSize(`file://${f}`, (w, h) => g === gen.current && setImgSize({ w, h }), () => {});
          }
        })
        .catch((e) => setError(String(e?.message ?? e)));
    }, 90);
    return () => clearTimeout(timer);
  }, [path, look]);

  // Kleine Vorschauen der Filter (einmal je Drehung)
  useEffect(() => {
    if (tab !== 'filter') {
      return;
    }
    FILTERS.forEach(([f]) =>
      core
        .editPreview(path, { ...look, filter: f, light: 0, contrast: 0, sat: 0, warm: 0 }, 160)
        .then((file) => setFilterThumbs((x) => ({ ...x, [f]: `file://${file}` })))
        .catch(() => {}),
    );
  }, [tab, path, look.rot]); // eslint-disable-line react-hooks/exhaustive-deps

  // Angezeigtes Bild im Arbeitsbereich (eingepasst)
  const box = useMemo(() => {
    if (!imgSize) {
      return null;
    }
    const s = Math.min(stage.w / imgSize.w, stage.h / imgSize.h);
    const w = imgSize.w * s;
    const h = imgSize.h * s;
    return { x: (stage.w - w) / 2, y: (stage.h - h) / 2, w, h };
  }, [imgSize, stage]);

  const setCrop = useCallback((r: Rect) => setP((x) => ({ ...x, crop: r.w > 0.999 && r.h > 0.999 ? null : r })), []);

  // Ziehen an Ecken bzw. im Rahmen
  const startRef = useRef<Rect>(FULL);
  const drag = useCallback(
    (kind: 'move' | 'tl' | 'tr' | 'bl' | 'br', dx: number, dy: number, begin: boolean) => {
      if (!box) {
        return;
      }
      if (begin) {
        startRef.current = crop;
        return;
      }
      const s = startRef.current;
      const nx = dx / box.w;
      const ny = dy / box.h;
      let { x, y, w, h } = s;
      if (kind === 'move') {
        x = clamp(s.x + nx, 0, 1 - s.w);
        y = clamp(s.y + ny, 0, 1 - s.h);
      } else {
        const left = kind === 'tl' || kind === 'bl';
        const top = kind === 'tl' || kind === 'tr';
        let x1 = left ? clamp(s.x + nx, 0, s.x + s.w - MIN) : s.x;
        let x2 = left ? s.x + s.w : clamp(s.x + s.w + nx, s.x + MIN, 1);
        let y1 = top ? clamp(s.y + ny, 0, s.y + s.h - MIN) : s.y;
        let y2 = top ? s.y + s.h : clamp(s.y + s.h + ny, s.y + MIN, 1);
        if (aspect && imgSize) {
          // Seitenverhältnis halten: Höhe folgt der Breite
          const r = aspect / (imgSize.w / imgSize.h);
          const nw = x2 - x1;
          let nh = nw / r;
          if (top) {
            nh = Math.min(nh, y2);
            y1 = y2 - nh;
          } else {
            nh = Math.min(nh, 1 - y1);
            y2 = y1 + nh;
          }
          const fixedW = nh * r;
          if (left) {
            x1 = x2 - fixedW;
          } else {
            x2 = x1 + fixedW;
          }
        }
        x = x1;
        y = y1;
        w = x2 - x1;
        h = y2 - y1;
      }
      setCrop({ x, y, w, h });
    },
    [box, crop, aspect, imgSize, setCrop],
  );
  const pan = (kind: 'move' | 'tl' | 'tr' | 'bl' | 'br') =>
    Gesture.Pan()
      .minDistance(0)
      .onBegin(() => runOnJS(drag)(kind, 0, 0, true))
      .onUpdate((e) => runOnJS(drag)(kind, e.translationX, e.translationY, false));

  const rotate = () => {
    setP((x) => ({ ...x, rot: (x.rot + 270) % 360, crop: null }));
    setAspect(null);
  };
  const chooseAspect = (a: number | null) => {
    setAspect(a);
    if (imgSize) {
      setCrop(fitAspect(a, imgSize.w / imgSize.h));
    }
  };
  const changed = p.rot !== 0 || p.crop !== null || p.light !== 0 || p.contrast !== 0 || p.sat !== 0 || p.warm !== 0 || p.filter !== '';

  const save = async (mode: 'copy' | 'replace') => {
    setSaving('busy');
    try {
      if (mode === 'copy') {
        const out = await core.editSave(path, p, 'copy');
        const outName = out.slice(out.lastIndexOf('/') + 1);
        onClose(true);
        notify(t('Gespeichert als „{name}“.', { name: outName }), () => {
          request({ FileOp: { op: { Trash: { paths: [out] } }, wait: true } }).catch(() => {});
        });
      } else {
        const tmp = await core.editSave(path, p, 'temp');
        await request({ FileOp: { op: { Replace: { path, with: tmp } }, wait: true } });
        onClose(true);
        notify(t('Gespeichert. Das Original liegt im Papierkorb.'), () => {
          request('Undo').catch(() => {});
        });
      }
    } catch (e: any) {
      setSaving(null);
      setError(String(e?.message ?? e));
    }
  };

  const onStage = (e: LayoutChangeEvent) => setStage({ w: e.nativeEvent.layout.width, h: e.nativeEvent.layout.height });

  return (
    <Modal visible animationType="fade" onRequestClose={() => (saving === 'ask' ? setSaving(null) : onClose(false))} statusBarTranslucent>
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
          {preview && box ? (
            <>
              <Image source={{ uri: preview }} style={{ position: 'absolute', left: box.x, top: box.y, width: box.w, height: box.h }} fadeDuration={0} />
              {tab === 'crop' && (
                <>
                  {/* Abdunkelung außerhalb des Zuschnitts */}
                  <View pointerEvents="none" style={[styles.shade, { left: box.x, top: box.y, width: box.w, height: crop.y * box.h }]} />
                  <View pointerEvents="none" style={[styles.shade, { left: box.x, top: box.y + (crop.y + crop.h) * box.h, width: box.w, height: (1 - crop.y - crop.h) * box.h }]} />
                  <View pointerEvents="none" style={[styles.shade, { left: box.x, top: box.y + crop.y * box.h, width: crop.x * box.w, height: crop.h * box.h }]} />
                  <View pointerEvents="none" style={[styles.shade, { left: box.x + (crop.x + crop.w) * box.w, top: box.y + crop.y * box.h, width: (1 - crop.x - crop.w) * box.w, height: crop.h * box.h }]} />
                  <GestureDetector gesture={pan('move')}>
                    <View style={[styles.frame, { left: box.x + crop.x * box.w, top: box.y + crop.y * box.h, width: crop.w * box.w, height: crop.h * box.h }]} accessibilityLabel={t('Zuschnitt')}>
                      <View pointerEvents="none" style={[styles.third, styles.thirdV, { left: '33.3%' }]} />
                      <View pointerEvents="none" style={[styles.third, styles.thirdV, { left: '66.6%' }]} />
                      <View pointerEvents="none" style={[styles.third, styles.thirdH, { top: '33.3%' }]} />
                      <View pointerEvents="none" style={[styles.third, styles.thirdH, { top: '66.6%' }]} />
                      {(['tl', 'tr', 'bl', 'br'] as const).map((k) => (
                        <GestureDetector key={k} gesture={pan(k)}>
                          <View
                            style={[styles.corner, k.includes('t') ? { top: -14 } : { bottom: -14 }, k.includes('l') ? { left: -14 } : { right: -14 }]}
                            accessibilityLabel={t('Ecke ziehen')}
                          >
                            <View style={[styles.cornerMark, k.includes('t') ? styles.cornerTop : styles.cornerBottom, k.includes('l') ? styles.cornerLeft : styles.cornerRight]} />
                          </View>
                        </GestureDetector>
                      ))}
                    </View>
                  </GestureDetector>
                </>
              )}
            </>
          ) : error ? (
            <Text style={styles.error}>{error}</Text>
          ) : (
            <ActivityIndicator color="#fff" />
          )}
        </View>

        <View style={styles.tools}>
          {tab === 'crop' && (
            <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={styles.chips}>
              {ASPECTS.map(([l, a]) => (
                <Pressable key={l} onPress={() => chooseAspect(a)} style={[styles.chip, aspect === a && styles.chipOn]}>
                  <Text style={[styles.chipText, aspect === a && styles.chipTextOn]}>{l === 'Frei' ? t('Frei') : l}</Text>
                </Pressable>
              ))}
              <Pressable onPress={rotate} style={styles.chip} accessibilityLabel={t('Drehen')}>
                <Text style={styles.chipText}>⟲ {t('Drehen')}</Text>
              </Pressable>
            </ScrollView>
          )}
          {tab === 'adjust' && (
            <View>
              <Slider label={t('Helligkeit')} value={p.light} onChange={(v) => setP((x) => ({ ...x, light: v }))} />
              <Slider label={t('Kontrast')} value={p.contrast} onChange={(v) => setP((x) => ({ ...x, contrast: v }))} />
              <Slider label={t('Sättigung')} value={p.sat} onChange={(v) => setP((x) => ({ ...x, sat: v }))} />
              <Slider label={t('Wärme')} value={p.warm} onChange={(v) => setP((x) => ({ ...x, warm: v }))} />
            </View>
          )}
          {tab === 'filter' && (
            <ScrollView horizontal showsHorizontalScrollIndicator={false} contentContainerStyle={styles.chips}>
              {FILTERS.map(([f, l]) => (
                <Pressable key={f || 'none'} onPress={() => setP((x) => ({ ...x, filter: f }))} style={[styles.filter, p.filter === f && styles.filterOn]} accessibilityLabel={t(l)}>
                  {filterThumbs[f] ? <Image source={{ uri: filterThumbs[f] }} style={styles.filterImg} /> : <View style={styles.filterImg} />}
                  <Text style={styles.filterText}>{t(l)}</Text>
                </Pressable>
              ))}
            </ScrollView>
          )}
          <View style={styles.tabs}>
            {([['crop', '⬚', t('Zuschneiden')], ['adjust', '☼', t('Anpassen')], ['filter', '◑', t('Filter')]] as const).map(([k, i, l]) => (
              <Pressable key={k} onPress={() => setTab(k)} style={styles.tab} accessibilityRole="tab" accessibilityState={{ selected: tab === k }}>
                <Text style={[styles.tabIcon, tab === k && styles.tabOn]}>{i}</Text>
                <Text style={[styles.tabText, tab === k && styles.tabOn]}>{l}</Text>
              </Pressable>
            ))}
          </View>
        </View>

        {saving && (
          <Animated.View entering={m(fadeIn)} style={styles.scrim}>
            <Pressable style={StyleSheet.absoluteFill} onPress={() => saving === 'ask' && setSaving(null)} />
            <Animated.View entering={m(popIn)} style={styles.sheet}>
              {saving === 'busy' ? (
                <View style={styles.busy}>
                  <ActivityIndicator color="#9fbcff" />
                  <Text style={styles.sheetText}>{t('Wird gespeichert …')}</Text>
                </View>
              ) : (
                <>
                  <Text style={styles.sheetTitle}>{t('Speichern')}</Text>
                  <Pressable onPress={() => save('copy')} style={styles.sheetRow} accessibilityRole="button">
                    <Text style={styles.sheetText}>{t('Als Kopie speichern')}</Text>
                    <Text style={styles.sheetHint}>{t('Original bleibt unverändert')}</Text>
                  </Pressable>
                  <Pressable onPress={() => save('replace')} style={styles.sheetRow} accessibilityRole="button">
                    <Text style={styles.sheetText}>{t('Original ersetzen')}</Text>
                    <Text style={styles.sheetHint}>{t('Original wandert in den Papierkorb')}</Text>
                  </Pressable>
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
  stage: { flex: 1, margin: 16, justifyContent: 'center', alignItems: 'center' },
  error: { color: '#eee', textAlign: 'center', padding: 24 },
  shade: { position: 'absolute', backgroundColor: 'rgba(0,0,0,0.6)' },
  frame: { position: 'absolute', borderWidth: 1.5, borderColor: '#fff' },
  third: { position: 'absolute', backgroundColor: 'rgba(255,255,255,0.35)' },
  thirdV: { top: 0, bottom: 0, width: StyleSheet.hairlineWidth },
  thirdH: { left: 0, right: 0, height: StyleSheet.hairlineWidth },
  corner: { position: 'absolute', width: 44, height: 44, justifyContent: 'center', alignItems: 'center' },
  cornerMark: { position: 'absolute', width: 22, height: 22, borderColor: '#fff' },
  cornerTop: { top: 13, borderTopWidth: 4 },
  cornerBottom: { bottom: 13, borderBottomWidth: 4 },
  cornerLeft: { left: 13, borderLeftWidth: 4 },
  cornerRight: { right: 13, borderRightWidth: 4 },
  tools: { paddingHorizontal: 12, paddingBottom: 14 },
  chips: { gap: 8, paddingVertical: 8, paddingHorizontal: 4 },
  chip: { minHeight: 40, paddingHorizontal: 16, borderRadius: 20, backgroundColor: 'rgba(255,255,255,0.1)', justifyContent: 'center' },
  chipOn: { backgroundColor: '#f1f1f4' },
  chipText: { color: '#f1f1f4', fontSize: 14 },
  chipTextOn: { color: '#0e0f12', fontWeight: '700' },
  slider: { flexDirection: 'row', alignItems: 'center', gap: 10, height: 44 },
  sliderLabel: { width: 92, color: '#f1f1f4', fontSize: 14 },
  track: { flex: 1, height: 44, justifyContent: 'center' },
  rail: { height: 3, borderRadius: 2, backgroundColor: 'rgba(255,255,255,0.2)' },
  fill: { position: 'absolute', height: 3, borderRadius: 2, backgroundColor: '#9fbcff' },
  thumb: { position: 'absolute', width: 22, height: 22, borderRadius: 11, backgroundColor: '#fff' },
  sliderValue: { width: 40, alignItems: 'flex-end' },
  sliderValueText: { color: '#b4b6be', fontSize: 13 },
  filter: { alignItems: 'center', gap: 6, padding: 6, borderRadius: 14 },
  filterOn: { backgroundColor: 'rgba(255,255,255,0.15)' },
  filterImg: { width: 64, height: 64, borderRadius: 10, backgroundColor: 'rgba(255,255,255,0.08)' },
  filterText: { color: '#f1f1f4', fontSize: 12 },
  tabs: { flexDirection: 'row', borderTopWidth: StyleSheet.hairlineWidth, borderTopColor: 'rgba(255,255,255,0.12)', paddingTop: 6, marginTop: 6 },
  tab: { flex: 1, minHeight: 50, alignItems: 'center', justifyContent: 'center', gap: 2 },
  tabIcon: { color: '#b4b6be', fontSize: 18 },
  tabText: { color: '#b4b6be', fontSize: 13 },
  tabOn: { color: '#9fbcff', fontWeight: '700' },
  scrim: { position: 'absolute', top: 0, left: 0, right: 0, bottom: 0, backgroundColor: 'rgba(0,0,0,0.5)', justifyContent: 'flex-end' },
  sheet: { backgroundColor: '#24262e', borderTopLeftRadius: 24, borderTopRightRadius: 24, paddingVertical: 14, paddingBottom: 28 },
  sheetTitle: { color: '#b4b6be', fontSize: 13, paddingHorizontal: 20, paddingBottom: 6 },
  sheetRow: { paddingHorizontal: 20, paddingVertical: 12, minHeight: 56 },
  sheetText: { color: '#f1f1f4', fontSize: 16 },
  sheetHint: { color: '#8a8f9c', fontSize: 13, marginTop: 2 },
  busy: { flexDirection: 'row', gap: 14, alignItems: 'center', padding: 20 },
});
