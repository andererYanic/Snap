/**
 * Bildschirmlogik mit nachgebildetem Kern (kein Gerät nötig): Liste, Auswahl per langem
 * Drücken, Papierkorb mit Rückgängig, Berechtigungsbildschirm.
 */
import React from 'react';
import ReactTestRenderer, { act } from 'react-test-renderer';
import { NativeModules } from 'react-native';

jest.mock('react-native-safe-area-context', () => require('react-native-safe-area-context/jest/mock').default);

const calls: unknown[] = [];
let allFiles = true;

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
    if (req.FileOp) return JSON.stringify({ FileOpDone: { job: 1 } });
    if (req === 'Undo') return JSON.stringify({ Success: { message: 'ok' } });
    return JSON.stringify({ Success: { message: 'ok' } });
  }),
  hasAllFilesAccess: jest.fn(async () => allFiles),
  requestAllFilesAccess: jest.fn(),
  storageRoot: jest.fn(async () => '/storage/emulated/0'),
  getPref: jest.fn(async (k: string) => (k === 'lang' ? 'de' : null)),
  setPref: jest.fn(),
  sdkInt: jest.fn(async () => 34),
  addListener: jest.fn(),
  removeListeners: jest.fn(),
};

import App from '../App';

const flush = () => act(async () => { await new Promise((r) => setTimeout(r, 0)); });

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
});

test('Liste, Auswahl, Papierkorb und Rückgängig', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = ReactTestRenderer.create(<App />); });
  await flush();
  await flush();
  const all = allText(r);
  expect(all).toContain('brief.pdf');
  expect(all).toContain('DCIM');
  // Langes Drücken wählt aus → Aktionsleiste statt Navigation
  await act(async () => { byLabel(r, 'brief.pdf').props.onLongPress(); });
  expect(allText(r)).toContain('1 ausgewählt');
  // In den Papierkorb
  const trashBtn = pressable(r, 'In den Papierkorb');
  await act(async () => { await trashBtn.props.onPress(); });
  await flush();
  expect(calls).toContainEqual({ FileOp: { op: { Trash: { paths: ['/storage/emulated/0/brief.pdf'] } }, wait: true } });
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
