/**
 * Bildschirmlogik mit nachgebildetem Kern (kein Gerät nötig): Liste, Auswahl per langem
 * Drücken, Papierkorb mit Rückgängig, Berechtigungsbildschirm.
 */
import React from 'react';
import ReactTestRenderer, { act } from 'react-test-renderer';
import { Alert, NativeModules } from 'react-native';
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
    if (req.ArchiveList) {
      return JSON.stringify({ Archive: { total_size: 3000, truncated: false, encrypted: false, entries: [
        { name: 'urlaub/', size: 0, dir: true },
        { name: 'urlaub/strand.jpg', size: 2000, dir: false },
        { name: 'liesmich.txt', size: 1000, dir: false },
      ] } });
    }
    if (req.ReadDocx || req.ReadDocument) {
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
  editPreview: jest.fn(async () => '/cache/edit/vorschau.jpg'),
  videoInfo: jest.fn(async () => JSON.stringify({ durationMs: 20000, width: 1920, height: 1080, rotation: 0, audio: true })),
  videoFrames: jest.fn(async () => JSON.stringify(['/cache/edit/v-0.jpg', null])),
  videoSave: jest.fn(async (_p: string, _j: string, mode: string) => (mode === 'temp' ? '/storage/emulated/0/DCIM/.snap-edit-2.mp4' : '/storage/emulated/0/DCIM/clip (bearbeitet).mp4')),
  videoProgress: jest.fn(async () => 42),
  videoCancel: jest.fn(),
  cacheDir: jest.fn(async () => '/data/cache'),
  editSave: jest.fn(async (_p: string, _j: string, mode: string) => (mode === 'temp' ? '/storage/emulated/0/DCIM/.snap-edit-1.jpg' : '/storage/emulated/0/DCIM/urlaub (bearbeitet).jpg')),
  lastCrash: jest.fn(async () => crashReport),
  requestAllFilesAccess: jest.fn(),
  storageRoot: jest.fn(async () => '/storage/emulated/0'),
  ocrImage: jest.fn(async () => 'Rechnung Nr. 42\nGesamt: 19,99 €'),
  ocrPdfPage: jest.fn(async () => null),
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
  // Archive zeigt die App selbst (Inhalt ohne Entpacken, Ordner aufklappbar)
  NativeModules.SnapCore.openFile.mockClear();
  await act(async () => { byLabel(r, 'urlaub.zip').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.openFile).not.toHaveBeenCalled();
  expect(calls).toContainEqual({ ArchiveList: { path: '/storage/emulated/0/urlaub.zip' } });
  expect(allText(r)).toContain('liesmich.txt');
  expect(allText(r)).not.toContain('strand.jpg');
  await act(async () => { pressable(r, 'urlaub').props.onPress(); });
  expect(allText(r)).toContain('strand.jpg');
  const back = () => r.root.findAll((n) => n.props.accessibilityLabel === 'Zurück' && typeof n.props.onPress === 'function').pop()!;
  await act(async () => { back().props.onPress(); });
  expect(allText(r)).not.toContain('strand.jpg');
  // „Entpacken“ nutzt die normale Dateioperation (hier mit Passwortabfrage)
  await act(async () => { await pressable(r, 'Entpacken').props.onPress(); });
  await flush();
  expect(calls).toContainEqual({ FileOp: { op: { Extract: { archive: '/storage/emulated/0/urlaub.zip' } }, wait: true } });
  expect(allText(r)).not.toContain('liesmich.txt');
  await act(async () => { pressable(r, 'Abbrechen').props.onPress(); });
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
  // Anzeige: Bild lässt sich mit zwei Fingern zoomen; Wischen (Blättern/Schließen) ist immer aktiv
  await act(async () => { byLabel(r, 'urlaub.jpg').props.onPress(); });
  await flush();
  expect((getByGestureTestId('viewer-pinch') as any).config.enabled).toBe(true);
  expect((getByGestureTestId('viewer-pan') as any).config.enabled).not.toBe(false);
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
  expect(tabs().length).toBe(3);
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
  expect(tabs().length).toBe(3);
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
    expect(calls).toContainEqual({ ReadDocument: { path: '/storage/emulated/0/zz-bewerbung.docx', image_dir: '/cache/docx' } });
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
  const fam = () => r.root.findAll((n) => (n.type as any) === 'Text' && Array.isArray(n.props.style)).map((n) => n.props.style[0]?.fontFamily);
  expect(fam().length).toBeGreaterThan(0);
  expect(fam().every((f) => f === FONT)).toBe(true);
  expect(AppText).toBeTruthy();
  // Einstellungen → „TTF-Datei wählen …“
  // Einstellungen → Allgemein
  await act(async () => { r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function')[2].props.onPress(); });
  await flush();
  await act(async () => { byLabel(r, 'Allgemein').props.onPress(); });
  await flush();
  expect(allText(r)).toContain('Inconsolata (Standard)');
  await act(async () => { await pressable(r, 'TTF-Datei wählen').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.pickFont).toHaveBeenCalled();
  expect(allText(r)).toContain('Fira Code');
  // Neuer Schriftname wird registriert und von allen Texten übernommen – man bleibt auf der Seite
  expect(NativeModules.SnapCore.applyFont).toHaveBeenLastCalledWith('SnapFont-1');
  expect(fam().every((f) => f === 'SnapFont-1')).toBe(true);
  expect(allText(r)).toContain('Schriftart');
  await act(async () => { await pressable(r, 'Zurück zu Inconsolata').props.onPress(); });
  await flush();
  expect(NativeModules.SnapCore.resetFont).toHaveBeenCalled();
  expect(allText(r)).toContain('Inconsolata (Standard)');
});

test('Unten Dateien/Fotos/Einstellungen; Geräte in den Einstellungen; Startseite zeigt Gerätespeicher; Menü ohne „Öffnen“', async () => {
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'PairList') {
      calls.push(req);
      return JSON.stringify({ Devices: { devices: [
        { node_id: 'aaa', name: 'Laptop', paired_at: 1700000000, last_sync_at: null, grants: [], online: true, storage: { total: 512e9, free: 200e9, updated_at: 1 } },
        { node_id: 'bbb', name: 'Tablet', paired_at: 1700000000, last_sync_at: null, grants: [], online: false },
      ] } });
    }
    if (req === 'Peers') return JSON.stringify({ Peers: { peers: [{ node_id: 'aaa', status: 'online' }] } });
    return origRequest(json);
  });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<App />); });
    await flush();
    const tabs = r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function');
    expect(tabs.map((n) => n.props.children?.[1]?.props?.children ?? '')).toEqual(['Dateien', 'Fotos', 'Einstellungen']);
    // Startseite: Laptop mit Belegung, Tablet nicht verbunden
    const home = allText(r);
    expect(home).toContain('Laptop');
    expect(home).toContain('von 477 GB belegt');
    expect(home).toContain('Tablet');
    expect(home).toContain('Nicht verbunden');
    // Einstellungen: Liste der Unterkategorien, Geräte als Unterseite
    await act(async () => { tabs[2].props.onPress(); });
    await flush();
    expect(allText(r)).toContain('2 gekoppelt · 1 verbunden');
    await act(async () => { byLabel(r, 'Geräte').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Gekoppelte Geräte');
    expect(allText(r)).toContain('186 GB frei');
    // Erneutes Antippen des Tabs: zurück zur Liste
    await act(async () => { r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function')[2].props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Über Snap');
    expect(allText(r)).not.toContain('Gekoppelte Geräte');
    // Kontextmenü am Finger ohne „Öffnen“
    await act(async () => { r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function')[0].props.onPress(); });
    await flush();
    await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
    await flush();
    await act(async () => {
      fireGestureHandler(getByGestureTestId('files-select'), [
        { state: State.BEGAN, x: 100, y: 148, absoluteX: 100, absoluteY: 300 },
        { state: State.ACTIVE, x: 100, y: 148, absoluteX: 100, absoluteY: 300 },
        { state: State.END, x: 100, y: 148, absoluteX: 100, absoluteY: 300 },
      ]);
    });
    await flush();
    const menu = r.root
      .findAll((n) => n.props.accessibilityRole === 'menuitem' && typeof n.type !== 'string')
      .map((n) => n.findAll((c) => (c.type as any) === 'Text').map((c) => [c.props.children].flat().join('')).join(''));
    expect(menu.length).toBeGreaterThan(0);
    expect(menu).not.toContain('Öffnen');
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Bildbearbeitung: Seitenverhältnis, Drehen, als Kopie speichern bzw. Original ersetzen', async () => {
  const { fitAspect } = require('../src/ImageEditor');
  // Querformat 4:3, Ausschnitt 1:1 → mittig, volle Höhe
  const sq = fitAspect(1, 4 / 3);
  expect(sq.h).toBeCloseTo(1);
  expect(sq.w).toBeCloseTo(0.75);
  expect(sq.x).toBeCloseTo(0.125);
  expect(fitAspect(null, 2)).toEqual({ x: 0, y: 0, w: 1, h: 1 });
  const RN = require('react-native');
  const getSize = jest.spyOn(RN.Image, 'getSize').mockImplementation((_u: any, ok: any) => ok(4000, 3000));
  try {
    for (const mode of ['copy', 'replace'] as const) {
      calls.length = 0;
      let r!: ReactTestRenderer.ReactTestRenderer;
      await act(async () => { r = mount(<App />); });
      await flush();
      // Bild über die Suche finden und in der Anzeige auf „Bearbeiten“
      const field = r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === 'Überall suchen');
      await act(async () => { field.props.onChangeText('urlaub'); });
      await act(async () => { await new Promise<void>((resolve) => { setTimeout(() => resolve(), 260); }); });
      await act(async () => { byLabel(r, 'urlaub.jpg').props.onPress(); });
      await flush();
      await act(async () => { byLabel(r, 'Bearbeiten').props.onPress(); });
      await act(async () => { await new Promise<void>((resolve) => { setTimeout(() => resolve(), 150); }); });
      expect(NativeModules.SnapCore.editPreview).toHaveBeenCalled();
      // Ohne Änderung ist Speichern gesperrt; Drehen ändert etwas
      expect(byLabel(r, 'Speichern').props.disabled).toBe(true);
      await act(async () => { byLabel(r, 'Drehen').props.onPress(); });
      expect(byLabel(r, 'Speichern').props.disabled).toBe(false);
      await act(async () => { byLabel(r, 'Speichern').props.onPress(); });
      await flush();
      await act(async () => { await pressable(r, mode === 'copy' ? 'Als Kopie speichern' : 'Original ersetzen').props.onPress(); });
      await flush();
      const saveCall = NativeModules.SnapCore.editSave.mock.calls.at(-1);
      expect(saveCall[0]).toBe('/storage/emulated/0/DCIM/urlaub.jpg');
      expect(JSON.parse(saveCall[1]).rot).toBe(270);
      if (mode === 'copy') {
        expect(saveCall[2]).toBe('copy');
        expect(allText(r)).toContain('Gespeichert als „urlaub (bearbeitet).jpg“.');
      } else {
        expect(saveCall[2]).toBe('temp');
        expect(calls).toContainEqual({ FileOp: { op: { Replace: { path: '/storage/emulated/0/DCIM/urlaub.jpg', with: '/storage/emulated/0/DCIM/.snap-edit-1.jpg' } }, wait: true } });
        expect(allText(r)).toContain('Das Original liegt im Papierkorb');
      }
      await act(async () => { r.unmount(); });
      mounted.length = 0;
    }
  } finally {
    getSize.mockRestore();
  }
});

test('Videobearbeitung: Zeitangaben, Einpassen, Ton aus, Drehen, Speichern', async () => {
  const { clock, fitVideo } = require('../src/VideoEditor');
  expect(clock(75500)).toBe('1:16');
  expect(clock(3723000)).toBe('1:02:03');
  // Querformat um 90° gedreht → Hochformat in der Bühne
  const box = fitVideo({ width: 1920, height: 1080, rotation: 0 }, 90, { w: 400, h: 800 });
  expect(box.w).toBeCloseTo(400);
  expect(box.h).toBeCloseTo(400 * (1920 / 1080));
  const VideoEditor = require('../src/VideoEditor').default;
  for (const mode of ['copy', 'replace'] as const) {
    calls.length = 0;
    const notes: string[] = [];
    let closed: boolean | null = null;
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => {
      r = mount(<VideoEditor path="/storage/emulated/0/DCIM/clip.mp4" name="clip.mp4" onClose={(c: boolean) => { closed = c; }} notify={(t: string) => notes.push(t)} />);
    });
    await flush();
    expect(allText(r)).toContain('0:00 – 0:20');
    expect(byLabel(r, 'Speichern').props.disabled).toBe(true);
    await act(async () => { pressable(r, 'Ton an').props.onPress(); });
    await act(async () => { byLabel(r, 'Drehen').props.onPress(); });
    expect(byLabel(r, 'Speichern').props.disabled).toBe(false);
    await act(async () => { byLabel(r, 'Speichern').props.onPress(); });
    await flush();
    await act(async () => { await pressable(r, mode === 'copy' ? 'Als Kopie speichern' : 'Original ersetzen').props.onPress(); });
    await flush();
    const call = NativeModules.SnapCore.videoSave.mock.calls.at(-1);
    expect(call[0]).toBe('/storage/emulated/0/DCIM/clip.mp4');
    expect(JSON.parse(call[1])).toEqual({ startMs: 0, endMs: 20000, mute: true, rot: 270 });
    expect(closed).toBe(true);
    if (mode === 'copy') {
      expect(call[2]).toBe('copy');
      expect(notes).toContain('Gespeichert als „clip (bearbeitet).mp4“.');
    } else {
      expect(call[2]).toBe('temp');
      expect(calls).toContainEqual({ FileOp: { op: { Replace: { path: '/storage/emulated/0/DCIM/clip.mp4', with: '/storage/emulated/0/DCIM/.snap-edit-2.mp4' } }, wait: true } });
    }
    await act(async () => { r.unmount(); });
    mounted.length = 0;
  }
});

test('Anderes Gerät durchsuchen: Freigaben, Ordner, Öffnen über den Cache, Video streamen', async () => {
  const { safeName, cachePath } = require('../src/RemoteBrowser');
  expect(safeName('../../etc/passwd')).toBe('.._.._etc_passwd');
  expect(safeName('..')).toBe('Datei');
  const H = 'a'.repeat(64);
  expect(cachePath('/c', { hash: H, name: 'x/../y.pdf' })).toBe(`/c/remote/${'a'.repeat(32)}/x_.._y.pdf`);
  expect(cachePath('/c', { hash: '../x', name: 'y' })).toBeNull();

  const RemoteBrowser = require('../src/RemoteBrowser').default;
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  const file = (name: string, n: number) => ({ origin: 'aaa', origin_name: 'Laptop', hash: String(n).repeat(64).slice(0, 64), name, size: 2048, created_at: 0, path: null, share: 'Fotos', rel_path: name });
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    calls.push(req);
    const rd = req.RemoteDir;
    if (rd) {
      if (!rd.share) return JSON.stringify({ RemoteDir: { dirs: ['Musik', 'Fotos'], files: [], next: null } });
      if (rd.dir === '') return JSON.stringify({ RemoteDir: { dirs: ['2024'], files: [file('bericht.pdf', 1), file('clip.mp4', 2)], next: null } });
      return JSON.stringify({ RemoteDir: { dirs: [], files: [], next: null } });
    }
    if (req.Download) return JSON.stringify({ Downloaded: { path: req.Download.output ?? '/storage/emulated/0/Download/Snap/bericht.pdf' } });
    if (req.StreamUrl) return JSON.stringify({ StreamUrl: { url: 'http://127.0.0.1:4321/s/abc', name: 'clip.mp4', mime: 'video/mp4' } });
    return origRequest(json);
  });
  try {
    const opened: [string, string][] = [];
    const streamed: string[] = [];
    let closed = false;
    const th = require('../src/theme').snapTheme(false);
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => {
      r = mount(<RemoteBrowser device={{ node_id: 'aaa', name: 'Laptop', online: true }} theme={th} notify={() => {}} onClose={() => { closed = true; }} onOpen={(p: string, n: string) => opened.push([p, n])} onStream={(u: string) => streamed.push(u)} />);
    });
    await flush();
    // Wurzel: Freigaben sortiert
    const root = allText(r);
    expect(root.indexOf('Fotos')).toBeLessThan(root.indexOf('Musik'));
    await act(async () => { byLabel(r, 'Fotos').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ RemoteDir: { device: 'aaa', share: 'Fotos', dir: '', after: null, limit: 300 } });
    // Ordner vor Dateien
    const inShare = allText(r);
    expect(inShare.indexOf('2024')).toBeLessThan(inShare.indexOf('bericht.pdf'));
    await act(async () => { byLabel(r, 'bericht.pdf').props.onPress(); });
    await flush();
    expect(opened).toEqual([[`/data/cache/remote/${'1'.repeat(32)}/bericht.pdf`, 'bericht.pdf']]);
    await act(async () => { byLabel(r, 'clip.mp4').props.onPress(); });
    await flush();
    expect(streamed).toEqual(['http://127.0.0.1:4321/s/abc']);
    // Unterordner und zurück bis zum Schließen
    await act(async () => { byLabel(r, '2024').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Dieser Ordner ist leer.');
    await act(async () => { byLabel(r, 'Zurück').props.onPress(); });
    await flush();
    await act(async () => { byLabel(r, 'Zurück').props.onPress(); });
    await flush();
    await act(async () => { byLabel(r, 'Zurück').props.onPress(); });
    expect(closed).toBe(true);
    await act(async () => { r.unmount(); });
    mounted.length = 0;
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Anderes Gerät: Archiv dort entpacken, mit Passwortabfrage', async () => {
  const RemoteBrowser = require('../src/RemoteBrowser').default;
  const RN = require('react-native');
  const alert = jest.spyOn(RN.Alert, 'alert').mockImplementation((_t: any, _m: any, buttons: any) => {
    buttons.find((b: any) => String(b.text).includes('entpacken')).onPress();
  });
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  const zip = { origin: 'aaa', origin_name: 'Laptop', hash: '3'.repeat(64), name: 'tresor.zip', size: 10, created_at: 0, path: null, share: 'Fotos', rel_path: 'Archiv/tresor.zip' };
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    calls.push(req);
    if (req.RemoteDir) return JSON.stringify({ RemoteDir: { dirs: [], files: [zip], next: null } });
    if (req.RemoteOp) {
      const ok = req.RemoteOp.op.password === 'richtig123';
      return JSON.stringify(ok ? { Success: { message: 'Laptop: Erledigt' } } : { Error: { code: 'PasswordRequired', message: 'Passwort nötig.' } });
    }
    return origRequest(json);
  });
  try {
    const notes: string[] = [];
    const th = require('../src/theme').snapTheme(false);
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => {
      r = mount(<RemoteBrowser device={{ node_id: 'aaa', name: 'Laptop', online: true }} theme={th} notify={(x: string) => notes.push(x)} onClose={() => {}} onOpen={() => {}} onStream={() => {}} />);
    });
    await flush();
    await act(async () => { byLabel(r, 'tresor.zip').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ RemoteOp: { device: 'aaa', op: { op: 'Extract', share: 'Fotos', archive: 'Archiv/tresor.zip' } } });
    // Passwort abfragen und erneut senden
    const field = r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === 'Passwort');
    await act(async () => { field.props.onChangeText('richtig123'); });
    await act(async () => { pressable(r, 'Entpacken').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ RemoteOp: { device: 'aaa', op: { op: 'Extract', share: 'Fotos', archive: 'Archiv/tresor.zip', password: 'richtig123' } } });
    expect(notes.some((n) => n.includes('Auf Laptop entpackt'))).toBe(true);
    await act(async () => { r.unmount(); });
    mounted.length = 0;
  } finally {
    alert.mockRestore();
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Zoom: Fingerpunkt bleibt stehen, Grenzen mit Gummiband, Seiten finden, Auflösungsstufen', () => {
  const { renderStep, MAX_ZOOM, layoutPages } = require('../src/PdfViewer');
  const z = require('../src/zoom');
  expect(renderStep(1)).toBe(1);
  expect(renderStep(1.2)).toBe(1.5);
  expect(renderStep(9)).toBe(MAX_ZOOM);
  // Erlaubte Verschiebung: Inhalt so groß wie der Ausschnitt → fest; doppelt so groß → bis −400
  expect(z.range(1, 400, 0, 400)).toEqual([0, 0]);
  expect(z.range(2, 400, 0, 400)).toEqual([-400, 0]);
  // Kurzer Inhalt: zentriert bzw. oben angelegt
  expect(z.range(1, 400, 0, 200)).toEqual([100, 100]);
  expect(z.range(1, 400, 0, 200, true)).toEqual([0, 0]);
  // Gummiband: über die Grenze nur ein Teil der Bewegung
  expect(z.rubber(-100, 0, 50)).toBeCloseTo(-35);
  expect(z.rubber(20, 0, 50)).toBe(20);
  const over = z.rubberScale(10, 1, 5);
  expect(over).toBeGreaterThan(5);
  expect(over).toBeLessThan(10);
  expect(z.rubberScale(0.5, 1, 5)).toBeGreaterThan(0.5);
  // Zoom ×2 um den Punkt (100|100): dieser Punkt bleibt an derselben Stelle
  const to = z.settleTarget(2, -100, -100, 100, 100, 1, 5, 400, 800, 0, 0, 400, 800);
  expect(to).toEqual({ s: 2, x: -100, y: -100 });
  // Unter das Minimum gezogen → zurück auf 1 und in die Grenzen
  const back = z.settleTarget(0.7, 60, 120, 200, 400, 1, 5, 400, 800, 0, 0, 400, 800);
  expect(back).toEqual({ s: 1, x: 0, y: 0 });
  // Seiten im Dokument finden
  const lay = layoutPages([1.4, 1.4, 0.7]);
  expect(lay.offsets[0]).toBeGreaterThan(0);
  expect(z.pageAt(lay.offsets, 0)).toBe(0);
  expect(z.pageAt(lay.offsets, lay.offsets[1] + 1)).toBe(1);
  expect(z.pageAt(lay.offsets, lay.height)).toBe(2);
});

test('Zoom-Mittelpunkt bleibt: erster Finger zuerst gehoben oder dritter Finger dazu – kein Sprung', () => {
  const z = require('../src/zoom');
  // Start bei Zoom 1, Finger-Mittelpunkt (200|300)
  let st = z.pinchBase(1, 0, 0, 200, 300, 1, 2);
  let cur = { s: 1, x: 0, y: 0 };
  // Auf ×2 aufziehen, Mittelpunkt bleibt bei (200|300) → Inhaltspunkt bleibt dort
  let r = z.pinchStep(st, cur, 2, 200, 300, 2, 1, 6);
  expect(r.s).toBe(2);
  expect(r.x).toBe(-200);
  expect(r.y).toBe(-300);
  ({ st } = r);
  cur = { s: r.s, x: r.x, y: r.y };
  // Ein Finger wird zuerst gehoben: Mittelpunkt springt zum anderen Finger → nichts bewegt sich
  r = z.pinchStep(st, cur, 2, 320, 420, 1, 1, 6);
  expect({ s: r.s, x: r.x, y: r.y }).toEqual(cur);
  // Einrasten mit dem letzten gültigen Fingerpunkt: keine Verschiebung
  const to = z.settleTarget(cur.s, cur.x, cur.y, r.st.fx, r.st.fy, 1, 6, 400, 800, 0, 0, 400, 800);
  expect(to).toEqual({ s: 2, x: -200, y: -300 });
  // Dritter Finger kommt dazu (Mittelpunkt springt): neu bezogen, kein Sprung
  r = z.pinchStep(st, cur, 2, 260, 330, 3, 1, 6);
  expect({ s: r.s, x: r.x, y: r.y }).toEqual(cur);
  const r2 = z.pinchStep(r.st, cur, 2, 260, 330, 3, 1, 6);
  expect({ s: r2.s, x: r2.x, y: r2.y }).toEqual(cur);
});

test('Sortieren nach Dateiformat: Ordner zuerst, Formate gruppiert, darin neueste zuerst', () => {
  const { entryOrder } = require('../src/Files');
  const e = (name: string, kind: string, mtime: number) => ({ name, kind, mtime, size: 0 });
  const list = [e('b.pdf', 'File', 1), e('Fotos', 'Dir', 5), e('a.jpg', 'File', 3), e('c.pdf', 'File', 9), e('LIESMICH', 'File', 7), e('d.jpg', 'File', 8)];
  list.sort(entryOrder('type', new Intl.Collator('de', { numeric: true, sensitivity: 'base' })));
  expect(list.map((x) => x.name)).toEqual(['Fotos', 'd.jpg', 'a.jpg', 'c.pdf', 'b.pdf', 'LIESMICH']);
});

test('PDF: Textmarker zeichnen und speichern, Signieren mit ID, Hinweis zur Signatur', async () => {
  const { signatureSummary } = require('../src/PdfViewer');
  const base = { field: 'Signatur1', issuer: 'Max', fingerprint: 'ab'.repeat(32), algorithm: 'ECDSA P-256 / SHA-256', signed_at: null, problem: null };
  expect(signatureSummary([])).toBeNull();
  expect(signatureSummary([{ ...base, valid: true, covers_document: true, signer: 'Max', self_signed: true, own: true }]).kind).toBe('ok');
  expect(signatureSummary([{ ...base, valid: true, covers_document: true, signer: 'Eva', self_signed: true, own: false }]).kind).toBe('warn');
  expect(signatureSummary([{ ...base, valid: true, covers_document: false, signer: 'Max', self_signed: false, own: false }]).text).toContain('danach geändert');
  expect(signatureSummary([{ ...base, valid: false, covers_document: true, signer: 'Max', self_signed: false, own: false, problem: 'Hash passt nicht' }]).kind).toBe('bad');

  NativeModules.SnapCore.pdfInfo = jest.fn(async () => ({ pages: 1, ratios: [1.414] }));
  NativeModules.SnapCore.pdfPage = jest.fn(async () => '/cache/pdf/seite.png');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let hasId = false;
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    calls.push(req);
    if (req.PdfVerify) return JSON.stringify({ PdfSignatures: { items: [] } });
    if (req === 'SignIdInfo') return JSON.stringify({ SignId: { id: hasId ? { name: 'Max', created: 1, fingerprint: 'ab'.repeat(32) } : null, has_signature: hasId } });
    if (req.SignIdCreate) { hasId = true; return JSON.stringify({ SignId: { id: { name: req.SignIdCreate.name, created: 1, fingerprint: 'ab'.repeat(32) }, has_signature: false } }); }
    if (req.SignatureSave) return JSON.stringify({ Signature: { strokes: req.SignatureSave.strokes } });
    if (req === 'SignatureGet') return JSON.stringify({ Signature: { strokes: [[[0.1, 0.5], [0.9, 0.5]]] } });
    if (req.PdfMark) return JSON.stringify({ PdfWritten: { path: '/storage/emulated/0/brief (markiert).pdf' } });
    if (req.PdfSign) {
      if (req.PdfSign.password !== 'geheim-123') return JSON.stringify({ Error: { code: 'PasswordRequired', message: 'Falsches Passwort.' } });
      return JSON.stringify({ PdfWritten: { path: '/storage/emulated/0/.snap-edit-1.pdf' } });
    }
    return origRequest(json);
  });
  try {
    const PdfViewer = require('../src/PdfViewer').default;
    const notes: string[] = [];
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => {
      r = mount(<PdfViewer path="/storage/emulated/0/brief.pdf" name="brief.pdf" notify={(x: string) => notes.push(x)} onClose={() => {}} />);
    });
    await flush();
    expect(calls).toContainEqual({ PdfVerify: { path: '/storage/emulated/0/brief.pdf' } });

    // Textmarker: zeichnen (Geste auf der Seite), speichern als Kopie
    await act(async () => { byLabel(r, 'Markieren').props.onPress(); });
    await act(async () => {
      fireGestureHandler(getByGestureTestId('mark-page-0'), [
        { state: State.BEGAN, x: 10, y: 20 },
        { state: State.ACTIVE, x: 100, y: 22 },
        { state: State.ACTIVE, x: 200, y: 24 },
        { state: State.END, x: 200, y: 24 },
      ]);
    });
    await act(async () => { pressable(r, 'Fertig').props.onPress(); });
    await act(async () => { await pressable(r, 'Als Kopie speichern').props.onPress(); });
    await flush();
    const mark = (calls as any[]).find((c) => c.PdfMark)?.PdfMark;
    expect(mark.mode).toBe('copy');
    expect(mark.layers[0].color).toBe('#ffe14d');
    expect(mark.layers[0].strokes[0].length).toBeGreaterThan(1);
    expect(notes.some((n) => n.includes('Markierungen gespeichert'))).toBe(true);

    // Signieren: erst ID anlegen, Unterschrift zeichnen, platzieren, Passwort (erst falsch)
    await act(async () => { await byLabel(r, 'Signieren').props.onPress(); });
    await flush();
    const field = (label: string) => r.root.find((n) => (n.type as any) === 'TextInput' && n.props.accessibilityLabel === label);
    await act(async () => { field('Name').props.onChangeText('Max Muster'); });
    await act(async () => { field('Passwort').props.onChangeText('geheim-123'); });
    await act(async () => { field('Passwort wiederholen').props.onChangeText('geheim-123'); });
    await act(async () => { await pressable(r, 'Erstellen').props.onPress(); });
    await flush();
    await act(async () => {
      fireGestureHandler(getByGestureTestId('signature-pad'), [
        { state: State.BEGAN, x: 10, y: 50 },
        { state: State.ACTIVE, x: 150, y: 40 },
        { state: State.END, x: 150, y: 40 },
      ]);
    });
    await act(async () => { await pressable(r, 'Übernehmen').props.onPress(); });
    await flush();
    expect((calls as any[]).some((c) => c.SignatureSave)).toBe(true);
    await act(async () => { pressable(r, 'Weiter').props.onPress(); });
    await act(async () => { pressable(r, 'Original ersetzen').props.onPress(); });
    await act(async () => { field('Passwort').props.onChangeText('falsch-123'); });
    await act(async () => { await pressable(r, 'Signieren').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Falsches Passwort.');
    await act(async () => { field('Passwort').props.onChangeText('geheim-123'); });
    await act(async () => { await pressable(r, 'Signieren').props.onPress(); });
    await flush();
    const sign = (calls as any[]).filter((c) => c.PdfSign).at(-1).PdfSign;
    expect(sign.mode).toBe('temp');
    expect(sign.placement.page).toBe(0);
    // Unterschrift liegt im platzierten Feld (rechts unten)
    expect(sign.placement.strokes[0][0][0]).toBeGreaterThan(0.5);
    // Nach dem Speichern der markierten Kopie zeigt die Anzeige diese – signiert wird also sie
    expect(sign.path).toBe('/storage/emulated/0/brief (markiert).pdf');
    expect(calls).toContainEqual({ FileOp: { op: { Replace: { path: '/storage/emulated/0/brief (markiert).pdf', with: '/storage/emulated/0/.snap-edit-1.pdf' } }, wait: true } });
    expect(notes.some((n) => n.includes('Signiert.'))).toBe(true);
    // Passwort steht nirgends im sichtbaren Text
    expect(allText(r)).not.toContain('geheim-123');
    await act(async () => { r.unmount(); });
    mounted.length = 0;
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Gesten-Callbacks rufen nur Worklets oder runOnJS auf (Absturz „Remote Function“)', () => {
  // Hilfsfunktionen, die in Gesten (UI-Thread) laufen, müssen als Worklet markiert sein.
  const fs = require('fs');
  const path = require('path');
  for (const f of ['PdfViewer.tsx', 'ImageEditor.tsx', 'VideoEditor.tsx']) {
    const src: string = fs.readFileSync(path.join((globalThis as any).process.cwd(), 'src', f), 'utf8');
    const m = src.match(/const clamp = \([^)]*\) => \{\s*'worklet';/);
    expect({ f, worklet: !!m }).toEqual({ f, worklet: true });
  }
});

test('Snap Drop: Sichtbarkeit, Angebot mit Prüfcode annehmen, Kontakt speichern, aus dem Dateimenü senden', async () => {
  const { DeviceEventEmitter } = require('react-native');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let mode = 'off';
  const peer = { id: 'ab'.repeat(32), name: 'Ben', kind: 'laptop', contact: null, via: 'lan', offline: false };
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    const status = () => ({ DropStatus: { mode, until: mode === 'everyone' ? Math.floor(Date.now() / 1000) + 600 : null, name: 'Mein Telefon', fingerprint: 'a1b2 c3d4 e5f6', dir: '/storage/emulated/0/Download/Snap Drop', code: mode === 'off' ? null : 'snapdrop1-abc' } });
    if (req === 'DropStatus') return JSON.stringify(status());
    if (req.DropSetVisibility) {
      calls.push(req);
      mode = req.DropSetVisibility.mode;
      return JSON.stringify(status());
    }
    if (req === 'DropContacts') return JSON.stringify({ DropContacts: { contacts: [] } });
    if (req === 'DropBlocked') return JSON.stringify({ DropBlocked: { blocked: [] } });
    if (req === 'DropConnectInfo') return JSON.stringify({ DropConnectInfo: { info: null } });
    if (req === 'DropOffers') {
      return JSON.stringify({ DropOffers: { offers: [{ id: 7, name: 'Anna', contact: null, code: '123 456', files: [{ name: 'urlaub.jpg', size: 2e6, mime: 'image/jpeg' }, { name: 'notiz.txt', size: 10, mime: 'text/plain' }], total: 2000010, preview: false, expires_at: 0 }] } });
    }
    if (req.DropRespond || req.DropSaveContact || req.DropSend) {
      calls.push(req);
      if (req.DropSaveContact) return JSON.stringify({ DropContact: { key: 'k', name: 'Anna', fingerprint: 'f', added_at: 0, last_seen: 0 } });
      if (req.DropSend) return JSON.stringify({ DropSent: { id: 9, ok: true, error: null, code: '654 321' } });
      return JSON.stringify({ Success: { message: 'ok' } });
    }
    if (req === 'DropNearby' || req.DropBrowse) return JSON.stringify({ DropNearby: { peers: [peer] } });
    return origRequest(json);
  });
  const emit = (ev: object) => act(async () => { DeviceEventEmitter.emit('snap-event', JSON.stringify(ev)); });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<App />); });
    await flush();
    const tabs = () => r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function');

    // Einstellungen › Snap Drop: unsichtbar, dann „Alle“ für 10 Minuten
    await act(async () => { tabs()[2].props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Unsichtbar · Senden an Geräte in der Nähe');
    await act(async () => { byLabel(r, 'Snap Drop').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Niemand sieht dieses Gerät');
    await act(async () => { pressable(r, 'Alle').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ DropSetVisibility: { mode: 'everyone' } });
    expect(allText(r)).toContain('noch 10 Min.');
    expect(allText(r)).toContain('Code teilen');

    // Eingehendes Angebot: Name, „Unbekannt“, Prüfcode, Dateien; Annehmen
    await emit({ type: 'DropOffer', id: 7, name: 'Anna', contact: null, code: '123 456', files: 2, total: 2000010, kinds: ['image', 'text'], preview: false });
    await flush();
    const offer = allText(r);
    expect(offer).toContain('Anna');
    expect(offer).toContain('Unbekannt');
    expect(offer).toContain('123 456');
    expect(offer).toContain('urlaub.jpg');
    expect(offer).toContain('Ablehnen und blockieren');
    await act(async () => { pressable(r, 'Annehmen').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ DropRespond: { id: 7, accept: true, block: false } });
    expect(allText(r)).not.toContain('Ablehnen und blockieren');

    // Fertig: Hinweis mit „Als Kontakt speichern“
    await emit({ type: 'DropFinished', id: 7, incoming: true, ok: true, peer: 'Anna', files: ['/a', '/b'], error: null, can_save_contact: true });
    await flush();
    expect(allText(r)).toContain('2 Dateien von Anna gespeichert.');
    await act(async () => { pressable(r, 'Als Kontakt speichern').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ DropSaveContact: { id: 7 } });

    // Dateien: Menü am Finger → „In der Nähe senden“ → Ben antippen
    await act(async () => { tabs()[0].props.onPress(); });
    await flush();
    await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
    await flush();
    await act(async () => {
      fireGestureHandler(getByGestureTestId('files-select'), [
        { state: State.BEGAN, x: 100, y: 148, absoluteX: 100, absoluteY: 300 },
        { state: State.ACTIVE, x: 100, y: 148, absoluteX: 100, absoluteY: 300 },
        { state: State.END, x: 100, y: 148, absoluteX: 100, absoluteY: 300 },
      ]);
    });
    await flush();
    await act(async () => { pressable(r, 'In der Nähe senden').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Ben');
    await act(async () => { byLabel(r, 'An Ben senden').props.onPress(); });
    await flush();
    const sent = calls.find((c: any) => c.DropSend) as any;
    expect(sent.DropSend.peer).toBe(peer.id);
    expect(sent.DropSend.paths).toEqual(['/storage/emulated/0/liste.txt']);
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('PDF-Text: markieren rastet an Zeilen ein, Auswahl mit langem Drücken kopiert, Rückgängig je Zug', async () => {
  const { rectsToMarks } = require('../src/PdfText');
  // Zeile 10 % breit, 2 % hoch auf A4 (Verhältnis 1,414): Strichbreite = Zeilenhöhe in Seitenbreiten
  const [mk] = rectsToMarks([[0.1, 0.2, 0.3, 0.02]], 1.414);
  expect(mk.width).toBeCloseTo(0.0283, 3);
  expect(mk.stroke[0][0]).toBeCloseTo(0.1 + mk.width / 2, 4);
  expect(mk.stroke[1][0]).toBeCloseTo(0.4 - mk.width / 2, 4);
  expect(mk.stroke[0][1]).toBeCloseTo(0.21, 4);

  NativeModules.SnapCore.pdfInfo = jest.fn(async () => ({ pages: 1, ratios: [1.414] }));
  NativeModules.SnapCore.pdfPage = jest.fn(async () => '/cache/pdf/seite.png');
  NativeModules.SnapCore.pdfTextSupported = jest.fn(() => true);
  NativeModules.SnapCore.pdfSelect = jest.fn(async () => ({ text: 'Mietvertrag', rects: [[0.1, 0.1, 0.4, 0.02], [0.1, 0.13, 0.2, 0.02]] }));
  NativeModules.SnapCore.copyText = jest.fn(async () => null);
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    calls.push(req);
    if (req.PdfVerify) return JSON.stringify({ PdfSignatures: { items: [] } });
    if (req.PdfMark) return JSON.stringify({ PdfWritten: { path: '/storage/emulated/0/brief (markiert).pdf' } });
    return origRequest(json);
  });
  try {
    const PdfViewer = require('../src/PdfViewer').default;
    const notes: string[] = [];
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => {
      r = mount(<PdfViewer path="/storage/emulated/0/brief.pdf" name="brief.pdf" notify={(x: string) => notes.push(x)} onClose={() => {}} />);
    });
    await flush();

    // Lange drücken wählt Text aus → Kopieren
    await act(async () => {
      fireGestureHandler(getByGestureTestId('select-page-0'), [
        { state: State.BEGAN, x: 50, y: 40 },
        { state: State.ACTIVE, x: 50, y: 40 },
        { state: State.ACTIVE, x: 150, y: 60 },
        { state: State.END, x: 150, y: 60 },
      ]);
    });
    await flush();
    await act(async () => { pressable(r, 'Kopieren').props.onPress(); });
    await flush();
    expect(NativeModules.SnapCore.copyText).toHaveBeenCalledWith('Mietvertrag');
    expect(notes).toContain('Text kopiert.');

    // Markieren: Werkzeug „Text“ ist vorgewählt; Ziehen markiert zwei Zeilen
    await act(async () => { byLabel(r, 'Markieren').props.onPress(); });
    await act(async () => {
      fireGestureHandler(getByGestureTestId('textmark-page-0'), [
        { state: State.BEGAN, x: 30, y: 40 },
        { state: State.ACTIVE, x: 30, y: 40 },
        { state: State.ACTIVE, x: 200, y: 60 },
        { state: State.END, x: 200, y: 60 },
      ]);
    });
    await flush();
    // Zweiter Zug und Rückgängig entfernt nur diesen ganzen Zug
    await act(async () => {
      fireGestureHandler(getByGestureTestId('textmark-page-0'), [
        { state: State.BEGAN, x: 30, y: 80 },
        { state: State.ACTIVE, x: 30, y: 80 },
        { state: State.END, x: 90, y: 80 },
      ]);
    });
    await flush();
    await act(async () => { byLabel(r, 'Rückgängig').props.onPress(); });
    await act(async () => { pressable(r, 'Fertig').props.onPress(); });
    await act(async () => { await pressable(r, 'Als Kopie speichern').props.onPress(); });
    await flush();
    const mark = (calls as any[]).find((c) => c.PdfMark)?.PdfMark;
    expect(mark.layers.reduce((n: number, l: any) => n + l.strokes.length, 0)).toBe(2);
    expect(mark.layers.every((l: any) => l.width > 0.02 && l.width < 0.04)).toBe(true);
    expect(notes.some((n) => n.includes('Markierungen gespeichert'))).toBe(true);
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
    delete NativeModules.SnapCore.pdfTextSupported;
  }
});

test('Geteilter Link landet in der Zwischenablage und lässt sich öffnen; Ordner für Geräte freigeben', async () => {
  const { DeviceEventEmitter } = require('react-native');
  NativeModules.SnapCore.copyText = jest.fn(async () => null);
  NativeModules.SnapCore.openLink = jest.fn(async () => null);
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'ShareList') return JSON.stringify({ Shares: { shares: [] } });
    if (req.ShareAdd) {
      calls.push(req);
      return JSON.stringify({ ShareAdded: { share: { id: 1, name: 'DCIM', root: req.ShareAdd.path, auto_grant: true, created_at: 0 }, granted: [] } });
    }
    return origRequest(json);
  });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<App />); });
    await flush();
    await act(async () => {
      DeviceEventEmitter.emit('snap-event', JSON.stringify({ type: 'DropFinished', id: 3, incoming: true, ok: true, peer: 'Anna', files: [], error: null, can_save_contact: false, text: 'https://example.org/rezept', link: 'https://example.org/rezept' }));
    });
    await flush();
    expect(NativeModules.SnapCore.copyText).toHaveBeenCalledWith('https://example.org/rezept');
    expect(allText(r)).toContain('Link von Anna');
    await act(async () => { pressable(r, 'Öffnen').props.onPress(); });
    expect(NativeModules.SnapCore.openLink).toHaveBeenCalledWith('https://example.org/rezept');

    // Ordner „DCIM“ (erste Zeile) lange drücken → „Für Geräte freigeben“
    await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
    await flush();
    await act(async () => {
      fireGestureHandler(getByGestureTestId('files-select'), [
        { state: State.BEGAN, x: 100, y: 20, absoluteX: 100, absoluteY: 200 },
        { state: State.ACTIVE, x: 100, y: 20, absoluteX: 100, absoluteY: 200 },
        { state: State.END, x: 100, y: 20, absoluteX: 100, absoluteY: 200 },
      ]);
    });
    await flush();
    await act(async () => { pressable(r, 'Für Geräte freigeben').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ ShareAdd: { path: '/storage/emulated/0/DCIM', name: null, devices: null } });
    expect(allText(r)).toContain('ist jetzt für deine gekoppelten Geräte sichtbar');
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Formate: APK zeigt App-Infos und installiert, CSV als Tabelle, Visitenkarte, Audio im eingebauten Player', async () => {
  const extra = [
    { name: 'zz-app.apk', kind: 'File', size: 5_000_000, mtime: 1700000000, hidden: false },
    { name: 'zz-daten.csv', kind: 'File', size: 40, mtime: 1700000000, hidden: false },
    { name: 'zz-kontakt.vcf', kind: 'File', size: 60, mtime: 1700000000, hidden: false },
    { name: 'zz-lied.mp3', kind: 'File', size: 3_000_000, mtime: 1700000000, hidden: false },
    { name: 'zz-spiel.xapk', kind: 'File', size: 90_000_000, mtime: 1700000000, hidden: false },
  ];
  listing.DirListing.entries.push(...extra);
  NativeModules.SnapCore.apkInfo = jest.fn(async (p: string) => (p.endsWith('.xapk') ? { label: 'Spiel', package: 'org.beispiel.spiel', version: '1.0', versionCode: 1, self: false, splits: 3, obb: true } : { label: 'Notizen', package: 'org.beispiel.notizen', version: '2.1', versionCode: 21, installedVersion: '2.0', installedCode: 20, self: false }));
  NativeModules.SnapCore.installApk = jest.fn(async (p: string) => (p.endsWith('.xapk') ? 'installed' : 'started'));
  const texts: Record<string, string> = {
    '/storage/emulated/0/zz-daten.csv': 'Name;Ort\n"Müller; Anna";Berlin\nBob;"Köln\nPorz"\n',
    '/storage/emulated/0/zz-kontakt.vcf': 'BEGIN:VCARD\r\nFN:Erika Muster\r\nTEL;TYPE=cell:+49 30 1234\r\nEMAIL:erika@example.org\r\nEND:VCARD\r\n',
  };
  NativeModules.SnapCore.readText = jest.fn(async (p: string) => ({ text: texts[p] ?? '', truncated: false }));
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<App />); });
    await flush();
    await act(async () => { byLabel(r, 'Interner Speicher').props.onPress(); });
    await flush();
    NativeModules.SnapCore.openFile.mockClear();

    // APK: Infos, Aktualisierung erkannt, Installer gestartet
    await act(async () => { byLabel(r, 'zz-app.apk').props.onPress(); });
    await flush();
    expect(NativeModules.SnapCore.apkInfo).toHaveBeenCalledWith('/storage/emulated/0/zz-app.apk');
    let text = allText(r);
    expect(text).toContain('Notizen');
    expect(text).toContain('org.beispiel.notizen');
    expect(text).toContain('Aktualisierung von 2.0 auf 2.1');
    await act(async () => { await pressable(r, 'Aktualisieren').props.onPress(); });
    await flush();
    expect(NativeModules.SnapCore.installApk).toHaveBeenCalledWith('/storage/emulated/0/zz-app.apk');
    expect(allText(r)).not.toContain('org.beispiel.notizen');

    // App-Paket (mehrere APKs): Teile und OBB-Hinweis, Ergebnis der Installer-Sitzung
    await act(async () => { byLabel(r, 'zz-spiel.xapk').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('App-Paket aus 3 Teilen');
    expect(allText(r)).toContain('OBB');
    await act(async () => { await pressable(r, 'Installieren').props.onPress(); });
    await flush();
    expect(NativeModules.SnapCore.installApk).toHaveBeenCalledWith('/storage/emulated/0/zz-spiel.xapk');
    expect(allText(r)).toContain('„Spiel“ installiert.');

    // CSV: Trennzeichen erkannt, Anführungszeichen mit Trennzeichen und Zeilenumbruch
    await act(async () => { byLabel(r, 'zz-daten.csv').props.onPress(); });
    await flush();
    text = allText(r);
    expect(text).toContain('Müller; Anna');
    expect(text).toContain('Köln\nPorz');
    expect(text).toContain('Berlin');
    const close = () => r.root.findAll((n) => ['Zurück', 'Schließen'].includes(n.props.accessibilityLabel) && typeof n.props.onPress === 'function').pop()!;
    await act(async () => { close().props.onPress(); });

    // Visitenkarte: Name als Überschrift, Felder als Tabelle
    await act(async () => { byLabel(r, 'zz-kontakt.vcf').props.onPress(); });
    await flush();
    text = allText(r);
    expect(text).toContain('Erika Muster');
    expect(text).toContain('Telefon');
    expect(text).toContain('+49 30 1234');
    expect(text).toContain('erika@example.org');
    await act(async () => { close().props.onPress(); });

    // Audio: eingebauter Player statt fremder App
    await act(async () => { byLabel(r, 'zz-lied.mp3').props.onPress(); });
    await flush();
    expect(r.root.findAll((n) => n.props.accessibilityLabel === 'Abspielen' || n.props.accessibilityLabel === 'Pause').length).toBeGreaterThan(0);
    expect(NativeModules.SnapCore.openFile).not.toHaveBeenCalled();
  } finally {
    listing.DirListing.entries.splice(-extra.length);
  }
});

test('CSV- und Kartenparser: Anführungszeichen, leere Zellen, gleich breite Zeilen, Termine', () => {
  const { parseCsv, parseCard } = require('../src/TextViewer');
  expect(parseCsv('a,b,c\n1,"x,""y""",\n2', ',')).toEqual([['a', 'b', 'c'], ['1', 'x,"y"', ''], ['2', '', '']]);
  expect(parseCsv('a\tb\r\nc\td\r\n', '\t')).toEqual([['a', 'b'], ['c', 'd']]);
  const blocks = parseCard('BEGIN:VEVENT\nSUMMARY:Treffen\nDTSTART:20261126T0930\nLOCATION:Büro\\, Raum 2\nEND:VEVENT');
  expect(blocks[0]).toMatchObject({ type: 'heading' });
  expect(blocks[1].rows).toContainEqual(['Beginn', '26.11.2026 09:30']);
  expect(blocks[1].rows).toContainEqual(['Ort', 'Büro, Raum 2']);
});

test('Entkoppeln durch die Gegenseite: Hinweis und Cache der fremden Dateien geleert', async () => {
  const { DeviceEventEmitter } = require('react-native');
  NativeModules.SnapCore.clearRemoteCache = jest.fn(async () => null);
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<App />); });
  await flush();
  await act(async () => { DeviceEventEmitter.emit('snap-event', JSON.stringify({ type: 'DeviceRemoved', node_id: 'ab' })); });
  await act(async () => { DeviceEventEmitter.emit('snap-event', JSON.stringify({ type: 'DeviceUnpairedByPeer', node_id: 'ab', name: 'Laptop' })); });
  await flush();
  expect(NativeModules.SnapCore.clearRemoteCache).toHaveBeenCalled();
  expect(allText(r)).toContain('„Laptop“ hat die Kopplung aufgehoben');
});

test('Snap Drop im Hintergrund: Einstellungen, Schlüssel-Datei, Akku-Hinweis, wer wecken darf', async () => {
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  const bgState = { enabled: false, wlan: false, who: 'contacts', batteryExempt: false, supported: true };
  NativeModules.SnapDrop = {
    capabilities: jest.fn(async () => ({ ble: true, bleOn: true, advertise: true, joinHotspot: true, sdk: 34 })),
    backgroundState: jest.fn(async () => ({ ...bgState })),
    setBackground: jest.fn((enabled: boolean, wlan: boolean, who: string) => Object.assign(bgState, { enabled, wlan, who })),
    requestBatteryExempt: jest.fn(),
    setEveryoneUntil: jest.fn(),
    wake: jest.fn(async () => true),
    wakeAll: jest.fn(async () => true),
    stopWake: jest.fn(),
    multicast: jest.fn(),
    scan: jest.fn(async () => null),
    advertise: jest.fn(async () => true),
    updateInfo: jest.fn(),
    stopAdvertise: jest.fn(),
    addListener: jest.fn(),
    removeListeners: jest.fn(),
  };
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'DropStatus') return JSON.stringify({ DropStatus: { mode: 'off', until: null, name: 'Mein Telefon', fingerprint: 'f', dir: '/x', code: null } });
    if (req === 'DropContacts') return JSON.stringify({ DropContacts: { contacts: [{ key: 'cc'.repeat(32), name: 'Clara', device_name: 'Clara', renamed: false, fingerprint: 'f', added_at: 0, last_seen: 0 }] } });
    if (req === 'DropBlocked') return JSON.stringify({ DropBlocked: { blocked: [] } });
    if (req === 'DropNearby' || req.DropBrowse) return JSON.stringify({ DropNearby: { peers: [] } });
    if (req.DropWakeExport) {
      calls.push(req);
      return JSON.stringify({ Success: { message: 'ok' } });
    }
    return origRequest(json);
  });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<App />); });
    await flush();
    const tabs = () => r.root.findAll((n) => n.props.accessibilityRole === 'tab' && typeof n.props.onPress === 'function');
    await act(async () => { tabs()[2].props.onPress(); });
    await flush();
    await act(async () => { byLabel(r, 'Snap Drop').props.onPress(); });
    await flush();
    expect(allText(r)).toContain('Wenn Snap geschlossen ist');
    // Einschalten: Einstellung an Android, Schlüssel-Datei, Akku-Hinweis
    const sw = r.root.find((n) => n.props.accessibilityLabel === 'Im Hintergrund empfangen' && typeof n.props.onValueChange === 'function');
    await act(async () => { await sw.props.onValueChange(true); });
    await flush();
    expect(NativeModules.SnapDrop.setBackground).toHaveBeenLastCalledWith(true, false, 'contacts');
    expect(calls).toContainEqual({ DropWakeExport: { enable: true } });
    expect(allText(r)).toContain('Akku-Optimierung für Snap aufheben');
    expect(allText(r)).toContain('Wecken dürfen');
    await act(async () => { pressable(r, 'Wie Sichtbarkeit').props.onPress(); });
    await flush();
    expect(NativeModules.SnapDrop.setBackground).toHaveBeenLastCalledWith(true, false, 'visibility');
    await act(async () => { pressable(r, 'Akku-Optimierung für Snap aufheben').props.onPress(); });
    expect(NativeModules.SnapDrop.requestBatteryExempt).toHaveBeenCalled();
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
    delete NativeModules.SnapDrop;
  }
});

test('Von überall: Kontakt mit Adresse direkt senden, ohne Adresse per Bluetooth wecken', async () => {
  const { DropSendSheet } = require('../src/Drop');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'DropContacts') {
      return JSON.stringify({ DropContacts: { contacts: [
        { key: 'aa'.repeat(32), name: 'Clara', device_name: 'Clara', renamed: false, fingerprint: 'f', added_at: 0, last_seen: 0, remote: true, remote_ok: true },
        { key: 'bb'.repeat(32), name: 'Dora', device_name: 'Dora', renamed: false, fingerprint: 'f', added_at: 0, last_seen: 0, remote: false, remote_ok: true },
      ] } });
    }
    if (req === 'DropNearby' || req.DropBrowse) return JSON.stringify({ DropNearby: { peers: [] } });
    if (req.DropSendContact) {
      calls.push(req);
      return JSON.stringify({ DropSent: { id: 4, ok: true, error: null, code: '111 222' } });
    }
    return origRequest(json);
  });
  const onClose = jest.fn();
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<DropSendSheet paths={['/storage/emulated/0/a.txt']} theme={snapTheme(false)} onClose={onClose} notify={() => {}} />); });
    await flush();
    const text = allText(r);
    expect(text).toContain('Kontakte, nicht in der Nähe');
    expect(text).toContain('Von überall senden – verschlüsselt, auch übers Internet');
    expect(text).toContain('Antippen zum Wecken');
    await act(async () => { await byLabel(r, 'An Clara senden').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ DropSendContact: { contact: 'aa'.repeat(32), paths: ['/storage/emulated/0/a.txt'], text: null, expire: null } });
    expect(allText(r)).toContain('Gesendet');
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Empfang: Fortschritt mit Abbrechen, Verlauf „Zuletzt empfangen“ mit Löschen', async () => {
  const { DeviceEventEmitter } = require('react-native');
  const { ReceiveBar, DropPage } = require('../src/Drop');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let items = [{ id: 1, at: Math.floor(Date.now() / 1000) - 60, peer: 'Anna', kind: 'files', count: 1, files: ['/storage/emulated/0/Download/Snap Drop/foto.jpg'], exists: true }];
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'DropHistory') return JSON.stringify({ DropHistory: { items } });
    if (req === 'DropHistoryClear') { items = []; return JSON.stringify({ Success: { message: 'ok' } }); }
    if (req === 'DropStatus') return JSON.stringify({ DropStatus: { mode: 'off', until: null, name: 'Ich', fingerprint: 'f', dir: '/x', code: null } });
    if (req === 'DropContacts') return JSON.stringify({ DropContacts: { contacts: [] } });
    if (req === 'DropBlocked') return JSON.stringify({ DropBlocked: { blocked: [] } });
    if (req.DropCancel) { calls.push(req); return JSON.stringify({ Success: { message: 'ok' } }); }
    return origRequest(json);
  });
  const emit = (ev: object) => act(async () => { DeviceEventEmitter.emit('snap-event', JSON.stringify(ev)); });
  try {
    let bar!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { bar = mount(<ReceiveBar theme={snapTheme(false)} />); });
    await emit({ type: 'DropOffer', id: 9, name: 'Anna', contact: 'Anna', code: '1', files: 1, total: 100, kinds: [], preview: false });
    await emit({ type: 'DropProgress', id: 9, incoming: true, done_bytes: 40, total_bytes: 100 });
    expect(allText(bar)).toContain('Empfange von Anna');
    await act(async () => { pressable(bar, 'Abbrechen').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ DropCancel: { id: 9 } });
    await emit({ type: 'DropFinished', id: 9, incoming: true, ok: false, peer: 'Anna', files: [], error: 'Abgebrochen', can_save_contact: false });
    expect(allText(bar)).not.toContain('Empfange von');

    let page!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { page = mount(<DropPage theme={snapTheme(false)} notify={() => {}} />); });
    await flush();
    expect(allText(page)).toContain('Zuletzt empfangen');
    expect(allText(page)).toContain('foto.jpg');
    NativeModules.SnapCore.openFile.mockClear();
    await act(async () => { pressable(page, 'foto.jpg').props.onPress(); });
    expect(NativeModules.SnapCore.openFile).toHaveBeenCalledWith('/storage/emulated/0/Download/Snap Drop/foto.jpg');
    await act(async () => { await pressable(page, 'Verlauf löschen').props.onPress(); });
    await flush();
    expect(allText(page)).not.toContain('Zuletzt empfangen');
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('An mehrere senden; Fotos auswählen und per Snap Drop senden', async () => {
  const { DropSendSheet } = require('../src/Drop');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  const peers = [
    { id: 'aa'.repeat(32), name: 'Ben', kind: 'phone', contact: null, via: 'lan', offline: false },
    { id: 'bb'.repeat(32), name: 'Carla', kind: 'laptop', contact: null, via: 'lan', offline: false },
  ];
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'DropContacts') return JSON.stringify({ DropContacts: { contacts: [] } });
    if (req === 'DropNearby' || req.DropBrowse) return JSON.stringify({ DropNearby: { peers } });
    if (req.DropSend) { calls.push(req); return JSON.stringify({ DropSent: { id: 1, ok: true, error: null, code: '1' } }); }
    return origRequest(json);
  });
  const messages: string[] = [];
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<DropSendSheet paths={['/x/a.jpg']} theme={snapTheme(false)} onClose={() => {}} notify={(m: string) => messages.push(m)} />); });
    await flush();
    await act(async () => { pressable(r, 'Mehrere').props.onPress(); });
    await act(async () => { byLabel(r, 'An Ben senden').props.onPress(); });
    await act(async () => { byLabel(r, 'An Carla senden').props.onPress(); });
    expect(allText(r)).toContain('An 2 senden');
    await act(async () => { await pressable(r, 'An 2 senden').props.onPress(); });
    await flush();
    const sent = calls.filter((c: any) => c.DropSend).map((c: any) => c.DropSend.peer).sort();
    expect(sent).toEqual([peers[0].id, peers[1].id].sort());
    expect(messages).toContain('An 2 von 2 gesendet.');
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }

  // Fotos: lange drücken wählt aus, „In der Nähe senden“ übergibt die Auswahl
  const Photos = require('../src/Photos').default;
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req.Timeline) {
      return JSON.stringify({ Timeline: { items: [
        { path: '/DCIM/1.jpg', taken: 1700000000, video: false },
        { path: '/DCIM/2.jpg', taken: 1700000100, video: false },
      ], next: null } });
    }
    return origRequest(json);
  });
  const onDrop = jest.fn();
  try {
    let p!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { p = mount(<Photos theme={snapTheme(false)} onDrop={onDrop} />); });
    await flush();
    const cells = p.root.findAll((n) => typeof n.props.onLongPress === 'function');
    await act(async () => { cells[0].props.onLongPress(); });
    await act(async () => { cells[1].props.onPress(); });
    expect(allText(p)).toContain('2 ausgewählt');
    await act(async () => { byLabel(p, 'In der Nähe senden').props.onPress(); });
    expect(onDrop).toHaveBeenCalledWith(['/DCIM/1.jpg', '/DCIM/2.jpg']);
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Zwischenablage: Recht vergeben, senden, automatisch beim Öffnen, Empfang ohne Ping-Pong', async () => {
  const { Devices } = require('../src/Pages');
  const clip = require('../src/clipboard');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let grants: { capability: string; folder: string | null }[] = [{ capability: 'catalog.read', folder: null }];
  const prefs: Record<string, string> = {};
  NativeModules.SnapCore.getPref = jest.fn(async (k: string) => prefs[k] ?? (k === 'lang' ? 'de' : null));
  NativeModules.SnapCore.setPref = jest.fn((k: string, v: string) => { prefs[k] = v; });
  NativeModules.SnapCore.readClipboard = jest.fn(async () => 'Einkaufsliste');
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'PairList') return JSON.stringify({ Devices: { devices: [{ node_id: 'n1', name: 'Laptop', paired_at: 0, last_sync_at: null, grants }] } });
    if (req === 'ShareList') return JSON.stringify({ Shares: { shares: [] } });
    if (req === 'Peers') return JSON.stringify({ Peers: { peers: [] } });
    if (req.PairGrant) { calls.push(req); grants = [...grants, { capability: 'clipboard.write', folder: null }]; return JSON.stringify({ Success: { message: 'ok' } }); }
    if (req.ClipboardSend) { calls.push(req); return JSON.stringify({ Success: { message: 'ok' } }); }
    return origRequest(json);
  });
  const messages: string[] = [];
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<Devices theme={snapTheme(false)} notify={(m: string) => messages.push(m)} />); });
    await flush();
    await act(async () => { pressable(r, 'Laptop').props.onPress(); });
    const fill = r.root.find((n) => n.props.accessibilityLabel === 'Darf meine Zwischenablage füllen' && typeof n.props.onValueChange === 'function');
    expect(fill.props.value).toBe(false);
    await act(async () => { await fill.props.onValueChange(true); });
    await flush();
    expect(calls).toContainEqual({ PairGrant: { device: 'n1', capability: 'clipboard.write', folder: null } });
    await act(async () => { await pressable(r, 'Zwischenablage senden').props.onPress(); });
    await flush();
    expect(calls).toContainEqual({ ClipboardSend: { device: 'n1', text: 'Einkaufsliste' } });
    // Automatisch: nur Neues, und nie den eben empfangenen Text zurück
    const auto = r.root.find((n) => n.props.accessibilityLabel === 'Zwischenablage automatisch senden' && typeof n.props.onValueChange === 'function');
    await act(async () => { await auto.props.onValueChange(true); });
    calls.length = 0;
    await clip.syncOnForeground();
    expect(calls.filter((c: any) => c.ClipboardSend)).toHaveLength(0); // unverändert seit dem Senden
    NativeModules.SnapCore.readClipboard = jest.fn(async () => 'Neu kopiert');
    await clip.syncOnForeground();
    expect(calls).toContainEqual({ ClipboardSend: { device: 'n1', text: 'Neu kopiert' } });
    calls.length = 0;
    clip.remember('Vom Laptop');
    NativeModules.SnapCore.readClipboard = jest.fn(async () => 'Vom Laptop');
    await clip.syncOnForeground();
    expect(calls.filter((c: any) => c.ClipboardSend)).toHaveLength(0);
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Gerät finden: Recht vergeben, klingeln lassen und stoppen', async () => {
  const { Devices } = require('../src/Pages');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let grants: { capability: string; folder: string | null }[] = [{ capability: 'catalog.read', folder: null }];
  const sent: any[] = [];
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'PairList') return JSON.stringify({ Devices: { devices: [{ node_id: 'n1', name: 'Handy', paired_at: 0, last_sync_at: null, grants }] } });
    if (req === 'ShareList') return JSON.stringify({ Shares: { shares: [] } });
    if (req === 'Peers') return JSON.stringify({ Peers: { peers: [] } });
    if (req.PairGrant) { sent.push(req); grants = [...grants, { capability: req.PairGrant.capability, folder: null }]; return JSON.stringify({ Success: { message: 'ok' } }); }
    if (req.Ring) { sent.push(req); return JSON.stringify({ Success: { message: "'Handy' klingelt 30 Sekunden lang." } }); }
    return origRequest(json);
  });
  const alert = jest.spyOn(Alert, 'alert').mockImplementation(() => {});
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<Devices theme={snapTheme(false)} notify={() => {}} />); });
    await flush();
    await act(async () => { pressable(r, 'Handy').props.onPress(); });
    const may = r.root.find((n) => n.props.accessibilityLabel === 'Darf mein Gerät klingeln lassen' && typeof n.props.onValueChange === 'function');
    expect(may.props.value).toBe(false); // nie Standard
    await act(async () => { await may.props.onValueChange(true); });
    await flush();
    expect(sent).toContainEqual({ PairGrant: { device: 'n1', capability: 'device.ring', folder: null } });
    await act(async () => { await pressable(r, 'Klingeln lassen').props.onPress(); });
    await flush();
    expect(sent).toContainEqual({ Ring: { device: 'n1', stop: false } });
    const buttons = alert.mock.calls.at(-1)?.[2] as { text: string; onPress: () => void }[];
    await act(async () => { buttons.find((b) => b.text === 'Stopp')!.onPress(); });
    await flush();
    expect(sent).toContainEqual({ Ring: { device: 'n1', stop: true } });
  } finally {
    alert.mockRestore();
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Fotos sichern: Freigabe wählen, Regel anlegen und wieder entfernen', async () => {
  const { Devices } = require('../src/Pages');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let rules: any[] = [];
  const sent: any[] = [];
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'PairList') return JSON.stringify({ Devices: { devices: [{ node_id: 'srv', name: 'Server', paired_at: 0, last_sync_at: null, grants: [] }] } });
    if (req === 'ShareList') return JSON.stringify({ Shares: { shares: [] } });
    if (req === 'Peers') return JSON.stringify({ Peers: { peers: [] } });
    if (req === 'UploadRuleList') return JSON.stringify({ UploadRules: { rules } });
    if (req === 'Info') return JSON.stringify({ Info: { device_name: 'Mein/Handy' } });
    if (req.RemoteDir) return JSON.stringify({ RemoteDir: { dirs: ['Fotos', 'Dokumente'], files: [], next: null } });
    if (req.UploadRuleAdd) {
      sent.push(req);
      rules = [{ id: 7, local_dir: req.UploadRuleAdd.path, device: 'srv', share: req.UploadRuleAdd.share, dest_dir: req.UploadRuleAdd.dest_dir, created_at: 0 }];
      return JSON.stringify({ UploadRules: { rules } });
    }
    if (req.UploadRuleRemove) { sent.push(req); rules = []; return JSON.stringify({ Success: { message: 'ok' } }); }
    return origRequest(json);
  });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<Devices theme={snapTheme(false)} notify={() => {}} />); });
    await flush();
    await act(async () => { pressable(r, 'Server').props.onPress(); });
    const sw = () => r.root.find((n) => n.props.accessibilityLabel === 'Fotos hierhin sichern' && typeof n.props.onValueChange === 'function');
    expect(sw().props.value).toBe(false);
    await act(async () => { await sw().props.onValueChange(true); });
    await flush();
    // Zwei Freigaben → Auswahl
    await act(async () => { await pressable(r, 'Fotos').props.onPress(); });
    await flush();
    const add = sent.find((c) => c.UploadRuleAdd)!.UploadRuleAdd;
    expect(add.path).toMatch(/\/DCIM\/Camera$/);
    expect(add).toMatchObject({ device: 'srv', share: 'Fotos', dest_dir: 'Mein Handy' });
    expect(sw().props.value).toBe(true);
    await act(async () => { await sw().props.onValueChange(false); });
    await flush();
    expect(sent).toContainEqual({ UploadRuleRemove: { id: 7 } });
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('Synchron gehaltene Ordner: Liste, beide Richtungen umschalten', async () => {
  const { Devices } = require('../src/Pages');
  const { snapTheme } = require('../src/theme');
  const origRequest = NativeModules.SnapCore.request.getMockImplementation();
  let twoWay = false;
  const sent: any[] = [];
  NativeModules.SnapCore.request.mockImplementation(async (json: string) => {
    const req = JSON.parse(json);
    if (req === 'PairList') return JSON.stringify({ Devices: { devices: [] } });
    if (req === 'ShareList') return JSON.stringify({ Shares: { shares: [] } });
    if (req === 'Peers') return JSON.stringify({ Peers: { peers: [] } });
    if (req === 'PinList') return JSON.stringify({ Pins: { pins: [{ id: 3, origin_name: 'PC', share: 'Docs', prefix: '', files_ok: 4, files_pending: 0, conflicts: 0, two_way: twoWay, held_deletes: 2 }] } });
    if (req.PinTwoWay) { sent.push(req); twoWay = req.PinTwoWay.enabled; return JSON.stringify({ Success: { message: 'ok' } }); }
    return origRequest(json);
  });
  try {
    let r!: ReactTestRenderer.ReactTestRenderer;
    await act(async () => { r = mount(<Devices theme={snapTheme(false)} notify={() => {}} />); });
    await flush();
    const text = JSON.stringify(r.toJSON());
    expect(text).toContain('Synchron gehaltene Ordner');
    expect(text).toContain('2 Löschungen warten');
    const sw = () => r.root.find((n) => n.props.accessibilityLabel === 'Beide Richtungen' && typeof n.props.onValueChange === 'function');
    expect(sw().props.value).toBe(false);
    await act(async () => { await sw().props.onValueChange(true); });
    await flush();
    expect(sent).toContainEqual({ PinTwoWay: { id: 3, enabled: true } });
    expect(sw().props.value).toBe(true);
  } finally {
    NativeModules.SnapCore.request.mockImplementation(origRequest);
  }
});

test('SVG: Skripte, Ereignisse und Verweise nach außen werden entfernt', () => {
  const { sanitizeSvg } = require('../src/SvgViewer');
  const evil = `<?xml version="1.0"?><!DOCTYPE svg [<!ENTITY a "aaaa">]><svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)">
    <script>fetch('https://x.example/')</script><foreignObject><div>hi</div></foreignObject>
    <image href="https://tracker.example/p.png"/><use xlink:href="#ok"/><image href="data:image/png;base64,AAAA"/>
    <rect style="fill:url(https://x.example/f)" fill="url(#grad)"/></svg>`;
  const out: string = sanitizeSvg(evil);
  expect(out).toContain('<svg');
  for (const bad of ['<script', 'foreignObject', 'onload', 'tracker.example', 'x.example', 'ENTITY']) {
    expect(out).not.toContain(bad);
  }
  expect(out).toContain('xlink:href="#ok"');
  expect(out).toContain('data:image/png;base64,AAAA');
  expect(out).toContain('url(#grad)');
  expect(sanitizeSvg('<html>kein svg</html>')).toBeNull();
});

test('Untertitel neben dem Video werden gefunden', () => {
  const { subtitleTracks } = require('../src/Viewer');
  const tracks = subtitleTracks('/sd/Filme/Mein Film (2024).mkv', [
    'Mein Film (2024).mkv', 'Mein Film (2024).srt', 'Mein Film (2024).de.srt', 'Mein Film (2024).en.vtt', 'Anderer Film.srt', 'Mein Film (2024).txt',
  ]);
  expect(tracks.map((x: any) => x.uri)).toEqual([
    'file:///sd/Filme/Mein Film (2024).srt', 'file:///sd/Filme/Mein Film (2024).de.srt', 'file:///sd/Filme/Mein Film (2024).en.vtt',
  ]);
  expect(tracks[1]).toMatchObject({ title: 'DE', language: 'de', type: 'application/x-subrip' });
  expect(tracks[2].type).toBe('text/vtt');
});

test('Texterkennung: Ergebnis zeigen und kopieren, sonst Hinweis', async () => {
  const { useOcr } = require('../src/Ocr');
  const messages: string[] = [];
  let api: any;
  function Probe() {
    api = useOcr((m: string) => messages.push(m));
    return api.sheet;
  }
  let r!: ReactTestRenderer.ReactTestRenderer;
  await act(async () => { r = mount(<Probe />); });
  await act(async () => { api.run({ image: '/storage/emulated/0/DCIM/rechnung.jpg' }); });
  await flush();
  expect(NativeModules.SnapCore.ocrImage).toHaveBeenCalledWith('/storage/emulated/0/DCIM/rechnung.jpg');
  expect(JSON.stringify(r.toJSON())).toContain('Gesamt: 19,99');
  await act(async () => { await pressable(r, 'Kopieren').props.onPress(); });
  expect(NativeModules.SnapCore.copyText).toHaveBeenCalledWith('Rechnung Nr. 42\nGesamt: 19,99 €');
  // PDF-Seite ohne Ergebnis → kein Fenster, sondern Hinweis
  await act(async () => { r.unmount(); r = mount(<Probe />); });
  await act(async () => { api.run({ pdf: '/storage/emulated/0/scan.pdf', page: 2 }); });
  await flush();
  expect(NativeModules.SnapCore.ocrPdfPage).toHaveBeenCalledWith('/storage/emulated/0/scan.pdf', 2);
  expect(r.toJSON()).toBeNull();
  expect(messages).toContain('Texterkennung ist hier nicht möglich.');
});
