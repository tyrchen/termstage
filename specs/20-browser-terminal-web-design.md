# 20-browser-terminal-web: Server and Browser UI

Status: draft v2
Owner: termstage
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
[11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md)

## 1. Purpose

The web layer owns the local Axum server, WebSocket upgrade path, bundled static
assets, and browser terminal UI. It does not parse shell commands or own PTY
state.

The browser UI is an embedded terminal component inside a page, not the entire
page. Today the page has a toolbar and one xterm surface; future versions may
add buttons, side panels, status views, or other HTML around the terminal. The
xterm instance must fit the container element assigned to it, and scrolling or
viewport gestures must be scoped to that terminal container rather than the
document body.

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
| `resize.ts` | Fit-to-container measurement and debounced viewport-size control messages. |
| `presentation.ts` | Presentation mode settings: font size, theme, cursor, copy/paste behavior. |

## 2a. Flow

```text
+--------------------- selected exposure policy ----------------------+
|                                                                     |
|  Browser page                                                       |
|  +---------------------------------------------------------------+  |
|  | page HTML                                                     |  |
|  | +----------------------+                                      |  |
|  | | toolbar / controls   |                                      |  |
|  | +----------------------+                                      |  |
|  | +---------------------------------------------------------+   |  |
|  | | terminal container                                      |   |  |
|  | | - owns scroll/viewport gestures                         |   |  |
|  | | - xterm fits this element, not the whole page           |   |  |
|  | | +--------------------+     binary/text WS  +----------+ |   |  |
|  | | | xterm.js frontend  | <-----------------> | /ws      | |   |  |
|  | | | - @xterm/xterm     |                     | Axum     | |   |  |
|  | | | - fit addon        |                     +----+-----+ |   |  |
|  | | | - web-links addon  |                          |       |   |  |
|  | | +--------------------+                          |       |   |  |
|  | +--------------------------------------------------|-------+   |  |
|  +----------------------------------------------------|----------+  |
|                                                          v          |
|                                                  +---------------+  |
|                                                  | Runtime or    |  |
|                                                  | Gateway state |  |
|                                                  +-------+-------+  |
|                                                          |          |
+----------------------------------------------------------+----------+
                                                           |
                                                           v
                                                   local shell/tmux/backend
```

## 2b. Embedded Size Model

The web UI keeps three size concepts separate:

```text
Browser page
┌──────────────────────────────────────────────────────────────┐
│ toolbar / future buttons / future panels                     │
├──────────────────────────────────────────────────────────────┤
│ terminal container                                           │
│                                                              │
│  visible viewport from fit addon: 100x30                     │
│  ┌────────────────────────────────────────────────────────┐  │
│  │ xterm instance: 100x30, fills this container           │  │
│  │                                                        │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                              │
│  component-local scroll gestures may adjust a logical        │
│  backend viewport; the document body must not scroll.        │
└──────────────────────────────────────────────────────────────┘

Backend session
┌──────────────────────────────────────────────────────────────┐
│ tmux/rmux pane screen: 188x52                                │
│ - owned by backend                                           │
│ - may be viewed through native attach                        │
│ - not resized by browser container changes                   │
└──────────────────────────────────────────────────────────────┘
```

Rules:

- The xterm DOM and xterm `cols`/`rows` fit the terminal container. They do not
  expand to the backend pane size merely because the backend screen is wider or
  taller.
- Browser `Resize` control messages describe the embedded terminal viewport,
  not necessarily a backend pane resize request.
- In runtime-owned shell/PTY mode, the server may apply viewport resize to the
  runtime PTY because the browser owns that PTY presentation.
- In backend-owned gateway mode, browser viewport changes do not resize the
  backend pane. The gateway projects the backend screen snapshot into the
  browser viewport size.
- Backend snapshot projection must be explicit: the server selects visible rows
  and columns from the backend screen according to viewport state, then writes a
  frame that fits the xterm instance. It must not rely on xterm overflow,
  autowrap suppression, or accidental clipping to hide content.
- The initial projection origin is top-left: column `0`, row `0`. A backend
  cursor near the right or bottom edge must not cause first attach to crop
  left-side labels or full-screen headers such as k9s `Context:`.
- Horizontal and vertical navigation over a larger backend screen is a
  component-local terminal viewport concern. The browser sends viewport-origin
  control frames for this navigation, and future buttons or page content must
  not change this contract.

## 3. Invariants

- The server listens on `127.0.0.1` by default. `::1` support is allowed after tests
  cover IPv6 loopback handling. Non-loopback binds require explicit public exposure
  mode from [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md).
- Static assets are bundled with the binary or served from a trusted local build
  directory in development; production UI never loads CDN JavaScript.
- The WebSocket upgrade validates token, Host, Origin, and peer IP before calling
  `on_upgrade`.
- WebSocket max frame size and max message size are explicitly configured.
- Browser UI sends terminal input as binary frames and resize as JSON text frames.
- UI text does not explain implementation details; the screen is the usable terminal.
- The browser terminal is embeddable: adding page-level buttons or panels cannot
  require changing the terminal protocol or backend sizing semantics.
- In backend-owned gateway mode, xterm size equals browser container fit size,
  while backend screen size remains backend-owned. Rendering uses a deterministic
  snapshot projection from backend screen space to browser viewport space.

## 4. Behavior

The server starts on port `0` unless the CLI provides a loopback port. After binding,
it builds a tokenized URL and optionally opens the browser. The URL token is exchanged
for an in-memory token check; it is not persisted.

When public exposure mode is enabled by
[21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md),
the server binds the requested pod address but builds launch URLs from the validated
public base URL. Host and Origin validation use that same public URL instead of the
pod socket address, and non-loopback peers are accepted because ingress traffic enters
the pod as ordinary TCP peers.

The frontend uses `@xterm/xterm`, `@xterm/addon-fit`, and
`@xterm/addon-web-links`. The fit addon measures the terminal container, not the
full browser page. The WebGL addon is optional and guarded behind a frontend
feature because rendering fallback must work on presentation machines without
GPU acceleration.

The WebSocket handler splits send and receive halves. Receive frames map to
`RuntimeCommand::Input` or validated `RuntimeCommand::Resize`. Runtime output is sent
as binary frames to the browser. Any protocol or security error closes the socket and
emits a redacted structured log event.

For backend-owned gateway sessions, receive frames map through the backend
adapter and operation lock instead of the runtime PTY actor. Browser resize
frames update only the browser viewport size in gateway state. Backend screen
snapshots are read with backend-native screen dimensions and projected into the
browser viewport before being sent as terminal bytes.

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
  [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md),
  [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
