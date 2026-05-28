# 50-browser-terminal-cli: Command Surface and Presentation UX

Status: draft v1
Owner: termstage
Depends on: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md)

## 1. Purpose

The CLI turns the runtime and web components into a presenter-friendly workflow. It
owns argument parsing, defaults, browser opening, and terminal session mode selection.

## 2. Interface

Backend-session workflow:

```text
termstage session create --backend tmux --name presentation --command k9s
termstage session attach TerminalUse-presentation
termstage session attach TerminalUse-presentation --browser --open
```

Session creation arguments:

| Argument | Default | Meaning |
| --- | --- | --- |
| `--backend <tmux|rmux>` | `tmux` | Backend that owns the real session/pane/PTY. |
| `--name <name>` | required | Human-readable session name. Termstage-created backend sessions are named `TerminalUse-<name>`. |
| `--command <path>` | `$SHELL` on Unix | Command executable for the first backend pane. |
| `-g, --command-arg <arg>` | unset | Repeatable argv tail for `--command`. |

Session attach arguments:

| Argument | Default | Meaning |
| --- | --- | --- |
| `<session-id>` | required | Backend session id. If an exact match is missing, `session attach` also tries `TerminalUse-<session-id>`; tmux additionally keeps legacy `ts-<session-id>` fallback. |
| `--backend <tmux|rmux>` | auto | Optional backend filter when the session id is ambiguous. |
| `--browser` | false | Start the browser/API gateway instead of invoking the backend's native attach command. |
| `--host <addr>` | `127.0.0.1` | Browser-mode bind address; non-loopback requires `--expose-public`. |
| `--port <port>` | `0` | Browser-mode port. Port `0` means OS-chosen random port. |
| `--open` | false | Browser-mode option that opens the tokenized URL in the default browser. |
| `--font-size <px>` | `24` | Browser-mode terminal font size. |
| `--theme <name>` | `high-contrast` | Browser-mode presentation theme preset. |
| `--expose-public` | false | Enable pod/internet mode; required before non-loopback bind addresses are accepted. |
| `--public-url <url>` | unset | HTTPS browser-visible base URL for public mode. |
| `--token-env <name>` | unset | Environment variable containing a 64-hex-character access token for public mode. |

## 2a. User Flow

```text
CLI                   Backend             Browser
 |                       |                    |
 | 1. session create     |                    |
 | 2. validate name/cmd  |                    |
 |---------------------->| 3. create TerminalUse-name |
 | 4. print id/attach    |                    |
 |                       |                    |
 | 5. session attach <id> --browser           |
 | 6. resolve exact/id+prefix                 |
 | 7. attach gateway --->|                    |
 | 8. open URL ------------------------------>|
 |                       |<-------------------| 9. WS connect
```

## 3. Invariants

- CLI never accepts a non-loopback bind address unless `--expose-public` is present
  and [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md)
  validation succeeds.
- CLI arguments crossing trust boundaries are validated before server startup.
- `--name` is a human-readable session name, not a shell command. Termstage
  prefixes every backend session it creates with `TerminalUse-`.
- `--command` is an executable path and `-g` / `--command-arg` is repeatable argv
  handling, not a string passed through `sh -c`.
- Command startup is backend-first. The backend owns the command PTY and exposes
  native attach through its own client.
- Local attach for shared sessions is provided by the selected backend's native
  command, for example `tmux attach -t <session>` or a future `rmux attach`
  command, not by a `termstage` local-terminal flag.
- Browser URL printed to logs redacts token unless the output is the explicit user
  launch URL.
- Public mode requires `--public-url` and `--token-env`; local mode rejects both to
  keep exposure intent explicit.

## 4. Behavior

If `tmux` is missing in tmux mode, startup fails with a clear actionable error. It
does not silently fall back to a new shell because that would break the shared-session
presentation promise.

If browser opening fails, the CLI prints the tokenized URL once to stderr/stdout using
the chosen logging/output convention. Structured logs still redact the token.

The browser-mode CLI exits on gateway shutdown or when the backend session
disappears. Ctrl-C closes browser connections and joins gateway tasks. The backend
session remains owned by the backend unless the user runs `termstage session stop`.

## 4a. CLI Command Groups

Target top-level command groups:

| Group | Purpose | Initial commands |
| --- | --- | --- |
| `termstage session` | Create, attach to, inspect, list, and stop backend-owned sessions. The backend session id is the termstage session id. | `session create --backend <tmux|rmux> --name <name> [--command <cmd> -g <arg>]`, `session attach <session-id> [--browser]`, `session list [--backend <backend>]`, `session inspect <session-id>`, `session stop <session-id>` |
| `termstage api` | CLI wrapper for semantic operations used by agents and automation. | `api send-text`, `api send-key`, `api run-command --wait-for --capture`, `api read-screen` |
| `termstage auth` | Inspect or manage authentication state. | `auth status`; future `auth login/logout` |

Design rules:

- `--backend <tmux|rmux>` chooses the owner of the actual session/pane/PTY.
  `session create --backend tmux --name abc` creates tmux session
  `TerminalUse-abc`; `session create --backend rmux --name abc` creates rmux
  session `TerminalUse-abc`. That backend session name is also the termstage
  session id.
- `session create --name <name>` returns the backend session id instead of
  writing a local registry file. If `--command` is omitted, the backend starts
  the user's default shell. If `--command <cmd>` is present,
  `-g, --command-arg <arg>` is repeatable and is passed as argv to the backend
  pane startup primitive; it must not be implemented by concatenating a shell
  command string.
- `session list` without `--backend` lists backend sessions visible to
  termstage. For tmux, this means valid tmux session names. The list output
  includes `SESSION_ID`, `BACKEND`, and `DISPLAY_NAME`; it does not repeat a
  separate `BACKEND_SESSION` column because tmux uses the same value.
- `session inspect <session-id>` returns the backend kind, backend
  session/window/pane details when live, the native attach command such as
  `tmux attach -t <backend-session>`, and the browser-mode attach command.
- `session attach <session-id>` invokes the backend's native attach command by
  default. `session attach <session-id> --browser` starts only the browser/API
  gateway for an existing backend session. It must not create backend sessions.
  Resolution first checks the exact session id, then checks
  `TerminalUse-<session-id>` when the requested value is unprefixed. Tmux also
  checks legacy `ts-<session-id>` so existing sessions created by older
  termstage builds remain reachable.
- Backend panes are sized from the invoking terminal at `session create` time
  when that size is detectable. In backend-owned gateway mode, browser viewport
  changes must not resize the backend pane because local native attaches should
  remain governed by the backend client size policy.
- Browser xterm is an embedded component inside the served page. It fits the
  terminal container allocated by the page layout, which currently sits below
  the toolbar and may later sit beside or among other HTML controls.
- `session attach --browser` must not make the xterm DOM grow to the backend pane
  size. When the backend screen is larger than the browser container, the gateway
  projects the backend snapshot into the browser viewport and exposes
  component-local navigation over the larger backend screen.
- Backend-native attaches, such as `tmux attach -t <session>`, are treated as
  terminal control for the browser toolbar. While a native client is attached,
  browser input is read-only and visual-only mouse selection in the native
  terminal is not mirrored to the browser.
- The browser snapshot stream preserves terminal color attributes emitted by
  the backend. Backend-local selection highlights are transient client UI and
  are not part of the shared screen model.
- `session attach --browser` exits when the backend session disappears, for
  example when the tmux session is killed, instead of keeping the web server
  alive indefinitely.
- `termstage`'s invoking terminal remains a supervisor surface for URL, status,
  health, and errors. Local viewing stays backend-native, for example
  `tmux attach -t <session>` or `rmux attach -t <session>`.
- Running `termstage` without a command group must fail with clap's missing
  subcommand error. Root-level gateway flags are not a compatibility alias.

Session identity:

- There is no `$HOME/.termstage/sessions.json` registry. The backend is the
  source of truth for live sessions.
- Termstage-created backend sessions always use the `TerminalUse-` prefix. The
  session id, backend session name, native attach target, and browser-mode attach
  target are the same value, for example `TerminalUse-abc`.
- Tmux sessions created by older termstage builds used `ts-`; attach and inspect
  keep a legacy fallback for those names, but new session creation does not use
  that prefix.
- `session attach --browser` can attach any existing tmux session whose name is
  valid for the termstage protocol, including sessions created outside
  termstage.

## 5. AGENTS.md Binding

- Error handling: CLI uses `anyhow` with context and typed core errors underneath.
- Async/concurrency: Ctrl-C shutdown is coordinated through runtime channels.
- Type design: `CliArgs` validates into separate command/config types such as
  `ValidatedCliCommand` and `ValidatedSessionAttachConfig`.
- Safety/security: no shell string concatenation; loopback-only bind by default;
  public exposure only through [21](./21-browser-terminal-public-exposure-design.md).
- Serialization: N/A.
- Testing: parser tests for invalid session names, bind addresses, browser-mode
  flags, and removed command groups.
- Observability: human-readable logs by default; JSON logs only behind a future flag.
- Performance: startup path avoids network calls and remote dependencies.
- Documentation: CLI help documents the security boundary plainly.

## 6. Cross-References

- Depends on: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md).
- Consumed by: [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
