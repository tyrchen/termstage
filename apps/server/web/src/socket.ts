import { Terminal } from '@xterm/xterm';

import { ConnectionStatus } from './connection-status';
import { TerminalSize } from './resize';
import { scrollTerminalViewportToContentEnd, writeTerminalOutput } from './terminal';

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
  onLeaseChange?: (owner: 'terminal' | 'browser') => void;
  onSessionReady?: (session: string) => void;
  onSizeChange?: (size: TerminalSize) => void;
}

const RECONNECT_DELAYS_MS = [250, 500, 1000, 2000] as const;
const LOST_AFTER_RECONNECT_ATTEMPTS = RECONNECT_DELAYS_MS.length;
const SESSION_ENDED_REASON = 'session ended';
const SERVER_SHUTDOWN_REASON = 'server shutting down';
const RUNTIME_ERROR_REASON = 'runtime error';
const CLIENT_DISCONNECTED_REASON = 'client disconnected';
const CONTROLLER_REPLACED_REASON = 'controller replaced';
const BROWSER_BACKPRESSURE_REASON = 'browser client backpressure';

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
  let terminalEnded = false;
  let lastSize: TerminalSize = { cols: terminal.cols, rows: terminal.rows };
  let socket = openSocket();
  let inputForwardingSuppressed = true;
  let leaseOwner: 'terminal' | 'browser' = 'terminal';
  const userInputArm = createUserInputArm(terminal);

  const disposable = terminal.onData((data: string) => {
    if (
      !inputForwardingSuppressed &&
      (leaseOwner === 'browser' || userInputArm.isArmed()) &&
      socket.readyState === WebSocket.OPEN
    ) {
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
      userInputArm.dispose();
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
      setInputForwardingSuppressed(true);
      emitStatus({ state: 'connected' });
      sendControl(nextSocket, { type: 'resize', cols: lastSize.cols, rows: lastSize.rows });
      heartbeatId = window.setInterval(() => {
        heartbeatSequence += 1;
        sendControl(nextSocket, { type: 'heartbeat', sequence: heartbeatSequence });
      }, 15000);
    });
    nextSocket.addEventListener('message', (event: MessageEvent<string | ArrayBuffer>) => {
      if (event.data instanceof ArrayBuffer) {
        writeTerminalOutput(terminal, decoder.decode(event.data, { stream: true }));
        return;
      }
      terminalEnded =
        handleControlMessage(
          terminal,
          event.data,
          setInputForwardingSuppressed,
          finishReplay,
          emitStatus,
          emitLeaseChange,
          emitSessionReady,
          emitSizeChange
        ) ||
        terminalEnded;
    });
    nextSocket.addEventListener('close', (event: CloseEvent) => {
      window.clearInterval(heartbeatId);
      if (closedByClient || terminalEnded) {
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

  function emitLeaseChange(owner: 'terminal' | 'browser'): void {
    leaseOwner = owner;
    options.onLeaseChange?.(owner);
  }

  function emitSessionReady(session: string): void {
    options.onSessionReady?.(session);
  }

  function emitSizeChange(size: TerminalSize): void {
    options.onSizeChange?.(size);
  }

  function setInputForwardingSuppressed(suppressed: boolean): void {
    inputForwardingSuppressed = suppressed;
    terminal.options.disableStdin = suppressed;
  }

  function finishReplay(): void {
    terminal.write('', () => {
      scrollTerminalViewportToContentEnd(terminal);
      setInputForwardingSuppressed(false);
    });
  }
}

interface UserInputArm {
  dispose: () => void;
  isArmed: () => boolean;
}

function createUserInputArm(terminal: Terminal): UserInputArm {
  let armed = false;
  let disarmTimeout: number | undefined;
  const element = terminal.element;
  if (element === undefined) {
    return {
      dispose: () => {},
      isArmed: () => false
    };
  }
  const arm = (): void => {
    armed = true;
    window.clearTimeout(disarmTimeout);
    disarmTimeout = window.setTimeout(() => {
      armed = false;
    }, 100);
  };
  const eventTypes = ['keydown', 'keypress', 'paste', 'compositionend', 'mousedown', 'wheel'];
  for (const eventType of eventTypes) {
    element.addEventListener(eventType, arm, { capture: true });
  }
  return {
    dispose: () => {
      window.clearTimeout(disarmTimeout);
      for (const eventType of eventTypes) {
        element.removeEventListener(eventType, arm, { capture: true });
      }
    },
    isArmed: () => armed
  };
}

function sendControl(socket: WebSocket, message: ClientControlMessage): void {
  if (socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(message));
  }
}

function handleControlMessage(
  terminal: Terminal,
  data: string,
  suppressInputForwarding: (suppressed: boolean) => void,
  finishReplay: () => void,
  emitStatus: (status: ConnectionStatus) => void,
  emitLeaseChange: (owner: 'terminal' | 'browser') => void,
  emitSessionReady: (session: string) => void,
  emitSizeChange: (size: TerminalSize) => void
): boolean {
  try {
    const message = JSON.parse(data) as { type?: string; message?: string };
    if (
      message.type === 'ready' &&
      typeof (message as { session?: unknown }).session === 'string'
    ) {
      emitSessionReady((message as { session: string }).session);
      return false;
    }
    if (message.type === 'replayStarted') {
      suppressInputForwarding(true);
      return false;
    }
    if (message.type === 'replayFinished') {
      finishReplay();
      return false;
    }
    if (message.type === 'sizeChanged') {
      const size = (message as { size?: unknown }).size;
      if (isTerminalSize(size)) {
        emitSizeChange(size);
        return false;
      }
    }
    if (
      message.type === 'leaseChanged' &&
      (message as { owner?: string }).owner !== undefined
    ) {
      const owner = (message as { owner?: string }).owner;
      if (owner === 'terminal' || owner === 'browser') {
        emitStatus({ state: 'connected' });
        emitLeaseChange(owner);
        return false;
      }
    }
    if (message.type === 'processExited') {
      emitStatus({
        state: 'ended',
        title: 'Process exited',
        message: message.message ?? 'The terminal process exited.'
      });
      return true;
    }
    if (message.type === 'error' && typeof message.message === 'string') {
      terminal.writeln(`\r\n${message.message}`);
    }
  } catch {
    terminal.writeln('\r\nprotocol error');
  }
  return false;
}

function isTerminalSize(value: unknown): value is TerminalSize {
  if (typeof value !== 'object' || value === null) {
    return false;
  }
  const candidate = value as { cols?: unknown; rows?: unknown };
  return typeof candidate.cols === 'number' && typeof candidate.rows === 'number';
}

function terminalEndStatus(event: CloseEvent): ConnectionStatus | undefined {
  switch (event.reason) {
    case SESSION_ENDED_REASON:
    case CLIENT_DISCONNECTED_REASON:
    case BROWSER_BACKPRESSURE_REASON:
      return undefined;
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
    default:
      return undefined;
  }
}
