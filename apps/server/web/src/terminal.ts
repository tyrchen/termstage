import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import { PresentationSettings, themePalette } from './presentation';

export interface TerminalSurface {
  terminal: Terminal;
  fitAddon: FitAddon;
}

export async function createTerminalSurface(
  root: HTMLElement,
  settings: PresentationSettings
): Promise<TerminalSurface> {
  // xterm.js measures cell dimensions once at `open()` from the resolved
  // computed font. If the bundled JetBrains Mono webfont hasn't loaded
  // yet, that measurement uses the platform monospace fallback (Menlo /
  // Consolas / DejaVu) which has different metrics — every subsequent
  // glyph then renders off the cell grid and box-drawing chars look
  // broken. Wait for the document's font set first.
  if (typeof document !== 'undefined' && 'fonts' in document) {
    try {
      await document.fonts.load(`500 ${settings.fontSize}px "JetBrains Mono"`);
      await document.fonts.ready;
    } catch {
      // Fall through — xterm will still render with the platform fallback.
    }
  }

  const terminal = new Terminal({
    allowProposedApi: false,
    convertEol: true,
    cursorBlink: true,
    cursorStyle: 'block',
    disableStdin: false,
    fontFamily:
      '"JetBrains Mono", "SFMono-Regular", "Cascadia Code", "Liberation Mono", monospace',
    fontSize: settings.fontSize,
    fontWeight: '500',
    lineHeight: 1.12,
    macOptionIsMeta: true,
    scrollback: 4000,
    theme: themePalette(settings.theme)
  });
  const fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  terminal.loadAddon(new WebLinksAddon());
  terminal.open(root);
  fitAddon.fit();
  terminal.focus();
  return { terminal, fitAddon };
}
