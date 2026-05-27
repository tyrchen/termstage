# Implementation Plan - Browser Terminal Presentation Mode

Status: draft v1
Owner: termstage
Last updated: 2026-05-19

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
| 4.3 | Add Playwright smoke tests and screenshots. | [72](./72-browser-terminal-verification-plan.md) | 1 day |

Exit criteria: M1 roadmap criteria pass and screenshots are non-empty across target
viewports.

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

## 11. Phase 7 - Remove Local Terminal Attach

Maps to roadmap: closes M5.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 7.1 | Remove `--attach-local-terminal` and local command terminal flags from the CLI surface; add parser tests that reject stale flags. | [23](./23-local-remote-command-lease-design.md), [50](./50-browser-terminal-cli-design.md) | 0.5 day |
| 7.2 | Delete local terminal frontend code that puts the invoking terminal in raw mode or alternate screen. | [23](./23-local-remote-command-lease-design.md) | 0.5 day |
| 7.3 | Ensure termstage stdout/stderr are supervisor-only: logs, launch URL, status, health, and errors. | [23](./23-local-remote-command-lease-design.md), [70](./70-browser-terminal-security-design.md) | 0.5 day |
| 7.4 | Preserve existing browser terminal behavior until the rmux session backend lands. | [20](./20-browser-terminal-web-design.md), [72](./72-browser-terminal-verification-plan.md) | 1 day |

Exit criteria: local attach behavior is gone; termstage no longer renders command
PTY output in the invoking terminal; existing browser terminal tests remain
green except environment-dependent tmux tests.

## 12. Phase 8 - rmux Session Gateway and Semantic Operations

Maps to roadmap: closes M5.

| # | Task | Spec | Effort |
| --- | --- | --- | --- |
| 8.1 | Add session registry mapping termstage session ids to backend session/window/pane references. | [23](./23-local-remote-command-lease-design.md), [80](./80-browser-terminal-glossary.md) | 1 day |
| 8.2 | Define backend adapter traits for session control and semantic operations. | [23](./23-local-remote-command-lease-design.md) | 1.5 days |
| 8.3 | Implement rmux backend adapter for create/attach/list/read-screen/send-text/press-key/wait-text. | [23](./23-local-remote-command-lease-design.md) | 3 days |
| 8.4 | Add browser gateway path that subscribes to backend screen updates and translates browser input into semantic operations. | [20](./20-browser-terminal-web-design.md), [23](./23-local-remote-command-lease-design.md) | 2 days |
| 8.5 | Add semantic API endpoints for `ReadScreen`, `PressKey`, `SendText`, `PasteText`, `WaitForText`, and `ExecCommand`. | [23](./23-local-remote-command-lease-design.md), [70](./70-browser-terminal-security-design.md) | 2 days |
| 8.6 | Add Level 1 lease enforcement for browser/API write operations. | [23](./23-local-remote-command-lease-design.md), [70](./70-browser-terminal-security-design.md) | 1.5 days |

Exit criteria: M5 roadmap criteria pass; rmux native attach, browser view, and
API operations observe the same backend session; non-owner browser/API writes are
rejected.

## 13. Cross-References

- Depends on: all numbered browser terminal specs.
- Pairs with: [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md).
