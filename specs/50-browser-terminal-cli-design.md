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
| `--command <path>` | `$SHELL` on Unix | Command executable for shell mode only. |
| `-g, --command-arg <arg>` | unset | Repeatable argv tail for shell mode only. |
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

- CLI never accepts a non-loopback bind address unless `--expose-public` is present
  and [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md)
  validation succeeds.
- CLI arguments crossing trust boundaries are validated before server startup.
- `--session` is a tmux session name, not a shell command.
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
