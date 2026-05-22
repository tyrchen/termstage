export type PresentationThemeName = 'high-contrast' | 'light';

export interface PresentationSettings {
  fontSize: number;
  theme: PresentationThemeName;
}

export interface ThemePalette {
  background: string;
  foreground: string;
  cursor: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

const DEFAULT_FONT_SIZE = 24;
const MIN_FONT_SIZE = 12;
const MAX_FONT_SIZE = 96;

const THEMES: Record<PresentationThemeName, ThemePalette> = {
  'high-contrast': {
    background: '#0c2f38',
    foreground: '#d5dee1',
    cursor: '#f4d35e',
    selectionBackground: '#225866',
    black: '#0b2028',
    red: '#e76f51',
    green: '#65d46e',
    yellow: '#f4d35e',
    blue: '#58c4dd',
    magenta: '#d65a9f',
    cyan: '#5fc9bd',
    white: '#d5dee1',
    brightBlack: '#6f858b',
    brightRed: '#ff8a6b',
    brightGreen: '#8de996',
    brightYellow: '#ffe17a',
    brightBlue: '#79d7ef',
    brightMagenta: '#ea74b7',
    brightCyan: '#83ded4',
    brightWhite: '#f7fbfc'
  },
  light: {
    background: '#fbfcfe',
    foreground: '#121820',
    cursor: '#0057b8',
    selectionBackground: '#b9d7ff',
    black: '#15191f',
    red: '#b42318',
    green: '#157f3b',
    yellow: '#8a6200',
    blue: '#0057b8',
    magenta: '#8c3eb5',
    cyan: '#007b83',
    white: '#e5e7eb',
    brightBlack: '#636b74',
    brightRed: '#d92d20',
    brightGreen: '#229954',
    brightYellow: '#b7791f',
    brightBlue: '#1570ef',
    brightMagenta: '#a855f7',
    brightCyan: '#0e9384',
    brightWhite: '#ffffff'
  }
};

export function readPresentationSettings(): PresentationSettings {
  const params = new URLSearchParams(window.location.search);
  const fontSize = parseFontSize(params.get('fontSize'));
  const theme = parseTheme(params.get('theme'));
  const palette = themePalette(theme);
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.setProperty('--terminal-font-size', `${fontSize}px`);
  document.documentElement.style.setProperty('--terminal-background', palette.background);
  document.documentElement.style.setProperty('--terminal-foreground', palette.foreground);
  document.documentElement.style.setProperty('--terminal-cursor', palette.cursor);
  document.documentElement.style.setProperty(
    '--terminal-selection-background',
    palette.selectionBackground
  );
  return { fontSize, theme };
}

export function themePalette(name: PresentationThemeName): ThemePalette {
  return THEMES[name];
}

function parseFontSize(value: string | null): number {
  if (value === null) {
    return DEFAULT_FONT_SIZE;
  }
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed)) {
    return DEFAULT_FONT_SIZE;
  }
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, parsed));
}

function parseTheme(value: string | null): PresentationThemeName {
  if (value === 'light') {
    return 'light';
  }
  return 'high-contrast';
}
