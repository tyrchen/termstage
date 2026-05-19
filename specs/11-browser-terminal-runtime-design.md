# 11-browser-terminal-runtime: PTY and Session Runtime

Status: draft v1
Owner: presenterm
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md)

## 1. Purpose

The runtime owns local terminal sessions. It creates a PTY, spawns either a new shell
or a tmux attach command, bridges PTY output to WebSocket sessions, applies resize
events, and implements deterministic shutdown. It does not own HTTP routing, browser
UI, or CLI argument parsing.

## 2. Interface

```rust
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub mode: SessionMode,
    pub initial_size: TerminalSize,
    pub reconnect_policy: ReconnectPolicy,
}

#[derive(Debug, Clone)]
pub enum SessionMode {
    NewShell { shell: ShellCommand },
    Tmux { session: SessionName },
}

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    AttachClient { client_id: ClientId, output: ClientOutputTx },
    DetachClient { client_id: ClientId },
    Input { client_id: ClientId, bytes: bytes::Bytes },
    Resize { size: TerminalSize },
    Shutdown { reason: ShutdownReason },
}
```

`ShellCommand` stores an executable path plus argv vector. It is never a shell string.
For tmux mode, the command shape is `tmux new-session -A -s <session>`, passed through
argv form.

## 2a. Architecture

```text
                         presenterm-server
                               |
                               | validated RuntimeCommand
                               v
        +------------------------------------------------+
        | presenterm-core runtime                         |
        |                                                |
        |  +-------------------+       +---------------+  |
        |  | Session Manager   | ----> | Session Actor |  |
        |  | owns registry     |       | owns PTY      |  |
        |  +---------+---------+       +-------+-------+  |
        |            |                         |          |
        |            | bounded channel         | blocking |
        |            v                         v read    |
        |  +-------------------+       +---------------+  |
        |  | Client Mailboxes  | <---- | PTY Reader    |  |
        |  | per connection    |       | thread/task   |  |
        |  +-------------------+       +-------+-------+  |
        |                                      |          |
        +--------------------------------------+----------+
                                               |
                                               v
                                        shell / tmux
```

## 3. Invariants

- Exactly one actor owns a PTY master handle.
- PTY reads and writes are isolated from async request handlers; blocking operations
  run in a dedicated thread or `spawn_blocking`.
- Runtime input is already validated before it reaches the actor boundary.
- Only one controlling client is allowed in M0 and M1; read-only mirror clients are a
  later milestone.
- Session shutdown is explicit: browser disconnect does not kill tmux-backed sessions
  unless the CLI selected an exit-on-disconnect policy.
- Every spawned task is joined, supervised, or explicitly documented as detached.

## 4. Behavior

### 4.1 New Shell Mode

The actor opens a PTY at the requested initial size and spawns the configured shell.
Default shell resolution is platform-specific and happens once at startup:

- Unix: `$SHELL` when present and valid, otherwise `/bin/sh`.
- Windows: PowerShell is a later milestone; M0 targets Unix/macOS first.

### 4.2 Tmux Mode

The actor opens a PTY and spawns `tmux new-session -A -s <session>`. This is the
supported path for sharing one live demo state between a native terminal and Chrome.
The implementation never tries to control an existing Terminal.app window.

### 4.3 Backpressure

Each client has a bounded output mailbox. If the browser cannot keep up, the runtime
emits a warning and closes that client rather than buffering unbounded terminal output.
The PTY process remains alive according to the reconnect policy.

### 4.4 Panic and Cancellation Policy

Task panics are reported through `tracing` and converted into runtime errors visible
to the server supervisor. Actor cancellation closes client mailboxes first, then the
PTY writer, then joins the PTY reader, then terminates the child process when the
session policy requires it.

## 5. AGENTS.md Binding

- Error handling: `RuntimeError` is a `thiserror` enum with source errors. Application
  entrypoints add `anyhow::Context`.
- Async/concurrency: actor owns mutable state and communicates through bounded
  channels; no `Mutex<HashMap<...>>` registry.
- Type design: `ClientId`, `TerminalSize`, `SessionName`, `ShellCommand`, and
  `ShutdownReason` are explicit types.
- Safety/security: no `unsafe`; argv process spawning only; checked arithmetic on
  frame accounting and buffer sizes.
- Serialization: N/A for internal runtime commands.
- Testing: unit tests for lifecycle transitions; integration tests with a short-lived
  shell command.
- Observability: spans for session start, attach, detach, resize, child exit, and
  shutdown; terminal byte payloads are never logged.
- Performance: bounded buffers; no unbounded `Vec<u8>` from PTY output.
- Documentation: public runtime types document lifecycle and failure modes.

## 6. Cross-References

- Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md).
- Consumed by: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
