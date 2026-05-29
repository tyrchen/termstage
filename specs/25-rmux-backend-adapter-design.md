# rmux Backend Adapter Design

Status: draft v1
Owner: termstage
Last updated: 2026-05-28
Depends on: [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md),
[61-browser-terminal-crates-and-features.md](./61-browser-terminal-crates-and-features.md),
[../docs/research/study-rmux-sdk-alignment.md](../docs/research/study-rmux-sdk-alignment.md)

## 1. Purpose

This spec starts the `RmuxBackend` implementation track. It turns the backend
gateway direction from
[23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md)
into an rmux-specific adapter contract that can be implemented without
re-opening the local-terminal ownership debate.

`termstage` remains the session registry, web gateway, semantic API gateway, and
Level 1 operation-lock owner. rmux owns the real terminal session, pane, PTY,
screen state, daemon output retention, and native local attach path.

## 2. Source Context

The design is based on the local `vendors/rmux` submodule at
`6301d12b7db85ebeea2277a19e43bda8675622a9`.

Load-bearing rmux surfaces:

- rmux exposes a tmux-compatible CLI, `rmux-sdk`, `ratatui-rmux`, and one local
  daemon protocol shared by those public surfaces
  (`vendors/rmux/README.md:184`, `vendors/rmux/README.md:188`).
- `rmux-sdk` creates or reuses sessions through `EnsureSession` and
  `EnsureSessionPolicy::{CreateOnly, CreateOrReuse, ReuseOnly}`
  (`vendors/rmux/crates/rmux-sdk/src/ensure.rs:16`,
  `vendors/rmux/crates/rmux-sdk/src/ensure.rs:31`).
- rmux session handles expose `pane(window_index, pane_index)` and
  `pane_by_id(PaneId)` so callers can move from slot addressing to stable pane
  identity after resolving the first pane
  (`vendors/rmux/crates/rmux-sdk/src/handles/session.rs:119`,
  `vendors/rmux/crates/rmux-sdk/src/handles/session.rs:135`).
- rmux pane handles expose raw output streams, render streams, structured
  snapshots, text/key input, resize, waits, and close operations
  (`vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:183`,
  `vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:247`,
  `vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:381`,
  `vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:418`,
  `vendors/rmux/crates/rmux-sdk/src/handles/pane.rs:428`).
- rmux snapshots carry row-major cells, cursor visibility/style, colors,
  hyperlinks, and a daemon-derived revision counter
  (`vendors/rmux/crates/rmux-proto/src/response/pane.rs:242`,
  `vendors/rmux/crates/rmux-proto/src/response/pane.rs:282`).

The current `termstage` spine already has `BackendAdapter`, `SessionGateway`,
`SessionRegistry`, `OperationLock`, and `TmuxBackend`. `RmuxBackend` must fit
that spine first, then improve the shared contracts only where rmux needs a
real streaming or structured-screen capability.

## 3. Problem

The tmux compatibility adapter proves the backend-session model, but it is
limited by command execution and snapshot polling:

- browser output is reconstructed by polling visible text;
- semantic `read-screen` loses cell style, cursor style, hyperlinks, and
  revision information;
- `run-command` wait/capture behavior cannot use backend-native output waits;
- pane identity is slot-oriented, while rmux can provide stable `PaneId`
  handles;
- rmux's daemon stream can report lag and retained recent bytes, but the current
  `BackendEvent` shape cannot represent that yet.

The rmux adapter should not be a tmux-shaped shell-out implementation. It should
use the typed SDK and expose the minimum shared capabilities that make rmux
valuable as the default backend candidate.

## 4. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Add an rmux-native backend behind the existing `SessionGateway`. | `RmuxBackend` implements `BackendAdapter` without changing browser/API auth or lock semantics. |
| G2 | Use rmux SDK session creation instead of shelling out. | `create_or_find_session` maps to `EnsureSession::CreateOrReuse` and returns a backend ref with stable pane identity when available. |
| G3 | Use rmux raw output stream for browser sync. | Browser output can subscribe to rmux `Pane::output_stream` instead of fixed-rate snapshot polling. |
| G4 | Preserve structured screen data for API and future UI work. | The core model can carry rmux cell/cursor/revision data, while text projection remains available for existing API responses. |
| G5 | Map semantic operations to rmux-native methods. | `send_text`, `send_key`, `run_command`, waits, and capture use rmux SDK methods instead of byte/key emulation. |
| G6 | Keep operation ownership in termstage. | rmux session leases are lifecycle tools only; the Level 1 write lock stays in `SessionGateway`. |
| G7 | Keep tmux compatibility intact. | Existing tmux backend tests and API behavior continue to pass while rmux support lands behind an explicit backend selection path. |

## 5. Non-goals

- Do not remove `TmuxBackend` in this spec.
- Do not make rmux mandatory for users who already rely on tmux.
- Do not use rmux app-owned session leases as the input-operation lock.
- Do not add OIDC, standalone web-server mode, or remote daemon networking.
- Do not add a local split terminal UI inside `termstage`.
- Do not require structured snapshot rendering in xterm.js before raw stream
  mirroring works.

## 6. Target Architecture

```text
Browser xterm.js / Agent API
        │
        │ Termstage Protocol
        ▼
┌──────────────────────────────────────────────────────────────┐
│ termstage                                                    │
│                                                              │
│  ┌────────────────────┐     ┌─────────────────────────────┐  │
│  │ SessionGateway     │     │ Semantic API gateway        │  │
│  │ - registry         │◀───▶│ - send-text / send-key      │  │
│  │ - Level 1 lock     │     │ - run-command wait/capture  │  │
│  └─────────┬──────────┘     └─────────────────────────────┘  │
│            │                                                 │
│  ┌─────────▼──────────────────────────────────────────────┐  │
│  │ BackendAdapter                                         │  │
│  │                                                        │  │
│  │  TmuxBackend              RmuxBackend                  │  │
│  │  - shell commands         - rmux-sdk facade            │  │
│  │  - snapshot polling       - stable PaneId              │  │
│  │                           - output stream              │  │
│  │                           - structured snapshot        │  │
│  └─────────┬──────────────────────────────────────────────┘  │
└────────────┼─────────────────────────────────────────────────┘
             │ rmux-sdk local IPC
             ▼
┌──────────────────────────────────────────────────────────────┐
│ rmux daemon                                                  │
│ - owns sessions/windows/panes/PTYs                           │
│ - owns screen parser and structured snapshots                 │
│ - owns output retention and lag reporting                     │
│ - supports native local attach via rmux CLI                   │
└──────────────────────────────────────────────────────────────┘
```

Local attach remains backend-native:

```text
termstage session "abc"
        │ registry entry
        ▼
rmux session "abc", pane id "%7"
        ├── local user: rmux attach -t abc
        └── browser/API: termstage -> RmuxBackend -> rmux-sdk -> pane "%7"
```

## 7. Crate and Feature Shape

Add rmux dependencies through workspace dependency management:

```toml
[workspace.dependencies]
rmux-sdk = { version = "0.3", default-features = false }
```

`termstage-core` should expose rmux behind an explicit feature:

```toml
[features]
backend-rmux = ["dep:rmux-sdk"]

[dependencies]
rmux-sdk = { workspace = true, optional = true }
```

The server binary enables this feature only when the CLI/backend selection path
requires it. If implementation proves rmux SDK needs default features for local
daemon startup, update this spec and
[61-browser-terminal-crates-and-features.md](./61-browser-terminal-crates-and-features.md)
with the exact feature list before landing code.

## 8. Adapter Types

`RmuxBackend` belongs in `crates/core/src/rmux_backend.rs` behind the
`backend-rmux` feature. It is the only module that imports `rmux_sdk`.

Required types:

```text
RmuxBackend
  - rmux: rmux_sdk::Rmux
  - sessions: map SessionName -> RmuxSessionBinding
  - config: RmuxBackendConfig

RmuxSessionBinding
  - backend_ref: BackendSessionRef
  - rmux_session_name: rmux_sdk::SessionName
  - pane_id: Option<rmux_sdk::PaneId>
  - window_index: u32
  - pane_index: u32

RmuxBackendConfig
  - connect_timeout
  - default_operation_timeout
  - output_stream_start
  - close_policy
```

`RmuxBackendConfig` must be a real typed config object, not ad hoc environment
lookups in adapter methods. Defaults should be conservative:

- connect or start the local daemon with a bounded timeout;
- create or reuse named sessions;
- start new browser output streams from `Now`;
- do not kill reused sessions unless termstage explicitly created them and the
  user selected an owning close policy.

## 9. Session Lifecycle Mapping

`BackendAdapter::create_or_find_session` maps to rmux as follows:

| Termstage operation | rmux SDK operation | Notes |
| --- | --- | --- |
| validate termstage session | `rmux_sdk::SessionName::new` | Convert through `TryFrom`/fallible constructor; never string-concatenate into commands. |
| connect backend | `Rmux::builder().default_timeout(...).connect_or_start()` | Use SDK startup and endpoint validation; no `sh -c`. |
| create/reuse session | `EnsureSession::named(...).policy(CreateOrReuse).detached(true).size(...)` | Preserve current shell-mode semantics until CLI command selection is redesigned. |
| resolve first pane | `session.pane(0, 0)` then `pane.id()` | Store stable pane id when available. |
| stable pane handle | `session.pane_by_id(pane_id)` | Use for input, resize, snapshot, stream, and waits after initial resolution. |
| close | `Session::kill()` or detach/no-op per close policy | Do not kill sessions that termstage only reused unless policy says so. |

The returned `BackendSessionRef` must use `BackendKind::Rmux`, the termstage
session name, window id `"0"` for the initial milestone, and a pane id string
derived from rmux `PaneId`. Later multi-window/multi-pane support can replace
the window slot with richer routing, but the first adapter targets the active
initial pane only.

## 10. Input Mapping

Browser byte input and semantic input are different surfaces:

| Termstage method | rmux SDK method | Behavior |
| --- | --- | --- |
| `write_input(Bytes)` | UTF-8 checked text -> `Pane::send_text` | Existing browser protocol sends user input as text. Reject non-UTF-8 bytes with `BackendError::UnsupportedInput`. |
| `send_text(&str)` | `Pane::send_text` | Literal text, no implicit newline. |
| `send_key(&str)` | `Pane::send_key` | One tmux-compatible key token. |
| `run_command(&str)` | `send_text(command)` then `send_key("Enter")` | Submit only; wait/capture is handled by the semantic request/response layer below. |
| `resize(TerminalSize)` | `Pane::resize(TerminalSizeSpec)` | Apply active-controller size only; do not sync every viewer's size. |

If rmux adds a byte-oriented input API, `write_input(Bytes)` can be upgraded to
preserve arbitrary bytes. Until then, the adapter must reject invalid UTF-8
rather than lossy-convert browser input.

## 11. Output Stream Mapping

The rmux adapter must add an event-stream capability to the backend contract.
The current `BackendEvent` enum should be extended before the browser consumes
rmux output directly:

```text
BackendEvent
  Output { bytes }
  Lag { missed_events, recent_bytes }
  Resized { size }
  SnapshotInvalidated { revision }
  Closed { message }
```

`RmuxBackend` opens `Pane::output_stream` or
`Pane::output_stream_starting_at(start)` and runs one bounded pump task per
browser/API stream:

```text
rmux PaneOutputStream         RmuxBackend pump        termstage WebSocket
       │                            │                         │
       │ Bytes(seq, bytes) ────────▶│ BackendEvent::Output ──▶│ output frame
       │                            │                         │
       │ Lag(notice) ──────────────▶│ BackendEvent::Lag ─────▶│ status/resync
       │                            │                         │
       │ stream end/error ─────────▶│ BackendEvent::Closed ──▶│ close/error
```

The channel between pump task and gateway must be bounded. On local backpressure
or rmux lag, the gateway must not allocate unbounded history. The first browser
behavior can emit the retained recent bytes and a status frame; a later rich
rendering milestone can reset from a structured snapshot.

## 12. Screen Snapshot Mapping

`BackendScreenSnapshot` is the semantic/API projection. It must remain plain
text so Agent and API consumers can analyze screen contents without stripping
terminal style sequences. Browser attach is a human-facing terminal replay path
and should use `BackendTerminalSnapshot`, whose lines may contain ANSI escape
sequences produced from backend style data.

rmux also requires a structured path:

```text
BackendStructuredSnapshot
  size
  cursor { row, col, visible, style }
  revision
  cells: Vec<BackendCell>

BackendCell
  text
  width
  padding
  attributes
  foreground
  background
  underline
  hyperlink
```

`read_screen` returns the plain text projection derived from
`PaneSnapshot::visible_lines`. `read_terminal_screen` renders rmux cells into an
ANSI terminal replay snapshot for browser xterm display. The adapter must also
expose structured snapshot conversion before rmux becomes the default backend.
That avoids permanently collapsing rmux's cell model into tmux `capture-pane`
text.

## 13. Semantic Request/Response

External semantic API shape stays transport-independent:

```text
POST /api/sessions/{session}/send-text
POST /api/sessions/{session}/send-key
POST /api/sessions/{session}/run-command
GET  /api/sessions/{session}/screen
```

`run-command` remains request/response:

```json
{
  "command": "echo hello",
  "controllerId": "browser:tab-1",
  "waitFor": "hello",
  "waitTimeoutMs": 5000,
  "capture": true
}
```

For rmux, wait/capture should use backend-native primitives:

- `waitFor` maps to `Pane::wait_for_text_next` when the caller wants future
  output after command submission;
- visible-state checks map to `Pane::expect_visible_text` or
  `Pane::wait_for_text` when the expected output may already be present;
- `capture: true` maps to `Pane::snapshot` and returns both the existing text
  projection and the future structured snapshot field.

The first implementation may keep the current gateway-level wait loop as a
compatibility fallback, but rmux support is not complete until native waits are
used for this endpoint.

### 13.1 rmux API vs termstage Semantic API

Implementation should directly use rmux SDK wherever rmux already has the
right primitive. The public termstage API should still remain backend-neutral.

| Termstage semantic surface | rmux SDK/API support | Ownership |
| --- | --- | --- |
| Create/reuse backend session | `Rmux::connect_or_start`, `EnsureSession` | Direct rmux SDK call inside `RmuxBackend`. |
| Native local attach | `rmux attach -t <session>` | rmux CLI, outside termstage local TTY rendering. |
| Send literal text | `Pane::send_text` | Direct rmux SDK call after termstage lock validation. |
| Press key / send key token | `Pane::send_key` | Direct rmux SDK call after termstage lock validation. |
| Resize pane | `Pane::resize` | Direct rmux SDK call from the active controller size. |
| Read visible screen | `Pane::snapshot`, `PaneSnapshot::visible_lines`, `visible_text` | Direct rmux SDK call plus plain-text termstage projection into API response types. |
| Render browser screen | `Pane::snapshot` cells, `Pane::output_stream`, `render_stream` | Preserve colors/styles for browser terminal replay without changing semantic API output. |
| Wait for output/text | `Pane::wait_for`, `wait_for_next`, `wait_for_text`, `wait_for_text_next` | Direct rmux SDK call used by termstage request/response operations. |
| Find visible text | `Pane::find_text`, locators | rmux SDK primitive; termstage may expose later as an Agent API operation. |
| Capture styled region | rmux capture helpers and `PaneSnapshot` cells | rmux SDK primitive; termstage decides response schema. |
| `run-command` with `controllerId`, `waitFor`, timeout, and optional capture | No single rmux API with this termstage envelope; composed from `send_text`, `send_key("Enter")`, waits, and snapshot. | termstage semantic operation. |
| Acquire/release one-writer operation lock | rmux has session leases and tmux-compatible lock commands, but not termstage's browser/API controller lease. | termstage-only Level 1 lock. |
| Controller status for browser toolbar | No rmux equivalent. | termstage-only session gateway state. |
| Auth/token/OIDC-compatible browser gateway | No rmux equivalent. | termstage-only gateway policy. |
| Backend-neutral session registry | rmux knows rmux sessions only. | termstage maps termstage session id to backend refs. |
| Backend-neutral error and event model | rmux has typed errors and lag notices, but not termstage's cross-backend envelope. | termstage normalizes backend details. |
| Browser/API scroll operation | rmux has copy-mode behavior and key support, but no first-class `Pane::scroll` SDK method in the studied surface. | termstage semantic operation with backend-specific implementation. |

Rule: `RmuxBackend` may depend on rmux SDK types. Public browser/API protocol
types and `SessionGateway` controller semantics must not expose rmux SDK types
directly.

## 14. Lock and Lease Boundaries

Termstage's Level 1 operation lock remains authoritative for browser/API
writes:

```text
Controller ── acquire/write ──▶ SessionGateway ──▶ RmuxBackend ──▶ rmux pane
                          validates lease owner
```

rmux app-owned session leases have a different purpose: keeping sessions alive
while an owning application renews them. They may be used later for lifecycle
cleanup of sessions created by `termstage`, but they must not replace the
operation lock.

Rules:

- every write path still calls `SessionGateway` lock validation before reaching
  `RmuxBackend`;
- local `rmux attach -t <session>` is outside Level 1 lock enforcement in this
  phase;
- backend-enforced write locks remain Level 2 future work;
- rmux lease loss must be reported as backend lifecycle failure, not as a
  controller ownership event.

## 15. Error Handling and Validation

This spec follows `AGENTS.md` as binding engineering policy.

- All rmux errors map to `BackendError` with redacted, length-capped
  `SafeMessage` values.
- User-provided session names, key names, command text, and semantic text remain
  validated at the HTTP/CLI boundary before reaching the adapter.
- No `unwrap()` or `expect()` is allowed in production adapter code.
- No shell command interpolation is allowed; rmux interaction uses `rmux-sdk`.
- Rmux endpoint configuration, if later exposed through CLI/config, must be a
  validated enum rather than raw string plumbing.
- Tracing spans must include backend kind, session id, pane id, operation name,
  and durations. They must not log full command text or arbitrary terminal
  output.

## 16. Testing and Verification

Required tests:

| Layer | Tests |
| --- | --- |
| Unit | Conversion from termstage `SessionName`/`TerminalSize` to rmux SDK types; rmux error mapping; snapshot projection; lag event conversion. |
| Adapter integration | Create/reuse session, resolve stable pane id, send text, send key, run command, wait for text, capture snapshot, resize, close/detach according to policy. |
| Gateway integration | Browser/API write requires Level 1 ownership; non-owner can read but cannot write; rmux backend and tmux backend share API behavior. |
| Browser E2E | Open browser against an rmux-backed session, verify output follows without polling-only drift, send Ctrl-C/key/text, verify local `rmux attach` sees the same pane. |
| Failure | rmux unavailable, daemon startup timeout, stale pane id, lag notice, invalid UTF-8 input, lock conflict, close reused session with no-kill policy. |

Environment-sensitive rmux tests may be skipped by default unless
`TERMSTAGE_RMUX_TEST=1` is set and a compatible rmux binary/daemon is available.
Normal unit tests must not require a live rmux daemon.

Before merging implementation, run the project quality gates from `AGENTS.md`:
`cargo build`, `cargo test`, `cargo +nightly fmt`, and strict `cargo clippy`.
Run rmux integration tests in CI once the dependency and daemon startup path are
stable.

## 17. Implementation Phases

### Phase 25.1 - Contract Amendments

- Add `backend-rmux` feature and optional `rmux-sdk` dependency.
- Add structured snapshot types or a clear extensible enum.
- Add backend output subscription/event-stream contract.
- Add error conversions and type conversion helpers.

Exit criteria: tmux backend still compiles and tests pass without enabling
`backend-rmux`.

### Phase 25.2 - RmuxBackend Skeleton

- Add `RmuxBackendConfig`.
- Connect or start rmux through `Rmux::builder`.
- Implement `create_or_find_session`, stable pane resolution, and close policy.

Exit criteria: ignored rmux integration test can create/reuse, resolve pane id,
and close/detach a session.

### Phase 25.3 - Input and Resize

- Implement `write_input`, `send_text`, `send_key`, `run_command`, and `resize`.
- Preserve current `SessionGateway` Level 1 lock checks.

Exit criteria: API and gateway tests can drive an rmux pane and see matching
screen output.

### Phase 25.4 - Output Stream

- Implement rmux output stream subscription and bounded pump task.
- Extend browser WebSocket gateway to prefer stream events for rmux and keep
tmux polling as compatibility fallback.

Exit criteria: browser follows live rmux output without fixed snapshot polling.

### Phase 25.5 - Snapshot and Semantic Capture

- Convert rmux `PaneSnapshot` into text and structured termstage snapshots.
- Map `run-command` wait/capture to rmux native wait and snapshot operations.

Exit criteria: `run-command` with `waitFor` and `capture` returns matched state
and captured screen without gateway-only polling.

### Phase 25.6 - Hardening

- Add stale-pane recovery and resubscribe behavior.
- Add lag handling/resync behavior.
- Add docs for `rmux attach -t <session>` local viewing.
- Decide whether rmux becomes the default backend after tmux parity checks pass.

Exit criteria: rmux and tmux behavior are documented, tested, and selectable.

## 18. Open Questions

- Should `BackendStructuredSnapshot` be part of `BackendScreenSnapshot` or a
  separate snapshot enum? The implementation should choose the shape with the
  smallest API churn while preserving rmux cell data.
- Should rmux streams start at `Now` or `Oldest` when a browser opens late? The
  initial recommendation is `Now` plus explicit snapshot replay, because replaying
  all retained bytes can be expensive and visually surprising.
- Should `RmuxBackend` own rmux app-owned session leases for sessions it creates?
  This is useful for cleanup, but only after close policy is explicit.
- How should non-owner local `rmux attach` writes be surfaced in the termstage
  toolbar? Level 1 cannot prevent them in this phase, but browser/API observers
  should still see resulting output.

## 19. Cross-References

- Extends [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md)
  with the concrete rmux adapter path.
- Updates [61-browser-terminal-crates-and-features.md](./61-browser-terminal-crates-and-features.md)
  when implementation adds `rmux-sdk`.
- Consumed by [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md)
  Phase 8 and follow-up rmux adapter phases.
- Based on [../docs/research/study-rmux-sdk-alignment.md](../docs/research/study-rmux-sdk-alignment.md).
