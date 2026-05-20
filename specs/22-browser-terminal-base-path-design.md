# 22-browser-terminal-base-path: Reverse-Proxy Path Prefix

Status: draft v1
Owner: termstage
Depends on: [20-browser-terminal-web-design.md](./20-browser-terminal-web-design.md),
[21-browser-terminal-public-exposure-design.md](./21-browser-terminal-public-exposure-design.md),
[50-browser-terminal-cli-design.md](./50-browser-terminal-cli-design.md)

## 1. Purpose

Run a single termstage server behind an upstream reverse proxy that routes one
URL prefix (per session, e.g. `/p/<sessionId>/`) to that server. The flag is
orthogonal to `--expose-public`: it composes with both local and public modes
and changes only path mounting, not exposure validation.

## 2. Interface

```text
termstage \
  --expose-public \
  --host 0.0.0.0 \
  --port 8080 \
  --public-url https://coder.int.tubi.io \
  --token-env TERMSTAGE_TOKEN \
  --base-path /p/sess-1/
```

| Argument | Required | Meaning |
| --- | --- | --- |
| `--base-path <path>` | no | Absolute, slash-bounded path prefix under which all routes mount. |

## 3. Validation rules

- The value must start and end with `/`.
- It must contain at least one non-empty path segment.
- Each segment must consist of RFC 3986 unreserved bytes plus `-`, `_`, `.`,
  `~`, or percent-encoded `%`. Empty, `.`, and `..` segments are rejected.
- The total length must be at most `BasePath::MAX_BYTES` (256 bytes).
- Validation is performed once at startup and the resulting `BasePath` is
  shared (clone-on-write) across the request path.

## 4. Routing

When set, the router registers `index`, `assets/{*path}`, `ws`, and `healthz`
under the prefix only. The bare-prefix path (without trailing slash) also maps
to the index handler so that browsers requesting `/p/sess-1` are not 404'd.

When unset, the router behaves exactly as in `20-browser-terminal-web-design`.

## 5. Browser asset resolution

Bundled assets are emitted with relative `./assets/...` URLs (`vite base: './'`).
At serve time, `index.html` injects a `<base href="<base-path>">` element so
the browser resolves both asset URLs and the WebSocket URL against the prefix.

`socket.ts` constructs the WebSocket URL from `new URL('ws', document.baseURI)`
so it inherits the same prefix without parsing query parameters or window
location segments.

## 6. Launch URL

The public-mode launch URL is built from `PublicBaseUrl::launch_url_with_base_path`,
substituting the prefix path on the URL before appending query parameters.
The local-mode launch URL inserts the prefix between `host:port` and the
query string. In both cases, when `--base-path` is unset, the rendered URLs
match the pre-existing format byte-for-byte.

## 7. Security behavior

`--base-path` is purely a path-prefix concern. It does not relax any
exposure-mode validation: Host, Origin, peer, and token checks all run
unchanged from `21-browser-terminal-public-exposure-design`. The flag does
not alter whether public mode is required for non-loopback bind addresses.

## 8. Tests

- `BasePath::parse` accepts well-formed prefixes and rejects empty, missing
  slash, double slash, dot, and disallowed-character cases.
- Router unit test: with prefix set, `/p/<id>/` returns 200 and the served
  HTML contains `<base href="/p/<id>/">`; the unprefixed `/` returns 404.
- Router unit test: default mode (no prefix) still serves `/`, has no
  `<base href>` element, and matches the pre-existing routes.
- Public launch URL test: prefix appears in the rendered URL exactly once,
  before the query string.
- CLI: `--base-path` parses valid prefixes into `BasePath`; invalid values
  fail validation before any runtime allocation.

## 9. AGENTS.md Binding

- Type design: `BasePath` is a validated newtype constructed via `parse`.
- Safety/security: prefix bytes are restricted to a path-safe set; injection
  into `<base href>` HTML escapes the standard set of characters even though
  the byte allowlist already excludes the dangerous ones.
- Testing: behavior is covered by unit tests in both `termstage-core` and
  the server crate; default-mode parity is asserted explicitly.
