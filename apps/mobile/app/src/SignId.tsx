// Digitale ID zum Signieren: Anlegen (Name + Passwort) und Passwortabfrage beim Signieren.
// Der Schlüssel entsteht im Kern (RustCrypto, P-256) und liegt nur verschlüsselt vor.
import { useCallback, useEffect, useState } from 'react';
import { ActivityIndicator, Alert, Modal, Pressable, ScrollView, StyleSheet, View } from 'react-native';
import SignaturePad from './SignaturePad';
import { Icon, type IconName } from './Icon';
import { snapTheme, type Theme } from './theme';
import { themed } from './ui';
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
export function groupFingerprint(fp: string, lines = false): string {
  const g = (fp.toUpperCase().match(/.{1,4}/g) ?? []).slice(0, 8);
  return lines ? `${g.slice(0, 4).join(' ')}\n${g.slice(4).join(' ')}` : g.join(' ');
}

function Sheet({ title, children, onClose, theme }: { title: string; children: React.ReactNode; onClose: () => void; theme: Theme }) {
  const styles = dialogStyles(theme);
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

export function CreateId({ onDone, onCancel, theme = snapTheme(true) }: { onDone: (id: IdInfo) => void; onCancel: () => void; theme?: Theme }) {
  const styles = dialogStyles(theme);
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
    <Sheet title={t('Digitale ID erstellen')} onClose={onCancel} theme={theme}>
      <Text style={styles.hint}>
        {t('Damit lassen sich PDFs fälschungssicher signieren. Empfänger sehen „unverändert seit dem Signieren“; ohne Zertifizierungsstelle gilt die Identität als nicht bestätigt.')}
      </Text>
      <TextInput style={styles.input} placeholder={t('Name (erscheint in der Signatur)')} placeholderTextColor={theme.text3 as string} value={name} onChangeText={setName} maxLength={64} accessibilityLabel={t('Name')} />
      <TextInput style={styles.input} placeholder={t('Passwort (mindestens 8 Zeichen)')} placeholderTextColor={theme.text3 as string} value={pw} onChangeText={setPw} secureTextEntry autoCapitalize="none" autoCorrect={false} accessibilityLabel={t('Passwort')} />
      <TextInput style={styles.input} placeholder={t('Passwort wiederholen')} placeholderTextColor={theme.text3 as string} value={pw2} onChangeText={setPw2} secureTextEntry autoCapitalize="none" autoCorrect={false} accessibilityLabel={t('Passwort wiederholen')} />
      <Text style={styles.small}>{t('Das Passwort lässt sich nicht wiederherstellen. Ohne Passwort kann mit dieser ID nicht mehr signiert werden.')}</Text>
      {error && <Text style={styles.error}>{error}</Text>}
      <View style={styles.row}>
        <Pressable onPress={onCancel} style={styles.btn} accessibilityRole="button">
          <Text style={styles.btnText}>{t('Abbrechen')}</Text>
        </Pressable>
        <Pressable onPress={create} disabled={!valid || busy} style={styles.btn} accessibilityRole="button">
          {busy ? <ActivityIndicator color={theme.accent as string} /> : <Text style={[styles.btnText, styles.strong, !valid && styles.dim]}>{t('Erstellen')}</Text>}
        </Pressable>
      </View>
    </Sheet>
  );
}

/** Passwort der ID abfragen (bei falschem Passwort mit Hinweis erneut). */
export function AskPassword({ wrong, busy, onSubmit, onCancel, theme = snapTheme(true) }: { wrong: boolean; busy: boolean; onSubmit: (pw: string) => void; onCancel: () => void; theme?: Theme }) {
  const styles = dialogStyles(theme);
  const [pw, setPw] = useState('');
  return (
    <Sheet title={t('Signieren')} onClose={onCancel} theme={theme}>
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
          {busy ? <ActivityIndicator color={theme.accent as string} /> : <Text style={[styles.btnText, styles.strong, !pw && styles.dim]}>{t('Signieren')}</Text>}
        </Pressable>
      </View>
    </Sheet>
  );
}

const dialogStyles = themed((th: Theme) =>
  StyleSheet.create({
    bg: { flex: 1, backgroundColor: 'rgba(10,10,9,0.5)', justifyContent: 'center', padding: 24 },
    card: { backgroundColor: th.surface, borderRadius: 16, paddingHorizontal: 24, paddingTop: 22, paddingBottom: 12, gap: 12 },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    hint: { color: th.text2, fontSize: 14, lineHeight: 20 },
    small: { color: th.text3, fontSize: 12, lineHeight: 17 },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 1.5, borderBottomColor: th.accent, paddingVertical: 8 },
    error: { color: th.danger, fontSize: 14 },
    row: { flexDirection: 'row', justifyContent: 'flex-end', gap: 4, marginTop: 4 },
    btn: { minHeight: 48, minWidth: 64, paddingHorizontal: 14, justifyContent: 'center', alignItems: 'center' },
    btnText: { color: th.accent, fontSize: 15 },
    strong: { fontWeight: '700' },
    dim: { opacity: 0.4 },
  }));

/** Einstellungsseite „Digitale ID“: anzeigen, anlegen, exportieren, löschen, Unterschrift neu. */
export function SignIdPage({ notify, downloads, theme }: { notify: (t: string) => void; downloads: () => Promise<string>; theme: Theme }) {
  const [st, setSt] = useState<{ id: IdInfo | null; has_signature: boolean } | null>(null);
  const [flow, setFlow] = useState<null | 'create' | 'pad'>(null);
  const load = useCallback(() => {
    idInfo().then(setSt).catch((e) => notify(String(e?.message ?? e)));
  }, [notify]);
  // Wichtig: der Effekt gibt nichts zurück (eine zurückgegebene Promise ließe React beim Verlassen abstürzen)
  useEffect(() => {
    load();
  }, [load]);
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
  const ps = pageStyles(theme);
  const row = (icon: IconName, label: string, onPress: () => void, danger = false) => (
    <Pressable onPress={onPress} style={ps.row} accessibilityRole="button" android_ripple={{ color: theme.accentSoft as string }}>
      <Icon name={icon} size={22} color={(danger ? theme.danger : theme.text2) as string} />
      <Text style={[ps.rowText, danger && { color: theme.danger }]}>{label}</Text>
    </Pressable>
  );
  return (
    <ScrollView contentContainerStyle={ps.page}>
      {st === null ? (
        <ActivityIndicator style={ps.loading} color={theme.accent as string} />
      ) : st.id ? (
        <View style={ps.idCard}>
          <View style={ps.idTop}>
            <Icon name="shieldCheck" size={28} color={theme.accent as string} />
            <View style={ps.grow}>
              <Text style={ps.title}>{st.id.name}</Text>
              <Text style={ps.meta}>{t('Selbst signierte ID · P-256')}</Text>
            </View>
          </View>
          <Text style={ps.label}>{t('Fingerabdruck')}</Text>
          <Text style={ps.mono} selectable>{groupFingerprint(st.id.fingerprint, true)}</Text>
          <Text style={ps.meta}>
            {t('Empfänger können diesen Fingerabdruck mit dem in der Signatur vergleichen, um sicherzugehen, dass sie von dir stammt.')}
          </Text>
        </View>
      ) : (
        <View style={ps.empty}>
          <View style={ps.emptyIcon}>
            <Icon name="shield" size={28} color={theme.text3 as string} />
          </View>
          <Text style={ps.title}>{t('Noch keine digitale ID')}</Text>
          <Text style={[ps.meta, ps.centerText]}>{t('Noch keine digitale ID. Sie wird zum Signieren von PDFs gebraucht.')}</Text>
          <Pressable onPress={() => setFlow('create')} style={ps.btn} accessibilityRole="button">
            <Text style={ps.btnText}>{t('Digitale ID erstellen')}</Text>
          </Pressable>
        </View>
      )}
      {st && (
        <View style={ps.group}>
          {st.id && row('download', t('Exportieren (verschlüsselt, in „Download“)'), exportId)}
          {st.id && <View style={ps.sep} />}
          {row('sign', st.has_signature ? t('Unterschrift neu zeichnen') : t('Unterschrift zeichnen'), () => setFlow('pad'))}
          {st.id && <View style={ps.sep} />}
          {st.id && row('trash', t('Digitale ID löschen'), remove, true)}
        </View>
      )}
      <Text style={ps.foot}>
        {t('Auf ein anderes Gerät übertragen: exportierte Datei (.snapid) dort in Snap antippen → „Als digitale ID importieren“.')}
      </Text>
      {flow === 'create' && <CreateId theme={theme} onDone={() => { setFlow(null); load(); }} onCancel={() => setFlow(null)} />}
      {flow === 'pad' && <SignaturePad theme={theme} onDone={() => { setFlow(null); load(); }} onCancel={() => setFlow(null)} />}
    </ScrollView>
  );
}

const pageStyles = themed((th: Theme) =>
  StyleSheet.create({
    page: { paddingHorizontal: 16, paddingTop: 8, paddingBottom: 40, gap: 16 },
    loading: { marginTop: 48 },
    grow: { flex: 1 },
    idCard: { borderRadius: 12, padding: 16, gap: 6, backgroundColor: th.surface },
    idTop: { flexDirection: 'row', alignItems: 'center', gap: 14, marginBottom: 8 },
    title: { color: th.text, fontSize: 18, lineHeight: 24, fontWeight: '700' },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
    label: { color: th.text3, fontSize: 12, lineHeight: 16, letterSpacing: 1, textTransform: 'uppercase', fontWeight: '600', marginTop: 4 },
    mono: { color: th.text, fontSize: 16, lineHeight: 24, letterSpacing: 1.5 },
    group: { borderRadius: 12, backgroundColor: th.surface, overflow: 'hidden' },
    row: { flexDirection: 'row', alignItems: 'center', gap: 18, minHeight: 56, paddingHorizontal: 16 },
    rowText: { flex: 1, color: th.text, fontSize: 16, lineHeight: 22 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.line, marginLeft: 56 },
    foot: { color: th.text3, fontSize: 13, lineHeight: 18, marginHorizontal: 4 },
    empty: { alignItems: 'center', gap: 6, paddingTop: 40, paddingBottom: 8, paddingHorizontal: 24 },
    emptyIcon: { width: 64, height: 64, borderRadius: 32, justifyContent: 'center', alignItems: 'center', marginBottom: 10, backgroundColor: th.surface2 },
    centerText: { textAlign: 'center' },
    btn: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 24, borderRadius: 24, backgroundColor: th.accent, marginTop: 16 },
    btnText: { color: th.onAccent, fontSize: 16, fontWeight: '700' },
  }));
