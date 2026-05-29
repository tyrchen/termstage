import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';

import { browserTerminalResizeSettings } from './settings';

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
  let lastSize = proposedTerminalSize(fitAddon, terminal);
  const resizeObserver = new ResizeObserver(() => {
    window.clearTimeout(timeout);
    timeout = window.setTimeout(() => {
      const nextSize = proposedTerminalSize(fitAddon, terminal);
      if (nextSize.cols !== lastSize.cols || nextSize.rows !== lastSize.rows) {
        lastSize = nextSize;
        sendResize(nextSize);
      }
    }, browserTerminalResizeSettings.observerDebounceMs);
  });
  resizeObserver.observe(root);
  sendResize(lastSize);
  return () => {
    window.clearTimeout(timeout);
    resizeObserver.disconnect();
  };
}

export function proposedTerminalSize(fitAddon: FitAddon, terminal: Terminal): TerminalSize {
  return clampTerminalSize(proposedSize(fitAddon) ?? currentSize(terminal));
}

export function clampTerminalSize(size: TerminalSize): TerminalSize {
  return {
    cols: clampDimension(
      size.cols,
      browserTerminalResizeSettings.colsMin,
      browserTerminalResizeSettings.colsMax
    ),
    rows: clampDimension(
      size.rows,
      browserTerminalResizeSettings.rowsMin,
      browserTerminalResizeSettings.rowsMax
    )
  };
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

function clampDimension(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) {
    return min;
  }
  return Math.min(max, Math.max(min, Math.floor(value)));
}
