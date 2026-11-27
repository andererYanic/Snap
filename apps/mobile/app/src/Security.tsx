// Sicherheitsübersicht: Hinweise zuerst, dann wer was darf, was freigegeben ist und was sichtbar
// ist. Nur lesend – geändert wird an den bekannten Stellen (Geräte, Snap Drop, Tresor).
import { useEffect, useState } from 'react';
import { ActivityIndicator, ScrollView, StyleSheet, View } from 'react-native';
import { request } from './core';
import { t } from './i18n';
import { Icon } from './Icon';
import { Text } from './text';
import type { Theme } from './theme';
import { SectionLabel, themed } from './ui';

interface Report {
  devices: { node_id: string; name: string; rights: string[]; strong: boolean; online: boolean }[];
  shares: { name: string; root: string; all_devices: boolean }[];
  all_files: boolean;
  links_active: number;
  drop_mode: string;
  vault_exists: boolean;
  vault_unlocked: boolean;
  denied_7d: number;
  relay_public: boolean;
  hints: { level: string; code: string; text: string; name?: string; count?: number }[];
}

/** Rechte lesbar machen. */
const RIGHTS: Record<string, string> = {
  'catalog.read': 'Dateiliste sehen',
  'files.read': 'Dateien lesen',
  'files.write': 'Dateien ändern',
  'files.delete': 'In den Papierkorb',
  'files.purge': 'Endgültig löschen',
  'pin.request': 'Kopien anfordern',
  'clipboard.write': 'Zwischenablage füllen',
  'device.ring': 'Klingeln lassen',
  admin: 'Verwalten (alles)',
};

/** Hinweis in der App-Sprache (Code und Werte vom Kern). */
export function hintText(h: Report['hints'][number]): string {
  switch (h.code) {
    case 'purge': return t('„{name}“ darf Dateien endgültig löschen (ohne Papierkorb).', { name: h.name ?? '' });
    case 'all_files': return t('„Alle Dateien“ ist eingeschaltet: Deine gekoppelten Geräte sehen den ganzen Speicher.');
    case 'links': return t('{n} Freigabelink(s) sind aktiv – wer den Link hat, kann die Datei laden.', { n: h.count ?? 0 });
    case 'drop_everyone': return t('Snap Drop ist gerade für alle in der Nähe sichtbar (läuft von selbst ab).');
    case 'vault_unlocked': return t('Der Tresor ist entsperrt (sperrt sich nach 5 Minuten ohne Benutzung).');
    case 'denied': return t('{n} abgewiesene Zugriffe in den letzten 7 Tagen – Einzelheiten im Protokoll.', { n: h.count ?? 0 });
    case 'no_devices': return t('Noch kein Gerät gekoppelt.');
    default: return h.text;
  }
}

export function rightLabel(r: string): string {
  const [cap, folder] = r.split(/: (.*)/s);
  const label = t(RIGHTS[cap] ?? cap);
  return folder ? `${label} · ${folder.replace(/^\/storage\/emulated\/0\/?/, '')}` : label;
}

export function SecurityPage({ theme }: { theme: Theme }) {
  const s = styles(theme);
  const [r, setR] = useState<Report | null>(null);
  useEffect(() => {
    request<Report>('SecurityOverview').then(setR).catch(() => {});
  }, []);
  if (!r) {
    return <ActivityIndicator style={s.loading} color={theme.accent as string} />;
  }
  const row = (label: string, value: string) => (
    <View style={s.row}>
      <Text style={[s.text, s.grow]}>{label}</Text>
      <Text style={s.meta}>{value}</Text>
    </View>
  );
  return (
    <ScrollView contentContainerStyle={s.page}>
      {r.hints.length > 0 && (
        <View style={s.card}>
          {r.hints.map((h, i) => (
            <View key={i} style={s.hint}>
              <Icon name={h.level === 'warn' ? 'warning' : 'info'} size={20} color={(h.level === 'warn' ? theme.danger ?? theme.accent : theme.text2) as string} />
              <Text style={[s.text, s.grow]}>{hintText(h)}</Text>
            </View>
          ))}
        </View>
      )}
      <SectionLabel theme={theme} style={s.section}>{t('Gekoppelte Geräte')}</SectionLabel>
      <View style={s.card}>
        {r.devices.length === 0 ? <Text style={[s.meta, s.pad]}>{t('Noch kein Gerät gekoppelt.')}</Text> : null}
        {r.devices.map((d, i) => (
          <View key={d.node_id}>
            {i > 0 && <View style={s.sep} />}
            <View style={s.device}>
              <View style={s.rowInline}>
                <Text style={[s.strongText, s.grow]}>{d.name}</Text>
                {d.strong ? <Icon name="shield" size={18} color={theme.accent as string} /> : null}
              </View>
              {d.rights.map((x) => <Text key={x} style={s.meta}>{`· ${rightLabel(x)}`}</Text>)}
            </View>
          </View>
        ))}
      </View>
      <SectionLabel theme={theme} style={s.section}>{t('Sichtbarkeit')}</SectionLabel>
      <View style={s.card}>
        {row(t('Freigegebene Ordner'), String(r.shares.length))}
        <View style={s.sep} />
        {row(t('Alle Dateien sichtbar'), r.all_files ? t('Ja') : t('Nein'))}
        <View style={s.sep} />
        {row(t('Aktive Freigabelinks'), String(r.links_active))}
        <View style={s.sep} />
        {row(t('Snap Drop'), r.drop_mode === 'Everyone' ? t('Für alle sichtbar') : r.drop_mode === 'Contacts' ? t('Für Kontakte sichtbar') : t('Unsichtbar'))}
        <View style={s.sep} />
        {row(t('Tresor'), !r.vault_exists ? t('Nicht eingerichtet') : r.vault_unlocked ? t('Entsperrt') : t('Gesperrt'))}
        <View style={s.sep} />
        {row(t('Abgewiesene Zugriffe (7 Tage)'), String(r.denied_7d))}
        <View style={s.sep} />
        {row(t('Relais'), r.relay_public ? t('Öffentlich – sieht nur Verschlüsseltes') : t('Eigenes oder aus'))}
      </View>
    </ScrollView>
  );
}

const styles = themed((th) =>
  StyleSheet.create({
    loading: { marginTop: 48 },
    page: { padding: 16, paddingBottom: 48 },
    card: { borderRadius: 14, backgroundColor: th.surface, overflow: 'hidden' },
    section: { marginTop: 20, marginBottom: 8 },
    hint: { flexDirection: 'row', gap: 12, padding: 14, alignItems: 'flex-start' },
    row: { flexDirection: 'row', alignItems: 'center', gap: 12, paddingHorizontal: 16, minHeight: 52 },
    rowInline: { flexDirection: 'row', alignItems: 'center', gap: 8 },
    device: { paddingHorizontal: 16, paddingVertical: 12, gap: 2 },
    sep: { height: StyleSheet.hairlineWidth, backgroundColor: th.surface2, marginLeft: 16 },
    grow: { flex: 1 },
    pad: { padding: 16 },
    text: { color: th.text, fontSize: 15, lineHeight: 21 },
    strongText: { color: th.text, fontSize: 15, lineHeight: 21, fontWeight: '700' },
    meta: { color: th.text2, fontSize: 13, lineHeight: 18 },
  }),
);
