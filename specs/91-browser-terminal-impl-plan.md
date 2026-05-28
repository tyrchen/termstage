# Implementation Plan - Browser Terminal Presentation Mode

Status: draft v1
Owner: termstage
Last updated: 2026-05-28

## 0. Readiness Assessment

The repo has a Rust 2024 workspace with `crates/core` and `apps/server`, but browser
terminal mode has no committed implementation yet. No `docs/research/` memo or
vendored prior-art source exists. Phase 0 must validate the assumptions from the
initial research before production code starts.

## 1. Why Dependency Order Differs From Feature Order

The protocol lands before the UI because both runtime and frontend depend on the same
binary/text WebSocket contract.

The security boundary lands before the WebSocket bridge because a working local shell
bridge without token/Host/Origin checks is the highest-risk failure mode.

The runtime actor lands before presentation polish because UI controls are only useful
once PTY lifecycle, resize, and shutdown semantics are stable.

## 2. Estimated Total Effort

M0-M3 is approximately 5-7 focused developer weeks. Calendar time may be longer if
frontend asset bundling, tmux portability, or CI security tooling requires iteration.

## 3. Phase 0 - Risk Retirement

Status: complete. Validation evidence is recorded in
[../docs/research/browser-terminal-phase-0-validation.md](../docs/research/browser-terminal-phase-0-validation.md).

| # | Deliverable | Lands in | Effort |
| --- | --- | --- | --- |
| 0.1 | Validate `portable-pty` spawn, read, write, resize on macOS with zsh and tmux. | Research memo or implementation note | 0.5 day |
| 0.2 | Validate `@xterm/xterm` 6.x APIs and addon versions with a minimal Vite or equivalent build. | Research memo or implementation note | 0.5 day |
| 0.3 | Validate Axum 0.8 WebSocket split/send/receive and frame-size configuration. | Research memo or implementation note | 0.5 day |
| 0.4 | Decide asset bundling mechanism and Makefile targets. | 61, Makefile design update | 0.5 day |

Exit gate: no production browser-terminal code until the validated API shapes are
reflected in the specs or implementation notes.

## 4. Phase 1 - Foundation Contracts

Maps to roadmap: starts M0.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 1.1 | Add protocol newtypes, validation, serde control messages, and redacted token type. | [10](./10-browser-terminal-protocol-design.md) | 1 day |
| 1.2 | Add security validation types for Host, Origin, peer address, and token comparison. | [70](./70-browser-terminal-security-design.md) | 1 day |
| 1.3 | Add unit tests for every validation boundary and redaction invariant. | [72](./72-browser-terminal-verification-plan.md) | 0.5 day |

Exit criteria: protocol/security unit tests pass; no production `unwrap()`/`expect()`;
public types have documentation.

## 5. Phase 2 - PTY Runtime

Maps to roadmap: continues M0.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 2.1 | Implement session actor command loop and bounded client mailboxes. | [11](./11-browser-terminal-runtime-design.md) | 1.5 days |
| 2.2 | Implement shell mode for local smoke tests. | [11](./11-browser-terminal-runtime-design.md) | 1 day |
| 2.3 | Implement tmux mode with validated session names. | [11](./11-browser-terminal-runtime-design.md) | 1 day |
| 2.4 | Add runtime lifecycle tests for start, resize, detach, child exit, and shutdown. | [72](./72-browser-terminal-verification-plan.md) | 1 day |

Exit criteria: runtime integration tests pass on macOS with shell mode and tmux mode.

## 6. Phase 3 - Web Bridge and CLI

Maps to roadmap: closes M0.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 3.1 | Add Axum routes, loopback bind, token URL, and security middleware/checks. | [20](./20-browser-terminal-web-design.md), [70](./70-browser-terminal-security-design.md) | 1.5 days |
| 3.2 | Add WebSocket bridge from frames to runtime commands and runtime output to browser. | [10](./10-browser-terminal-protocol-design.md), [20](./20-browser-terminal-web-design.md) | 1.5 days |
| 3.3 | Add CLI parsing and `termstage --session presentation --open`. | [50](./50-browser-terminal-cli-design.md) | 1 day |
| 3.4 | Add route/security integration tests. | [72](./72-browser-terminal-verification-plan.md) | 1 day |

Exit criteria: M0 roadmap criteria pass end to end.

## 7. Phase 4 - Presentation UX

Maps to roadmap: closes M1.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 4.1 | Add frontend terminal app with xterm.js, fit addon, and custom socket protocol. | [20](./20-browser-terminal-web-design.md) | 1.5 days |
| 4.2 | Add presentation theme/font presets and CLI plumbing. | [50](./50-browser-terminal-cli-design.md) | 1 day |
| 4.3 | Make xterm an embedded component that fits its terminal container rather than the whole page. | [20](./20-browser-terminal-web-design.md), [72](./72-browser-terminal-verification-plan.md) | 0.5 day |
| 4.4 | Add Playwright smoke tests and screenshots. | [72](./72-browser-terminal-verification-plan.md) | 1 day |

Exit criteria: M1 roadmap criteria pass and screenshots are non-empty across target
viewports. The page can contain toolbar/future HTML around the terminal without
changing xterm sizing semantics.

## 8. Phase 5 - Reconnect and Hardening

Maps to roadmap: closes M2 and M3.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 5.1 | Implement reconnect policy and browser refresh behavior. | [11](./11-browser-terminal-runtime-design.md), [20](./20-browser-terminal-web-design.md) | 1.5 days |
| 5.2 | Implement backpressure behavior and tests. | [11](./11-browser-terminal-runtime-design.md), [72](./72-browser-terminal-verification-plan.md) | 1 day |
| 5.3 | Add Makefile quality gate targets for fmt, clippy, audit, deny, frontend tests. | [61](./61-browser-terminal-crates-and-features.md), [72](./72-browser-terminal-verification-plan.md) | 1 day |
| 5.4 | Update CLI help/docs for local-only safety boundary. | [50](./50-browser-terminal-cli-design.md), [70](./70-browser-terminal-security-design.md) | 0.5 day |

Exit criteria: M2 and M3 roadmap criteria pass.

## 9. What Makes This Order Correct

Security and protocol invariants are foundation work because every later component
consumes them. PTY runtime is next because it proves real terminal semantics before UI
polish. The browser UI is later because its correct behavior depends on the runtime
and WebSocket contract, not the other way around.

## 10. Phase 6 - Explicit Public Pod Exposure

Maps to roadmap: closes M4.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 6.1 | Add exposure-mode configuration, public base URL validation, and public Host/Origin/peer checks. | [20](./20-browser-terminal-web-design.md), [21](./21-browser-terminal-public-exposure-design.md), [70](./70-browser-terminal-security-design.md) | 1 day |
| 6.2 | Add CLI flags `--expose-public`, `--public-url`, and `--token-env`, including token env-name/value validation. | [21](./21-browser-terminal-public-exposure-design.md), [50](./50-browser-terminal-cli-design.md) | 1 day |
| 6.3 | Add route and CLI tests covering public-mode required flags, launch URL construction, matching Host/Origin acceptance, and mismatch rejection. | [72](./72-browser-terminal-verification-plan.md) | 1 day |

Exit criteria: M4 roadmap criteria pass; local loopback tests still pass; standard
Cargo quality gates remain green.

## 11. Phase 7 - Remove Obsolete Local Attach

Maps to roadmap: closes M5.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 7.1 | Rewrite the local/remote command lease spec around backend-owned sessions, Termstage Protocol, and Level 1 operation locking. | [23](./23-local-remote-command-lease-design.md) | 0.5 day |
| 7.2 | Remove the CLI flag and config fields for the obsolete local attach mode. | [23](./23-local-remote-command-lease-design.md), [50](./50-browser-terminal-cli-design.md) | 0.5 day |
| 7.3 | Delete the server-side local terminal passthrough implementation and module wiring. | [23](./23-local-remote-command-lease-design.md) | 0.5 day |
| 7.4 | Remove runtime commands, state, tests, and docs that model the invoking terminal as a write-capable frontend. | [11](./11-browser-terminal-runtime-design.md), [23](./23-local-remote-command-lease-design.md), [72](./72-browser-terminal-verification-plan.md) | 1 day |
| 7.5 | Verify shell mode with `--command` remains browser-first and exits according to the configured exit policy. | [50](./50-browser-terminal-cli-design.md), [72](./72-browser-terminal-verification-plan.md) | 0.5 day |

Exit criteria: M5 roadmap criteria pass; repository search finds no obsolete
local attach symbols; standard Cargo quality gates remain green.

## 12. Phase 8 - Session Backend Gateway and Level 1 Lock

Maps to roadmap: closes M6.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 8.1 | Add an in-memory gateway registry that maps request session ids to backend session/window/pane references for the active web/API gateway process. | [23](./23-local-remote-command-lease-design.md), [11](./11-browser-terminal-runtime-design.md) | 1 day |
| 8.2 | Define the backend adapter trait and implement the first rmux/tmux adapter path for create/find, stream output, write input, resize, and read-screen. | [23](./23-local-remote-command-lease-design.md), [61](./61-browser-terminal-crates-and-features.md) | 2 days |
| 8.3 | Route browser WebSocket traffic through Termstage Protocol into the backend adapter. | [10](./10-browser-terminal-protocol-design.md), [20](./20-browser-terminal-web-design.md), [23](./23-local-remote-command-lease-design.md) | 1.5 days |
| 8.4 | Add Semantic Operations API for press-key, write-text, run-command, read-screen, and scroll. | [23](./23-local-remote-command-lease-design.md), [70](./70-browser-terminal-security-design.md) | 2 days |
| 8.5 | Implement Level 1 operation lock with owner kind, owner id, epoch, TTL, and conflict responses. | [23](./23-local-remote-command-lease-design.md), [72](./72-browser-terminal-verification-plan.md) | 1 day |
| 8.6 | Implement backend-screen to browser-viewport projection for backend-owned gateway sessions. Browser resize updates viewport state and must not resize the backend pane. | [20](./20-browser-terminal-web-design.md), [23](./23-local-remote-command-lease-design.md), [72](./72-browser-terminal-verification-plan.md) | 1 day |
| 8.7 | Add integration tests for browser/API synchronization, lock conflict, native backend attach compatibility, viewport projection, and semantic request/response behavior. | [23](./23-local-remote-command-lease-design.md), [72](./72-browser-terminal-verification-plan.md) | 1.5 days |

Exit criteria: M6 roadmap criteria pass; browser and Agent API operate the same
backend session; exactly one controller can write at a time; backend-native local
attach does not share stdout/stderr with `termstage`; backend screens larger than
the browser terminal container remain navigable without resizing the backend pane.

## 13. Phase 9 - CLI Command Groups

Maps to roadmap: follow-up after M6.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 9.1 | Move backend session creation under `termstage session create --backend <backend> --name <name> [--command <cmd>]`, use the backend session id as the termstage session id, and reject root-level startup aliases with clap's missing/unknown subcommand errors. | [50](./50-browser-terminal-cli-design.md), [23](./23-local-remote-command-lease-design.md) | 1 day |
| 9.2 | Introduce argv-safe tmux pane startup for `session create --command <cmd> -g <arg>` while keeping `--mode shell` as the compatibility path for browser-only command runs. | [50](./50-browser-terminal-cli-design.md), [23](./23-local-remote-command-lease-design.md) | 1 day |
| 9.3 | Add `termstage session` commands for list, inspect, and stop `--detach|--kill`; inspect returns backend-native attach info, so a separate attach-info command is unnecessary. | [50](./50-browser-terminal-cli-design.md), [70](./70-browser-terminal-security-design.md) | 2 days |
| 9.4 | Add `termstage api` commands for send-text, send-key, run-command wait/capture, and read-screen as CLI wrappers over the semantic API. | [50](./50-browser-terminal-cli-design.md), [23](./23-local-remote-command-lease-design.md) | 2 days |
| 9.5 | Add `termstage web attach <session-id>` for browser/API gateway attachment to an existing session id, and reserve `termstage auth` for future OIDC login/logout/status flows. | [50](./50-browser-terminal-cli-design.md), [21](./21-browser-terminal-public-exposure-design.md) | 1 day |

Exit criteria: the CLI help is organized by `session`, `api`, `web`, and `auth`
command groups; tmux session ids resolve directly from tmux with `ts-` fallback
for unprefixed names; browser gateway attachment does not create backend
sessions; legacy root invocation is rejected; parser tests cover command
grouping and invalid flag placement.

## 14. Cross-References

- Depends on: all numbered browser terminal specs.
- Pairs with: [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md).
