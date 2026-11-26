// Gemeinsame Rechnung für Zoomen und Verschieben (Bilder und PDF), wie in Foto- und PDF-Apps:
// - Der Inhalt folgt den Fingern live; der Punkt zwischen den Fingern bleibt unter ihnen stehen,
//   zwei Finger verschieben dabei gleichzeitig.
// - Über die Grenzen hinaus gibt es spürbaren Widerstand (Gummiband) und beim Loslassen ein
//   weiches Zurückfedern.
// - Doppeltippen zoomt animiert auf die getippte Stelle; erneut doppeltippen zoomt zurück.
// - Verschieben läuft mit Schwung aus und bremst an den Rändern weich ab.
// Alle Funktionen laufen als Worklet auf dem UI-Thread (keine Verzögerung über JavaScript).
import { Easing, withTiming } from 'react-native-reanimated';

export const ZOOM_EASE = Easing.bezier(0.2, 0.9, 0.25, 1);
export const ZOOM_MS = 260;

/** Weiche Bewegung ohne Nachfedern für das Einrasten nach einer Geste. */
export const settle = (v: number) => {
  'worklet';
  return withTiming(v, { duration: ZOOM_MS, easing: ZOOM_EASE });
};

export const clampV = (v: number, a: number, b: number) => {
  'worklet';
  return Math.max(a, Math.min(b, v));
};

/**
 * Erlaubter Bereich der Verschiebung auf einer Achse. Inhalt `cLen` beginnt bei `cOff`
 * (Inhaltskoordinaten, Zoom 1); Ausschnitt `view`. Ist der Inhalt kleiner als der Ausschnitt, wird
 * er zentriert (bzw. oben angelegt, `top`).
 */
export function range(s: number, view: number, cOff: number, cLen: number, top = false): [number, number] {
  'worklet';
  const len = s * cLen;
  if (len <= view) {
    const c = top ? 0 - s * cOff : (view - len) / 2 - s * cOff;
    return [c, c];
  }
  return [view - s * (cOff + cLen), 0 - s * cOff];
}

/** Gummiband: über die Grenzen hinaus nur noch ein Teil der Bewegung. */
export function rubber(v: number, min: number, max: number, k = 0.35): number {
  'worklet';
  if (v < min) {
    return min - (min - v) * k;
  }
  if (v > max) {
    return max + (v - max) * k;
  }
  return v;
}

/** Gummiband für den Zoom: über das Maximum bzw. unter das Minimum mit wachsendem Widerstand. */
export function rubberScale(raw: number, min: number, max: number): number {
  'worklet';
  if (raw > max) {
    return max * Math.pow(raw / max, 0.3);
  }
  if (raw < min) {
    return min * Math.pow(raw / min, 0.45);
  }
  return raw;
}

/**
 * Ziel nach dem Loslassen: Zoom in die Grenzen, Verschiebung so, dass der Inhaltspunkt unter dem
 * letzten Fingerpunkt bleibt, dann in den erlaubten Bereich.
 */
export function settleTarget(
  s: number, tx: number, ty: number, fx: number, fy: number,
  min: number, max: number, vw: number, vh: number,
  cx: number, cy: number, cw: number, ch: number, top = false,
): { s: number; x: number; y: number } {
  'worklet';
  const ns = clampV(s, min, max);
  const px = (fx - tx) / s;
  const py = (fy - ty) / s;
  const [x0, x1] = range(ns, vw, cx, cw);
  const [y0, y1] = range(ns, vh, cy, ch, top);
  return { s: ns, x: clampV(fx - px * ns, x0, x1), y: clampV(fy - py * ns, y0, y1) };
}

/** Index der Seite, in der `y` (Inhaltskoordinaten) liegt; `offsets` aufsteigend. */
export function pageAt(offsets: number[], y: number): number {
  'worklet';
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid] <= y) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  return Math.max(0, lo);
}

/** Laufender Zwei-Finger-Zoom: Bezugspunkt (Inhaltskoordinaten) und letzter gültiger Fingerpunkt. */
export interface PinchState {
  s: number;
  px: number;
  py: number;
  /** `scale` der Geste zum Zeitpunkt des Bezugs. */
  base: number;
  /** Fingerzahl beim Bezug. */
  n: number;
  fx: number;
  fy: number;
}

/** Neuer Bezug: Der Inhaltspunkt unter (`fx`|`fy`) bleibt ab jetzt unter dem Fingerpunkt. */
export function pinchBase(s: number, x: number, y: number, fx: number, fy: number, scale: number, n: number): PinchState {
  'worklet';
  return { s, px: (fx - x) / s, py: (fy - y) / s, base: scale || 1, n, fx, fy };
}

/**
 * Ein Schritt des Zooms. Ändert sich die Zahl der Finger (z. B. einer wird zuerst gehoben), springt
 * der Mittelpunkt der Finger – dann wird neu bezogen statt das Bild mitzureißen. Mit weniger als
 * zwei Fingern bleibt alles stehen.
 */
export function pinchStep(
  st: PinchState, cur: { s: number; x: number; y: number },
  scale: number, fx: number, fy: number, n: number, min: number, max: number,
): { st: PinchState; s: number; x: number; y: number } {
  'worklet';
  if (n < 2) {
    return { st, s: cur.s, x: cur.x, y: cur.y };
  }
  if (n !== st.n) {
    return { st: pinchBase(cur.s, cur.x, cur.y, fx, fy, scale, n), s: cur.s, x: cur.x, y: cur.y };
  }
  const ns = rubberScale(st.s * (scale / st.base), min, max);
  return { st: { ...st, fx, fy }, s: ns, x: fx - st.px * ns, y: fy - st.py * ns };
}
