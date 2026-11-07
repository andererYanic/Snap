// Eigene, schlichte Symbole (SVG) im Fluent-Stil – keine fremden Grafiken.
import type { JSX } from "react";

type P = { size?: number };
const s = (size = 16) => ({ width: size, height: size, viewBox: "0 0 16 16", fill: "none", stroke: "currentColor", strokeWidth: 1.25, strokeLinecap: "round" as const, strokeLinejoin: "round" as const });

export const Icon: Record<string, (p: P) => JSX.Element> = {
  back: ({ size }) => <svg {...s(size)}><path d="M13 8H3M7 4 3 8l4 4" /></svg>,
  forward: ({ size }) => <svg {...s(size)}><path d="M3 8h10M9 4l4 4-4 4" /></svg>,
  up: ({ size }) => <svg {...s(size)}><path d="M8 13V3M4 7l4-4 4 4" /></svg>,
  refresh: ({ size }) => <svg {...s(size)}><path d="M13 8a5 5 0 1 1-1.5-3.5M13 2.5v3h-3" /></svg>,
  search: ({ size }) => <svg {...s(size)}><circle cx="7" cy="7" r="4.5" /><path d="m10.5 10.5 3 3" /></svg>,
  plus: ({ size }) => <svg {...s(size)}><path d="M8 3v10M3 8h10" /></svg>,
  close: ({ size }) => <svg {...s(size)}><path d="m4 4 8 8M12 4l-8 8" /></svg>,
  min: ({ size }) => <svg {...s(size)}><path d="M3.5 8h9" /></svg>,
  max: ({ size }) => <svg {...s(size)}><rect x="3.5" y="3.5" width="9" height="9" rx="1.5" /></svg>,
  cut: ({ size }) => <svg {...s(size)}><circle cx="4.5" cy="11.5" r="2" /><circle cx="11.5" cy="11.5" r="2" /><path d="M6 10 12 2.5M10 10 4 2.5" /></svg>,
  copy: ({ size }) => <svg {...s(size)}><rect x="5.5" y="5.5" width="8" height="8" rx="1.5" /><path d="M10.5 5.5V4a1.5 1.5 0 0 0-1.5-1.5H4A1.5 1.5 0 0 0 2.5 4v5A1.5 1.5 0 0 0 4 10.5h1.5" /></svg>,
  paste: ({ size }) => <svg {...s(size)}><rect x="3" y="3" width="10" height="11" rx="1.5" /><path d="M6 3V2h4v1M6 7h4M6 10h3" /></svg>,
  rename: ({ size }) => <svg {...s(size)}><path d="M2.5 13.5h3l7-7-3-3-7 7v3ZM8.5 4.5l3 3" /></svg>,
  trash: ({ size }) => <svg {...s(size)}><path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5a1 1 0 0 0 1 .9h3.8a1 1 0 0 0 1-.9l.6-8.5" /></svg>,
  undo: ({ size }) => <svg {...s(size)}><path d="M5 3 2.5 5.5 5 8M2.5 5.5H10a3.5 3.5 0 0 1 0 7H7" /></svg>,
  newfolder: ({ size }) => <svg {...s(size)}><path d="M2 5V4a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5ZM8 7v4M6 9h4" /></svg>,
  list: ({ size }) => <svg {...s(size)}><path d="M5.5 4h8M5.5 8h8M5.5 12h8M2.5 4h.5M2.5 8h.5M2.5 12h.5" /></svg>,
  gridview: ({ size }) => <svg {...s(size)}><rect x="2.5" y="2.5" width="4.5" height="4.5" rx="1" /><rect x="9" y="2.5" width="4.5" height="4.5" rx="1" /><rect x="2.5" y="9" width="4.5" height="4.5" rx="1" /><rect x="9" y="9" width="4.5" height="4.5" rx="1" /></svg>,
  eye: ({ size }) => <svg {...s(size)}><path d="M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8Z" /><circle cx="8" cy="8" r="2" /></svg>,
  open: ({ size }) => <svg {...s(size)}><path d="M9 2.5h4.5V7M13.5 2.5 7 9M11.5 9.5v3a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1h3" /></svg>,
  home: ({ size }) => <svg {...s(size)}><path d="M2.5 7 8 2.5 13.5 7v6a.5.5 0 0 1-.5.5H10V10H6v3.5H3a.5.5 0 0 1-.5-.5V7Z" /></svg>,
  desktop: ({ size }) => <svg {...s(size)}><rect x="2" y="3" width="12" height="8" rx="1" /><path d="M6 13.5h4M8 11v2.5" /></svg>,
  documents: ({ size }) => <svg {...s(size)}><path d="M4 1.5h5l3.5 3.5v9a.5.5 0 0 1-.5.5H4a.5.5 0 0 1-.5-.5V2a.5.5 0 0 1 .5-.5ZM9 1.5V5h3.5M5.5 8.5h5M5.5 11h5" /></svg>,
  downloads: ({ size }) => <svg {...s(size)}><path d="M8 2v8M4.5 6.5 8 10l3.5-3.5M3 13.5h10" /></svg>,
  pictures: ({ size }) => <svg {...s(size)}><rect x="2" y="3" width="12" height="10" rx="1.5" /><circle cx="6" cy="6.5" r="1.2" /><path d="m2.5 12 3.5-3.5 2.5 2.5 2-2 3 3" /></svg>,
  music: ({ size }) => <svg {...s(size)}><circle cx="5" cy="12" r="2" /><circle cx="12" cy="10.5" r="2" /><path d="M7 12V3.5l7-1.5v8.5" /></svg>,
  videos: ({ size }) => <svg {...s(size)}><rect x="1.5" y="3.5" width="9.5" height="9" rx="1.5" /><path d="m11 7 3.5-2v6L11 9" /></svg>,
  photos: ({ size }) => <svg {...s(size)}><rect x="2" y="2" width="5" height="5" rx="1" /><rect x="9" y="2" width="5" height="5" rx="1" /><rect x="2" y="9" width="5" height="5" rx="1" /><rect x="9" y="9" width="5" height="5" rx="1" /></svg>,
  drive: ({ size }) => <svg {...s(size)}><rect x="1.5" y="5" width="13" height="6" rx="1.5" /><path d="M11.5 8h.5" /></svg>,
  bin: ({ size }) => <svg {...s(size)}><path d="M3 4.5h10M4.5 4.5l.7 9h5.6l.7-9M6.5 4.5V3h3v1.5" /></svg>,
  restore: ({ size }) => <svg {...s(size)}><path d="M3 8a5 5 0 1 0 1.5-3.5M3 2.5v3h3" /></svg>,
  devices: ({ size }) => <svg {...s(size)}><rect x="1.5" y="3" width="9" height="7" rx="1" /><path d="M4 12.5h4" /><rect x="11" y="5" width="3.5" height="8" rx="1" /></svg>,
  backup: ({ size }) => <svg {...s(size)}><path d="M4.5 12.5a3 3 0 0 1-.4-6 4 4 0 0 1 7.8 0 3 3 0 0 1-.4 6M8 8v5.5M6 10l2-2 2 2" /></svg>,
  link: ({ size }) => <svg {...s(size)}><path d="M6.5 9.5a3 3 0 0 0 4.2 0l2-2a3 3 0 0 0-4.2-4.2l-.8.8M9.5 6.5a3 3 0 0 0-4.2 0l-2 2a3 3 0 0 0 4.2 4.2l.8-.8" /></svg>,
  chart: ({ size }) => <svg {...s(size)}><path d="M3 13.5h10M4.5 11V7M8 11V3.5M11.5 11V8" /></svg>,
  settings: ({ size }) => <svg {...s(size)}><circle cx="8" cy="8" r="2" /><path d="M8 1.5v2M8 12.5v2M14.5 8h-2M3.5 8h-2M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4M12.6 12.6l-1.4-1.4M4.8 4.8 3.4 3.4" /></svg>,
  hidden: ({ size }) => <svg {...s(size)}><path d="M2 2l12 12M6.5 6.6A2 2 0 0 0 9.4 9.5M4.3 4.5C2.6 5.7 1.5 8 1.5 8s2.5 4.5 6.5 4.5c1.3 0 2.4-.4 3.3-1M7 3.6c.3 0 .7-.1 1-.1 4 0 6.5 4.5 6.5 4.5s-.5.9-1.3 1.9" /></svg>,
};

/** Farbiges Symbol je Dateiart (Ordner gelb, Bilder grün, …). */
export function FileGlyph({ name, dir, size = 20 }: { name: string; dir: boolean; size?: number }) {
  if (dir) {
    return (
      <svg width={size} height={size} viewBox="0 0 20 20">
        <path d="M2 5.5A1.5 1.5 0 0 1 3.5 4h4l1.6 1.6H16.5A1.5 1.5 0 0 1 18 7.1V15a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 2 15V5.5Z" fill="#e8b33c" />
        <path d="M2 8a1.5 1.5 0 0 1 1.5-1.5h13A1.5 1.5 0 0 1 18 8v7a1.5 1.5 0 0 1-1.5 1.5h-13A1.5 1.5 0 0 1 2 15V8Z" fill="#f7c948" />
      </svg>
    );
  }
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";
  const color =
    /^(jpe?g|png|gif|webp|heic|heif|bmp|tiff?|svg|avif|raw|dng|cr[23]|nef|arw)$/.test(ext) ? "#3fa66b" :
    /^(mp4|mkv|mov|webm|avi|m4v|3gp)$/.test(ext) ? "#c9483c" :
    /^(mp3|flac|m4a|ogg|opus|wav|aac)$/.test(ext) ? "#9a55d6" :
    /^(pdf)$/.test(ext) ? "#d64545" :
    /^(docx?|odt|rtf|txt|md)$/.test(ext) ? "#3b72d9" :
    /^(xlsx?|ods|csv)$/.test(ext) ? "#2e8b57" :
    /^(zip|tar|gz|tgz|7z|rar|xz|zst|bz2|iso)$/.test(ext) ? "#a07a3c" :
    "#8a8f9c";
  return (
    <svg width={size} height={size} viewBox="0 0 20 20">
      <path d="M5 2h6.5L16 6.5V17a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1Z" fill="#fff" stroke="#c9ccd4" />
      <path d="M11.5 2v3.5a1 1 0 0 0 1 1H16" fill="none" stroke="#c9ccd4" />
      <rect x="5.5" y="11.5" width="9" height="4.5" rx="1" fill={color} />
    </svg>
  );
}
