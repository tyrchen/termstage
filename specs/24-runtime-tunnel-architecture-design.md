# 24-runtime-tunnel-architecture: Retired Embedded Runtime Tunnel

Status: retired
Owner: termstage
Last updated: 2026-05-27
Depends on: [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md)

## 1. Purpose

This spec previously described an embedded runtime tunnel WebSocket endpoint and
a runtime-specific tunnel bridge between web routing and `RuntimeSession`. That
shape has been retired by
[23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md).

The current direction is not an external WebSocket bridge directly into the old
runtime actor. The next architecture is a backend-session gateway:

```text
Browser xterm.js / Agent API
        │
        │ Termstage Protocol
        ▼
termstage
  - session registry
  - browser websocket gateway
  - semantic API gateway
  - input lease / operation lock
  - backend adapter
        │
        ▼
rmux / tmux / future backend
  - owns actual session/pane/PTY
  - native attach support
```

## 2. Retired Pieces

The following pieces are no longer part of the product contract:

- external runtime tunnel WebSocket route;
- `apps/server/src/tunnel_ws.rs` Axum transport endpoint;
- tests that connect to the tunnel route and expect direct runtime command
  mapping;
- documentation that presents `TunnelFrame` as the external runtime bridge
  protocol.

The in-process bridge used by the current browser route can remain as an
implementation detail until Phase 8 replaces it with the backend-session
gateway. It must not be treated as the final Termstage Protocol.

## 3. Replacement Direction

Phase 8 in [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md)
will introduce the replacement design:

- session registry keyed by termstage session id;
- backend adapter trait for rmux/tmux/future backends;
- browser WebSocket gateway that routes through the registry and adapter;
- Semantic Operations API for agents;
- Level 1 termstage-managed operation lock.

## 4. Cross-References

- Replaced by: [23-local-remote-command-lease-design.md](./23-local-remote-command-lease-design.md).
- Follow-up implementation: [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md)
  Phase 8.
