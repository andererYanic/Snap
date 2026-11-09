// Digitale ID zum Signieren: Anlegen (Name + Passwort) und Passwortabfrage beim Signieren.
// Der Schlüssel entsteht im Kern (RustCrypto, P-256) und liegt nur verschlüsselt vor.
import { useEffect, useState } from 'react';
import { ActivityIndicator, Alert, Modal, Pressable, ScrollView, StyleSheet, View } from 'react-native';
import SignaturePad from './SignaturePad';
import { request } from './core';
import { t } from './i18n';
import { Text, TextInput } from './text';

export interface IdInfo {
  name: string;
  created: number;
  fingerprint: string;
}

export const idInfo = () => request<{ id: IdInfo | null; has_signature: boolean }>('SignIdInfo');

/** Fingerabdruck lesbar in Vierergruppen (zum Vergleichen mit Empfängern). */
export function groupFingerprint(fp: string): string {
  return (fp.toUpperCase().match(/.{1,4}/g) ?? []).slice(0, 8).join(' ');
}

function Sheet({ title, children, onClose }: { title: string; children: React.ReactNode; onClose: () => void }) {
  return (
    <Modal visible transparent animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <View style={styles.bg}>
        <View style={styles.card}>
          <Text style={styles.title}>{title}</Text>
          {children}
        </View>
      </View>
    </Modal>
  );
}

export function CreateId({ onDone, onCancel }: { onDone: (id: IdInfo) => void; onCancel: () => void }) {
  const [name, setName] = useState('');
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const valid = name.trim().length > 0 && pw.length >= 8 && pw === pw2;
  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await request<{ id: IdInfo | null }>({ SignIdCreate: { name: name.trim(), password: pw } });
      if (r.id) {
        onDone(r.id);
      }
    } catch (e: any) {
      setError(String(e?.message ?? e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet title={t('Digitale ID erstellen')} onClose={onCancel}>
      <Text style={styles.hint}>
        {t('Damit lassen sich PDFs fälschungssicher signieren. Empfänger sehen „unverändert seit dem Signieren“; ohne Zertifizierungsstelle gilt die Identität als nicht bestätigt.')}
      </Text>
      <TextInput style={styles.input} placeholder={t('Name (erscheint in der Signatur)')} placeholderTextColor="#8a8f9c" value={name} onChangeText={setName} maxLength={64} accessibilityLabel={t('Name')} />
      <TextInput style={styles.input} placeholder={t('Passwort (mindestens 8 Zeichen)')} placeholderTextColor="#8a8f9c" value={pw} onChangeText={setPw} secureTextEntry autoCapitalize="none" autoCorrect={false} accessibilityLabel={t('Passwort')} />
      <TextInput style={styles.input} placeholder={t('Passwort wiederholen')} placeholderTextColor="#8a8f9c" value={pw2} onChangeText={setPw2} secureTextEntry autoCapitalize="none" autoCorrect={false} accessibilityLabel={t('Passwort wiederholen')} />
      <Text style={styles.small}>{t('Das Passwort lässt sich nicht wiederherstellen. Ohne Passwort kann mit dieser ID nicht mehr signiert werden.')}</Text>
      {error && <Text style={styles.error}>{error}</Text>}
      <View style={styles.row}>
        <Pressable onPress={onCancel} style={styles.btn} accessibilityRole="button">
          <Text style={styles.btnText}>{t('Abbrechen')}</Text>
        </Pressable>
        <Pressable onPress={create} disabled={!valid || busy} style={styles.btn} accessibilityRole="button">
          {busy ? <ActivityIndicator color="#9fbcff" /> : <Text style={[styles.btnText, styles.strong, !valid && styles.dim]}>{t('Erstellen')}</Text>}
        </Pressable>
      </View>
    </Sheet>
  );
}

/** Passwort der ID abfragen (bei falschem Passwort mit Hinweis erneut). */
export function AskPassword({ wrong, busy, onSubmit, onCancel }: { wrong: boolean; busy: boolean; onSubmit: (pw: string) => void; onCancel: () => void }) {
  const [pw, setPw] = useState('');
  return (
    <Sheet title={t('Signieren')} onClose={onCancel}>
      <Text style={styles.hint}>{wrong ? t('Falsches Passwort.') : t('Passwort der digitalen ID eingeben.')}</Text>
      <TextInput
        style={styles.input}
        autoFocus
        value={pw}
        onChangeText={setPw}
        secureTextEntry
        autoCapitalize="none"
        autoCorrect={false}
        accessibilityLabel={t('Passwort')}
        onSubmitEditing={() => pw && onSubmit(pw)}
      />
      <View style={styles.row}>
        <Pressable onPress={onCancel} style={styles.btn} accessibilityRole="button">
          <Text style={styles.btnText}>{t('Abbrechen')}</Text>
        </Pressable>
        <Pressable onPress={() => onSubmit(pw)} disabled={!pw || busy} style={styles.btn} accessibilityRole="button">
          {busy ? <ActivityIndicator color="#9fbcff" /> : <Text style={[styles.btnText, styles.strong, !pw && styles.dim]}>{t('Signieren')}</Text>}
        </Pressable>
      </View>
    </Sheet>
  );
}

const styles = StyleSheet.create({
  bg: { flex: 1, backgroundColor: 'rgba(0,0,0,0.55)', justifyContent: 'center', padding: 24 },
  card: { backgroundColor: '#24262e', borderRadius: 24, padding: 20, gap: 10 },
  title: { color: '#f1f1f4', fontSize: 18, fontWeight: '700' },
  hint: { color: '#b4b6be', fontSize: 14 },
  small: { color: '#8a8f9c', fontSize: 12 },
  input: { color: '#f1f1f4', fontSize: 16, borderBottomWidth: 2, borderBottomColor: '#9fbcff', paddingVertical: 8 },
  error: { color: '#ffb4a8' },
  row: { flexDirection: 'row', justifyContent: 'flex-end', gap: 8, marginTop: 4 },
  btn: { minHeight: 44, minWidth: 64, paddingHorizontal: 12, justifyContent: 'center', alignItems: 'center' },
  btnText: { color: '#9fbcff', fontSize: 15 },
  strong: { fontWeight: '700' },
  dim: { opacity: 0.4 },
});

/** Einstellungsseite „Digitale ID“: anzeigen, anlegen, exportieren, löschen, Unterschrift neu. */
export function SignIdPage({ notify, downloads, theme }: { notify: (t: string) => void; downloads: () => Promise<string>; theme: { card: any; text: any; text2: any; accent: any; danger: any } }) {
  const [st, setSt] = useState<{ id: IdInfo | null; has_signature: boolean } | null>(null);
  const [flow, setFlow] = useState<null | 'create' | 'pad'>(null);
  const load = () => idInfo().then(setSt).catch((e) => notify(String(e?.message ?? e)));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffectOnce(load);
  const exportId = async () => {
    try {
      const dir = await downloads();
      const r = await request<{ path: string }>({ SignIdExport: { dest: dir } });
      notify(t('Exportiert nach {path}. Die Datei ist mit deinem Passwort verschlüsselt.', { path: r.path }));
    } catch (e: any) {
      notify(String(e?.message ?? e));
    }
  };
  const remove = () =>
    Alert.alert(t('Digitale ID löschen?'), t('Ohne Export lässt sie sich nicht wiederherstellen. Bereits signierte PDFs bleiben gültig.'), [
      { text: t('Abbrechen'), style: 'cancel' },
      { text: t('Löschen'), style: 'destructive', onPress: () => request('SignIdDelete').then(load).catch((e) => notify(String(e?.message ?? e))) },
    ]);
  const row = (label: string, onPress: () => void, danger = false) => (
    <Pressable onPress={onPress} style={pageStyles.row} accessibilityRole="button">
      <Text style={[pageStyles.rowText, { color: danger ? theme.danger : theme.accent }]}>{label}</Text>
    </Pressable>
  );
  return (
    <ScrollView contentContainerStyle={pageStyles.page}>
      <View style={[pageStyles.card, { backgroundColor: theme.card }]}>
        {st === null ? (
          <ActivityIndicator />
        ) : st.id ? (
          <>
            <Text style={[pageStyles.title, { color: theme.text }]}>{st.id.name}</Text>
            <Text style={[pageStyles.meta, { color: theme.text2 }]}>{t('Fingerabdruck')}</Text>
            <Text style={[pageStyles.mono, { color: theme.text }]} selectable>{groupFingerprint(st.id.fingerprint)}</Text>
            <Text style={[pageStyles.meta, { color: theme.text2 }]}>
              {t('Empfänger können diesen Fingerabdruck mit dem in der Signatur vergleichen, um sicherzugehen, dass sie von dir stammt.')}
            </Text>
          </>
        ) : (
          <Text style={[pageStyles.meta, { color: theme.text2 }]}>{t('Noch keine digitale ID. Sie wird zum Signieren von PDFs gebraucht.')}</Text>
        )}
      </View>
      {st && (
        <View style={[pageStyles.card, { backgroundColor: theme.card }]}>
          {!st.id && row(t('Digitale ID erstellen'), () => setFlow('create'))}
          {st.id && row(t('Exportieren (verschlüsselt, in „Download“)'), exportId)}
          {row(st.has_signature ? t('Unterschrift neu zeichnen') : t('Unterschrift zeichnen'), () => setFlow('pad'))}
          {st.id && row(t('Digitale ID löschen'), remove, true)}
        </View>
      )}
      <Text style={[pageStyles.meta, { color: theme.text2 }]}>
        {t('Auf ein anderes Gerät übertragen: exportierte Datei (.snapid) dort in Snap antippen → „Als digitale ID importieren“.')}
      </Text>
      {flow === 'create' && <CreateId onDone={() => { setFlow(null); load(); }} onCancel={() => setFlow(null)} />}
      {flow === 'pad' && <SignaturePad onDone={() => { setFlow(null); load(); }} onCancel={() => setFlow(null)} />}
    </ScrollView>
  );
}

function useEffectOnce(f: () => void) {
  useEffect(f, []); // eslint-disable-line react-hooks/exhaustive-deps
}

const pageStyles = StyleSheet.create({
  page: { padding: 16, gap: 14 },
  card: { borderRadius: 20, padding: 16, gap: 6 },
  title: { fontSize: 18, fontWeight: '700' },
  meta: { fontSize: 13 },
  mono: { fontSize: 15, letterSpacing: 1 },
  row: { minHeight: 48, justifyContent: 'center' },
  rowText: { fontSize: 15 },
});
