// Übersetzungen. Ausgangssprache ist Deutsch: Der deutsche Text im Code ist zugleich der Schlüssel
// (wie bei gettext). Jede Datei in `../locales/*.json` ist eine Sprache und wird automatisch
// gefunden – eine neue Sprache braucht keine Codeänderung (siehe docs/TRANSLATING.md).

export interface LocaleFile {
  /** Name der Sprache in ihr selbst, z. B. „English“. */
  name: string;
  /** BCP-47-Tag für Datums- und Zahlenformate, z. B. „en-GB“. */
  locale: string;
  /** Deutscher Text → Übersetzung. Platzhalter wie `{n}` bleiben erhalten. */
  strings: Record<string, string>;
}

const files = import.meta.glob("../locales/*.json", { eager: true, import: "default" }) as Record<string, LocaleFile>;

/** Verfügbare Sprachen: Kürzel (Dateiname) → Datei. Deutsch ist immer dabei. */
export const LANGUAGES: Record<string, LocaleFile> = { de: { name: "Deutsch", locale: "de-DE", strings: {} } };
for (const [path, f] of Object.entries(files)) {
  const code = path.slice(path.lastIndexOf("/") + 1, -".json".length);
  if (code !== "de" && f && typeof f.strings === "object") LANGUAGES[code] = f;
}

function detect(): string {
  const saved = localStorage.getItem("lang");
  if (saved && LANGUAGES[saved]) return saved;
  for (const l of navigator.languages ?? [navigator.language]) {
    const code = l.toLowerCase();
    if (LANGUAGES[code]) return code;
    const base = code.split("-")[0];
    if (LANGUAGES[base]) return base;
  }
  return "de";
}

export const lang = detect();
const current = LANGUAGES[lang];
/** Gebietsschema für Datum, Zahlen, Sortierung. */
export const locale = current.locale;

/** Übersetzt `de` (deutscher Text) und setzt `{platzhalter}` ein. Fehlt eine Übersetzung: Deutsch. */
export function t(de: string, vars?: Record<string, string | number>): string {
  let s = current.strings[de] ?? de;
  if (vars) for (const [k, v] of Object.entries(vars)) s = s.split(`{${k}}`).join(String(v));
  return s;
}

/** Sprache wechseln: merken und neu laden (einfach und ohne halb übersetzte Zustände). */
export function setLanguage(code: string) {
  localStorage.setItem("lang", code);
  location.reload();
}
