# 21-browser-terminal-public-exposure: Pod and Internet Exposure

Status: draft v1
Owner: termstage
Depends on: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
[50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md),
[70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md)

## 1. Purpose

This spec defines the smallest safe public exposure mode for running `termstage` in a
container behind an ingress or reverse proxy. Public exposure is opt-in and does not
relax local-mode defaults: `termstage` remains loopback-only unless the operator passes
an explicit exposure flag and a validated token source.

## 2. Interface

Pod-oriented command:

```text
termstage \
  --expose-public \
  --host 0.0.0.0 \
  --port 8080 \
  --public-url https://term.example.com \
  --token-env TERMSTAGE_TOKEN
```

Public mode arguments:

| Argument | Required | Meaning |
| --- | --- | --- |
| `--expose-public` | yes | Enables internet-facing validation rules. Without it, non-loopback bind hosts remain rejected. |
| `--public-url <url>` | yes | Browser-visible HTTPS base URL supplied by ingress/proxy configuration. |
| `--token-env <name>` | yes | Environment variable containing the 64-hex-character access token. |

`--token <value>` and token files are deliberately excluded from the initial public
mode. Environment variables map cleanly to Kubernetes secrets and avoid putting the
token in CLI arguments, shell history, or `kubectl describe pod` command arrays.

## 3. Flow

```text
Operator / Pod             Ingress / TLS                termstage Pod
     |                          |                             |
     | 1. env TERMSTAGE_TOKEN   |                             |
     |    set from Secret       |                             |
     |                          |                             |
     | 2. start --expose-public ----------------------------->|
     |    --public-url https://term.example.com               |
     |                          |                             |
     |                          | 3. HTTPS GET /?token=... -->|
     |                          |    Host: term.example.com   |
     |                          |                             |
     |                          |<-- 4. terminal page --------|
     |                          |                             |
     |                          | 5. WSS /ws?token=... ------>|
     |                          |    Origin: https://term...  |
     |                          |                             |
     |                          |<-- 6. terminal byte stream -|
```

## 4. Invariants

- `--host 0.0.0.0` is never accepted unless `--expose-public` is also present.
- Public mode requires `--public-url` with `https` scheme, a host, no username,
  password, query, or fragment, and an explicit or default HTTPS port.
- Public mode launch URLs are built from `--public-url`, not the pod bind address.
- Public mode accepts non-loopback peers because ingress/proxy traffic reaches the pod
  as ordinary TCP peers.
- Public mode validates `Host` and WebSocket `Origin` against `--public-url`.
- Public mode requires `--token-env`; generated per-run tokens remain local-mode only.
- Token environment variable names are ASCII `[A-Z_][A-Z0-9_]{0,127}`.
- The token value must parse as the existing 256-bit `AccessToken` hex format.
- Public mode assumes TLS termination before `termstage`; the in-pod listener can be
  plain HTTP only when `--public-url` is HTTPS.

## 5. Security Behavior

The access token is still passed in the launch URL for this phase to preserve the
existing browser contract. This is acceptable only because public mode requires an
operator-provided high-entropy token and generic forbidden responses. Future cookie or
header authentication can supersede URL tokens without changing the PTY protocol.

Failed Host, Origin, peer, and token checks return the same generic forbidden response.
Logs may contain stable reason codes but never token values, full tokenized URLs, or
terminal bytes.

Rate limiting is deferred because the current project has no rate-limit dependency or
middleware. The first public exposure phase keeps the surface small and records rate
limiting as the next hardening item before multi-user or long-lived internet service
deployment.

## 6. AGENTS.md Binding

- Error handling: CLI uses `anyhow` with context; core validation types use
  `thiserror` through existing protocol/security errors.
- Async/concurrency: no new tasks; public mode reuses the existing Axum server and
  runtime actor.
- Type design: exposure mode, public base URL, and token environment name are validated
  domain types before server startup.
- Safety/security: follows `AGENTS.md` Rust Safety, Input Validation, Injection
  Prevention, Resource Limits, and Cryptography sections; no public mode is enabled by
  default.
- Serialization: N/A.
- Testing: parser/config tests for required public flags; security tests for public
  Host/Origin acceptance and mismatch rejection.
- Observability: public-mode logs redact tokens and never print full tokenized URLs
  except the explicit launch URL.
- Performance: request validation remains allocation-light and happens before runtime
  session work.
- Documentation: CLI help must state that public mode exposes a real shell/tmux
  session as the container user.

## 7. Cross-References

- Depends on: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
  [50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md),
  [70-browser-terminal-security-design.md](./70-browser-terminal-security-design.md).
- Consumed by: [72-browser-terminal-verification-plan.md](./72-browser-terminal-verification-plan.md),
  [90-browser-terminal-roadmap.md](./90-browser-terminal-roadmap.md),
  [91-browser-terminal-impl-plan.md](./91-browser-terminal-impl-plan.md).
