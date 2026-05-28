import { Terminal } from '@xterm/xterm';

import { ConnectionStatus } from './connection-status';
import { TerminalSize, clampTerminalSize } from './resize';
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

interface AcquireControlMessage {
  type: 'acquireControl';
}

interface ViewportControlMessage {
  type: 'viewport';
  col?: number;
  row?: number;
}

type ClientControlMessage =
  | ResizeControlMessage
  | HeartbeatControlMessage
  | AcquireControlMessage
  | ViewportControlMessage;

export interface TerminalViewportOrigin {
  col?: number;
  row?: number;
}

export interface TerminalSocket {
  sendResize: (size: TerminalSize) => void;
  sendViewport: (origin: TerminalViewportOrigin) => void;
  close: () => void;
}

export interface TerminalSocketOptions {
  onStatusChange?: (status: ConnectionStatus) => void;
  onLeaseChange?: (owner: 'terminal' | 'browser' | 'agent') => void;
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
const ACQUIRE_CONTROL_THROTTLE_MS = 50;
const PENDING_ACQUIRE_INPUT_TTL_MS = 1000;
const PENDING_ACQUIRE_INPUT_MAX_CHARS = 4096;

export function connectTerminalSocket(
  terminal: Terminal,
  options: TerminalSocketOptions = {}
): TerminalSocket {
  const token = new URLSearchParams(window.location.search).get('token') ?? '';
  const baseUrl = new URL('ws', document.baseURI);
  baseUrl.protocol = baseUrl.protocol === 'https:' ? 'wss:' : 'ws:';
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();
  let heartbeatSequence = 0;
  let heartbeatId: number | undefined;
  let reconnectId: number | undefined;
  let reconnectAttempt = 0;
  let closedByClient = false;
  let terminalEnded = false;
  let lastSize: TerminalSize = clampTerminalSize({ cols: terminal.cols, rows: terminal.rows });
  let socket = openSocket();
  let inputForwardingSuppressed = true;
  let leaseOwner: 'terminal' | 'browser' | 'agent' = 'terminal';
  let lastAcquireControlAt = 0;
  let pendingAcquireInput = '';
  let pendingAcquireInputExpiresAt = 0;

  const disposable = terminal.onData((data: string) => {
    if (socket.readyState !== WebSocket.OPEN) {
      return;
    }
    if (inputForwardingSuppressed) {
      queuePendingAcquireInput(data);
      return;
    }
    if (leaseOwner === 'browser') {
      socket.send(encoder.encode(data));
      return;
    }
    queuePendingAcquireInput(data);
    requestBrowserControl();
  });
  const acquireControlArm = createAcquireControlArm(terminal, requestBrowserControl);

  return {
    sendResize: (size: TerminalSize) => {
      lastSize = clampTerminalSize(size);
      sendControl(socket, { type: 'resize', cols: lastSize.cols, rows: lastSize.rows });
    },
    sendViewport: (origin: TerminalViewportOrigin) => {
      sendControl(socket, { type: 'viewport', ...origin });
    },
    close: () => {
      closedByClient = true;
      disposable.dispose();
      acquireControlArm.dispose();
      window.clearInterval(heartbeatId);
      window.clearTimeout(reconnectId);
      socket.close();
    }
  };

  function openSocket(): WebSocket {
    const nextSocket = new WebSocket(currentSocketUrl());
    nextSocket.binaryType = 'arraybuffer';
    nextSocket.addEventListener('open', () => {
      reconnectAttempt = 0;
      lastAcquireControlAt = 0;
      clearPendingAcquireInput();
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

  function emitLeaseChange(owner: 'terminal' | 'browser' | 'agent'): void {
    leaseOwner = owner;
    lastAcquireControlAt = 0;
    if (owner === 'browser') {
      flushPendingAcquireInput();
    } else {
      clearPendingAcquireInput();
    }
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
      resumePendingInput();
    });
  }

  function requestBrowserControl(): void {
    if (
      inputForwardingSuppressed ||
      leaseOwner === 'browser' ||
      socket.readyState !== WebSocket.OPEN
    ) {
      return;
    }
    const now = Date.now();
    if (now - lastAcquireControlAt < ACQUIRE_CONTROL_THROTTLE_MS) {
      return;
    }
    lastAcquireControlAt = now;
    sendControl(socket, { type: 'acquireControl' });
  }

  function queuePendingAcquireInput(data: string): void {
    if (pendingAcquireInput.length + data.length > PENDING_ACQUIRE_INPUT_MAX_CHARS) {
      clearPendingAcquireInput();
      return;
    }
    pendingAcquireInput += data;
    pendingAcquireInputExpiresAt = Date.now() + PENDING_ACQUIRE_INPUT_TTL_MS;
  }

  function flushPendingAcquireInput(): void {
    if (pendingAcquireInput.length === 0) {
      return;
    }
    const input = pendingAcquireInput;
    const expiresAt = pendingAcquireInputExpiresAt;
    clearPendingAcquireInput();
    if (
      Date.now() <= expiresAt &&
      socket.readyState === WebSocket.OPEN &&
      !inputForwardingSuppressed
    ) {
      socket.send(encoder.encode(input));
    }
  }

  function clearPendingAcquireInput(): void {
    pendingAcquireInput = '';
    pendingAcquireInputExpiresAt = 0;
  }

  function resumePendingInput(): void {
    if (pendingAcquireInput.length === 0) {
      return;
    }
    if (leaseOwner === 'browser') {
      flushPendingAcquireInput();
    } else {
      requestBrowserControl();
    }
  }

  function currentSocketUrl(): string {
    baseUrl.search = '';
    baseUrl.searchParams.set('token', token);
    baseUrl.searchParams.set('cols', lastSize.cols.toString());
    baseUrl.searchParams.set('rows', lastSize.rows.toString());
    return baseUrl.toString();
  }
}

function createAcquireControlArm(terminal: Terminal, requestBrowserControl: () => void): {
  dispose: () => void;
} {
  const element = terminal.element;
  if (element === undefined) {
    return { dispose: () => undefined };
  }
  const listener = (): void => {
    requestBrowserControl();
  };
  element.addEventListener('mousedown', listener, { capture: true });
  element.addEventListener('keydown', listener, { capture: true });
  element.addEventListener('paste', listener, { capture: true });
  return {
    dispose: () => {
      element.removeEventListener('mousedown', listener, { capture: true });
      element.removeEventListener('keydown', listener, { capture: true });
      element.removeEventListener('paste', listener, { capture: true });
    }
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
  emitLeaseChange: (owner: 'terminal' | 'browser' | 'agent') => void,
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
      if (owner === 'terminal' || owner === 'browser' || owner === 'agent') {
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
    case CLIENT_DISCONNECTED_REASON:
    case BROWSER_BACKPRESSURE_REASON:
      return undefined;
    case SESSION_ENDED_REASON:
      return {
        state: 'ended',
        title: 'Session ended',
        message: 'The backend session ended.'
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
    default:
      return undefined;
  }
}
