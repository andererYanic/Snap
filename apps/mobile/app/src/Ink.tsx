// Striche zeichnen und anzeigen (Textmarker, Unterschrift). Punkte sind normiert (0..1) auf die
// Fläche, in der gezeichnet wird; angezeigt wird mit SVG, gespeichert im Kern als PDF-Inhalt.
import { useCallback, useRef, useState } from 'react';
import { type StyleProp, StyleSheet, View, type ViewStyle } from 'react-native';
import { Gesture, GestureDetector } from 'react-native-gesture-handler';
import { runOnJS } from 'react-native-reanimated';
import Svg, { Path } from 'react-native-svg';

export type Point = [number, number];
export type Stroke = Point[];

/** Standardfarben des Textmarkers (wie Leuchtstifte). */
export const MARKER_COLORS: [string, string][] = [
  ['#ffe14d', 'Gelb'],
  ['#8be08b', 'Grün'],
  ['#8ccfff', 'Blau'],
  ['#ff9ccf', 'Rosa'],
];
/** Breite des Textmarkers relativ zur Seitenbreite (≈ 13 pt auf A4). */
export const MARKER_WIDTH = 0.022;
/** Breite der Unterschrift relativ zur Seitenbreite. */
export const INK_WIDTH = 0.004;

/** SVG-Pfad aus normierten Punkten für eine Fläche `w` × `h`. */
export function pathOf(points: Stroke, w: number, h: number): string {
  if (points.length === 0) {
    return '';
  }
  const [x0, y0] = points[0];
  let d = `M${(x0 * w).toFixed(1)} ${(y0 * h).toFixed(1)}`;
  if (points.length === 1) {
    d += ` L${(x0 * w + 0.1).toFixed(1)} ${(y0 * h).toFixed(1)}`;
  }
  for (let i = 1; i < points.length; i++) {
    d += ` L${(points[i][0] * w).toFixed(1)} ${(points[i][1] * h).toFixed(1)}`;
  }
  return d;
}

/** Punkte ausdünnen: nur übernehmen, wenn sich der Finger merklich bewegt hat. */
export function addPoint(s: Stroke, p: Point, min = 0.002): Stroke {
  const last = s[s.length - 1];
  if (last && Math.abs(last[0] - p[0]) < min && Math.abs(last[1] - p[1]) < min) {
    return s;
  }
  return [...s, p];
}

const clamp01 = (v: number) => Math.max(0, Math.min(1, v));

/** Anzeige von Strichen (ohne Eingabe). */
export function Strokes({ strokes, width, height, color, strokeWidth, opacity = 1 }: {
  strokes: Stroke[]; width: number; height: number; color: string; strokeWidth: number; opacity?: number;
}) {
  return (
    <Svg width={width} height={height} style={StyleSheet.absoluteFill} pointerEvents="none">
      {strokes.map((s, i) => (
        <Path key={i} d={pathOf(s, width, height)} stroke={color} strokeWidth={strokeWidth} strokeOpacity={opacity} strokeLinecap="round" strokeLinejoin="round" fill="none" />
      ))}
    </Svg>
  );
}

/**
 * Zeichenfläche: ein Finger zeichnet, fertige Striche gehen an `onStroke`. Der laufende Strich
 * wird live angezeigt.
 */
export function DrawArea({ width, height, color, strokeWidth, opacity = 1, onStroke, style, enabled = true, testID }: {
  width: number; height: number; color: string; strokeWidth: number; opacity?: number;
  onStroke: (s: Stroke) => void; style?: StyleProp<ViewStyle>; enabled?: boolean; testID?: string;
}) {
  const [live, setLive] = useState<Stroke>([]);
  const cur = useRef<Stroke>([]);
  const begin = useCallback((x: number, y: number) => {
    cur.current = [[clamp01(x / width), clamp01(y / height)]];
    setLive(cur.current);
  }, [width, height]);
  const move = useCallback((x: number, y: number) => {
    const next = addPoint(cur.current, [clamp01(x / width), clamp01(y / height)]);
    if (next !== cur.current) {
      cur.current = next;
      setLive(next);
    }
  }, [width, height]);
  const end = useCallback(() => {
    if (cur.current.length > 0) {
      onStroke(cur.current);
    }
    cur.current = [];
    setLive([]);
  }, [onStroke]);
  const pan = Gesture.Pan()
    .enabled(enabled)
    .minDistance(0)
    .maxPointers(1)
    .onBegin((e) => runOnJS(begin)(e.x, e.y))
    .onUpdate((e) => runOnJS(move)(e.x, e.y))
    .onFinalize(() => runOnJS(end)());
  if (testID) {
    pan.withTestId(testID);
  }
  return (
    <GestureDetector gesture={pan}>
      <View style={[{ width, height }, styles.area, style]} collapsable={false}>
        {live.length > 0 && <Strokes strokes={[live]} width={width} height={height} color={color} strokeWidth={strokeWidth} opacity={opacity} />}
      </View>
    </GestureDetector>
  );
}

const styles = StyleSheet.create({
  area: { position: 'absolute', left: 0, top: 0 },
});
