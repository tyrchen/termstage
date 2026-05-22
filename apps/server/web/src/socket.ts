import { Terminal } from '@xterm/xterm';

import { ConnectionStatus } from './connection-status';
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

export interface TerminalSocketOptions {
  onStatusChange?: (status: ConnectionStatus) => void;
}

const RECONNECT_DELAYS_MS = [250, 500, 1000, 2000] as const;
const LOST_AFTER_RECONNECT_ATTEMPTS = RECONNECT_DELAYS_MS.length;
const SESSION_ENDED_REASON = 'session ended';
const SERVER_SHUTDOWN_REASON = 'server shutting down';
const RUNTIME_ERROR_REASON = 'runtime error';
const CLIENT_DISCONNECTED_REASON = 'client disconnected';
const CONTROLLER_REPLACED_REASON = 'controller replaced';
const BROWSER_BACKPRESSURE_REASON = 'browser client backpressure';
const NORMAL_CLOSE_CODE = 1000;

export function connectTerminalSocket(
  terminal: Terminal,
  options: TerminalSocketOptions = {}
): TerminalSocket {
  const token = new URLSearchParams(window.location.search).get('token') ?? '';
  const baseUrl = new URL('ws', document.baseURI);
  baseUrl.protocol = baseUrl.protocol === 'https:' ? 'wss:' : 'ws:';
  baseUrl.search = `?token=${token}`;
  const socketUrl = baseUrl.toString();
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
      emitStatus({ state: 'connected' });
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
    nextSocket.addEventListener('close', (event: CloseEvent) => {
      window.clearInterval(heartbeatId);
      if (closedByClient) {
        return;
      }

      const terminalEnd = terminalEndStatus(event);
      if (terminalEnd !== undefined) {
        emitStatus(terminalEnd);
        return;
      }

      scheduleReconnect();
    });
    return nextSocket;
  }

  function scheduleReconnect(): void {
    if (reconnectAttempt >= LOST_AFTER_RECONNECT_ATTEMPTS) {
      emitStatus({ state: 'lost' });
      return;
    }

    emitStatus({ state: 'reconnecting' });
    const delay =
      RECONNECT_DELAYS_MS[Math.min(reconnectAttempt, RECONNECT_DELAYS_MS.length - 1)];
    reconnectAttempt += 1;
    reconnectId = window.setTimeout(() => {
      socket = openSocket();
    }, delay);
  }

  function emitStatus(status: ConnectionStatus): void {
    options.onStatusChange?.(status);
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

function terminalEndStatus(event: CloseEvent): ConnectionStatus | undefined {
  if (event.code === NORMAL_CLOSE_CODE && event.reason === '') {
    return {
      state: 'ended',
      title: 'Connection closed',
      message: 'The server closed this browser connection.'
    };
  }

  switch (event.reason) {
    case SESSION_ENDED_REASON:
      return {
        state: 'ended',
        title: 'Session ended',
        message: 'The terminal process exited.'
      };
    case SERVER_SHUTDOWN_REASON:
      return {
        state: 'ended',
        title: 'Session ended',
        message: 'The server shut down.'
      };
    case RUNTIME_ERROR_REASON:
      return {
        state: 'ended',
        title: 'Session ended',
        message: 'The terminal runtime stopped after an error.'
      };
    case CONTROLLER_REPLACED_REASON:
      return {
        state: 'ended',
        title: 'Session attached elsewhere',
        message: 'A newer browser connection took over this session.'
      };
    case CLIENT_DISCONNECTED_REASON:
    case BROWSER_BACKPRESSURE_REASON:
      return {
        state: 'ended',
        title: 'Connection closed',
        message: 'The server closed this browser connection.'
      };
    default:
      return undefined;
  }
}
