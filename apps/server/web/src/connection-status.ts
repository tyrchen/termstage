export type ConnectionStatus =
  | { state: 'connected' }
  | { state: 'reconnecting' }
  | { state: 'lost' }
  | { state: 'ended'; title: string; message: string };

export interface ConnectionStatusDialog {
  update: (status: ConnectionStatus) => void;
}

export function createConnectionStatusDialog(root: HTMLElement): ConnectionStatusDialog {
  const overlay = document.createElement('div');
  overlay.className = 'connection-status';
  overlay.hidden = true;
  overlay.setAttribute('role', 'dialog');
  overlay.setAttribute('aria-modal', 'true');
  overlay.setAttribute('aria-labelledby', 'connection-status-title');
  overlay.setAttribute('aria-describedby', 'connection-status-message');

  const panel = document.createElement('div');
  panel.className = 'connection-status__panel';

  const title = document.createElement('h1');
  title.id = 'connection-status-title';

  const message = document.createElement('p');
  message.id = 'connection-status-message';

  panel.append(title, message);
  overlay.append(panel);
  root.append(overlay);

  return {
    update: (status: ConnectionStatus) => {
      if (status.state === 'connected') {
        overlay.hidden = true;
        return;
      }

      const copy = copyForStatus(status);
      title.textContent = copy.title;
      message.textContent = copy.message;
      overlay.hidden = false;
    }
  };
}

function copyForStatus(status: Exclude<ConnectionStatus, { state: 'connected' }>): {
  title: string;
  message: string;
} {
  switch (status.state) {
    case 'reconnecting':
      return {
        title: 'Reconnecting',
        message: 'The terminal connection dropped. Trying to reconnect.'
      };
    case 'lost':
      return {
        title: 'Lost connectivity',
        message: 'The server is not reachable. Refresh after restarting it.'
      };
    case 'ended':
      return {
        title: status.title,
        message: status.message
      };
  }
}
