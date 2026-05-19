import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import { PresentationSettings, themePalette } from './presentation';

export interface TerminalSurface {
  terminal: Terminal;
  fitAddon: FitAddon;
}

export function createTerminalSurface(
  root: HTMLElement,
  settings: PresentationSettings
): TerminalSurface {
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
