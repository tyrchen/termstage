# Changelog

All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

---
## [termstage-v0.2.1](https://github.com/compare/termstage-v0.2.0..termstage-v0.2.1) - 2026-05-22

### Bug Fixes

- **(web)** bundle JetBrains Mono so xterm renders consistently in browsers (#3) - ([72e4b85](https://github.com/commit/72e4b85d6fd070d5b42e9f170c4acdaf458bb7be)) - Tyr Chen

### Miscellaneous Chores

- bump version - ([7d732be](https://github.com/commit/7d732be73bf24af2d43fb363753f49d3e29b5216)) - Tyr Chen

### Other

- Update CHANGELOG.md - ([57b4e70](https://github.com/commit/57b4e706ad4e6e3fb27f78166e609e2cd0a93eac)) - Tyr Chen

---
## [termstage-v0.2.0](https://github.com/compare/termstage-v0.1.0..termstage-v0.2.0) - 2026-05-20

### Features

- **(coder)** add reverse-proxy base path mounting (#2) - ([754bf24](https://github.com/commit/754bf24b14e85f81f69ed06f6156abeea31519bc)) - Tyr Chen

### Miscellaneous Chores

- update README.md - ([402e34b](https://github.com/commit/402e34bbb38da90f9eebb4a5943e413035cb7b3f)) - Tyr Chen
- bump version - ([a2da817](https://github.com/commit/a2da81717722c301241ac199e828998bbb58adee)) - Tyr Chen

### Other

- Update CHANGELOG.md - ([1350e08](https://github.com/commit/1350e083b8a786da86c19a96a44ce74376dea357)) - Tyr Chen
- Fix CI shell portability - ([951f33e](https://github.com/commit/951f33e9badf941a692a23edd2a66fde6eeca829)) - Tyr Chen
- Add explicit public pod exposure mode (#1)

## Summary

- add M4/Phase 6 specs for explicit public pod exposure using
`--expose-public`, `--public-url`, and `--token-env`
- implement typed exposure policy, HTTPS public URL validation, public
Host/Origin checks, and public launch URL construction
- keep default local mode loopback-only with generated per-run tokens,
and document public pod usage in README/user guides

## Verification

- `make build`
- `make test-cargo`
- `make fmt`
- `make clippy`
- `make clippy-pedantic`
- `make clippy-boundary`
- `make doc`
- `make audit`
- `make deny`
- pre-commit: `cargo fmt`, `cargo deny check`, `cargo check`, `cargo
clippy`, `cargo test`

Note: `cargo deny check` exits successfully with existing
duplicate/license warnings. - ([4c7787b](https://github.com/commit/4c7787bbc5035701dd42bbe0129ed0693acacdab)) - Tyr Chen

---
## [termstage-v0.1.0] - 2026-05-19

### Miscellaneous Chores

- init the project - ([ed61a90](https://github.com/commit/ed61a90307a07d1afb9fc744d55132ba230383cf)) - Tyr Chen
- add specs - ([f10c863](https://github.com/commit/f10c863694c4734da5062776010e285e263026ec)) - Tyr Chen
- update cargo - ([de958d2](https://github.com/commit/de958d2c420f92f403f6c1bc5e2b8ec1e0c8697a)) - Tyr Chen
- update docs - ([e79f2e2](https://github.com/commit/e79f2e290a93f5a275a561424e369e1063ec5c01)) - Tyr Chen
- update app name to termstage - ([75a4cf8](https://github.com/commit/75a4cf80013542d635ed82aff946a6a449ec4a7c)) - Tyr Chen

### Other

- phase 0: retire browser terminal risks

Validate portable-pty zsh/tmux PTY behavior, xterm 6.x addon/Vite build APIs, Axum 0.8 WebSocket split and frame-cap APIs, and the asset bundling direction required by specs 61 and 91. Add the Phase 0 research memo plus spec cross-links so Phase 1 starts from observed API shapes.

Add the Rust toolchain pin and Makefile quality gates needed for the phase exit criteria. Review found two P2/P3 issues; ci now includes clippy-pedantic, and the scaffold server cleanup is documented as quality-baseline work. No deferred findings. - ([b575e39](https://github.com/commit/b575e390a5ee17948d4a339957631b33328296f9)) - Tyr Chen
- phase 1-2: add browser terminal contracts and runtime

Land Phase 1 foundation contracts from specs 10, 70, and 72: validated protocol newtypes, serde control frames, redacted 256-bit access tokens, and local-only Host/Origin/peer/token validation.

Land Phase 2 PTY runtime from specs 11 and 72: dedicated session actor thread, bounded command/client mailboxes, shell mode, tmux mode, resize/input/detach/shutdown lifecycle tests. Review fixed the Drop shutdown path to avoid Tokio blocking_send in async contexts. No deferred findings. - ([b77e0b0](https://github.com/commit/b77e0b0a866b8522db931b132616e84fbc970d46)) - Tyr Chen
- phase 3-4: land browser terminal bridge and UX

Implements browser-terminal phases 3 and 4 for M0/M1: Axum loopback routes, tokenized launch URL, Host/Origin/peer/token validation, WebSocket frame bridge into the runtime actor, validated CLI flags for session/mode/open/font/theme, embedded Vite/xterm frontend assets, presentation theme/font plumbing, and real-server Playwright smoke coverage.

Exit criteria covered by cargo build/test/fmt/clippy/doc, strict boundary clippy, cargo audit, cargo deny check, frontend typecheck/build/test, route/security/WebSocket tests, and real PTY browser smoke screenshots. Review findings were fixed in-phase: token-safe tracing, asset validation, HTTP-only origin, root guard, server task error propagation, real Playwright E2E, peer/frame-cap tests. - ([9c669dd](https://github.com/commit/9c669dd39141c5e655900ebbc898d21bf4328a5c)) - Tyr Chen
- phase 5: harden browser terminal reconnect

Land Phase 5 / M2-M3 browser terminal hardening: bounded runtime replay for browser refresh, reconnecting frontend socket behavior, explicit slow-client WebSocket close handling, backpressure and refresh regression tests, Makefile aggregate hardening gates, and local-only CLI/README safety wording.

Exit criteria covered by make ci, including Rust build/test/fmt/clippy/doc/audit/deny plus frontend typecheck/build/Playwright. Independent review findings were fixed in-phase; no deferred findings were added. - ([285db5d](https://github.com/commit/285db5dc722b10de4fef2fad207013f1379ce8b9)) - Tyr Chen

<!-- generated by git-cliff -->
