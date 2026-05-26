# Browser Terminal User Guide

Status: draft v1
Last updated: 2026-05-19

This guide explains how to run `termstage` as a browser terminal for live
presentations. It focuses on presenter workflows: starting a local session, sharing
the right browser window, preserving state across refreshes, understanding the local
security boundary, and opting into pod exposure when an operator provides HTTPS
ingress and a token environment variable.

## Mental Model

`termstage` does not create a terminal emulator in Rust and it does not sandbox your
commands. It opens a PTY-backed shell or tmux session and displays that session in a
browser terminal.

```text
You type in Chrome
      |
      v
+--------------------+     binary WebSocket frame     +----------------------+
| xterm.js frontend  | -----------------------------> | local Rust server    |
+--------------------+                               +----------+-----------+
                                                              |
                                                              v
                                                     +----------------------+
                                                     | runtime actor        |
                                                     | owns PTY             |
                                                     +----------+-----------+
                                                              |
                                                              v
                                                     +----------------------+
                                                     | shell or tmux        |
                                                     | real user privileges |
                                                     +----------------------+
```

The best presentation setup is tmux mode. It gives you a named session that survives
browser refreshes and lets you keep demo state stable.

## Requirements

Required for the current local workflow:

- macOS or Unix-like environment.
- Rust toolchain pinned by `rust-toolchain.toml`.
- `tmux` installed for the default tmux mode.
- Node.js and npm when rebuilding or testing frontend assets.
- A Chromium-compatible browser for the Playwright smoke test and Chrome-sharing
  presentation workflow.

Check common tools:

```bash
rustc --version
cargo --version
tmux -V
node --version
npm --version
```

## Starting a Presentation Session

Use the default tmux session name, `presentation`:

```bash
cargo run -p termstage --bin termstage -- --session presentation --open
```

The command:

1. Validates CLI arguments.
2. Starts a PTY runtime actor.
3. Starts a loopback-only Axum server.
4. Generates a per-start access token.
5. Opens a local browser URL when `--open` is present.

```text
CLI start
  |
  +--> validate session name / host / font / theme
  |
  +--> start runtime actor
  |
  +--> bind 127.0.0.1:0
  |
  +--> mint token
  |
  +--> open or print URL
```

If browser opening fails, the launch URL is printed once. Open it manually in your
browser.

## CLI Reference

| Option | Default | Use When |
| --- | --- | --- |
| `--session <name>` | `presentation` | You want a stable tmux session name for a talk. |
| `--mode tmux` | `tmux` | You want refresh-safe demo state. |
| `--mode shell` | `tmux` | You want a fresh local shell for quick testing. |
| `--command <path>` | `$SHELL` or `/bin/sh` | You use shell mode and want a specific executable. |
| `-g, --command-arg <arg>` | unset | You need to pass argv to the shell-mode command. |
| `-a, --attach-local-terminal` | off | You want the invoking terminal to control the shell-mode PTY too. |
| `--host <addr>` | `127.0.0.1` | You need a bind address. Non-loopback addresses require `--expose-public`. |
| `--port <port>` | `0` | You need a fixed local port, otherwise let the OS choose. |
| `--open` | off | You want the default browser opened automatically. |
| `--font-size <px>` | `24` | You need larger or smaller presentation text. |
| `--theme high-contrast` | `high-contrast` | You want dark high-contrast terminal colors. |
| `--theme light` | `high-contrast` | You present in a bright room or light slide deck. |
| `--keepalive session` | `session` | You want browser refreshes to reattach to session state. |
| `--keepalive exit` | `session` | You want shutdown to end runtime-owned state. |
| `--expose-public` | off | You are running in a pod behind HTTPS ingress. |
| `--public-url <url>` | unset | You need the browser-visible HTTPS URL for public mode. |
| `--token-env <name>` | unset | You keep the access token in an environment variable. |

## Recommended Demo Workflow

1. Start your session before sharing the screen:

   ```bash
   cargo run -p termstage --bin termstage -- \
     --session presentation \
     --font-size 28 \
     --theme high-contrast \
     --open
   ```

2. Confirm the browser tab shows a prompt.
3. Run a small output check:

   ```bash
   printf 'ready\n'
   ```

4. Share only the browser tab or browser window in your meeting software.
5. If the browser refreshes or briefly disconnects, wait for reconnect. The terminal
   should reattach and replay recent visible output.
6. Stop the CLI with `Ctrl-C` after the presentation.

## Refresh and Reconnect Behavior

The browser socket reconnects with short backoff delays. When the runtime accepts a
new client, it sends a bounded replay of recent PTY output before new output.

```text
Browser refresh
  |
  v
old WebSocket closes
  |
  v
runtime detaches old client but keeps session actor alive
  |
  v
new WebSocket connects with same token
  |
  v
runtime sends Ready + recent PTY replay
  |
  v
presenter continues demo
```

The replay is bounded. It is intended to restore recent visible context, not to act as
a full recording or terminal scrollback database.

## Backpressure Behavior

Terminal programs can emit output faster than a browser can render. `termstage`
uses bounded mailboxes so slow clients cannot grow memory without limit.

```text
PTY emits bytes
  |
  v
client mailbox has room? ---- yes ----> send to browser
  |
  no
  |
  v
log warning + close slow browser client
  |
  v
keep runtime / tmux session alive
```

If this happens during a presentation, refresh the browser tab. The runtime should
accept a new connection and keep the underlying session alive.

## Security Boundary

The local server validates every sensitive request before it reaches the runtime.

```text
HTTP / WebSocket request
  |
  +--> peer IP is loopback?
  |
  +--> Host matches selected loopback host and port?
  |
  +--> token matches this server start?
  |
  +--> WebSocket Origin is same-origin?
  |
  v
upgrade / serve terminal
```

Important limits:

- The default mode is local-only.
- Public pod exposure is opt-in and requires HTTPS ingress, `--public-url`, and
  `--token-env`.
- It does not provide shell sandboxing.
- It does not reduce the privileges of the local user.
- It should not be placed behind tunnels or reverse proxies unless public mode is
  explicitly configured.

## Public Pod Exposure

Public mode is for a container running behind TLS-terminating ingress. The listener
inside the pod can be plain HTTP, but the browser-visible URL must be HTTPS.

```bash
TERMSTAGE_TOKEN=<64-hex-character-token> \
cargo run -p termstage --bin termstage -- \
  --expose-public \
  --host 0.0.0.0 \
  --port 8080 \
  --public-url https://term.example.com \
  --token-env TERMSTAGE_TOKEN
```

In public mode, `termstage` accepts non-loopback peers, builds launch URLs from
`--public-url`, and validates Host and WebSocket Origin against that URL. The token is
still a bearer token in the launch URL, so configure ingress logs and shell history
accordingly and prefer Kubernetes secret-backed environment variables.

## Troubleshooting

### `tmux executable was not found`

Install tmux and retry:

```bash
brew install tmux
tmux -V
```

### Browser does not open

Run without `--open` or copy the printed URL manually:

```bash
cargo run -p termstage --bin termstage -- --session presentation
```

### Browser says forbidden or the WebSocket closes immediately

Use the exact printed launch URL. A missing or stale token will fail. Also check that
the browser is connecting to the same host and port printed by the CLI.

### Text is too small or clipped

Increase font size gradually:

```bash
cargo run -p termstage --bin termstage -- \
  --session presentation \
  --font-size 30 \
  --open
```

If the viewport is narrow, reduce font size or widen the browser window.

### The terminal produced too much output

Refresh the browser. If the runtime closed the slow client due to backpressure, the
new connection should reattach to the session. Avoid commands that stream unlimited
output during a live demo.

## 中文用户指南

这份指南写给实际要拿 `termstage` 做演示的人。你会看到怎么启动会话，怎么调整
字体和主题，浏览器刷新后为什么还能接回去，以及这个工具的安全边界到底在哪里。

## 先建立一个直觉

`termstage` 做的事情很直接：它不是在 Rust 里重新写一个终端，也不会替你隔离
命令。它只是把一个真实的 PTY 会话接到浏览器里，让你可以用更适合演示的方式展示
终端。

```text
你在 Chrome 里输入
      |
      v
+--------------------+     二进制 WebSocket 帧     +----------------------+
| xterm.js 界面      | --------------------------> | 本地 Rust 服务       |
+--------------------+                            +----------+-----------+
                                                           |
                                                           v
                                                  +----------------------+
                                                  | runtime actor        |
                                                  | 持有 PTY             |
                                                  +----------+-----------+
                                                           |
                                                           v
                                                  +----------------------+
                                                  | shell 或 tmux        |
                                                  | 当前用户权限         |
                                                  +----------------------+
```

演示时建议用 tmux 模式。tmux 有具名会话，浏览器刷新了也能重新接上，刚才的上下文
不会轻易丢掉。

## 运行要求

当前这套本地工作流需要：

- macOS 或类 Unix 环境。
- 项目里 `rust-toolchain.toml` 指定的 Rust 工具链。
- 默认 tmux 模式需要本机安装 `tmux`。
- 如果要重新构建或测试前端资源，需要 Node.js 和 npm。
- 如果要跑浏览器冒烟测试，需要 Chromium 兼容浏览器。

检查常用工具：

```bash
rustc --version
cargo --version
tmux -V
node --version
npm --version
```

## 启动一个演示会话

最常用的启动方式是使用默认 tmux 会话名 `presentation`：

```bash
cargo run -p termstage --bin termstage -- --session presentation --open
```

这条命令会按顺序做几件事：

1. 检查 CLI 参数。
2. 启动 PTY runtime actor。
3. 启动只监听 loopback 的 Axum 服务。
4. 生成只属于这次启动的访问令牌。
5. 如果带了 `--open`，打开本地浏览器 URL。

```text
CLI 启动
  |
  +--> 检查 session name / host / font / theme
  |
  +--> 启动 runtime actor
  |
  +--> 绑定 127.0.0.1:0
  |
  +--> 生成本次启动的 token
  |
  +--> 打开或打印 URL
```

如果浏览器没有自动打开，命令行会把启动 URL 打印出来一次。把它复制到浏览器里
打开即可。

## CLI 参考

| 选项 | 默认值 | 什么时候用 |
| --- | --- | --- |
| `--session <name>` | `presentation` | 想给演示固定一个 tmux 会话名。 |
| `--mode tmux` | `tmux` | 希望刷新浏览器后还能接回原来的演示状态。 |
| `--mode shell` | `tmux` | 只是临时开一个新 shell 做测试。 |
| `--command <path>` | `$SHELL` 或 `/bin/sh` | shell 模式下想指定具体命令。 |
| `-g, --command-arg <arg>` | 未设置 | shell 模式下需要给命令传 argv。 |
| `-a, --attach-local-terminal` | 关闭 | 希望当前终端也接管 shell-mode PTY。 |
| `--host <addr>` | `127.0.0.1` | 绑定地址；非 loopback 需要显式设置 `--expose-public`。 |
| `--port <port>` | `0` | 需要固定本地端口时指定，否则让系统分配。 |
| `--open` | 关闭 | 启动后自动打开默认浏览器。 |
| `--font-size <px>` | `24` | 调整演示时的终端字号。 |
| `--theme high-contrast` | `high-contrast` | 使用暗色高对比度主题。 |
| `--theme light` | `high-contrast` | 在明亮环境或浅色页面旁边演示。 |
| `--keepalive session` | `session` | 让浏览器刷新后能重新接回会话。 |
| `--keepalive exit` | `session` | 希望关闭时结束 runtime 管理的状态。 |
| `--expose-public` | 关闭 | 在 HTTPS ingress 后面的 pod 中运行。 |
| `--public-url <url>` | 未设置 | public mode 下浏览器实际访问的 HTTPS URL。 |
| `--token-env <name>` | 未设置 | 从环境变量读取访问 token。 |

## 推荐的现场演示流程

1. 在共享屏幕之前先启动会话：

   ```bash
   cargo run -p termstage --bin termstage -- \
     --session presentation \
     --font-size 28 \
     --theme high-contrast \
     --open
   ```

2. 确认浏览器标签页里已经出现 shell 提示符。
3. 先跑一个很小的输出检查：

   ```bash
   printf 'ready\n'
   ```

4. 在会议软件里只共享这个浏览器标签页或窗口。
5. 如果浏览器刷新或短暂断开，稍等它自动重连。终端会重新接上，并回放近期输出。
6. 演示结束后回到启动命令所在的终端，按 `Ctrl-C` 停止服务。

## 浏览器刷新和重连

浏览器的 WebSocket 断开后会自动重连。runtime 接受新客户端时，会先发 `Ready`，
再把近期 PTY 输出回放一段，让你看到刷新前的上下文。

```text
刷新浏览器
  |
  v
旧 WebSocket 关闭
  |
  v
runtime 移除旧客户端，但 session actor 继续运行
  |
  v
新 WebSocket 带着同一个 token 连接
  |
  v
runtime 发送 Ready + 近期输出回放
  |
  v
继续演示
```

这段回放是有上限的。它的目标是恢复最近的可见上下文，不是录屏，也不是完整的
终端历史数据库。

## 输出太快时会发生什么

有些命令会在短时间里输出大量内容，浏览器可能来不及渲染。`termstage` 使用
有界队列，不会为了慢客户端无限占用内存。

```text
PTY 产生字节
  |
  v
客户端队列还有空间? ---- 有 ----> 发给浏览器
  |
  否
  |
  v
记录 warning + 关闭这个慢客户端
  |
  v
runtime / tmux 会话继续保留
```

如果演示时遇到这种情况，刷新浏览器标签页即可。底层会话仍在，新连接会重新接上。

## 安全边界

本地服务会在请求进入 runtime 前完成必要检查。

```text
HTTP / WebSocket 请求进来
  |
  +--> peer IP 是 loopback 吗?
  |
  +--> Host 和当前 loopback host、端口匹配吗?
  |
  +--> token 是本次启动生成的吗?
  |
  +--> WebSocket Origin 是同源吗?
  |
  v
通过后才升级或返回终端页面
```

重要限制：

- 默认模式仅用于本地。
- pod 公网暴露必须显式启用 public mode，并提供 HTTPS ingress、`--public-url`
  和 `--token-env`。
- 它不是 shell 沙箱。
- 它不会降低当前本地用户权限。
- 除非显式配置 public mode，否则不应该把它放到隧道或反向代理后面。

## Pod 公网暴露

public mode 面向运行在 TLS 终止 ingress 后面的容器。pod 内部监听可以是普通 HTTP，
但浏览器看到的 URL 必须是 HTTPS。

```bash
TERMSTAGE_TOKEN=<64 位 hex token> \
cargo run -p termstage --bin termstage -- \
  --expose-public \
  --host 0.0.0.0 \
  --port 8080 \
  --public-url https://term.example.com \
  --token-env TERMSTAGE_TOKEN
```

public mode 会接受非 loopback peer，用 `--public-url` 生成启动 URL，并按这个 URL
校验 Host 和 WebSocket Origin。token 仍然是启动 URL 里的 bearer token，因此要注意
ingress 日志、shell 历史和部署清单中不要泄漏它，优先用 Kubernetes Secret 注入环境
变量。

## 故障排查

### `tmux executable was not found`

安装 tmux 后重试：

```bash
brew install tmux
tmux -V
```

### 浏览器没有自动打开

可以去掉 `--open`，然后手动复制命令行打印出的 URL：

```bash
cargo run -p termstage --bin termstage -- --session presentation
```

### 浏览器显示 forbidden，或者 WebSocket 立刻断开

请使用完整的启动 URL。token 缺失、复制错了、或者已经过期都会失败。也要确认
浏览器访问的 host 和 port 与 CLI 打印的一致。

### 字太小，或者内容被裁剪

可以先把字体调大一点：

```bash
cargo run -p termstage --bin termstage -- \
  --session presentation \
  --font-size 30 \
  --open
```

如果窗口很窄，反过来需要降低字号，或者把浏览器窗口拉宽。

### 一下子输出太多

刷新浏览器即可。如果 runtime 因为背压关闭了慢客户端，新连接会重新附着到会话。
现场演示时尽量避免执行无限输出或持续刷屏的命令。
