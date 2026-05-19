import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';

export interface TerminalSize {
  cols: number;
  rows: number;
}

export function watchTerminalResize(
  root: HTMLElement,
  terminal: Terminal,
  fitAddon: FitAddon,
  sendResize: (size: TerminalSize) => void
): () => void {
  let timeout: number | undefined;
  let lastSize = currentSize(terminal);
  const resizeObserver = new ResizeObserver(() => {
    window.clearTimeout(timeout);
    timeout = window.setTimeout(() => {
      fitAddon.fit();
      const nextSize = currentSize(terminal);
      if (nextSize.cols !== lastSize.cols || nextSize.rows !== lastSize.rows) {
        lastSize = nextSize;
        sendResize(nextSize);
      }
    }, 80);
  });
  resizeObserver.observe(root);
  sendResize(lastSize);
  return () => {
    window.clearTimeout(timeout);
    resizeObserver.disconnect();
  };
}

function currentSize(terminal: Terminal): TerminalSize {
  return {
    cols: terminal.cols,
    rows: terminal.rows
  };
}
