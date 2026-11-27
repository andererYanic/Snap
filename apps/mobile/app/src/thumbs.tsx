// Vorschaubilder: Android erzeugt sie (schnell, Cache auf dem Gerät), hier zusätzlich ein
// Speicher-Cache – beim Zurückscrollen sofort da. Bilder blenden weich ein.
import { useEffect, useState } from 'react';
import { Image, StyleSheet, type ImageStyle, type StyleProp } from 'react-native';
import { request, thumbnail } from './core';

/** Kamera-Rohdaten: Snap zeigt die eingebettete JPEG-Vorschau (der Kern liest sie aus). */
const RAW = /\.(cr2|nef|nrw|arw|sr2|srf|dng|orf|rw2|pef|srw|raf|3fr|erf|kdc|mos)$/i;
export const isRaw = (name: string) => RAW.test(name);
const MEDIA = /\.(jpe?g|png|gif|webp|heic|heif|bmp|avif|mp4|mkv|mov|webm|3gp|m4v|cr2|nef|nrw|arw|sr2|srf|dng|orf|rw2|pef|srw|raf|3fr|erf|kdc|mos)$/i;
const VIDEO = /\.(mp4|mkv|mov|webm|3gp|m4v)$/i;
export const isMedia = (name: string) => MEDIA.test(name);
export const isVideo = (name: string) => VIDEO.test(name);
export const isImage = (name: string) => MEDIA.test(name) && !VIDEO.test(name);
const AUDIO = /\.(mp3|flac|m4a|ogg|oga|opus|wav|aac|wma|amr|mid|midi)$/i;
export const isAudio = (name: string) => AUDIO.test(name);

const cache = new Map<string, string | null>();
const pending = new Map<string, Promise<string | null>>();

export function loadThumb(path: string, edge = 256): Promise<string | null> {
  const key = `${edge}|${path}`;
  if (cache.has(key)) {
    return Promise.resolve(cache.get(key) ?? null);
  }
  let p = pending.get(key);
  if (!p) {
    const size = edge > 1024 ? 'Full' : edge > 256 ? 'Large' : 'Small';
    const make = isRaw(path) ? request<{ path: string }>({ Thumbnail: { path, size } }).then((r) => r.path) : thumbnail(path, edge);
    p = make
      .catch(() => null)
      .then((file) => {
        const uri = file ? `file://${file}` : null;
        cache.set(key, uri);
        pending.delete(key);
        return uri;
      });
    pending.set(key, p);
  }
  return p;
}

export function useThumb(path: string | null, edge = 256): string | null {
  const key = path ? `${edge}|${path}` : '';
  const [uri, setUri] = useState<string | null>(() => (path ? cache.get(key) ?? null : null));
  useEffect(() => {
    let alive = true;
    if (path) {
      loadThumb(path, edge).then((u) => alive && setUri(u));
    }
    return () => {
      alive = false;
    };
  }, [path, edge, key]);
  return uri;
}

/** Vorschaubild mit Einblenden; zeigt nichts, solange es lädt. */
export function Thumb({ path, edge = 256, style }: { path: string; edge?: number; style: StyleProp<ImageStyle> }) {
  const uri = useThumb(path, edge);
  if (!uri) {
    return null;
  }
  return (
    // Android blendet selbst ein (fadeDuration) – ohne Animationsarbeit pro Bild in JS
    <Image source={{ uri }} style={[StyleSheet.absoluteFill, style]} fadeDuration={150} />
  );
}
