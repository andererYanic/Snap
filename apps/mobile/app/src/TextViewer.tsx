// Eingebaute Anzeige für Text- und Markdown-Dateien. Text wird nur gelesen (höchstens 2 MB),
// Markdown nativ dargestellt – ohne HTML oder WebView. Links öffnen nur nach Antippen und nur
// http(s).
import { useEffect, useMemo, useState } from 'react';
import { ActivityIndicator, FlatList, Image, Linking, Modal, Pressable, ScrollView, StatusBar, StyleSheet, View } from 'react-native';
import { Text } from './text';
import * as core from './core';
import { Icon } from './Icon';
import { t } from './i18n';
import { fromDocx, parseMarkdown, type Block, type Inline } from './markdown';
import type { Theme } from './theme';
import { themed } from './ui';

const MD = /\.(md|markdown)$/i;
/** Formate, die der Kern als Leseansicht liefert. */
const DOCS = /\.(docx|odt|ods|odp|epub|pptx|xlsx)$/i;
const TABLE = /\.(csv|tsv)$/i;
const CARD = /\.(vcf|vcard|ics|ical)$/i;
/** Text, Code und Konfiguration: als reiner Text angezeigt (höchstens 2 MB). */
const PLAIN = /\.(txt|text|log|json|jsonc|json5|xml|ini|conf|cfg|yaml|yml|toml|properties|env|gradle|kts?|java|js|mjs|cjs|jsx|ts|tsx|py|rb|rs|go|c|h|cc|cpp|hpp|cs|swift|m|mm|php|pl|lua|r|dart|scala|sh|bash|zsh|fish|ps1|bat|cmd|sql|html?|css|scss|sass|less|svg|vue|svelte|srt|vtt|ass|sub|tex|bib|rst|adoc|org|nfo|diff|patch|gitignore|gitattributes|editorconfig|dockerfile|makefile|cmake|lock|plist|desktop|service|reg|csv|tsv)$/i;
const BARE = /^(readme|license|licence|copying|changelog|authors|makefile|dockerfile|notice|todo)$/i;
/** Dateien, die die Textanzeige öffnet. */
export const isText = (name: string) => MD.test(name) || DOCS.test(name) || PLAIN.test(name) || CARD.test(name) || BARE.test(name);
/** Höchstens so viele Tabellenzeilen werden gezeichnet (der Rest steht als Hinweis darunter). */
const MAX_SHOWN_ROWS = 400;

/** CSV/TSV → Tabelle (Anführungszeichen, eingebettete Trennzeichen und Zeilenumbrüche). */
export function parseCsv(text: string, sep: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [];
  let cell = '';
  let quoted = false;
  for (let i = 0; i < text.length && rows.length < 5000; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') {
        cell += '"';
        i++;
      } else if (c === '"') {
        quoted = false;
      } else {
        cell += c;
      }
    } else if (c === '"' && cell === '') {
      quoted = true;
    } else if (c === sep) {
      row.push(cell);
      cell = '';
    } else if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') {
        i++;
      }
      row.push(cell);
      rows.push(row);
      row = [];
      cell = '';
    } else {
      cell += c;
    }
  }
  if (cell !== '' || row.length) {
    row.push(cell);
    rows.push(row);
  }
  const width = Math.min(200, Math.max(0, ...rows.map((r) => r.length)));
  return rows.map((r) => {
    const x = r.slice(0, width).map((v) => v.slice(0, 500));
    while (x.length < width) {
      x.push('');
    }
    return x;
  });
}

/** Trennzeichen raten: Tab bei .tsv, sonst das häufigste aus ; , Tab in der ersten Zeile. */
function csvSep(name: string, text: string): string {
  if (/\.tsv$/i.test(name)) {
    return '\t';
  }
  const first = text.slice(0, text.indexOf('\n') >>> 0 || 2000);
  const counts = [';', ',', '\t'].map((c) => [c, first.split(c).length] as const);
  return counts.sort((a, b) => b[1] - a[1])[0][0];
}

/** Visitenkarten (vCard) und Termine (iCalendar) → lesbare Blöcke. */
export function parseCard(text: string): Block[] {
  // Gefaltete Zeilen (Fortsetzung mit Leerzeichen/Tab) zusammenführen
  const lines = text.replace(/\r\n?/g, '\n').replace(/\n[ \t]/g, '').split('\n');
  const out: Block[] = [];
  const unescape = (v: string) => v.replace(/\\n/gi, '\n').replace(/\\([,;\\])/g, '$1');
  const when = (v: string) => {
    const m = /^(\d{4})(\d{2})(\d{2})(?:T(\d{2})(\d{2}))?/.exec(v);
    return m ? `${m[3]}.${m[2]}.${m[1]}${m[4] ? ` ${m[4]}:${m[5]}` : ''}` : v;
  };
  const LABEL: Record<string, string> = {
    TEL: 'Telefon', EMAIL: 'E-Mail', ADR: 'Adresse', ORG: 'Organisation', TITLE: 'Position', URL: 'Web', BDAY: 'Geburtstag', NOTE: 'Notiz',
    DTSTART: 'Beginn', DTEND: 'Ende', LOCATION: 'Ort', DESCRIPTION: 'Beschreibung', ORGANIZER: 'Organisiert von',
  };
  let rows: string[][] = [];
  const flush = () => {
    if (rows.length) {
      out.push({ type: 'table', rows: [[t('Feld'), t('Wert')], ...rows] });
      rows = [];
    }
  };
  for (const line of lines) {
    const i = line.indexOf(':');
    if (i <= 0) {
      continue;
    }
    const key = line.slice(0, i).split(';')[0].toUpperCase();
    const value = unescape(line.slice(i + 1)).trim();
    if (key === 'BEGIN' && (value === 'VCARD' || value === 'VEVENT')) {
      flush();
    } else if (key === 'FN' || key === 'SUMMARY') {
      out.push({ type: 'heading', level: 2, inline: [{ text: value }] });
    } else if (LABEL[key] && value) {
      const v = key === 'ADR' ? value.split(';').filter(Boolean).join(', ') : /^DT|BDAY/.test(key) ? when(value) : value.replace(/^mailto:/i, '');
      rows.push([t(LABEL[key]), v.slice(0, 2000)]);
    }
    if (out.length > 2000) {
      break;
    }
  }
  flush();
  return out;
}

/** Bild aus einer Word-Datei: Breite des Bildschirms, Seitenverhältnis aus dem Bild. */
function DocImage({ file }: { file: string }) {
  const [ratio, setRatio] = useState(0.6);
  return (
    <Image
      source={{ uri: `file://${file}` }}
      onLoad={(e) => {
        const { width, height } = e.nativeEvent.source;
        if (width > 0) {
          setRatio(height / width);
        }
      }}
      style={{ width: '100%', aspectRatio: 1 / ratio, marginBottom: 12, borderRadius: 6 }}
      resizeMode="contain"
    />
  );
}

function Spans({ inline, s, theme }: { inline: Inline[]; s: ReturnType<typeof styles>; theme: Theme }) {
  return (
    <>
      {inline.map((x, k) => (
        <Text
          key={k}
          style={[x.bold && s.bold, x.italic && s.italic, x.code && s.code, x.link && { color: theme.accent, textDecorationLine: 'underline' }]}
          onPress={x.link && /^https?:\/\//i.test(x.link) ? () => Linking.openURL(x.link!).catch(() => {}) : undefined}
        >
          {x.text}
        </Text>
      ))}
    </>
  );
}

function BlockView({ b, s, theme }: { b: Block; s: ReturnType<typeof styles>; theme: Theme }) {
  switch (b.type) {
    case 'heading':
      return <Text style={[s.text, s[`h${Math.min(b.level, 3)}` as 'h1']]}><Spans inline={b.inline} s={s} theme={theme} /></Text>;
    case 'paragraph':
      return <Text style={[s.text, s.para]} selectable><Spans inline={b.inline} s={s} theme={theme} /></Text>;
    case 'quote':
      return (
        <View style={[s.quote, { borderLeftColor: theme.accent }]}>
          <Text style={[s.text, s.quoteText]}><Spans inline={b.inline} s={s} theme={theme} /></Text>
        </View>
      );
    case 'code':
      return (
        <View style={s.codeBox}>
          <Text style={[s.text, s.codeText]} selectable>{b.text}</Text>
        </View>
      );
    case 'rule':
      return <View style={[s.rule, { backgroundColor: theme.stroke }]} />;
    case 'image':
      return <DocImage file={b.file} />;
    case 'table':
      return (
        <ScrollView horizontal style={s.tableWrap} showsHorizontalScrollIndicator={false}>
          <View style={[s.table, { borderColor: theme.stroke }]}>
            {b.rows.slice(0, MAX_SHOWN_ROWS).map((row, i) => (
              <View key={i} style={[s.tr, i === 0 && { backgroundColor: theme.accentSoft }, { borderColor: theme.stroke }]}>
                {row.map((cell, k) => (
                  <Text key={k} style={[s.text, s.td, i === 0 && s.bold, { borderColor: theme.stroke }]} selectable>{cell}</Text>
                ))}
              </View>
            ))}
            {b.rows.length > MAX_SHOWN_ROWS && (
              <Text style={[s.text, s.td]}>{t('… und {n} weitere Zeilen', { n: b.rows.length - MAX_SHOWN_ROWS })}</Text>
            )}
          </View>
        </ScrollView>
      );
    case 'list':
      return (
        <View style={s.list}>
          {b.items.map((it, k) => (
            <View key={k} style={s.li}>
              {it.checked !== undefined ? (
                <View style={[s.box, it.checked && s.boxOn]}>
                  {it.checked && <Icon name="check" size={14} color={theme.onAccent as string} strokeWidth={2.6} />}
                </View>
              ) : (
                <Text style={[s.text, s.bullet]}>{b.ordered ? `${k + 1}.` : '•'}</Text>
              )}
              <Text style={[s.text, s.liText]}><Spans inline={it.inline} s={s} theme={theme} /></Text>
            </View>
          ))}
        </View>
      );
  }
}

export default function TextViewer({ path, name, theme, onClose }: { path: string; name: string; theme: Theme; onClose: () => void }) {
  const [doc, setDoc] = useState<{ text: string; truncated: boolean } | null | 'error'>(null);
  const [raw, setRaw] = useState(false);
  const [docx, setDocx] = useState<Block[] | null>(null);
  const s = styles(theme);
  const md = MD.test(name);
  const isDocx = DOCS.test(name);

  useEffect(() => {
    if (isDocx) {
      // Office/E-Book: der Kern liest Text, Überschriften, Listen, Tabellen und Bilder (Leseansicht)
      core
        .docxImageDir()
        .then((dir) => core.request<{ blocks: any[]; truncated: boolean }>({ ReadDocument: { path, image_dir: dir } }))
        .then((d) => {
          setDocx(fromDocx(d.blocks));
          setDoc({ text: '', truncated: d.truncated });
        })
        .catch(() => setDoc('error'));
    } else {
      core.readText(path).then(setDoc).catch(() => setDoc('error'));
    }
  }, [path, isDocx]);

  const blocks = useMemo((): Block[] | null => {
    if (docx) {
      return docx;
    }
    if (!doc || doc === 'error') {
      return null;
    }
    if (md) {
      return parseMarkdown(doc.text);
    }
    if (TABLE.test(name)) {
      const rows = parseCsv(doc.text, csvSep(name, doc.text));
      return rows.length ? [{ type: 'table', rows }] : null;
    }
    if (CARD.test(name)) {
      const b = parseCard(doc.text);
      return b.length ? b : null;
    }
    return null;
  }, [doc, md, docx, name]);
  // Reiner Text: in Abschnitte teilen, damit lange Dateien flüssig scrollen (virtualisierte Liste)
  const chunks = useMemo(() => {
    if (!doc || doc === 'error') {
      return [];
    }
    const lines = doc.text.split('\n');
    const out: string[] = [];
    for (let i = 0; i < lines.length; i += 60) {
      out.push(lines.slice(i, i + 60).join('\n'));
    }
    return out;
  }, [doc]);

  return (
    <Modal visible animationType="fade" onRequestClose={onClose} statusBarTranslucent>
      <View style={s.root}>
        <StatusBar barStyle={theme.dark ? 'light-content' : 'dark-content'} />
        <View style={s.top}>
          <Pressable onPress={onClose} style={s.btn} accessibilityLabel={t('Schließen')}>
            <Icon name="back" color={theme.text as string} />
          </Pressable>
          <Text style={s.title} numberOfLines={1}>{name}</Text>
          {isDocx && <Text style={s.badge}>{t('Leseansicht')}</Text>}
          {(md || TABLE.test(name) || CARD.test(name)) && (
            <Pressable onPress={() => setRaw((v) => !v)} style={s.btn} accessibilityLabel={raw ? t('Formatiert anzeigen') : t('Quelltext anzeigen')}>
              <Text style={s.small}>{raw ? t('Formatiert') : t('Quelltext')}</Text>
            </Pressable>
          )}
          <Pressable onPress={() => core.openFile(path).catch(() => {})} style={s.btn} accessibilityLabel={t('Öffnen mit …')}>
            <Icon name="more" color={theme.text as string} strokeWidth={2.4} />
          </Pressable>
        </View>
        {doc === null ? (
          <ActivityIndicator style={s.fill} color={theme.accent as string} />
        ) : doc === 'error' ? (
          <Text style={[s.text, s.error]}>{t('Diese Datei lässt sich nicht anzeigen.')}</Text>
        ) : blocks && !raw ? (
          <FlatList
            data={blocks}
            keyExtractor={(_, i) => String(i)}
            contentContainerStyle={s.body}
            initialNumToRender={20}
            renderItem={({ item }) => <BlockView b={item} s={s} theme={theme} />}
            ListFooterComponent={
              doc.truncated ? <Text style={s.note}>{t('Nur der Anfang der Datei wird angezeigt (2 MB).')}</Text> : isDocx ? (
                <Pressable onPress={() => core.openFile(path).catch(() => {})} style={s.officeBtn}>
                  <Text style={s.small}>{t('Original mit Office-App öffnen')}</Text>
                </Pressable>
              ) : undefined
            }
          />
        ) : (
          <FlatList
            data={chunks}
            keyExtractor={(_, i) => String(i)}
            contentContainerStyle={s.body}
            initialNumToRender={4}
            renderItem={({ item }) => <Text style={[s.text, s.mono]} selectable>{item}</Text>}
            ListFooterComponent={doc.truncated ? <Text style={s.note}>{t('Nur der Anfang der Datei wird angezeigt (2 MB).')}</Text> : undefined}
          />
        )}
      </View>
    </Modal>
  );
}

const styles = themed((th: Theme) =>
  StyleSheet.create({
    root: { flex: 1, backgroundColor: th.surface },
    fill: { flex: 1 },
    top: {
      flexDirection: 'row', alignItems: 'center', paddingTop: 36, paddingHorizontal: 4, paddingBottom: 4, minHeight: 92,
      borderBottomWidth: StyleSheet.hairlineWidth, borderBottomColor: th.line,
    },
    btn: { minWidth: 48, height: 48, borderRadius: 24, justifyContent: 'center', alignItems: 'center', paddingHorizontal: 8 },
    btnText: { color: th.text, fontSize: 22 },
    small: { color: th.accent, fontSize: 14, fontWeight: '700' },
    title: { flex: 1, color: th.text, fontSize: 17, lineHeight: 24, fontWeight: '700', marginHorizontal: 4 },
    body: { paddingHorizontal: 22, paddingTop: 20, paddingBottom: 64 },
    text: { color: th.text, fontSize: 16, lineHeight: 26 },
    mono: { fontFamily: 'monospace', fontSize: 14, lineHeight: 20 },
    para: { marginBottom: 12 },
    h1: { fontSize: 26, lineHeight: 32, fontWeight: '700', marginTop: 8, marginBottom: 12 },
    h2: { fontSize: 21, lineHeight: 28, fontWeight: '700', marginTop: 8, marginBottom: 10 },
    h3: { fontSize: 18, lineHeight: 25, fontWeight: '700', marginTop: 6, marginBottom: 8 },
    bold: { fontWeight: '700' },
    italic: { fontStyle: 'italic' },
    code: { backgroundColor: th.surface2 },
    codeBox: { backgroundColor: th.surface2, borderRadius: 8, padding: 12, marginBottom: 14 },
    codeText: { fontSize: 14, lineHeight: 20 },
    quote: { borderLeftWidth: 3, paddingLeft: 12, marginBottom: 12 },
    quoteText: { color: th.text2, fontStyle: 'italic' },
    rule: { height: StyleSheet.hairlineWidth * 2, marginVertical: 14 },
    list: { marginBottom: 12 },
    li: { flexDirection: 'row', marginBottom: 4 },
    bullet: { width: 24, color: th.text3 },
    box: { width: 18, height: 18, borderRadius: 4, borderWidth: 1.5, borderColor: th.text3, marginTop: 4, marginRight: 8, justifyContent: 'center', alignItems: 'center' },
    boxOn: { backgroundColor: th.accent, borderColor: th.accent },
    liText: { flex: 1 },
    note: { color: th.text2, fontSize: 13, marginTop: 16, textAlign: 'center' },
    error: { padding: 24, textAlign: 'center', color: th.text2 },
    tableWrap: { marginBottom: 12 },
    table: { borderWidth: StyleSheet.hairlineWidth, borderRadius: 6, overflow: 'hidden' },
    tr: { flexDirection: 'row', borderBottomWidth: StyleSheet.hairlineWidth },
    td: { minWidth: 90, maxWidth: 260, padding: 8, fontSize: 14, lineHeight: 20, borderRightWidth: StyleSheet.hairlineWidth },
    badge: { color: th.text3, fontSize: 12, letterSpacing: 1, textTransform: 'uppercase', marginRight: 6 },
    officeBtn: { alignSelf: 'center', marginTop: 20, minHeight: 44, justifyContent: 'center', paddingHorizontal: 20, borderRadius: 22, backgroundColor: th.accentSoft },
  }));
