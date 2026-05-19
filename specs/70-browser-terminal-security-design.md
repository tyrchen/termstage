# 70-browser-terminal-security: Local Shell Threat Model

Status: draft v1
Owner: presenterm
Depends on: [00-browser-terminal-prd.md](./00-browser-terminal-prd.md),
[10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md)

## 1. Purpose

This spec defines the security boundary for a local browser terminal. The core risk is
simple and severe: any page that can write to the WebSocket can write bytes to a local
shell. Therefore the M0/M1 product is local-only, token-gated, origin-checked, and
single-controller by default.

## 2. Trust Boundaries

```text
                     untrusted until validated
                              |
                              v
+----------------------+  HTTP/WS   +------------------------------+
| Browser JavaScript   |----------->| Local Axum server            |
| - first-party assets |            | - token check                |
| - no CDN runtime     |<-----------| - Host/Origin/peer checks    |
+----------------------+ terminal   +---------------+--------------+
                         bytes                      |
                                                    | validated commands
                                                    v
                                      +------------------------------+
                                      | Runtime actor                |
                                      | - owns PTY                   |
                                      | - bounded channels           |
                                      +---------------+--------------+
                                                      |
                                                      v
                                      +------------------------------+
                                      | User shell / tmux            |
                                      | OS user privileges           |
                                      +------------------------------+
```

## 3. Mandatory Controls

| Control | Requirement |
| --- | --- |
| Bind address | M0 binds `127.0.0.1`; `::1` allowed only after tests. Never `0.0.0.0`. |
| Token | 256-bit CSPRNG per server start; constant-time comparison; redacted debug/logs. |
| Host | Accept exact loopback host and selected port only. Reject other Host headers. |
| Origin | Accept same-origin browser requests only. Reject missing or mismatched Origin for WebSocket. |
| Peer IP | Reject non-loopback socket peers. |
| Assets | Bundle first-party JS/CSS. No CDN, eval, or runtime script injection. |
| Controller count | One write-capable client by default. |
| Frame limits | Explicit WebSocket frame/message size caps. |
| Logs | Never log terminal bytes, tokens, full URLs containing tokens, or environment secrets. |
| Privileges | Do not run as root/admin; warn or fail when effective UID is root on Unix. |

## 4. Behavior

HTTP requests that fail Host, token, or peer validation return a generic forbidden
response without saying which check failed. WebSocket requests perform the same checks
before upgrade.

The server does not implement TLS for local-only loopback M0. Any LAN, tunnel, or
remote-share mode requires a separate spec with WSS, authentication, authorization,
rate limiting, audit logging, and read-only/viewer semantics.

DNS rebinding is mitigated by requiring all of: unpredictable token, exact Host,
same-origin Origin, and loopback peer address. No single check is treated as sufficient.

## 5. AGENTS.md Binding

- Error handling: security failures are typed and redacted.
- Async/concurrency: failed handshakes do not allocate PTY sessions.
- Type design: `AccessToken`, `AllowedOrigin`, `AllowedHost`, and `PeerAddr` are
  validated newtypes.
- Safety/security: follows `AGENTS.md` Rust Safety, Input Validation, Injection
  Prevention, Resource Limits, and Cryptography sections.
- Serialization: text control frames deny unknown fields.
- Testing: negative tests for token, Host, Origin, peer, frame size, and log redaction.
- Observability: security event spans include reason codes but no secrets.
- Performance: reject invalid requests before expensive session work.
- Documentation: CLI help must state that this is a local shell bridge, not a sandbox.

## 6. Cross-References

- Depends on: [00-browser-terminal-prd.md](./00-browser-terminal-prd.md),
  [10-browser-terminal-protocol-design.md](./10-browser-terminal-protocol-design.md).
- Consumed by: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md).
