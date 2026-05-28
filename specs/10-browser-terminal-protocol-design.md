# 10-browser-terminal-protocol: Data Model and Wire Contract

Status: draft v1
Owner: termstage
Depends on: [00-browser-terminal-prd.md](./00-browser-terminal-prd.md)

## 1. Purpose

This spec owns the wire protocol between the browser terminal and the local Rust
server. The protocol is intentionally small: binary WebSocket frames carry raw
terminal bytes, and text WebSocket frames carry validated JSON control messages.
This keeps terminal behavior byte-accurate while making viewport size, backend
resize, and lifecycle messages explicit.

## 2. Interface

### 2.1 WebSocket Frames

```text
Browser -> Server

Binary frame:
  raw UTF-8/control byte stream produced by xterm.js term.onData()

Text frame:
  JSON control message using camelCase fields
  - `resize` updates the embedded terminal viewport size.
  - `viewport` updates the browser-local backend-screen origin.
  - `heartbeat` keeps the socket alive.
  - `acquireControl` requests the input lease.

Server -> Browser

Binary frame:
  terminal bytes for xterm.js term.write()
  - runtime mode: raw PTY output
  - gateway mode: projected backend-screen frame

Text frame:
  JSON status/error control message
```

### 2.2 Control Messages

All control messages use `#[serde(rename_all = "camelCase")]` and
`#[serde(deny_unknown_fields)]`. Validation runs immediately after deserialization,
per `AGENTS.md` input validation guidance.

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ClientControlMessage {
    Resize { cols: TerminalCols, rows: TerminalRows },
    Viewport { col: Option<ViewportOrigin>, row: Option<ViewportOrigin> },
    AcquireControl,
    Heartbeat { sequence: HeartbeatSequence },
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ServerControlMessage {
    Ready { session: SessionName },
    Warning { code: WarningCode, message: SafeMessage },
    Error { code: ErrorCode, message: SafeMessage },
}
```

Required newtypes:

| Type | Validation |
| --- | --- |
| `SessionName` | ASCII `[A-Za-z0-9_.-]`, 1..=64 bytes. |
| `TerminalCols` | Integer range 20..=300. |
| `TerminalRows` | Integer range 5..=120. |
| `ViewportOrigin` | Zero-based backend-screen row/column origin, integer range 0..=10000. |
| `HeartbeatSequence` | Monotonic `u64`; overflow closes the connection with a protocol error. |
| `SafeMessage` | Server-generated only; max 512 bytes; no secrets. |
| `AccessToken` | 256-bit CSPRNG token, redacted in `Debug`, compared constant-time. |

### 2.3 Flow

```text
Browser                         Server                         Runtime/Gateway
  |                               |                               |
  | 1. GET /?token=... ---------->|                               |
  |                               | 2. Validate token/host/origin |
  |                               |                               |
  | 3. WS /ws?token=... --------->|                               |
  |                               | 4. Upgrade after validation   |
  |                               |                               |
  | 5. resize JSON -------------->|                               |
  |    browser viewport size      | 6. Validate cols/rows         |
  |                               |    runtime mode: resize PTY ->| resize PTY
  |                               |    gateway mode: store viewport
  | 7. input bytes -------------->|                               |
  |                               | 8. Forward bytes ------------->| write master
  |                               |                               |
  |                               | 9. PTY output bytes <---------| read master
  | 10. terminal bytes <----------|                               |
  |                               |                               |
  | 11. close/refresh ----------->|                               |
  |                               | 12. detach or stop per policy |
```

## 3. Invariants

- Raw terminal input and output are never wrapped in JSON.
- Every JSON text frame is schema-validated and size-limited before use.
- Unknown control message fields are rejected.
- Resize dimensions are valid domain newtypes before reaching runtime or gateway
  state.
- Tokens never appear in logs, debug output, browser-visible errors, or panic messages.
- The protocol supports reconnect without changing the PTY byte contract.
- Browser resize means "the embedded xterm viewport can currently display this
  many rows/columns." Applying that size to a PTY or backend pane is a
  session-mode decision, not an unconditional protocol rule.
- Browser viewport origin means "the embedded terminal component is viewing this
  zero-based row/column slice of a backend-owned screen." It is browser-local
  gateway state and is ignored by runtime-owned PTY mode.

## 4. Behavior

Invalid binary frames are not parsed; they are forwarded as terminal bytes subject to
frame size limits from [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md).

Invalid text frames produce `ServerControlMessage::Error` and close the WebSocket
with a protocol close code. The server does not attempt to sanitize malformed JSON.

Resize events are debounced in the browser, but the server remains correct if
resize messages arrive rapidly. In runtime-owned PTY mode, the PTY actor
processes them sequentially in mailbox order. In backend-owned gateway mode,
the gateway stores them as browser viewport state and uses them when projecting
backend screen snapshots into the browser xterm.

## 5. AGENTS.md Binding

- Error handling: per `AGENTS.md` Error Handling, domain errors use `thiserror` in
  `termstage-core`; CLI/application layers use `anyhow` with context.
- Async/concurrency: per `AGENTS.md`, messages cross actor boundaries through bounded
  channels; no shared mutable PTY state.
- Type design: dimensions, tokens, and names are newtypes with fallible constructors.
- Safety/security: no `unsafe`; no `unwrap()`/`expect()` in production paths.
- Serialization: `serde`, camelCase, deny unknown fields, immediate validation.
- Testing: same-file unit tests for validation; integration tests for protocol errors.
- Observability: structured `tracing`; token and terminal byte payloads are redacted.
- Performance: use `bytes::Bytes` or borrowed slices for frame payloads where practical.
- Documentation: public protocol types have doc comments and `# Errors` sections.

## 6. Cross-References

- Depends on: [00-browser-terminal-prd.md](./00-browser-terminal-prd.md).
- Consumed by: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
