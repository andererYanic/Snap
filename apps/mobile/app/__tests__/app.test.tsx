/**
 * Bildschirmlogik mit nachgebildetem Kern (kein Gerät nötig): Liste, Auswahl per langem
 * Drücken, Papierkorb mit Rückgängig, Berechtigungsbildschirm.
 */
import React from 'react';
import ReactTestRenderer, { act } from 'react-test-renderer';
import { NativeModules } from 'react-native';
import { State } from 'react-native-gesture-handler';
import { fireGestureHandler, getByGestureTestId } from 'react-native-gesture-handler/jest-utils';

jest.mock('react-native-safe-area-context', () => require('react-native-safe-area-context/jest/mock').default);

const calls: unknown[] = [];
let allFiles = true;
let crashReport: string | null = null;

const listing = {
  DirListing: {
    path: '/storage/emulated/0',
    total: 3,
    entries: [
      { name: 'DCIM', kind: 'Dir', size: 0, mtime: 1700000000, hidden: false },
      { name: 'brief.pdf', kind: 'File', size: 2048, mtime: 1700000000, hidden: false },
      { name: 'liste.txt', kind: 'File', size: 10, mtime: 1700000000, hidden: false },
    ],
  },
};

NativeModules.SnapCore = {
  start: jest.fn(async () => 1),
  request: jest.fn(async (json: string) => {
    const req = JSON.parse(json);
    calls.push(req);
    if (req.ListDir) return JSON.stringify(listing);
    if (req.Search) {
      return JSON.stringify({ SearchResults: { hits: [
        { path: '/storage/emulated/0/DCIM/urlaub.jpg', name: 'urlaub.jpg', kind: 'File', size: 1, mtime: 1700000000, fuzzy: true },
      ] } });
    }
    if (req.FileOp) return JSON.stringify({ FileOpDone: { job: 1 } });
    if (req === 'Undo') return JSON.stringify({ Success: { message: 'ok' } });
    return JSON.stringify({ Success: { message: 'ok' } });
  }),
  hasAllFilesAccess: jest.fn(async () => allFiles),
  storageInfo: jest.fn(async () => ({ total: 64e9, free: 20e9 })),
  openFile: jest.fn(async () => null),
  shareFiles: jest.fn(async () => null),
  thumbnail: jest.fn(async () => null),
  haptic: jest.fn(),
  lastCrash: jest.fn(async () => crashReport),
  requestAllFilesAccess: jest.fn(),
  storageRoot: jest.fn(async () => '/storage/emulated/0'),
  getPref: jest.fn(async (k: string) => (k === 'lang' ? 'de' : null)),
  setPref: jest.fn(),
  sdkInt: jest.fn(async () => 34),
  addListener: jest.fn(),
  removeListeners: jest.fn(),
};

import App from '../App';

const flush = () => act(async () => { await new Promise<void>((resolve) => { setTimeout(() => resolve(), 0); }); });

/** Alle sichtbaren Texte (aus den Text-Elementen). */
function allText(r: ReactTestRenderer.ReactTestRenderer): string {
  const out: string[] = [];
  const walk = (c: unknown): void => {
    if (typeof c === 'string' || typeof c === 'number') out.push(String(c));
    else if (Array.isArray(c)) c.forEach(walk);
  };
  r.root.findAll((n) => (n.type as any) === 'Text').forEach((n) => walk(n.props.children));
  return out.join(' | ');
}

/** Tippbares Element mit diesem Text (das innerste). */
function pressable(r: ReactTestRenderer.ReactTestRenderer, text: string) {
  const textOf = (n: ReactTestRenderer.ReactTestInstance) =>
    n.findAll((c) => (c.type as any) === 'Text').map((c) => [c.props.children].flat().join('')).join(' ');
  const hits = r.root.findAll((n) => typeof n.props.onPress === 'function' && textOf(n).includes(text));
  return hits[hits.length - 1];
}

function byLabel(r: ReactTestRenderer.ReactTestRenderer, label: string) {
  return r.root.find((n) => n.props.accessibilityLabel === label && typeof n.props.onPress === 'function');
}

beforeEach(() => {
  calls.length = 0;
  allFiles = true;
  crashReport = null;
});

test('Liste, Auswahl, Papierkorb und Rückgängig', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = ReactTestRenderer.create(<App />); });
  await flush();
  await flush();
  // Startseite: Speicher mit Belegung und vorhandene Standardordner
  expect(allText(r)).toContain('Interner Speicher');
  expect(allText(r)).toContain('von 60 GB belegt');
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  const all = allText(r);
  expect(all).toContain('brief.pdf');
  expect(all).toContain('DCIM');
  // Lang halten auf „brief.pdf“ (Zeile 2) und ohne Loslassen über „liste.txt“ wischen
  const g = getByGestureTestId('files-select');
  await act(async () => {
    fireGestureHandler(g, [
      { state: State.BEGAN, x: 100, y: 64 + 20, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 64 + 20, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 128 + 20, absoluteX: 100, absoluteY: 364 },
      { state: State.END, x: 100, y: 128 + 20, absoluteX: 100, absoluteY: 364 },
    ]);
  });
  await flush();
  expect(allText(r)).toContain('2 ausgewählt');
  expect(NativeModules.SnapCore.haptic).toHaveBeenCalled();
  // Menü am Finger: Papierkorb wählen
  const trashBtn = pressable(r, 'In den Papierkorb');
  await act(async () => { await trashBtn.props.onPress(); });
  await flush();
  expect(calls).toContainEqual({ FileOp: { op: { Trash: { paths: ['/storage/emulated/0/brief.pdf', '/storage/emulated/0/liste.txt'] } }, wait: true } });
  expect(allText(r)).toContain('Rückgängig');
  // Rückgängig ruft Undo im Kern
  const undo = pressable(r, 'Rückgängig');
  await act(async () => { undo.props.onPress(); });
  await flush();
  expect(calls).toContain('Undo');
});

test('Ohne Dateizugriff: Hinweis und Knopf zu den Systemeinstellungen', async () => {
  allFiles = false;
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = ReactTestRenderer.create(<App />); });
  await flush();
  expect(allText(r)).toContain('Zugriff erlauben');
  expect(NativeModules.SnapCore.start).not.toHaveBeenCalledTimes(99);
  const btn = r.root.find((n) => n.props.accessibilityRole === 'button' && typeof n.props.onPress === 'function');
  await act(async () => { btn.props.onPress(); });
  expect(NativeModules.SnapCore.requestAllFilesAccess).toHaveBeenCalled();
});

test('Absturz beim letzten Start: Bericht und „Erneut versuchen“ statt erneutem Absturz', async () => {
  crashReport = '[1] Thread snap-core: panicked at core/src/x.rs:1:1';
  NativeModules.SnapCore.start.mockClear();
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = ReactTestRenderer.create(<App />); });
  await flush();
  expect(allText(r)).toContain('panicked at core/src/x.rs');
  expect(NativeModules.SnapCore.start).not.toHaveBeenCalled();
  crashReport = null;
  await act(async () => { await pressable(r, 'Erneut versuchen').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.start).toHaveBeenCalledTimes(1);
  expect(allText(r)).toContain('Interner Speicher');
});

test('Datei antippen öffnet sie mit der passenden App, „+“ legt eine Textdatei an', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = ReactTestRenderer.create(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  await act(async () => { byLabel(r, 'brief.pdf').props.onPress(); });
  expect(NativeModules.SnapCore.openFile).toHaveBeenCalledWith('/storage/emulated/0/brief.pdf');
  // „+“ → „Neue Textdatei“ → Name bestätigen
  await act(async () => { byLabel(r, 'Neu').props.onPress(); });
  await act(async () => { pressable(r, 'Neue Textdatei').props.onPress(); });
  await flush();
  const input = r.root.find((n) => (n.type as any) === 'TextInput' && n.props.autoFocus);
  await act(async () => { input.props.onChangeText('einkauf.txt'); });
  await act(async () => { await pressable(r, 'Speichern').props.onPress(); });
  await flush();
  expect(calls).toContainEqual({ FileOp: { op: { CreateFile: { parent: '/storage/emulated/0', name: 'einkauf.txt' } }, wait: true } });
});

test('Suche beim Tippen (ohne Enter), Tippfehler-Treffer markiert, Bild öffnet die eingebaute Anzeige', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = ReactTestRenderer.create(<App />); });
  await flush();
  const field = r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === 'Überall suchen');
  await act(async () => { field.props.onChangeText('urluab'); });
  await act(async () => { await new Promise<void>((resolve) => { setTimeout(() => resolve(), 260); }); });
  expect(calls).toContainEqual({ Search: { query: 'urluab', filter: {}, limit: 200 } });
  expect(allText(r)).toContain('urlaub.jpg');
  expect(allText(r)).toContain('ähnlich');
  NativeModules.SnapCore.openFile.mockClear();
  await act(async () => { byLabel(r, 'urlaub.jpg').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.openFile).not.toHaveBeenCalled();
  expect(allText(r)).toContain('1 / 1');
});
