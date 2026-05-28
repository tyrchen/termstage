# 80-browser-terminal-glossary

Status: draft v1
Owner: termstage

## Browser terminal

The Chrome-hosted xterm.js UI plus WebSocket client code. It is not the terminal
emulator implementation in Rust and it is not a sandbox.

## Embedded terminal component

The page component that contains xterm.js. It fits the HTML element allocated by
the page layout and can coexist with nav bars, buttons, sidebars, or other future
HTML. Its scroll and viewport interactions are scoped to the component, not to
the entire document.

## Browser viewport

The rows and columns currently visible inside the embedded terminal component,
derived from the terminal container's CSS size, font family, font size, and line
height. Browser viewport size is not automatically the backend pane size.

## Backend screen

The rows and columns owned by a backend pane, such as a tmux or rmux pane. It is
the source of truth for terminal application layout in backend-owned gateway
mode.

## Viewport projection

The gateway operation that maps a backend screen into the browser viewport. If
the backend screen is wider or taller than the browser viewport, projection
selects a visible slice and translates cursor/input coordinates through the
current viewport origin.

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

## Backend session

A session/window/pane owned by a backend such as tmux, rmux, or a future terminal
backend. `termstage` stores a reference to it and reaches it through a backend
adapter; the backend owns the actual PTY and native local attach behavior.

## Operation lock

The `termstage`-managed write lease for a backend session. At Level 1 it allows
one browser or API controller to write at a time while other clients remain
read-only observers.

## Local-only

The server binds and accepts only loopback traffic, validates token/Host/Origin/peer,
and does not support LAN or internet access.

## Cross-References

- Used by all browser terminal specs.
