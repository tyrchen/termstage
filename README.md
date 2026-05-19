![](https://github.com/tyrchen/rust-lib-template/workflows/build/badge.svg)

# presenterm

`presenterm` provides a local browser terminal presentation mode. It starts a
loopback-only web server, opens a tokenized browser URL, and bridges the page to a
real local shell or tmux session.

## Browser Terminal

```bash
cargo run -p presenterm-server --bin presenterm -- --session presentation --open
```

The browser terminal is a local shell bridge, not a sandbox. It only binds to a
loopback address, requires the per-start token in the browser URL, checks same-origin
WebSocket requests, and runs commands with the current OS user's privileges. Do not
expose it through LAN binding, tunnels, or reverse proxies without a separate remote
sharing design.

Useful options:

- `--mode tmux|shell`: use a shared tmux session or a fresh shell.
- `--keepalive session|exit`: keep the session available across browser refreshes or
  terminate it during shutdown.
- `--font-size <px>` and `--theme high-contrast|light`: tune presentation readability.

## Agent support

Generated projects include agent-facing guidance for both Codex and Claude:

- `AGENTS.md` for Codex project instructions.
- `.agents/skills/{spec,research,impl}` for Codex skills.
- `CLAUDE.md` and `.claude/skills/{spec,research,impl}` for Claude Code compatibility.

Have fun with this crate!

## License

This project is distributed under the terms of MIT.

See [LICENSE](LICENSE.md) for details.

Copyright 2025 Tyr Chen
