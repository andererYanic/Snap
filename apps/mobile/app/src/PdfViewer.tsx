// Eingebaute PDF-Anzeige (wie bei Google Drive, aber lokal): Android zeichnet die Seiten selbst
// (`PdfRenderer`, Teil des Systems). Seiten erscheinen beim Scrollen, Doppeltippen zoomt eine
// Seite in voller Schärfe heran.
import { memo, useEffect, useState } from 'react';
import { ActivityIndicator, Dimensions, FlatList, Image, Modal, PixelRatio, Pressable, StatusBar, StyleSheet, View } from 'react-native';
import { Text } from './text';
import { GestureHandlerRootView } from 'react-native-gesture-handler';
import Animated from 'react-native-reanimated';
import * as core from './core';
import { t } from './i18n';
import { fadeIn, m } from './motion';
import Viewer from './Viewer';

const { width: W } = Dimensions.get('window');
const GAP = 8;
const PAGE_W = W - 16;
const PX = Math.round(PAGE_W * PixelRatio.get());

const PdfPage = memo(function PdfPage({ path, index, ratio, onZoom }: { path: string; index: number; ratio: number; onZoom: (i: number) => void }) {
  const [uri, setUri] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    core.pdfPage(path, index, PX).then((f) => alive && f && setUri(`file://${f}`)).catch(() => {});
    return () => {
      alive = false;
    };
  }, [path, index]);
  return (
    <Pressable onPress={() => onZoom(index)} style={[styles.page, { width: PAGE_W, height: PAGE_W * ratio }]} accessibilityLabel={t('Seite {n}', { n: index + 1 })}>
      {uri ? <Image source={{ uri }} style={styles.fill} fadeDuration={120} /> : <ActivityIndicator style={styles.fill} color="#8a8f9c" />}
    </Pressable>
  );
});

export default function PdfViewer({ path, name, onClose }: { path: string; name: string; onClose: () => void }) {
  const [info, setInfo] = useState<{ pages: number; ratios: number[] } | null | 'error'>(null);
  const [page, setPage] = useState(0);
  const [zoom, setZoom] = useState<{ uri: string; name: string } | null>(null);

  useEffect(() => {
    core.pdfInfo(path).then((i) => setInfo(i && i.pages > 0 ? i : 'error')).catch(() => setInfo('error'));
  }, [path]);

  // Doppelt so fein rendern und in der Bildanzeige öffnen (Zoomen, Verschieben)
  const openZoom = (i: number) => {
    core.pdfPage(path, i, Math.min(4096, PX * 2)).then((f) => f && setZoom({ uri: f, name: t('Seite {n}', { n: i + 1 }) })).catch(() => {});
  };

  const ok = info && info !== 'error' ? info : null;
  const offsets = ok ? ok.ratios.reduce<number[]>((acc, r, i) => (acc.push(i === 0 ? 0 : acc[i - 1] + PAGE_W * ok.ratios[i - 1] + GAP), acc), []) : [];

  return (
    <Modal visible transparent={false} animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <GestureHandlerRootView style={styles.root}>
        <StatusBar barStyle="light-content" />
        <View style={styles.top}>
          <Pressable onPress={onClose} style={styles.btn} accessibilityLabel={t('Schließen')}>
            <Text style={styles.btnText}>✕</Text>
          </Pressable>
          <View style={styles.titleBox}>
            <Text style={styles.title} numberOfLines={1}>{name}</Text>
            {ok && <Text style={styles.counter}>{t('Seite {n} von {total}', { n: page + 1, total: ok.pages })}</Text>}
          </View>
          <Pressable onPress={() => core.printFile(path).catch(() => {})} style={styles.btn} accessibilityLabel={t('Drucken')}>
            <Text style={styles.small}>{t('Drucken')}</Text>
          </Pressable>
          <Pressable onPress={() => core.openFile(path).catch(() => {})} style={styles.btn} accessibilityLabel={t('Öffnen mit …')}>
            <Text style={styles.btnText}>⋯</Text>
          </Pressable>
        </View>
        {info === null ? (
          <ActivityIndicator style={styles.fill} color="#fff" />
        ) : info === 'error' ? (
          <Animated.View entering={m(fadeIn)} style={styles.error}>
            <Text style={styles.errorText}>{t('Dieses PDF lässt sich nicht anzeigen.')}</Text>
            <Pressable onPress={() => core.openFile(path).catch(() => {})} style={styles.errorBtn}>
              <Text style={styles.small}>{t('Mit anderer App öffnen')}</Text>
            </Pressable>
          </Animated.View>
        ) : (
          <FlatList
            data={Array.from({ length: info.pages }, (_, i) => i)}
            keyExtractor={(i) => String(i)}
            contentContainerStyle={styles.list}
            getItemLayout={(_, i) => ({ length: PAGE_W * info.ratios[i] + GAP, offset: offsets[i], index: i })}
            initialNumToRender={3}
            maxToRenderPerBatch={3}
            windowSize={5}
            removeClippedSubviews
            onViewableItemsChanged={({ viewableItems }) => viewableItems[0] && setPage(viewableItems[0].index ?? 0)}
            viewabilityConfig={{ itemVisiblePercentThreshold: 40 }}
            renderItem={({ item: i }) => <PdfPage path={path} index={i} ratio={info.ratios[i]} onZoom={openZoom} />}
          />
        )}
        {zoom && <Viewer items={[{ path: zoom.uri, name: zoom.name }]} start={0} onClose={() => setZoom(null)} />}
      </GestureHandlerRootView>
    </Modal>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#1c1d22' },
  fill: { flex: 1 },
  top: { flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 8, paddingBottom: 8, backgroundColor: '#25262c' },
  btn: { minWidth: 48, height: 48, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 8 },
  btnText: { color: '#fff', fontSize: 22 },
  small: { color: '#fff', fontSize: 15, fontWeight: '600' },
  titleBox: { flex: 1, marginHorizontal: 4 },
  title: { color: '#fff', fontSize: 15, fontWeight: '600' },
  counter: { color: '#bbb', fontSize: 12 },
  list: { padding: 8, alignItems: 'center' },
  page: { marginBottom: GAP, backgroundColor: '#fff', borderRadius: 2, overflow: 'hidden' },
  error: { flex: 1, alignItems: 'center', justifyContent: 'center', padding: 24 },
  errorText: { color: '#eee', fontSize: 15, textAlign: 'center', marginBottom: 12 },
  errorBtn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: 'rgba(255,255,255,0.15)' },
});
