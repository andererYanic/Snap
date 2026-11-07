// Bewegung nur, wenn das System sie erlaubt. Bei „Animationen entfernen“ (Bedienungshilfen bzw.
// Animationsfaktor 0) blieben Einblend-Animationen sonst auf ihrem Startwert stehen – Inhalte
// wären unsichtbar. Dann werden Ein- und Ausblendungen ganz weggelassen.
import { AccessibilityInfo } from 'react-native';

let reduced = false;

/** Einmal beim Start aufrufen; folgt danach Änderungen der Systemeinstellung. */
export async function initMotion(): Promise<void> {
  try {
    reduced = await AccessibilityInfo.isReduceMotionEnabled();
    AccessibilityInfo.addEventListener('reduceMotionChanged', (v) => {
      reduced = v;
    });
  } catch {
    reduced = false;
  }
}

/** Animation oder – bei reduzierter Bewegung – keine. */
export function m<T>(animation: T): T | undefined {
  return reduced ? undefined : animation;
}
