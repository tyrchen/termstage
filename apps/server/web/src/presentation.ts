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
    background: '#080b0f',
    foreground: '#f4f7fb',
    cursor: '#f8d84a',
    selectionBackground: '#315f7a',
    black: '#11151a',
    red: '#ff5f57',
    green: '#38d878',
    yellow: '#f8d84a',
    blue: '#54a6ff',
    magenta: '#d186ff',
    cyan: '#43d7d6',
    white: '#d9e2ec',
    brightBlack: '#6a7380',
    brightRed: '#ff8a80',
    brightGreen: '#72f0a0',
    brightYellow: '#ffe978',
    brightBlue: '#8ac7ff',
    brightMagenta: '#e5b4ff',
    brightCyan: '#78f0ef',
    brightWhite: '#ffffff'
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
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.setProperty('--terminal-font-size', `${fontSize}px`);
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
