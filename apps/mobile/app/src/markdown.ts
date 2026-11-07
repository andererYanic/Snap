// Kleiner Markdown-Leser für die Anzeige: Überschriften, Absätze, Listen, Zitate, Code,
// Trennlinien, **fett**, *kursiv*, `Code`, [Text](Link). Kein HTML, keine Bilder – fremde
// Dateien können so nichts nachladen oder ausführen.

export type Inline = { text: string; bold?: boolean; italic?: boolean; code?: boolean; link?: string };
export type Block =
  | { type: 'heading'; level: number; inline: Inline[] }
  | { type: 'paragraph'; inline: Inline[] }
  | { type: 'list'; ordered: boolean; items: { inline: Inline[]; checked?: boolean }[] }
  | { type: 'quote'; inline: Inline[] }
  | { type: 'code'; text: string }
  | { type: 'rule' };

/** Zerlegt Text in Inline-Teile (ohne Verschachtelung über Grenzen hinweg). */
export function parseInline(src: string): Inline[] {
  const out: Inline[] = [];
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*|__[^_]+__)|(\*[^*\s][^*]*\*|_[^_\s][^_]*_)|(\[[^\]]+\]\([^)\s]+\))/g;
  let last = 0;
  for (let m = re.exec(src); m; m = re.exec(src)) {
    if (m.index > last) {
      out.push({ text: src.slice(last, m.index) });
    }
    const t = m[0];
    if (m[1]) {
      out.push({ text: t.slice(1, -1), code: true });
    } else if (m[2]) {
      out.push({ text: t.slice(2, -2), bold: true });
    } else if (m[3]) {
      out.push({ text: t.slice(1, -1), italic: true });
    } else {
      const close = t.indexOf('](');
      out.push({ text: t.slice(1, close), link: t.slice(close + 2, -1) });
    }
    last = m.index + t.length;
  }
  if (last < src.length) {
    out.push({ text: src.slice(last) });
  }
  return out;
}

/** Zerlegt ein Markdown-Dokument in Blöcke. Begrenzt auf `maxBlocks` (sehr lange Dateien). */
export function parseMarkdown(src: string, maxBlocks = 5000): Block[] {
  const lines = src.replace(/\r\n?/g, '\n').split('\n');
  const blocks: Block[] = [];
  let i = 0;
  const isBlockStart = (l: string) =>
    /^\s{0,3}(#{1,6}\s|>|```|~~~|([-*+]|\d+[.)])\s|(\*\s*){3,}$|(-\s*){3,}$|(_\s*){3,}$)/.test(l);
  while (i < lines.length && blocks.length < maxBlocks) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    const fence = /^\s{0,3}(```|~~~)/.exec(line);
    if (fence) {
      const body: string[] = [];
      i++;
      while (i < lines.length && !lines[i].trimStart().startsWith(fence[1])) {
        body.push(lines[i]);
        i++;
      }
      i++;
      blocks.push({ type: 'code', text: body.join('\n') });
      continue;
    }
    const h = /^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
    if (h) {
      blocks.push({ type: 'heading', level: h[1].length, inline: parseInline(h[2]) });
      i++;
      continue;
    }
    if (/^\s{0,3}((\*\s*){3,}|(-\s*){3,}|(_\s*){3,})$/.test(line)) {
      blocks.push({ type: 'rule' });
      i++;
      continue;
    }
    if (/^\s{0,3}>/.test(line)) {
      const body: string[] = [];
      while (i < lines.length && /^\s{0,3}>/.test(lines[i])) {
        body.push(lines[i].replace(/^\s{0,3}>\s?/, ''));
        i++;
      }
      blocks.push({ type: 'quote', inline: parseInline(body.join(' ')) });
      continue;
    }
    const li = /^\s*([-*+]|\d+[.)])\s+/.exec(line);
    if (li) {
      const ordered = /\d/.test(li[1]);
      const items: { inline: Inline[]; checked?: boolean }[] = [];
      while (i < lines.length) {
        const m = /^\s*([-*+]|\d+[.)])\s+(.*)$/.exec(lines[i]);
        if (!m) {
          break;
        }
        const task = /^\[( |x|X)\]\s+(.*)$/.exec(m[2]);
        items.push(task ? { inline: parseInline(task[2]), checked: task[1] !== ' ' } : { inline: parseInline(m[2]) });
        i++;
      }
      blocks.push({ type: 'list', ordered, items });
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && lines[i].trim() && !isBlockStart(lines[i])) {
      para.push(lines[i].trim());
      i++;
    }
    if (para.length === 0) {
      // Sicherheitsnetz: Zeile, die wie ein Block aussieht, aber keiner ist
      para.push(lines[i].trim());
      i++;
    }
    blocks.push({ type: 'paragraph', inline: parseInline(para.join(' ')) });
  }
  return blocks;
}
