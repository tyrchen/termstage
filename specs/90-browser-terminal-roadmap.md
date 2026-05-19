# Roadmap - Browser Terminal Presentation Mode

Status: draft v1
Owner: termstage
Last updated: 2026-05-19

## 1. Principles

- Always shippable: every milestone leaves the standard Cargo quality gates green.
- Presentation-first: prioritize Chrome-share demo flow over generic web shell features.
- Local security first: remote sharing is out of scope until a dedicated security spec
  exists.
- Terminal compatibility by byte stream: do not regress to command-string transport.

## 2. Milestones

### M0 - Local Browser Terminal

User-visible outcome: a presenter runs `termstage --session presentation --open`,
Chrome opens, and the tab controls a local tmux-backed terminal.

Specs touched: 00, 10, 11, 20, 50, 61, 70, 72.

Exit criteria:

- Loopback server starts on a random port.
- Tokenized browser URL opens or is printed once if browser launch fails.
- WebSocket bridges binary terminal bytes and resize messages.
- Tmux mode can run commands, Ctrl-C, paste, resize, and preserve session after refresh.
- Security checks reject invalid token, Host, Origin, and non-loopback peers.

Estimate: 2-3 focused weeks for one developer after Phase 0 risk retirement.

### M1 - Presentation-Ready UX

User-visible outcome: terminal readability and layout fit live talks and screen shares.

Specs touched: 20, 50, 72.

Exit criteria:

- Large-font and high-contrast presets are available through CLI flags.
- Terminal fits desktop and narrow browser viewports without clipped text.
- Browser smoke screenshots show non-empty terminal content and successful input/output.
- UI contains no explanatory landing page before the usable terminal.

Estimate: 1 focused week.

### M2 - Reconnect and Session Reliability

User-visible outcome: browser refresh or temporary tab disconnect does not destroy the
demo session.

Specs touched: 11, 20, 50, 72.

Exit criteria:

- Reconnect to the same tmux session works after browser refresh.
- Runtime actor cleanup leaves no leaked tasks in integration tests.
- Backpressure closes slow browser clients without killing the tmux session.

Estimate: 1-2 focused weeks.

### M3 - Release Hardening

User-visible outcome: the tool is packageable and has clear safety boundaries.

Specs touched: 61, 70, 72.

Exit criteria:

- `cargo audit` and `cargo deny check` pass.
- Makefile exposes the full quality gate.
- Static assets are bundled or built reproducibly.
- CLI help and docs state local-only security assumptions.

Estimate: 1 focused week.

## 3. Deferred Milestones

- Read-only mirror clients.
- Remote/LAN sharing with WSS and stronger auth.
- Recording/replay.
- Demo command palette and script runner.
- Windows PowerShell support.

Each deferred milestone needs its own spec update before implementation.

## 4. Cross-References

- Depends on: [00-browser-terminal-prd.md](./00-browser-terminal-prd.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
- Pairs with: [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
