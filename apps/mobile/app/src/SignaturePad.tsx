// Unterschrift zeichnen (einmalig, wird lokal gespeichert und beim Signieren auf die Seite gesetzt).
import { useState } from 'react';
import { Dimensions, Modal, Pressable, StyleSheet, View } from 'react-native';
import { GestureHandlerRootView } from 'react-native-gesture-handler';
import { request } from './core';
import { t } from './i18n';
import { DrawArea, type Stroke, Strokes } from './Ink';
import { Text } from './text';

/** Seitenverhältnis der Unterschriftsfläche (Breite : Höhe). */
export const PAD_RATIO = 3;
const PAD_W = Math.min(Dimensions.get('window').width - 48, 520);
const PAD_H = PAD_W / PAD_RATIO;

export default function SignaturePad({ onDone, onCancel }: { onDone: (strokes: Stroke[]) => void; onCancel: () => void }) {
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

const styles = StyleSheet.create({
  bg: { flex: 1, backgroundColor: 'rgba(0,0,0,0.55)', justifyContent: 'center', alignItems: 'center' },
  card: { backgroundColor: '#24262e', borderRadius: 24, padding: 20, gap: 10 },
  title: { color: '#f1f1f4', fontSize: 18, fontWeight: '700' },
  hint: { color: '#b4b6be', fontSize: 13 },
  pad: { backgroundColor: '#fff', borderRadius: 12, overflow: 'hidden', marginTop: 6 },
  line: { position: 'absolute', left: 16, right: 16, bottom: PAD_H * 0.25, height: 1, backgroundColor: '#c9ccd6' },
  error: { color: '#ffb4a8' },
  row: { flexDirection: 'row', alignItems: 'center', marginTop: 4 },
  grow: { flex: 1 },
  btn: { minHeight: 44, paddingHorizontal: 12, justifyContent: 'center' },
  btnText: { color: '#9fbcff', fontSize: 15 },
  strong: { fontWeight: '700' },
  dim: { opacity: 0.4 },
});
