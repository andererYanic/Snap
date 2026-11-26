// Eingebaute PDF-Anzeige (wie bei Google Drive, aber lokal): Android zeichnet die Seiten selbst
// (`PdfRenderer`, Teil des Systems). Eigenes Scrollen und Zoomen auf dem UI-Thread (`zoom.ts`):
// zwei Finger zoomen live um den Punkt zwischen ihnen und verschieben gleichzeitig, Gummiband an
// den Grenzen, Doppeltippen zoomt auf die Stelle, Wischen läuft mit Schwung aus. Nach dem Loslassen
// werden die sichtbaren Seiten in der neuen Größe scharf nachgezeichnet. Dazu: Textmarker in Standardfarben und Signieren (sichtbare
// Unterschrift + kryptografische PAdES-Signatur, im Kern); beides wird Seiteninhalt und ist daher
// überall sichtbar, auch gedruckt. Beim Öffnen werden vorhandene Signaturen geprüft.
import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ActivityIndicator, Alert, Dimensions, Image, Modal, PixelRatio, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import { Text } from './text';
import { Gesture, GestureDetector, GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated, { cancelAnimation, runOnJS, useAnimatedReaction, useAnimatedStyle, useSharedValue, withDecay } from 'react-native-reanimated';
import { pageAt, range, rubber, rubberScale, settle, settleTarget } from './zoom';
import * as core from './core';
import { request } from './core';
import { t } from './i18n';
import { fadeIn, m, popIn } from './motion';
import { Icon } from './Icon';
import { DrawArea, INK_WIDTH, MARKER_COLORS, MARKER_WIDTH, type Stroke, Strokes } from './Ink';
import { AskPassword, CreateId, groupFingerprint, idInfo } from './SignId';
import SignaturePad, { PAD_RATIO } from './SignaturePad';
import { rectsToMarks, SelectionRects, TextLayer, type Rect, type TextSelection } from './PdfText';

const { width: W } = Dimensions.get('window');
const GAP = 8;
const PAD = 8;
const PAGE_W = W - 2 * PAD;
export const MIN_ZOOM = 1;
export const MAX_ZOOM = 5;

// Worklet: wird auch in Gesten auf dem UI-Thread aufgerufen (sonst Absturz „Remote Function“).
const clamp = (v: number, a: number, b: number) => {
  'worklet';
  return Math.max(a, Math.min(b, v));
};

/** Auflösungsstufe für eine Zoomstufe (nicht bei jedem Zwischenwert neu zeichnen). */
export function renderStep(scale: number): number {
  return [1, 1.5, 2, 3, 4, 5].find((st) => st >= scale - 0.01) ?? 5;
}

/** Obere Kanten der Seiten (Inhaltskoordinaten bei Zoom 1) und Gesamthöhe. */
export function layoutPages(ratios: number[]): { offsets: number[]; height: number } {
  const offsets: number[] = [];
  let y = PAD;
  for (const r of ratios) {
    offsets.push(y);
    y += PAGE_W * r + GAP;
  }
  return { offsets, height: y - GAP + PAD };
}

const PdfPage = memo(function PdfPage({ path, rev, index, ratio, scale }: { path: string; rev: number; index: number; ratio: number; scale: number }) {
  // Größe immer in Inhaltskoordinaten (Zoom 1); der Zoom ist eine Transformation darüber.
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
  const w = PAGE_W;
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

/** Markierung: Freihand-Strich oder (Text) Strich über einer Textzeile; `group` fasst einen Zug zusammen. */
type Mark = { page: number; color: string; stroke: Stroke; width?: number; group: number };
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
  // Zoom, für den die Seiten scharf gezeichnet sind (nach dem Loslassen übernommen)
  const [scale, setScale] = useState(1);
  const [shown, setShown] = useState<[number, number]>([0, 2]);
  const [mode, setMode] = useState<'view' | 'mark' | 'place'>('view');
  const [color, setColor] = useState(MARKER_COLORS[0][0]);
  const [marks, setMarks] = useState<Mark[]>([]);
  const [hand, setHand] = useState(false);
  const textOK = useMemo(() => core.pdfTextSupported(), []);
  const [tool, setTool] = useState<'text' | 'free'>(textOK ? 'text' : 'free');
  const [selection, setSelection] = useState<TextSelection | null>(null);
  const group = useRef(0);
  const noTextShown = useRef(false);
  const [sigs, setSigs] = useState<SignatureReport[]>([]);
  const [flow, setFlow] = useState<Flow>(null);
  const [sigStrokes, setSigStrokes] = useState<Stroke[]>([]);
  const [place, setPlace] = useState<Place | null>(null);
  const [saveMode, setSaveMode] = useState<'copy' | 'replace'>('copy');
  const [wrongPw, setWrongPw] = useState(false);
  // Zoom und Verschiebung des Inhalts (UI-Thread)
  const zs = useSharedValue(1);
  const tx = useSharedValue(0);
  const ty = useSharedValue(0);
  const vw = useSharedValue(W);
  const vh = useSharedValue(600);
  const contentH = useSharedValue(1);
  const offsetsSV = useSharedValue<number[]>([PAD]);
  const start0 = useSharedValue({ s: 1, x: 0, y: 0, px: 0, py: 0 });
  const lastShown = useSharedValue([0, 2, 0]);

  useEffect(() => {
    setInfo(null);
    core.pdfInfo(doc).then((i) => setInfo(i && i.pages > 0 ? i : 'error')).catch(() => setInfo('error'));
    request<{ items: SignatureReport[] }>({ PdfVerify: { path: doc } }).then((r) => setSigs(r.items ?? [])).catch(() => setSigs([]));
  }, [doc, rev]);

  const ok = info && info !== 'error' ? info : null;
  const lay = useMemo(() => (ok ? layoutPages(ok.ratios) : { offsets: [PAD], height: 1 }), [ok]);
  useEffect(() => {
    offsetsSV.value = lay.offsets;
    contentH.value = lay.height;
  }, [lay, offsetsSV, contentH]);
  // Neues Dokument: oben, ohne Zoom
  useEffect(() => {
    zs.value = 1;
    tx.value = 0;
    ty.value = 0;
    setScale(1);
  }, [doc, zs, tx, ty]);

  const drawing = mode === 'mark' && !hand;
  const listScroll = !drawing && mode !== 'place';

  const showRange = useCallback((a: number, b: number, center: number) => {
    setShown([a, b]);
    setPage(center);
  }, []);
  // Sichtbare Seiten aus Verschiebung und Zoom (nur bei Änderung an die Oberfläche melden)
  useAnimatedReaction(
    () => [ty.value, zs.value, vh.value] as const,
    ([y, sc, h]) => {
      const top = -y / sc;
      const bot = (h - y) / sc;
      const a = pageAt(offsetsSV.value, top);
      const b = pageAt(offsetsSV.value, bot);
      const c = pageAt(offsetsSV.value, (top + bot) / 2);
      const l = lastShown.value;
      if (l[0] !== a || l[1] !== b || l[2] !== c) {
        lastShown.value = [a, b, c];
        runOnJS(showRange)(a, b, c);
      }
    },
  );

  const pinch = Gesture.Pinch()
    .withTestId('pdf-pinch')
    .onStart((e) => {
      cancelAnimation(tx);
      cancelAnimation(ty);
      const s0 = zs.value;
      start0.value = { s: s0, x: tx.value, y: ty.value, px: (e.focalX - tx.value) / s0, py: (e.focalY - ty.value) / s0 };
    })
    .onUpdate((e) => {
      const st = start0.value;
      const ns = rubberScale(st.s * e.scale, MIN_ZOOM, MAX_ZOOM);
      zs.value = ns;
      tx.value = e.focalX - st.px * ns;
      ty.value = e.focalY - st.py * ns;
    })
    .onEnd((e) => {
      const to = settleTarget(zs.value, tx.value, ty.value, e.focalX, e.focalY, MIN_ZOOM, MAX_ZOOM, vw.value, vh.value, 0, 0, W, contentH.value, true);
      zs.value = settle(to.s);
      tx.value = settle(to.x);
      ty.value = settle(to.y);
      runOnJS(setScale)(to.s);
    });
  const pan = Gesture.Pan()
    .withTestId('pdf-pan')
    .enabled(listScroll)
    .maxPointers(1)
    .onBegin(() => {
      // Antippen hält auslaufendes Scrollen an
      cancelAnimation(tx);
      cancelAnimation(ty);
      start0.value = { s: zs.value, x: tx.value, y: ty.value, px: 0, py: 0 };
    })
    .onUpdate((e) => {
      const sc = zs.value;
      const [x0, x1] = range(sc, vw.value, 0, W);
      const [y0, y1] = range(sc, vh.value, 0, contentH.value, true);
      tx.value = rubber(start0.value.x + e.translationX, x0, x1);
      ty.value = rubber(start0.value.y + e.translationY, y0, y1);
    })
    .onEnd((e) => {
      const sc = zs.value;
      const [x0, x1] = range(sc, vw.value, 0, W);
      const [y0, y1] = range(sc, vh.value, 0, contentH.value, true);
      tx.value = withDecay({ velocity: e.velocityX, clamp: [x0, x1], rubberBandEffect: true });
      ty.value = withDecay({ velocity: e.velocityY, clamp: [y0, y1], rubberBandEffect: true });
    });
  const doubleTap = Gesture.Tap()
    .numberOfTaps(2)
    .enabled(mode === 'view')
    .onEnd((e) => {
      const sc = zs.value;
      const target = sc > 1.2 ? 1 : 2.5;
      const px = (e.x - tx.value) / sc;
      const py = (e.y - ty.value) / sc;
      const to = settleTarget(target, e.x - px * target, e.y - py * target, e.x, e.y, MIN_ZOOM, MAX_ZOOM, vw.value, vh.value, 0, 0, W, contentH.value, true);
      zs.value = settle(to.s);
      tx.value = settle(to.x);
      ty.value = settle(to.y);
      runOnJS(setScale)(to.s);
    });
  const gestures = Gesture.Simultaneous(pinch, pan, doubleTap);
  const contentStyle = useAnimatedStyle(() => ({
    transform: [{ translateX: tx.value }, { translateY: ty.value }, { scale: zs.value }],
  }));

  const pageW = PAGE_W;
  const summary = useMemo(() => signatureSummary(sigs), [sigs]);

  /** Markierte Textzeilen (Rechtecke) als Marker-Striche übernehmen. */
  const addTextMarks = useCallback((pg: number, rects: Rect[], c: string) => {
    if (!ok) {
      return;
    }
    const g = ++group.current;
    const add = rectsToMarks(rects, ok.ratios[pg]).map((x) => ({ page: pg, color: c, stroke: x.stroke, width: x.width, group: g }));
    setMarks((m0) => [...m0, ...add]);
  }, [ok]);
  const noText = useCallback(() => {
    if (!noTextShown.current) {
      noTextShown.current = true;
      notify?.(t('Auf dieser Seite gibt es keinen auswählbaren Text (z. B. eingescannt). Nutze den Freihand-Marker.'));
    }
  }, [notify]);
  const undoMark = () => setMarks((x) => {
    const last = x[x.length - 1];
    return last ? x.filter((mk) => mk.group !== last.group) : x;
  });

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
    const byKey = new Map<string, { page: number; color: string; width: number; strokes: Stroke[] }>();
    for (const mk of marks) {
      const w = mk.width ?? MARKER_WIDTH;
      const k = `${mk.page}|${mk.color}|${w}`;
      const e = byKey.get(k) ?? { page: mk.page, color: mk.color, width: w, strokes: [] };
      e.strokes.push(mk.stroke);
      byKey.set(k, e);
    }
    const layers = Array.from(byKey.values());
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
    if (selection) {
      setSelection(null);
    } else if (flow && flow !== 'busy') {
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
            <Icon name="back" color="#fff" />
          </Pressable>
          <View style={styles.titleBox}>
            <Text style={styles.title} numberOfLines={1}>{docName}</Text>
            {ok && <Text style={styles.counter}>{t('Seite {n} von {total}', { n: page + 1, total: ok.pages })}{scale > 1.01 ? `  ·  ${Math.round(scale * 100)} %` : ''}</Text>}
          </View>
          <Pressable onPress={() => core.printFile(doc).catch(() => {})} style={styles.btn} accessibilityLabel={t('Drucken')}>
            <Icon name="print" size={22} color="#fff" />
          </Pressable>
          <Pressable onPress={() => core.openFile(doc).catch(() => {})} style={styles.btn} accessibilityLabel={t('Öffnen mit …')}>
            <Icon name="more" size={22} color="#fff" strokeWidth={2.4} />
          </Pressable>
        </View>
        {summary && (
          <Pressable onPress={showDetails} style={[styles.banner, styles[summary.kind]]} accessibilityRole="button" accessibilityLabel={summary.text}>
            <Icon name={summary.kind === 'ok' ? 'shieldCheck' : 'warning'} size={20} color={summary.kind === 'ok' ? '#8FD8B5' : summary.kind === 'warn' ? '#E9C46A' : '#F2B8B5'} />
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
            <Animated.View
              style={[styles.fill, styles.clip]}
              collapsable={false}
              onLayout={(e) => {
                vw.value = e.nativeEvent.layout.width;
                vh.value = e.nativeEvent.layout.height;
              }}
              accessibilityHint={t('Mit zwei Fingern zoomen, doppelt tippen für 250 %')}
            >
              <Animated.View style={[styles.content, { height: lay.height }, contentStyle]}>
                {Array.from({ length: info.pages }, (_, i) => i)
                  .filter((i) => i >= shown[0] - 1 && i <= shown[1] + 1)
                  .map((i) => {
                    const h = pageW * info.ratios[i];
                    const pageMarks = marks.filter((mk) => mk.page === i);
                    const groups = Array.from(new Set(pageMarks.map((mk) => `${mk.color}|${mk.width ?? MARKER_WIDTH}`)));
                    return (
                      <View key={`${doc}|${i}`} style={[styles.pageSlot, { top: lay.offsets[i], width: pageW, height: h }]}>
                        <PdfPage path={doc} rev={rev} index={i} ratio={info.ratios[i]} scale={scale} />
                        {groups.map((gk) => {
                          const [c, w] = gk.split('|');
                          return (
                            <Strokes key={gk} strokes={pageMarks.filter((mk) => `${mk.color}|${mk.width ?? MARKER_WIDTH}` === gk).map((mk) => mk.stroke)} width={pageW} height={h} color={c} strokeWidth={Number(w) * pageW} opacity={0.45} />
                          );
                        })}
                        {selection && selection.page === i && <SelectionRects rects={selection.rects} width={pageW} height={h} />}
                        {mode === 'view' && textOK && (
                          <TextLayer
                            path={doc}
                            page={i}
                            width={pageW}
                            height={h}
                            mode="select"
                            color={color}
                            testID={`select-page-${i}`}
                            onSelect={setSelection}
                          />
                        )}
                        {drawing && tool === 'text' && (
                          <TextLayer
                            path={doc}
                            page={i}
                            width={pageW}
                            height={h}
                            mode="mark"
                            color={color}
                            testID={`textmark-page-${i}`}
                            onMark={(rects) => addTextMarks(i, rects, color)}
                            onNoText={noText}
                          />
                        )}
                        {drawing && tool === 'free' && (
                          <DrawArea
                            width={pageW}
                            height={h}
                            color={color}
                            strokeWidth={MARKER_WIDTH * pageW}
                            opacity={0.45}
                            testID={`mark-page-${i}`}
                            onStroke={(stroke) => setMarks((x) => [...x, { page: i, color, stroke, group: ++group.current }])}
                          />
                        )}
                        {mode === 'place' && place && place.page === i && (
                          <SignatureBox place={place} strokes={sigStrokes} pageW={pageW} pageH={h} onChange={setPlace} />
                        )}
                      </View>
                    );
                  })}
              </Animated.View>
            </Animated.View>
          </GestureDetector>
        )}

        {ok && mode === 'view' && selection && (
          <Animated.View entering={m(popIn)} style={styles.bar}>
            <Pressable
              onPress={() => {
                core.copyText(selection.text).then(() => notify?.(t('Text kopiert.'))).catch(() => {});
                setSelection(null);
              }}
              style={styles.barBtn}
              accessibilityRole="button"
              accessibilityLabel={t('Kopieren')}
            >
              <Icon name="copy" size={22} color="#EDEBE6" />
              <Text style={styles.barText}>{t('Kopieren')}</Text>
            </Pressable>
            <View style={styles.grow} />
            {MARKER_COLORS.map(([c, label]) => (
              <Pressable
                key={c}
                onPress={() => {
                  addTextMarks(selection.page, selection.rects, c);
                  setColor(c);
                  setTool('text');
                  setHand(false);
                  setSelection(null);
                  setMode('mark');
                }}
                style={styles.colorBtn}
                accessibilityRole="button"
                accessibilityLabel={t('Markieren: {color}', { color: t(label) })}
              >
                <View style={[styles.colorDot, { backgroundColor: c }]} />
              </Pressable>
            ))}
            <Pressable onPress={() => setSelection(null)} style={styles.colorBtn} accessibilityLabel={t('Auswahl aufheben')}>
              <Icon name="close" size={22} color="#EDEBE6" />
            </Pressable>
          </Animated.View>
        )}
        {ok && mode === 'view' && !selection && (
          <View style={styles.bar}>
            <Pressable onPress={() => setMode('mark')} style={styles.barBtn} accessibilityRole="button" accessibilityLabel={t('Markieren')}>
              <Icon name="highlight" size={22} color="#EDEBE6" />
              <Text style={styles.barText}>{t('Markieren')}</Text>
            </Pressable>
            <Pressable onPress={startSign} style={styles.barBtn} accessibilityRole="button" accessibilityLabel={t('Signieren')}>
              <Icon name="sign" size={22} color="#EDEBE6" />
              <Text style={styles.barText}>{t('Signieren')}</Text>
            </Pressable>
          </View>
        )}
        {mode === 'mark' && (
          <Animated.View entering={m(popIn)} style={styles.bar}>
            {textOK && (
              <View style={styles.tools} accessibilityRole="radiogroup">
                <Pressable onPress={() => { setTool('text'); setHand(false); }} style={[styles.toolBtn, tool === 'text' && !hand && styles.toolOn]} accessibilityRole="radio" accessibilityState={{ selected: tool === 'text' && !hand }} accessibilityLabel={t('Text markieren')}>
                  <Icon name="font" size={20} color={tool === 'text' && !hand ? '#0B1F1A' : '#EDEBE6'} />
                </Pressable>
                <Pressable onPress={() => { setTool('free'); setHand(false); }} style={[styles.toolBtn, tool === 'free' && !hand && styles.toolOn]} accessibilityRole="radio" accessibilityState={{ selected: tool === 'free' && !hand }} accessibilityLabel={t('Freihand markieren')}>
                  <Icon name="highlight" size={20} color={tool === 'free' && !hand ? '#0B1F1A' : '#EDEBE6'} />
                </Pressable>
              </View>
            )}
            {MARKER_COLORS.map(([c, label]) => (
              <Pressable key={c} onPress={() => { setColor(c); setHand(false); }} style={styles.colorBtn} accessibilityRole="radio" accessibilityState={{ selected: color === c && !hand }} accessibilityLabel={t(label)}>
                <View style={[styles.colorDot, { backgroundColor: c }, color === c && !hand && styles.colorOn]} />
              </Pressable>
            ))}
            <Pressable onPress={() => setHand((x) => !x)} style={styles.colorBtn} accessibilityRole="switch" accessibilityState={{ checked: hand }} accessibilityLabel={t('Verschieben')}>
              <Icon name="hand" size={22} color={hand ? '#72C7B2' : '#EDEBE6'} />
            </Pressable>
            <Pressable onPress={undoMark} disabled={marks.length === 0} style={styles.colorBtn} accessibilityLabel={t('Rückgängig')}>
              <View style={marks.length === 0 && styles.dim}>
                <Icon name="undo" size={22} color="#EDEBE6" />
              </View>
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
                  <ActivityIndicator color="#72C7B2" />
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
  root: { flex: 1, backgroundColor: '#161615' },
  fill: { flex: 1 },
  top: { flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 8, paddingBottom: 8, backgroundColor: '#1F1F1D' },
  btn: { minWidth: 48, height: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 8 },
  btnText: { color: '#fff', fontSize: 22 },
  small: { color: '#fff', fontSize: 15, fontWeight: '600' },
  titleBox: { flex: 1, marginHorizontal: 4 },
  title: { color: '#fff', fontSize: 15, fontWeight: '600' },
  counter: { color: '#bbb', fontSize: 12 },
  clip: { overflow: 'hidden' },
  content: { position: 'absolute', left: 0, top: 0, width: W, transformOrigin: 'left top' },
  pageSlot: { position: 'absolute', left: PAD },
  page: { backgroundColor: '#fff', borderRadius: 2, overflow: 'hidden' },
  error: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 24 },
  errorText: { color: '#eee', fontSize: 15, textAlign: 'center', marginBottom: 12 },
  errorBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: 'rgba(255,255,255,0.15)' },
  banner: { flexDirection: 'row', alignItems: 'center', gap: 10, paddingHorizontal: 14, paddingVertical: 8 },
  ok: { backgroundColor: '#1C2E27' },
  warn: { backgroundColor: '#332B17' },
  bad: { backgroundColor: '#3A1F1D' },
  bannerIcon: { color: '#fff', fontSize: 16, fontWeight: '700', width: 18, textAlign: 'center' },
  bannerText: { color: '#fff', fontSize: 13, flex: 1 },
  bannerMore: { color: '#fff', fontSize: 13, fontWeight: '700' },
  bar: { flexDirection: 'row', alignItems: 'center', gap: 4, paddingHorizontal: 10, paddingTop: 6, paddingBottom: 18, backgroundColor: '#1F1F1D' },
  barBtn: { minHeight: 52, paddingHorizontal: 14, justifyContent: 'center', alignItems: 'center', flexDirection: 'row', gap: 8 },
  barIcon: { color: '#fff', fontSize: 20 },
  barText: { color: '#f1f1f4', fontSize: 15 },
  colorBtn: { width: 44, height: 52, justifyContent: 'center', alignItems: 'center' },
  tools: { flexDirection: 'row', padding: 3, borderRadius: 22, backgroundColor: '#2B2B28', marginRight: 4 },
  toolBtn: { width: 40, height: 36, borderRadius: 18, justifyContent: 'center', alignItems: 'center' },
  toolOn: { backgroundColor: '#72C7B2' },
  colorDot: { width: 28, height: 28, borderRadius: 14, borderWidth: 2, borderColor: 'transparent' },
  colorOn: { borderColor: '#fff', transform: [{ scale: 1.15 }] },
  handOn: { color: '#9fbcff' },
  dim: { opacity: 0.35 },
  grow: { flex: 1 },
  doneBtn: { minHeight: 44, paddingHorizontal: 20, borderRadius: 22, backgroundColor: '#72C7B2', justifyContent: 'center' },
  doneText: { color: '#0B1F1A', fontSize: 15, fontWeight: '700' },
  placeHint: { position: 'absolute', left: 16, right: 16, bottom: 92, color: '#fff', fontSize: 13, textAlign: 'center', backgroundColor: 'rgba(0,0,0,0.55)', borderRadius: 12, padding: 8 },
  sigBox: { position: 'absolute', borderWidth: 1.5, borderColor: '#1F6B5C', borderStyle: 'dashed', backgroundColor: 'rgba(31,107,92,0.06)' },
  sigHandle: { position: 'absolute', right: -16, bottom: -16, width: 36, height: 36, justifyContent: 'center', alignItems: 'center' },
  sigHandleDot: { width: 16, height: 16, borderRadius: 8, backgroundColor: '#1F6B5C', borderWidth: 2, borderColor: '#fff' },
  scrim: { position: 'absolute', top: 0, left: 0, right: 0, bottom: 0, backgroundColor: 'rgba(0,0,0,0.5)', justifyContent: 'flex-end' },
  sheet: { backgroundColor: '#1F1F1D', borderTopLeftRadius: 24, borderTopRightRadius: 24, paddingVertical: 14, paddingBottom: 28 },
  sheetTitle: { color: '#b4b6be', fontSize: 13, paddingHorizontal: 20, paddingBottom: 6 },
  sheetRow: { paddingHorizontal: 20, paddingVertical: 12, minHeight: 56 },
  sheetText: { color: '#f1f1f4', fontSize: 16 },
  sheetHint: { color: '#8C8A83', fontSize: 13, marginTop: 2 },
  busy: { flexDirection: 'row', gap: 14, alignItems: 'center', padding: 20 },
});
