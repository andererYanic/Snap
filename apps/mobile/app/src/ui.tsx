// Gemeinsame Bausteine: Datei-/Ordnersymbole, Abschnittstitel,
// untere Auswahlfläche (Bottom Sheet), tippbare Zeile mit Rückmeldung.
import type { ReactNode } from 'react';
import { Modal, Pressable, StyleSheet, View } from 'react-native';
import { Text } from './text';
import Animated from 'react-native-reanimated';
import { fadeIn, m, sheetIn } from './motion';
import type { Theme } from './theme';
import { FolderShape, Icon, type IconName } from './Icon';

// Dateiart: gedämpfte Farbe und kurzes Etikett (in der Monospace-Schrift auf dem Blatt).
const TYPE_COLORS: [RegExp, string, string][] = [
  [/^(jpe?g|png|gif|webp|heic|heif|bmp|tiff?|avif|svg|raw|dng)$/, '#3F8F6A', 'IMG'],
  [/^(mp4|mkv|mov|webm|avi|m4v|3gp)$/, '#B4533F', 'VID'],
  [/^(mp3|flac|m4a|ogg|opus|wav|aac)$/, '#8458B8', 'AUD'],
  [/^pdf$/, '#C0392B', 'PDF'],
  [/^(docx?|odt|rtf)$/, '#2F62B5', 'DOC'],
  [/^(txt|md|log|csv|json|xml)$/, '#6B6A65', 'TXT'],
  [/^(xlsx?|ods)$/, '#2E7D4F', 'XLS'],
  [/^(pptx?|odp)$/, '#C2662D', 'PPT'],
  [/^(zip|tar|gz|tgz|7z|rar|xz|zst|bz2)$/, '#8C6D3A', 'ZIP'],
  [/^(ttf|otf|woff2?)$/, '#5A5A8C', 'FNT'],
  [/^(apk|apks|xapk|apkm)$/, '#3F8F5F', 'APK'],
];

export function typeOf(name: string): [string, string] {
  const ext = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1).toLowerCase() : '';
  for (const [re, color, label] of TYPE_COLORS) {
    if (re.test(ext)) {
      return [color, label];
    }
  }
  return ['#8A8880', ext.slice(0, 3).toUpperCase()];
}

/** Ordner (gefüllt) oder Datei (Blatt mit Eselsohr und Typ-Etikett), `size` × `size`. */
export function Glyph({ name, dir, theme, size = 40 }: { name: string; dir: boolean; theme: Theme; size?: number }) {
  if (dir) {
    return <FolderShape size={size} color={theme.folder as string} tab={theme.dark ? '#A57E2E' : '#C99634'} />;
  }
  const [color, label] = typeOf(name);
  const k = size / 40;
  return (
    <View style={{ width: size, height: size, justifyContent: 'center', alignItems: 'center' }}>
      <View
        style={[
          g.sheet,
          {
            width: 28 * k,
            height: 34 * k,
            borderRadius: 3 * k,
            borderTopRightRadius: 9 * k,
            backgroundColor: theme.surface,
            borderColor: theme.dark ? 'rgba(237,235,230,0.18)' : 'rgba(26,26,24,0.16)',
            paddingBottom: 5 * k,
          },
        ]}
      >
        {label ? <Text style={[g.badgeText, { fontSize: 8.5 * k, color }]}>{label}</Text> : null}
        <View style={[g.typeBar, { backgroundColor: color, height: 2 * k, width: 12 * k, marginTop: 2 * k }]} />
      </View>
    </View>
  );
}

const NAV_ICON: Record<string, IconName> = { files: 'folder', photos: 'photos', settings: 'settings', devices: 'devices' };

/** Symbole der unteren Leiste. */
export function NavIcon({ name, color }: { name: string; color: string }) {
  return <Icon name={NAV_ICON[name] ?? 'more'} size={24} color={color} strokeWidth={1.7} />;
}

/** Abschnittstitel: kleine Versalien mit Laufweite, gedämpft. */
export function SectionLabel({ children, theme, style }: { children: ReactNode; theme: Theme; style?: any }) {
  return <Text style={[g.section, { color: theme.text3 }, style]}>{children}</Text>;
}

export interface SheetAction {
  label: string;
  onPress: () => void;
  danger?: boolean;
  disabled?: boolean;
  icon?: IconName;
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
      <Animated.View entering={m(sheetIn)} style={[g.sheetBox, { backgroundColor: theme.surface }]}>
        <View style={[g.grabber, { backgroundColor: theme.text3 }]} />
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
            {a.icon ? <Icon name={a.icon} size={22} color={(a.danger ? theme.danger : theme.text2) as string} /> : null}
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
  sheet: { borderWidth: 1.2, justifyContent: 'flex-end', alignItems: 'center' },
  badgeText: { fontWeight: '700', letterSpacing: 0.3 },
  typeBar: { borderRadius: 1 },
  section: { fontSize: 12, lineHeight: 16, letterSpacing: 1, textTransform: 'uppercase', fontWeight: '600' },
  scrim: { flex: 1, backgroundColor: 'rgba(10,10,9,0.42)' },
  fill: { flex: 1 },
  sheetBox: { paddingBottom: 24, paddingTop: 10, borderTopLeftRadius: 20, borderTopRightRadius: 20 },
  grabber: { alignSelf: 'center', width: 32, height: 4, borderRadius: 2, marginBottom: 10, opacity: 0.5 },
  sheetTitle: { fontSize: 13, lineHeight: 18, paddingHorizontal: 20, paddingTop: 4, paddingBottom: 8 },
  sheetRow: { minHeight: 52, flexDirection: 'row', alignItems: 'center', gap: 16, paddingHorizontal: 20 },
  sheetText: { fontSize: 16 },
  dim: { opacity: 0.4 },
});
