# Browser Terminal Phase 0 Validation

Status: complete
Owner: presenterm
Last updated: 2026-05-19

## Scope

This memo retires the Phase 0 risks from
[91-browser-terminal-impl-plan.md](../../specs/91-browser-terminal-impl-plan.md):

- `portable-pty` spawn, read, write, and resize on macOS with zsh and tmux.
- `@xterm/xterm` 6.x and official addon imports through a minimal Vite build.
- Axum 0.8 WebSocket split/send/receive and explicit frame-size configuration.
- Asset bundling and Makefile target direction before production browser-terminal code.

## Environment

| Tool | Observed version |
| --- | --- |
| macOS shell | `/bin/zsh` |
| Rust | `rustc 1.95.0` |
| Nightly Cargo | `cargo 1.96.0-nightly` |
| Node.js | `v24.10.0` |
| npm | `11.6.1` |
| tmux | `3.6a` |

`tmux` was not present initially and was installed with Homebrew for this validation.

## Dependency Checks

Registry checks on 2026-05-19 matched the dependency baseline in
[61-browser-terminal-crates-and-features.md](../../specs/61-browser-terminal-crates-and-features.md):

| Dependency | Validated version | Evidence |
| --- | --- | --- |
| `portable-pty` | `0.9.0` | `cargo search portable-pty --limit 1` and `cargo info portable-pty` |
| `axum` | `0.8.9` | `cargo search axum --limit 1` and `cargo info axum` |
| `tower-http` | `0.6.11` | `cargo search tower-http --limit 1` |
| `@xterm/xterm` | `6.0.0` | `npm view @xterm/xterm version` |
| `@xterm/addon-fit` | `0.11.0` | `npm view @xterm/addon-fit version` |
| `@xterm/addon-web-links` | `0.12.0` | `npm view @xterm/addon-web-links version` |
| `@xterm/addon-webgl` | `0.19.0` | `npm view @xterm/addon-webgl version` |
| `vite` | `8.0.13` | `npm view vite version` |

Primary docs checked:

- `portable-pty` docs show `native_pty_system`, `openpty`, `PtySize`,
  `CommandBuilder`, `spawn_command`, `try_clone_reader`, and `take_writer`.
- Axum 0.8.9 WebSocket docs expose `max_frame_size`, `max_message_size`, and
  `on_upgrade`; `axum::serve` is gated behind `tokio` plus `http1` or `http2`.
- xterm.js addon docs confirm the `Terminal.loadAddon(new FitAddon())` pattern.
- Vite build docs confirm `build.outDir`, `build.assetsDir`, and `build.manifest`
  as the right production asset knobs.

## 0.1 PTY Validation

A throwaway Rust probe in `/tmp/presenterm-phase0-pty` used:

- `portable-pty = 0.9.0`
- `anyhow = 1.0.102`
- `/bin/zsh -f`
- `/usr/bin/env -u TMUX tmux new-session -A -s presenterm-phase0`

Validated operations:

- Opened a native PTY at `80x24`.
- Spawned zsh, wrote `printf` input through the master writer, and read the marker
  from a cloned master reader.
- Resized the zsh PTY to `101x33` and observed `stty size` report `33 101`.
- Spawned tmux through the PTY, wrote input, read output, resized the PTY, and
  observed tmux client size `101x33` via `tmux list-clients`.

Command evidence:

```text
$ cargo run --quiet
zsh spawn/read/write/resize passed
tmux spawn/read/write/resize passed
phase0 portable-pty probe passed
```

Implementation notes:

- Use argv form for shells and tmux; do not build shell command strings.
- Clear inherited `TMUX` for managed tmux launches and tmux supervisor commands.
  Without this, `tmux new-session -A -s <session>` can target the parent tmux
  environment instead of the intended session.
- Resize should call `MasterPty::resize(PtySize { rows, cols, pixel_width: 0,
  pixel_height: 0 })`.
- `try_clone_reader` plus `take_writer` supports the runtime architecture where a
  dedicated reader task/thread and actor-owned writer are separate.

## 0.2 xterm and Vite Validation

A throwaway frontend probe in `/tmp/presenterm-phase0-xterm` used:

- `@xterm/xterm = 6.0.0`
- `@xterm/addon-fit = 0.11.0`
- `@xterm/addon-web-links = 0.12.0`
- `@xterm/addon-webgl = 0.19.0`
- `vite = 8.0.13`
- `typescript = latest`

Validated operations:

- Imported `Terminal`, `FitAddon`, `WebLinksAddon`, `WebglAddon`, and xterm CSS.
- Constructed a terminal with presentation-size font settings.
- Loaded addons through `terminal.loadAddon(...)`.
- Registered `terminal.onData((data: string) => ...)`.
- Built production assets with Vite manifest output.

Command evidence:

```text
$ npx tsc --noEmit

$ npm run build
vite v8.0.13 building client environment for production...
dist/.vite/manifest.json          0.18 kB
dist/index.html                   0.41 kB
dist/assets/index-*.css           3.93 kB
dist/assets/index-*.js          467.15 kB
✓ built
```

Implementation notes:

- Include `/// <reference types="vite/client" />` so side-effect CSS imports type
  check cleanly.
- Keep WebGL optional; the default frontend should work with the DOM/canvas fallback.
- Use Vite's manifest output so Rust static serving or embedding can reference hashed
  asset names without guessing.

## 0.3 Axum WebSocket Validation

A throwaway Rust probe in `/tmp/presenterm-phase0-axum` used:

- `axum = { version = "0.8.9", default-features = false, features = ["ws", "http1"] }`
- `tokio = { version = "1.52", features = ["rt-multi-thread", "macros", "net", "sync", "time"] }`
- `futures-util = { version = "0.3", features = ["sink"] }`
- `tokio-tungstenite = "0.29"`

Validated operations:

- Bound a local `127.0.0.1:0` listener.
- Routed `GET /ws` through `WebSocketUpgrade`.
- Applied `max_frame_size(4096)` and `max_message_size(8192)` before `on_upgrade`.
- Split the upgraded `WebSocket` into sender and receiver halves.
- Echoed binary and text frames to a local Tungstenite client.
- Shut down the Axum server through a supervised task and oneshot signal.

Command evidence:

```text
$ cargo run --quiet
phase0 axum websocket probe passed
```

Implementation notes:

- `axum::serve` is unavailable if `http1` or `http2` is omitted.
- `WebSocket::split()` requires `futures-util` stream/sink traits.
- Production routes should configure frame/message caps before `on_upgrade`, then
  perform token/Host/Origin/peer validation before allowing the upgrade.

## 0.4 Asset Bundling and Makefile Decision

Decision:

- Build browser assets with Vite under `apps/server/web`.
- Commit `package-lock.json` once the frontend lands.
- Enable `build.manifest = true`, `build.outDir = "dist"`, and
  `build.assetsDir = "assets"`.
- Serve or embed only built first-party assets. No CDN scripts.
- Keep WebGL behind an optional frontend/runtime choice because presentation machines
  may lack reliable WebGL acceleration.

Makefile targets added for discoverability:

- `build`, `test-cargo`, `fmt`, `clippy`, `clippy-pedantic`, `doc`, `audit`, `deny`, `ci`
- `frontend-install` and `frontend-build`, which are no-ops until `apps/server/web`
  exists and then run npm in that directory.

Repository quality baseline:

- Added `rust-toolchain.toml` to pin stable Rust `1.95.0` with `clippy` and `rustfmt`.
- Enabled workspace Rust lints required by `AGENTS.md`.
- Added crate-level docs to the existing scaffold crates so documentation gates can
  run before Phase 1.
- Removed the scaffold server `println!` because production entry points should use
  structured logging once server behavior exists.

## Phase 0 Outcome

Phase 0 is complete. No production browser-terminal code is required before Phase 1.
The validated API shapes are reflected here and in
[61-browser-terminal-crates-and-features.md](../../specs/61-browser-terminal-crates-and-features.md).
