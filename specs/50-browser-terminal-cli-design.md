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
termstage web attach <session-id> --open
```

Session creation arguments:

| Argument | Default | Meaning |
| --- | --- | --- |
| `--backend <tmux|rmux>` | `tmux` | Backend that owns the real session/pane/PTY. |
| `--name <name>` | required | Backend session name. |
| `--command <path>` | `$SHELL` on Unix | Command executable for the first backend pane. |
| `-g, --command-arg <arg>` | unset | Repeatable argv tail for `--command`. |

Web attach arguments:

| Argument | Default | Meaning |
| --- | --- | --- |
| `<session-id>` | required | Termstage session id returned by `session create`. |
| `--host <addr>` | `127.0.0.1` | Bind address; non-loopback requires `--expose-public`. |
| `--port <port>` | `0` | Port `0` means OS-chosen random port. |
| `--open` | false | Open the tokenized URL in the default browser. |
| `--font-size <px>` | `24` | Browser terminal font size. |
| `--theme <name>` | `high-contrast` | Presentation theme preset. |
| `--keepalive <policy>` | `session` | Keep tmux session after browser disconnect. |
| `--expose-public` | false | Enable pod/internet mode; required before non-loopback bind addresses are accepted. |
| `--public-url <url>` | unset | HTTPS browser-visible base URL for public mode. |
| `--token-env <name>` | unset | Environment variable containing a 64-hex-character access token for public mode. |

## 2a. User Flow

```text
CLI                   Registry             Backend             Browser
 |                       |                    |                    |
 | 1. session create     |                    |                    |
 | 2. validate name/cmd  |                    |                    |
 |------------------------------------------->| 3. create pane     |
 | 4. persist id ------->|                    |                    |
 | 5. print id/attach    |                    |                    |
 |                       |                    |                    |
 | 6. web attach <id>    |                    |                    |
 | 7. resolve id ------->|                    |                    |
 | 8. attach gateway -----------------------> |                    |
 | 9. open URL -------------------------------------------------->|
 |                       |                    |<-------------------| 10. WS connect
```

## 3. Invariants

- CLI never accepts a non-loopback bind address unless `--expose-public` is present
  and [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md)
  validation succeeds.
- CLI arguments crossing trust boundaries are validated before server startup.
- `--name` is a backend session name, not a shell command.
- `--command` is an executable path and `-g` / `--command-arg` is repeatable argv
  handling, not a string passed through `sh -c`.
- Shell mode is browser-first. The invoking terminal remains the `termstage`
  supervisor console and does not attach to the command PTY.
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

The CLI exits on server shutdown. Ctrl-C initiates graceful shutdown: close browser
connections, stop runtime actors according to keepalive policy, and join tasks.

## 4a. CLI Command Groups

The current `--mode <tmux|shell>` surface is a compatibility shape. The
backend-session gateway design should move the CLI toward explicit subcommands
and make backend selection separate from command/session creation.

Target top-level command groups:

| Group | Purpose | Initial commands |
| --- | --- | --- |
| `termstage session` | Create and manage backend-owned sessions. Each created session receives a stable termstage session id persisted under `$HOME/.termstage`. | `session create --backend <tmux|rmux> --name <name> [--command <cmd> -g <arg>]`, `session list [--backend <backend>]`, `session inspect <session-id>`, `session stop <session-id> --detach|--kill` |
| `termstage api` | CLI wrapper for semantic operations used by agents and automation. | `api send-text`, `api send-key`, `api run-command --wait-for --capture`, `api read-screen` |
| `termstage web` | Attach browser/API gateway surfaces to an existing termstage session id and manage URL/token helper surfaces. | `web attach <session-id> --open`, `web url`, `web token generate`; future token revocation |
| `termstage auth` | Inspect or manage authentication state. | `auth status`; future `auth login/logout` |

Design rules:

- `--backend <tmux|rmux>` chooses the owner of the actual session/pane/PTY. It
  is required when creating a session and can be omitted for list/inspect/stop
  because the persisted termstage session id resolves the backend.
- `session create --name <name>` names the backend session. The command returns
  a separate generated termstage session id. If `--command` is omitted, the
  backend starts the user's default shell. If `--command <cmd>` is present,
  `-g, --command-arg <arg>` is repeatable and is passed as argv to the backend
  pane startup primitive; it must not be implemented by concatenating a shell
  command string.
- `session list` without `--backend` lists all persisted termstage sessions
  across supported backends. `session inspect <session-id>` returns the backend
  kind, backend session/window/pane details when live, and the native attach
  command such as `tmux attach -t <backend-session>`.
- `web attach <session-id>` starts only the browser/API gateway for an existing
  termstage session id. It must not create backend sessions. If the session id
  is unknown or the backend session no longer exists, startup fails with a clear
  error.
- Backend panes are sized from the invoking terminal at `session create` time
  when that size is detectable. In backend-owned gateway mode, browser viewport
  changes must not resize the backend pane because local native attaches should
  remain governed by the backend client size policy.
- Backend-native attaches, such as `tmux attach -t <session>`, are treated as
  terminal control for the browser toolbar. While a native client is attached,
  browser input is read-only and visual-only mouse selection in the native
  terminal is not mirrored to the browser.
- The browser snapshot stream preserves terminal color attributes emitted by
  the backend. Backend-local selection highlights are transient client UI and
  are not part of the shared screen model.
- `web attach` exits when the backend session disappears, for example when the
  tmux session is killed, instead of keeping the web server alive indefinitely.
- `termstage`'s invoking terminal remains a supervisor surface for URL, status,
  health, and errors. Local viewing stays backend-native, for example
  `tmux attach -t <session>` or `rmux attach -t <session>`.
- Running `termstage` without a command group must fail with clap's missing
  subcommand error. Root-level gateway flags are not a compatibility alias.

Registry persistence:

- A local registry under `$HOME/.termstage` stores termstage-created session ids
  and non-secret metadata, such as backend kind, backend session reference,
  creation timestamp, display name, and startup argv. It must not store bearer
  tokens in cleartext.
- Local single-host persistence should use the platform state/config directory
  with owner-only permissions and atomic file replacement. EKS or multi-process
  deployments should use a shared control-plane store instead of a per-pod
  `~/.termstage/session.json` file.

## 5. AGENTS.md Binding

- Error handling: CLI uses `anyhow` with context and typed core errors underneath.
- Async/concurrency: Ctrl-C shutdown is coordinated through runtime channels.
- Type design: `CliArgs` validates into a separate `ValidatedCliConfig`.
- Safety/security: no shell string concatenation; loopback-only bind by default;
  public exposure only through [21](./21-browser-terminal-public-exposure-design.md).
- Serialization: N/A.
- Testing: parser tests for invalid session names, bind addresses, ports, and shell
  paths; smoke test for default config.
- Observability: human-readable logs by default; JSON logs only behind a future flag.
- Performance: startup path avoids network calls and remote dependencies.
- Documentation: CLI help documents the security boundary plainly.

## 6. Cross-References

- Depends on: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md).
- Consumed by: [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
