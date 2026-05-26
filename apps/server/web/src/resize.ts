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
  let lastSize = proposedSize(fitAddon) ?? currentSize(terminal);
  const resizeObserver = new ResizeObserver(() => {
    window.clearTimeout(timeout);
    timeout = window.setTimeout(() => {
      const nextSize = proposedSize(fitAddon);
      if (nextSize === undefined) {
        return;
      }
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

export function proposedTerminalSize(fitAddon: FitAddon, terminal: Terminal): TerminalSize {
  return proposedSize(fitAddon) ?? currentSize(terminal);
}

function currentSize(terminal: Terminal): TerminalSize {
  return {
    cols: terminal.cols,
    rows: terminal.rows
  };
}

function proposedSize(fitAddon: FitAddon): TerminalSize | undefined {
  const dimensions = fitAddon.proposeDimensions();
  if (dimensions === undefined) {
    return undefined;
  }
  return {
    cols: dimensions.cols,
    rows: dimensions.rows
  };
}
