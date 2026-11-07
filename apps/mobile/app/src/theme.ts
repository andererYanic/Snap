// Snap-Optik (wie Desktop: weicher Farbverlauf, durchscheinende Karten) oder – wahlweise –
// Material You mit Farben aus dem Hintergrundbild (Android 12+, PlatformColor).
import { PlatformColor, type ColorValue } from 'react-native';

export interface Theme {
  dark: boolean;
  bg: ColorValue;
  bgTint: ColorValue;
  card: ColorValue;
  text: ColorValue;
  text2: ColorValue;
  accent: ColorValue;
  accentSoft: ColorValue;
  stroke: ColorValue;
  danger: ColorValue;
  onAccent: ColorValue;
}

const cache = new Map<string, Theme>();
/** Jedes Thema nur einmal erzeugen: gleiche Objekte über alle Renderläufe (wichtig für Memos). */
function once(key: string, make: () => Theme): Theme {
  let t = cache.get(key);
  if (!t) {
    t = make();
    cache.set(key, t);
  }
  return t;
}

export function snapTheme(dark: boolean): Theme {
  return once(`snap-${dark}`, () => makeSnap(dark));
}

export function materialTheme(dark: boolean): Theme {
  return once(`material-${dark}`, () => makeMaterial(dark));
}

function makeSnap(dark: boolean): Theme {
  return dark
    ? {
        dark,
        bg: '#1b2131',
        bgTint: '#2a1f33',
        card: 'rgba(44,46,56,0.78)',
        text: '#f1f1f4',
        text2: '#b4b6be',
        accent: '#7aa2ff',
        accentSoft: 'rgba(122,162,255,0.18)',
        stroke: 'rgba(255,255,255,0.08)',
        danger: '#ff6b81',
        onAccent: '#0d1220',
      }
    : {
        dark,
        bg: '#dfe6f5',
        bgTint: '#ead9f2',
        card: 'rgba(255,255,255,0.78)',
        text: '#1b1b1f',
        text2: '#5d5f66',
        accent: '#3b6fe0',
        accentSoft: 'rgba(59,111,224,0.14)',
        stroke: 'rgba(0,0,0,0.08)',
        danger: '#c4314b',
        onAccent: '#ffffff',
      };
}

/** Material You: Systemfarben des Geräts (nur Android 12+). */
function makeMaterial(dark: boolean): Theme {
  const c = (name: string) => PlatformColor(`@android:color/${name}`);
  return {
    dark,
    bg: c(dark ? 'system_neutral1_900' : 'system_neutral1_50'),
    bgTint: c(dark ? 'system_accent2_900' : 'system_accent2_50'),
    card: c(dark ? 'system_neutral2_800' : 'system_neutral2_10'),
    text: c(dark ? 'system_neutral1_50' : 'system_neutral1_900'),
    text2: c(dark ? 'system_neutral2_200' : 'system_neutral2_700'),
    accent: c(dark ? 'system_accent1_200' : 'system_accent1_600'),
    accentSoft: c(dark ? 'system_accent1_800' : 'system_accent1_100'),
    stroke: c(dark ? 'system_neutral2_700' : 'system_neutral2_100'),
    danger: dark ? '#ffb4ab' : '#ba1a1a',
    onAccent: c(dark ? 'system_accent1_900' : 'system_accent1_0'),
  };
}
