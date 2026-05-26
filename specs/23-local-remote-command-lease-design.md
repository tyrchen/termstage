# Shell Mode Local/Remote Lease Design

Status: draft v2
Last updated: 2026-05-26

## 1. Problem

`termstage` currently starts a browser terminal whose primary controller is the
browser. That works for presentation-first flows, but shell mode also needs an
operator-local shape where the shell process feels native in the invoking terminal
while also being visible and controllable from the browser.

The new mode must preserve TTY semantics for interactive CLIs. The child process
must run inside a PTY, not through ordinary piped stdin/stdout, so color, raw mode,
cursor movement, Ctrl-C, paste, and resize behavior remain compatible with native
terminal execution. This is an extension of `--mode shell`, not a separate
positional subcommand mode.

## 2. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Launch shell-mode commands with argv support. | `termstage --mode shell --command claude --attach-local-terminal` and `termstage --mode shell --command codemax -g claude --attach-local-terminal` run the requested command in the local terminal frontend. |
| G2 | Keep `termstage` lifecycle coupled to explicit shell-mode commands. | When the configured command exits or is killed, the web server shuts down unless the operator explicitly sets `--exit-policy hold`. |
| G3 | Make remote browser readonly at startup. | Browser attach receives a lease state where `terminal` owns input until browser sends terminal bytes. |
| G4 | Support explicit input takeover. | First real browser input transfers ownership to browser; first real local terminal input transfers ownership back to terminal. |
| G5 | Notify both sides about control changes. | Runtime emits a lease control frame to browser clients and local terminal frontend on each ownership epoch change. |

## 3. Non-goals

- This does not add multi-viewer read-only authorization. Existing token, Host,
  Origin, and peer checks still define access.
- This does not persist a lease across process restarts. The runtime owns lease
  state in memory.
- This does not multiplex multiple browser controllers. The latest browser
  connection remains the active browser endpoint; previous browser controllers are
  closed as before.

## 4. Shell Mode Local Terminal Attachment

The CLI extends existing shell mode instead of adding a new positional
subcommand. `--attach-local-terminal` is valid only with `--mode shell`. When present,
the configured shell-mode command becomes the PTY child process and the
invoking terminal becomes a local frontend.

```bash
termstage --mode shell --command claude --attach-local-terminal
termstage --mode shell --command codemax -g claude --attach-local-terminal
termstage --mode shell --command cargo -g test --attach-local-terminal
```

Without `--attach-local-terminal`, shell mode keeps the existing browser-first behavior.
With `--attach-local-terminal`, the local terminal is placed into raw mode and becomes a
first-class runtime client. The server still starts in the background and prints
or opens the tokenized browser URL before raw mode starts.

```text
Terminal process
  │
  │ termstage --mode shell --command claude --attach-local-terminal
  ▼
┌────────────────────────────────────────────────────────────────────┐
│ termstage wrapper                                                  │
│                                                                    │
│  ┌──────────────────────┐      RuntimeCommand::TerminalInput       │
│  │ local terminal input │ ──────────────────────────────────────┐  │
│  └──────────────────────┘                                      │  │
│                                                                ▼  │
│  ┌──────────────────────┐      ClientOutput::Bytes       ┌──────────────┐
│  │ local terminal output│ ◀────────────────────────────── │ runtime actor│
│  └──────────────────────┘                                │ owns PTY     │
│                                                          │ owns lease   │
│  ┌──────────────────────┐      WebSocket binary/control  │ owns replay  │
│  │ browser xterm.js     │ ◀══════════════════════════════▶│              │
│  └──────────────────────┘                                └──────┬───────┘
│                                                                 │ PTY
└─────────────────────────────────────────────────────────────────┼───────┘
                                                                  ▼
                                                           shell-mode child
```

## 5. Lease State Machine

The runtime is the only authority for input ownership. Frontends can display
readonly state, but they cannot enforce correctness independently.

```text
States:
  Terminal(owner_epoch)
  Browser(client_id, owner_epoch)

Initial state:
  Terminal(epoch = 0)

Events:
  browser terminal bytes from active browser client
    if owner != Browser(client_id):
      owner = Browser(client_id)
      epoch += 1
      notify all clients LeaseChanged(browser, epoch)
    write bytes to PTY

  local terminal bytes
    if owner != Terminal:
      owner = Terminal
      epoch += 1
      notify all clients LeaseChanged(terminal, epoch)
    write bytes to PTY

  browser attach
    replace previous browser controller
    send Ready
    send current LeaseChanged(owner, epoch)
    replay recent PTY bytes

  child exit
    close all clients
    local-terminal shell-mode supervisor shuts down the web server
```

Resize, heartbeat, reconnect, and attach are not input ownership events. Only bytes
that are about to be written to the PTY can change the lease owner.

## 6. Protocol

Server-to-client control messages add:

```json
{ "type": "leaseChanged", "owner": "terminal", "epoch": 0 }
{ "type": "leaseChanged", "owner": "browser", "epoch": 1 }
```

`epoch` is monotonic within one runtime session. Browser clients treat `owner:
"terminal"` as readonly presentation state, but still send the first input bytes so
the runtime can transfer the lease.

## 7. Implementation Notes

- Follow `AGENTS.md` for Rust 2024, no unsafe, no production `unwrap()`/`expect()`,
  structured errors, actor-owned mutable state, strict protocol validation, and
  bounded channels.
- Reuse the existing `RuntimeSession`, PTY reader, replay buffer, and WebSocket
  bridge.
- Add a local terminal frontend in `apps/server` rather than duplicating PTY logic.
- Avoid ordinary `Command::stdin(Stdio::piped())` for the child. The child must
  continue to spawn through `portable-pty`.

## 8. Cross-references

- Depends on [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md)
  for WebSocket control frame conventions.
- Depends on [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md)
  for actor ownership and PTY lifecycle.
- Extends [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md)
  with shell-mode local terminal attachment and shell argv support.
- Extends [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md)
  without changing the trust boundary.
