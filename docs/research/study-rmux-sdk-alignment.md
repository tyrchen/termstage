# Study: rmux SDK Alignment for Termstage Backend Gateway

Status: Done · Owner: termstage · Date: 2026-05-28 · Vendor pin: `vendors/rmux` @ `6301d12b7db85ebeea2277a19e43bda8675622a9`

## Why this study

Termstage is moving from an owned command PTY model to a backend-owned session
model. rmux is the preferred future backend because it is daemon-backed,
scriptable, inspectable, and exposes a public SDK over the same daemon used by
the CLI. This memo checks whether the current `SessionGateway`, semantic API,
and backend adapter contract align with rmux's real SDK and protocol shape.

## Architecture Map

```text
Termstage today                         rmux current public surface
──────────────────────────────────      ──────────────────────────────────

Browser xterm.js / Agent API            rmux CLI / rmux-sdk / ratatui-rmux
        │                                      │
        │ HTTP + WebSocket                     │ typed async handles
        ▼                                      ▼
SessionGateway                         Rmux facade
- registry                             - EnsureSession
- Level 1 operation lock               - Session / Window / Pane handles
- BackendAdapter trait                 - Pane streams / snapshot / waits
        │                                      │
        │ BackendAdapter methods               │ rmux-proto Request/Response
        ▼                                      ▼
TmuxBackend today                      rmux daemon
- shell out to tmux                    - owns panes, PTYs, screen state
- capture-pane polling                 - local IPC transport
- send-keys                            - native attach and SDK control
```

## Findings

1. The backend-owned session model is aligned.

rmux's README describes a daemon-backed SDK, persistent sessions, structured
snapshots, and native local transports. Its workspace splits CLI, SDK, proto,
client, server, PTY, and ratatui integration into separate crates
(`vendors/rmux/README.md:124`, `vendors/rmux/README.md:158`). Termstage's
`BackendAdapter` already models rmux/tmux as owners of real sessions and panes
rather than letting termstage own a command PTY
(`crates/core/src/backend.rs:1`).

2. Session creation maps cleanly to rmux `EnsureSession`.

rmux exposes `EnsureSessionPolicy::{CreateOnly, CreateOrReuse, ReuseOnly}` and
a builder that captures session name, detached state, size, process, and timeout
(`vendors/rmux/crates/rmux-sdk/src/ensure.rs:16`,
`vendors/rmux/crates/rmux-sdk/src/ensure.rs:31`). Termstage's
`SessionGateway::create_or_find_session` maps a termstage session id to a
backend session reference and calls `BackendAdapter::create_or_find_session`
with an initial terminal size (`crates/core/src/session_gateway.rs:57`).

3. Pane input partially aligns, but the abstraction is too byte-oriented for
rmux.

rmux separates literal text from key tokens: `Pane::send_text` sends UTF-8 text
without interpretation, while `Pane::send_key` sends one tmux-compatible key
token (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:418`,
`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:428`). Its wire request also
preserves `keys: Vec<String>` plus a `literal` flag
(`vendors/rmux/crates/rmux-proto/src/request/pane.rs:309`). Termstage now keeps
raw `BackendAdapter::write_input(Bytes)` for browser byte streams and exposes
backend-native `send_text`, `send_key`, and `run_command` for semantic
operations. That matches the rmux split while preserving tmux compatibility.

4. Screen reads are under-modelled for rmux.

rmux `PaneSnapshot` is a structured grid with cells, attributes, colors, cursor,
and revision, captured directly from the daemon's screen state
(`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:381`,
`vendors/rmux/crates/rmux-proto/src/response/pane.rs:282`). Termstage's
`BackendScreenSnapshot` keeps only size, cursor coordinates, and text lines
(`crates/core/src/backend.rs:123`). That is enough for tmux `capture-pane`, but
it loses rmux's style, cursor visibility/style, wide-glyph padding, hyperlink,
and revision semantics.

5. Browser output should use rmux streams, not snapshot polling.

rmux exposes `Pane::output_stream`, `line_stream`, and `render_stream`. The raw
stream preserves arbitrary bytes, carries per-pane sequence numbers, and reports
lag (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:247`). The protocol also
has `SubscribePaneOutput`, `PaneOutputCursor`, lag notices, and batch limits
(`vendors/rmux/crates/rmux-proto/src/request/pane.rs:445`,
`vendors/rmux/crates/rmux-proto/src/response/pane.rs:196`). Termstage's current
tmux gateway polls `read_screen` every 250 ms and serializes a text snapshot to
the browser. That is acceptable for the tmux compatibility adapter, but it is
not the rmux-native browser bridge.

6. Semantic request/response should include waits and captured results.

rmux has daemon-backed waits for output bytes, visible-text polling over fresh
snapshots, armed waits for future output, and wait-for-exit helpers
(`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:183`,
`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:202`,
`vendors/rmux/crates/rmux-sdk/src/wait.rs:31`). Termstage's `run-command`
semantic endpoint now accepts `waitFor`, `waitTimeoutMs`, and `capture`, then
returns `matched` and an optional captured screen. The tmux implementation uses
visible-screen polling; a future rmux backend should map the same request shape
to rmux output waits and structured snapshots.

7. Termstage Level 1 operation lock is not the same as rmux session leases.

rmux has app-owned session leases that keep a session alive while renewed, and
kill the session when the lease expires (`vendors/rmux/crates/rmux-proto/src/request/session.rs:244`,
`vendors/rmux/crates/rmux-server/src/handler_session/leases.rs:20`). rmux also
has lock-server/session/client behavior for attached clients
(`vendors/rmux/crates/rmux-server/src/handler_lock.rs:36`). These are not the
same as termstage's one-writer operation lock. Termstage should keep Level 1
ownership in `SessionGateway` and treat rmux session leases as lifecycle
ownership, not as input ownership.

## Decision

GO-with-amendments.

The current architecture direction is compatible with rmux, but the backend
contract should be amended before implementing `RmuxBackend`:

- keep `SessionGateway` and termstage-managed Level 1 operation locks;
- keep backend-native operations for `send_text`, `send_key`, and
  `run_command`, and map future rmux support to `wait_for_text` and
  structured capture primitives;
- extend screen snapshots to support structured cells and revisions, or add a
  separate `BackendStructuredSnapshot` path for rmux;
- add an event/stream capability to `BackendAdapter` so the browser can consume
  rmux output/render streams rather than polling snapshots;
- represent backend references with stable pane identity where available
  (`PaneId` for rmux), while keeping slot-based session/window/pane references
  for tmux compatibility;
- do not map termstage's operation lock to rmux session leases. Use rmux leases
  later for lifecycle cleanup of app-owned sessions.

## What We Will Adopt

- rmux `EnsureSession` as the model for create/reuse behavior.
- rmux `Pane::send_text` and `Pane::send_key` as the semantic input model.
- rmux `PaneSnapshot` as the richer target for screen reads.
- rmux `Pane::output_stream` or `Pane::render_stream` as the browser sync path.
- rmux `wait_for_text`, `wait_for_text_next`, and `wait_exit` as the basis for
  semantic request/response operations.

## What We Will Avoid

- Do not force rmux through `capture-pane`-style text reconstruction.
- Do not collapse key tokens into hardcoded ANSI byte mappings for rmux.
- Do not use rmux session leases as a substitute for termstage's write
  controller lock.
- Do not make browser synchronization depend on fixed-rate polling when the
  backend can emit stream updates.

## Open Questions

- `spike-rmux-backend-adapter.md`: implement a tiny `RmuxBackend` prototype using
  `rmux-sdk` to ensure a session, send text, read a structured snapshot, and
  subscribe to output.
- `spike-rmux-browser-stream.md`: decide whether the browser bridge should feed
  xterm.js with raw output bytes, structured render snapshots, or both.
- `spike-rmux-lifecycle-lease.md`: decide how termstage should use rmux
  app-owned session leases for cleanup without affecting input ownership.
