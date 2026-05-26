import './style.css';

import { createConnectionStatusDialog, createTerminalToolbar } from './connection-status';
import {
  TerminalFontFamily,
  TerminalFontFamilyName,
  PresentationThemeName,
  applyThemeToDocument,
  clampFontSize,
  fontFamilyByName,
  readPresentationSettings,
  themePalette
} from './presentation';
import { proposedTerminalSize, TerminalSize, watchTerminalResize } from './resize';
import { connectTerminalSocket } from './socket';
import { createTerminalSurface, resizeTerminalSurface, setTerminalFontFamily } from './terminal';

const root = document.querySelector<HTMLElement>('#terminal-root');

if (root !== null) {
  const settings = readPresentationSettings();
  const terminalViewport = document.createElement('div');
  terminalViewport.className = 'terminal-viewport';
  let currentFontFamily = settings.fontFamily;
  let currentFontSize = settings.fontSize;
  let currentTheme = settings.theme;
  let currentRuntimeSize: TerminalSize | undefined;
  let applyFontFamily = (_fontFamilyName: TerminalFontFamilyName): void => {};
  let applyFontSize = (_delta: number): void => {};
  let applyTheme = (_theme: PresentationThemeName): void => {};
  const toolbar = createTerminalToolbar(root, {
    onChangeFontFamily: (fontFamilyName) => {
      applyFontFamily(fontFamilyName);
    },
    onChangeTheme: (theme) => {
      applyTheme(theme);
    },
    onDecreaseFont: () => {
      applyFontSize(-1);
    },
    onIncreaseFont: () => {
      applyFontSize(1);
    }
  });
  root.append(terminalViewport);
  const statusDialog = createConnectionStatusDialog(root);
  toolbar.updateFontFamily(currentFontFamily);
  toolbar.updateFontSize(currentFontSize);
  toolbar.updateTheme(currentTheme);
  void createTerminalSurface(terminalViewport, settings).then((surface) => {
    const socket = connectTerminalSocket(surface.terminal, {
      onStatusChange: statusDialog.update,
      onLeaseChange: toolbar.updateLease,
      onSessionReady: toolbar.updateSession,
      onSizeChange: (size) => {
        currentRuntimeSize = size;
        resizeTerminalSurface(surface.terminal, size);
      }
    });
    const stopResize = watchTerminalResize(
      terminalViewport,
      surface.terminal,
      surface.fitAddon,
      socket.sendResize
    );
    window.addEventListener('beforeunload', () => {
      stopResize();
      socket.close();
    });

    applyFontFamily = (fontFamilyName: TerminalFontFamilyName): void => {
      const nextFontFamily = fontFamilyByName(fontFamilyName);
      if (nextFontFamily.name === currentFontFamily.name) {
        surface.terminal.focus();
        return;
      }
      currentFontFamily = nextFontFamily;
      updateFontFamilyQuery(nextFontFamily);
      void setTerminalFontFamily(surface.terminal, nextFontFamily).then(() => {
        if (currentRuntimeSize !== undefined) {
          resizeTerminalSurface(surface.terminal, currentRuntimeSize);
        }
        toolbar.updateFontFamily(nextFontFamily);
        surface.terminal.focus();
      });
    };

    applyTheme = (theme: PresentationThemeName): void => {
      if (theme === currentTheme) {
        surface.terminal.focus();
        return;
      }
      currentTheme = theme;
      applyThemeToDocument(theme);
      surface.terminal.options.theme = themePalette(theme);
      updateThemeQuery(theme);
      toolbar.updateTheme(theme);
      surface.terminal.focus();
    };

    applyFontSize = (delta: number): void => {
      const nextFontSize = clampFontSize(currentFontSize + delta);
      if (nextFontSize === currentFontSize) {
        surface.terminal.focus();
        return;
      }
      currentFontSize = nextFontSize;
      surface.terminal.options.fontSize = nextFontSize;
      document.documentElement.style.setProperty('--terminal-font-size', `${nextFontSize}px`);
      if (currentRuntimeSize !== undefined) {
        resizeTerminalSurface(surface.terminal, currentRuntimeSize);
      }
      socket.sendResize(proposedTerminalSize(surface.fitAddon, surface.terminal));
      toolbar.updateFontSize(nextFontSize);
      surface.terminal.focus();
    };
  });
}

function updateFontFamilyQuery(fontFamily: TerminalFontFamily): void {
  const url = new URL(window.location.href);
  url.searchParams.set('fontFamily', fontFamily.name);
  window.history.replaceState(null, '', url);
}

function updateThemeQuery(theme: PresentationThemeName): void {
  const url = new URL(window.location.href);
  url.searchParams.set('theme', theme);
  window.history.replaceState(null, '', url);
}
