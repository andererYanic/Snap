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
/** Aus dem „Teilen“-Menü anderer Apps empfangene Dateien bzw. Text (einmalig abholen). */
export const takeShare = (): Promise<{ files: string[]; text: string | null } | null> => native().takeShare?.() ?? Promise.resolve(null);
/** Neu Geteiltes, während die App läuft. */
export function onShare(cb: () => void) {
  const sub = new NativeEventEmitter(native()).addListener('snap-share', () => cb());
  return () => sub.remove();
}
export interface ApkInfo {
  label: string;
  package: string;
  version: string;
  versionCode: number;
  installedVersion?: string;
  installedCode?: number;
  self: boolean;
  icon?: string;
  /** Zahl der APKs bei App-Paketen (.apks, .xapk, .apkm). */
  splits?: number;
  /** Paket enthält Zusatzdaten (OBB), die nicht übertragen werden. */
  obb?: boolean;
}
/** App-Infos aus einer APK (ohne Installation). */
export const apkInfo = (path: string): Promise<ApkInfo | null> => native().apkInfo?.(path) ?? Promise.resolve(null);
/**
 * APK an den System-Installer übergeben; `"permission"`, wenn erst die Erlaubnis geöffnet wurde.
 * App-Pakete laufen über eine Installer-Sitzung und enden mit `"installed"` oder `"aborted"`.
 */
export const installApk = (path: string): Promise<'started' | 'permission' | 'installed' | 'aborted'> => native().installApk(path);
/** Web-Link mit der Standard-App öffnen (nur http/https). */
export const openLink = (url: string): Promise<void> => native().openLink(url);
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
/** PDF-Textschicht über Android (ab 15, oder 11–14 mit Systemupdate). */
export const pdfTextSupported = (): boolean => !!native().pdfTextSupported?.();
/** Hat die Seite Text (eingescannte Seiten nicht)? */
export const pdfHasText = (path: string, index: number): Promise<boolean> => native().pdfHasText?.(path, index) ?? Promise.resolve(false);
/** Text zwischen zwei Punkten (normiert, 0..1) auswählen; gleiche Punkte = Wort. */
export const pdfSelect = (path: string, index: number, x1: number, y1: number, x2: number, y2: number): Promise<{ text: string; rects: number[][] } | null> =>
  native().pdfSelect?.(path, index, x1, y1, x2, y2) ?? Promise.resolve(null);
export const copyText = (text: string): Promise<void> => native().copyText(text);
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
export interface VideoParams {
  startMs: number;
  endMs: number;
  mute: boolean;
  /** Drehung im Uhrzeigersinn (0, 90, 180, 270). */
  rot: number;
}
export interface VideoInfo {
  durationMs: number;
  width: number;
  height: number;
  rotation: number;
  audio: boolean;
}
export const videoInfo = async (path: string): Promise<VideoInfo> => JSON.parse(await native().videoInfo(path));
export const videoFrames = async (path: string, count: number, edge: number): Promise<(string | null)[]> =>
  JSON.parse(await native().videoFrames(path, count, edge));
export const videoSave = (path: string, p: VideoParams, mode: 'copy' | 'temp'): Promise<string> => native().videoSave(path, JSON.stringify(p), mode);
export const videoProgress = (): Promise<number> => native().videoProgress();
export const videoCancel = (): void => native().videoCancel();
export const cacheDir = (): Promise<string> => native().cacheDir();
/** Geöffnete Dateien anderer Geräte aus dem Cache löschen (nach dem Entkoppeln). */
export const clearRemoteCache = (): Promise<void> => native().clearRemoteCache?.() ?? Promise.resolve();
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

/**
 * Kurzes, menschliches Datum für Listen: heute „14:30“, gestern `yesterday`, in den letzten
 * sechs Tagen der Wochentag („Mo.“), im selben Jahr „3. Nov.“, sonst „3.11.2024“.
 */
export function relativeDate(unixSecs: number, loc: string, yesterday: string, nowSecs = Date.now() / 1000): string {
  const d = new Date(unixSecs * 1000);
  const now = new Date(nowSecs * 1000);
  const day = (x: Date) => Math.floor((x.getTime() - x.getTimezoneOffset() * 60000) / 86400000);
  const diff = day(now) - day(d);
  if (diff === 0) {
    return fmt(dateFmt, `${loc}|hm`, () => new Intl.DateTimeFormat(loc, { hour: '2-digit', minute: '2-digit' })).format(d);
  }
  if (diff === 1) {
    return yesterday;
  }
  if (diff > 1 && diff < 7) {
    return fmt(dateFmt, `${loc}|wd`, () => new Intl.DateTimeFormat(loc, { weekday: 'short' })).format(d);
  }
  if (d.getFullYear() === now.getFullYear() && diff >= 0) {
    return fmt(dateFmt, `${loc}|dm`, () => new Intl.DateTimeFormat(loc, { day: 'numeric', month: 'short' })).format(d);
  }
  return fmt(dateFmt, `${loc}|dmy`, () => new Intl.DateTimeFormat(loc, { day: 'numeric', month: 'numeric', year: 'numeric' })).format(d);
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
