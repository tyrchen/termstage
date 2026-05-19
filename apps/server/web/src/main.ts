import './style.css';

import { readPresentationSettings } from './presentation';
import { watchTerminalResize } from './resize';
import { connectTerminalSocket } from './socket';
import { createTerminalSurface } from './terminal';

const root = document.querySelector<HTMLElement>('#terminal-root');

if (root !== null) {
  const settings = readPresentationSettings();
  const surface = createTerminalSurface(root, settings);
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
}
