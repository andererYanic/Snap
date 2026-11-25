// Unterschrift zeichnen (einmalig, wird lokal gespeichert und beim Signieren auf die Seite gesetzt).
import { useState } from 'react';
import { Dimensions, Modal, Pressable, StyleSheet, View } from 'react-native';
import { GestureHandlerRootView } from 'react-native-gesture-handler';
import { request } from './core';
import { t } from './i18n';
import { DrawArea, type Stroke, Strokes } from './Ink';
import { Text } from './text';
import { snapTheme, type Theme } from './theme';
import { themed } from './ui';

/** Seitenverhältnis der Unterschriftsfläche (Breite : Höhe). */
export const PAD_RATIO = 3;
const PAD_W = Math.min(Dimensions.get('window').width - 48, 520);
const PAD_H = PAD_W / PAD_RATIO;

export default function SignaturePad({ onDone, onCancel, theme = snapTheme(true) }: { onDone: (strokes: Stroke[]) => void; onCancel: () => void; theme?: Theme }) {
  const styles = padStyles(theme);
  const [strokes, setStrokes] = useState<Stroke[]>([]);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    try {
      await request({ SignatureSave: { strokes } });
      onDone(strokes);
    } catch (e: any) {
      setError(String(e?.message ?? e));
    }
  };
  return (
    <Modal visible transparent animationType="fade" onRequestClose={onCancel} statusBarTranslucent>
      <GestureHandlerRootView style={styles.bg}>
        <View style={styles.card}>
          <Text style={styles.title}>{t('Unterschrift zeichnen')}</Text>
          <Text style={styles.hint}>{t('Mit dem Finger unterschreiben. Die Unterschrift bleibt nur auf diesem Gerät.')}</Text>
          <View style={[styles.pad, { width: PAD_W, height: PAD_H }]}>
            <View style={styles.line} />
            <Text style={styles.cross}>×</Text>
            <Strokes strokes={strokes} width={PAD_W} height={PAD_H} color="#141f59" strokeWidth={2.6} />
            <DrawArea width={PAD_W} height={PAD_H} color="#141f59" strokeWidth={2.6} onStroke={(s) => setStrokes((x) => [...x, s])} testID="signature-pad" />
          </View>
          {error && <Text style={styles.error}>{error}</Text>}
          <View style={styles.row}>
            <Pressable onPress={() => setStrokes([])} disabled={strokes.length === 0} style={styles.btn} accessibilityRole="button">
              <Text style={[styles.btnText, strokes.length === 0 && styles.dim]}>{t('Löschen')}</Text>
            </Pressable>
            <View style={styles.grow} />
            <Pressable onPress={onCancel} style={styles.btn} accessibilityRole="button">
              <Text style={styles.btnText}>{t('Abbrechen')}</Text>
            </Pressable>
            <Pressable onPress={save} disabled={strokes.length === 0} style={styles.btn} accessibilityRole="button">
              <Text style={[styles.btnText, styles.strong, strokes.length === 0 && styles.dim]}>{t('Übernehmen')}</Text>
            </Pressable>
          </View>
        </View>
      </GestureHandlerRootView>
    </Modal>
  );
}

const padStyles = themed((th: Theme) =>
  StyleSheet.create({
    bg: { flex: 1, backgroundColor: 'rgba(10,10,9,0.55)', justifyContent: 'center', alignItems: 'center' },
    card: { backgroundColor: th.surface, borderRadius: 16, paddingHorizontal: 20, paddingTop: 22, paddingBottom: 12, gap: 10 },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    hint: { color: th.text2, fontSize: 13, lineHeight: 18 },
    // Unterschriftsfeld bleibt weiß wie Papier (auch im dunklen Thema)
    pad: { backgroundColor: '#FFFFFF', borderRadius: 10, overflow: 'hidden', marginTop: 6, borderWidth: StyleSheet.hairlineWidth, borderColor: th.line },
    line: { position: 'absolute', left: 20, right: 20, bottom: PAD_H * 0.26, height: 1, backgroundColor: '#D9D6CE' },
    cross: { position: 'absolute', left: 20, bottom: PAD_H * 0.26 + 4, color: '#B9B6AD', fontSize: 14 },
    error: { color: th.danger },
    row: { flexDirection: 'row', alignItems: 'center', marginTop: 4 },
    grow: { flex: 1 },
    btn: { minHeight: 48, paddingHorizontal: 12, justifyContent: 'center' },
    btnText: { color: th.accent, fontSize: 15 },
    strong: { fontWeight: '700' },
    dim: { opacity: 0.4 },
  }));
