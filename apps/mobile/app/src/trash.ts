// Papierkorb: gemeinsame Helfer für die Papierkorb-Seite und die Speicherübersicht.
import { Alert } from 'react-native';
import { humanSize, request } from './core';
import { locale, t } from './i18n';

export interface TrashEntry { id: string; name: string; original_path: string; deleted_at: string; is_dir?: boolean; size?: number }

export const trashList = () => request<{ entries?: TrashEntry[] }>('TrashList').then((r) => r?.entries ?? []);

/** Belegter Platz (Ordner zählen ohne ihren Inhalt – der Kern liefert nur Dateigrößen). */
export const trashSize = (entries: TrashEntry[]) => entries.reduce((sum, e) => sum + (e.size ?? 0), 0);

/** Fragt nach und leert den Papierkorb endgültig. `done` läuft nach Erfolg. */
export function confirmEmptyTrash(entries: TrashEntry[], notify: (m: string) => void, done: () => void) {
  const size = trashSize(entries);
  const what = size > 0
    ? t('{n} Elemente ({size}) werden endgültig gelöscht.', { n: entries.length, size: humanSize(size, locale()) })
    : t('{n} Elemente werden endgültig gelöscht.', { n: entries.length });
  Alert.alert(t('Papierkorb leeren?'), `${what} ${t('Das lässt sich nicht rückgängig machen.')}`, [
    { text: t('Abbrechen'), style: 'cancel' },
    {
      text: t('Endgültig löschen'),
      style: 'destructive',
      onPress: () =>
        request('TrashEmpty')
          .then(() => notify(t('Papierkorb geleert.')))
          .catch((e) => notify(String(e?.message ?? e)))
          .finally(done),
    },
  ]);
}

/** Fragt nach und löscht einen Eintrag endgültig. */
export function confirmPurge(e: TrashEntry, notify: (m: string) => void, done: () => void) {
  Alert.alert(t('Endgültig löschen?'), `${e.name}\n${t('Das lässt sich nicht rückgängig machen.')}`, [
    { text: t('Abbrechen'), style: 'cancel' },
    {
      text: t('Endgültig löschen'),
      style: 'destructive',
      onPress: () =>
        request({ TrashPurge: { id: e.id } })
          .catch((err) => notify(String(err?.message ?? err)))
          .finally(done),
    },
  ]);
}
