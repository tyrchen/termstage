import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';

import { PresentationSettings, TerminalFontFamily, themePalette } from './presentation';
import { clampTerminalSize } from './resize';
import { browserPresentationSettings, browserTerminalSurfaceSettings } from './settings';

export interface TerminalSurface {
  terminal: Terminal;
  fitAddon: FitAddon;
}

export async function createTerminalSurface(
  root: HTMLElement,
  settings: PresentationSettings
): Promise<TerminalSurface> {
  await waitForTerminalFonts(settings.fontFamily, settings.fontSize);

  const terminal = new Terminal({
    allowProposedApi: true,
    convertEol: true,
    customGlyphs: true,
    cursorBlink: true,
    cursorStyle: 'block',
    disableStdin: false,
    fontFamily: settings.fontFamily.css,
    fontSize: settings.fontSize,
    fontWeight: '400',
    fontWeightBold: '700',
    lineHeight: browserTerminalSurfaceSettings.lineHeight,
    macOptionIsMeta: true,
    minimumContrastRatio: themePalette(settings.theme).minimumContrastRatio,
    rescaleOverlappingGlyphs: true,
    scrollback: browserTerminalSurfaceSettings.scrollbackLines,
    theme: themePalette(settings.theme)
  });
  const fitAddon = new FitAddon();
  terminal.loadAddon(fitAddon);
  terminal.loadAddon(new Unicode11Addon());
  terminal.unicode.activeVersion = '11';
  terminal.loadAddon(new WebLinksAddon());
  suppressBackgroundColorReport(terminal);
  suppressCursorPositionReport(terminal);
  terminal.open(root);
  attachScrollbackWheelHandler(terminal);
  fitAddon.fit();
  resizeTerminalSurface(terminal, clampTerminalSize({ cols: terminal.cols, rows: terminal.rows }));
  syncTerminalGeometry(terminal);
  terminal.focus();
  return { terminal, fitAddon };
}

export function resizeTerminalSurface(terminal: Terminal, size: { cols: number; rows: number }): void {
  if (terminal.cols !== size.cols || terminal.rows !== size.rows) {
    terminal.resize(size.cols, size.rows);
  }
  syncTerminalGeometry(terminal);
}

export async function setTerminalFontFamily(
  terminal: Terminal,
  fontFamily: TerminalFontFamily
): Promise<void> {
  await waitForTerminalFonts(
    fontFamily,
    terminal.options.fontSize ?? browserPresentationSettings.defaultFontSize
  );
  terminal.options.fontFamily = fontFamily.css;
  document.documentElement.style.setProperty('--terminal-font-family', fontFamily.css);
  syncTerminalGeometry(terminal);
}

export function writeTerminalOutput(terminal: Terminal, data: string): void {
  const followOutput = isTerminalViewportPinnedToContentEnd(terminal);
  terminal.write(data, () => {
    if (followOutput) {
      scrollTerminalViewportToContentEnd(terminal);
    }
  });
}

function suppressBackgroundColorReport(terminal: Terminal): void {
  terminal.parser.registerOscHandler(11, () => true);
}

function suppressCursorPositionReport(terminal: Terminal): void {
  terminal.parser.registerCsiHandler({ final: 'n' }, (params) => {
    const [status] = params;
    return status === 6;
  });
}

function syncTerminalGeometry(terminal: Terminal): void {
  window.requestAnimationFrame(() => {
    const element = terminal.element;
    const screen = element?.querySelector<HTMLElement>('.xterm-screen');
    if (element === undefined || screen === null || screen === undefined) {
      return;
    }
    const width = Math.ceil(screen.getBoundingClientRect().width);
    const height = Math.ceil(screen.getBoundingClientRect().height);
    if (width > 0) {
      element.style.width = `${width}px`;
    }
    if (height > 0) {
      element.style.height = `${height}px`;
    }
  });
}

export function scrollTerminalViewportToContentEnd(terminal: Terminal): void {
  window.requestAnimationFrame(() => {
    window.requestAnimationFrame(() => {
      scrollTerminalViewportToRenderedContentEnd(terminal);
    });
  });
}

function scrollTerminalViewportToRenderedContentEnd(terminal: Terminal): void {
  const viewport = terminal.element?.parentElement;
  const screen = terminal.element?.querySelector<HTMLElement>('.xterm-screen');
  if (viewport === undefined || viewport === null || screen === undefined || screen === null) {
    return;
  }
  const contentBottom = (terminalContentEndRow(terminal) + 1) * terminalRowHeight(terminal, screen);
  viewport.scrollTop = clampScrollTop(contentBottom - viewport.clientHeight, viewport);
}

function isTerminalViewportPinnedToContentEnd(terminal: Terminal): boolean {
  const viewport = terminal.element?.parentElement;
  const screen = terminal.element?.querySelector<HTMLElement>('.xterm-screen');
  if (
    viewport === undefined ||
    viewport === null ||
    screen === undefined ||
    screen === null
  ) {
    return true;
  }
  const contentBottom = (terminalContentEndRow(terminal) + 1) * terminalRowHeight(terminal, screen);
  const desiredScrollTop = clampScrollTop(contentBottom - viewport.clientHeight, viewport);
  return Math.abs(viewport.scrollTop - desiredScrollTop) <= 2;
}

function terminalContentEndRow(terminal: Terminal): number {
  const rows = terminal.element?.querySelectorAll<HTMLElement>('.xterm-rows > div');
  if (rows === undefined || rows.length === 0) {
    return terminal.buffer.active.cursorY;
  }
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const row = rows.item(index);
    if (row.textContent?.trim() !== '') {
      return Math.max(index, terminal.buffer.active.cursorY);
    }
  }
  return terminal.buffer.active.cursorY;
}

function terminalRowHeight(terminal: Terminal, screen: HTMLElement): number {
  return screen.getBoundingClientRect().height / terminal.rows;
}

function clampScrollTop(value: number, viewport: HTMLElement): number {
  const maxScrollTop = Math.max(0, viewport.scrollHeight - viewport.clientHeight);
  return Math.min(maxScrollTop, Math.max(0, value));
}

async function waitForTerminalFonts(
  fontFamily: TerminalFontFamily,
  fontSize: number
): Promise<void> {
  if (typeof document === 'undefined' || !('fonts' in document)) {
    return;
  }

  try {
    await document.fonts.load(`400 ${fontSize}px ${fontFamily.css}`);
    await document.fonts.ready;
  } catch {
    return;
  }
}

function attachScrollbackWheelHandler(terminal: Terminal): void {
  terminal.element?.addEventListener('wheel', handleWheel, { capture: true, passive: false });

  function handleWheel(event: WheelEvent): void {
    if (scrollContainingViewport(event, terminal.element)) {
      return;
    }
    if (terminal.modes.mouseTrackingMode !== 'none' || terminal.buffer.active.baseY === 0) {
      return;
    }
    const lines = wheelDeltaToLines(event, terminal.rows);
    if (lines === 0) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    terminal.scrollLines(lines);
  }
}

function scrollContainingViewport(event: WheelEvent, element: HTMLElement | undefined): boolean {
  if (element === undefined || event.deltaY === 0) {
    return false;
  }
  const scroller = element.parentElement;
  if (scroller === null || !canScrollVertically(scroller, event.deltaY)) {
    return false;
  }
  const before = scroller.scrollTop;
  scroller.scrollTop += wheelDeltaToPixels(event, scroller.clientHeight);
  if (scroller.scrollTop === before) {
    return false;
  }
  event.preventDefault();
  event.stopPropagation();
  return true;
}

function canScrollVertically(element: HTMLElement, deltaY: number): boolean {
  if (deltaY > 0) {
    return element.scrollTop + element.clientHeight < element.scrollHeight;
  }
  return element.scrollTop > 0;
}

function wheelDeltaToPixels(event: WheelEvent, pageHeight: number): number {
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) {
    return Math.sign(event.deltaY) * pageHeight;
  }
  if (event.deltaMode === WheelEvent.DOM_DELTA_LINE) {
    return event.deltaY * browserTerminalSurfaceSettings.wheelPixelLineHeight;
  }
  return event.deltaY;
}

function wheelDeltaToLines(event: WheelEvent, rows: number): number {
  if (event.deltaY === 0) {
    return 0;
  }
  const direction = Math.sign(event.deltaY);
  const magnitude = Math.abs(event.deltaY);
  const lines =
    event.deltaMode === WheelEvent.DOM_DELTA_PAGE
      ? Math.max(1, rows - 1)
      : event.deltaMode === WheelEvent.DOM_DELTA_LINE
        ? Math.max(1, Math.round(magnitude))
        : Math.max(1, Math.round(magnitude / browserTerminalSurfaceSettings.wheelPixelLineHeight));
  return direction * Math.min(browserTerminalSurfaceSettings.maxWheelLines, lines);
}
