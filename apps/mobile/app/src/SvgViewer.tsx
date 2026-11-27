// SVG-Grafiken als Bild anzeigen (statt als Quelltext). Bereinigt vor dem Zeichnen: keine Skripte,
// keine eingebetteten Fremdinhalte und keine Verweise nach außen – ein SVG könnte sonst beim
// Öffnen Bilder aus dem Netz nachladen und so verraten, dass und wann es angesehen wurde.
import { useEffect, useMemo, useState } from 'react';
import { ActivityIndicator, Modal, Pressable, StatusBar, StyleSheet, useWindowDimensions, View } from 'react-native';
import { SvgXml } from 'react-native-svg';
import * as core from './core';
import { t } from './i18n';
import { Icon } from './Icon';
import { Text } from './text';
import type { Theme } from './theme';
import { themed } from './ui';

export const isSvg = (name: string) => /\.svg$/i.test(name);

/** Entfernt alles, was ausführen oder nachladen könnte. Gibt `null` zurück, wenn kein SVG. */
export function sanitizeSvg(src: string): string | null {
  let s = src;
  // Doctype/Entitäten (Entitäten-Bomben), Kommentare, Verarbeitungsanweisungen
  s = s.replace(/<!DOCTYPE[\s\S]*?(\[[\s\S]*?\])?\s*>/gi, '').replace(/<!--[\s\S]*?-->/g, '').replace(/<\?[\s\S]*?\?>/g, '');
  // Skripte und Fremdinhalte samt Inhalt
  s = s.replace(/<(script|foreignObject|iframe|object|embed)\b[\s\S]*?<\/\1\s*>/gi, '').replace(/<(script|foreignObject|iframe|object|embed)\b[^>]*\/?>/gi, '');
  // Ereignis-Attribute (onload= …)
  s = s.replace(/\son[a-z]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)/gi, ' ');
  // Verweise: nur interne (#id) und eingebettete Bilder (data:image/…) bleiben
  s = s.replace(/\s((?:xlink:)?href)\s*=\s*("([^"]*)"|'([^']*)')/gi, (m, _a, _q, d1, d2) => {
    const v = String(d1 ?? d2 ?? '').trim();
    return v.startsWith('#') || /^data:image\/(png|jpe?g|gif|webp);base64,/i.test(v) ? m : ' ';
  });
  // url(...) in Stilen nur intern
  s = s.replace(/url\(\s*(['"]?)(?!#)[^)]*\1\s*\)/gi, 'none');
  return /<svg[\s>]/i.test(s) ? s : null;
}

export default function SvgViewer({ path, name, theme, onClose, onSource }: {
  path: string; name: string; theme: Theme; onClose: () => void; onSource: () => void;
}) {
  const s = styles(theme);
  const { width, height } = useWindowDimensions();
  const [src, setSrc] = useState<string | null | 'error'>(null);
  useEffect(() => {
    core
      .readText(path)
      .then((r) => setSrc(r.truncated ? 'error' : r.text))
      .catch(() => setSrc('error'));
  }, [path]);
  const xml = useMemo(() => (src && src !== 'error' ? sanitizeSvg(src) : null), [src]);
  return (
    <Modal visible animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <View style={s.root}>
        <StatusBar barStyle={theme.dark ? 'light-content' : 'dark-content'} />
        <View style={s.top}>
          <Pressable onPress={onClose} style={s.btn} accessibilityLabel={t('Zurück')}>
            <Icon name="back" color={theme.text as string} />
          </Pressable>
          <Text style={[s.title, s.grow]} numberOfLines={1}>{name}</Text>
          <Pressable onPress={onSource} style={s.btn} accessibilityLabel={t('Quelltext')}>
            <Icon name="document" color={theme.text2 as string} />
          </Pressable>
        </View>
        {src === null ? (
          <ActivityIndicator style={s.grow} color={theme.accent as string} />
        ) : xml ? (
          <View style={s.canvas}>
            <SvgXml xml={xml} width={width - 32} height={height - 160} onError={() => setSrc('error')} />
          </View>
        ) : (
          <Text style={s.note}>{t('Diese Grafik lässt sich nicht anzeigen.')}</Text>
        )}
      </View>
    </Modal>
  );
}

const styles = themed((th) =>
  StyleSheet.create({
    root: { flex: 1, backgroundColor: th.bg },
    top: { flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 4, gap: 4 },
    btn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    grow: { flex: 1 },
    canvas: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 16 },
    note: { color: th.text2, fontSize: 15, lineHeight: 22, padding: 24, textAlign: 'center' },
  }),
);
