![](https://github.com/tyrchen/rust-lib-template/workflows/build/badge.svg)

# termstage

`termstage` is a local browser terminal presentation tool. It starts a
loopback-only Rust server, opens a tokenized browser URL, and bridges the browser
terminal to a real local shell or tmux session. The main use case is live demos:
share a Chrome tab with large readable terminal text while keeping the session state
stable across browser refreshes.

> Security boundary: this is a local shell bridge, not a sandbox. Browser input is
> sent to a real shell or tmux session with the current OS user's privileges.

## What It Does

```text
Presenter
  |
  | termstage --session presentation --open
  v
+----------------------+       tokenized local URL       +--------------------+
| termstage CLI       | ------------------------------> | Browser tab        |
| - validates options  |                                 | - xterm.js UI      |
| - starts runtime     | <======== WebSocket ==========> | - binary input     |
| - starts server      |                                 | - resize controls  |
+----------+-----------+                                 +--------------------+
           |
           | validated runtime commands
           v
+----------------------+       PTY bytes       +-----------------------------+
| Runtime actor        | <===================> | local shell / tmux session  |
| - owns PTY           |                       | user's OS privileges        |
| - bounded mailboxes  |                       +-----------------------------+
| - reconnect replay   |
+----------------------+
```

The current browser terminal mode provides:

- Loopback-only HTTP/WebSocket server.
- Per-start access token in the launch URL.
- Host, Origin, token, and peer-address checks.
- Binary terminal byte transport.
- JSON control frames for resize and heartbeat.
- tmux-backed session mode for demo state preservation.
- Fresh shell mode for local smoke tests or simple usage.
- Browser reconnect behavior with bounded recent-output replay.
- Slow-client backpressure handling that closes the browser client without killing
  the terminal session.
- Vite-built, first-party frontend assets embedded into the server binary.

## Quick Start

Run a tmux-backed presentation session and open the browser:

```bash
cargo run -p termstage --bin termstage -- --session presentation --open
```

Run a fresh shell instead of tmux:

```bash
cargo run -p termstage --bin termstage -- --mode shell --shell /bin/zsh --open
```

Print the launch URL instead of opening the browser:

```bash
cargo run -p termstage --bin termstage -- --session presentation
```

Tune readability for screen sharing:

```bash
cargo run -p termstage --bin termstage -- \
  --session presentation \
  --font-size 28 \
  --theme high-contrast \
  --open
```

## Important Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--session <name>` | `presentation` | Attach to or create this validated tmux session. |
| `--mode <tmux|shell>` | `tmux` | Use a shared tmux session or a fresh shell. |
| `--shell <path>` | `$SHELL` or `/bin/sh` | Shell executable for shell mode. |
| `--host <loopback>` | `127.0.0.1` | Bind address. Non-loopback addresses are rejected. |
| `--port <port>` | `0` | `0` lets the OS choose a free port. |
| `--open` | `false` | Open the tokenized URL in the default browser. |
| `--font-size <px>` | `24` | Browser terminal font size. |
| `--theme <name>` | `high-contrast` | Presentation theme preset. |
| `--keepalive <session|exit>` | `session` | Keep browser-refresh session behavior enabled. |

## Safety Model

The browser terminal intentionally stays local-only.

```text
Allowed:
  127.0.0.1:<random-port> + per-start token + same-origin WebSocket

Rejected:
  0.0.0.0, LAN addresses, mismatched Host, mismatched Origin, bad token,
  non-loopback peer addresses
```

Do not expose the server through a LAN bind, tunnel, reverse proxy, or remote
desktop helper unless a separate remote-sharing design adds TLS, authentication,
authorization, rate limiting, audit logging, and read-only viewer semantics.

## Documentation

- [User Guide](./docs/guides/user-guide.md): installation assumptions, CLI usage,
  demo workflow, troubleshooting, and Chinese translation.
- [Developer Guide](./docs/guides/developer-guide.md): workspace layout,
  architecture, protocol/runtime flow, quality gates, and Chinese translation.
- [Documentation Index](./docs/index.md): all project documentation.
- [Specs Index](./specs/index.md): product, protocol, runtime, web, CLI, security,
  verification, roadmap, and implementation plan.

## Development

Use the Makefile targets instead of ad hoc shell scripts:

```bash
make build
make test-cargo
make fmt
make clippy
make clippy-boundary
make frontend-ci
make ci
```

`make ci` is the full local gate. It runs Rust build/test/fmt/clippy/doc/audit/deny
and frontend install/typecheck/build/Playwright tests.

## 中文

`termstage` 是给现场演示用的本地浏览器终端。它在本机启动一个只监听
loopback 的 Rust 服务，生成带访问令牌的本地 URL，然后把浏览器里的终端界面接到
真实的本地 shell 或 tmux 会话上。

最常见的用法是：演讲时只共享 Chrome 标签页，让观众看到字号更大、对比度更高的
终端；如果浏览器刷新或短暂断开，演示会话还能接回来，不至于丢掉刚才的状态。

> 需要先说清楚：这是本地 shell 桥接工具，不是沙箱。你在浏览器里输入的内容，会
> 以当前操作系统用户的权限进入真实 shell 或 tmux 会话。

### 它怎么工作

```text
演讲者
  |
  | termstage --session presentation --open
  v
+----------------------+       带令牌的本地 URL       +--------------------+
| termstage CLI       | ---------------------------> | 浏览器标签页       |
| - 检查参数           |                              | - xterm.js 界面    |
| - 启动运行时         | <====== WebSocket =========> | - 输入字节         |
| - 启动本地服务       |                              | - 窗口尺寸消息     |
+----------+-----------+                              +--------------------+
           |
           | 已校验的运行时命令
           v
+----------------------+       PTY 字节流      +-----------------------------+
| Runtime actor        | <===================> | 本地 shell / tmux 会话      |
| - 持有 PTY           |                       | 当前系统用户权限            |
| - 有界客户端队列     |                       +-----------------------------+
| - 重连时回放近期输出 |
+----------------------+
```

目前已经具备这些能力：

- HTTP 和 WebSocket 服务只监听本机 loopback 地址。
- 每次启动都会生成新的访问令牌，令牌只出现在启动 URL 中。
- 服务端会检查 Host、Origin、token 和 peer 地址。
- 终端数据走二进制 WebSocket 帧，不绕成 JSON。
- resize 和 heartbeat 使用 JSON 控制帧。
- 默认使用 tmux，会话状态适合演示时保留。
- 也可以用 shell 模式，方便快速本地验证。
- 浏览器断开后会自动重连，并回放近期终端输出。
- 如果浏览器跟不上大量输出，会关闭这个慢客户端，但不会杀掉底层终端会话。
- 前端资源由 Vite 构建，并作为一方资源嵌进服务端二进制，不依赖 CDN。

### 快速开始

启动默认的 tmux 演示会话，并打开浏览器：

```bash
cargo run -p termstage --bin termstage -- --session presentation --open
```

如果只想开一个新的 shell：

```bash
cargo run -p termstage --bin termstage -- --mode shell --shell /bin/zsh --open
```

如果不想自动打开浏览器，只打印 URL：

```bash
cargo run -p termstage --bin termstage -- --session presentation
```

演示时可以把字体调大一些：

```bash
cargo run -p termstage --bin termstage -- \
  --session presentation \
  --font-size 28 \
  --theme high-contrast \
  --open
```

### 安全边界

浏览器终端只面向本机使用，当前不提供远程共享能力。

```text
允许的形态：
  127.0.0.1:<随机端口> + 每次启动的 token + 同源 WebSocket

会被拒绝的形态：
  0.0.0.0、局域网地址、Host 不匹配、Origin 不匹配、token 错误、
  非 loopback peer 地址
```

不要把这个服务通过局域网绑定、隧道、反向代理等方式暴露出去。远程共享不是
“改个 host” 就能安全支持的功能，需要单独设计 TLS、认证、授权、限流、审计日志
以及只读观看者语义。

### 文档

- [用户指南](./docs/guides/user-guide.md)：运行环境、CLI 用法、演示流程、刷新重连、故障排查。
- [开发者指南](./docs/guides/developer-guide.md)：工作区结构、架构、协议和运行时流程、质量门禁。
- [文档索引](./docs/index.md)：项目文档入口。
- [规格索引](./specs/index.md)：产品、协议、运行时、Web、CLI、安全、验证、路线图和实现计划。

## License

This project is distributed under the terms of MIT.

See [LICENSE](LICENSE.md) for details.

Copyright 2025 Tyr Chen
