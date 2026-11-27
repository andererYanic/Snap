// Symbolsatz der App: eigene Linien-Symbole im 24er-Raster (Strich 1,6, runde Enden).
// Einzige Quelle für Symbole – keine Emoji oder Unicode-Zeichen (docs/DESIGN.md).
import { memo } from 'react';
import Svg, { Circle, Path, Rect } from 'react-native-svg';

const P: Record<string, string> = {
  // Navigation
  back: 'M15 5l-7 7 7 7',
  forward: 'M9 5l7 7-7 7',
  chevron: 'M10 7l5 5-5 5',
  up: 'M6 15l6-6 6 6',
  close: 'M6 6l12 12M18 6L6 18',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  plus: 'M12 5v14M5 12h14',
  minus: 'M5 12h14',
  search: 'M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13zM20 20l-4.8-4.8',
  // Ablage
  folder: 'M3.5 7.5A1.5 1.5 0 0 1 5 6h4l2 2h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5z',
  file: 'M7 3.5h7l4 4V19a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 6 19V5a1.5 1.5 0 0 1 1-1.5zM14 3.5V8h4',
  photos: 'M4.5 5.5h15v13h-15zM4.5 15l4-4 3.5 3.5 2.5-2.5 5 4.5M15.5 9.5h.01',
  settings: 'M4 7h9M17 7h3M4 17h3M11 17h9M15 5v4M9 15v4',
  devices: 'M3.5 6.5h13v8h-13zM2 17.5h16M18.5 9.5h3v9h-3z',
  phone: 'M8 3h8a1.5 1.5 0 0 1 1.5 1.5v15A1.5 1.5 0 0 1 16 21H8a1.5 1.5 0 0 1-1.5-1.5v-15A1.5 1.5 0 0 1 8 3zM11 18h2',
  laptop: 'M5 6h14v9H5zM3 18h18',
  tablet: 'M6 3h12a1.5 1.5 0 0 1 1.5 1.5v15A1.5 1.5 0 0 1 18 21H6a1.5 1.5 0 0 1-1.5-1.5v-15A1.5 1.5 0 0 1 6 3zM11 18h2',
  drive: 'M4 13.5l2.5-7h11l2.5 7v4H4zM4 13.5h16M16.5 15.8h.01',
  download: 'M12 4v11M7.5 10.5L12 15l4.5-4.5M5 19.5h14',
  camera: 'M4 8h3l1.5-2h7L17 8h3v11H4zM12 16.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7z',
  music: 'M9 17.5V6l10-2v11.5M9 17.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0zM19 15.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0z',
  video: 'M4 6.5h11v11H4zM15 10l5-3v10l-5-3',
  document: 'M7 3.5h7l4 4V19a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 6 19V5a1.5 1.5 0 0 1 1-1.5zM9 12h6M9 15.5h6M9 8.5h2',
  trash: 'M4.5 7h15M9.5 7V4.5h5V7M6.5 7l1 13h9l1-13M10 11v5.5M14 11v5.5',
  restore: 'M4.5 12a7.5 7.5 0 1 0 2.2-5.3M4.5 4v4h4',
  // Aktionen
  more: 'M12 5.5h.01M12 12h.01M12 18.5h.01',
  grid: 'M4.5 4.5h6v6h-6zM13.5 4.5h6v6h-6zM4.5 13.5h6v6h-6zM13.5 13.5h6v6h-6z',
  list: 'M8.5 6.5h11M8.5 12h11M8.5 17.5h11M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01',
  share: 'M12 15V4M8 7.5L12 3.5l4 4M6 11v8.5h12V11',
  copy: 'M8.5 8.5h11v11h-11zM15.5 8.5v-4h-11v11h4',
  move: 'M3.5 7.5A1.5 1.5 0 0 1 5 6h4l2 2h8a1.5 1.5 0 0 1 1.5 1.5v8A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5zM9 13.5h6M13 11.5l2 2-2 2',
  rename: 'M4 20h4l10.5-10.5a2.1 2.1 0 0 0-3-3L5 17v3zM13.5 8.5l3 3',
  info: 'M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM12 11v5.5M12 7.8h.01',
  print: 'M7 9V3.5h10V9M7 17H4.5v-8h15v8H17M7 14h10v6.5H7z',
  archive: 'M4 4.5h16v4H4zM5.5 8.5V19.5h13V8.5M10 12h4',
  extract: 'M4 4.5h16v4H4zM5.5 8.5V19.5h13V8.5M12 11v6M9.5 14.5L12 17l2.5-2.5',
  open: 'M14 4.5h5.5V10M19.5 4.5L11 13M17 14v5.5H4.5V7H10',
  edit: 'M4 20h4l10.5-10.5a2.1 2.1 0 0 0-3-3L5 17v3z',
  font: 'M5 19L10 5h1l5 14M7 14h7M17 19v-6.5M17 12.5a2.5 2.5 0 1 1 4 2M21 19v-4.5',
  globe: 'M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM3.5 12h17M12 3.5c2.3 2.4 3.5 5.2 3.5 8.5s-1.2 6.1-3.5 8.5c-2.3-2.4-3.5-5.2-3.5-8.5s1.2-6.1 3.5-8.5z',
  palette: 'M12 20.5a8.5 8.5 0 1 1 8.5-8.5c0 2-1.6 3-3.3 3H15a1.8 1.8 0 0 0-1.2 3.1c.6.6.3 2.4-1.8 2.4zM7.5 12h.01M9.5 8h.01M14.5 8h.01',
  key: 'M14.5 13a4.5 4.5 0 1 0-4.3-3.2L4 16v4h4v-2h2v-2h2l1.2-1.2c.4.1.9.2 1.3.2zM16 8h.01',
  link: 'M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1',
  // Bearbeiten
  crop: 'M6.5 3v14.5H21M3 6.5h14.5V21',
  rotate: 'M4.5 12a7.5 7.5 0 1 0 2.2-5.3M4.5 4v4h4',
  adjust: 'M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.3 5.3l1.4 1.4M17.3 17.3l1.4 1.4M5.3 18.7l1.4-1.4M17.3 6.7l1.4-1.4',
  filter: 'M9 15a5 5 0 1 0 0-10 5 5 0 0 0 0 10zM15 19a5 5 0 1 0 0-10 5 5 0 0 0 0 10z',
  soundOn: 'M4.5 9.5h3l4.5-4v13l-4.5-4h-3zM15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11',
  soundOff: 'M4.5 9.5h3l4.5-4v13l-4.5-4h-3zM16 9.5l5 5M21 9.5l-5 5',
  play: 'M8 5.5v13l10.5-6.5z',
  pause: 'M8 5.5v13M16 5.5v13',
  undo: 'M9 14.5L4.5 10 9 5.5M4.5 10H14a5.5 5.5 0 0 1 0 11h-3',
  hand: 'M8 13V6.5a1.5 1.5 0 0 1 3 0V11M11 10V4.5a1.5 1.5 0 0 1 3 0V11M14 10.5V6a1.5 1.5 0 0 1 3 0v7.5c0 3.8-2.4 6.5-6 6.5-2.3 0-3.6-1-5-3l-2.3-3.6a1.4 1.4 0 0 1 2.2-1.7L8 13',
  highlight: 'M14.5 4.5l5 5-8 8H6.5v-5zM4 20.5h9',
  sign: 'M3.5 17c2-3 3.6-4.5 4.8-4.5 2.2 0-1 5 1.4 5 1.3 0 2.2-2.5 3.6-2.5 1 0 .9 1.7 2 1.7.9 0 1.5-.8 2.2-1.6M3.5 20.5h17',
  shield: 'M12 3.5l7 2.5v5.5c0 4.4-3 7.7-7 9-4-1.3-7-4.6-7-9V6z',
  shieldCheck: 'M12 3.5l7 2.5v5.5c0 4.4-3 7.7-7 9-4-1.3-7-4.6-7-9V6zM8.8 12l2.3 2.3 4.2-4.6',
  warning: 'M12 4l9 15.5H3zM12 10v4.5M12 17h.01',
  sun: 'M12 16a4 4 0 1 0 0-8 4 4 0 0 0 0 8z',
  // Snap Drop
  nearby: 'M12 13.6a1.6 1.6 0 1 0 0-3.2 1.6 1.6 0 0 0 0 3.2zM8.4 15.6a5 5 0 0 1 0-7.2M15.6 8.4a5 5 0 0 1 0 7.2M5.6 18.4a9 9 0 0 1 0-12.8M18.4 5.6a9 9 0 0 1 0 12.8',
  wifi: 'M3.5 9.5a12 12 0 0117 0M6.5 12.5a8 8 0 0111 0M9.5 15.5a4 4 0 015 0M12 19h.01',
  user: 'M12 12a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7zM5 19.5c.8-3.2 3.6-5 7-5s6.2 1.8 7 5',
  block: 'M12 20.5a8.5 8.5 0 1 0 0-17 8.5 8.5 0 0 0 0 17zM6 6l12 12',
};

export type IconName = keyof typeof P | 'dot';

/** Linien-Symbol in `color`, Kantenlänge `size`. `fill` füllt geschlossene Formen (z. B. Ordner). */
export const Icon = memo(function Icon({ name, size = 24, color, fill, strokeWidth = 1.6 }: {
  name: IconName; size?: number; color: string; fill?: string; strokeWidth?: number;
}) {
  if (name === 'dot') {
    return (
      <Svg width={size} height={size} viewBox="0 0 24 24">
        <Circle cx={12} cy={12} r={4} fill={color} />
      </Svg>
    );
  }
  return (
    <Svg width={size} height={size} viewBox="0 0 24 24">
      <Path d={P[name]} stroke={color} strokeWidth={strokeWidth} strokeLinecap="round" strokeLinejoin="round" fill={fill ?? 'none'} />
    </Svg>
  );
});

/** Gefüllter Ordner (Ablage-Symbol in Listen und Kacheln). */
export const FolderShape = memo(function FolderShape({ size, color, tab }: { size: number; color: string; tab: string }) {
  return (
    <Svg width={size} height={size} viewBox="0 0 40 40">
      <Path d="M5 11.5A2.5 2.5 0 0 1 7.5 9h8.2a2 2 0 0 1 1.5.7L19.5 12H32.5A2.5 2.5 0 0 1 35 14.5V16H5z" fill={tab} />
      <Rect x={5} y={14} width={30} height={18} rx={2.5} fill={color} />
    </Svg>
  );
});
