// Typisierte Aufrufe der Rust-Befehle (siehe src-tauri/src/commands.rs).
// Es gibt bewusst keinen allgemeinen Aufruf – jede Funktion entspricht genau einem Befehl.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type NodeKind = "File" | "Dir" | "Symlink";

export interface DirEntry {
  name: string;
  kind: NodeKind;
  size: number;
  mtime: number;
  hidden: boolean;
}

export interface DirListing {
  path: string;
  entries: DirEntry[];
  total: number;
}

export interface SearchHit {
  path: string;
  name: string;
  kind: NodeKind;
  size: number;
  mtime: number;
}

export interface Place {
  kind: string;
  name: string;
  path: string;
}

export interface TrashEntry {
  id: string;
  name: string;
  original_path: string;
  deleted_at: string;
  is_dir: boolean;
  size: number;
}

export interface TimelineItem {
  path: string;
  taken: number;
  exif: boolean;
  video: boolean;
  size: number;
  width: number | null;
  height: number | null;
  camera: string | null;
}

export interface TimelinePage {
  items: TimelineItem[];
  next: string | null;
}

export interface MonthCount {
  year: number;
  month: number;
  count: number;
}

export interface UiError {
  code: string;
  message: string;
}

export type DaemonEvent =
  | { type: "FileOpProgress"; job: number; done_files: number; total_files: number; done_bytes: number; total_bytes: number }
  | { type: "FileOpFinished"; job: number; ok: boolean; message: string }
  | { type: string; [k: string]: unknown };

export function errorText(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String((e as UiError).message);
  return String(e);
}

export function isCancelled(e: unknown): boolean {
  return !!e && typeof e === "object" && (e as UiError).code === "Cancelled";
}

export const api = {
  places: () => invoke<Place[]>("places"),
  listDir: (path: string, showHidden: boolean, offset = 0) =>
    invoke<DirListing>("list_dir", { path, showHidden, offset }),
  search: (query: string, includeHidden: boolean) =>
    invoke<{ hits: SearchHit[] }>("search", { query, includeHidden }),
  metadata: (path: string) => invoke<Record<string, unknown>>("metadata", { path }),
  copy: (sources: string[], dest: string) => invoke<{ job: number }>("copy_items", { sources, dest }),
  move: (sources: string[], dest: string) => invoke<{ job: number }>("move_items", { sources, dest }),
  rename: (path: string, newName: string) => invoke<{ job: number }>("rename", { path, newName }),
  mkdir: (parent: string, name: string) => invoke<{ job: number }>("mkdir", { parent, name }),
  trash: (items: string[]) => invoke<{ job: number }>("trash", { items }),
  deletePermanently: (items: string[]) => invoke<{ job: number }>("delete_permanently", { items }),
  undo: () => invoke<{ message: string }>("undo"),
  trashList: () => invoke<{ entries: TrashEntry[] }>("trash_list"),
  trashRestore: (id: string) => invoke<unknown>("trash_restore", { id }),
  trashEmpty: () => invoke<unknown>("trash_empty"),
  open: (path: string) => invoke<void>("open_path", { path }),
  startDaemon: () => invoke<void>("start_daemon"),
  streamUrl: (path: string) => invoke<{ url: string }>("stream_url", { path }),
  timeline: (path: string | null, before: string | null) =>
    invoke<TimelinePage>("timeline", { path, before }),
  timelineMonths: (path: string | null) => invoke<{ months: MonthCount[] }>("timeline_months", { path }),
};

export function onDaemonEvent(cb: (e: DaemonEvent) => void): Promise<UnlistenFn> {
  return listen<DaemonEvent>("daemon-event", (ev) => cb(ev.payload));
}

export function onDaemonStatus(cb: (up: boolean) => void): Promise<UnlistenFn> {
  return listen<boolean>("daemon-status", (ev) => cb(ev.payload));
}

/** URL eines Vorschaubilds (über das eigene `thumb:`-Protokoll der App). */
export function thumbUrl(path: string, large = false): string {
  const enc = path.split("/").map(encodeURIComponent).join("/");
  // Linux/macOS: thumb://localhost/…; Windows: http://thumb.localhost/…
  const base = navigator.userAgent.includes("Windows") ? "http://thumb.localhost" : "thumb://localhost";
  return `${base}${enc}${large ? "?large" : ""}`;
}

// ---------------------------------------------------------------- Hilfen für die Anzeige

export function humanSize(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toLocaleString("de-DE", { maximumFractionDigits: v < 10 ? 1 : 0 })} ${u[i]}`;
}

export function formatDate(unix: number): string {
  if (!unix) return "";
  return new Date(unix * 1000).toLocaleString("de-DE", {
    day: "2-digit",
    month: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function joinPath(dir: string, name: string): string {
  return dir.endsWith("/") ? dir + name : `${dir}/${name}`;
}

export function parentOf(path: string): string | null {
  if (path === "/") return null;
  const i = path.replace(/\/+$/, "").lastIndexOf("/");
  return i <= 0 ? "/" : path.slice(0, i);
}

export function baseName(path: string): string {
  const p = path.replace(/\/+$/, "");
  return p.slice(p.lastIndexOf("/") + 1) || "/";
}

const IMAGE = ["jpg", "jpeg", "png", "gif", "webp", "heic", "heif", "bmp", "tif", "tiff", "avif"];
const VIDEO = ["mp4", "mkv", "mov", "webm", "avi", "m4v", "3gp"];
export function extOf(name: string): string {
  const i = name.lastIndexOf(".");
  return i > 0 ? name.slice(i + 1).toLowerCase() : "";
}
export const isImage = (name: string) => IMAGE.includes(extOf(name));
export const isVideo = (name: string) => VIDEO.includes(extOf(name));
