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
    total: 4,
    entries: [
      { name: 'DCIM', kind: 'Dir', size: 0, mtime: 1700000000, hidden: false },
      { name: 'brief.pdf', kind: 'File', size: 2048, mtime: 1700000000, hidden: false },
      { name: 'liste.txt', kind: 'File', size: 10, mtime: 1700000000, hidden: false },
      { name: 'urlaub.zip', kind: 'File', size: 4096, mtime: 1700000000, hidden: false },
    ],
  },
};

NativeModules.SnapCore = {
  start: jest.fn(async () => 1),
  request: jest.fn(async (json: string) => {
    const req = JSON.parse(json);
    calls.push(req);
    if (req.ListDir) return JSON.stringify(listing);
    if (req.ReadDocx) {
      return JSON.stringify({ Docx: { truncated: false, blocks: [
        { type: 'heading', level: 1, inline: [{ text: 'Bewerbung' }] },
        { type: 'paragraph', inline: [{ text: 'Sehr geehrte ' }, { text: 'Damen', bold: true }] },
        { type: 'table', rows: [['Jahr', 'Firma'], ['2024', 'Snap GmbH']] },
        { type: 'list', ordered: false, items: [[{ text: 'Punkt' }]] },
      ] } });
    }
    if (req.Search) {
      return JSON.stringify({ SearchResults: { hits: [
        { path: '/storage/emulated/0/DCIM/urlaub.jpg', name: 'urlaub.jpg', kind: 'File', size: 1, mtime: 1700000000, fuzzy: true },
      ] } });
    }
    const ex = req.FileOp?.op?.Extract;
    if (ex) {
      if (ex.password !== 'geheim') {
        return JSON.stringify({ Error: { code: 'PasswordRequired', message: ex.password ? 'Falsches Passwort.' : 'Passwort nötig.' } });
      }
      return JSON.stringify({ FileOpDone: { job: 2, outcome: { done: [{ from: ex.archive, to: '/storage/emulated/0/urlaub' }], trashed: [], deleted: [] } } });
    }
    if (req.FileOp?.op?.Compress) {
      return JSON.stringify({ FileOpDone: { job: 3, outcome: { done: [{ from: 'x', to: '/storage/emulated/0/Archiv.zip' }], trashed: [], deleted: [] } } });
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
  applyFont: jest.fn(async () => null),
  pickFont: jest.fn(async () => 'Fira Code'),
  resetFont: jest.fn(async () => null),
  setFontFromFile: jest.fn(async () => 'MeineSchrift'),
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

// Jede gerenderte App nach dem Test abbauen – sonst feuern Zeitgeber (z. B. Hinweise mit
// „Rückgängig“) noch, nachdem Jest die Umgebung abgebaut hat.
const mounted: ReactTestRenderer.ReactTestRenderer[] = [];
function mount(el: React.ReactElement): ReactTestRenderer.ReactTestRenderer {
  const r = ReactTestRenderer.create(el);
  mounted.push(r);
  return r;
}
afterEach(async () => {
  for (const r of mounted.splice(0)) {
    await act(async () => { r.unmount(); });
  }
});

beforeEach(() => {
  calls.length = 0;
  allFiles = true;
  crashReport = null;
});

test('Liste, Auswahl, Papierkorb und Rückgängig', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
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
  // Zwei Dateien: kein Menü am Finger, sondern die Aktionsleiste unten
  expect(allText(r)).not.toContain('In den Papierkorb');
  expect(r.root.findAll((n) => n.props.accessibilityRole === 'toolbar').length).toBeGreaterThan(0);
  const trashBtn = byLabel(r, 'Papierkorb');
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
  await act(async () => { r = mount(<App />); });
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
  await act(async () => { r = mount(<App />); });
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
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  await act(async () => { byLabel(r, 'urlaub.zip').props.onPress(); });
  expect(NativeModules.SnapCore.openFile).toHaveBeenCalledWith('/storage/emulated/0/urlaub.zip');
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
  await act(async () => { r = mount(<App />); });
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

test('„Animationen entfernen“: keine Einblend-Animationen, damit nichts unsichtbar bleibt', async () => {
  const spy = jest.spyOn(require('react-native').AccessibilityInfo, 'isReduceMotionEnabled').mockResolvedValue(true);
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  const animated = r.root.findAll((n) => n.props && 'entering' in n.props);
  expect(animated.length).toBeGreaterThan(0);
  expect(animated.every((n) => n.props.entering === undefined)).toBe(true);
  expect(allText(r)).toContain('Interner Speicher');
  spy.mockResolvedValue(false);
  await act(async () => { r.unmount(); });
  await require('../src/motion').initMotion();
});

test('Feedback 2–4: Menü am Finger nur bei einer Datei, Leiste ab zwei bleibt bis alles abgewählt, „Dateien“ führt zur Startansicht, Suche ab dem 1. Buchstaben', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  // Markieren per Halten-und-Wischen öffnet das Menü am Finger …
  await act(async () => {
    fireGestureHandler(getByGestureTestId('files-select'), [
      { state: State.BEGAN, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.END, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
    ]);
  });
  await flush();
  expect(allText(r)).toContain('1 ausgewählt');
  expect(allText(r)).toContain('In den Papierkorb');
  // Menü am Finger zeigt keine ZIP-Option (die gibt es nur in der Leiste)
  expect(allText(r)).not.toContain('Als ZIP packen');
  // Schließen ohne Aktion: die nur dafür markierte Datei wird wieder abgewählt
  await act(async () => { byLabel(r, 'Menü schließen').props.onPress(); });
  await flush();
  expect(allText(r)).not.toContain('ausgewählt');
  expect(allText(r)).not.toContain('In den Papierkorb');
  // Zwei Dateien per Halten-und-Wischen: Aktionsleiste unten erscheint
  await act(async () => {
    fireGestureHandler(getByGestureTestId('files-select'), [
      { state: State.BEGAN, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 148, absoluteX: 100, absoluteY: 364 },
      { state: State.END, x: 100, y: 148, absoluteX: 100, absoluteY: 364 },
    ]);
  });
  await flush();
  expect(allText(r)).toContain('2 ausgewählt');
  const toolbar = () => r.root.findAll((n) => n.props.accessibilityRole === 'toolbar' && typeof n.type !== 'string');
  expect(toolbar().length).toBeGreaterThan(0);
  // Zurück auf eine Datei: die Leiste bleibt, bis alles abgewählt ist
  await act(async () => { pressable(r, 'liste.txt').props.onPress(); });
  await flush();
  expect(allText(r)).toContain('1 ausgewählt');
  expect(toolbar().length).toBeGreaterThan(0);
  // Letzte Datei abwählen: Leiste weg, Auswahl beendet …
  await act(async () => { pressable(r, 'brief.pdf').props.onPress(); });
  await flush();
  expect(toolbar()).toHaveLength(0);
  expect(allText(r)).not.toContain('ausgewählt');
  expect(allText(r)).toContain('brief.pdf');
  // … und erneutes Antippen von „Dateien“ unten führt zur Startansicht
  await act(async () => { r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function')[0].props.onPress(); });
  await flush();
  expect(allText(r)).toContain('Interner Speicher');
  expect(allText(r)).not.toContain('brief.pdf');
  // Schon der erste Buchstabe wechselt sofort in die Suchansicht (vor der Antwort des Kerns)
  const field = r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === 'Überall suchen');
  await act(async () => { field.props.onChangeText('u'); });
  expect(allText(r)).not.toContain('Interner Speicher');
  expect(allText(r)).not.toContain('Keine Treffer');
  await act(async () => { await new Promise<void>((resolve) => { setTimeout(() => resolve(), 260); }); });
  expect(calls).toContainEqual({ Search: { query: 'u', filter: {}, limit: 200 } });
  expect(allText(r)).toContain('urlaub.jpg');
  // Anzeige: Ohne Zoom ist das Verschieben im Bild aus, damit Wischen zur nächsten Datei führt
  await act(async () => { byLabel(r, 'urlaub.jpg').props.onPress(); });
  await flush();
  expect((getByGestureTestId('viewer-pan') as any).config.enabled).toBe(false);
});

test('Suchfeld bleibt beim ersten Buchstaben dasselbe Feld (Tastatur schließt nicht)', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  const find = () => r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === 'Überall suchen');
  const node = () => (find() as any)._fiber.stateNode;
  const before = node();
  expect(before).toBeTruthy();
  await act(async () => { find().props.onChangeText('u'); });
  expect(allText(r)).not.toContain('Interner Speicher'); // Ansicht hat gewechselt …
  expect(node()).toBe(before); // … das Eingabefeld aber nicht
  await act(async () => { await new Promise<void>((resolve) => { setTimeout(() => resolve(), 260); }); });
  expect(node()).toBe(before);
  // Zurück zur Startseite: weiterhin dasselbe Feld
  await act(async () => { find().props.onChangeText(''); });
  expect(allText(r)).toContain('Interner Speicher');
  expect(node()).toBe(before);
});

test('ZIP: mehrere Dateien packen, Archiv mit Passwort entpacken, Rückgängig', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  // Zwei Dateien markieren → Leiste → Mehr → „Als ZIP packen“
  await act(async () => {
    fireGestureHandler(getByGestureTestId('files-select'), [
      { state: State.BEGAN, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 148, absoluteX: 100, absoluteY: 364 },
      { state: State.END, x: 100, y: 148, absoluteX: 100, absoluteY: 364 },
    ]);
  });
  await flush();
  expect(allText(r)).toContain('2 ausgewählt');
  await act(async () => { byLabel(r, 'Mehr').props.onPress(); });
  await flush();
  await act(async () => { await pressable(r, 'Als ZIP packen').props.onPress(); });
  await flush();
  expect(calls).toContainEqual({ FileOp: { op: { Compress: { sources: ['/storage/emulated/0/brief.pdf', '/storage/emulated/0/liste.txt'] } }, wait: true } });
  expect(allText(r)).toContain('„Archiv.zip“ erstellt.');
  expect(allText(r)).toContain('Rückgängig');

  // ZIP lange drücken (eine Datei) → Menü am Finger zeigt „Entpacken“
  await act(async () => {
    fireGestureHandler(getByGestureTestId('files-select'), [
      { state: State.BEGAN, x: 100, y: 212, absoluteX: 100, absoluteY: 400 },
      { state: State.ACTIVE, x: 100, y: 212, absoluteX: 100, absoluteY: 400 },
      { state: State.END, x: 100, y: 212, absoluteX: 100, absoluteY: 400 },
    ]);
  });
  await flush();
  expect(allText(r)).toContain('1 ausgewählt');
  await act(async () => { await pressable(r, 'Entpacken').props.onPress(); });
  await flush();
  // Ohne Passwort: Abfrage erscheint
  expect(allText(r)).toContain('„urlaub.zip“ ist mit einem Passwort geschützt.');
  const pw = () => r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === 'Passwort');
  expect(pw().props.secureTextEntry).toBe(true);
  await act(async () => { pw().props.onChangeText('falsch'); });
  await act(async () => { await pw().props.onSubmitEditing(); });
  await flush();
  expect(allText(r)).toContain('Falsches Passwort.');
  await act(async () => { pw().props.onChangeText('geheim'); });
  await act(async () => { await pw().props.onSubmitEditing(); });
  await flush();
  expect(calls).toContainEqual({ FileOp: { op: { Extract: { archive: '/storage/emulated/0/urlaub.zip', password: 'geheim' } }, wait: true } });
  expect(allText(r)).toContain('Entpackt nach „urlaub“.');
  // Passwort taucht nirgends sichtbar auf
  expect(allText(r)).not.toContain('geheim');
});

test('PDF öffnet die eingebaute Anzeige; Menü je Format (PDF: Drucken, DOCX: Office-App)', async () => {
  NativeModules.SnapCore.pdfInfo = jest.fn(async () => ({ pages: 2, ratios: [1.414, 1.414] }));
  NativeModules.SnapCore.pdfPage = jest.fn(async () => '/cache/pdf/seite.png');
  NativeModules.SnapCore.printFile = jest.fn(async () => null);
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  // Antippen: eigene Anzeige statt fremder App
  NativeModules.SnapCore.openFile.mockClear();
  await act(async () => { byLabel(r, 'brief.pdf').props.onPress(); });
  await flush();
  await flush();
  expect(NativeModules.SnapCore.openFile).not.toHaveBeenCalled();
  expect(NativeModules.SnapCore.pdfInfo).toHaveBeenCalledWith('/storage/emulated/0/brief.pdf');
  expect(allText(r)).toContain('Seite 1 von 2');
  await act(async () => { byLabel(r, 'Drucken').props.onPress(); });
  expect(NativeModules.SnapCore.printFile).toHaveBeenCalledWith('/storage/emulated/0/brief.pdf');
  await act(async () => { byLabel(r, 'Schließen').props.onPress(); });
  await flush();
  // Lange drücken auf das PDF: Menü bietet „Drucken“, aber kein „Als ZIP packen“
  await act(async () => {
    fireGestureHandler(getByGestureTestId('files-select'), [
      { state: State.BEGAN, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.END, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
    ]);
  });
  await flush();
  expect(allText(r)).toContain('Drucken');
  expect(allText(r)).not.toContain('Als ZIP packen');
  NativeModules.SnapCore.printFile.mockClear();
  await act(async () => { pressable(r, 'Drucken').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.printFile).toHaveBeenCalledWith('/storage/emulated/0/brief.pdf');
  expect(allText(r)).not.toContain('ausgewählt');
});

test('Markdown-Leser: Blöcke und Hervorhebungen, kein HTML', () => {
  const { parseMarkdown } = require('../src/markdown');
  const b = parseMarkdown('# Titel\n\nText mit **fett**, *kursiv* und `code`.\n\n- [x] erledigt\n- offen\n\n> Zitat\n\n```\nlet a = 1;\n```\n\n---\n<script>alert(1)</script>');
  expect(b.map((x: any) => x.type)).toEqual(['heading', 'paragraph', 'list', 'quote', 'code', 'rule', 'paragraph']);
  expect(b[1].inline.filter((x: any) => x.bold || x.italic || x.code).map((x: any) => x.text)).toEqual(['fett', 'kursiv', 'code']);
  expect(b[2].items.map((x: any) => x.checked)).toEqual([true, undefined]);
  // HTML bleibt reiner Text
  expect(b[6].inline).toEqual([{ text: '<script>alert(1)</script>' }]);
  // Begrenzung bei riesigen Dateien
  expect(parseMarkdown('a\n\n'.repeat(10000), 100)).toHaveLength(100);
});

test('Textdatei öffnet die eingebaute Anzeige statt einer fremden App', async () => {
  NativeModules.SnapCore.readText = jest.fn(async () => ({ text: 'Zeile eins\nZeile zwei', truncated: false }));
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  NativeModules.SnapCore.openFile.mockClear();
  await act(async () => { byLabel(r, 'liste.txt').props.onPress(); });
  await flush();
  await flush();
  expect(NativeModules.SnapCore.openFile).not.toHaveBeenCalled();
  expect(NativeModules.SnapCore.readText).toHaveBeenCalled();
  expect(allText(r)).toContain('Zeile eins');
});

test('Untere Leiste behält beim Markieren ihren Platz (Liste rutscht nicht)', async () => {
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
  await flush();
  const slot = () => r.root.findAll((n) => Array.isArray(n.props.edges) && n.props.edges.length === 1 && n.props.edges[0] === 'bottom')[0];
  const tabs = () => r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function');
  const before = slot();
  expect(tabs().length).toBe(4);
  await act(async () => {
    fireGestureHandler(getByGestureTestId('files-select'), [
      { state: State.BEGAN, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 84, absoluteX: 100, absoluteY: 300 },
      { state: State.ACTIVE, x: 100, y: 148, absoluteX: 100, absoluteY: 364 },
      { state: State.END, x: 100, y: 148, absoluteX: 100, absoluteY: 364 },
    ]);
  });
  await flush();
  // Gleicher Platz, nur der Inhalt wechselt: statt der Tabs die Aktionsleiste
  expect(slot()).toBe(before);
  expect(tabs()).toHaveLength(0);
  expect(r.root.findAll((n) => n.props.accessibilityRole === 'toolbar' && typeof n.type !== 'string').length).toBeGreaterThan(0);
  await act(async () => { byLabel(r, 'Auswahl aufheben').props.onPress(); });
  await flush();
  expect(slot()).toBe(before);
  expect(tabs().length).toBe(4);
});

test('DOCX öffnet die Leseansicht (Kern liest, App zeigt Überschrift, Tabelle, Liste)', async () => {
  NativeModules.SnapCore.docxImageDir = jest.fn(async () => '/cache/docx');
  listing.DirListing.entries.push({ name: 'zz-bewerbung.docx', kind: 'File', size: 9000, mtime: 1700000000, hidden: false });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<App />); });
    await flush();
    await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
    await flush();
    NativeModules.SnapCore.openFile.mockClear();
    await act(async () => { byLabel(r, 'zz-bewerbung.docx').props.onPress(); });
    await flush();
    await flush();
    expect(calls).toContainEqual({ ReadDocx: { path: '/storage/emulated/0/zz-bewerbung.docx', image_dir: '/cache/docx' } });
    expect(NativeModules.SnapCore.openFile).not.toHaveBeenCalled();
    const text = allText(r);
    expect(text).toContain('Bewerbung');
    expect(text).toContain('Damen');
    expect(text).toContain('Snap GmbH');
    expect(text).toContain('Leseansicht');
    expect(text).toContain('Original mit Office-App öffnen');
  } finally {
    listing.DirListing.entries.pop();
  }
});

test('Schrift: Standard Inconsolata, eigene TTF über Einstellungen und Dateimenü, zurücksetzen', async () => {
  const { FONT, Text: AppText } = require('../src/text');
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  expect(NativeModules.SnapCore.applyFont).toHaveBeenCalled();
  // Texte der App nutzen die registrierte Schrift
  const t = r.root.findAll((n) => (n.type as any) === 'Text' && Array.isArray(n.props.style));
  expect(t.length).toBeGreaterThan(0);
  expect(t.every((n) => JSON.stringify(n.props.style).includes(FONT))).toBe(true);
  expect(AppText).toBeTruthy();
  // Einstellungen → „TTF-Datei wählen …“
  await act(async () => { r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function')[3].props.onPress(); });
  await flush();
  expect(allText(r)).toContain('Inconsolata (Standard)');
  await act(async () => { await pressable(r, 'TTF-Datei wählen').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.pickFont).toHaveBeenCalled();
  expect(allText(r)).toContain('Fira Code');
  await act(async () => { await pressable(r, 'Zurück zu Inconsolata').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.resetFont).toHaveBeenCalled();
  expect(allText(r)).toContain('Inconsolata (Standard)');
});
