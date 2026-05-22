# Changelog

All notable changes to this project will be documented in this file. See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

---
## [termstage-v0.3.1](https://github.com/compare/termstage-v0.3.0..termstage-v0.3.1) - 2026-05-22

### Miscellaneous Chores

- bump version - ([688d09a](https://github.com/commit/688d09a27277134d0adf7bc18ca4c980fde86012)) - Tyr Chen

### Other

- Update CHANGELOG.md - ([c0276d9](https://github.com/commit/c0276d99d4e08f9e327d31e489d8c6fffed4edce)) - Tyr Chen
- Fix reconnect on ambiguous websocket close (#6)

## Summary
- reconnect after ambiguous transport/proxy WebSocket closes instead of
showing a terminal-ended modal
- require an explicit processExited control frame before suppressing
reconnect for real child exits
- add Rust and Playwright regression coverage for child-exit close
ordering and ambiguous session-ended close behavior

## Verification
- make build
- make test-cargo
- make test
- make fmt
- make clippy
- make clippy-pedantic
- make clippy-boundary
- make audit
- make deny
- make frontend-typecheck
- make frontend-build
- make frontend-test
- commit hooks: cargo fmt, cargo deny check, typos, cargo check, cargo
clippy, cargo test - ([02981e0](https://github.com/commit/02981e0c6f1127f7760be8e16e1c87288040d6d5)) - Tyr Chen

---
## [termstage-v0.3.0](https://github.com/compare/termstage-v0.2.2..termstage-v0.3.0) - 2026-05-22

### Miscellaneous Chores

- bump version - ([14a70b2](https://github.com/commit/14a70b2eb17a42bc2400bb2dd0a4dee221efdd2d)) - Tyr Chen

### Other

- Update CHANGELOG.md - ([0331b73](https://github.com/commit/0331b73b6ca0ecce34f416ea4f15e7895c08b27f)) - Tyr Chen
- Improve terminal disconnect and font handling (#5)

## Summary
- show an accessible mono-styled terminal status modal for reconnecting,
ended sessions, and lost connectivity
- add `ExitPolicy` and default the CLI to `--exit-policy hold`, so
accidental `exit` keeps the current browser session open and shows a
`Process exited` modal instead of tearing down/reconnecting
- restart a held/exited PTY on the next browser attach, so refreshing
after accidental `exit` opens a fresh usable terminal instead of
replaying the exited modal
- tag PTY reader events by generation and ignore stale events after
restart, preventing late EOF/error events from the old PTY from marking
the newly restarted child as exited
- avoid blocking on the old PTY reader before dropping/replacing old PTY
handles, preventing Linux CI from hanging in `cargo nextest` after a
held process exit
- drop PTY handles before joining the reader during actor shutdown, so
end-policy shutdown paths do not hang Linux CI either
- poll child process status instead of relying on PTY EOF, which is not
delivered reliably on Linux while the actor still owns a slave handle
- serialize tmux integration tests that mutate the default tmux server
environment, avoiding parallel nextest races
- keep `--exit-policy end` available for the previous behavior where
child exit closes the browser session
- send WebSocket close reasons for runtime shutdown/session exit and
stop retrying after connectivity is declared lost
- close browser sockets as `session ended` when the runtime has already
stopped, including attach failures, input/resize sends after shutdown,
dropped output mailboxes, and normal closes whose reason was stripped by
a proxy
- use server-neutral status copy, including "The server shut down." for
ended sessions
- replace stale/live controller WebSockets when a newer browser
connection attaches, closing the displaced browser with a non-retrying
"controller replaced" reason
- add server-side WebSocket heartbeat expiry so silent proxy-held
browser connections cannot pin the controller indefinitely
- classify controller replacement and server-side client disconnect as
terminal browser states instead of reconnecting forever
- tune the high-contrast web palette and modal styling toward the native
terminal session colors
- advertise truecolor terminal capability to child PTY processes with
TERM=xterm-256color, COLORTERM=truecolor, CLICOLOR=1, and tmux
RGB/256-color support
- scrub NO_COLOR / ANSI_COLORS_DISABLED from spawned terminal sessions,
tmux global environment, and existing tmux session environments before
attaching
- pass color env into newly created tmux sessions; existing
already-running panes may need a new shell/pane because process env
cannot be rewritten externally
- embed all Vite asset files with rust-embed 8.11.0, including browser
terminal font files, so CSS font URLs do not 404
- replace Fontsource JetBrains Mono with a single embedded
JetBrainsMonoNL Nerd Font Mono regular/bold pair for stable terminal
metrics and prompt glyph coverage
- include the bundled font SIL OFL license and a source/version notice
beside the vendored font files
- add Playwright coverage for missing asset failures, Unicode glyph
output, color capability env, controller takeover, process-exited hold
plus refresh restart, and lost-connectivity states
- add Rust coverage for serving embedded font assets, rejecting unknown
asset paths, terminal color env setup, tmux global/session env cleanup,
controller replacement, runtime-unavailable WebSocket close paths, and
hold/end child-exit policies

## Verification
- cargo nextest run --all-features
- npm run typecheck --prefix apps/server/web
- npm run build --prefix apps/server/web
- npm test --prefix apps/server/web
- make build
- make test
- make test-cargo
- make fmt
- make clippy
- make clippy-pedantic
- make clippy-boundary
- make audit
- make deny
- commit hooks: cargo check, cargo clippy, cargo test, cargo deny check,
cargo fmt, typos - ([b36189c](https://github.com/commit/b36189ccca45fb6962c8daf0fe642d774178047a)) - Tyr Chen

---
## [termstage-v0.2.2](https://github.com/compare/termstage-v0.2.1..termstage-v0.2.2) - 2026-05-22

### Miscellaneous Chores

- bump version - ([41e80e0](https://github.com/commit/41e80e04ec6c7e4d439fd82f2dd1d9e8c2e564d6)) - Tyr Chen

### Other

- Update CHANGELOG.md - ([cdb1d93](https://github.com/commit/cdb1d93b0b92a00514c897040674e4ab7037d04a)) - Tyr Chen
- Fix browser terminal wheel scrolling (#4)

## Summary
- intercept terminal wheel events before xterm's custom scrollbar path
swallows them
- normalize wheel deltas and scroll xterm scrollback directly when mouse
tracking is inactive
- add a Playwright regression covering wheel-up scrollback behavior

## Verification
- npm run typecheck --prefix apps/server/web
- npm run build --prefix apps/server/web
- npm test --prefix apps/server/web
- cargo build --workspace --all-targets
- cargo test --workspace --all-targets
- cargo +nightly fmt --all -- --check
- cargo clippy --workspace --all-targets -- -D warnings -W
clippy::pedantic
- cargo audit
- cargo deny check - ([349222e](https://github.com/commit/349222eecfbef310e0d404b98ad75c723ac2e6f4)) - Tyr Chen

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
