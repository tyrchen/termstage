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

export function connectTerminalSocket(terminal: Terminal): TerminalSocket {
  const token = new URLSearchParams(window.location.search).get('token') ?? '';
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const socket = new WebSocket(`${protocol}//${window.location.host}/ws?token=${token}`);
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let heartbeatSequence = 0;
  let heartbeatId: number | undefined;

  socket.binaryType = 'arraybuffer';
  const disposable = terminal.onData((data: string) => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(encoder.encode(data));
    }
  });

  socket.addEventListener('open', () => {
    heartbeatId = window.setInterval(() => {
      heartbeatSequence += 1;
      sendControl(socket, { type: 'heartbeat', sequence: heartbeatSequence });
    }, 15000);
  });
  socket.addEventListener('message', (event: MessageEvent<string | ArrayBuffer>) => {
    if (event.data instanceof ArrayBuffer) {
      terminal.write(decoder.decode(event.data, { stream: true }));
      return;
    }
    handleControlMessage(terminal, event.data);
  });
  socket.addEventListener('close', () => {
    disposable.dispose();
    window.clearInterval(heartbeatId);
  });

  return {
    sendResize: (size: TerminalSize) => {
      sendControl(socket, { type: 'resize', cols: size.cols, rows: size.rows });
    },
    close: () => {
      disposable.dispose();
      window.clearInterval(heartbeatId);
      socket.close();
    }
  };
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
