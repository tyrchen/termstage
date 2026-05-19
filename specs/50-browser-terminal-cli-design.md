# 50-browser-terminal-cli: Command Surface and Presentation UX

Status: draft v1
Owner: termstage
Depends on: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md)

## 1. Purpose

The CLI turns the runtime and web components into a presenter-friendly workflow. It
owns argument parsing, defaults, browser opening, and terminal session mode selection.

## 2. Interface

M0 command:

```text
termstage --session presentation --open
```

Planned arguments:

| Argument | Default | Meaning |
| --- | --- | --- |
| `--session <name>` | `presentation` | Attach/create a tmux session with the validated name. |
| `--mode <tmux|shell>` | `tmux` | Choose shared tmux or fresh shell mode. |
| `--shell <path>` | `$SHELL` on Unix | Shell executable for shell mode only. |
| `--host <loopback>` | `127.0.0.1` | Bind address; only loopback accepted before remote-share mode exists. |
| `--port <port>` | `0` | Port `0` means OS-chosen random port. |
| `--open` | false | Open the tokenized URL in the default browser. |
| `--font-size <px>` | `24` | Browser terminal font size. |
| `--theme <name>` | `high-contrast` | Presentation theme preset. |
| `--keepalive <policy>` | `session` | Keep tmux session after browser disconnect. |

## 2a. User Flow

```text
CLI                 Server              Browser              Runtime
 |                    |                    |                    |
 | 1. parse args      |                    |                    |
 | 2. validate names  |                    |                    |
 | 3. bind loopback ->|                    |                    |
 |                    | 4. mint token      |                    |
 | 5. open URL ------>|------------------->|                    |
 |                    |                    | 6. load assets     |
 |                    |<-------------------| 7. WS connect      |
 |                    | 8. validate        |                    |
 |                    |----------------------------------------->|
 |                    |                    |                    | 9. start tmux
 |                    |<-----------------------------------------|
 |                    |------------------->| 10. terminal ready |
```

## 3. Invariants

- CLI never accepts a non-loopback bind address until a dedicated remote-sharing spec
  exists.
- CLI arguments crossing trust boundaries are validated before server startup.
- `--session` is a tmux session name, not a shell command.
- `--shell` is an executable path plus argv handling, not a string passed through
  `sh -c`.
- Browser URL printed to logs redacts token unless the output is the explicit user
  launch URL.

## 4. Behavior

If `tmux` is missing in tmux mode, startup fails with a clear actionable error. It
does not silently fall back to a new shell because that would break the shared-session
presentation promise.

If browser opening fails, the CLI prints the tokenized URL once to stderr/stdout using
the chosen logging/output convention. Structured logs still redact the token.

The CLI exits on server shutdown. Ctrl-C initiates graceful shutdown: close browser
connections, stop runtime actors according to keepalive policy, and join tasks.

## 5. AGENTS.md Binding

- Error handling: CLI uses `anyhow` with context and typed core errors underneath.
- Async/concurrency: Ctrl-C shutdown is coordinated through runtime channels.
- Type design: `CliArgs` validates into a separate `ValidatedCliConfig`.
- Safety/security: no shell string concatenation; loopback-only bind.
- Serialization: N/A.
- Testing: parser tests for invalid session names, bind addresses, ports, and shell
  paths; smoke test for default config.
- Observability: human-readable logs by default; JSON logs only behind a future flag.
- Performance: startup path avoids network calls and remote dependencies.
- Documentation: CLI help documents the security boundary plainly.

## 6. Cross-References

- Depends on: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md).
- Consumed by: [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
