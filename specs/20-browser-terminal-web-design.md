# 20-browser-terminal-web: Server and Browser UI

Status: draft v1
Owner: termstage
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
[11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md)

## 1. Purpose

The web layer owns the local Axum server, WebSocket upgrade path, bundled static
assets, and browser terminal UI. It does not parse shell commands or own PTY state.

## 2. Interface

Routes:

| Route | Method | Purpose |
| --- | --- | --- |
| `/` | GET | Serve the terminal page after token, Host, Origin, and peer validation. |
| `/assets/*path` | GET | Serve bundled, same-origin frontend assets. |
| `/ws` | GET upgrade | Upgrade to WebSocket after full security validation. |
| `/healthz` | GET | Local readiness endpoint with no sensitive runtime data. |

Frontend modules:

| Module | Purpose |
| --- | --- |
| `terminal.ts` | xterm.js creation, fit addon, theme/font presets. |
| `socket.ts` | WebSocket lifecycle and protocol frame handling. |
| `resize.ts` | Fit-to-container and debounced resize control messages. |
| `presentation.ts` | Presentation mode settings: font size, theme, cursor, copy/paste behavior. |

## 2a. Flow

```text
+--------------------------- loopback only ---------------------------+
|                                                                     |
|  Chrome tab                                                         |
|  +--------------------+      binary/text WS       +--------------+  |
|  | xterm.js frontend  | <-----------------------> | Axum routes  |  |
|  | - @xterm/xterm     |                           | / /ws/assets |  |
|  | - fit addon        |                           +------+-------+  |
|  | - web-links addon  |                                  |          |
|  +--------------------+                                  |          |
|                                                          v          |
|                                                  +---------------+  |
|                                                  | Runtime actor |  |
|                                                  | PTY/session   |  |
|                                                  +-------+-------+  |
|                                                          |          |
+----------------------------------------------------------+----------+
                                                           |
                                                           v
                                                   local shell/tmux
```

## 3. Invariants

- The server listens on `127.0.0.1` by default. `::1` support is allowed after tests
  cover IPv6 loopback handling.
- Static assets are bundled with the binary or served from a trusted local build
  directory in development; production UI never loads CDN JavaScript.
- The WebSocket upgrade validates token, Host, Origin, and peer IP before calling
  `on_upgrade`.
- WebSocket max frame size and max message size are explicitly configured.
- Browser UI sends terminal input as binary frames and resize as JSON text frames.
- UI text does not explain implementation details; the screen is the usable terminal.

## 4. Behavior

The server starts on port `0` unless the CLI provides a loopback port. After binding,
it builds a tokenized URL and optionally opens the browser. The URL token is exchanged
for an in-memory token check; it is not persisted.

The frontend uses `@xterm/xterm`, `@xterm/addon-fit`, and `@xterm/addon-web-links`.
The WebGL addon is optional and guarded behind a frontend feature because rendering
fallback must work on presentation machines without GPU acceleration.

The WebSocket handler splits send and receive halves. Receive frames map to
`RuntimeCommand::Input` or validated `RuntimeCommand::Resize`. Runtime output is sent
as binary frames to the browser. Any protocol or security error closes the socket and
emits a redacted structured log event.

## 5. AGENTS.md Binding

- Error handling: handler errors convert into typed HTTP/WebSocket responses without
  leaking tokens or terminal payloads.
- Async/concurrency: handler tasks are supervised; no unbounded spawned task per frame.
- Type design: route state is a typed `AppState`; token/host/origin checks use domain
  types from `termstage-core`.
- Safety/security: local-service security follows
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md).
- Serialization: JSON control messages use `serde` and immediate validation.
- Testing: HTTP route tests, WebSocket protocol tests, and Playwright smoke tests.
- Observability: `tower-http` trace layer with redacted fields.
- Performance: static assets compressed at build time when supported; terminal frames
  avoid JSON encoding.
- Documentation: public server builder APIs document loopback and token assumptions.

## 6. Cross-References

- Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
  [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md).
- Consumed by: [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
