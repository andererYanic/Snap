// Favoriten und zuletzt geöffnete Dateien – nur lokal auf diesem Gerät gespeichert (nie
// abgeglichen). Höchstens 50 Favoriten und 20 zuletzt geöffnete Dateien.
import * as core from './core';

const FAV = 'favorites';
const RECENT = 'recent';
const MAX_FAV = 50;
const MAX_RECENT = 20;

export interface Place { path: string; name: string; dir: boolean }

async function read(key: string): Promise<Place[]> {
  try {
    const raw = await core.getPref(key);
    const v = raw ? JSON.parse(raw) : [];
    return Array.isArray(v) ? v.filter((x) => x && typeof x.path === 'string' && typeof x.name === 'string').slice(0, 100) : [];
  } catch {
    return [];
  }
}

const write = (key: string, list: Place[]) => core.setPref(key, JSON.stringify(list));

export const favorites = () => read(FAV);
export const recent = () => read(RECENT);

export async function toggleFavorite(p: Place): Promise<boolean> {
  const list = await favorites();
  const has = list.some((x) => x.path === p.path);
  write(FAV, has ? list.filter((x) => x.path !== p.path) : [p, ...list].slice(0, MAX_FAV));
  return !has;
}

export async function isFavorite(path: string): Promise<boolean> {
  return (await favorites()).some((x) => x.path === path);
}

/** Geöffnete Datei vorne einreihen (ohne Doppelte). */
export async function noteOpened(p: Place): Promise<void> {
  const list = (await recent()).filter((x) => x.path !== p.path);
  write(RECENT, [p, ...list].slice(0, MAX_RECENT));
}

export const clearRecent = () => write(RECENT, []);

/** Nach Umbenennen/Verschieben/Löschen: Einträge anpassen bzw. entfernen. */
export async function forget(paths: string[]): Promise<void> {
  const gone = new Set(paths);
  for (const key of [FAV, RECENT]) {
    const list = await read(key);
    const next = list.filter((x) => !gone.has(x.path));
    if (next.length !== list.length) {
      write(key, next);
    }
  }
}
