// Zugriff auf den eingebetteten Snap-Kern (Kotlin-Modul → Rust).
// Anfragen im JSON-Format des Desktop-Dienstes; Antworten ohne Varianten-Hülle.
import { NativeEventEmitter, NativeModules } from 'react-native';

/** Das native Modul (beim Aufruf gelesen, nicht beim Import). */
const native = () => NativeModules.SnapCore;

export class CoreError extends Error {
  constructor(public code: string, message: string) {
    super(message);
  }
}

export async function start(): Promise<number> {
  return native().start();
}

/** Sendet eine Anfrage; `{"DirListing": {...}}` → `{...}`, Fehler werfen `CoreError`. */
export async function request<T = any>(req: unknown): Promise<T> {
  const raw: string = await native().request(JSON.stringify(req));
  const res = JSON.parse(raw);
  if (typeof res === 'string') {
    return res as T;
  }
  const [kind, body] = Object.entries(res)[0] as [string, any];
  if (kind === 'Error') {
    throw new CoreError(body.code, body.message);
  }
  return body as T;
}

export function onEvent(cb: (ev: { type: string; [k: string]: any }) => void) {
  const emitter = new NativeEventEmitter(native());
  const sub = emitter.addListener('snap-event', (arg: unknown) => {
    const json = String(arg);
    try {
      cb(JSON.parse(json));
    } catch {
      // ungültiges Ereignis ignorieren
    }
  });
  return () => sub.remove();
}

export const lastCrash = (): Promise<string | null> => native().lastCrash();
export const openFile = (path: string): Promise<void> => native().openFile(path);
export const shareFiles = (paths: string[]): Promise<void> => native().shareFiles(paths);
export const storageInfo = (): Promise<{ total: number; free: number }> => native().storageInfo();
/** Vorschaubild über Android (schnell, mit Cache). `null`, wenn keins möglich ist. */
export const thumbnail = (path: string, edge: number): Promise<string | null> => native().thumbnail(path, edge);
/** Kurze Vibration: `select` (Markieren), `tick` (Auswahl erweitert), `confirm`. */
/** PDF über Android (`PdfRenderer`): Seitenzahl und Seitenverhältnisse (Höhe/Breite). */
export const pdfInfo = (path: string): Promise<{ pages: number; ratios: number[] } | null> => native().pdfInfo(path);
/** PDF-Seite als PNG-Datei in der gewünschten Breite (Pixel). */
export const pdfPage = (path: string, index: number, width: number): Promise<string | null> => native().pdfPage(path, index, width);
/** Druckdialog des Systems (PDF unverändert, Bilder auf eine Seite eingepasst). */
export const printFile = (path: string): Promise<void> => native().printFile(path);
/** Textdatei für die Anzeige lesen (begrenzt; `truncated`, wenn die Datei länger ist). */
export const readText = (path: string, maxBytes = 2 * 1024 * 1024): Promise<{ text: string; truncated: boolean }> =>
  native().readText(path, maxBytes);
/** Ordner für Bilder aus Word-Dateien (App-Cache). */
export const docxImageDir = (): Promise<string> => native().docxImageDir();
/** Schrift registrieren (beim Start); Name der eigenen Schrift oder null (= Inconsolata). */
export const applyFont = (family = 'SnapFont'): Promise<string | null> => native().applyFont?.(family) ?? Promise.resolve(null);
/** Eigene Schrift aus einer Datei bzw. über den Auswahldialog; Ergebnis: Name oder null (abgebrochen). */
export const setFontFromFile = (path: string): Promise<string | null> => native().setFontFromFile(path);
export const pickFont = (): Promise<string | null> => native().pickFont();
export const resetFont = (): Promise<void> => native().resetFont();
/** Bildbearbeitung (Android): Parameter wie in `ImageEditor`. */
export interface EditParams {
  rot: number;
  crop: { x: number; y: number; w: number; h: number } | null;
  light: number;
  contrast: number;
  sat: number;
  warm: number;
  filter: string;
}
export const editPreview = (path: string, p: EditParams, edge: number, withCrop = false): Promise<string> =>
  native().editPreview(path, JSON.stringify(p), edge, withCrop);
export const editSave = (path: string, p: EditParams, mode: 'copy' | 'temp'): Promise<string> => native().editSave(path, JSON.stringify(p), mode);
export const haptic = (kind: 'select' | 'tick' | 'confirm' = 'select'): void => native().haptic?.(kind);
export const hasAllFilesAccess = (): Promise<boolean> => native().hasAllFilesAccess();
export const requestAllFilesAccess = (): void => native().requestAllFilesAccess();
export const storageRoot = (): Promise<string> => native().storageRoot();
export const getPref = (k: string): Promise<string | null> => native().getPref(k);
export const setPref = (k: string, v: string): void => native().setPref(k, v);
export const sdkInt = (): Promise<number> => native().sdkInt();

// ------------------------------------------------------------ Typen (wie Desktop)

export interface DirEntry {
  name: string;
  kind: 'File' | 'Dir' | 'Symlink';
  size: number;
  mtime: number;
  hidden: boolean;
}

export interface TimelineItem {
  path: string;
  taken: number;
  video: boolean;
  size: number;
}

const numFmt = new Map<string, Intl.NumberFormat>();
const dateFmt = new Map<string, Intl.DateTimeFormat>();
function fmt<T>(m: Map<string, T>, key: string, make: () => T): T {
  let f = m.get(key);
  if (!f) {
    f = make();
    m.set(key, f);
  }
  return f;
}

/** Datum kurz (z. B. 7.11.2026), mit zwischengespeichertem Format. */
export function shortDate(unixSecs: number, loc: string, utc = false): string {
  const f = fmt(dateFmt, `${loc}|${utc}`, () => new Intl.DateTimeFormat(loc, utc ? { timeZone: 'UTC' } : undefined));
  return f.format(new Date(unixSecs * 1000));
}

/** Monat und Jahr (z. B. „November 2026“). */
export function monthYear(unixSecs: number, loc: string): string {
  const f = fmt(dateFmt, `${loc}|my`, () => new Intl.DateTimeFormat(loc, { month: 'long', year: 'numeric', timeZone: 'UTC' }));
  return f.format(new Date(unixSecs * 1000));
}

export function humanSize(n: number, loc: string): string {
  if (n < 1024) {
    return `${n} B`;
  }
  const u = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  const digits = v < 10 ? 1 : 0;
  const f = fmt(numFmt, `${loc}|${digits}`, () => new Intl.NumberFormat(loc, { maximumFractionDigits: digits }));
  return `${f.format(v)} ${u[i]}`;
}

export const joinPath = (dir: string, name: string) => (dir.endsWith('/') ? dir + name : `${dir}/${name}`);

export function parentOf(path: string, root: string): string | null {
  if (path === root || path === '/') {
    return null;
  }
  const i = path.replace(/\/+$/, '').lastIndexOf('/');
  return i <= 0 ? '/' : path.slice(0, i);
}
