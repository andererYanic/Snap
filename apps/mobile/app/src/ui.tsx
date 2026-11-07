// Gemeinsame Bausteine: Symbole (aus Formen gezeichnet – keine fremden Grafiken),
// untere Auswahlfläche (Bottom Sheet), tippbare Zeile mit Rückmeldung.
import type { ReactNode } from 'react';
import { Modal, Pressable, StyleSheet, View } from 'react-native';
import { Text } from './text';
import Animated from 'react-native-reanimated';
import { fadeIn, m, sheetIn } from './motion';
import type { Theme } from './theme';

const TYPE_COLORS: [RegExp, string, string][] = [
  [/^(jpe?g|png|gif|webp|heic|heif|bmp|tiff?|avif|svg|raw|dng)$/, '#3fa66b', 'IMG'],
  [/^(mp4|mkv|mov|webm|avi|m4v|3gp)$/, '#d0503f', 'VID'],
  [/^(mp3|flac|m4a|ogg|opus|wav|aac)$/, '#9a55d6', 'AUD'],
  [/^pdf$/, '#d64545', 'PDF'],
  [/^(docx?|odt|rtf)$/, '#3b72d9', 'DOC'],
  [/^(txt|md|log|csv|json|xml)$/, '#6b7280', 'TXT'],
  [/^(xlsx?|ods)$/, '#2e8b57', 'XLS'],
  [/^(zip|tar|gz|tgz|7z|rar|xz|zst|bz2)$/, '#a07a3c', 'ZIP'],
  [/^apk$/, '#4b9a5f', 'APK'],
];

function typeOf(name: string): [string, string] {
  const ext = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1).toLowerCase() : '';
  for (const [re, color, label] of TYPE_COLORS) {
    if (re.test(ext)) {
      return [color, label];
    }
  }
  return ['#8a8f9c', ext.slice(0, 4).toUpperCase()];
}

/** Ordner (gelb) oder Datei (Blatt mit farbigem Typ-Etikett), `size` × `size`. */
export function Glyph({ name, dir, theme, size = 40 }: { name: string; dir: boolean; theme: Theme; size?: number }) {
  const k = size / 40;
  if (dir) {
    return (
      <View style={{ width: size, height: size, justifyContent: 'center', alignItems: 'center' }}>
        <View style={[g.tab, { top: 7 * k, left: 4 * k, width: 14 * k, height: 8 * k, borderRadius: 3 * k }]} />
        <View style={[g.folder, { width: 34 * k, height: 24 * k, borderRadius: 5 * k, marginTop: 6 * k }]} />
      </View>
    );
  }
  const [color, label] = typeOf(name);
  return (
    <View style={{ width: size, height: size, justifyContent: 'center', alignItems: 'center' }}>
      <View
        style={[
          g.sheet,
          { width: 28 * k, height: 34 * k, borderRadius: 4 * k, backgroundColor: theme.dark ? '#3a3d47' : '#ffffff', borderColor: theme.stroke },
        ]}
      >
        <View style={[g.badge, { backgroundColor: color, borderRadius: 2 * k, paddingVertical: 1 * k }]}>
          {label ? <Text style={[g.badgeText, { fontSize: 7 * k }]}>{label}</Text> : null}
        </View>
      </View>
    </View>
  );
}

/** Symbole der unteren Leiste. */
export function NavIcon({ name, color }: { name: string; color: string }) {
  const b = { borderColor: color };
  switch (name) {
    case 'files':
      return (
        <View style={g.nav}>
          <View style={[g.navFolderTab, b]} />
          <View style={[g.navFolder, b]} />
        </View>
      );
    case 'photos':
      return (
        <View style={[g.nav, g.grid]}>
          {[0, 1, 2, 3].map((i) => <View key={i} style={[g.cell, b]} />)}
        </View>
      );
    case 'devices':
      return (
        <View style={g.nav}>
          <View style={[g.phone, b]} />
        </View>
      );
    default:
      return (
        <View style={[g.nav, g.dots]}>
          {[0, 1, 2].map((i) => <View key={i} style={[g.dot, { backgroundColor: color }]} />)}
        </View>
      );
  }
}

export interface SheetAction {
  label: string;
  onPress: () => void;
  danger?: boolean;
  disabled?: boolean;
}

/** Auswahl von unten (Daumenzone), schließt beim Tippen daneben oder mit „Zurück“. */
export function BottomSheet({ visible, title, actions, onClose, theme }: {
  visible: boolean; title?: string; actions: SheetAction[]; onClose: () => void; theme: Theme;
}) {
  return (
    // Eigene Animation statt der trägen Fenster-Animation des Systems; Schließen ist sofort.
    <Modal visible={visible} transparent animationType="none" statusBarTranslucent onRequestClose={onClose}>
      <Animated.View entering={m(fadeIn)} style={g.scrim}>
        <Pressable style={g.fill} onPress={onClose} accessibilityLabel="Schließen" />
      </Animated.View>
      <Animated.View entering={m(sheetIn)} style={[g.sheetBox, { backgroundColor: theme.dark ? '#24262e' : '#ffffff' }]}>
        <View style={[g.grabber, { backgroundColor: theme.stroke }]} />
        {title ? <Text style={[g.sheetTitle, { color: theme.text2 }]}>{title}</Text> : null}
        {actions.map((a) => (
          <Pressable
            key={a.label}
            disabled={a.disabled}
            onPress={() => {
              onClose();
              a.onPress();
            }}
            android_ripple={{ color: theme.accentSoft as string }}
            style={[g.sheetRow, a.disabled && g.dim]}
            accessibilityRole="button"
          >
            <Text style={[g.sheetText, { color: a.danger ? theme.danger : theme.text }]}>{a.label}</Text>
          </Pressable>
        ))}
      </Animated.View>
    </Modal>
  );
}

/** Tippbare Fläche mit Android-Rückmeldung (Welle). */
/** Stil-Fabrik je Thema nur einmal ausführen (StyleSheet.create pro Renderlauf ist teuer). */
export function themed<T>(make: (th: Theme) => T): (th: Theme) => T {
  const cache = new WeakMap<Theme, T>();
  return (th) => {
    let v = cache.get(th);
    if (!v) {
      v = make(th);
      cache.set(th, v);
    }
    return v;
  };
}

export function Tap({ children, onPress, onLongPress, delayLongPress = 350, style, theme, label, disabled, selected }: {
  children: ReactNode; onPress?: () => void; onLongPress?: () => void; delayLongPress?: number; style?: any; theme: Theme; label?: string; disabled?: boolean; selected?: boolean;
}) {
  return (
    <Pressable
      onPress={onPress}
      onLongPress={onLongPress}
      delayLongPress={delayLongPress}
      disabled={disabled}
      android_ripple={{ color: theme.accentSoft as string }}
      style={style}
      accessibilityLabel={label}
      accessibilityRole="button"
      accessibilityState={{ selected, disabled }}
    >
      {children}
    </Pressable>
  );
}

const g = StyleSheet.create({
  tab: { position: 'absolute', backgroundColor: '#e8b33c' },
  folder: { backgroundColor: '#f7c948' },
  sheet: { borderWidth: 1, justifyContent: 'flex-end', padding: 3 },
  badge: { alignItems: 'center' },
  badgeText: { color: '#fff', fontWeight: '800' },
  nav: { width: 24, height: 24, justifyContent: 'center', alignItems: 'center' },
  navFolderTab: { position: 'absolute', top: 4, left: 2, width: 9, height: 5, borderWidth: 2, borderBottomWidth: 0, borderTopLeftRadius: 2, borderTopRightRadius: 2 },
  navFolder: { width: 22, height: 15, borderWidth: 2, borderRadius: 3, marginTop: 4 },
  grid: { flexDirection: 'row', flexWrap: 'wrap', gap: 3, padding: 1 },
  cell: { width: 9, height: 9, borderWidth: 2, borderRadius: 2 },
  phone: { width: 13, height: 22, borderWidth: 2, borderRadius: 3 },
  dots: { flexDirection: 'row', gap: 3 },
  dot: { width: 5, height: 5, borderRadius: 3 },
  scrim: { flex: 1, backgroundColor: 'rgba(0,0,0,0.4)' },
  fill: { flex: 1 },
  sheetBox: { paddingBottom: 24, paddingTop: 8, borderTopLeftRadius: 24, borderTopRightRadius: 24 },
  grabber: { alignSelf: 'center', width: 36, height: 4, borderRadius: 2, marginBottom: 8 },
  sheetTitle: { fontSize: 13, fontWeight: '600', paddingHorizontal: 24, paddingVertical: 8 },
  sheetRow: { minHeight: 56, justifyContent: 'center', paddingHorizontal: 24 },
  sheetText: { fontSize: 16 },
  dim: { opacity: 0.4 },
});
