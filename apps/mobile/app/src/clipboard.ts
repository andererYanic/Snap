// Zwischenablage zwischen eigenen Geräten: senden (Knopf oder automatisch beim Öffnen der App)
// und das Zurückschicken eines eben empfangenen Textes vermeiden.
import * as core from './core';
import { request } from './core';

const AUTO_KEY = 'clip_auto';
let last: string | null = null;

/** Zuletzt gesendeter oder empfangener Text (gegen Ping-Pong zwischen Geräten). */
export const remember = (text: string) => {
  last = text;
};

export async function autoDevices(): Promise<string[]> {
  try {
    const v = JSON.parse((await core.getPref(AUTO_KEY)) ?? '[]');
    return Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

export async function setAuto(nodeId: string, on: boolean) {
  const list = new Set(await autoDevices());
  on ? list.add(nodeId) : list.delete(nodeId);
  core.setPref(AUTO_KEY, JSON.stringify([...list]));
}

/** Aktuelle Zwischenablage an ein Gerät senden. */
export async function sendTo(nodeId: string): Promise<void> {
  const text = await core.readClipboard();
  if (!text?.trim()) {
    throw new Error('Die Zwischenablage ist leer.');
  }
  await request({ ClipboardSend: { device: nodeId, text } });
  last = text;
}

/** Beim Öffnen der App: neue Zwischenablage an die gewählten Geräte (Android liest sie nur im
 *  Vordergrund). Fehler bleiben still – das Gerät kann gerade offline sein. */
export async function syncOnForeground() {
  const devices = await autoDevices();
  if (devices.length === 0) {
    return;
  }
  const text = await core.readClipboard().catch(() => null);
  if (!text?.trim() || text === last || text.length > 64 * 1024) {
    return;
  }
  last = text;
  await Promise.all(devices.map((d) => request({ ClipboardSend: { device: d, text } }).catch(() => {})));
}
