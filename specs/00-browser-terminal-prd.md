# PRD - Browser Terminal Presentation Mode

Status: draft v1
Owner: termstage
Last updated: 2026-05-19

## 1. Problem

Technical presentations often happen from a shared Chrome window: slides, docs,
GitHub, dashboards, and demo pages are already browser tabs. When a presenter needs
a terminal, they either stop and restart sharing, share the entire desktop, or keep
the terminal in a browser-hosted tool that is not connected to the local demo
environment. All three options interrupt flow or expose more of the presenter's
machine than intended.

The required job is narrower than "web terminal for sysadmins": give a presenter a
local browser tab that behaves like a real terminal and can drive a local shell or
tmux session during a live demo.

## 2. Vision

`termstage --session presentation --open` starts a local-only Rust service on a
random loopback port, opens a Chrome tab containing an xterm.js terminal, and bridges
browser terminal bytes to a PTY-backed shell or shared tmux session. The presenter can
keep sharing Chrome while using slides and terminal tabs side by side.

```text
Presenter workflow

1. termstage --session presentation --open
2. Chrome opens http://127.0.0.1:<random>/?token=<redacted>
3. Browser terminal attaches to tmux session "presentation"
4. Presenter runs demo commands without switching Zoom share targets
```

## 3. Goals

| # | Goal | Measure |
| --- | --- | --- |
| G1 | Start a browser terminal for a local demo quickly. | Fresh checkout can run the M0 command and reach an interactive shell in <= 10 seconds after build. |
| G2 | Preserve real terminal semantics. | Ctrl-C, Ctrl-D, arrow keys, tab completion, paste, resize, `vim`, `less`, and `tmux` work through PTY byte streaming. |
| G3 | Keep the shell boundary local by default. | Server binds only loopback, requires a random token, validates Host/Origin, and rejects non-loopback peers in M0. |
| G4 | Optimize for presentation readability. | M1 ships large-font presets, high-contrast themes, fit-to-container embedded terminal layout, and no in-app instructional clutter. |
| G5 | Keep implementation auditable. | Protocol, security checks, session lifecycle, and CLI behavior are covered by automated tests in the standard quality gates. |

## 4. Non-Goals

- No public internet sharing in M0 or M1.
- No account system, cloud relay, browser extension, or hosted control plane.
- No attempt to attach to an arbitrary existing Terminal.app/iTerm2 window.
- No custom terminal emulator implementation in Rust or JavaScript.
- No shell sandbox claim; the tool controls a real local shell with the user's OS permissions.
- No multi-user collaborative editing in the initial roadmap.

## 5. Users

Primary user: a developer, teacher, or conference speaker presenting from Chrome who
needs to run local commands during a live demo.

Secondary user: a screencast or workshop author who wants a readable browser-native
terminal and a repeatable tmux session.

Anti-persona: an operator who wants to expose an admin shell over the network. That
use case requires a different authentication, authorization, TLS, audit, and hardening
model.

## 6. Success Metrics

- Demo setup time: median time from command invocation to interactive browser terminal.
- Presentation continuity: presenter can keep a single Chrome share active for slides
  and terminal demos.
- Terminal compatibility: smoke matrix passes for common interactive programs.
- Security posture: all local-service security tests in
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md)
  pass before release.
- Reliability: reconnect to an existing tmux-backed session without losing session
  state after browser refresh.

## 7. Naming Conventions

- Product command: `termstage`.
- Feature name in specs and code modules: `browser_terminal`.
- Default tmux session name: `presentation`.
- Browser route names: `/`, `/assets/*`, `/ws`.
- Rust workspace ownership:
  - `termstage-core`: protocol, validation newtypes, session/runtime contracts.
  - `termstage`: CLI, Axum routes, static asset serving, PTY process wiring.

## 8. Cross-References

- Depends on: user-provided initial research in the request.
- Consumed by: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
  [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
