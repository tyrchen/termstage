# 80-browser-terminal-glossary

Status: draft v1
Owner: termstage

## Browser terminal

The Chrome-hosted xterm.js UI plus WebSocket client code. It is not the terminal
emulator implementation in Rust and it is not a sandbox.

## PTY

The operating-system pseudo-terminal pair used to make shells and terminal programs
behave as if they are attached to an interactive terminal. Browser input writes to
the PTY master; PTY output is rendered by xterm.js.

## Terminal byte stream

The raw bytes exchanged with the PTY, including printable text, control characters,
escape sequences, mouse events, alternate-screen switching, and paste data. This is
not a command-line string.

## Control message

A JSON WebSocket text frame used for non-byte-stream metadata such as resize or
heartbeat. Control messages are schema-validated and separate from terminal bytes.

## Controller

A browser connection that can write terminal bytes to the PTY. M0 allows exactly one
controller.

## Mirror

A future read-only browser connection that can observe PTY output without writing
input. This is not part of M0.

## Session

The logical terminal runtime state owned by a session actor. In tmux mode, the session
maps to a tmux session name. In shell mode, it maps to a spawned shell process.

## Tmux mode

The default presentation mode. The runtime spawns `tmux new-session -A -s <name>` in
a PTY so Chrome and a native terminal can attach to the same demo state.

## Shell mode

A fresh local shell spawned in a PTY. Useful for smoke tests and simple demos, but it
does not attach to an existing Terminal.app window.

## Local command terminal

The optional split TUI rendered inside the invoking terminal when shell mode is
started with `--local-command-terminal`. It contains a `termstage` supervisor/log
pane and a separate command terminal pane backed by the runtime command PTY.

## Command terminal pane

The local TUI pane that renders parsed command PTY output and forwards keyboard
input to the runtime when local terminal owns the input lease. It is distinct from
`termstage`'s supervisor/log pane.

## Local-only

The server binds and accepts only loopback traffic, validates token/Host/Origin/peer,
and does not support LAN or internet access.

## Cross-References

- Used by all browser terminal specs.
