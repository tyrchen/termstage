import { Terminal } from '@xterm/xterm';

import { TerminalSize } from './resize';

interface ResizeControlMessage {
  type: 'resize';
  cols: number;
  rows: number;
}

interface HeartbeatControlMessage {
  type: 'heartbeat';
  sequence: number;
}

type ClientControlMessage = ResizeControlMessage | HeartbeatControlMessage;

export interface TerminalSocket {
  sendResize: (size: TerminalSize) => void;
  close: () => void;
}

const RECONNECT_DELAYS_MS = [250, 500, 1000, 2000] as const;

export function connectTerminalSocket(terminal: Terminal): TerminalSocket {
  const token = new URLSearchParams(window.location.search).get('token') ?? '';
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const socketUrl = `${protocol}//${window.location.host}/ws?token=${token}`;
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let heartbeatSequence = 0;
  let heartbeatId: number | undefined;
  let reconnectId: number | undefined;
  let reconnectAttempt = 0;
  let closedByClient = false;
  let lastSize: TerminalSize = { cols: terminal.cols, rows: terminal.rows };
  let socket = openSocket();

  const disposable = terminal.onData((data: string) => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(encoder.encode(data));
    }
  });

  return {
    sendResize: (size: TerminalSize) => {
      lastSize = size;
      sendControl(socket, { type: 'resize', cols: size.cols, rows: size.rows });
    },
    close: () => {
      closedByClient = true;
      disposable.dispose();
      window.clearInterval(heartbeatId);
      window.clearTimeout(reconnectId);
      socket.close();
    }
  };

  function openSocket(): WebSocket {
    const nextSocket = new WebSocket(socketUrl);
    nextSocket.binaryType = 'arraybuffer';
    nextSocket.addEventListener('open', () => {
      reconnectAttempt = 0;
      sendControl(nextSocket, { type: 'resize', cols: lastSize.cols, rows: lastSize.rows });
      heartbeatId = window.setInterval(() => {
        heartbeatSequence += 1;
        sendControl(nextSocket, { type: 'heartbeat', sequence: heartbeatSequence });
      }, 15000);
    });
    nextSocket.addEventListener('message', (event: MessageEvent<string | ArrayBuffer>) => {
      if (event.data instanceof ArrayBuffer) {
        terminal.write(decoder.decode(event.data, { stream: true }));
        return;
      }
      handleControlMessage(terminal, event.data);
    });
    nextSocket.addEventListener('close', () => {
      window.clearInterval(heartbeatId);
      if (!closedByClient) {
        scheduleReconnect();
      }
    });
    return nextSocket;
  }

  function scheduleReconnect(): void {
    const delay =
      RECONNECT_DELAYS_MS[Math.min(reconnectAttempt, RECONNECT_DELAYS_MS.length - 1)];
    reconnectAttempt += 1;
    reconnectId = window.setTimeout(() => {
      socket = openSocket();
    }, delay);
  }
}

function sendControl(socket: WebSocket, message: ClientControlMessage): void {
  if (socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(message));
  }
}

function handleControlMessage(terminal: Terminal, data: string): void {
  try {
    const message = JSON.parse(data) as { type?: string; message?: string };
    if (message.type === 'error' && typeof message.message === 'string') {
      terminal.writeln(`\r\n${message.message}`);
    }
  } catch {
    terminal.writeln('\r\nprotocol error');
  }
}
