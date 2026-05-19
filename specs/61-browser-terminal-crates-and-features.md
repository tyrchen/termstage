# 61-browser-terminal-crates: Crates, Features, and Dependencies

Status: draft v1
Owner: presenterm
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
[11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md)

## 1. Purpose

This spec pins the workspace placement and dependency policy for browser terminal
mode. It records dependency versions verified during spec drafting so implementation
starts from current packages instead of stale examples.

## 2. Workspace Layout

```text
presenterm/
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

`presenterm-core` owns reusable domain contracts. `presenterm-server` owns integration
code that depends on Axum, browser asset tooling, and process spawning.

## 3. Dependency Baseline

Versions verified on 2026-05-19:

| Dependency | Version | Owner | Feature notes |
| --- | --- | --- | --- |
| `axum` | `0.8.9` | `presenterm-server` | Use `ws`; avoid unnecessary optional features. |
| `portable-pty` | `0.9.0` | `presenterm-server` initially | PTY creation/spawn/resize; wrap behind core-owned traits if needed. |
| `tower-http` | `0.6.11` | `presenterm-server` | Use trace and static asset helpers only when feature-scoped. |
| `tokio` | workspace | both | Explicit features; runtime already in workspace. |
| `thiserror` | workspace | `presenterm-core` | Domain errors. |
| `anyhow` | workspace | `presenterm-server` | CLI/application context. |
| `serde` / `serde_json` | workspace | both | Control message protocol. |
| `bytes` | latest compatible `1` | both | Efficient terminal frame payloads. |
| `secrecy` | `0.10.3` | core/server | Token redaction and explicit exposure. |
| `getrandom` | `0.3` | core/server | CSPRNG token generation. |
| `@xterm/xterm` | `6.0.0` | frontend | Use scoped package; legacy `xterm` package is deprecated. |
| `@xterm/addon-fit` | `0.11.0` | frontend | Fit terminal to browser viewport. |
| `@xterm/addon-web-links` | `0.12.0` | frontend | Link detection. |
| `@xterm/addon-webgl` | `0.19.0` | frontend optional | Optional renderer feature, not required for M0. |

## 4. Feature Policy

- Cargo dependencies live in `[workspace.dependencies]` when shared by more than one
  crate.
- Optional features are explicit and minimal. Do not enable `tokio = "full"` unless a
  phase proves each feature is needed.
- Frontend packages are pinned in lockfiles. No CDN scripts in production.
- Add Makefile targets for frontend build and browser tests when they land.
- Security tooling follows `AGENTS.md`: `cargo audit` and `cargo-deny` are required
  quality gates before release.

## 5. Cross-References

- Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
  [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md).
- Consumed by: [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
