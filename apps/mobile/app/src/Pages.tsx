// Unterseiten der Einstellungen: Geräte (Status, Speicher, Koppeln), Papierkorb, Allgemein.
import { useCallback, useEffect, useState } from 'react';
import { Alert, Pressable, ScrollView, StyleSheet, Switch, View } from 'react-native';
import { Text, TextInput } from './text';
import * as clip from './clipboard';
import * as core from './core';
import { humanSize, request, shortDate } from './core';
import { LANGUAGES, lang, locale, setLanguage, t } from './i18n';
import type { Theme } from './theme';
import { BottomSheet, Glyph, SectionLabel, themed } from './ui';
import { Icon, type IconName } from './Icon';

export interface Device {
  node_id: string;
  name: string;
  paired_at: number;
  grants: { capability: string; folder: string | null }[];
  online?: boolean;
  /** Gemeldeter Speicher (Summen der Freigaben des Geräts). */
  storage?: { total: number; free: number; updated_at: number };
}

export function Devices({ theme, notify }: { theme: Theme; notify: (t: string) => void }) {
  // Geräte: Status, Speicher, Koppeln (Unterseite der Einstellungen)
  const s = styles(theme);
  const [devices, setDevices] = useState<Device[]>([]);
  const [online, setOnline] = useState<Set<string>>(new Set());
  const [invite, setInvite] = useState<string | null>(null);
  const [code, setCode] = useState('');
  const [shares, setShares] = useState<{ name: string; root: string; file_count: number }[]>([]);
  // Nach dem Koppeln: fragen, was das neue Gerät sehen soll
  const [afterPair, setAfterPair] = useState<string | null>(null);
  // Aufgeklapptes Gerät (Zwischenablage) und Geräte mit automatischem Senden
  const [open, setOpen] = useState<string | null>(null);
  const [auto, setAuto] = useState<Set<string>>(new Set());

  const load = useCallback(async () => {
    try {
      setDevices((await request<{ devices: Device[] }>('PairList')).devices);
      setShares((await request<{ shares: { name: string; root: string; file_count: number }[] }>('ShareList')).shares ?? []);
      const peers = (await request<{ peers: { node_id: string; status: string }[] }>('Peers')).peers;
      setOnline(new Set(peers.filter((p) => p.status === 'online').map((p) => p.node_id)));
    } catch (e: any) {
      notify(e.message);
    }
  }, [notify]);

  useEffect(() => {
    load();
    clip.autoDevices().then((l) => setAuto(new Set(l))).catch(() => {});
  }, [load]);

  const mayFill = (d: Device) => d.grants.some((g) => g.capability === 'clipboard.write');
  const toggleFill = (d: Device, on: boolean) =>
    request({ [on ? 'PairGrant' : 'PairRevoke']: { device: d.node_id, capability: 'clipboard.write', folder: null } })
      .then(load)
      .catch((e) => notify(e?.message ?? String(e)));
  const toggleAuto = async (d: Device, on: boolean) => {
    await clip.setAuto(d.node_id, on);
    setAuto(new Set(await clip.autoDevices()));
  };
  const sendClip = (d: Device) =>
    clip
      .sendTo(d.node_id)
      .then(() => notify(t('Zwischenablage an „{name}“ gesendet.', { name: d.name })))
      .catch((e) => notify(e?.message ?? String(e)));

  const startInvite = async () => {
    try {
      const inv = await request<{ code: string }>('PairInvite');
      setInvite(inv.code);
      const r = await request<{ device: Device }>('PairInviteWait');
      setInvite(null);
      notify(t('„{name}“ ist jetzt gekoppelt.', { name: r.device.name }));
      setAfterPair(r.device.name);
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
      setAfterPair(r.device.name);
      load();
    } catch (e: any) {
      notify(e.message);
    }
  };

  /** Ordner unter dem gemeinsamen Speicher für alle gekoppelten Geräte freigeben. */
  const shareDirs = async (dirs: string[]) => {
    try {
      const root = await core.storageRoot();
      for (const d of dirs) {
        await request({ ShareAdd: { path: `${root}/${d}`, name: null, devices: null } });
      }
      notify(t('Freigegeben. Deine gekoppelten Geräte sehen diese Ordner jetzt.'));
      load();
    } catch (e: any) {
      notify(e?.message ?? String(e));
    }
  };
  const stopShare = (sh: { name: string }) =>
    Alert.alert(t('Freigabe beenden'), t('„{name}“ nicht mehr für gekoppelte Geräte freigeben? Die Dateien bleiben auf diesem Gerät.', { name: sh.name }), [
      { text: t('Abbrechen'), style: 'cancel' },
      { text: t('Beenden'), style: 'destructive', onPress: () => request({ ShareRemove: { name: sh.name } }).then(load).catch((e) => notify(e.message)) },
    ]);

  const unpair = (d: Device) =>
    Alert.alert(t('Gerät entkoppeln'), t('„{name}“ entkoppeln? Beide Geräte sehen danach die Dateien des anderen nicht mehr; Offline-Kopien und Vorschaubilder werden gelöscht. Heruntergeladene Dateien bleiben.', { name: d.name }), [
      { text: t('Abbrechen'), style: 'cancel' },
      { text: t('Entkoppeln'), style: 'destructive', onPress: () => request({ PairRemove: { device: d.node_id } }).then(load).catch((e) => notify(e.message)) },
    ]);

  const kind = (n: string): IconName => (/tablet/i.test(n) ? 'tablet' : /handy|phone|pixel/i.test(n) ? 'phone' : 'laptop');
  return (
    <ScrollView contentContainerStyle={s.page}>
      <SectionLabel theme={theme} style={s.label}>{t('Gekoppelte Geräte')}</SectionLabel>
      {devices.length === 0 ? (
        <View style={[s.group, s.emptyGroup]}>
          <Icon name="devices" size={28} color={theme.text3 as string} />
          <Text style={[s.muted, s.centerText]}>{t('Noch keine Geräte gekoppelt.')}</Text>
        </View>
      ) : (
        <View style={s.group}>
          {devices.map((d, i) => {
            const on = online.has(d.node_id);
            return (
              <View key={d.node_id}>
                {i > 0 && <View style={s.sep} />}
                <Pressable style={s.row} onPress={() => setOpen(open === d.node_id ? null : d.node_id)} onLongPress={() => unpair(d)} android_ripple={{ color: theme.accentSoft as string }} accessibilityState={{ expanded: open === d.node_id }}>
                  <Icon name={kind(d.name)} size={24} color={(on ? theme.accent : theme.text3) as string} />
                  <View style={s.grow}>
                    <Text style={s.strong}>{d.name}</Text>
                    <Text style={s.muted} numberOfLines={2}>
                      {on ? t('Verbunden') : t('Nicht verbunden')}
                      {d.storage ? ` · ${t('{free} frei', { free: humanSize(d.storage.free, locale()) })}` : ''}
                      {` · ${t('seit {date}', { date: shortDate(d.paired_at, locale()) })}`}
                    </Text>
                  </View>
                  <Pressable onPress={() => unpair(d)} style={s.iconBtn} accessibilityLabel={t('Entkoppeln')} android_ripple={{ color: theme.accentSoft as string, borderless: true }}>
                    <Icon name="more" size={22} color={theme.text2 as string} strokeWidth={2.4} />
                  </Pressable>
                </Pressable>
                {open === d.node_id && (
                  <View style={s.devPanel}>
                    <Pressable onPress={() => sendClip(d)} style={s.devAction} accessibilityRole="button">
                      <Icon name="copy" size={20} color={theme.text2 as string} />
                      <Text style={[s.text, s.grow]}>{t('Zwischenablage senden')}</Text>
                    </Pressable>
                    <View style={s.devAction}>
                      <Text style={[s.text, s.grow]}>{t('Darf meine Zwischenablage füllen')}</Text>
                      <Switch
                        value={mayFill(d)}
                        onValueChange={(v) => toggleFill(d, v)}
                        trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
                        thumbColor={theme.surface as string}
                        accessibilityLabel={t('Darf meine Zwischenablage füllen')}
                      />
                    </View>
                    <View style={s.devAction}>
                      <View style={s.grow}>
                        <Text style={s.text}>{t('Zwischenablage automatisch senden')}</Text>
                        <Text style={s.muted}>{t('Beim Öffnen von Snap, wenn sich etwas geändert hat.')}</Text>
                      </View>
                      <Switch
                        value={auto.has(d.node_id)}
                        onValueChange={(v) => toggleAuto(d, v)}
                        trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
                        thumbColor={theme.surface as string}
                        accessibilityLabel={t('Zwischenablage automatisch senden')}
                      />
                    </View>
                  </View>
                )}
              </View>
            );
          })}
        </View>
      )}

      <SectionLabel theme={theme} style={s.label}>{t('Meine Freigaben')}</SectionLabel>
      <View style={s.group}>
        {shares.length === 0 ? (
          <Text style={[s.muted, s.block]}>{t('Noch keine Ordner freigegeben. In „Dateien“ einen Ordner lange drücken › „Für Geräte freigeben“.')}</Text>
        ) : (
          shares.map((sh, i) => (
            <View key={sh.name}>
              {i > 0 && <View style={s.sep} />}
              <View style={s.row}>
                <Glyph name={sh.name} dir theme={theme} size={32} />
                <View style={s.grow}>
                  <Text style={s.strong} numberOfLines={1}>{sh.name}</Text>
                  <Text style={s.muted} numberOfLines={1}>{sh.root.replace(/^\/storage\/emulated\/0\/?/, '') || t('Interner Speicher')}</Text>
                </View>
                <Pressable onPress={() => stopShare(sh)} style={s.iconBtn} accessibilityLabel={t('Freigabe „{name}“ beenden', { name: sh.name })} android_ripple={{ color: theme.accentSoft as string, borderless: true }}>
                  <Icon name="close" size={20} color={theme.text3 as string} />
                </Pressable>
              </View>
            </View>
          ))
        )}
      </View>

      <SectionLabel theme={theme} style={s.label}>{t('Neues Gerät koppeln')}</SectionLabel>
      <View style={s.group}>
        <View style={s.block}>
          <Text style={s.strong}>{t('Dieses Gerät lädt ein')}</Text>
          <Text style={s.muted}>{t('Code erzeugen und am neuen Gerät eingeben.')}</Text>
          {invite ? (
            <>
              <Text selectable style={s.code}>{invite}</Text>
              <Text style={s.hint}>{t('Warte auf das andere Gerät …')}</Text>
            </>
          ) : (
            <Pressable onPress={startInvite} style={s.btnTonal} accessibilityRole="button">
              <Text style={s.btnTonalText}>{t('Code erzeugen')}</Text>
            </Pressable>
          )}
        </View>
        <View style={s.sepFull} />
        <View style={s.block}>
          <Text style={s.strong}>{t('Code von einem anderen Gerät')}</Text>
          <View style={s.joinRow}>
            <TextInput value={code} onChangeText={setCode} placeholder={t('Einladungscode')} placeholderTextColor={theme.text3 as string} autoCapitalize="none" autoCorrect={false} style={s.input} />
            <Pressable onPress={join} disabled={!code.trim()} style={[s.btn, !code.trim() && s.dim]} accessibilityRole="button">
              <Text style={s.btnText}>{t('Koppeln')}</Text>
            </Pressable>
          </View>
        </View>
      </View>
      <Text style={s.foot}>{t('Gekoppelte Geräte sehen nur Ordner, die du freigibst – sonst nichts.')}</Text>
      <BottomSheet
        visible={!!afterPair}
        theme={theme}
        title={t('Was soll „{name}“ von diesem Gerät sehen?', { name: afterPair ?? '' })}
        onClose={() => setAfterPair(null)}
        actions={[
          { icon: 'photos', label: t('Fotos und Videos (DCIM)'), onPress: () => shareDirs(['DCIM']) },
          { icon: 'download', label: t('Downloads'), onPress: () => shareDirs(['Download']) },
          { icon: 'folder', label: t('Fotos und Downloads'), onPress: () => shareDirs(['DCIM', 'Download']) },
          { icon: 'close', label: t('Später selbst wählen'), onPress: () => {} },
        ]}
      />
    </ScrollView>
  );
}

interface TrashEntry { id: string; name: string; original_path: string; deleted_at: string }

/** Papierkorb (Unterseite der Einstellungen): Einträge ansehen und wiederherstellen. */
export function TrashPage({ theme, notify }: { theme: Theme; notify: (t: string) => void }) {
  const s = styles(theme);
  const [trash, setTrash] = useState<TrashEntry[] | null>(null);
  const loadTrash = useCallback(() => request<{ entries: TrashEntry[] }>('TrashList').then((r) => setTrash(r.entries)).catch((e) => notify(e.message)), [notify]);
  useEffect(() => {
    loadTrash();
  }, [loadTrash]);
  return (
    <ScrollView contentContainerStyle={s.page}>
      {trash === null ? null : trash.length === 0 ? (
        <View style={s.emptyPage}>
          <View style={s.emptyIcon}>
            <Icon name="trash" size={28} color={theme.text3 as string} />
          </View>
          <Text style={s.emptyTitle}>{t('Der Papierkorb ist leer')}</Text>
          <Text style={[s.muted, s.centerText]}>{t('Gelöschte Dateien landen hier und lassen sich wiederherstellen.')}</Text>
        </View>
      ) : (
        <View style={[s.group, s.firstGroup]}>
          {trash.map((e, i) => (
            <View key={e.id}>
              {i > 0 && <View style={s.sepFile} />}
              <View style={s.row}>
                <Glyph name={e.name} dir={false} theme={theme} size={36} />
                <View style={s.grow}>
                  <Text style={s.text} numberOfLines={1}>{e.name}</Text>
                  <Text style={s.muted} numberOfLines={1} ellipsizeMode="head">{e.original_path.replace(/\/[^/]*$/, '').replace(/^\/storage\/emulated\/0/, '~')}</Text>
                </View>
                <Pressable
                  style={s.iconBtn}
                  accessibilityLabel={t('Wiederherstellen')}
                  android_ripple={{ color: theme.accentSoft as string, borderless: true }}
                  onPress={() => request({ TrashRestore: { id: e.id, keep_both: true } }).then(loadTrash).catch((err) => notify(err.message))}
                >
                  <Icon name="restore" size={22} color={theme.accent as string} />
                </Pressable>
              </View>
            </View>
          ))}
        </View>
      )}
    </ScrollView>
  );
}

/** Allgemein (Unterseite der Einstellungen): Sprache, Material You, Schriftart. */
export function General({ theme, notify, material, setMaterial, materialAvailable, fontName, onFont }: {
  theme: Theme; notify: (t: string) => void; material: boolean; setMaterial: (v: boolean) => void; materialAvailable: boolean;
  /** Name der eigenen Schrift oder null (= Inconsolata). */
  fontName?: string | null;
  onFont?: (name: string | null) => void;
}) {
  const s = styles(theme);
  return (
    <ScrollView contentContainerStyle={s.page}>
      <SectionLabel theme={theme} style={s.label}>{t('Sprache')}</SectionLabel>
      <View style={s.segment} accessibilityRole="radiogroup">
        {Object.entries(LANGUAGES).map(([code, l]) => {
          const on = lang() === code;
          return (
            <Pressable key={code} onPress={() => { setLanguage(code); notify(t('Gespeichert.')); }} style={[s.segItem, on && s.segOn]} accessibilityRole="radio" accessibilityState={{ selected: on }}>
              <Text style={[s.text, on && s.segTextOn]}>{l.name}</Text>
            </Pressable>
          );
        })}
      </View>

      <SectionLabel theme={theme} style={s.label}>{t('Erscheinung')}</SectionLabel>
      <View style={s.group}>
        <View style={s.row}>
          <Icon name="palette" size={22} color={theme.text2 as string} />
          <View style={s.grow}>
            <Text style={s.text}>Material You</Text>
            <Text style={s.muted}>{materialAvailable ? t('Farben aus dem Hintergrundbild verwenden') : t('Ab Android 12 verfügbar')}</Text>
          </View>
          <Switch
            value={material}
            disabled={!materialAvailable}
            onValueChange={setMaterial}
            trackColor={{ true: theme.accent as string, false: theme.surface2 as string }}
            thumbColor={theme.surface as string}
          />
        </View>
        <View style={s.sep} />
        <View style={s.row}>
          <Icon name="font" size={22} color={theme.text2 as string} />
          <View style={s.grow}>
            <Text style={s.text}>{t('Schriftart')}</Text>
            <Text style={s.muted}>{fontName ?? t('Inconsolata (Standard)')}</Text>
            <View style={s.chips}>
              <Pressable
                style={s.btnTonal}
                accessibilityRole="button"
                onPress={() =>
                  core
                    .pickFont()
                    .then((name) => {
                      if (name) {
                        onFont?.(name);
                        notify(t('Schriftart „{name}“ wird verwendet.', { name }));
                      }
                    })
                    .catch((e) => notify(e.message))
                }
              >
                <Text style={s.btnTonalText}>{t('TTF-Datei wählen …')}</Text>
              </Pressable>
              {fontName && (
                <Pressable style={s.btnText2} accessibilityRole="button" onPress={() => core.resetFont().then(() => onFont?.(null)).catch((e) => notify(e.message))}>
                  <Text style={s.link}>{t('Zurück zu Inconsolata')}</Text>
                </Pressable>
              )}
            </View>
          </View>
        </View>
      </View>
    </ScrollView>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    page: { paddingHorizontal: 16, paddingBottom: 40 },
    label: { marginTop: 20, marginBottom: 10, marginLeft: 4 },
    group: { borderRadius: 12, backgroundColor: th.surface, overflow: 'hidden' },
    firstGroup: { marginTop: 8 },
    emptyGroup: { alignItems: 'center', gap: 10, paddingVertical: 28, paddingHorizontal: 24 },
    row: { flexDirection: 'row', alignItems: 'center', gap: 18, minHeight: 64, paddingLeft: 16, paddingRight: 8, paddingVertical: 10 },
    block: { padding: 16, gap: 2 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.line, marginLeft: 58 },
    devPanel: { paddingLeft: 58, paddingRight: 12, paddingBottom: 8, gap: 2 },
    devAction: { flexDirection: 'row', alignItems: 'center', gap: 12, minHeight: 48 },
    sepFile: { height: StyleSheet.hairlineWidth, backgroundColor: th.line, marginLeft: 70 },
    sepFull: { height: StyleSheet.hairlineWidth, backgroundColor: th.line },
    grow: { flex: 1 },
    text: { color: th.text, fontSize: 16, lineHeight: 22 },
    strong: { color: th.text, fontSize: 16, lineHeight: 22, fontWeight: '600' },
    muted: { color: th.text2, fontSize: 13, lineHeight: 18 },
    hint: { color: th.text3, fontSize: 13, lineHeight: 18, marginTop: 6 },
    foot: { color: th.text3, fontSize: 13, lineHeight: 18, marginTop: 12, marginHorizontal: 4 },
    centerText: { textAlign: 'center' },
    link: { color: th.accent, fontSize: 15, fontWeight: '700' },
    iconBtn: { width: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center' },
    btn: { minHeight: 44, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 22, backgroundColor: th.accent },
    btnText: { color: th.onAccent, fontSize: 15, fontWeight: '700' },
    btnTonal: { alignSelf: 'flex-start', minHeight: 40, justifyContent: 'center', paddingHorizontal: 18, borderRadius: 20, backgroundColor: th.accentSoft, marginTop: 12 },
    btnTonalText: { color: th.accent, fontSize: 15, fontWeight: '700' },
    btnText2: { minHeight: 40, justifyContent: 'center', paddingHorizontal: 12, marginTop: 12 },
    dim: { opacity: 0.4 },
    code: { color: th.text, fontSize: 24, lineHeight: 32, letterSpacing: 3, fontWeight: '700', marginTop: 14 },
    joinRow: { flexDirection: 'row', alignItems: 'center', gap: 12, marginTop: 6 },
    input: { flex: 1, color: th.text, fontSize: 16, borderBottomWidth: 1.5, borderBottomColor: th.accent, paddingVertical: 8 },
    chips: { flexDirection: 'row', flexWrap: 'wrap', alignItems: 'center', gap: 4 },
    segment: { flexDirection: 'row', padding: 4, borderRadius: 24, backgroundColor: th.surface2 },
    segItem: { flex: 1, minHeight: 40, borderRadius: 20, justifyContent: 'center', alignItems: 'center' },
    segOn: { backgroundColor: th.surface, elevation: 1 },
    segTextOn: { fontWeight: '700' },
    emptyPage: { alignItems: 'center', paddingTop: 96, paddingHorizontal: 40, gap: 6 },
    emptyIcon: { width: 64, height: 64, borderRadius: 32, justifyContent: 'center', alignItems: 'center', marginBottom: 10, backgroundColor: th.surface2 },
    emptyTitle: { color: th.text, fontSize: 17, lineHeight: 24, fontWeight: '700' },
  }));
