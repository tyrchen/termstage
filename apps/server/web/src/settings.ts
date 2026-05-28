export const browserPresentationSettings = {
  defaultFontFamily: 'termstage',
  defaultFontSize: 24,
  fontSizeMin: 12,
  fontSizeMax: 96
} as const;

export const browserTerminalResizeSettings = {
  colsMin: 20,
  colsMax: 300,
  rowsMin: 5,
  rowsMax: 120,
  observerDebounceMs: 80
} as const;

export const browserTerminalSocketSettings = {
  reconnectDelaysMs: [250, 500, 1000, 2000],
  acquireControlThrottleMs: 50,
  pendingAcquireInputTtlMs: 1000,
  pendingAcquireInputMaxChars: 4096
} as const;

export const browserTerminalCloseReasons = {
  sessionEnded: 'session ended',
  serverShutdown: 'server shutting down',
  runtimeError: 'runtime error',
  clientDisconnected: 'client disconnected',
  controllerReplaced: 'controller replaced',
  browserBackpressure: 'browser client backpressure'
} as const;

export const browserTerminalViewportSettings = {
  originMax: 10000,
  wheelPixelCell: 36,
  wheelLineCell: 3,
  wheelPageCell: 24
} as const;

export const browserTerminalSurfaceSettings = {
  lineHeight: 1.08,
  scrollbackLines: 4000,
  wheelPixelLineHeight: 40,
  maxWheelLines: 24
} as const;
