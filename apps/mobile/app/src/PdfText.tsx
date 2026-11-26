// Text in PDF-Seiten auswählen und markieren (Textschicht über Android, `PdfPages.kt`).
// - „mark“: Ziehen markiert Text vom Startpunkt bis zum Finger (rastet an Buchstaben ein) – wie
//   der Textmarker in PDF-Apps. Loslassen übernimmt die Markierung.
// - „select“: Lange drücken wählt das Wort; weiterziehen erweitert die Auswahl. Danach erscheinen
//   Aktionen (Kopieren, Markieren).
// Abfragen laufen gedrosselt (höchstens eine gleichzeitig, immer mit dem neuesten Punkt).
import { useCallback, useRef, useState } from 'react';
import { StyleSheet, View } from 'react-native';
import { Gesture, GestureDetector } from 'react-native-gesture-handler';
import { runOnJS } from 'react-native-reanimated';
import * as core from './core';
import type { Stroke } from './Ink';

export type Rect = number[]; // [x, y, w, h], normiert auf die Seite
export interface TextSelection {
  page: number;
  text: string;
  rects: Rect[];
}

/**
 * Rechtecke (normiert) → Textmarker-Striche. `ratio` = Seitenhöhe/Seitenbreite. Die Strichbreite ist
 * relativ zur Seitenbreite (wie beim Freihand-Marker); die runden Enden reichen genau bis an den Rand.
 */
export function rectsToMarks(rects: Rect[], ratio: number): { width: number; stroke: Stroke }[] {
  const out: { width: number; stroke: Stroke }[] = [];
  for (const [x, y, w, h] of rects) {
    if (!(w > 0 && h > 0)) {
      continue;
    }
    const width = Math.min(0.2, Math.max(0.0005, h * ratio));
    const cy = y + h / 2;
    const half = width / 2;
    const a = x + half;
    const b = x + w - half;
    out.push({ width: Math.round(width * 10000) / 10000, stroke: b > a ? [[a, cy], [b, cy]] : [[x + w / 2, cy]] });
  }
  return out;
}

export function TextLayer({ path, page, width, height, mode, color, enabled = true, onMark, onSelect, onNoText, testID }: {
  path: string;
  page: number;
  width: number;
  height: number;
  mode: 'mark' | 'select';
  color: string;
  enabled?: boolean;
  onMark?: (rects: Rect[]) => void;
  onSelect?: (sel: TextSelection) => void;
  /** Seite hat keine Textschicht (z. B. eingescannt). */
  onNoText?: () => void;
  testID?: string;
}) {
  const [live, setLive] = useState<Rect[]>([]);
  const start = useRef<[number, number]>([0, 0]);
  const latest = useRef<[number, number] | null>(null);
  /** Letzter bekannter Fingerpunkt (bleibt nach dem Abfragen erhalten). */
  const lastPoint = useRef<[number, number]>([0, 0]);
  const busy = useRef(false);
  const result = useRef<{ text: string; rects: Rect[] } | null>(null);
  const active = useRef(false);

  const query = useCallback(async () => {
    if (busy.current || !latest.current) {
      return;
    }
    busy.current = true;
    const [x, y] = latest.current;
    latest.current = null;
    const [sx, sy] = start.current;
    try {
      const r = await core.pdfSelect(path, page, sx, sy, x, y);
      if (active.current) {
        result.current = r;
        setLive(r?.rects ?? []);
      }
    } finally {
      busy.current = false;
      if (latest.current && active.current) {
        query();
      }
    }
  }, [path, page]);

  const norm = useCallback((x: number, y: number): [number, number] => [Math.max(0, Math.min(1, x / width)), Math.max(0, Math.min(1, y / height))], [width, height]);

  const begin = useCallback((x: number, y: number) => {
    active.current = true;
    result.current = null;
    start.current = norm(x, y);
    latest.current = start.current;
    lastPoint.current = start.current;
    if (mode === 'select') {
      core.haptic('select');
    }
    query();
  }, [norm, mode, query]);

  const move = useCallback((x: number, y: number) => {
    latest.current = norm(x, y);
    lastPoint.current = latest.current;
    query();
  }, [norm, query]);

  const end = useCallback(async (ok: boolean) => {
    if (!ok) {
      active.current = false;
      setLive([]);
      return;
    }
    // Immer mit dem letzten Fingerpunkt abschließen (eine laufende Abfrage kann älter sein)
    active.current = false;
    latest.current = null;
    const [sx, sy] = start.current;
    const [lx, ly] = lastPoint.current;
    result.current = await core.pdfSelect(path, page, sx, sy, lx, ly);
    const r = result.current;
    setLive([]);
    if (!r || r.rects.length === 0) {
      onNoText?.();
      return;
    }
    if (mode === 'mark') {
      onMark?.(r.rects);
    } else {
      onSelect?.({ page, text: r.text, rects: r.rects });
    }
  }, [path, page, mode, onMark, onSelect, onNoText]);

  let pan = Gesture.Pan().enabled(enabled).maxPointers(1);
  pan = mode === 'mark' ? pan.minDistance(0) : pan.activateAfterLongPress(380);
  pan = pan
    .onStart((e) => runOnJS(begin)(e.x, e.y))
    .onUpdate((e) => runOnJS(move)(e.x, e.y))
    .onFinalize((_e, ok) => runOnJS(end)(ok));
  if (testID) {
    pan.withTestId(testID);
  }

  return (
    <GestureDetector gesture={pan}>
      <View style={[styles.area, { width, height }]} collapsable={false}>
        {live.map(([x, y, w, h], i) => (
          <View
            key={i}
            pointerEvents="none"
            style={[styles.rect, { left: x * width, top: y * height, width: w * width, height: h * height, backgroundColor: mode === 'mark' ? color : '#72C7B2' }]}
          />
        ))}
      </View>
    </GestureDetector>
  );
}

/** Anzeige einer bestehenden Auswahl (ohne Eingabe). */
export function SelectionRects({ rects, width, height }: { rects: Rect[]; width: number; height: number }) {
  return (
    <View pointerEvents="none" style={[styles.area, { width, height }]}>
      {rects.map(([x, y, w, h], i) => (
        <View key={i} style={[styles.rect, styles.sel, { left: x * width, top: y * height, width: w * width, height: h * height }]} />
      ))}
    </View>
  );
}

const styles = StyleSheet.create({
  area: { position: 'absolute', left: 0, top: 0 },
  rect: { position: 'absolute', opacity: 0.45, borderRadius: 2 },
  sel: { backgroundColor: '#72C7B2', opacity: 0.4 },
});
