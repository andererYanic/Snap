// Texterkennung (OCR): offline mit Tesseract, Deutsch + Englisch. Ergebnis als auswählbarer Text
// mit „Kopieren“ – ohne weitere Einstellungen.
import { useState } from 'react';
import { ActivityIndicator, Modal, Pressable, ScrollView, StyleSheet, View } from 'react-native';
import * as core from './core';
import { t } from './i18n';
import { Icon } from './Icon';
import { Text } from './text';

export type OcrSource = { image: string } | { pdf: string; page: number };

export function useOcr(notify?: (msg: string) => void) {
  const [state, setState] = useState<{ busy: boolean; text: string | null } | null>(null);
  const run = (src: OcrSource) => {
    setState({ busy: true, text: null });
    const job = 'image' in src ? core.ocrImage(src.image) : core.ocrPdfPage(src.pdf, src.page);
    job
      .then((text) => {
        if (text == null) {
          setState(null);
          notify?.(t('Texterkennung ist hier nicht möglich.'));
        } else {
          setState({ busy: false, text });
        }
      })
      .catch(() => setState(null));
  };
  const sheet = state ? <OcrSheet busy={state.busy} text={state.text} onClose={() => setState(null)} notify={notify} /> : null;
  return { run, sheet };
}

function OcrSheet({ busy, text, onClose, notify }: { busy: boolean; text: string | null; onClose: () => void; notify?: (m: string) => void }) {
  return (
    <Modal visible transparent animationType="fade" statusBarTranslucent onRequestClose={onClose}>
      <View style={styles.bg}>
        <View style={styles.card}>
          <View style={styles.head}>
            <Text style={styles.title}>{t('Erkannter Text')}</Text>
            <Pressable onPress={onClose} style={styles.btn} accessibilityLabel={t('Schließen')}>
              <Icon name="close" size={22} color="#fff" />
            </Pressable>
          </View>
          {busy ? (
            <View style={styles.busy}>
              <ActivityIndicator color="#fff" />
              <Text style={styles.muted}>{t('Text wird erkannt …')}</Text>
            </View>
          ) : text ? (
            <>
              <ScrollView style={styles.scroll}>
                <Text style={styles.text} selectable>{text}</Text>
              </ScrollView>
              <Pressable
                onPress={() => core.copyText(text).then(() => notify?.(t('Text kopiert.'))).catch(() => {})}
                style={styles.copy}
                accessibilityRole="button"
              >
                <Icon name="copy" size={20} color="#0B1F1A" />
                <Text style={styles.copyText}>{t('Kopieren')}</Text>
              </Pressable>
            </>
          ) : (
            <Text style={[styles.muted, styles.pad]}>{t('Kein Text gefunden.')}</Text>
          )}
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  bg: { flex: 1, backgroundColor: 'rgba(0,0,0,0.6)', justifyContent: 'flex-end' },
  card: { backgroundColor: '#1C1C1A', borderTopLeftRadius: 20, borderTopRightRadius: 20, maxHeight: '80%', paddingBottom: 24 },
  head: { flexDirection: 'row', alignItems: 'center', paddingLeft: 20, paddingRight: 4, paddingTop: 8 },
  title: { flex: 1, color: '#fff', fontSize: 18, lineHeight: 24, fontWeight: '700' },
  btn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
  busy: { flexDirection: 'row', gap: 12, alignItems: 'center', padding: 24 },
  muted: { color: 'rgba(255,255,255,0.7)', fontSize: 15, lineHeight: 22 },
  pad: { padding: 24 },
  scroll: { paddingHorizontal: 20 },
  text: { color: '#fff', fontSize: 15, lineHeight: 22 },
  copy: { flexDirection: 'row', gap: 8, alignSelf: 'flex-end', alignItems: 'center', backgroundColor: '#9FE3C8', borderRadius: 22, paddingHorizontal: 18, height: 44, marginTop: 12, marginRight: 20 },
  copyText: { color: '#0B1F1A', fontSize: 15, fontWeight: '700' },
});
