# Roadmap - Browser Terminal Presentation Mode

Status: draft v1
Owner: termstage
Last updated: 2026-05-19

## 1. Principles

- Always shippable: every milestone leaves the standard Cargo quality gates green.
- Presentation-first: prioritize Chrome-share demo flow over generic web shell features.
- Local security first: the default remains loopback-only; internet-facing pod mode is
  opt-in and requires its dedicated public exposure spec.
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

### M4 - Explicit Public Pod Exposure

User-visible outcome: an operator can run `termstage` inside a pod behind HTTPS
ingress, supply a token from a Kubernetes secret-backed environment variable, and
accept internet traffic only through explicit public-mode validation.

Specs touched: 20, 21, 50, 70, 72.

Exit criteria:

- `--host 0.0.0.0` is rejected unless `--expose-public` is present.
- Public mode requires `--public-url https://...` and `--token-env <NAME>`.
- Launch URLs use the public URL, not the pod bind address.
- Public route tests accept matching public Host/Origin and reject mismatches.
- Local loopback behavior and generated token flow remain unchanged.

Estimate: 0.5-1 focused week.

### M5 - rmux Session Gateway and Semantic Operations

User-visible outcome: an operator can create a termstage session backed by rmux,
open that session in the browser, and also attach natively with
`rmux attach -t <session>`. Browser and API operations mutate the same backend
session through termstage, while termstage's own terminal remains supervisor-only
logs/status/URL output.

Specs touched: 20, 23, 50, 70, 72, 80.

Exit criteria:

- The old local terminal attach/split-TUI path is removed.
- Termstage session ids map to backend session references.
- rmux is the default backend for shared sessions.
- Browser input is translated into backend semantic operations and reflected in
  browser screen updates.
- API `PressKey`, `SendText`, `ReadScreen`, `WaitForText`, and `ExecCommand`
  are available.
- Level 1 lease rejects browser/API write operations from non-owners.
- Native `rmux attach -t <session>` observes the same backend session.

Estimate: 2-4 focused weeks after the runtime tunnel foundation is stable.

## 3. Deferred Milestones

- Read-only mirror clients.
- Stronger remote auth beyond one bearer token, including cookie/header auth,
  rate limiting, audit logging, and viewer/controller authorization.
- Recording/replay.
- Demo command palette and script runner.
- Windows PowerShell support.

Each deferred milestone needs its own spec update before implementation.

## 4. Cross-References

- Depends on: [00-browser-terminal-prd.md](./00-browser-terminal-prd.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
- Pairs with: [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
