// Typisierte Aufrufe der Rust-Befehle (siehe src-tauri/src/commands.rs).
// Es gibt bewusst keinen allgemeinen Aufruf – jede Funktion entspricht genau einem Befehl.
import { invoke } from "@tauri-apps/api/core";
import { locale } from "./i18n";
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
  createFile: (parent: string, name: string) => invoke<{ job: number }>("create_file", { parent, name }),
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
  return `${v.toLocaleString(locale, { maximumFractionDigits: v < 10 ? 1 : 0 })} ${u[i]}`;
}

export function formatDate(unix: number): string {
  if (!unix) return "";
  return new Date(unix * 1000).toLocaleString(locale, {
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

// ---------------------------------------------------------------- 3b: Geräte, Sicherungen, Einstellungen

export interface Grant { capability: string; folder: string | null; granted_at: number }
/** Gemeldeter Speicher eines Geräts (Summen über die Laufwerke seiner Freigaben). */
export interface DeviceStorage { total: number; free: number; updated_at: number }
export interface Device { node_id: string; name: string; paired_at: number; last_sync_at: number | null; grants: Grant[]; online?: boolean; storage?: DeviceStorage }
export interface Peer { node_id: string; status: string; last_seen: number | null }
export interface Approval { id: number; node_id: string; summary?: string; [k: string]: unknown }
export interface BackupJob {
  id: string;
  target: { kind: "Dir"; path: string } | { kind: "S3" };
  sources: string[];
  last_run: number | null;
  last_ok: number | null;
  last_error: string | null;
  last_snapshot: string | null;
  running: boolean;
}
export interface Snapshot { id: string; time: number; host: string; files: number; bytes: number; new_bytes: number; sources: { path: string; tree: string }[] }
export interface Setting { key: string; value: number; default: number; overridden: boolean; unit: string; min: number; max: number; help: string }

export const api3 = {
  devices: () => invoke<{ devices: Device[] }>("devices"),
  peers: () => invoke<{ peers: Peer[] }>("peers"),
  pairInvite: () => invoke<{ code: string; expires_at: number }>("pair_invite"),
  pairInviteWait: () => invoke<{ device: Device }>("pair_invite_wait"),
  pairJoin: (code: string) => invoke<{ device: Device }>("pair_join", { code }),
  pairRemove: (device: string, name: string) => invoke<unknown>("pair_remove", { device, name }),
  pairGrant: (device: string, name: string, capability: string, folder: string | null) =>
    invoke<unknown>("pair_grant", { device, name, capability, folder }),
  pairRevoke: (device: string, capability: string, folder: string | null) =>
    invoke<unknown>("pair_revoke", { device, capability, folder }),
  opsPending: () => invoke<{ items: Approval[] }>("ops_pending"),
  opsApprove: (id: number, summary: string) => invoke<unknown>("ops_approve", { id, summary }),
  opsReject: (id: number) => invoke<unknown>("ops_reject", { id }),
  backups: () => invoke<{ jobs: BackupJob[] }>("backups"),
  backupCreate: (target: string | null, sources: string[], password: string) =>
    invoke<{ id: string; recovery_key: string }>("backup_create", { target, sources, password }),
  backupRun: (id: string) => invoke<Snapshot>("backup_run", { id }),
  backupSnapshots: (id: string) => invoke<{ items: Snapshot[] }>("backup_snapshots", { id }),
  backupRestore: (id: string, snapshot: string, path: string, dest: string) =>
    invoke<{ path: string; files: number }>("backup_restore", { id, snapshot, path, dest }),
  backupRemove: (id: string) => invoke<unknown>("backup_remove", { id }),
  settings: () => invoke<{ items: Setting[] }>("settings"),
  settingSet: (key: string, value: number | null) => invoke<unknown>("setting_set", { key, value }),
};

// ---------------------------------------------------------------- 5: Links, Speicheranalyse, Schrift

export interface LinkInfo {
  id: string;
  url: string;
  name: string;
  size: number;
  created_at: number;
  expires_at: number;
  max_downloads: number | null;
  password: boolean;
  state: "active" | "revoked" | "expired" | string;
}
export interface UsageEntry { name: string; is_dir: boolean; size: number; files: number }
export interface TypeUsage { category: string; size: number; files: number }
export interface SpaceUsage { path: string; total: number; files: number; children: UsageEntry[]; by_type: TypeUsage[]; largest: { path: string; size: number }[] }
export interface DuplicateGroup { hash: string; size: number; paths: string[]; wasted: number; other_devices: string[] }
export interface DuplicateReport { groups: DuplicateGroup[]; wasted: number }

export const api5 = {
  links: () => invoke<{ items: LinkInfo[]; base_url: string | null }>("links"),
  linkSetup: (baseUrl: string, endpoint: string, bucket: string, accessKeyId: string, secretAccessKey: string) =>
    invoke<unknown>("link_setup", { baseUrl, endpoint, bucket, accessKeyId, secretAccessKey }),
  linkCreate: (path: string, days: number | null, maxDownloads: number | null, password: string | null) =>
    invoke<LinkInfo>("link_create", { path, days, maxDownloads, password }),
  linkRevoke: (id: string) => invoke<LinkInfo>("link_revoke", { id }),
  spaceUsage: (path: string) => invoke<SpaceUsage>("space_usage", { path }),
  duplicates: (path: string | null) => invoke<DuplicateReport>("duplicates", { path }),
  fontGet: () => invoke<ArrayBuffer>("font_get"),
  fontPick: () => invoke<string>("font_pick"),
  fontName: () => invoke<string>("font_name"),
  fontReset: () => invoke<void>("font_reset"),
};

/** Eigene Schrift (falls gespeichert) laden und als „SnapCustom“ vor Inconsolata setzen. */
export async function applyFont(): Promise<void> {
  const root = document.documentElement;
  try {
    const buf = await api5.fontGet();
    if (buf && buf.byteLength > 0) {
      const face = new FontFace(`SnapCustom${Date.now()}`, buf);
      await face.load();
      document.fonts.add(face);
      root.style.setProperty("--font", `"${face.family}", "Inconsolata", monospace`);
      return;
    }
  } catch {
    // Kaputte Schrift: zurück zum Standard
  }
  root.style.removeProperty("--font");
}
