# Key Decisions - Browser Terminal Presentation Mode

Status: draft v1
Owner: termstage
Last updated: 2026-05-28

Each decision is permanent. Supersede with a new decision rather than editing history
after implementation begins.

## D1 - Use xterm.js in the Browser Instead of Writing a Terminal Emulator

- Context: Browser terminal rendering and keyboard handling.
- Alternatives considered: custom canvas terminal, DOM-only terminal, xterm.js.
- Decision: Use `@xterm/xterm` and selected official addons.
- Why: The product needs mature terminal semantics for demos, not a terminal-emulator
  research project.
- Pinned by: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md).
- Date: 2026-05-19.

## D2 - Transport Terminal Bytes, Not Command Strings

- Context: Browser-to-PTY protocol.
- Alternatives considered: command-line JSON API, all-JSON byte arrays, raw binary
  WebSocket frames plus JSON controls.
- Decision: Binary frames carry raw terminal bytes; text frames carry JSON controls.
- Why: Ctrl-C, tab completion, Vim, tmux, mouse events, alternate screen, and paste are
  byte-stream terminal behavior, not command submissions.
- Pinned by: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md).
- Date: 2026-05-19.

## D3 - Default to Tmux Mode for Presentation

- Context: Sharing demo state between a native terminal and Chrome.
- Alternatives considered: attach to existing Terminal.app window, spawn a new shell
  only, tmux shared session.
- Decision: Default M0 mode is `tmux new-session -A -s <session>`.
- Why: Operating systems do not provide a safe portable way to take over an arbitrary
  terminal window's PTY master. Tmux is the practical sharing layer.
- Pinned by: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md).
- Date: 2026-05-19.

## D4 - Local-Only Before Remote Sharing

- Context: Security boundary.
- Alternatives considered: LAN sharing in M0, tunnel support, local-only loopback.
- Decision: M0-M3 are loopback-only.
- Why: A web terminal is direct shell control. Remote access needs a separate threat
  model with TLS, authentication, authorization, rate limiting, and audit behavior.
- Pinned by: [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md),
  [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md).
- Date: 2026-05-19.

## D5 - Keep Domain Contracts in `termstage-core`

- Context: Workspace ownership.
- Alternatives considered: put all code in `apps/server`, create a separate protocol
  crate, use current `termstage-core`.
- Decision: Put protocol, validation, security, and runtime contracts in
  `termstage-core`; put integration code in `termstage`.
- Why: The current workspace already has a core/server split, and keeping validation
  contracts outside Axum handlers makes testing and review cleaner.
- Pinned by: [61-browser-terminal-crates-and-features.md](./61-browser-terminal-crates-and-features.md).
- Date: 2026-05-19.

## D6 - One Write-Capable Browser Client by Default

- Context: Multi-client behavior.
- Alternatives considered: unrestricted controllers, read-only mirrors in M0, single
  controller first.
- Decision: M0 allows one controller. Mirror clients are deferred.
- Why: Multiple writers create surprising terminal races and a larger authz surface.
  The presentation job only needs one controlling tab.
- Pinned by: [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md).
- Date: 2026-05-19.

## D7 - Public Pod Exposure Requires Explicit Operator Intent

- Context: Running `termstage` in a pod behind internet-facing ingress.
- Alternatives considered: allow any non-loopback `--host`, infer public mode from
  `--host 0.0.0.0`, require explicit `--expose-public` plus public URL and token env.
- Decision: Require `--expose-public`, `--public-url https://...`, and
  `--token-env <NAME>` before accepting non-loopback bind addresses.
- Why: A web terminal is shell control. Binding to all interfaces must never be an
  accidental consequence of a host flag, and generated local tokens do not fit
  reproducible pod deployments.
- Pinned by: [21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md),
  [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md).
- Date: 2026-05-19.

## D8 - Termstage Manages Session References, Not Local Attach PTYs

- Context: The old shell-mode local attach plan made `termstage` own a command
  PTY and mirror it into the invoking terminal and browser.
- Alternatives considered: keep local passthrough behavior,
  replace it with a split local TUI, or move shared terminal ownership to a
  backend session such as rmux/tmux.
- Decision: Remove the local attach flag and model `termstage` sessions as
  backend session references. `termstage` owns the registry, gateways, auth, and
  Level 1 operation lock; rmux/tmux/future backends own actual panes, PTYs,
  screen state, and native local attach.
- Why: Backend-native attach is the right abstraction for local viewing.
  `termstage` should not mix supervisor logs with command output or implement a
  second terminal multiplexer inside its own stdout/stderr.
- Pinned by: [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md),
  [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md).
- Date: 2026-05-27.

## D9 - Browser Xterm Fits Its Embedded Container

- Context: Browser UI sizing for backend-owned sessions, where a tmux/rmux pane
  may be wider or taller than the browser terminal container.
- Alternatives considered: resize backend panes whenever the browser container
  changes, make xterm DOM grow to backend pane size with native DOM overflow, or
  keep xterm fit to its embedded container and project backend screens into that
  viewport.
- Decision: xterm fits the terminal container allocated by the page. Backend
  screen size remains backend-owned, and `termstage` projects backend snapshots
  into the browser viewport.
- Why: The terminal is a reusable page component, not the whole browser page.
  Future UI may add buttons, nav, sidebars, or other HTML around it. Browser
  layout changes should not resize backend sessions or break native attach
  clients, while wider backend screens must still be navigable from the
  embedded terminal component.
- Pinned by: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
- Date: 2026-05-28.
