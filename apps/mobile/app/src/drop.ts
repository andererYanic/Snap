// Snap Drop auf Android: verbindet den Kern (Protokoll, Prüfungen) mit Bluetooth LE, mDNS
// (Multicast-Sperre) und dem Offline-Weg über einen kurzzeitigen Hotspot (Kotlin-Modul `SnapDrop`).
// Entwurf und Risiken: docs/DROP.md.
import { NativeEventEmitter, NativeModules, PermissionsAndroid, Platform } from 'react-native';
import { request } from './core';

export type Mode = 'off' | 'contacts' | 'everyone';

export interface DropStatus {
  mode: Mode;
  until: number | null;
  name: string;
  default_name: string;
  custom_name: boolean;
  fingerprint: string;
  dir: string;
  code: string | null;
  /** Kontakte dürfen von überall senden. */
  remote?: boolean;
}

export interface NearbyPeer {
  id: string;
  name: string | null;
  kind: 'phone' | 'tablet' | 'laptop' | 'desktop' | null;
  contact: string | null;
  contact_key?: string | null;
  via: 'lan' | 'bluetooth' | 'code';
  offline: boolean;
}

export interface IncomingOffer {
  id: number;
  name: string;
  contact: string | null;
  code: string;
  files: { name: string; size: number; mime: string }[];
  total: number;
  preview: boolean;
  expires_at: number;
  text?: string | null;
  link?: string | null;
}

export interface HotspotOffer {
  name: string;
  files: number;
  total: number;
  ssid: string;
  passphrase: string;
}

export interface Contact {
  key: string;
  name: string;
  device_name: string;
  renamed: boolean;
  fingerprint: string;
  added_at: number;
  last_seen: number;
  /** Adresse bekannt: Senden von überall möglich. */
  remote?: boolean;
  /** Darf von überall an dieses Gerät senden. */
  remote_ok?: boolean;
}

export interface Blocked {
  key: string;
  name: string;
  blocked_at: number;
}

interface Caps {
  ble: boolean;
  bleOn: boolean;
  advertise: boolean;
  joinHotspot: boolean;
  sdk: number;
}

const native = () => NativeModules.SnapDrop as any | undefined;

// --- Kern ---

export const status = () => request<DropStatus>('DropStatus');
export const nearby = () => request<{ peers: NearbyPeer[] }>('DropNearby').then((r) => r.peers);
export const offers = () => request<{ offers: IncomingOffer[] }>('DropOffers').then((r) => r.offers);
export const respond = (id: number, accept: boolean, block = false) => request({ DropRespond: { id, accept, block } });
export const preview = (id: number) => request<{ png: string | null }>({ DropPreview: { id } }).then((r) => r.png);
export const contacts = () => request<{ contacts: Contact[] }>('DropContacts').then((r) => r.contacts);
export const saveContact = (id: number) => request<Contact>({ DropSaveContact: { id } });
export const removeContact = (key: string) => request({ DropRemoveContact: { key } });
export const renameContact = (key: string, alias: string | null) => request<Contact>({ DropRenameContact: { key, alias } });
/** Eigener Anzeigename (`null` = Gerätename). */
/** Kennzeichen eines für die Übertragung gepackten Ordners (wird beim Empfang entpackt). */
export const FOLDER_MIME = 'application/x-snap-folder+zip';

export interface HistoryItem {
  id: number;
  at: number;
  peer: string;
  kind: 'files' | 'text' | 'link';
  count: number;
  files: string[];
  exists: boolean;
}
/** Laufende Übertragung oder offenes Angebot abbrechen. */
export const cancel = (id: number) => request({ DropCancel: { id } });
export const history = () => request<{ items?: HistoryItem[] }>('DropHistory').then((r) => r?.items ?? []);
export const clearHistory = () => request('DropHistoryClear');
export const setRemote = (enabled: boolean) => request<DropStatus>({ DropSetRemote: { enabled } });
export const setContactRemote = (key: string, ok: boolean) => request<Contact>({ DropContactRemote: { key, ok } });
/** An einen gespeicherten Kontakt senden, egal wo er ist (Ende-zu-Ende verschlüsselt, ggf. über ein Relais). */
export const sendToContact = (key: string, paths: string[], text: string | null = null) =>
  request<SendResult>({ DropSendContact: { contact: key, paths, text } });
export const setName = (name: string | null) => request<DropStatus>({ DropSetName: { name } });
export const blocked = () => request<{ blocked: Blocked[] }>('DropBlocked').then((r) => r.blocked);
export const unblock = (key: string) => request({ DropUnblock: { key } });
export const addCode = (code: string) => request<NearbyPeer>({ DropAddCode: { code } });

// --- Rechte (erst beim Einschalten bzw. beim ersten Senden) ---

const P = PermissionsAndroid.PERMISSIONS as Record<string, string>;

function neededPermissions(sdk: number): string[] {
  if (sdk >= 33) {
    return [P.BLUETOOTH_SCAN, P.BLUETOOTH_ADVERTISE, P.BLUETOOTH_CONNECT, P.NEARBY_WIFI_DEVICES].filter(Boolean);
  }
  if (sdk >= 31) {
    // Hotspot verlangt bis Android 12 den Standort (Systemvorgabe).
    return [P.BLUETOOTH_SCAN, P.BLUETOOTH_ADVERTISE, P.BLUETOOTH_CONNECT, P.ACCESS_FINE_LOCATION].filter(Boolean);
  }
  return [P.ACCESS_FINE_LOCATION].filter(Boolean);
}

let caps: Caps | null = null;

export async function capabilities(): Promise<Caps> {
  if (!native()) {
    return { ble: false, bleOn: false, advertise: false, joinHotspot: false, sdk: 0 };
  }
  caps = await native().capabilities();
  return caps!;
}

/** Hat Snap die Rechte schon? (Ohne Rückfrage.) */
async function hasPermissions(): Promise<boolean> {
  if (Platform.OS !== 'android' || !native()) {
    return false;
  }
  const c = caps ?? (await capabilities());
  const results = await Promise.all(neededPermissions(c.sdk).map((p) => PermissionsAndroid.check(p as any)));
  return results.every(Boolean);
}

/** Fragt die Rechte ab. `false`, wenn etwas abgelehnt wurde (Drop läuft dann nur im WLAN). */
export async function askPermissions(): Promise<boolean> {
  if (Platform.OS !== 'android' || !native()) {
    return false;
  }
  const c = caps ?? (await capabilities());
  const res = await PermissionsAndroid.requestMultiple(neededPermissions(c.sdk) as any);
  return Object.values(res).every((v) => v === PermissionsAndroid.RESULTS.GRANTED);
}

// --- Sichtbarkeit: Bluetooth-Werbung und Multicast folgen dem Kern ---

let browsing = false;
let visible = false;

function applyMulticast() {
  native()?.multicast(browsing || visible);
}

/** Setzt die Sichtbarkeit im Kern und richtet Bluetooth passend ein. */
export async function setVisibility(mode: Mode): Promise<DropStatus> {
  if (mode !== 'off' && !(await hasPermissions())) {
    await askPermissions();
  }
  const st = await request<DropStatus>({ DropSetVisibility: { mode } });
  await syncRadio(st.mode);
  // Für das Wecken „wie Sichtbarkeit“: Ende des „Alle“-Fensters merken (auch wenn Snap schließt)
  native()?.setEveryoneUntil?.(st.mode === 'everyone' && st.until ? st.until : 0);
  return st;
}

/** Bluetooth-Werbung an den Stand des Kerns anpassen (z. B. nach dem Start oder Ablauf von „Alle“). */
export async function syncRadio(mode?: Mode) {
  const m = mode ?? (await status()).mode;
  visible = m !== 'off';
  applyMulticast();
  const n = native();
  if (!n) {
    return;
  }
  if (!visible) {
    n.stopAdvertise();
    return;
  }
  if (!(await hasPermissions())) {
    return; // nur WLAN (mDNS) – Bluetooth ohne Rechte geht nicht
  }
  const info = await request<{ info: string | null }>('DropConnectInfo');
  if (info.info) {
    n.advertise(info.info).catch(() => {});
    n.updateInfo(info.info);
  }
}

// --- Im Hintergrund empfangen (Android) ---

export interface BackgroundState {
  enabled: boolean;
  wlan: boolean;
  who: 'contacts' | 'visibility';
  batteryExempt: boolean;
  supported: boolean;
  /** Anfragen zusätzlich als Popup (Erlaubnis vorhanden und eingeschaltet). */
  popup?: boolean;
}

export const backgroundState = (): Promise<BackgroundState | null> =>
  native()?.backgroundState ? native().backgroundState() : Promise.resolve(null);

/** Beim App-Start: Weck-Schlüssel bereitstellen und die Weck-Suche neu einrichten. */
export async function initBackground() {
  const st = await backgroundState().catch(() => null);
  if (!st) {
    return;
  }
  // Die Schlüssel braucht auch der Sender (zum Wecken eines Kontakts) – nur als private Datei.
  await request({ DropWakeExport: { enable: true } }).catch(() => {});
  native().setBackground(st.enabled, st.wlan, st.who);
}

/** Einstellungen übernehmen; beim Einschalten werden Bluetooth- und Mitteilungsrechte erfragt. */
export async function setBackground(enabled: boolean, wlan: boolean, who: 'contacts' | 'visibility') {
  if (enabled) {
    if (!(await hasPermissions())) {
      await askPermissions();
    }
    const c = caps ?? (await capabilities());
    if (c.sdk >= 33 && P.POST_NOTIFICATIONS) {
      await PermissionsAndroid.request(P.POST_NOTIFICATIONS as any).catch(() => {});
    }
  }
  await request({ DropWakeExport: { enable: true } }).catch(() => {});
  native()?.setBackground?.(enabled, wlan, who);
}

/** Popup über anderen Apps ein-/ausschalten (öffnet beim ersten Mal die Systemeinstellung). */
export const setPopup = (enabled: boolean) => native()?.setPopup?.(enabled);
export const requestBatteryExempt = () => native()?.requestBatteryExempt?.();

/** Einen gespeicherten Kontakt per Bluetooth wecken (90 s). `false`, wenn nicht möglich. */
export async function wakeContact(key: string): Promise<boolean> {
  if (!native()?.wake || !(await hasPermissions())) {
    return false;
  }
  return native().wake(key, 90);
}

/** „Jemand sucht“ (30 s) – weckt nur Geräte mit laufendem „Alle“-Fenster. */
export async function wakeAll() {
  if (native()?.wakeAll && (await hasPermissions())) {
    native().wakeAll(30).catch(() => {});
  }
}

export const stopWake = () => native()?.stopWake?.();

// --- Suchen ---

/** Bluetooth-Adresse je Transport-Kennung (für den Offline-Weg). */
const deviceOf = new Map<string, string>();

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

/** Base64 → Bytes (nur das Nötigste; ungültige Zeichen → null). */
function fromBase64(b64: string): number[] | null {
  const out: number[] = [];
  let buf = 0;
  let bits = 0;
  for (const ch of b64.replace(/=+$/, '')) {
    const v = B64.indexOf(ch);
    if (v < 0) {
      return null;
    }
    buf = (buf << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out.push((buf >> bits) & 0xff);
    }
  }
  return out;
}

/** Transport-Kennung (Hex) aus Verbindungsdaten: Version, Flags, dann 32 Byte Schlüssel. */
export function endpointHex(b64: string): string | null {
  const bytes = fromBase64(b64);
  if (!bytes || bytes.length < 34) {
    return null;
  }
  return bytes
    .slice(2, 34)
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

export async function startBrowse() {
  browsing = true;
  applyMulticast();
  await request({ DropBrowse: { on: true } }).catch(() => {});
  if (await hasPermissions()) {
    native()?.scan(true).catch(() => {});
  }
}

export async function stopBrowse() {
  browsing = false;
  applyMulticast();
  native()?.scan(false).catch(() => {});
  await request({ DropBrowse: { on: false } }).catch(() => {});
}

// --- Bluetooth-Ereignisse ---

type BleEvent = { type: 'peer' | 'hotspot' | 'ready' | 'error' | 'hotspot-lost'; device?: string; data?: string; message?: string };
const readyWaiters = new Map<string, (info: string) => void>();
let hotspotListener: ((data: string, offer: HotspotOffer) => void) | null = null;
let listening = false;

/** Einmal beim Start: Bluetooth-Ereignisse an den Kern bzw. die Oberfläche weiterreichen. */
export function listen() {
  const n = native();
  if (!n || listening) {
    return;
  }
  listening = true;
  new NativeEventEmitter(n).addListener('snap-drop-ble', (raw: unknown) => {
    let ev: BleEvent;
    try {
      ev = JSON.parse(String(raw));
    } catch {
      return;
    }
    if (ev.type === 'peer' && ev.data) {
      const ep = endpointHex(ev.data);
      if (ep && ev.device) {
        deviceOf.set(ep, ev.device);
      }
      request({ DropAddPeer: { info: ev.data } }).catch(() => {});
    } else if (ev.type === 'hotspot' && ev.data) {
      const data = ev.data;
      request<HotspotOffer>({ DropHotspotParse: { data } })
        .then((offer) => hotspotListener?.(data, offer))
        .catch(() => {});
    } else if (ev.type === 'ready' && ev.data && ev.device) {
      readyWaiters.get(ev.device)?.(ev.data);
    }
  });
}

export function onHotspotRequest(cb: ((data: string, offer: HotspotOffer) => void) | null) {
  hotspotListener = cb;
}

// --- Senden ---

export interface SendResult {
  id: number;
  ok: boolean;
  error: string | null;
  code: string | null;
}

/** Sendet an ein Gerät aus der Liste; offline erreichbare Geräte über einen kurzen Hotspot. */
export async function send(peer: NearbyPeer, paths: string[], text: string | null = null): Promise<SendResult> {
  if (!peer.offline) {
    return request<SendResult>({ DropSend: { peer: peer.id, paths, text } });
  }
  if (paths.length === 0) {
    throw new Error('Text lässt sich nur über ein gemeinsames Netz senden.');
  }
  const n = native();
  const device = deviceOf.get(peer.id);
  if (!n || !device) {
    throw new Error('Gerät ist nur über Bluetooth erreichbar, aber nicht mehr in der Nähe.');
  }
  const hs: { ssid: string; passphrase: string } = await n.startHotspot();
  try {
    const { data } = await request<{ data: string }>({ DropHotspotRequest: { ssid: hs.ssid, passphrase: hs.passphrase, paths } });
    const ready = new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => {
        readyWaiters.delete(device);
        reject(new Error('Keine Antwort vom anderen Gerät.'));
      }, 150_000);
      readyWaiters.set(device, (info) => {
        clearTimeout(timer);
        readyWaiters.delete(device);
        resolve(info);
      });
    });
    await n.sendRequest(device, data);
    const info = await ready;
    await request({ DropAddPeer: { info } });
    const id = endpointHex(info);
    if (!id) {
      throw new Error('Ungültige Antwort vom anderen Gerät.');
    }
    return await request<SendResult>({ DropSend: { peer: id, paths, text } });
  } finally {
    n.closeLink(device);
    n.stopHotspot();
  }
}

// --- Offline-Weg (Empfänger) ---

let joined = false;

/** Kurzanfrage angenommen: beitreten, Sitzung neu starten, dem Sender die neuen Daten melden. */
export async function acceptHotspot(data: string, offer: HotspotOffer) {
  const n = native();
  if (!n) {
    throw new Error('Nicht verfügbar');
  }
  await request({ DropHotspotAccept: { data } });
  await n.joinHotspot(offer.ssid, offer.passphrase);
  joined = true;
  // Neue Sockets laufen jetzt über den Hotspot.
  const r = await request<{ info: string | null }>('DropRefresh');
  if (r.info) {
    n.updateInfo(r.info);
  }
}

/** Nach dem Empfang (oder Abbruch) wieder ins normale Netz. */
export async function leaveHotspot() {
  if (!joined) {
    return;
  }
  joined = false;
  native()?.leaveHotspot();
  const r = await request<{ info: string | null }>('DropRefresh').catch(() => ({ info: null }));
  if (r.info) {
    native()?.updateInfo(r.info);
  }
}
