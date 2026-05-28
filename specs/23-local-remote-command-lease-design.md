# Session Backend Gateway and Operation Lease Design

Status: redesign draft v4
Last updated: 2026-05-27

## 1. Problem

The previous local attach direction modeled the shared command
terminal as a PTY owned directly by `termstage`, then tried to mirror that PTY
into both the invoking terminal and the browser. That model mixed two
responsibilities:

- `termstage` as supervisor, web gateway, auth boundary, and session registry;
- the actual terminal multiplexer/session backend that owns panes, PTYs,
  screen state, and native local attach.

That is the wrong long-term boundary. Local viewing should not require
`termstage` to render command bytes into its own stdout. A user should be able to
attach locally with the backend's native tool, for example `rmux attach -t abc`
or `tmux attach -t abc`, while browser and API clients reach the same backend
session through `termstage`.

## 2. Recommendation

Model a `termstage` session as a reference to a backend session/pane, not as a
local command PTY owned by `termstage`.

```text
Browser xterm.js / Agent API
        │
        │ Termstage Protocol
        ▼
┌─────────────────────────────────────────────────────────────┐
│ termstage                                                   │
│                                                             │
│  ┌────────────────────┐   ┌──────────────────────────────┐  │
│  │ session registry   │   │ browser WebSocket gateway    │  │
│  │ - termstage id     │   │ - token/auth boundary        │  │
│  │ - backend ref      │   │ - embedded xterm bridge      │  │
│  │                    │   │ - viewport projection        │  │
│  └─────────┬──────────┘   └──────────────┬───────────────┘  │
│            │                             │                  │
│  ┌─────────▼──────────┐   ┌──────────────▼───────────────┐  │
│  │ semantic API       │   │ input lease / operation lock │  │
│  │ gateway            │   │ - one writer at a time       │  │
│  │ - press-key        │   │ - other clients read-only    │  │
│  │ - run-command      │   │ - bounded lease timeout      │  │
│  │ - read-screen      │   └──────────────┬───────────────┘  │
│  └─────────┬──────────┘                  │                  │
│            │                 ┌───────────▼──────────────┐   │
│            └────────────────▶│ backend adapter          │   │
│                              │ - rmux first             │   │
│                              │ - tmux/future backends   │   │
│                              └───────────┬──────────────┘   │
└──────────────────────────────────────────┼──────────────────┘
                                           │ backend-native API
                                           ▼
┌─────────────────────────────────────────────────────────────┐
│ rmux / tmux / future backend                                │
│ - owns actual session/window/pane/PTY                       │
│ - owns terminal screen state                                │
│ - supports native local attach                              │
└─────────────────────────────────────────────────────────────┘
```

Concrete default model:

```text
termstage session "abc"
        │ registry entry
        ▼
rmux session "abc" / window / pane
        │ native attach
        ├── local operator: rmux attach -t abc
        │
        │ gateway attach
        └── browser: browser -> termstage -> rmux API -> pane
```

`termstage` still remains the command entry point and policy owner. It creates or
locates backend sessions, exposes browser and API gateways, enforces auth and
input ownership, and forwards terminal operations to the backend adapter.

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Remove the old local attach model. | No CLI flag, module, runtime command, or docs reference the obsolete local attach feature. |
| G2 | Keep `termstage`'s invoking terminal as a supervisor surface. | Local stdout/stderr show logs, URL, health, and errors only. |
| G3 | Make backend sessions the source of truth. | Session state points to backend session/window/pane ids; backend owns the PTY. |
| G4 | Support browser and API control through one protocol boundary. | Browser xterm.js and Agent API clients use Termstage Protocol over their transports. |
| G5 | Enforce one writer at a time in Level 1. | `termstage` tracks the active controller and rejects or queues conflicting write operations. |
| G6 | Leave backend-enforced locks for Level 2. | Backend-native lock integration is deferred until the backend adapter supports it. |

## 4. Non-goals

- Do not implement a split local TUI inside `termstage`.
- Do not mirror the command PTY into `termstage`'s own stdout/stderr.
- Do not make `termstage` attach to a `/dev/tty` device owned by tmux/rmux.
- Do not split the embedded web server into a standalone process in this spec.
- Do not implement OIDC/Okta in this spec; the gateway/auth boundary must remain
  compatible with a future OIDC layer.

## 5. Protocol Layers

The protocol has three conceptual layers. The first implementation can keep them
inside one process and one crate boundary; the separation is still part of the
contract so web, API, and backend adapters do not grow direct dependencies on
each other.

```text
┌────────────────────────────────────────────────────────────┐
│ Layer 3: Semantic Operations                              │
│ - press-key                                               │
│ - write-text                                              │
│ - run-command                                             │
│ - read-screen                                             │
│ - scroll                                                  │
│ - acquire/release operation lock                          │
└──────────────────────────────┬─────────────────────────────┘
                               │ maps to
┌──────────────────────────────▼─────────────────────────────┐
│ Layer 2: Terminal Byte Stream                              │
│ - VT/ANSI output bytes                                     │
│ - input bytes                                              │
│ - browser viewport resize                                  │
│ - explicit backend resize operation                        │
│ - replay                                                   │
│ - close/error/control frames                               │
└──────────────────────────────┬─────────────────────────────┘
                               │ carried by
┌──────────────────────────────▼─────────────────────────────┐
│ Layer 1: Transport                                          │
│ - browser WebSocket                                         │
│ - future TCP/gRPC/QUIC                                      │
│ - in-process channels for embedded mode                     │
└────────────────────────────────────────────────────────────┘
```

Layer 2 must faithfully carry the terminal byte stream:

- text and UTF-8 bytes;
- cursor movement, colors, and alternate screen;
- mouse protocols;
- terminal query/response traffic;
- title, hyperlink, and clipboard OSC sequences;
- browser viewport resize, explicit backend resize, and close events.
- browser viewport origin updates for component-local navigation across a larger
  backend screen.

Browser viewport resize is not the same operation as resizing a backend-owned
pane. A browser xterm is embedded inside a page container and must fit that
container. A backend pane is owned by rmux/tmux/future backends and may have a
different screen size because native local attach clients and backend policies
control it.

Layer 3 adds request/response semantics for agents. Example operations:

- `pressKey(session, key = "h")`;
- `writeText(session, text)`;
- `runCommand(session, command = "echo hello", waitFor = "hello", capture = true)
  -> { matched, screen }`;
- `readScreen(session) -> screen snapshot`;
- `scroll(session, direction, amount)`;
- `acquireLock(session, controller, ttl)`.

Semantic operations must be observable by browser and backend viewers because
they ultimately mutate the same backend session/pane.

`runCommand` is a request/response operation, not just an input write. The first
implementation submits the command through backend-native command entry, then
optionally waits for visible screen text and optionally returns a captured screen
snapshot. Backends with stronger primitives, such as rmux, should implement this
with `send_text`, `send_key`, output waits, and structured snapshots instead of
forcing everything through raw bytes.

## 6. Level 1 Lock

Level 1 lock is managed by `termstage`.

```text
Controller A              termstage lock table              Backend pane
     │                            │                              │
     │ 1. acquire(session) ──────▶│                              │
     │                            │ 2. grant lease               │
     │◀───────────────────────────│                              │
     │                            │                              │
     │ 3. input bytes / op ──────▶│ 4. validate owner            │
     │                            │ 5. forward operation ───────▶│
     │                            │                              │
Controller B              termstage lock table              Backend pane
     │                            │                              │
     │ 6. input bytes / op ──────▶│                              │
     │                            │ 7. reject read-only/conflict │
     │◀───────────────────────────│                              │
```

Rules:

- A session has at most one write controller.
- Browser xterm.js, Agent API clients, and future transports are all controllers.
- Non-owners may read screen/output but cannot write input.
- Lock state includes owner id, owner kind, epoch, and expiration time.
- Expired locks can be reclaimed by another controller.
- Ctrl-C and other write operations require ownership.

## 7. Level 2 Deferred Lock

Level 2 is backend-enforced locking. It is intentionally deferred.

Examples:

- rmux or tmux exposes an explicit pane/session write lock;
- backend rejects writes from non-owner clients;
- backend emits authoritative lease changes.

`termstage` must keep the Level 1 lock boundary explicit so Level 2 can replace
or reinforce it without changing browser/API semantics.

## 8. Backend Adapter Contract

The backend adapter hides rmux/tmux/future-specific APIs behind a small
session-oriented interface.

Required capabilities:

- create or find a backend session by validated name;
- resolve a session to window/pane identity;
- stream VT/ANSI output bytes to `termstage`;
- write input bytes to the pane;
- send literal text, key tokens, and submitted commands as backend-native
  semantic operations;
- resize the pane only for explicit backend resize operations or backend modes
  where the controller truly owns the pane size;
- read a screen snapshot for semantic API operations;
- close, detach, or keep the backend session according to exit policy.

The first backend should prefer rmux if its API exposes screen read/write
semantics cleanly. tmux remains a compatibility backend because native local
attach is well understood.

## 8a. Browser Viewport Projection

Browser xterm.js is a component embedded inside the web page. Its dimensions are
derived from its container element through the fit addon. Those dimensions are a
browser viewport, not the backend screen size.

```text
Browser terminal component                  termstage gateway
┌──────────────────────────────┐            ┌──────────────────────────┐
│ page                         │            │ gateway session state    │
│ ┌──────────────────────────┐ │            │ - backend ref            │
│ │ toolbar / future HTML    │ │            │ - operation lock         │
│ └──────────────────────────┘ │            │ - browser viewport       │
│ ┌──────────────────────────┐ │ resize --->│   cols/rows + origin     │
│ │ terminal container       │ │            └────────────┬─────────────┘
│ │ xterm fit: 100x30       │ │                         │ read screen
│ └──────────────────────────┘ │                         ▼
└──────────────────────────────┘            ┌──────────────────────────┐
                                            │ backend pane screen      │
                                            │ e.g. tmux 188x52        │
                                            └────────────┬─────────────┘
                                                         │ project
                                                         ▼
                                            xterm-sized frame 100x30
```

Projection rules:

- The gateway reads backend screen size, cursor, lines, and attributes from the
  backend adapter.
- The gateway preserves backend cursor visibility when projecting snapshots.
- The gateway maintains browser viewport state: visible `cols`, visible `rows`,
  horizontal origin, and vertical origin. The initial origin is top-left:
  column `0`, row `0`. This preserves full-screen application headers such as
  k9s `Context:` on first attach. The origin changes only after explicit
  component-local viewport navigation.
- The binary frame sent to browser xterm must fit the browser viewport. For a
  backend screen wider than the browser viewport, the frame contains the
  selected horizontal slice rather than the entire backend line.
- Each projected row starts from a reset SGR state before applying that row's
  own ANSI attributes, avoiding highlight/color leakage across row boundaries.
- Browser input coordinates and mouse events are translated through the current
  viewport origin before reaching the backend.
- Component-local scroll, trackpad, keyboard navigation, or future toolbar
  controls may update the viewport origin. They must not scroll the document
  body or require xterm to grow to backend size.
- Runtime-owned PTY mode ignores viewport-origin control frames because there is
  no separate backend screen to project.
- Browser viewport changes never resize a backend-owned pane. Native local
  attach clients remain governed by the backend's size policy.

## 9. Migration Plan

### Phase 1 - Remove Local Attach

Remove the incorrect local attach implementation:

- delete the obsolete local attach flag and any replacement split-TUI flag;
- delete the server-side local terminal passthrough module;
- remove runtime commands that attach or write from a local terminal frontend;
- remove tests and docs that describe local terminal ownership;
- keep shell mode browser-first until the backend gateway lands.

Exit criteria:

- repository search finds no obsolete local attach symbols;
- CLI rejects `-a` because it is no longer defined;
- shell mode with `--command` still starts the command for browser access;
- standard Rust quality gates pass, except environment-specific tmux tests may
  remain skipped or documented if tmux is unavailable.

### Phase 2 - Implement Session Backend Gateway

Implement the new architecture:

- add session registry keyed by termstage session id;
- add backend adapter trait and rmux/tmux implementation path;
- route browser WebSocket traffic through Termstage Protocol into the adapter;
- add semantic API gateway for request/response operations;
- add Level 1 operation lock with owner/epoch/TTL;
- update browser toolbar from `control by terminal/browser` to controller-aware
  status;
- add tests for lock conflict, browser/API synchronization, backend attach, and
  semantic read/write operations.

Exit criteria:

- local attach uses backend-native command such as `rmux attach -t <session>`;
- browser and Agent API can operate the same backend pane;
- only the active controller can write;
- other viewers remain read-only and receive output/screen updates;
- `termstage` local terminal prints only supervisor information.

## 10. Cross-references

- Extends [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md)
  with semantic operations and transport-independent terminal frames.
- Refines [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md)
  for backend-owned gateway viewport projection.
- Replaces the local-terminal ownership portions of
  [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md).
- Updates [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md)
  by removing the local attach flag.
- Must be verified through [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
