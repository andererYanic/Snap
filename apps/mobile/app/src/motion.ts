// Bewegung nur, wenn das System sie erlaubt. Bei „Animationen entfernen“ (Bedienungshilfen bzw.
// Animationsfaktor 0) blieben Einblend-Animationen sonst auf ihrem Startwert stehen – Inhalte
// wären unsichtbar. Dann werden Ein- und Ausblendungen ganz weggelassen.
import { AccessibilityInfo } from 'react-native';
import { Easing, FadeInDown, FadeOutDown, Keyframe } from 'react-native-reanimated';

// Einheitliche Kurven: klarer Start- und Endpunkt, kein Nachfedern.
export const EASE_OUT = Easing.bezier(0.2, 0, 0, 1);
export const EASE_IN = Easing.bezier(0.3, 0, 1, 1);

/** Aufklappen (Menü, „+“, Dialog): von 92 % auf 100 % Größe und eingeblendet. */
export const popIn = new Keyframe({
  0: { opacity: 0, transform: [{ scale: 0.92 }] },
  100: { opacity: 1, transform: [{ scale: 1 }], easing: EASE_OUT },
}).duration(180);

/** Zuklappen: kurz und ohne Rückfedern. */
export const popOut = new Keyframe({
  0: { opacity: 1, transform: [{ scale: 1 }] },
  100: { opacity: 0, transform: [{ scale: 0.96 }], easing: EASE_IN },
}).duration(120);

/** Hinweis unten: gleitet herein bzw. hinaus. */
export const toastIn = FadeInDown.duration(220).easing(EASE_OUT);
export const toastOut = FadeOutDown.duration(150).easing(EASE_IN);

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
