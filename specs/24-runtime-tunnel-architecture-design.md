# 24-runtime-tunnel-architecture: Embedded Runtime Tunnel Layers

Status: implemented
Owner: termstage
Last updated: 2026-05-27
Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
[11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
[20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
[23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md)

## 1. Purpose

This spec defines a three-layer communication architecture between the embedded
web server and the PTY runtime while keeping `termstage` as one process. It is a
refactor of the internal communication boundary, not a process split.

The current CLI remains the user contract. The main `termstage` command still
starts the embedded web server, runtime, and browser launch flow. Existing flags
keep their current meaning:

- `--mode shell`
- `--command`
- `--command-arg`
- `--local-command-terminal`
- `--open`
- `--base-path`
- public exposure flags

## 2. Scope

In scope:

- Introduce an explicit tunnel protocol between web-side routing and
  `RuntimeSession`.
- Keep `RuntimeSession` / `SessionActor` as the PTY and child-process owner.
- Keep browser-facing routes and CLI behavior compatible with today.
- Add WebSocket as the first transport implementation.
- Make future transports possible without changing `SessionActor`.

Out of scope:

- Splitting the embedded web server into a standalone process.
- Creating `termstage-web` or `termstage-agent` binaries.
- EKS deployment, OIDC, Okta, multi-tenant auth, or public gateway routing.
- Agent Sandbox integration.
- HA registry or cross-process session discovery.

Those process, deployment, and identity topics belong in a later spec that builds
on this tunnel boundary.

## 3. Architecture

The design has three layers:

```text
RuntimeSession / SessionActor
        ▲
        │ RuntimeCommand / ClientOutput
        ▼
Runtime Tunnel Bridge
        ▲
        │ TunnelFrame
        ▼
Tunnel Transport
        ▲
        │ WebSocket / TCP / gRPC / QUIC
        ▼
Embedded web tunnel endpoint
```

Layer responsibilities:

| Layer | Rust surface | Responsibility | Must not own |
| --- | --- | --- | --- |
| Runtime | `RuntimeSession`, `SessionActor`, `RuntimeCommand`, `ClientOutput` | PTY, child process, replay, resize, input lease, process exit. | WebSocket, TCP, auth, routing, reconnect policy. |
| Tunnel protocol | `TunnelFrame`, `TunnelControl`, `RuntimeTunnelBridge` | Stable semantic frame model between web side and runtime side. | Socket implementation details. |
| Transport | `TunnelTransport`, `TunnelCodec` | Bidirectional frame IO, encode/decode, heartbeat, close/error mapping, connection-edge backpressure. | PTY state, browser authorization, runtime lease semantics. |

The first implementation keeps all components in one process:

```text
single `termstage` process

┌────────────────────────────────────────────────────────────────────┐
│ Embedded web server                                                 │
│                                                                    │
│  Browser /ws ◀──── browser protocol adapter                         │
│       │                                                            │
│       ▼                                                            │
│  in-process TunnelTransport                                        │
│                                                                    │
│  /tunnel/ws ───── WebSocket TunnelTransport                         │
└──────────────────────────────────────▲─────────────────────────────┘
                                       │ TunnelFrame transport
┌──────────────────────────────────────┴─────────────────────────────┐
│ RuntimeTunnelBridge boundary                                        │
│ - TunnelTransport implementation                                    │
│ - TunnelFrame encode/decode                                         │
│ - TunnelFrame <-> RuntimeCommand / ClientOutput                     │
└──────────────────────────────────────▲─────────────────────────────┘
                                       │ in-process runtime commands
┌──────────────────────────────────────┴─────────────────────────────┐
│ RuntimeSession / SessionActor                                       │
│ - PTY                                                               │
│ - command child                                                     │
│ - replay                                                            │
└────────────────────────────────────────────────────────────────────┘
```

Current embedded server route state:

| Route | Purpose | Status in this spec |
| --- | --- | --- |
| `/` | Serves the browser terminal page. | Existing behavior, unchanged. |
| `/assets/*` | Serves browser JavaScript, CSS, fonts, and static assets. | Existing behavior, unchanged. |
| `/ws` | Existing browser terminal WebSocket used by the page. | Browser protocol adapter backed by `TunnelFrame`. |
| `/tunnel/ws` | WebSocket transport endpoint for `TunnelFrame`. | Added as the first transport-backed runtime bridge endpoint. |
| `/healthz` | Health check. | Existing behavior, unchanged. |

When `--base-path` is configured, all routes mount below that prefix. For
example, `--base-path /p/sess-1/` exposes `/p/sess-1/tunnel/ws`. In embedded
mode the base path remains a reverse-proxy path prefix, not a multi-session
registry. A single embedded web server still owns one runtime session.

Implementation status:

- `RuntimeSession` and `SessionActor` remain the only PTY and child-process
  owners.
- Browser `/ws` is now a browser protocol adapter backed by an in-process
  `TunnelTransport`.
- `/tunnel/ws` is the first external `TunnelTransport` implementation and
  accepts authenticated `TunnelFrame` WebSocket connections.
- The embedded web server still starts from the main command. CLI flags and
  launch URLs remain compatible.

`SessionActor` must not depend on WebSocket, TCP, gRPC, protobuf, HTTP, or any
transport crate. It continues to receive `RuntimeCommand` and emit
`ClientOutput`. The bridge owns translation between the runtime model and the
tunnel frame model.

## 4. Tunnel Protocol

`TunnelFrame` is the stable protocol between the web side and runtime side.
Transports and codecs can change, but frame semantics must remain stable.

| Direction | Frame | Runtime mapping | Purpose |
| --- | --- | --- | --- |
| Runtime side -> Web side | `registerSession` | Startup metadata | Announces session id, command metadata, initial size, capabilities. |
| Web side -> Runtime side | `attachBrowser` | `RuntimeCommand::AttachClient` | Starts routing one browser controller to the runtime. |
| Web side -> Runtime side | `detachBrowser` | `RuntimeCommand::DetachClient` | Removes a browser client. |
| Web side -> Runtime side | `browserInput` | `RuntimeCommand::Input` | Browser terminal bytes. |
| Web side -> Runtime side | `browserResize` | `RuntimeCommand::BrowserResize` | Browser size proposal. |
| Runtime side -> Web side | `ptyOutput` | `ClientOutput::Bytes` | Raw PTY output bytes. |
| Runtime side -> Web side | `runtimeControl` | `ClientOutput::Control` / `Closed` | `ready`, `leaseChanged`, `sizeChanged`, `processExited`, warnings, errors, close reasons. |
| Either | `heartbeat` | Transport liveness | Liveness, backpressure, and reconnect detection. |

All external input to the tunnel protocol is hostile until validated:

- session ids and client ids are bounded and validated before use;
- control frame strings have byte caps;
- terminal payloads have frame size caps;
- unknown frame variants are rejected;
- malformed frames close only the affected tunnel, not the runtime process.

## 5. Transport and Codec

The transport abstraction is intentionally narrow:

```rust
trait TunnelTransport {
    async fn send(&mut self, frame: TunnelFrame) -> Result<(), TunnelError>;
    async fn recv(&mut self) -> Result<Option<TunnelFrame>, TunnelError>;
}

trait TunnelCodec {
    fn encode(&self, frame: TunnelFrame) -> Result<TunnelPayload, TunnelError>;
    fn decode(&self, payload: TunnelPayload) -> Result<TunnelFrame, TunnelError>;
}
```

The first implementation is WebSocket:

- text frames carry JSON control frames;
- binary frames carry terminal payload frames where that avoids unnecessary
  encoding overhead;
- ping/pong or explicit `heartbeat` frames provide liveness;
- close codes map to typed `TunnelError` / runtime close reasons.

Future TCP, gRPC/protobuf, or QUIC transports must implement the same
`TunnelFrame` semantics. They must not require changes inside `SessionActor`.

## 6. Runtime Tunnel Bridge

`RuntimeTunnelBridge` is the only component allowed to translate between
`TunnelFrame` and runtime commands.

Runtime-bound mapping:

| Tunnel frame | Runtime command |
| --- | --- |
| `attachBrowser` | `RuntimeCommand::AttachClient` with a bridge-owned output mailbox. |
| `detachBrowser` | `RuntimeCommand::DetachClient`. |
| `browserInput` | `RuntimeCommand::Input`. |
| `browserResize` | `RuntimeCommand::BrowserResize`. |
| tunnel close | `RuntimeCommand::DetachClient` for attached clients. |

Web-bound mapping:

| Runtime output | Tunnel frame |
| --- | --- |
| `ClientOutput::Bytes` | `ptyOutput`. |
| `ClientOutput::Control` | `runtimeControl`. |
| `ClientOutput::Closed` | `runtimeControl` close reason followed by transport close. |

The bridge owns:

- one or more runtime client mailboxes;
- tunnel send/receive tasks;
- bounded queues on both directions;
- mapping between browser `ClientId` and tunnel stream/controller state;
- shutdown propagation between transport close and runtime detach.

The bridge must not own:

- PTY state;
- replay buffer;
- child process lifecycle;
- browser token validation;
- HTTP route mounting.

## 7. Embedded Web Server Changes

The embedded web server remains started by the main command. It adds a web-side
tunnel endpoint while preserving the existing browser-facing UX.

Required behavior:

1. Browser URL shape remains compatible with today.
2. Browser `/ws` still accepts the existing browser protocol, but translates
   attach/input/resize/detach through `TunnelFrame`.
3. `/tunnel/ws` accepts authenticated WebSocket upgrades and speaks
   `TunnelFrame` over the first WebSocket transport implementation.
4. A tunnel bridge connected to `/tunnel/ws` can translate frames to
   `RuntimeCommand` and runtime output back to frames.
5. `--base-path` applies consistently to browser routes and tunnel routes.
6. Public exposure validation remains unchanged.

The web-side tunnel endpoint depends on `TunnelFrame`, not on browser terminal
frames. This is the boundary that a later split-process spec can reuse.

Final state for this spec:

```text
Browser page
  -> /ws existing browser protocol
  -> embedded browser protocol adapter
  -> in-process TunnelTransport
  -> RuntimeTunnelBridge
  -> RuntimeSession / SessionActor

External tunnel clients
  -> /tunnel/ws WebSocket transport
  -> RuntimeTunnelBridge
  -> RuntimeSession / SessionActor
```

The final state above is still embedded in the main `termstage` process. It does
not introduce a standalone web server, external gateway, remote agent, OIDC, or
multi-session registry.

## 8. Backpressure and Shutdown

Backpressure rules:

- Browser -> web mailbox remains bounded.
- Web -> tunnel transport queue remains bounded.
- Tunnel -> bridge queue remains bounded.
- Bridge -> runtime command channel remains bounded.
- Runtime -> bridge output mailbox remains bounded.
- Slow browser clients are closed without killing the runtime child.

Shutdown rules:

- Browser disconnect detaches only that browser client.
- Tunnel transport close detaches attached browser clients.
- Runtime child exit is reported as `runtimeControl`.
- Main process shutdown closes browser sockets, tunnel transport, bridge tasks,
  and finally `RuntimeSession`.
- Dropping `RuntimeSession` remains the final PTY cleanup guard.

## 9. Implementation Plan

This refactor plan for embedded mode has been implemented.

### Phase 1: Protocol Types

- Add `TunnelFrame`, `TunnelControl`, `TunnelPayload`, and `TunnelError`.
- Add validation constructors for ids, string fields, payload caps, and unknown
  variants.
- Add JSON codec tests for valid and invalid frames.

Exit criteria:

- No runtime or web behavior changes.
- Unit tests cover encode/decode and validation failure cases.

Status: complete.

### Phase 2: Runtime Bridge

- Add `RuntimeTunnelBridge`.
- Translate `TunnelFrame` to `RuntimeCommand`.
- Translate `ClientOutput` to `TunnelFrame`.
- Keep `SessionActor` unchanged and transport-agnostic.

Exit criteria:

- Unit tests cover bridge mappings in both directions.
- Existing direct web/runtime path still works.

Status: complete.

### Phase 3: WebSocket Transport

- Implement `TunnelTransport` for WebSocket.
- Add the embedded tunnel route, for example `/tunnel/ws`.
- Keep the browser-facing `/ws` route unchanged until Phase 4.

Exit criteria:

- The embedded `/tunnel/ws` route accepts authenticated `TunnelFrame`
  WebSocket connections.
- A tunnel WebSocket client can forward a browser-side frame to
  `RuntimeTunnelBridge` and then to `RuntimeSession`.

Status: complete.

### Phase 4: Replace Direct Web Runtime Path

- Route browser attach/input/resize/detach through `TunnelFrame`.
- Remove direct web-server dependency on `RuntimeCommand` where no longer needed.
- Keep CLI flags and launch behavior unchanged.

Exit criteria:

- One browser session can drive a shell command through:

```text
browser /ws
  -> embedded browser protocol adapter
  -> TunnelTransport
  -> RuntimeTunnelBridge
  -> RuntimeSession
```

- PTY output returns through the reverse path and renders in xterm.js.
- Browser refresh replay, resize, process-exit notification, and input lease
  behavior match the direct-channel implementation.
- Existing browser tests pass through the tunnel path.

Status: complete.

## 10. AGENTS.md Binding

- Error handling: tunnel errors use `thiserror`; binaries add `anyhow::Context`.
- Async/concurrency: bridge and transport use bounded channels and explicit
  shutdown. Spawned tasks are joined or deliberately supervised.
- Type design: `TunnelFrame`, `TunnelTransport`, `TunnelCodec`, and
  `RuntimeTunnelBridge` encode invariants in types.
- Safety/security: no `unsafe`; reject malformed external frames at decode time;
  cap all string and byte payloads.
- Serialization: control frames use `serde`, `camelCase`, and
  `deny_unknown_fields`.
- Testing: unit tests for frame validation and bridge mappings; integration tests
  for embedded web server plus tunnel transports.
- Observability: structured `tracing` spans for tunnel connect, attach, detach,
  frame validation failure, backpressure close, and shutdown.
- Performance: use `Bytes` for terminal payloads and avoid copying terminal data
  through JSON.
- Documentation: public tunnel types document lifecycle and failure modes with
  `# Errors`.

## 11. Decisions

- WebSocket tunnel frames use the JSON codec in this spec. A later transport
  optimization may add a binary envelope without changing `RuntimeTunnelBridge`
  or `SessionActor`.
- Tunnel mode is internal default behavior for the embedded browser path. No
  user-facing feature flag is required.
- The embedded tunnel endpoint is `/tunnel/ws`. With `--base-path`, it mounts
  below the prefix exactly like the browser routes.
- In this embedded spec, one browser WebSocket maps to one `ClientId` and one
  bridge-owned runtime mailbox. Multiplexed stream ids are deferred to a future
  multi-client or split-process spec.

## 12. Cross-References

- Depends on: [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md),
  [11-browser-terminal-runtime-design.md](./11-browser-terminal-runtime-design.md),
  [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md).
- Consumed by future updates to:
  [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md),
  [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
