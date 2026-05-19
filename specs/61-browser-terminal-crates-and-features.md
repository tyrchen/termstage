# 61-browser-terminal-crates: Crates, Features, and Dependencies

Status: draft v1
Owner: termstage
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
[11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md)

## 1. Purpose

This spec pins the workspace placement and dependency policy for browser terminal
mode. It records dependency versions verified during spec drafting so implementation
starts from current packages instead of stale examples.

## 2. Workspace Layout

```text
termstage/
  crates/core/
    src/browser_terminal/
      protocol.rs
      runtime.rs
      security.rs
      types.rs
  apps/server/
    src/
      cli.rs
      web.rs
      assets.rs
      main.rs
    web/
      package.json
      src/
        terminal.ts
        socket.ts
        resize.ts
        presentation.ts
```

`termstage-core` owns reusable domain contracts. `termstage` owns integration
code that depends on Axum, browser asset tooling, and process spawning.

## 3. Dependency Baseline

Versions verified on 2026-05-19. Phase 0 validation evidence is in
[../docs/research/browser-terminal-phase-0-validation.md](../docs/research/browser-terminal-phase-0-validation.md).

| Dependency | Version | Owner | Feature notes |
| --- | --- | --- | --- |
| `axum` | `0.8.9` | `termstage` | Use `ws`; avoid unnecessary optional features. |
| `portable-pty` | `0.9.0` | `termstage` initially | PTY creation/spawn/resize; wrap behind core-owned traits if needed. |
| `tower-http` | `0.6.11` | `termstage` | Use trace and static asset helpers only when feature-scoped. |
| `tokio` | workspace | both | Explicit features; runtime already in workspace. |
| `thiserror` | workspace | `termstage-core` | Domain errors. |
| `anyhow` | workspace | `termstage` | CLI/application context. |
| `serde` / `serde_json` | workspace | both | Control message protocol. |
| `bytes` | latest compatible `1` | both | Efficient terminal frame payloads. |
| `secrecy` | `0.10.3` | core/server | Token redaction and explicit exposure. |
| `getrandom` | `0.3` | core/server | CSPRNG token generation. |
| `@xterm/xterm` | `6.0.0` | frontend | Use scoped package; legacy `xterm` package is deprecated. |
| `@xterm/addon-fit` | `0.11.0` | frontend | Fit terminal to browser viewport. |
| `@xterm/addon-web-links` | `0.12.0` | frontend | Link detection. |
| `@xterm/addon-webgl` | `0.19.0` | frontend optional | Optional renderer feature, not required for M0. |

### 3.1 Validated API Shapes

`portable-pty`:

- Use `native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })`.
- Spawn commands with `CommandBuilder` and argv arguments only.
- Use `try_clone_reader()` for PTY output and `take_writer()` for PTY input.
- Apply resize through the PTY master handle.
- Launch managed tmux commands through `/usr/bin/env -u TMUX tmux ...` so an inherited
  tmux environment cannot redirect the command to the presenter's outer session.

`@xterm/xterm`:

- Import from scoped packages: `@xterm/xterm`, `@xterm/addon-fit`,
  `@xterm/addon-web-links`, and optional `@xterm/addon-webgl`.
- Load addons with `terminal.loadAddon(new FitAddon())` style APIs.
- Include Vite client types for xterm CSS side-effect imports.

`axum`:

- Use `axum = { version = "0.8.9", default-features = false, features = ["ws", "http1"] }`
  for the first HTTP server phase.
- Configure `WebSocketUpgrade::max_frame_size` and `max_message_size` before
  `on_upgrade`.
- Use `futures-util` stream/sink traits for split send/receive halves.

## 4. Feature Policy

- Cargo dependencies live in `[workspace.dependencies]` when shared by more than one
  crate.
- Optional features are explicit and minimal. Do not enable `tokio = "full"` unless a
  phase proves each feature is needed.
- Frontend packages are pinned in lockfiles. No CDN scripts in production.
- Build frontend assets with Vite under `apps/server/web`, commit `package-lock.json`,
  and enable manifest output for hashed asset lookup.
- Makefile targets for frontend install/build exist before the frontend lands and are
  no-ops until `apps/server/web/package.json` is present.
- Add browser test targets when Playwright lands.
- Security tooling follows `AGENTS.md`: `cargo audit` and `cargo-deny` are required
  quality gates before release.

## 5. Cross-References

- Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
  [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md).
- Consumed by: [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
