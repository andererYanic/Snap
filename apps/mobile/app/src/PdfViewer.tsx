// Eingebaute PDF-Anzeige (wie bei Google Drive, aber lokal): Android zeichnet die Seiten selbst
// (`PdfRenderer`, Teil des Systems). Seiten erscheinen beim Scrollen. Zoomen mit zwei Fingern um
// den Punkt zwischen ihnen (live per Transformation, danach werden die Seiten in der neuen Größe
// scharf neu gezeichnet), Doppeltippen wechselt zwischen 100 % und 250 %. Gezoomt lässt sich in
// beide Richtungen scrollen. Dazu: Textmarker in Standardfarben und Signieren (sichtbare
// Unterschrift + kryptografische PAdES-Signatur, im Kern); beides wird Seiteninhalt und ist daher
// überall sichtbar, auch gedruckt. Beim Öffnen werden vorhandene Signaturen geprüft.
import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Alert, Dimensions, Image, Modal, PixelRatio, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import { Text } from './text';
import { FlatList, Gesture, GestureDetector, GestureHandlerRootView, ScrollView } from 'react-native-gesture-handler';
import Animated, { runOnJS, useAnimatedStyle, useSharedValue } from 'react-native-reanimated';
import * as core from './core';
import { request } from './core';
import { t } from './i18n';
import { fadeIn, m, popIn } from './motion';
import { DrawArea, INK_WIDTH, MARKER_COLORS, MARKER_WIDTH, type Stroke, Strokes } from './Ink';
import { AskPassword, CreateId, groupFingerprint, idInfo } from './SignId';
import SignaturePad, { PAD_RATIO } from './SignaturePad';

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

const PdfPage = memo(function PdfPage({ path, rev, index, ratio, scale }: { path: string; rev: number; index: number; ratio: number; scale: number }) {
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
  }, [path, rev, index, step]);
  const w = PAGE_W * scale;
  return (
    <View style={[styles.page, { width: w, height: w * ratio }]} accessibilityLabel={t('Seite {n}', { n: index + 1 })}>
      {uri ? <Image source={{ uri }} style={styles.fill} fadeDuration={0} /> : <ActivityIndicator style={styles.fill} color="#8a8f9c" />}
    </View>
  );
});

/** Geprüfte Signatur (vom Kern). */
export interface SignatureReport {
  field: string;
  valid: boolean;
  covers_document: boolean;
  signer: string;
  issuer: string;
  self_signed: boolean;
  own: boolean;
  fingerprint: string;
  algorithm: string;
  signed_at: string | null;
  problem: string | null;
}

/** Zusammenfassung für den Hinweis oben: Art und Text. */
export function signatureSummary(items: SignatureReport[]): { kind: 'ok' | 'warn' | 'bad'; text: string } | null {
  if (items.length === 0) {
    return null;
  }
  const bad = items.find((s) => !s.valid);
  if (bad) {
    return { kind: 'bad', text: t('Signatur ungültig: {why}', { why: bad.problem ?? t('unbekannter Fehler') }) };
  }
  const last = items.reduce((a, b) => (b.covers_document ? b : a), items[items.length - 1]);
  const names = Array.from(new Set(items.map((s) => s.signer))).join(', ');
  if (!items.some((s) => s.covers_document)) {
    return { kind: 'warn', text: t('Signiert von {name} – danach geändert', { name: names }) };
  }
  return { kind: last.self_signed && !last.own ? 'warn' : 'ok', text: t('Signiert von {name} – unverändert', { name: names }) };
}

type Mark = { page: number; color: string; stroke: Stroke };
type Place = { page: number; x: number; y: number; w: number };
type Flow = null | 'createId' | 'pad' | 'saveMark' | 'saveSign' | 'password' | 'busy';

/** Unterschrift auf der Seite verschieben (ziehen) und in der Größe ändern (Ecke ziehen). */
function SignatureBox({ place, strokes, pageW, pageH, onChange }: { place: Place; strokes: Stroke[]; pageW: number; pageH: number; onChange: (p: Place) => void }) {
  const start = useRef(place);
  const w = place.w * pageW;
  const h = w / PAD_RATIO;
  const move = useCallback((dx: number, dy: number, begin: boolean) => {
    if (begin) {
      start.current = place;
      return;
    }
    const s = start.current;
    const hn = (s.w * pageW) / PAD_RATIO / pageH;
    onChange({ ...s, x: clamp(s.x + dx / pageW, 0, 1 - s.w), y: clamp(s.y + dy / pageH, 0, 1 - hn) });
  }, [place, pageW, pageH, onChange]);
  const resize = useCallback((dx: number, begin: boolean) => {
    if (begin) {
      start.current = place;
      return;
    }
    const s = start.current;
    onChange({ ...s, w: clamp(s.w + dx / pageW, 0.12, 1 - s.x) });
  }, [place, pageW, onChange]);
  const panMove = Gesture.Pan().minDistance(0).maxPointers(1)
    .onBegin(() => runOnJS(move)(0, 0, true))
    .onUpdate((e) => runOnJS(move)(e.translationX, e.translationY, false));
  const panSize = Gesture.Pan().minDistance(0).maxPointers(1)
    .onBegin(() => runOnJS(resize)(0, true))
    .onUpdate((e) => runOnJS(resize)(e.translationX, false));
  return (
    <GestureDetector gesture={panMove}>
      <View style={[styles.sigBox, { left: place.x * pageW, top: place.y * pageH, width: w, height: h }]} accessibilityLabel={t('Unterschrift verschieben')}>
        <Strokes strokes={strokes} width={w} height={h} color="#141f59" strokeWidth={Math.max(1.2, INK_WIDTH * pageW)} />
        <GestureDetector gesture={panSize}>
          <View style={styles.sigHandle} accessibilityLabel={t('Größe ändern')}>
            <View style={styles.sigHandleDot} />
          </View>
        </GestureDetector>
      </View>
    </GestureDetector>
  );
}

export default function PdfViewer({ path, name, onClose, notify, onChanged, initialMode = 'view' }: {
  path: string;
  name: string;
  onClose: () => void;
  notify?: (text: string, undo?: () => void) => void;
  /** Datei wurde gespeichert (Liste neu laden). */
  onChanged?: () => void;
  initialMode?: 'view' | 'mark' | 'sign';
}) {
  const [doc, setDoc] = useState(path);
  const [docName, setDocName] = useState(name);
  const [rev, setRev] = useState(0);
  const [info, setInfo] = useState<{ pages: number; ratios: number[] } | null | 'error'>(null);
  const [page, setPage] = useState(0);
  const [scale, setScale] = useState(1);
  const [scrolling, setScrolling] = useState(true);
  const [mode, setMode] = useState<'view' | 'mark' | 'place'>('view');
  const [color, setColor] = useState(MARKER_COLORS[0][0]);
  const [marks, setMarks] = useState<Mark[]>([]);
  const [hand, setHand] = useState(false);
  const [sigs, setSigs] = useState<SignatureReport[]>([]);
  const [flow, setFlow] = useState<Flow>(null);
  const [sigStrokes, setSigStrokes] = useState<Stroke[]>([]);
  const [place, setPlace] = useState<Place | null>(null);
  const [saveMode, setSaveMode] = useState<'copy' | 'replace'>('copy');
  const [wrongPw, setWrongPw] = useState(false);
  const hScroll = useRef<any>(null);
  const vList = useRef<any>(null);
  const pos = useRef({ x: 0, y: 0 });
  // Live-Zoom während der Geste (Transformation um den Fingerpunkt), danach übernommen
  const live = useSharedValue(1);
  const ox = useSharedValue(0);
  const oy = useSharedValue(0);

  useEffect(() => {
    setInfo(null);
    core.pdfInfo(doc).then((i) => setInfo(i && i.pages > 0 ? i : 'error')).catch(() => setInfo('error'));
    request<{ items: SignatureReport[] }>({ PdfVerify: { path: doc } }).then((r) => setSigs(r.items ?? [])).catch(() => setSigs([]));
  }, [doc, rev]);

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
    .enabled(mode === 'view')
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
  const summary = useMemo(() => signatureSummary(sigs), [sigs]);
  const drawing = mode === 'mark' && !hand;
  const listScroll = scrolling && !drawing && mode !== 'place';

  // ------------------------------------------------------------ Speichern (Kopie oder ersetzen)
  const finishSave = async (out: string, replace: boolean, what: string) => {
    let next = out;
    if (replace) {
      await request({ FileOp: { op: { Replace: { path: doc, with: out } }, wait: true } });
      next = doc;
    }
    setMarks([]);
    setPlace(null);
    setMode('view');
    setFlow(null);
    setDoc(next);
    setDocName(next.slice(next.lastIndexOf('/') + 1));
    setRev((r) => r + 1);
    onChanged?.();
    notify?.(replace ? t('{what} Das Original liegt im Papierkorb.', { what }) : t('{what} Gespeichert als „{name}“.', { what, name: next.slice(next.lastIndexOf('/') + 1) }), replace ? () => {
      request('Undo').then(() => { setRev((r) => r + 1); onChanged?.(); }).catch(() => {});
    } : undefined);
  };

  const saveMarks = async (m2: 'copy' | 'replace') => {
    setFlow('busy');
    const byKey = new Map<string, { page: number; color: string; strokes: Stroke[] }>();
    for (const mk of marks) {
      const k = `${mk.page}|${mk.color}`;
      const e = byKey.get(k) ?? { page: mk.page, color: mk.color, strokes: [] };
      e.strokes.push(mk.stroke);
      byKey.set(k, e);
    }
    const layers = Array.from(byKey.values()).map((l) => ({ ...l, width: MARKER_WIDTH }));
    try {
      const r = await request<{ path: string }>({ PdfMark: { path: doc, layers, mode: m2 === 'replace' ? 'temp' : 'copy' } });
      await finishSave(r.path, m2 === 'replace', t('Markierungen gespeichert.'));
    } catch (e: any) {
      setFlow(null);
      notify?.(String(e?.message ?? e));
    }
  };

  const startSign = async () => {
    try {
      const st = await idInfo();
      if (!st.id) {
        setFlow('createId');
        return;
      }
      await continueSign(st.has_signature);
    } catch (e: any) {
      notify?.(String(e?.message ?? e));
    }
  };
  const continueSign = async (hasSignature: boolean) => {
    if (!hasSignature) {
      setFlow('pad');
      return;
    }
    const r = await request<{ strokes: Stroke[] }>('SignatureGet');
    if (!r.strokes?.length) {
      setFlow('pad');
      return;
    }
    beginPlace(r.strokes);
  };
  const beginPlace = (strokes: Stroke[]) => {
    setSigStrokes(strokes);
    setFlow(null);
    setPlace({ page, x: 0.55, y: 0.78, w: 0.35 });
    setMode('place');
  };
  useEffect(() => {
    if (ok && initialMode === 'sign') {
      startSign();
    } else if (ok && initialMode === 'mark') {
      setMode('mark');
    }
    // nur beim ersten Laden
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [!!ok]);

  const sign = async (pw: string) => {
    if (!place || !ok) {
      return;
    }
    setFlow('busy');
    const ratio = ok.ratios[place.page];
    const hn = place.w / PAD_RATIO / ratio;
    const strokes = sigStrokes.map((s) => s.map(([u, v]) => [place.x + u * place.w, place.y + v * hn]));
    try {
      const r = await request<{ path: string }>({
        PdfSign: { path: doc, placement: { page: place.page, strokes, width: INK_WIDTH }, password: pw, mode: saveMode === 'replace' ? 'temp' : 'copy' },
      });
      setWrongPw(false);
      await finishSave(r.path, saveMode === 'replace', t('Signiert.'));
    } catch (e: any) {
      if (e?.code === 'PasswordRequired') {
        setWrongPw(true);
        setFlow('password');
      } else {
        setFlow(null);
        notify?.(String(e?.message ?? e));
      }
    }
  };

  const showDetails = () => {
    const lines = sigs.map((s) => {
      const state = !s.valid ? t('ungültig') : s.covers_document ? t('gültig, Dokument unverändert') : t('gültig, aber danach geändert');
      const who = s.own ? t('{name} (deine ID)', { name: s.signer }) : s.self_signed ? t('{name} (Identität nicht bestätigt)', { name: s.signer }) : t('{name}, ausgestellt von {issuer}', { name: s.signer, issuer: s.issuer });
      return [`${s.field}: ${state}`, who, s.signed_at ? t('Zeitpunkt: {when}', { when: s.signed_at }) : '', s.algorithm, s.fingerprint ? t('Fingerabdruck: {fp}', { fp: groupFingerprint(s.fingerprint) }) : '', s.problem ?? ''].filter(Boolean).join('\n');
    });
    Alert.alert(t('Signaturen'), lines.join('\n\n'));
  };

  const closeOrBack = () => {
    if (flow && flow !== 'busy') {
      setFlow(null);
    } else if (mode !== 'view') {
      setMode('view');
      setPlace(null);
    } else {
      onClose();
    }
  };

  return (
    <Modal visible transparent={false} animationType="fade" onRequestClose={closeOrBack} statusBarTranslucent>
      <GestureHandlerRootView style={styles.root}>
        <StatusBar barStyle="light-content" />
        <View style={styles.top}>
          <Pressable onPress={onClose} style={styles.btn} accessibilityLabel={t('Schließen')}>
            <Text style={styles.btnText}>✕</Text>
          </Pressable>
          <View style={styles.titleBox}>
            <Text style={styles.title} numberOfLines={1}>{docName}</Text>
            {ok && <Text style={styles.counter}>{t('Seite {n} von {total}', { n: page + 1, total: ok.pages })}{scale > 1.01 ? `  ·  ${Math.round(scale * 100)} %` : ''}</Text>}
          </View>
          <Pressable onPress={() => core.printFile(doc).catch(() => {})} style={styles.btn} accessibilityLabel={t('Drucken')}>
            <Text style={styles.small}>{t('Drucken')}</Text>
          </Pressable>
          <Pressable onPress={() => core.openFile(doc).catch(() => {})} style={styles.btn} accessibilityLabel={t('Öffnen mit …')}>
            <Text style={styles.btnText}>⋯</Text>
          </Pressable>
        </View>
        {summary && (
          <Pressable onPress={showDetails} style={[styles.banner, styles[summary.kind]]} accessibilityRole="button" accessibilityLabel={summary.text}>
            <Text style={styles.bannerIcon}>{summary.kind === 'ok' ? '✓' : summary.kind === 'warn' ? '!' : '✕'}</Text>
            <Text style={styles.bannerText} numberOfLines={2}>{summary.text}</Text>
            <Text style={styles.bannerMore}>{t('Details')}</Text>
          </Pressable>
        )}
        {info === null ? (
          <ActivityIndicator style={styles.fill} color="#fff" />
        ) : info === 'error' ? (
          <Animated.View entering={m(fadeIn)} style={styles.error}>
            <Text style={styles.errorText}>{t('Dieses PDF lässt sich nicht anzeigen.')}</Text>
            <Pressable onPress={() => core.openFile(doc).catch(() => {})} style={styles.errorBtn}>
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
                  scrollEnabled={listScroll && scale > 1.01}
                  showsHorizontalScrollIndicator={false}
                  onScroll={(e) => (pos.current.x = e.nativeEvent.contentOffset.x)}
                  scrollEventThrottle={32}
                  contentContainerStyle={{ width: pageW + 2 * PAD }}
                >
                  <FlatList
                    ref={vList}
                    style={{ width: pageW + 2 * PAD }}
                    data={Array.from({ length: info.pages }, (_, i) => i)}
                    keyExtractor={(i) => `${doc}|${i}`}
                    extraData={[scale, marks, mode, hand, place, color, rev]}
                    scrollEnabled={listScroll}
                    contentContainerStyle={styles.list}
                    getItemLayout={(_, i) => ({ length: pageW * info.ratios[i] + GAP, offset: offsets[i], index: i })}
                    initialNumToRender={3}
                    maxToRenderPerBatch={3}
                    windowSize={5}
                    onScroll={(e) => (pos.current.y = e.nativeEvent.contentOffset.y)}
                    scrollEventThrottle={32}
                    onViewableItemsChanged={({ viewableItems }) => viewableItems[0] && setPage(viewableItems[0].index ?? 0)}
                    viewabilityConfig={{ itemVisiblePercentThreshold: 40 }}
                    renderItem={({ item: i }) => {
                      const h = pageW * info.ratios[i];
                      const pageMarks = marks.filter((mk) => mk.page === i);
                      const colors = Array.from(new Set(pageMarks.map((mk) => mk.color)));
                      return (
                        <View style={{ width: pageW, height: h, marginBottom: GAP }}>
                          <PdfPage path={doc} rev={rev} index={i} ratio={info.ratios[i]} scale={scale} />
                          {colors.map((c) => (
                            <Strokes key={c} strokes={pageMarks.filter((mk) => mk.color === c).map((mk) => mk.stroke)} width={pageW} height={h} color={c} strokeWidth={MARKER_WIDTH * pageW} opacity={0.45} />
                          ))}
                          {drawing && (
                            <DrawArea
                              width={pageW}
                              height={h}
                              color={color}
                              strokeWidth={MARKER_WIDTH * pageW}
                              opacity={0.45}
                              testID={`mark-page-${i}`}
                              onStroke={(stroke) => setMarks((x) => [...x, { page: i, color, stroke }])}
                            />
                          )}
                          {mode === 'place' && place && place.page === i && (
                            <SignatureBox place={place} strokes={sigStrokes} pageW={pageW} pageH={h} onChange={setPlace} />
                          )}
                        </View>
                      );
                    }}
                  />
                </ScrollView>
              </Animated.View>
            </Animated.View>
          </GestureDetector>
        )}

        {ok && mode === 'view' && (
          <View style={styles.bar}>
            <Pressable onPress={() => setMode('mark')} style={styles.barBtn} accessibilityRole="button" accessibilityLabel={t('Markieren')}>
              <Text style={styles.barIcon}>🖍</Text>
              <Text style={styles.barText}>{t('Markieren')}</Text>
            </Pressable>
            <Pressable onPress={startSign} style={styles.barBtn} accessibilityRole="button" accessibilityLabel={t('Signieren')}>
              <Text style={styles.barIcon}>✍</Text>
              <Text style={styles.barText}>{t('Signieren')}</Text>
            </Pressable>
          </View>
        )}
        {mode === 'mark' && (
          <Animated.View entering={m(popIn)} style={styles.bar}>
            {MARKER_COLORS.map(([c, label]) => (
              <Pressable key={c} onPress={() => { setColor(c); setHand(false); }} style={styles.colorBtn} accessibilityRole="radio" accessibilityState={{ selected: color === c && !hand }} accessibilityLabel={t(label)}>
                <View style={[styles.colorDot, { backgroundColor: c }, color === c && !hand && styles.colorOn]} />
              </Pressable>
            ))}
            <Pressable onPress={() => setHand((x) => !x)} style={styles.colorBtn} accessibilityRole="switch" accessibilityState={{ checked: hand }} accessibilityLabel={t('Verschieben')}>
              <Text style={[styles.barIcon, hand && styles.handOn]}>✋</Text>
            </Pressable>
            <Pressable onPress={() => setMarks((x) => x.slice(0, -1))} disabled={marks.length === 0} style={styles.colorBtn} accessibilityLabel={t('Rückgängig')}>
              <Text style={[styles.barIcon, marks.length === 0 && styles.dim]}>↶</Text>
            </Pressable>
            <View style={styles.grow} />
            <Pressable onPress={() => (marks.length ? setFlow('saveMark') : setMode('view'))} style={styles.doneBtn} accessibilityRole="button">
              <Text style={styles.doneText}>{t('Fertig')}</Text>
            </Pressable>
          </Animated.View>
        )}
        {mode === 'place' && (
          <Animated.View entering={m(popIn)} style={styles.bar}>
            <Pressable onPress={() => { setMode('view'); setPlace(null); }} style={styles.barBtn} accessibilityRole="button">
              <Text style={styles.barText}>{t('Abbrechen')}</Text>
            </Pressable>
            <Pressable onPress={() => setFlow('pad')} style={styles.barBtn} accessibilityRole="button">
              <Text style={styles.barText}>{t('Neu zeichnen')}</Text>
            </Pressable>
            <View style={styles.grow} />
            <Pressable onPress={() => setFlow('saveSign')} style={styles.doneBtn} accessibilityRole="button">
              <Text style={styles.doneText}>{t('Weiter')}</Text>
            </Pressable>
          </Animated.View>
        )}
        {mode === 'place' && <Text style={styles.placeHint}>{t('Unterschrift an die richtige Stelle ziehen, Ecke ziehen für die Größe.')}</Text>}

        {(flow === 'saveMark' || flow === 'saveSign' || flow === 'busy') && (
          <Animated.View entering={m(fadeIn)} style={styles.scrim}>
            <Pressable style={StyleSheet.absoluteFill} onPress={() => flow !== 'busy' && setFlow(null)} />
            <Animated.View entering={m(popIn)} style={styles.sheet}>
              {flow === 'busy' ? (
                <View style={styles.busy}>
                  <ActivityIndicator color="#9fbcff" />
                  <Text style={styles.sheetText}>{t('Wird gespeichert …')}</Text>
                </View>
              ) : (
                <>
                  <Text style={styles.sheetTitle}>{flow === 'saveSign' ? t('Signiertes PDF speichern') : t('Markierungen speichern')}</Text>
                  {(['copy', 'replace'] as const).map((md) => (
                    <Pressable
                      key={md}
                      onPress={() => {
                        if (flow === 'saveMark') {
                          saveMarks(md);
                        } else {
                          setSaveMode(md);
                          setWrongPw(false);
                          setFlow('password');
                        }
                      }}
                      style={styles.sheetRow}
                      accessibilityRole="button"
                    >
                      <Text style={styles.sheetText}>{md === 'copy' ? t('Als Kopie speichern') : t('Original ersetzen')}</Text>
                      <Text style={styles.sheetHint}>{md === 'copy' ? t('Original bleibt unverändert') : t('Original wandert in den Papierkorb')}</Text>
                    </Pressable>
                  ))}
                </>
              )}
            </Animated.View>
          </Animated.View>
        )}
        {flow === 'password' && <AskPassword wrong={wrongPw} busy={false} onSubmit={sign} onCancel={() => setFlow(null)} />}
        {flow === 'createId' && <CreateId onDone={() => continueSign(false).catch(() => setFlow(null))} onCancel={() => setFlow(null)} />}
        {flow === 'pad' && <SignaturePad onDone={beginPlace} onCancel={() => setFlow(null)} />}
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
  page: { backgroundColor: '#fff', borderRadius: 2, overflow: 'hidden' },
  error: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 24 },
  errorText: { color: '#eee', fontSize: 15, textAlign: 'center', marginBottom: 12 },
  errorBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: 'rgba(255,255,255,0.15)' },
  banner: { flexDirection: 'row', alignItems: 'center', gap: 10, paddingHorizontal: 14, paddingVertical: 8 },
  ok: { backgroundColor: '#1f4d33' },
  warn: { backgroundColor: '#5a4512' },
  bad: { backgroundColor: '#6b2121' },
  bannerIcon: { color: '#fff', fontSize: 16, fontWeight: '700', width: 18, textAlign: 'center' },
  bannerText: { color: '#fff', fontSize: 13, flex: 1 },
  bannerMore: { color: '#fff', fontSize: 13, fontWeight: '700' },
  bar: { flexDirection: 'row', alignItems: 'center', gap: 4, paddingHorizontal: 10, paddingTop: 6, paddingBottom: 18, backgroundColor: '#25262c' },
  barBtn: { minHeight: 52, paddingHorizontal: 14, justifyContent: 'center', alignItems: 'center', flexDirection: 'row', gap: 8 },
  barIcon: { color: '#fff', fontSize: 20 },
  barText: { color: '#f1f1f4', fontSize: 15 },
  colorBtn: { width: 44, height: 52, justifyContent: 'center', alignItems: 'center' },
  colorDot: { width: 28, height: 28, borderRadius: 14, borderWidth: 2, borderColor: 'transparent' },
  colorOn: { borderColor: '#fff', transform: [{ scale: 1.15 }] },
  handOn: { color: '#9fbcff' },
  dim: { opacity: 0.35 },
  grow: { flex: 1 },
  doneBtn: { minHeight: 44, paddingHorizontal: 18, borderRadius: 22, backgroundColor: '#9fbcff', justifyContent: 'center' },
  doneText: { color: '#0e0f12', fontSize: 15, fontWeight: '700' },
  placeHint: { position: 'absolute', left: 16, right: 16, bottom: 92, color: '#fff', fontSize: 13, textAlign: 'center', backgroundColor: 'rgba(0,0,0,0.55)', borderRadius: 12, padding: 8 },
  sigBox: { position: 'absolute', borderWidth: 1.5, borderColor: '#4f8cff', borderStyle: 'dashed', backgroundColor: 'rgba(79,140,255,0.08)' },
  sigHandle: { position: 'absolute', right: -16, bottom: -16, width: 36, height: 36, justifyContent: 'center', alignItems: 'center' },
  sigHandleDot: { width: 16, height: 16, borderRadius: 8, backgroundColor: '#4f8cff', borderWidth: 2, borderColor: '#fff' },
  scrim: { position: 'absolute', top: 0, left: 0, right: 0, bottom: 0, backgroundColor: 'rgba(0,0,0,0.5)', justifyContent: 'flex-end' },
  sheet: { backgroundColor: '#24262e', borderTopLeftRadius: 24, borderTopRightRadius: 24, paddingVertical: 14, paddingBottom: 28 },
  sheetTitle: { color: '#b4b6be', fontSize: 13, paddingHorizontal: 20, paddingBottom: 6 },
  sheetRow: { paddingHorizontal: 20, paddingVertical: 12, minHeight: 56 },
  sheetText: { color: '#f1f1f4', fontSize: 16 },
  sheetHint: { color: '#8a8f9c', fontSize: 13, marginTop: 2 },
  busy: { flexDirection: 'row', gap: 14, alignItems: 'center', padding: 20 },
});
