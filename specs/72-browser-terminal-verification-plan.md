# 72-browser-terminal-verification: Test and Quality Gates

Status: draft v1
Owner: presenterm
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
[11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
[70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md)

## 1. Purpose

This plan defines how browser terminal mode proves correctness before implementation
phases can close. It combines Rust unit/integration tests, browser smoke tests, and
security regression tests.

## 2. Required Gates

Per `AGENTS.md`, every implementation phase must leave these passing:

```text
cargo build
cargo test
cargo +nightly fmt
cargo clippy -- -D warnings -W clippy::pedantic
cargo audit
cargo deny check
```

The existing Makefile may be extended with targets for these gates, but no standalone
ad hoc scripts should become the primary automation entry point.

## 3. Test Matrix

| Area | Tests |
| --- | --- |
| Protocol validation | Valid/invalid resize ranges, unknown JSON fields, frame type handling, redacted token debug. |
| Runtime lifecycle | Start shell, start tmux, attach/detach, resize, child exit, graceful Ctrl-C shutdown. |
| Web routes | `/`, `/assets`, `/ws`, `/healthz`, invalid token, invalid Host, invalid Origin. |
| Browser UI | xterm renders, sends input bytes, receives output bytes, resizes on viewport changes. |
| Terminal compatibility | `printf`, Ctrl-C, paste, `less`, `vim` smoke where environment supports it. |
| Security | Non-loopback bind rejected, peer checks, frame caps, no CDN assets, no token in logs. |

## 4. Browser Verification

Use Playwright once frontend assets exist. Required screenshots:

- Desktop presentation viewport.
- Narrow browser viewport.
- Terminal after command output.

The test must assert that the terminal canvas/DOM is non-empty and that user input
round-trips to PTY output.

## 5. Exit Criteria by Milestone

- M0: CLI starts loopback server, browser connects, interactive shell/tmux works, and
  all security negative route tests pass.
- M1: presentation UX presets pass visual smoke checks and resize tests.
- M2: reconnect to tmux-backed session preserves state across browser refresh.
- M3: hardening gates include `cargo audit`, `cargo deny check`, and documented release
  packaging checks.

## 6. Cross-References

- Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
  [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md).
- Consumed by: [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
