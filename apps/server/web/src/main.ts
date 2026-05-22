import './style.css';

import { createConnectionStatusDialog } from './connection-status';
import { readPresentationSettings } from './presentation';
import { watchTerminalResize } from './resize';
import { connectTerminalSocket } from './socket';
import { createTerminalSurface } from './terminal';

const root = document.querySelector<HTMLElement>('#terminal-root');

if (root !== null) {
  const settings = readPresentationSettings();
  const statusDialog = createConnectionStatusDialog(root);
  void createTerminalSurface(root, settings).then((surface) => {
    const socket = connectTerminalSocket(surface.terminal, {
      onStatusChange: statusDialog.update
    });
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
