// Massen-Umbenennen: neue Namen planen (rein, ohne Dateizugriff – leicht zu testen).
// „Nummerieren“: Basisname + laufende Nummer (Breite passend zur Anzahl), Endung bleibt.
// „Ersetzen“: Text im Namen ersetzen (ohne Groß-/Kleinschreibung), Endung bleibt unangetastet.

export type RenameMode = { kind: 'number'; base: string; start: number } | { kind: 'replace'; find: string; with: string };

const splitExt = (name: string): [string, string] => {
  const i = name.lastIndexOf('.');
  return i > 0 ? [name.slice(0, i), name.slice(i)] : [name, ''];
};

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');

/** Neue Namen in derselben Reihenfolge wie `names`. */
export function planRename(names: string[], mode: RenameMode): string[] {
  if (mode.kind === 'number') {
    const width = String(mode.start + names.length - 1).length;
    const base = mode.base.trim();
    return names.map((n, i) => {
      const [, ext] = splitExt(n);
      const num = String(mode.start + i).padStart(Math.max(width, 2), '0');
      return `${base ? `${base} ` : ''}${num}${ext}`;
    });
  }
  if (!mode.find) {
    return [...names];
  }
  const re = new RegExp(escapeRe(mode.find), 'gi');
  return names.map((n) => {
    const [stem, ext] = splitExt(n);
    return stem.replace(re, mode.with) + ext;
  });
}

/** Probleme: leere/ungültige Namen oder doppelte Ergebnisse (Groß-/Kleinschreibung zählt). */
export function renameProblem(oldNames: string[], newNames: string[]): string | null {
  const seen = new Set<string>();
  for (const n of newNames) {
    if (!n.trim() || n === '.' || n === '..' || n.includes('/') || n.includes('\0') || n.length > 255) {
      return 'invalid';
    }
    if (seen.has(n)) {
      return 'duplicate';
    }
    seen.add(n);
  }
  return newNames.every((n, i) => n === oldNames[i]) ? 'unchanged' : null;
}
