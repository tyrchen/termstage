# Browser Terminal Developer Guide

Status: draft v1
Last updated: 2026-05-19

This guide explains how the browser terminal mode is organized for developers. It
covers workspace layout, runtime ownership, WebSocket flow, frontend build behavior,
quality gates, and the constraints that keep the local shell bridge auditable.

## Workspace Layout

```text
termstage/
  Cargo.toml
  Makefile
  crates/
    core/
      src/
        lib.rs
        protocol.rs
        runtime.rs
        security.rs
  apps/
    server/
      src/
        main.rs
        cli.rs
        web.rs
        assets.rs
      web/
        src/
          main.ts
          presentation.ts
          resize.ts
          socket.ts
          terminal.ts
          style.css
        tests/
          smoke.spec.ts
        dist/
          index.html
          assets/
            index.js
            index.css
  specs/
  docs/
    guides/
    research/
```

Ownership is intentionally split:

| Area | Owner | Responsibility |
| --- | --- | --- |
| Protocol types | `termstage-core` | Tokens, session names, terminal size, JSON control frames. |
| Security types | `termstage-core` | Loopback bind, Host, Origin, peer, token comparison. |
| Runtime actor | `termstage-core` | PTY process, client mailboxes, replay, shutdown. |
| CLI | `termstage` | Argument parsing, validation into config, startup/shutdown. |
| Web server | `termstage` | Axum routes, WebSocket bridge, static asset serving. |
| Frontend | `apps/server/web` | xterm.js UI, resize, socket reconnect, presentation theme. |

## Architecture

```text
+----------------------------------------------------------------------------+
| termstage                                                           |
|                                                                            |
|  +----------+      +-------------+      +--------------------------------+  |
|  | cli.rs   | ---> | web.rs      | ---> | Axum routes                    |  |
|  | clap     |      | AppState    |      | / /assets/{path} /ws /healthz  |  |
|  +----------+      +------+------+      +----------------+---------------+  |
|                           |                              |                  |
|                           | RuntimeCommand               | WebSocket frames |
+---------------------------|------------------------------|------------------+
                            v                              v
+----------------------------------------------------------------------------+
| termstage-core                                                             |
|                                                                            |
|  +----------------+      bounded mpsc       +---------------------------+  |
|  | protocol.rs    | <---------------------> | runtime.rs SessionActor   |  |
|  | security.rs    |                         | - owns PTY                |  |
|  +----------------+                         | - owns clients map        |  |
|                                             | - owns replay buffer      |  |
|                                             +-------------+-------------+  |
|                                                           |                |
+-----------------------------------------------------------|----------------+
                                                            v
                                                   shell or tmux process
```

The runtime actor is the concurrency boundary. HTTP handlers and WebSocket tasks do
not own PTY handles and do not perform blocking PTY reads. They send validated
commands over bounded channels.

## Protocol Flow

Binary WebSocket frames carry terminal bytes. Text frames carry JSON control
messages.

```text
Browser input byte(s)
  |
  v
WebSocket binary frame
  |
  v
RuntimeCommand::Input { client_id, bytes }
  |
  v
PTY writer

Browser resize
  |
  v
JSON text frame: {"type":"resize","cols":120,"rows":34}
  |
  v
ClientControlMessage::Resize
  |
  v
RuntimeCommand::Resize { size }
  |
  v
PTY resize
```

Important protocol rules:

- Unknown JSON control fields are rejected by serde validation.
- Terminal dimensions are validated with explicit min/max ranges.
- Terminal data stays binary and is not JSON encoded.
- Access tokens are redacted in debug output.
- Token comparison uses constant-time equality.

## Runtime Lifecycle

The runtime starts one actor thread and one PTY reader thread.

```text
RuntimeSession::start
  |
  +--> native_pty_system().openpty(...)
  |
  +--> clone reader
  |
  +--> take writer
  |
  +--> spawn shell or tmux with argv form
  |
  +--> spawn PTY reader thread
  |
  +--> spawn SessionActor thread
```

Runtime commands:

| Command | Behavior |
| --- | --- |
| `AttachClient` | Accepts the controller when no other controller is active, sends `Ready`, replays recent PTY output. |
| `DetachClient` | Removes the client and frees the controller slot. |
| `Input` | Writes bytes only when they come from the active controller. |
| `Resize` | Resizes the PTY master handle. |
| `Shutdown` | Closes clients, drops writer, kills child, joins reader. |

Reconnect behavior uses actor state, not a global registry. The actor keeps a bounded
recent-output replay buffer:

```text
PTY output
  |
  +--> append to replay buffer
  |       |
  |       +--> trim oldest chunks past byte cap
  |
  +--> broadcast to attached clients

new client attach
  |
  +--> Ready
  |
  +--> replay recent PTY chunks
  |
  +--> receive live PTY output
```

## WebSocket Bridge

The bridge splits each WebSocket into receive and send halves. It races browser
messages against runtime output.

```text
                       +--------------------+
browser binary ------> | receive half       | --> RuntimeCommand::Input
browser text --------> | receive half       | --> RuntimeCommand::Resize / Heartbeat
runtime output ------> | send half          | --> browser binary/text/close
mailbox closed ------> | send close frame   | --> browser disconnect
                       +--------------------+
```

If a runtime client mailbox closes, the bridge sends a WebSocket close frame and
exits. This prevents slow-client eviction from leaving a browser socket open forever.

## Frontend Flow

The frontend is intentionally direct:

```text
main.ts
  |
  +--> readPresentationSettings()
  |
  +--> createTerminalSurface()
  |
  +--> connectTerminalSocket()
  |
  +--> watchTerminalResize()
```

Key frontend modules:

| Module | Responsibility |
| --- | --- |
| `presentation.ts` | Reads `fontSize` and `theme` query settings. |
| `terminal.ts` | Creates xterm.js terminal and addons. |
| `resize.ts` | Fits xterm to the container and sends debounced resize messages. |
| `socket.ts` | Connects WebSocket, sends input, handles output, reconnects after close. |

Reconnect behavior:

```text
socket close
  |
  +--> closed by app? yes --> stop
  |
  no
  |
  +--> wait 250ms, 500ms, 1000ms, then 2000ms max
  |
  +--> open new socket
  |
  +--> resend last terminal size
  |
  +--> continue heartbeat
```

## Security Rules for Contributors

Treat every request and browser frame as hostile until validated.

```text
Boundary input
  |
  +--> validate immediately
  |
  +--> convert to typed domain value
  |
  +--> pass typed value inward
  |
  v
business/runtime logic
```

Do not add:

- Implicit non-loopback bind support; public exposure must stay behind the explicit
  `--expose-public`/`--public-url`/`--token-env` contract.
- CDN JavaScript.
- Shell command strings built with concatenation.
- Unbounded channels or unbounded terminal output buffers.
- Token logging.
- Terminal byte logging.
- `unwrap()` or `expect()` in production code.
- `unsafe`.

Remote sharing is out of scope until there is a separate design for TLS,
authentication, authorization, rate limiting, audit logging, and read-only viewers.

## Quality Gates

Use Makefile targets. They are the discoverable automation surface.

```bash
make build
make test-cargo
make fmt
make clippy
make clippy-pedantic
make clippy-boundary
make doc
make audit
make deny
make frontend-ci
make ci
```

`make ci` runs the full gate:

```text
Rust:
  cargo build --workspace --all-targets
  cargo test --workspace --all-targets
  cargo +nightly fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic
  cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic \
    -W clippy::unwrap_used -W clippy::expect_used \
    -W clippy::indexing_slicing -W clippy::panic
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
  cargo audit
  cargo deny check

Frontend:
  npm ci --prefix apps/server/web
  npm run typecheck --prefix apps/server/web
  npm run build --prefix apps/server/web
  npm test --prefix apps/server/web
```

## Adding a Feature

Follow this order for non-trivial changes:

```text
read specs
  |
  v
identify owner module
  |
  v
add or adjust typed boundary
  |
  v
implement smallest coherent behavior
  |
  v
add unit/integration/browser tests
  |
  v
run make ci
  |
  v
review against specs
```

Keep changes scoped. If a feature needs a new security boundary or remote sharing
behavior, update the specs before implementation.

## 中文开发者指南

这一节写给要改代码的人。浏览器终端模式牵涉到本地 shell、WebSocket、PTY、前端
终端和安全校验，边界比较多。理解这些边界之后再动手，后面的实现和 review 都会
顺很多。

## 工作区结构

```text
termstage/
  Cargo.toml
  Makefile
  crates/
    core/
      src/
        lib.rs
        protocol.rs
        runtime.rs
        security.rs
  apps/
    server/
      src/
        main.rs
        cli.rs
        web.rs
        assets.rs
      web/
        src/
          main.ts
          presentation.ts
          resize.ts
          socket.ts
          terminal.ts
          style.css
        tests/
          smoke.spec.ts
        dist/
          index.html
          assets/
            index.js
            index.css
  specs/
  docs/
    guides/
    research/
```

代码按职责拆得比较清楚：

| 区域 | 位置 | 负责什么 |
| --- | --- | --- |
| 协议类型 | `termstage-core` | token、session name、terminal size、JSON 控制帧。 |
| 安全类型 | `termstage-core` | loopback bind、Host、Origin、peer 地址和 token 比较。 |
| Runtime actor | `termstage-core` | PTY 进程、客户端队列、输出回放和关闭流程。 |
| CLI | `termstage` | 参数解析、配置校验、启动和关闭。 |
| Web server | `termstage` | Axum 路由、WebSocket 桥接、静态资源服务。 |
| Frontend | `apps/server/web` | xterm.js 界面、resize、socket 重连和演示主题。 |

## 架构

```text
+----------------------------------------------------------------------------+
| termstage                                                           |
|                                                                            |
|  +----------+      +-------------+      +--------------------------------+  |
|  | cli.rs   | ---> | web.rs      | ---> | Axum routes                    |  |
|  | clap     |      | AppState    |      | / /assets/{path} /ws /healthz  |  |
|  +----------+      +------+------+      +----------------+---------------+  |
|                           |                              |                  |
|                           | RuntimeCommand               | WebSocket frames |
+---------------------------|------------------------------|------------------+
                            v                              v
+----------------------------------------------------------------------------+
| termstage-core                                                             |
|                                                                            |
|  +----------------+      bounded mpsc       +---------------------------+  |
|  | protocol.rs    | <---------------------> | runtime.rs SessionActor   |  |
|  | security.rs    |                         | - 独占 PTY                |  |
|  +----------------+                         | - 拥有 clients map        |  |
|                                             | - 拥有 replay buffer      |  |
|                                             +-------------+-------------+  |
|                                                           |                |
+-----------------------------------------------------------|----------------+
                                                            v
                                                   shell 或 tmux 进程
```

runtime actor 是最重要的并发边界。HTTP handler 和 WebSocket task 不持有 PTY
handle，也不做阻塞式 PTY 读取。它们只负责把已经校验过的命令放进有界 channel。

## 协议怎么走

终端输入输出走二进制 WebSocket 帧；resize、heartbeat 这类控制消息走 JSON 文本帧。

```text
浏览器输入的字节
  |
  v
WebSocket binary frame
  |
  v
RuntimeCommand::Input { client_id, bytes }
  |
  v
PTY writer

浏览器窗口尺寸变化
  |
  v
WebSocket text frame: {"type":"resize","cols":120,"rows":34}
  |
  v
ClientControlMessage::Resize
  |
  v
RuntimeCommand::Resize { size }
  |
  v
PTY resize
```

这里有几条规则不要破坏：

- 未知 JSON 控制字段必须被 serde 校验拒绝。
- 终端尺寸必须有明确的上下限。
- 终端数据必须保持二进制，不要塞进 JSON。
- Access token 的 debug 输出必须遮蔽敏感内容。
- token 比较必须使用 constant-time equality。

## Runtime 生命周期

runtime 启动时会建一个 actor 线程和一个 PTY reader 线程。

```text
RuntimeSession::start
  |
  +--> native_pty_system().openpty(...)
  |
  +--> clone reader
  |
  +--> take writer
  |
  +--> 用 argv 形式启动 shell 或 tmux，不能拼 shell 字符串
  |
  +--> 启动 PTY reader 线程
  |
  +--> 启动 SessionActor 线程，后续状态都归它管
```

Runtime command 的含义：

| 命令 | 处理方式 |
| --- | --- |
| `AttachClient` | 没有其他 controller 时接入客户端，发送 `Ready`，再回放近期 PTY 输出。 |
| `DetachClient` | 移除客户端，释放 controller 槽位。 |
| `Input` | 只接受当前 controller 的字节，并写入 PTY。 |
| `Resize` | 调整 PTY master 的尺寸。 |
| `Shutdown` | 关闭客户端，丢弃 writer，结束 child，join reader。 |

重连不靠全局 registry。actor 自己保存一段有界的近期输出，用来让新客户端接上时
看到刷新前后的上下文：

```text
PTY 有输出
  |
  +--> 追加到 replay buffer
  |       |
  |       +--> 超过字节上限后，从最旧的 chunk 开始裁剪
  |
  +--> 广播给当前已连接客户端

新客户端 attach 进来
  |
  +--> Ready
  |
  +--> 回放近期 PTY 输出
  |
  +--> 接收实时 PTY 输出
```

## WebSocket 桥接层

每个 WebSocket 连接都会被拆成 receive half 和 send half。桥接层在浏览器消息和
runtime 输出之间做 `select`，哪边先来就先处理哪边。

```text
                       +--------------------+
浏览器 binary -------> | receive half       | --> RuntimeCommand::Input
浏览器 text ---------> | receive half       | --> RuntimeCommand::Resize / Heartbeat
runtime output ------> | send half          | --> 浏览器 binary/text/close
mailbox closed ------> | send close frame   | --> 让浏览器断开
                       +--------------------+
```

如果 runtime 侧的客户端队列已经关闭，桥接层会主动发 WebSocket close frame，然后
退出。这样慢客户端被 runtime 移除后，浏览器端不会一直挂着一个看似还活着的连接。

## 前端怎么组织

前端入口很薄，主要是把几个模块接起来：

```text
main.ts
  |
  +--> readPresentationSettings()
  |
  +--> createTerminalSurface()
  |
  +--> connectTerminalSocket()
  |
  +--> watchTerminalResize()
```

几个模块各管一块：

| 模块 | 做什么 |
| --- | --- |
| `presentation.ts` | 读取 `fontSize` 和 `theme` 查询参数。 |
| `terminal.ts` | 创建 xterm.js terminal 和 addons。 |
| `resize.ts` | 让 xterm 适配容器，并发送 debounce 后的 resize 消息。 |
| `socket.ts` | 连接 WebSocket，发送输入，处理输出，断开后重连。 |

socket 断开后的处理：

```text
socket close
  |
  +--> 是应用主动关闭的吗? 是 --> 停止
  |
  否
  |
  +--> 等待 250ms、500ms、1000ms，之后最多等 2000ms
  |
  +--> 重新打开 socket
  |
  +--> 重新发送最后一次终端尺寸
  |
  +--> 继续 heartbeat
```

## 给贡献者的安全规则

任何从边界进来的值，在校验前都按不可信处理。

```text
边界输入进来
  |
  +--> 立刻校验
  |
  +--> 转成 typed domain value
  |
  +--> 只把 typed value 往里传
  |
  v
业务逻辑 / runtime 逻辑
```

不要引入这些东西：

- 隐式的非 loopback bind 支持；公网暴露必须继续走显式的
  `--expose-public`/`--public-url`/`--token-env` 契约。
- CDN JavaScript。
- 用字符串拼接 shell 命令。
- 无界 channel，或者无界终端输出缓冲。
- token 日志。
- 终端字节日志。
- 生产代码里的 `unwrap()` 或 `expect()`。
- `unsafe`。

远程共享不属于当前功能范围。要做远程共享，必须先单独设计 TLS、认证、授权、
限流、审计日志和只读观看者语义。

## 质量门禁怎么跑

统一使用 Makefile target。这样每个人看到的自动化入口都是一样的。

```bash
make build
make test-cargo
make fmt
make clippy
make clippy-pedantic
make clippy-boundary
make doc
make audit
make deny
make frontend-ci
make ci
```

`make ci` 会跑完整门禁：

```text
Rust:
  cargo build --workspace --all-targets
  cargo test --workspace --all-targets
  cargo +nightly fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic
  cargo clippy --workspace --all-targets -- -D warnings -W clippy::pedantic \
    -W clippy::unwrap_used -W clippy::expect_used \
    -W clippy::indexing_slicing -W clippy::panic
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
  cargo audit
  cargo deny check

Frontend:
  npm ci --prefix apps/server/web
  npm run typecheck --prefix apps/server/web
  npm run build --prefix apps/server/web
  npm test --prefix apps/server/web
```

## 新增功能时的顺序

不是一两行的小改动，建议按这个顺序来：

```text
先读 specs
  |
  v
确认该改哪个模块
  |
  v
添加或调整 typed boundary
  |
  v
实现最小但完整的行为
  |
  v
补 unit / integration / browser tests
  |
  v
跑 make ci
  |
  v
按 specs 做 review
```

保持变更范围收敛。如果一个功能会改变安全边界，或者会让本地工具变成远程共享工具，
先更新 specs，把设计说清楚，再写代码。
