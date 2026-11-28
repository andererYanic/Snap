// Scharfstellen beim Zoomen: Sobald Zoom und Verschiebung zur Ruhe kommen, wird nur der sichtbare
// Ausschnitt in Bildschirmauflösung neu gezeichnet (Fotos: Ausschnitt-Dekodierung, PDF: Rendern
// des Bereichs) und über das unscharfe Bild gelegt. Das ist viel weniger Arbeit als das ganze
// Bild bzw. die ganze Seite in hoher Auflösung.
import { useEffect, useRef } from 'react';
import { PixelRatio } from 'react-native';
import { runOnJS, useAnimatedReaction, type SharedValue } from 'react-native-reanimated';

export type Box = { x: number; y: number; w: number; h: number };

/** Ab diesem Zoom lohnt ein scharfer Ausschnitt. */
export const SHARPEN_FROM = 1.3;
/** Wartezeit nach der letzten Bewegung, bevor gezeichnet wird. */
export const SETTLE_MS = 120;

/**
 * Sichtbarer Teil von `b` (Inhaltskoordinaten bei Zoom 1, Ursprung links oben) bei Zoom `s` und
 * Verschiebung `tx/ty` in einem Fenster `vw × vh`, mit `margin` (Anteil des Fensters) Zugabe,
 * damit kleines Verschieben scharf bleibt. `rel` relativ zu `b` (0…1), `content` in
 * Inhaltskoordinaten, `px` Zielbreite in Gerätepixeln. null, wenn nichts sichtbar ist.
 */
export function visibleRegion(s: number, tx: number, ty: number, b: Box, vw: number, vh: number, margin = 0.15, ratio = PixelRatio.get()) {
  if (!(s > 0 && b.w > 0 && b.h > 0)) {
    return null;
  }
  const mx = (vw * margin) / s;
  const my = (vh * margin) / s;
  const x0 = Math.max(b.x, -tx / s - mx);
  const y0 = Math.max(b.y, -ty / s - my);
  const x1 = Math.min(b.x + b.w, (vw - tx) / s + mx);
  const y1 = Math.min(b.y + b.h, (vh - ty) / s + my);
  if (x1 - x0 < 1 || y1 - y0 < 1) {
    return null;
  }
  const rel = { x: (x0 - b.x) / b.w, y: (y0 - b.y) / b.h, w: (x1 - x0) / b.w, h: (y1 - y0) / b.h };
  return {
    rel,
    content: { x: x0, y: y0, w: x1 - x0, h: y1 - y0 },
    px: Math.max(16, Math.min(4096, Math.round((x1 - x0) * s * ratio))),
  };
}

/**
 * Ruft `onSettled(s, tx, ty)` auf, wenn sich Zoom/Verschiebung `SETTLE_MS` lang nicht mehr
 * geändert haben (auch nach Schwung und Einrasten). Nur solange `enabled`.
 */
export function useSettled(
  s: SharedValue<number>,
  tx: SharedValue<number>,
  ty: SharedValue<number>,
  enabled: boolean,
  onSettled: (s: number, tx: number, ty: number) => void,
) {
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const cb = useRef(onSettled);
  cb.current = onSettled;
  const moved = (sv: number, x: number, y: number) => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => cb.current(sv, x, y), SETTLE_MS);
  };
  useAnimatedReaction(
    // Gerundet: nicht bei jedem Unterpixel an die Oberfläche melden
    () => (enabled ? [Math.round(s.value * 200), Math.round(tx.value / 2), Math.round(ty.value / 2)] : null),
    (cur, prev) => {
      if (cur && (!prev || cur[0] !== prev[0] || cur[1] !== prev[1] || cur[2] !== prev[2])) {
        runOnJS(moved)(s.value, tx.value, ty.value);
      }
    },
    [enabled],
  );
  useEffect(() => () => clearTimeout(timer.current), []);
}
