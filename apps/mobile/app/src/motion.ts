// Bewegung nur, wenn das System sie erlaubt. Bei „Animationen entfernen“ (Bedienungshilfen bzw.
// Animationsfaktor 0) blieben Einblend-Animationen sonst auf ihrem Startwert stehen – Inhalte
// wären unsichtbar. Dann werden Ein- und Ausblendungen ganz weggelassen.
import { AccessibilityInfo, Dimensions } from 'react-native';
import { Easing, FadeIn, FadeInDown, FadeOutDown, Keyframe } from 'react-native-reanimated';

// Werte aus dem Designlabor (apps/mobile/prototype, abgestimmt mit dem Nutzer am 2026-11-07).
// Kurve „iOS-artig“: klarer Start- und Endpunkt, kein Nachfedern.
export const EASE_OUT = Easing.bezier(0.32, 0.72, 0, 1);
export const EASE_IN = Easing.bezier(0.3, 0, 1, 1);

export const DUR = { pop: 180, popOut: 120, page: 140, fade: 150 };
/** Langes Drücken bis zur Auswahl (ms). */
export const LONG_PRESS = 190;

/** Aufklappen (Menü, „+“, Dialog): von 92 % auf 100 % Größe und eingeblendet. */
export const popIn = new Keyframe({
  0: { opacity: 0, transform: [{ scale: 0.92 }] },
  100: { opacity: 1, transform: [{ scale: 1 }], easing: EASE_OUT },
}).duration(DUR.pop);

/** Zuklappen: kurz und ohne Rückfedern. */
export const popOut = new Keyframe({
  0: { opacity: 1, transform: [{ scale: 1 }] },
  100: { opacity: 0, transform: [{ scale: 0.96 }], easing: EASE_IN },
}).duration(DUR.popOut);

/** Hinweis unten: gleitet herein bzw. hinaus. */
export const toastIn = FadeInDown.duration(DUR.page).easing(EASE_OUT);
export const toastOut = FadeOutDown.duration(DUR.popOut).easing(EASE_IN);

/** Einblenden ohne Bewegung. */
export const fadeIn = FadeIn.duration(DUR.fade).easing(EASE_OUT);

// Ordnerwechsel „Gleiten + Blende“: kommt um 30 % der Breite versetzt und blendet dabei auf.
const SHIFT = Math.round(Dimensions.get('window').width * 0.3);
const pageForward = new Keyframe({
  0: { opacity: 0, transform: [{ translateX: SHIFT }] },
  100: { opacity: 1, transform: [{ translateX: 0 }], easing: EASE_OUT },
}).duration(DUR.page);
const pageBack = new Keyframe({
  0: { opacity: 0, transform: [{ translateX: -SHIFT }] },
  100: { opacity: 1, transform: [{ translateX: 0 }], easing: EASE_OUT },
}).duration(DUR.page);
export const pageIn = (dir: 'forward' | 'back') => (dir === 'forward' ? pageForward : pageBack);

/** Blatt von unten (BottomSheet): gleitet herein, ohne Nachfedern. */
export const sheetIn = new Keyframe({
  0: { transform: [{ translateY: 400 }] },
  100: { transform: [{ translateY: 0 }], easing: EASE_OUT },
}).duration(DUR.page + 40);

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
