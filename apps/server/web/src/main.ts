import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@fontsource/jetbrains-mono/700.css';

import './style.css';

import { readPresentationSettings } from './presentation';
import { watchTerminalResize } from './resize';
import { connectTerminalSocket } from './socket';
import { createTerminalSurface } from './terminal';

const root = document.querySelector<HTMLElement>('#terminal-root');

if (root !== null) {
  const settings = readPresentationSettings();
  void createTerminalSurface(root, settings).then((surface) => {
    const socket = connectTerminalSocket(surface.terminal);
    const stopResize = watchTerminalResize(
      root,
      surface.terminal,
      surface.fitAddon,
      socket.sendResize
    );
    window.addEventListener('beforeunload', () => {
      stopResize();
      socket.close();
    });
  });
}
