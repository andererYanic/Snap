// Snap-Optik (ruhiges Papier, eine Akzentfarbe „Tinte“, siehe docs/DESIGN.md) oder – wahlweise –
// Material You mit Farben aus dem Hintergrundbild (Android 12+, PlatformColor).
import { PlatformColor, type ColorValue } from 'react-native';

export interface Theme {
  dark: boolean;
  bg: ColorValue;
  /** Veraltet (früher Verlauf): gleich `bg`. */
  bgTint: ColorValue;
  /** Fläche für Listen-Gruppen und Blätter (früher „card“). */
  card: ColorValue;
  surface: ColorValue;
  /** Felder, gedrückte Zustände, Platzhalter. */
  surface2: ColorValue;
  text: ColorValue;
  text2: ColorValue;
  text3: ColorValue;
  accent: ColorValue;
  accentSoft: ColorValue;
  /** Haarlinien (früher „stroke“). */
  stroke: ColorValue;
  line: ColorValue;
  danger: ColorValue;
  warn: ColorValue;
  ok: ColorValue;
  folder: ColorValue;
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
  const c = dark
    ? {
        bg: '#121211', surface: '#1C1C1A', surface2: '#272725',
        text: '#EDEBE6', text2: '#A3A19A', text3: '#6F6D67', line: 'rgba(237,235,230,0.09)',
        accent: '#72C7B2', accentSoft: 'rgba(114,199,178,0.16)', onAccent: '#0B1F1A',
        danger: '#F2B8B5', warn: '#E9C46A', ok: '#7FD19B', folder: '#C99A3C',
      }
    : {
        bg: '#F4F2ED', surface: '#FFFFFF', surface2: '#EAE7E0',
        text: '#1A1A18', text2: '#66655F', text3: '#9C9A92', line: 'rgba(26,26,24,0.09)',
        accent: '#1F6B5C', accentSoft: 'rgba(31,107,92,0.12)', onAccent: '#FFFFFF',
        danger: '#B3261E', warn: '#8A5A00', ok: '#2E7D4F', folder: '#E2B04A',
      };
  return { dark, ...c, bgTint: c.bg, card: c.surface, stroke: c.line };
}

/** Material You: Systemfarben des Geräts (nur Android 12+). */
function makeMaterial(dark: boolean): Theme {
  const c = (name: string) => PlatformColor(`@android:color/${name}`);
  const bg = c(dark ? 'system_neutral1_900' : 'system_neutral1_50');
  const surface = c(dark ? 'system_neutral2_800' : 'system_neutral1_10');
  const line = c(dark ? 'system_neutral2_700' : 'system_neutral2_100');
  return {
    dark,
    bg,
    bgTint: bg,
    card: surface,
    surface,
    surface2: c(dark ? 'system_neutral2_700' : 'system_neutral2_100'),
    text: c(dark ? 'system_neutral1_50' : 'system_neutral1_900'),
    text2: c(dark ? 'system_neutral2_200' : 'system_neutral2_700'),
    text3: c(dark ? 'system_neutral2_400' : 'system_neutral2_500'),
    accent: c(dark ? 'system_accent1_200' : 'system_accent1_600'),
    accentSoft: c(dark ? 'system_accent1_800' : 'system_accent1_100'),
    stroke: line,
    line,
    danger: dark ? '#ffb4ab' : '#ba1a1a',
    warn: dark ? '#E9C46A' : '#8A5A00',
    ok: dark ? '#7FD19B' : '#2E7D4F',
    folder: dark ? '#C99A3C' : '#E2B04A',
    onAccent: c(dark ? 'system_accent1_900' : 'system_accent1_0'),
  };
}
