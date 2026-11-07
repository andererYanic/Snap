// Kontextmenü am Finger (wie bei iOS): öffnet sich federnd am Berührungspunkt, wird an den
// Bildschirmrändern nach innen geschoben, schließt beim Tippen daneben oder mit „Zurück“.
import { useEffect } from 'react';
import { BackHandler, Dimensions, Pressable, StyleSheet, Text, View } from 'react-native';
import Animated, { FadeIn, FadeOut } from 'react-native-reanimated';
import type { Theme } from './theme';
import { m, popIn, popOut } from './motion';

export interface MenuAction {
  label: string;
  onPress: () => void;
  danger?: boolean;
  disabled?: boolean;
  /** Trennlinie davor */
  separate?: boolean;
}

const WIDTH = 236;
const ROW = 50;

export default function ContextMenu({ x, y, actions, onClose, theme, passThrough = false }: {
  x: number; y: number; actions: MenuAction[]; onClose: () => void; theme: Theme;
  /** Berührungen neben dem Menü gehen an den Inhalt darunter (z. B. weitere Dateien markieren). */
  passThrough?: boolean;
}) {
  const { width: W, height: H } = Dimensions.get('window');
  const height = actions.length * ROW + 12;
  // Bevorzugt rechts unterhalb des Fingers, sonst gespiegelt; immer im Bildschirm
  const left = Math.max(12, Math.min(x - 24, W - WIDTH - 12));
  const below = y + 12 + height < H - 24;
  const top = below ? y + 12 : Math.max(48, y - height - 12);

  useEffect(() => {
    const sub = BackHandler.addEventListener('hardwareBackPress', () => {
      onClose();
      return true;
    });
    return () => sub.remove();
  }, [onClose]);

  return (
    <View style={StyleSheet.absoluteFill} pointerEvents="box-none">
      {!passThrough && (
        <Animated.View entering={m(FadeIn.duration(120))} exiting={m(FadeOut.duration(120))} style={StyleSheet.absoluteFill}>
          <Pressable style={[StyleSheet.absoluteFill, styles.scrim]} onPress={onClose} accessibilityLabel="Menü schließen" />
        </Animated.View>
      )}
      <Animated.View
        entering={m(popIn)}
        exiting={m(popOut)}
        style={[
          styles.menu,
          { left, top, width: WIDTH, backgroundColor: theme.dark ? '#2b2d36' : '#ffffff', borderColor: theme.stroke },
          { transformOrigin: below ? 'top left' : 'bottom left' } as any,
        ]}
      >
        {actions.map((a) => (
          <View key={a.label}>
            {a.separate && <View style={[styles.sep, { backgroundColor: theme.stroke }]} />}
            <Pressable
              disabled={a.disabled}
              onPress={() => {
                onClose();
                a.onPress();
              }}
              android_ripple={{ color: theme.accentSoft as string }}
              style={[styles.row, a.disabled && styles.dim]}
              accessibilityRole="menuitem"
            >
              <Text style={[styles.text, { color: a.danger ? theme.danger : theme.text }]}>{a.label}</Text>
            </Pressable>
          </View>
        ))}
      </Animated.View>
    </View>
  );
}

const styles = StyleSheet.create({
  scrim: { backgroundColor: 'rgba(0,0,0,0.18)' },
  menu: {
    position: 'absolute', borderRadius: 16, paddingVertical: 6, borderWidth: StyleSheet.hairlineWidth, overflow: 'hidden',
    elevation: 12, shadowColor: '#000', shadowOpacity: 0.25, shadowRadius: 18, shadowOffset: { width: 0, height: 8 },
  },
  row: { height: ROW, justifyContent: 'center', paddingHorizontal: 18 },
  text: { fontSize: 16 },
  sep: { height: StyleSheet.hairlineWidth, marginVertical: 4 },
  dim: { opacity: 0.4 },
});
