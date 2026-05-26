export type PresentationThemeName = 'high-contrast' | 'light';
export type TerminalFontFamilyName =
  | 'termstage'
  | 'sf-mono'
  | 'menlo'
  | 'monaco'
  | 'jetbrains'
  | 'monospace';

export interface PresentationSettings {
  fontFamily: TerminalFontFamily;
  fontSize: number;
  theme: PresentationThemeName;
}

export interface TerminalFontFamily {
  name: TerminalFontFamilyName;
  label: string;
  css: string;
}

export interface PresentationTheme {
  name: PresentationThemeName;
  label: string;
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
const DEFAULT_FONT_FAMILY: TerminalFontFamilyName = 'termstage';
export const MIN_FONT_SIZE = 12;
export const MAX_FONT_SIZE = 96;

export const FONT_FAMILIES: readonly TerminalFontFamily[] = [
  {
    name: 'termstage',
    label: 'Termstage Nerd',
    css: '"Termstage Nerd Font", monospace'
  },
  {
    name: 'sf-mono',
    label: 'SF Mono',
    css: '"SF Mono", Menlo, Monaco, monospace'
  },
  {
    name: 'menlo',
    label: 'Menlo',
    css: 'Menlo, Monaco, "Courier New", monospace'
  },
  {
    name: 'monaco',
    label: 'Monaco',
    css: 'Monaco, Menlo, "Courier New", monospace'
  },
  {
    name: 'jetbrains',
    label: 'JetBrains Mono',
    css: '"JetBrains Mono", "Termstage Nerd Font", monospace'
  },
  {
    name: 'monospace',
    label: 'System Mono',
    css: 'monospace'
  }
];

export const PRESENTATION_THEMES: readonly PresentationTheme[] = [
  {
    name: 'high-contrast',
    label: 'High Contrast'
  },
  {
    name: 'light',
    label: 'Light'
  }
];

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
  const fontFamily = parseFontFamily(params.get('fontFamily'));
  const fontSize = parseFontSize(params.get('fontSize'));
  const theme = parseTheme(params.get('theme'));
  document.documentElement.style.setProperty('--terminal-font-family', fontFamily.css);
  document.documentElement.style.setProperty('--terminal-font-size', `${fontSize}px`);
  applyThemeToDocument(theme);
  return { fontFamily, fontSize, theme };
}

export function applyThemeToDocument(theme: PresentationThemeName): void {
  const palette = themePalette(theme);
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.setProperty('--terminal-background', palette.background);
  document.documentElement.style.setProperty('--terminal-foreground', palette.foreground);
  document.documentElement.style.setProperty('--terminal-cursor', palette.cursor);
  document.documentElement.style.setProperty(
    '--terminal-selection-background',
    palette.selectionBackground
  );
}

export function themePalette(name: PresentationThemeName): ThemePalette {
  return THEMES[name];
}

export function themeByName(name: PresentationThemeName): PresentationTheme {
  return PRESENTATION_THEMES.find((theme) => theme.name === name) ?? PRESENTATION_THEMES[0];
}

export function clampFontSize(fontSize: number): number {
  return Math.min(MAX_FONT_SIZE, Math.max(MIN_FONT_SIZE, fontSize));
}

export function fontFamilyByName(name: TerminalFontFamilyName): TerminalFontFamily {
  return FONT_FAMILIES.find((family) => family.name === name) ?? FONT_FAMILIES[0];
}

function parseFontSize(value: string | null): number {
  if (value === null) {
    return DEFAULT_FONT_SIZE;
  }
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed)) {
    return DEFAULT_FONT_SIZE;
  }
  return clampFontSize(parsed);
}

function parseFontFamily(value: string | null): TerminalFontFamily {
  const family = FONT_FAMILIES.find((candidate) => candidate.name === value);
  return family ?? fontFamilyByName(DEFAULT_FONT_FAMILY);
}

function parseTheme(value: string | null): PresentationThemeName {
  if (value === 'light') {
    return 'light';
  }
  return 'high-contrast';
}
