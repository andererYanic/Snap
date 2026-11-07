// Übersetzungen – dieselben Dateien wie die Desktop-App (apps/desktop/locales/*.json).
// Deutsch ist die Ausgangssprache: der deutsche Text ist der Schlüssel.
import { NativeModules } from 'react-native';

interface LocaleFile {
  name: string;
  locale: string;
  strings: Record<string, string>;
}

// Neue Sprachen hier eintragen (Metro kennt keinen Verzeichnis-Import).
const FILES: Record<string, LocaleFile> = {
  en: require('../../../desktop/locales/en.json'),
};

export const LANGUAGES: Record<string, LocaleFile> = {
  de: { name: 'Deutsch', locale: 'de-DE', strings: {} },
  ...FILES,
};

let current = 'de';

function systemLanguage(): string {
  const i = (Intl.DateTimeFormat().resolvedOptions().locale || 'de').toLowerCase();
  return LANGUAGES[i] ? i : LANGUAGES[i.split('-')[0]] ? i.split('-')[0] : 'de';
}

/** Gespeicherte Wahl laden, sonst Systemsprache. */
export async function initLanguage(): Promise<void> {
  const saved: string | null = await NativeModules.SnapCore.getPref('lang');
  current = saved && LANGUAGES[saved] ? saved : systemLanguage();
}

export function lang(): string {
  return current;
}

export function locale(): string {
  return LANGUAGES[current].locale;
}

export function setLanguage(code: string) {
  current = LANGUAGES[code] ? code : 'de';
  NativeModules.SnapCore.setPref('lang', current);
}

export function t(de: string, vars?: Record<string, string | number>): string {
  let s = LANGUAGES[current].strings[de] ?? de;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      s = s.split(`{${k}}`).join(String(v));
    }
  }
  return s;
}
