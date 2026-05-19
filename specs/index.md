# Specs Index

Status: draft v1
Last updated: 2026-05-19

This spec set defines the browser-based terminal presentation mode for `termstage`.
Read in numbered order for build order, then use the roadmap and implementation plan
to map that order to user-visible milestones.

## Reading Order

| Spec | Type | Purpose |
| --- | --- | --- |
| [00-browser-terminal-prd.md](./00-browser-terminal-prd.md) | PRD | Product problem, users, goals, non-goals, success metrics. |
| [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md) | Data model | WebSocket message contract, session identifiers, validation ranges. |
| [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md) | Runtime design | PTY/session actor model, lifecycle, shutdown, reconnection. |
| [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md) | Web design | Axum server, WebSocket upgrade, static asset serving, browser terminal frontend. |
| [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md) | Public exposure design | Opt-in pod/internet mode, public URL validation, token-env token source. |
| [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md) | CLI design | Command surface, presentation UX, tmux/new-shell modes. |
| [61-browser-terminal-crates-and-features.md](./61-browser-terminal-crates-and-features.md) | Crates/features | Workspace placement, dependency versions, feature policy. |
| [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md) | Security design | Threat model and mandatory local-service controls. |
| [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md) | Verification plan | Unit, integration, browser, and security checks. |
| [80-browser-terminal-glossary.md](./80-browser-terminal-glossary.md) | Glossary | Terms whose meaning affects implementation. |
| [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md) | Roadmap | Stakeholder-facing milestones and exit criteria. |
| [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md) | Implementation plan | Engineer-facing dependency-ordered phases. |
| [99-browser-terminal-key-decisions.md](./99-browser-terminal-key-decisions.md) | Key decisions | Load-bearing decisions and alternatives. |

## Build-Order Graph

```text
                    +----------------+
                    | 00 PRD         |
                    | product scope  |
                    +-------+--------+
                            |
                            v
                    +----------------+
                    | 10 Protocol    |
                    | wire contract  |
                    +-------+--------+
                            |
          +-----------------+-----------------+
          |                                   |
          v                                   v
+-------------------+             +----------------------+
| 11 Runtime Core   |             | 70 Security          |
| PTY/session actor |             | local trust boundary |
+---------+---------+             +----------+-----------+
          |                                  |
          +-----------------+----------------+
                            |
                            v
                    +----------------+
                    | 20 Web Design  |
                    | HTTP/WS/UI     |
                    +-------+--------+
                            |
                 +----------+----------+
                 |                     |
                 v                     v
        +----------------+     +---------------+
        | 21 Public Mode |     | 50 CLI UX     |
        | pod exposure   |     | commands      |
        +-------+--------+     +-------+-------+
                |                      |
                +----------+-----------+
                           |
                           v
                    +----------------+
                    | 61 Crates      |
                    | features/deps  |
                    +-------+--------+
                            |
                            v
                    +----------------+
                    | 72 Verification|
                    | quality gates  |
                    +-------+--------+
                            |
          +-----------------+-----------------+
          |                                   |
          v                                   v
+-------------------+             +----------------------+
| 90 Roadmap        |             | 91 Impl Plan         |
| milestones        |             | dependency phases    |
+-------------------+             +----------------------+
```

## Source Context

Phase 0 validation is recorded in
[../docs/research/browser-terminal-phase-0-validation.md](../docs/research/browser-terminal-phase-0-validation.md).
No committed `vendors/` prior-art source exists yet because Phase 0 only required
local API probes for PTY, xterm.js, Axum WebSockets, and asset bundling.

Project engineering norms are binding through `AGENTS.md`: Rust 2024, no `unsafe`,
no `unwrap()` or `expect()` in production code, structured errors, actor-style runtime
ownership, strict input validation, `tracing`, and the documented Cargo quality gates.
