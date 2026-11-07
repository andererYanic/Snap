// Geräte (Status, Koppeln per Code) und „Mehr“ (Papierkorb, Einstellungen).
import { useCallback, useEffect, useState } from 'react';
import { Alert, Pressable, ScrollView, StyleSheet, Switch, Text, TextInput, View } from 'react-native';
import { request } from './core';
import { LANGUAGES, lang, locale, setLanguage, t } from './i18n';
import type { Theme } from './theme';

interface Device { node_id: string; name: string; paired_at: number; grants: { capability: string; folder: string | null }[] }

export function Devices({ theme, notify }: { theme: Theme; notify: (t: string) => void }) {
  const s = styles(theme);
  const [devices, setDevices] = useState<Device[]>([]);
  const [online, setOnline] = useState<Set<string>>(new Set());
  const [invite, setInvite] = useState<string | null>(null);
  const [code, setCode] = useState('');

  const load = useCallback(async () => {
    try {
      setDevices((await request<{ devices: Device[] }>('PairList')).devices);
      const peers = (await request<{ peers: { node_id: string; status: string }[] }>('Peers')).peers;
      setOnline(new Set(peers.filter((p) => p.status === 'online').map((p) => p.node_id)));
    } catch (e: any) {
      notify(e.message);
    }
  }, [notify]);

  useEffect(() => {
    load();
  }, [load]);

  const startInvite = async () => {
    try {
      const inv = await request<{ code: string }>('PairInvite');
      setInvite(inv.code);
      const r = await request<{ device: Device }>('PairInviteWait');
      setInvite(null);
      notify(t('„{name}“ ist jetzt gekoppelt. Rechte vergibst du einzeln.', { name: r.device.name }));
      load();
    } catch (e: any) {
      setInvite(null);
      notify(e.message);
    }
  };

  const join = async () => {
    try {
      const r = await request<{ device: Device }>({ PairJoin: { code: code.trim() } });
      setCode('');
      notify(t('Mit „{name}“ gekoppelt.', { name: r.device.name }));
      load();
    } catch (e: any) {
      notify(e.message);
    }
  };

  const unpair = (d: Device) =>
    Alert.alert(t('Gerät entkoppeln'), t('„{name}“ entkoppeln? Das Gerät verliert alle Rechte und sieht deine Freigaben nicht mehr.', { name: d.name }), [
      { text: t('Abbrechen'), style: 'cancel' },
      { text: t('Entkoppeln'), style: 'destructive', onPress: () => request({ PairRemove: { device: d.node_id } }).then(load).catch((e) => notify(e.message)) },
    ]);

  return (
    <ScrollView contentContainerStyle={s.page}>
      <Text style={s.h2}>{t('Gekoppelte Geräte')}</Text>
      {devices.length === 0 && <Text style={s.muted}>{t('Noch keine Geräte gekoppelt.')}</Text>}
      {devices.map((d) => (
        <Pressable key={d.node_id} style={s.card} onLongPress={() => unpair(d)}>
          <View style={[s.dot, online.has(d.node_id) && s.dotOn]} />
          <View style={s.grow}>
            <Text style={s.strong}>{d.name}</Text>
            <Text style={s.muted}>
              {online.has(d.node_id) ? t('Verbunden') : t('Nicht verbunden')} · {t('gekoppelt am {date}', { date: new Date(d.paired_at * 1000).toLocaleDateString(locale()) })}
            </Text>
          </View>
          <Pressable onPress={() => unpair(d)} style={s.btnSmall} accessibilityLabel={t('Entkoppeln')}>
            <Text style={s.link}>{t('Entkoppeln')}</Text>
          </Pressable>
        </Pressable>
      ))}
      <Text style={s.h2}>{t('Neues Gerät koppeln')}</Text>
      <View style={s.card}>
        <View style={s.grow}>
          <Text style={s.muted}>{t('Auf diesem Gerät einen Code erzeugen und am neuen Gerät eingeben.')}</Text>
          {invite ? <Text selectable style={s.code}>{invite}</Text> : (
            <Pressable onPress={startInvite} style={s.btn}><Text style={s.btnText}>{t('Code erzeugen')}</Text></Pressable>
          )}
        </View>
      </View>
      <View style={s.card}>
        <View style={s.grow}>
          <Text style={s.muted}>{t('Oder den Code eines anderen Geräts hier eingeben.')}</Text>
          <TextInput value={code} onChangeText={setCode} placeholder={t('Einladungscode')} placeholderTextColor={theme.text2 as string} autoCapitalize="none" autoCorrect={false} style={s.input} />
          <Pressable onPress={join} disabled={!code.trim()} style={[s.btn, !code.trim() && s.dim]}><Text style={s.btnText}>{t('Koppeln')}</Text></Pressable>
        </View>
      </View>
    </ScrollView>
  );
}

interface TrashEntry { id: string; name: string; original_path: string; deleted_at: string }

export function More({ theme, notify, material, setMaterial, materialAvailable }: {
  theme: Theme; notify: (t: string) => void; material: boolean; setMaterial: (v: boolean) => void; materialAvailable: boolean;
}) {
  const s = styles(theme);
  const [trash, setTrash] = useState<TrashEntry[] | null>(null);
  const loadTrash = () => request<{ entries: TrashEntry[] }>('TrashList').then((r) => setTrash(r.entries)).catch((e) => notify(e.message));

  return (
    <ScrollView contentContainerStyle={s.page}>
      <Text style={s.h2}>{t('Papierkorb')}</Text>
      <View style={s.card}>
        <View style={s.grow}>
          {trash === null ? (
            <Pressable onPress={loadTrash} style={s.btn}><Text style={s.btnText}>{t('Papierkorb')}</Text></Pressable>
          ) : trash.length === 0 ? (
            <Text style={s.muted}>{t('Der Papierkorb ist leer')}</Text>
          ) : (
            trash.map((e) => (
              <View key={e.id} style={s.line}>
                <Text style={[s.grow, s.text]} numberOfLines={1}>{e.name}</Text>
                <Pressable style={s.btnSmall} onPress={() => request({ TrashRestore: { id: e.id, keep_both: true } }).then(loadTrash).catch((err) => notify(err.message))}>
                  <Text style={s.link}>{t('Wiederherstellen')}</Text>
                </Pressable>
              </View>
            ))
          )}
        </View>
      </View>

      <Text style={s.h2}>{t('Einstellungen')}</Text>
      <View style={s.card}>
        <View style={s.grow}>
          <Text style={s.strong}>{t('Sprache')}</Text>
          <View style={s.chips}>
            {Object.entries(LANGUAGES).map(([code, l]) => (
              <Pressable key={code} onPress={() => { setLanguage(code); notify(t('Gespeichert.')); }} style={[s.chip, lang() === code && s.chipOn]}>
                <Text style={[s.text, lang() === code && { color: theme.onAccent }]}>{l.name}</Text>
              </Pressable>
            ))}
          </View>
        </View>
      </View>
      <View style={s.card}>
        <View style={s.grow}>
          <Text style={s.strong}>Material You</Text>
          <Text style={s.muted}>{materialAvailable ? t('Farben aus dem Hintergrundbild verwenden') : t('Ab Android 12 verfügbar')}</Text>
        </View>
        <Switch value={material} disabled={!materialAvailable} onValueChange={setMaterial} />
      </View>
    </ScrollView>
  );
}

const styles = (th: Theme) =>
  StyleSheet.create({
    page: { padding: 16, paddingBottom: 40 },
    h2: { color: th.text, fontSize: 18, fontWeight: '600', marginTop: 12, marginBottom: 8 },
    card: {
      flexDirection: 'row', alignItems: 'center', padding: 16, marginBottom: 10, borderRadius: 18,
      backgroundColor: th.card, borderWidth: StyleSheet.hairlineWidth, borderColor: th.stroke,
    },
    grow: { flex: 1 },
    line: { flexDirection: 'row', alignItems: 'center', minHeight: 48 },
    text: { color: th.text, fontSize: 15 },
    strong: { color: th.text, fontSize: 16, fontWeight: '600' },
    muted: { color: th.text2, fontSize: 14, marginTop: 2, lineHeight: 20 },
    link: { color: th.accent, fontSize: 15, fontWeight: '600' },
    dot: { width: 12, height: 12, borderRadius: 6, backgroundColor: th.text2, marginRight: 14 },
    dotOn: { backgroundColor: '#2fa36b' },
    btn: { alignSelf: 'flex-start', minHeight: 48, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 14, backgroundColor: th.accent, marginTop: 12 },
    btnText: { color: th.onAccent, fontSize: 15, fontWeight: '600' },
    btnSmall: { minHeight: 48, justifyContent: 'center', paddingHorizontal: 10 },
    dim: { opacity: 0.45 },
    code: { color: th.text, fontFamily: 'monospace', fontSize: 15, marginTop: 12, padding: 12, borderRadius: 10, backgroundColor: th.accentSoft },
    input: { color: th.text, fontSize: 16, borderBottomWidth: 2, borderBottomColor: th.accent, paddingVertical: 8, marginTop: 8 },
    chips: { flexDirection: 'row', flexWrap: 'wrap', gap: 8, marginTop: 10 },
    chip: { minHeight: 44, justifyContent: 'center', paddingHorizontal: 16, borderRadius: 22, backgroundColor: th.accentSoft },
    chipOn: { backgroundColor: th.accent },
  });
