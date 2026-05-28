# Study: rmux SDK Alignment for Termstage Semantic Operations

Status: Done · Owner: termstage · Date: 2026-05-29 · Vendor pin: `vendors/rmux` @ `6301d12b7db85ebeea2277a19e43bda8675622a9`

## Why This Study

PR #6 proposes `specs/25-rmux-backend-adapter-design.md`: an rmux-native backend
adapter for the termstage backend-session gateway. The question this memo
answers is narrower than the spec:

> How should rmux be adapted into termstage's Semantic Operations API, and which
> operations should remain termstage-only wrappers rather than direct rmux API
> exposure?

Termstage already has the right high-level spine: `BackendAdapter` defines a
backend-owned session boundary (`crates/core/src/backend.rs:274`), and
`SessionGateway` owns the registry plus the Level 1 write lock before any
browser or Agent API write reaches a backend (`crates/core/src/session_gateway.rs:66`).
The Semantic API currently exposes controller lock, press-key, write-text,
run-command, read-screen, and scroll routes (`apps/server/src/web.rs:658`,
`apps/server/src/web.rs:700`, `apps/server/src/web.rs:714`,
`apps/server/src/web.rs:728`, `apps/server/src/web.rs:770`,
`apps/server/src/web.rs:789`). The rmux adapter must fit this shape first.

## Architecture Map

```text
                          termstage public control surface
┌──────────────────────────────────────────────────────────────────────────────┐
│ Browser xterm.js WebSocket                       Agent Semantic HTTP API     │
│ - raw-ish terminal input                         - acquire/release lock      │
│ - viewport updates                               - press-key / write-text    │
│ - output rendering                               - run-command / read-screen │
└──────────────────────────────┬───────────────────────────────────────────────┘
                               │ Termstage Protocol + controller id
                               ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ termstage SessionGateway                                                     │
│ - maps termstage session id -> backend session/window/pane ref               │
│ - enforces Level 1 one-writer lock                                           │
│ - normalizes errors and API response shapes                                  │
└──────────────────────────────┬───────────────────────────────────────────────┘
                               │ BackendAdapter calls
                               ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ RmuxBackend                                                                  │
│ - owns rmux-sdk facade and per-session bindings                              │
│ - resolves stable PaneId                                                     │
│ - maps semantic writes to Pane methods                                       │
│ - maps snapshots/streams/waits into termstage-neutral events and responses   │
└──────────────────────────────┬───────────────────────────────────────────────┘
                               │ rmux-sdk local IPC
                               ▼
┌──────────────────────────────────────────────────────────────────────────────┐
│ rmux daemon                                                                  │
│ - owns sessions/windows/panes/PTYs                                           │
│ - owns parser, screen grid, output retention, and native attach              │
│ - exposes SDK: EnsureSession, Pane::send_text/key, snapshot, streams, waits  │
└──────────────────────────────────────────────────────────────────────────────┘
```

Local viewing remains backend-native. A local user should attach with rmux
directly, while browser and API control goes through termstage:

```text
rmux attach -t abc        browser/API
        │                     │
        │ native attach       │ Termstage Protocol
        ▼                     ▼
     rmux daemon ◀────── RmuxBackend
        │
        ▼
   session abc / pane %N
```

## Semantic Operation Mapping

The key rule is: use rmux SDK primitives directly inside `RmuxBackend`, but keep
termstage's public Semantic API backend-neutral. rmux types should not leak into
browser or Agent API JSON.

| Termstage semantic operation | rmux SDK primitive | Adapter decision |
| --- | --- | --- |
| `session create` / `session attach --browser` resolution | `Rmux::connect_or_start`, `EnsureSession`, `Rmux::session` | Direct SDK usage. Use create/reuse for termstage-created sessions and reuse-only for attaching an existing rmux session. |
| Stable pane binding | `Session::pane(0, 0)`, `Pane::id`, `Session::pane_by_id` | Resolve the initial slot, then store stable `PaneId` when available. |
| `writeText` | `Pane::send_text` | Direct SDK usage after termstage lock validation. No implicit newline. |
| `pressKey` | `Pane::send_key` or `Pane::keyboard().press` | Direct SDK usage. Prefer termstage's own allowed key-token grammar at the API boundary; use rmux normalization only inside adapter tests or higher-level helper paths. |
| Browser input bytes | Currently `BackendAdapter::write_input(Bytes)` | rmux's studied pane input is text/key oriented, not arbitrary byte oriented. Preserve valid UTF-8 as `send_text`; reject invalid bytes with `BackendError::UnsupportedInput` until rmux exposes byte input. |
| `runCommand` submit | `send_text(command)` then `send_key("Enter")` | Composed termstage operation. rmux does not expose one method with controller id, wait/capture, timeout, and response envelope. |
| `runCommand` wait/capture | `wait_for_text_next`, `wait_for_text`, `expect_visible_text`, `snapshot` | Backend-native request/response implementation for rmux; gateway polling is only a fallback. |
| `readScreen` | `Pane::snapshot` | Direct SDK usage, projected to existing text response first; preserve structured cells in a new/extended model before rmux becomes default. |
| Browser output sync | `Pane::output_stream` or `Pane::render_stream` | Prefer stream/event bridge over fixed-rate `read_screen` polling. |
| `scroll` | No first-class `Pane::scroll` in studied SDK surface | Gap. Implement either as backend-specific copy-mode key composition or define it as browser viewport-only until rmux exposes a scroll primitive. |
| One-writer operation lock | termstage `OperationLockTable` | termstage-only Level 1 control. rmux leases/locks are not a replacement. |
| Toolbar controller status | No rmux equivalent | termstage-only session gateway state. |
| Auth, token, future OIDC | No rmux equivalent | termstage-only web gateway responsibility. |

## Hot Path Walkthrough

### Session Creation and Attachment

rmux exposes a daemon-backed facade. `Rmux::connect_or_start` connects to the
default daemon and starts a hidden daemon if none is reachable
(`vendors/rmux/crates/rmux-sdk/src/handles/rmux.rs:58`). The README positions
the CLI, SDK, and ratatui widget as three public surfaces over one local daemon
protocol (`vendors/rmux/README.md:184`).

Session creation/reuse maps cleanly to `EnsureSession`. The builder carries the
session name, detached flag, initial size, process, policy, tags, and timeout
(`vendors/rmux/crates/rmux-sdk/src/ensure.rs:31`). Its policies distinguish
`CreateOnly`, `CreateOrReuse`, and `ReuseOnly` (`vendors/rmux/crates/rmux-sdk/src/ensure.rs:16`),
and it supports explicit argv and shell launch modes for the initial pane
(`vendors/rmux/crates/rmux-sdk/src/ensure.rs:163`,
`vendors/rmux/crates/rmux-sdk/src/ensure.rs:177`).

Termstage's `create_or_find_session` already accepts a termstage session id, a
backend session name, and an initial size, then stores the returned
`BackendSessionRef` in the registry (`crates/core/src/session_gateway.rs:97`).
For rmux, PR #6 should keep that API but make `RmuxBackend` choose:

- create path: `EnsureSession::named(name).create_or_reuse().detached(true)`;
- attach path: `Rmux::session(name)` or `EnsureSession::named(name).reuse_only()`;
- command path: `EnsureSession::argv(args)` for argv mode or `.shell(text)` for
  explicit shell-text mode.

After session resolution, rmux allows slot handles and stable pane-id handles.
`Session::pane(window_index, pane_index)` resolves the exact slot lazily
(`vendors/rmux/crates/rmux-sdk/src/handles/session.rs:119`), while
`Session::pane_by_id` returns a handle whose critical operations use stable
pane-id targeting (`vendors/rmux/crates/rmux-sdk/src/handles/session.rs:135`).
`Pane::id` reports the live daemon pane identity or `None` for a stale slot
(`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:333`). `RmuxBackend` should
store stable `PaneId` in its binding when available and convert it to the
string-backed `BackendPaneId`.

### Input: Browser Bytes and Semantic Keys

Termstage currently separates raw input bytes from semantic text/key operations
at the adapter boundary (`crates/core/src/backend.rs:292`,
`crates/core/src/backend.rs:303`, `crates/core/src/backend.rs:314`).
`SessionGateway` validates the controller lease before forwarding each write
(`crates/core/src/session_gateway.rs:183`, `crates/core/src/session_gateway.rs:202`,
`crates/core/src/session_gateway.rs:221`).

rmux's pane API also separates literal text from interpreted key tokens.
`Pane::send_text` sends UTF-8 text without key-name interpretation or implicit
newline (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:418`).
`Pane::send_key` sends one tmux-compatible key token
(`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:428`). Under the hood, stable
pane ids use `PaneInputRequest { keys, literal }`; literal text sets
`literal: true`, while key tokens set `literal: false`
(`vendors/rmux/crates/rmux-sdk/src/handles/pane/input.rs:10`,
`vendors/rmux/crates/rmux-sdk/src/handles/pane/input.rs:43`).

This is a strong alignment point. PR #6 should not invent key encoding inside
termstage for rmux. The adapter should call rmux SDK methods directly after the
gateway has validated the Level 1 lock. The only mismatch is
`write_input(Bytes)`: rmux's studied SDK surface does not expose arbitrary byte
input into a pane. The safe first implementation is UTF-8 checked text input,
returning `UnsupportedInput` for non-UTF-8 browser bytes.

rmux also has higher-level keyboard and mouse helpers. `Pane::keyboard().press`
normalizes common Playwright-style spellings such as `Control+C` and `Ctrl+C`
to tmux-style key tokens (`vendors/rmux/crates/rmux-sdk/src/actions.rs:21`).
`Pane::mouse().click` injects SGR mouse reports as text into the pane
(`vendors/rmux/crates/rmux-sdk/src/actions.rs:77`). These are useful reference
semantics for future Agent API operations, but the first rmux adapter should
keep termstage's public key grammar explicit and stable.

### `runCommand` Request/Response

Termstage's current HTTP endpoint makes `run-command` a request/response
operation: it submits command text, optionally waits for a target text, and
optionally returns a captured screen (`apps/server/src/web.rs:728`). The
existing implementation submits first, then uses gateway-level visible-screen
polling for `waitFor` and `capture` (`apps/server/src/web.rs:742`,
`apps/server/src/web.rs:750`).

rmux does not expose a single `run_command(controller_id, wait_for, capture)`
method. It provides better building blocks:

```text
Agent API runCommand(command, waitFor, capture)
        │
        │ 1. termstage validates controller lease
        ▼
RmuxBackend
        │
        │ 2. optional: arm future wait if waitFor means "after submit"
        │    Pane::wait_for_text_next(waitFor)
        │
        │ 3. Pane::send_text(command)
        │ 4. Pane::send_key("Enter")
        │
        │ 5. await future output wait or visible-screen wait
        │
        │ 6. Pane::snapshot() if capture=true
        ▼
RunCommandResponse { ok, matched, screen }
```

The distinction between wait types matters:

- `Pane::wait_for_text_next` arms a daemon-backed future-output wait; it only
  matches UTF-8 bytes emitted after the wait is armed
  (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:214`).
- `Pane::wait_for_text` polls fresh rendered snapshots and can match text that
  is already visible (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:202`).
- `Pane::expect_visible_text` is a builder over rendered snapshots that observes
  terminal effects after clears, wrapping, and redraws
  (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:223`).
- `wait_for_text_next` and raw future waits use a daemon wait record plus
  best-effort cancellation on drop (`vendors/rmux/crates/rmux-sdk/src/wait.rs:31`).
- visible text waits poll `Pane::snapshot` every 25 ms by default
  (`vendors/rmux/crates/rmux-sdk/src/wait.rs:29`,
  `vendors/rmux/crates/rmux-sdk/src/wait/visible.rs:162`).

Recommended PR #6 amendment: make the spec explicit that `run-command` should
eventually move below `SessionGateway` into a backend-native request/response
hook, not remain permanently implemented as web-layer polling. A compatible
trait shape is:

```text
BackendAdapter::run_command_request(target, request) -> RunCommandOutcome
```

where the default implementation composes `run_command` + `read_screen` polling
for tmux, and `RmuxBackend` overrides it with rmux waits and snapshots.

### Read Screen and Structured Snapshot

Termstage's current `BackendScreenSnapshot` is intentionally small: size,
cursor column/row/visibility, and text lines (`crates/core/src/backend.rs:156`).
That is enough for tmux `capture-pane` compatibility but not enough to preserve
rmux's screen model.

rmux snapshots are structured. The daemon response carries row-major cells,
cursor row/col/visibility/style, and a revision counter
(`vendors/rmux/crates/rmux-proto/src/response/pane.rs:242`,
`vendors/rmux/crates/rmux-proto/src/response/pane.rs:269`,
`vendors/rmux/crates/rmux-proto/src/response/pane.rs:282`). `Pane::snapshot`
documents that the grid is read from the daemon's live in-memory screen, not
reconstructed through text capture, preserving wide-glyph padding, colors,
attributes, cursor state, and revision changes
(`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:381`). The SDK conversion
validates the row-major cell shape before returning a `PaneSnapshot`
(`vendors/rmux/crates/rmux-sdk/src/handles/pane/snapshot.rs:51`).

PR #6 is correct to require a structured snapshot path. The compatibility order
should be:

1. map rmux `PaneSnapshot::visible_lines` or equivalent rendered text into
   current `ScreenResponse` so existing Agent API users work;
2. add `BackendStructuredSnapshot` or a snapshot enum so rmux cell/cursor/revision
   data is not permanently thrown away;
3. let browser render stay raw-byte/xterm-first until structured rendering is a
   separate deliberate UI feature.

### Browser Sync: Stream First, Polling as Fallback

Current tmux web gateway polls `read_screen` with idle and active intervals,
then serializes changed screen text to the browser (`apps/server/src/web.rs:946`,
`apps/server/src/web.rs:951`, `apps/server/src/web.rs:1015`). That is a tmux
compatibility strategy, not the rmux-native path.

rmux provides raw output streams. `Pane::output_stream` preserves arbitrary
bytes, pairs chunks with monotonic per-pane sequence numbers, and surfaces lag
notices without lossy UTF-8 conversion (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:247`).
The stream type exposes `Bytes { sequence, bytes }` and `Lag` chunks with
expected/resume/missed/newest sequence details and bounded recent output
(`vendors/rmux/crates/rmux-sdk/src/events/streams.rs:138`,
`vendors/rmux/crates/rmux-sdk/src/events/streams.rs:170`). The daemon protocol
also returns subscription id, stable pane id, cursor state, output events, and
explicit lag responses (`vendors/rmux/crates/rmux-proto/src/response/pane.rs:196`,
`vendors/rmux/crates/rmux-proto/src/response/pane.rs:218`,
`vendors/rmux/crates/rmux-proto/src/response/pane.rs:231`).

rmux also provides a render stream. It is not a daemon-native diff stream; it is
output-driven, debounced, captures a fresh snapshot, and suppresses unchanged
snapshot revisions (`vendors/rmux/crates/rmux-sdk/src/events/render.rs:37`).
That makes it useful for semantic/UI snapshots, while raw `output_stream` is the
better fit for xterm.js because it preserves the original VT/ANSI byte stream.

PR #6 should therefore add a backend event subscription API before making rmux
the browser path:

```text
BackendEventStream
  Output { bytes }
  Lag { missed_events, recent_bytes, resume_sequence }
  SnapshotInvalidated { revision }   optional
  Closed { message }
```

For rmux:

- browser xterm.js should consume `Output { bytes }`;
- lag should trigger a status/resync path, not unbounded replay;
- late browser-mode attach should prefer initial structured/text snapshot plus
  stream-from-`Now`, rather than replaying all retained output by default;
- `render_stream` can feed future screen-diff or Agent observation features.

### Scroll

This is the main semantic gap. Termstage exposes `scroll(session, direction,
amount)` and validates ownership before forwarding it to the backend
(`crates/core/src/session_gateway.rs:307`). The studied rmux SDK pane surface
does not expose a first-class `Pane::scroll` method. The protocol has copy-mode
requests and `SendKeysExtRequest` includes `copy_mode_command`
(`vendors/rmux/crates/rmux-proto/src/request/pane.rs:500`), and the server has
copy-mode command handling for cursor motion, selection, and search
(`vendors/rmux/crates/rmux-server/src/handler_copy_mode/input.rs:13`), but the
SDK does not currently wrap a pane-scroll semantic operation.

Recommended PR #6 amendment:

- classify `scroll` as **not directly covered by rmux SDK v0.3.1**;
- choose one first milestone behavior:
  - browser viewport-only scroll for browser history, not backend mutation; or
  - backend copy-mode scroll implemented as explicit copy-mode/key composition;
- do not pretend it is equivalent to raw terminal mouse wheel input.

The safer first milestone is viewport-only for browser scroll and no-op/error
for Agent `scroll` on rmux until a copy-mode contract is specified.

## Lock, Lease, and Ownership Boundaries

Termstage Level 1 write ownership is a browser/API coordination concept. The
gateway validates ownership before `write_input`, `send_text`, `send_key`,
`run_command`, `resize`, and `scroll` (`crates/core/src/session_gateway.rs:183`,
`crates/core/src/session_gateway.rs:202`, `crates/core/src/session_gateway.rs:221`,
`crates/core/src/session_gateway.rs:240`, `crates/core/src/session_gateway.rs:253`,
`crates/core/src/session_gateway.rs:307`).

rmux has different lifecycle and attach concepts:

- `OwnedSession` policies decide whether an SDK-owned session is killed,
  preserved, or killed when the owner stops renewing a daemon-side lease
  (`vendors/rmux/crates/rmux-sdk/src/handles/owned_session.rs:29`).
- The daemon session lease store tracks per-session lease tokens and deadlines,
  then reaps expired leased sessions by killing them
  (`vendors/rmux/crates/rmux-server/src/handler_session/leases.rs:20`,
  `vendors/rmux/crates/rmux-server/src/handler_session/leases.rs:176`).
- rmux lock-server/session/client behavior suspends attached clients and runs
  configured lock commands, which is user/session locking rather than an API
  write lease (`vendors/rmux/crates/rmux-server/src/handler_lock.rs:36`).

These should not be collapsed. PR #6 is correct to keep the termstage Level 1
lock in `SessionGateway` and treat rmux session leases as future lifecycle
cleanup only.

Level 2 remains future work: local `rmux attach -t <session>` can still mutate
the pane outside termstage's Level 1 lock. Browser/API observers should see the
resulting output, but termstage cannot enforce one-writer ownership against
native rmux clients until rmux exposes a backend-enforced input lease or
read-only attach mode that termstage can control.

## What We Will Adopt

- Use `rmux-sdk`, not shelling out to `rmux`, for the adapter.
- Use `Rmux::connect_or_start` plus `EnsureSession` for create/reuse.
- Use stable `PaneId` after initial slot resolution.
- Use `Pane::send_text` and `Pane::send_key` for semantic input.
- Compose `run-command` from text + Enter + rmux waits + snapshot capture.
- Use `Pane::snapshot` as the canonical rmux screen read source.
- Add a structured snapshot model before making rmux the default backend.
- Add a backend stream contract and feed browser xterm.js from
  `Pane::output_stream`.
- Keep termstage's Level 1 lock and public Semantic API backend-neutral.

## What We Will Avoid

- Do not force rmux through tmux-style shell commands or `capture-pane` text
  reconstruction.
- Do not expose rmux SDK DTOs directly in public HTTP/WebSocket protocol types.
- Do not map rmux app-owned session leases to termstage controller leases.
- Do not treat browser viewport resize as backend pane resize.
- Do not use `line_stream` for xterm.js output; it is lossy and line-oriented.
- Do not implement rmux `scroll` by sending arbitrary mouse-wheel bytes without
  a defined copy-mode or viewport semantic.
- Do not replay all retained rmux output on late browser-mode attach by default.

## Recommended PR #6 Amendments

1. **Make `scroll` an explicit gap.** The spec currently lists scroll as part of
   the shared adapter contract. It should state that rmux v0.3.1 has no direct
   `Pane::scroll` SDK primitive in the studied surface, and the first rmux
   milestone either returns `UnsupportedInput` for Agent scroll or defines
   viewport-only scroll.

2. **Add a backend-native `run-command` request/response hook.** Existing
   `BackendAdapter::run_command` submits only (`crates/core/src/backend.rs:322`).
   rmux can implement wait/capture better than the web-layer polling loop, so
   PR #6 should specify a future adapter method or outcome type that owns wait
   semantics.

3. **Treat `write_input(Bytes)` as UTF-8-limited for rmux.** The studied rmux
   input path is text/key based. PR #6 should require invalid UTF-8 rejection,
   not lossy conversion.

4. **Prefer raw output stream for browser xterm.js.** `render_stream` is useful
   for screen snapshots, but xterm.js should receive byte-faithful VT/ANSI
   output from `output_stream`.

5. **State the attach limitation.** Level 1 locks coordinate browser and Agent
   API writes. Native `rmux attach` writes remain outside enforcement until
   Level 2 backend-enforced locks exist.

6. **Keep structured snapshot out of the first browser renderer.** Add the core
   data model now, but do not replace xterm.js rendering with cell-grid rendering
   as part of the first rmux adapter milestone.

## Decision

GO-with-amendments.

rmux is a strong fit for termstage's Semantic Operations API because it already
has a daemon-backed SDK, stable pane addressing, literal text/key input,
structured snapshots, raw output streams, and output/text waits. The adapter
should be thin where rmux already has exact primitives and termstage-specific
where the operation includes controller ownership, public API response envelopes,
auth, or backend-neutral error/event normalization.

The implementation sequence should be:

1. add optional `rmux-sdk` dependency and `RmuxBackend` skeleton;
2. implement session create/reuse/attach with stable pane binding;
3. implement `send_text`, `send_key`, UTF-8 checked `write_input`, and submit-only
   `run_command`;
4. add backend event stream and bridge rmux `output_stream` to browser xterm.js;
5. add structured snapshot conversion and text projection for `readScreen`;
6. move `run-command` wait/capture to rmux-native waits and snapshots;
7. resolve the scroll semantic gap before claiming full Semantic API parity.

## Open Questions

- `spike-rmux-scroll-semantics.md`: decide whether termstage `scroll` means
  browser viewport movement, rmux copy-mode movement, or both through separate
  operations.
- `spike-rmux-byte-input.md`: validate whether rmux should expose arbitrary pane
  byte input for browser xterm.js parity, or whether UTF-8 text/key input is
  sufficient.
- `spike-rmux-stream-resync.md`: define the exact browser resync behavior after
  `PaneOutputChunk::Lag`, including whether to send a structured snapshot reset.
- `spike-rmux-level2-lock.md`: evaluate rmux read-only attach or backend-enforced
  input ownership so native attach clients can participate in the same lock
  model as browser/API controllers.
