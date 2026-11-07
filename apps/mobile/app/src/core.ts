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
  return `${v.toLocaleString(loc, { maximumFractionDigits: v < 10 ? 1 : 0 })} ${u[i]}`;
}

export const joinPath = (dir: string, name: string) => (dir.endsWith('/') ? dir + name : `${dir}/${name}`);

export function parentOf(path: string, root: string): string | null {
  if (path === root || path === '/') {
    return null;
  }
  const i = path.replace(/\/+$/, '').lastIndexOf('/');
  return i <= 0 ? '/' : path.slice(0, i);
}
