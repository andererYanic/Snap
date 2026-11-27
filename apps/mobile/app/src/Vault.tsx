// Tresor: Dateien mit Passwort verschlüsselt auf dem Gerät. Einrichten (Passwort + einmal
// angezeigter Wiederherstellungscode), Entsperren, Inhalt ansehen, herausholen, löschen.
// Der Kern sperrt nach 5 Minuten ohne Benutzung; die App sperrt zusätzlich im Hintergrund.
import { useCallback, useEffect, useState } from 'react';
import { ActivityIndicator, Alert, FlatList, Pressable, StyleSheet, View } from 'react-native';
import * as core from './core';
import { humanSize, request } from './core';
import { locale, t } from './i18n';
import { Icon } from './Icon';
import { Text, TextInput } from './text';
import type { Theme } from './theme';
import { Glyph, themed } from './ui';

/** Zeitpunkt, zu dem eine Tresor-Datei an eine andere App übergeben wurde: Dann sperrt die App im
 *  Hintergrund nicht sofort (die andere App liest die Datei noch); der Kern sperrt nach 5 Minuten. */
export const handoff = { at: 0 };

interface Status { exists: boolean; unlocked: boolean; entries: number | null; retry_after_secs: number }
interface Entry { id: string; name: string; size: number; added: number }

export function VaultPage({ theme, notify, onOpen }: {
  theme: Theme;
  notify: (m: string) => void;
  /** Geöffnete (entschlüsselte) Datei mit den eingebauten Anzeigen zeigen. */
  onOpen: (path: string, name: string) => void;
}) {
  const s = styles(theme);
  const [status, setStatus] = useState<Status | null>(null);
  const [entries, setEntries] = useState<Entry[] | null>(null);
  const [pw, setPw] = useState('');
  const [pw2, setPw2] = useState('');
  const [code, setCode] = useState<string | null>(null);
  const [recover, setRecover] = useState(false);
  const [recCode, setRecCode] = useState('');
  const [busy, setBusy] = useState(false);
  const fail = (e: any) => notify(e?.message ?? String(e));

  const load = useCallback(async () => {
    try {
      const st = await request<Status>('VaultStatus');
      setStatus(st);
      setEntries(st.unlocked ? (await request<{ entries: Entry[] }>('VaultList')).entries : null);
    } catch (e) {
      fail(e);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    load();
    return core.onEvent((ev) => ev.type === 'VaultLocked' && load());
  }, [load]);

  const busyRun = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
      setPw('');
      setPw2('');
    }
  };
  const create = () =>
    pw !== pw2
      ? notify(t('Die Passwörter stimmen nicht überein.'))
      : busyRun(async () => {
          const r = await request<{ recovery_code: string }>({ VaultCreate: { password: pw } });
          setCode(r.recovery_code);
          await load();
        });
  const unlock = () => busyRun(async () => { await request({ VaultUnlock: { password: pw } }); await load(); });
  const doRecover = () =>
    pw !== pw2
      ? notify(t('Die Passwörter stimmen nicht überein.'))
      : busyRun(async () => {
          await request({ VaultRecover: { code: recCode, new_password: pw } });
          setRecover(false);
          setRecCode('');
          await load();
        });
  const lock = () => request('VaultLock').then(load).catch(fail);
  const open = (e: Entry) =>
    busyRun(async () => {
      const r = await request<{ path: string }>({ VaultOpen: { id: e.id } });
      onOpen(r.path, e.name.split('/').pop() ?? e.name);
    });
  const exportTo = (e: Entry) =>
    busyRun(async () => {
      const r = await request<{ path: string }>({ VaultExport: { id: e.id, dest_dir: `${await core.storageRoot()}/Download` } });
      notify(t('Unverschlüsselt gespeichert: {path}', { path: r.path.replace(/^\/storage\/emulated\/0\//, '') }));
    });
  const remove = (e: Entry) =>
    Alert.alert(t('Endgültig löschen'), t('„{name}“ aus dem Tresor löschen? Das lässt sich nicht rückgängig machen.', { name: e.name }), [
      { text: t('Abbrechen'), style: 'cancel' },
      { text: t('Löschen'), style: 'destructive', onPress: () => request({ VaultRemove: { id: e.id } }).then(load).catch(fail) },
    ]);
  const menu = (e: Entry) =>
    Alert.alert(e.name, undefined, [
      { text: t('In „Download“ holen'), onPress: () => exportTo(e) },
      { text: t('Löschen'), style: 'destructive', onPress: () => remove(e) },
      { text: t('Abbrechen'), style: 'cancel' },
    ]);

  const field = (value: string, set: (v: string) => void, label: string) => (
    <TextInput value={value} onChangeText={set} secureTextEntry autoCorrect={false} autoCapitalize="none" placeholder={label} placeholderTextColor={theme.text3 as string} style={s.input} accessibilityLabel={label} />
  );

  if (!status) {
    return <ActivityIndicator style={s.loading} color={theme.accent as string} />;
  }
  // Wiederherstellungscode nach dem Anlegen: einmal zeigen, Bestätigung verlangen
  if (code) {
    return (
      <View style={s.page}>
        <Icon name="key" size={36} color={theme.accent as string} />
        <Text style={s.title}>{t('Wiederherstellungscode')}</Text>
        <Text style={s.code} selectable>{code}</Text>
        <Text style={s.text}>{t('Schreib ihn jetzt auf und bewahre ihn sicher auf. Er wird nie wieder angezeigt. Ohne Passwort und ohne diesen Code ist der Inhalt des Tresors verloren.')}</Text>
        <Pressable onPress={() => setCode(null)} style={s.primary} accessibilityRole="button">
          <Text style={s.primaryText}>{t('Ich habe ihn aufgeschrieben')}</Text>
        </Pressable>
      </View>
    );
  }
  if (!status.exists || recover) {
    return (
      <View style={s.page}>
        <Icon name="shield" size={36} color={theme.accent as string} />
        <Text style={s.title}>{recover ? t('Neues Passwort setzen') : t('Tresor einrichten')}</Text>
        <Text style={s.text}>
          {recover
            ? t('Mit dem Wiederherstellungscode, den du beim Einrichten aufgeschrieben hast.')
            : t('Dateien im Tresor sind mit deinem Passwort verschlüsselt – auch Namen. Sie werden nie freigegeben, abgeglichen oder durchsucht.')}
        </Text>
        {recover && (
          <TextInput value={recCode} onChangeText={setRecCode} autoCorrect={false} autoCapitalize="characters" placeholder={t('Wiederherstellungscode')} placeholderTextColor={theme.text3 as string} style={s.input} accessibilityLabel={t('Wiederherstellungscode')} />
        )}
        {field(pw, setPw, t('Passwort (mind. 8 Zeichen)'))}
        {field(pw2, setPw2, t('Passwort wiederholen'))}
        <Pressable onPress={recover ? doRecover : create} disabled={busy || pw.length < 8} style={[s.primary, (busy || pw.length < 8) && s.disabled]} accessibilityRole="button">
          {busy ? <ActivityIndicator color="#0B1F1A" /> : <Text style={s.primaryText}>{recover ? t('Passwort setzen') : t('Einrichten')}</Text>}
        </Pressable>
        {recover && (
          <Pressable onPress={() => setRecover(false)} style={s.link}><Text style={s.linkText}>{t('Abbrechen')}</Text></Pressable>
        )}
      </View>
    );
  }
  if (!status.unlocked) {
    return (
      <View style={s.page}>
        <Icon name="shield" size={36} color={theme.text2 as string} />
        <Text style={s.title}>{t('Tresor gesperrt')}</Text>
        {status.retry_after_secs > 0 && <Text style={s.text}>{t('Zu viele falsche Versuche – bitte {n} s warten.', { n: status.retry_after_secs })}</Text>}
        {field(pw, setPw, t('Passwort'))}
        <Pressable onPress={unlock} disabled={busy || !pw} style={[s.primary, (busy || !pw) && s.disabled]} accessibilityRole="button">
          {busy ? <ActivityIndicator color="#0B1F1A" /> : <Text style={s.primaryText}>{t('Entsperren')}</Text>}
        </Pressable>
        <Pressable onPress={() => setRecover(true)} style={s.link}><Text style={s.linkText}>{t('Passwort vergessen?')}</Text></Pressable>
      </View>
    );
  }
  return (
    <View style={s.fill}>
      <View style={s.bar}>
        <Text style={[s.muted, s.grow]}>{t('Entsperrt · sperrt nach 5 Minuten ohne Benutzung')}</Text>
        <Pressable onPress={lock} style={s.lockBtn} accessibilityRole="button" accessibilityLabel={t('Sperren')}>
          <Icon name="shield" size={20} color={theme.accent as string} />
          <Text style={s.linkText}>{t('Sperren')}</Text>
        </Pressable>
      </View>
      {busy && <ActivityIndicator color={theme.accent as string} />}
      <FlatList
        data={entries ?? []}
        keyExtractor={(e) => e.id}
        ListEmptyComponent={<Text style={s.note}>{t('Noch leer. In „Dateien“ Dateien auswählen › Mehr › „In den Tresor“.')}</Text>}
        renderItem={({ item }) => (
          <Pressable onPress={() => open(item)} onLongPress={() => menu(item)} style={s.row} android_ripple={{ color: theme.accentSoft as string }} accessibilityLabel={item.name}>
            <Glyph name={item.name} dir={false} theme={theme} />
            <View style={s.grow}>
              <Text style={s.text} numberOfLines={1}>{item.name}</Text>
              <Text style={s.muted}>{humanSize(item.size, locale())}</Text>
            </View>
            <Pressable onPress={() => menu(item)} style={s.lockBtn} accessibilityLabel={t('Mehr')} hitSlop={8}>
              <Icon name="more" size={22} color={theme.text2 as string} strokeWidth={2.4} />
            </Pressable>
          </Pressable>
        )}
      />
    </View>
  );
}

const styles = themed((th) =>
  StyleSheet.create({
    fill: { flex: 1 },
    grow: { flex: 1 },
    loading: { marginTop: 48 },
    page: { padding: 24, gap: 14, alignItems: 'stretch' },
    title: { color: th.text, fontSize: 20, lineHeight: 26, fontWeight: '700' },
    text: { color: th.text, fontSize: 15, lineHeight: 22 },
    muted: { color: th.text2, fontSize: 13, lineHeight: 18 },
    note: { color: th.text2, fontSize: 15, lineHeight: 22, padding: 24 },
    code: { color: th.text, fontSize: 20, lineHeight: 30, fontWeight: '700', letterSpacing: 1, textAlign: 'center', backgroundColor: th.surface2, borderRadius: 12, padding: 16 },
    input: { color: th.text, fontSize: 16, borderWidth: 1, borderColor: th.surface2, borderRadius: 12, paddingHorizontal: 14, height: 48 },
    primary: { backgroundColor: th.accent, borderRadius: 24, height: 48, alignItems: 'center', justifyContent: 'center' },
    primaryText: { color: '#0B1F1A', fontSize: 16, fontWeight: '700' },
    disabled: { opacity: 0.5 },
    link: { alignSelf: 'center', padding: 8 },
    linkText: { color: th.accent, fontSize: 15, fontWeight: '600' },
    bar: { flexDirection: 'row', alignItems: 'center', paddingHorizontal: 16, paddingVertical: 8 },
    lockBtn: { flexDirection: 'row', alignItems: 'center', gap: 6, padding: 8 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 14, paddingHorizontal: 16, minHeight: 60 },
  }),
);
