// Gemeinsame Bausteine: Symbole (schlicht, aus Formen gezeichnet – keine fremden Grafiken).
import { StyleSheet, View } from 'react-native';
import type { Theme } from './theme';

/** Ordner (gelb) oder Datei (Blatt) als kleines Symbol, 40 × 40. */
export function Glyph({ kind, theme }: { kind: 'folder' | 'file'; theme: Theme }) {
  if (kind === 'folder') {
    return (
      <View style={g.box}>
        <View style={[g.tab, { backgroundColor: '#e8b33c' }]} />
        <View style={[g.folder, { backgroundColor: '#f7c948' }]} />
      </View>
    );
  }
  return (
    <View style={g.box}>
      <View style={[g.sheet, { backgroundColor: theme.dark ? '#3a3d47' : '#ffffff', borderColor: theme.stroke }]}>
        <View style={[g.band, { backgroundColor: theme.accent }]} />
      </View>
    </View>
  );
}

/** Symbole der unteren Leiste. */
export function NavIcon({ name, color }: { name: string; color: string }) {
  const b = { borderColor: color };
  switch (name) {
    case 'files':
      return (
        <View style={g.nav}>
          <View style={[g.navFolderTab, b]} />
          <View style={[g.navFolder, b]} />
        </View>
      );
    case 'photos':
      return (
        <View style={[g.nav, g.grid]}>
          {[0, 1, 2, 3].map((i) => <View key={i} style={[g.cell, b]} />)}
        </View>
      );
    case 'devices':
      return (
        <View style={g.nav}>
          <View style={[g.phone, b]} />
        </View>
      );
    default:
      return (
        <View style={[g.nav, g.dots]}>
          {[0, 1, 2].map((i) => <View key={i} style={[g.dot, { backgroundColor: color }]} />)}
        </View>
      );
  }
}

const g = StyleSheet.create({
  box: { width: 40, height: 40, justifyContent: 'center', alignItems: 'center' },
  tab: { position: 'absolute', top: 7, left: 4, width: 14, height: 8, borderRadius: 3 },
  folder: { width: 34, height: 24, borderRadius: 5, marginTop: 6 },
  sheet: { width: 28, height: 34, borderRadius: 4, borderWidth: 1, justifyContent: 'flex-end', padding: 4 },
  band: { height: 7, borderRadius: 2 },
  nav: { width: 24, height: 24, justifyContent: 'center', alignItems: 'center' },
  navFolderTab: { position: 'absolute', top: 4, left: 2, width: 9, height: 5, borderWidth: 2, borderBottomWidth: 0, borderTopLeftRadius: 2, borderTopRightRadius: 2 },
  navFolder: { width: 22, height: 15, borderWidth: 2, borderRadius: 3, marginTop: 4 },
  grid: { flexDirection: 'row', flexWrap: 'wrap', gap: 3, padding: 1 },
  cell: { width: 9, height: 9, borderWidth: 2, borderRadius: 2 },
  phone: { width: 13, height: 22, borderWidth: 2, borderRadius: 3 },
  dots: { flexDirection: 'row', gap: 3 },
  dot: { width: 5, height: 5, borderRadius: 3 },
});
