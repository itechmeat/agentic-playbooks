# Server mode: authenticated remote deployment of the apb dashboard

Date: 2026-08-16. Status: approved for implementation planning.

## Motivation

apb's web dashboard and API bind 127.0.0.1 with no authentication. That is safe only because it is loopback-only: the API can create and run playbooks and invoke connector calls, which makes it remote-code-execution-equivalent. The owner wants a supported deployment where apb runs on a server, a domain points at it through a reverse proxy, and access is protected by an issued authorization key. This spec adds that mode. It is also the prerequisite for the webhook ingest feature (separate spec, 2026-08-16-webhook-ingest-design.md): a machine that receives provider webhooks is by definition network-reachable, so the dashboard running next to the ingest listener must be securable first.

## Relationship to the remote-access spec (2026-07-10)

The approved, unimplemented remote-access spec (cloudflared tunnel and a Cloudflare relay) is complementary, not superseded. It commits to reusing the same REST and WS API through a transparent proxy; this spec keeps that assumption intact because the bearer key rides in a normal Authorization header that any reverse proxy or relay forwards unchanged. Two of its patterns are adopted here: secrets-adjacent state lives in a separate 0600 file next to config.yaml, not inside it, and nothing in this spec executes anywhere but the local process. If the tunnel feature is built later, a named tunnel simply fronts an already-authenticated server.

## Design summary

- An operator issues up to two static API keys with `apb server key issue`. A key is `apb_` plus 32 CSPRNG bytes in unpadded base64url. Only the SHA-256 hash is stored, in `<config_dir>/server-auth.yaml` (0600, atomic write). Two keys exist so rotation has no downtime window.
- Auth is enabled if and only if at least one key exists. With no keys, behavior is exactly today's: loopback, unauthenticated. With keys, every `/api/*` route requires auth regardless of bind address.
- Interlock: binding to a non-loopback address with zero keys configured is a hard startup error, not a warning.
- Two credentials are accepted: `Authorization: Bearer apb_...` (CLI, scripts, CI), or a session cookie minted by `POST /api/auth/login` for the browser SPA. The SPA never stores the raw key; it shows a one-time paste screen and exchanges the key for an HttpOnly cookie.
- TLS is the reverse proxy's job (Caddy documented as the default, nginx as the alternative). apb keeps serving plain HTTP behind it.

## Configuration

`GlobalConfig` gains one optional nested section (serde default, so existing configs load unchanged):

```yaml
# <config_dir>/config.yaml
port: 7321
server:
  bind: "127.0.0.1"          # IP address to bind; "0.0.0.0" for server deployments
  public_base_url: null       # e.g. https://apb.example.com; used for printing absolute URLs
  trusted_proxies: []         # peer IPs whose X-Forwarded-For / X-Forwarded-Proto are trusted
```

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub bind: Option<String>,           // parsed as IpAddr at startup; invalid value = startup error
    pub public_base_url: Option<String>,
    pub trusted_proxies: Vec<String>,   // exact peer IPs, no CIDR in v1
}
```

Bind precedence: `--bind` flag > `server.bind` > `127.0.0.1`. Port precedence is unchanged (flag > `port` > 7321). `apb dashboard` gains the `--bind <ip>` flag. `apb dev` keeps its current behavior; if keys exist the developer logs in once through the Vite proxy, no special case.

## Key management

New module `apb_core::server_auth` (shared by cli and server):

- Key file `<config_dir>/server-auth.yaml`, written with `fsutil` atomic write, 0600:

```yaml
keys:
  - id: "3f9c2a1b"                      # first 8 hex chars of the sha256
    sha256: "<64 hex chars>"
    created_at: "2026-08-16T12:00:00Z"  # from apb_core::clock
```

- `issue() -> (String, KeyRecord)`: 32 bytes from the OS CSPRNG (`getrandom`), unpadded base64url, prefixed `apb_`. The plaintext key is returned for one-time printing and never persisted or logged. Refuses to create a third key: the caller must revoke one first (two active keys are the rotation window, not a key-management system).
- `verify(presented: &str) -> Option<key_id>`: SHA-256 of the presented string compared against every stored hash with `subtle::ConstantTimeEq`. No plain `==` on any secret-derived bytes anywhere in this feature.
- The server does not hold the key set as a permanent startup snapshot: it re-stats `server-auth.yaml` before every bearer-key verification (a failure-triggered reload alone cannot catch revocation, because a revoked key still verifies against the stale set and never fails), plus a throttled once-per-minute check on the ordinary request path so a first key turns auth on without a restart. Cost accounting: bearer-authenticated requests pay one stat syscall each; session-cookie and unauthenticated requests stay filesystem-free. Issue and revoke therefore take effect on the very next bearer request.
- CLI surface, new `apb server` command group:
  - `apb server key issue` prints the key once with a short warning that it will not be shown again.
  - `apb server key list` prints id and created_at only.
  - `apb server key revoke <id>` removes the record.

The GitHub-style CRC32 checksum, JWTs, OAuth, passkeys, and N-key label systems are deliberately excluded; the 2026 self-hosted benchmark (Miniflux, Vaultwarden, Grafana service accounts, Home Assistant long-lived tokens) is one or two admin-issued opaque static tokens, and that is what this is.

## Auth enforcement in apb-server

An axum middleware wraps the API. Evaluation order per request:

1. Exempt paths, always reachable: `GET /api/health` (returns only `{"status":"ok"}`, verified to leak nothing else), `POST /api/auth/login`, `GET /api/auth/status`, `POST /api/hooks/{run_id}/{secret}` (authenticated by its own path secret; see hygiene fixes), and the static-asset fallback (the SPA shell must render the login screen). The exemption predicate matches these exact paths; a comment on it must warn that any future non-`/api` prefix added to the router would silently bypass auth and needs a deliberate decision.
2. If no keys exist (auth disabled): pass through. This preserves today's local UX byte for byte.
3. Credentials are evaluated as a union: a valid `Authorization: Bearer <key>` header passes; otherwise a valid `apb_session` cookie passes; 401 with body `{"error":"auth"}` only when neither is present and valid. An invalid or stale bearer header does not block a request carrying a live session cookie.

Sessions: on successful login the server mints a 32-byte random session token, stores its SHA-256 in an in-memory map with a 7-day sliding TTL and a fixed capacity cap (evict oldest), and sets `apb_session` as HttpOnly, SameSite=Lax, Path=/, with no Max-Age (a browser session cookie); the sliding expiry is enforced server-side, so the cookie never outlives the store entry and never expires under a still-active session. The Secure attribute is set when the effective scheme is https, meaning the request arrived with `X-Forwarded-Proto: https` from a trusted proxy peer or `public_base_url` starts with https. Server restart drops sessions; the SPA just shows the login screen again. `POST /api/auth/logout` deletes the session and clears the cookie.

`GET /api/auth/status` returns `{"auth_required": bool, "authenticated": bool}` so the SPA can decide whether to show the login screen without probing a 401.

Every response from the dashboard listener carries `X-Frame-Options: DENY`: an RCE-equivalent panel must not be frameable, and this must not depend on the operator's proxy config.

CSRF: session-cookie-authenticated requests with a method other than GET or HEAD must carry the header `X-Requested-With: apb-dashboard`, otherwise 403. Bearer-authenticated requests are exempt (an attacker cannot set that header cross-site, and a bearer header cannot be attached by the browser automatically). SameSite=Lax remains the first layer; the custom header is the required second layer because the API is RCE-equivalent.

WebSocket `/api/ws` goes through the same middleware at upgrade time: cookie for browsers (sent automatically on the upgrade request), bearer header for non-browser clients. Token-in-query-param is not supported (leaks into proxy logs).

## Brute-force protection and logging

- In-memory fixed-window rate limiter keyed by client IP on the auth-failure path only: more than 10 failed auth attempts (login or bad bearer) per minute from one IP returns 429 for the remainder of the window. No external crate needed; a HashMap window under a mutex is sufficient at this scale.
- Client IP is the socket peer address, unless the peer is listed in `trusted_proxies`, in which case the rightmost entry of `X-Forwarded-For` is used: that is the entry appended by the trusted proxy itself, while leftmost entries are client-supplied and spoofable (Caddy's reverse_proxy appends to an attacker-provided header by default). The deployment docs must state this and show the safe proxy configuration. Forwarded headers are never used for any auth decision, only for rate-limit keying and logging.
- Startup prints a warning when `public_base_url` is set but `trusted_proxies` is empty: behind a proxy every client then shares the proxy's IP as one rate-limit key, so one attacker can exhaust the failure budget for everyone.
- Every auth failure emits one stable, fail2ban-greppable log line to stderr: `apb auth_failed ip=<ip> path=<path>`. Successful auth is not logged per request.

## Hygiene fixes bundled with this feature

- The existing run-hook endpoint compares its path secret with plain `==` (`routes/runs.rs`); switch to a constant-time comparison via the same helper this feature introduces.
- Run-hook secrets are uuid v4 today (122 bits); keep format compatibility but note that new machinery must use 256-bit tokens (`server_auth` and sessions already do).

## Frontend changes

- `web/src/lib/api/http.ts`: attach `X-Requested-With: apb-dashboard` to every request (harmless when auth is off), and translate any 401 response into a global auth-required state instead of a generic error.
- New auth store (`web/src/lib/auth.ts`): holds `auth_required` and `authenticated` from `GET /api/auth/status`, refreshed at app boot and on any 401.
- New login page (`web/src/pages/Login.svelte`): a single key-paste field posting to `/api/auth/login`; on success it re-runs the status check and returns to the previous hash route. Rendered by `App.svelte` before any other page when auth is required and not authenticated. A logout item appears in the topbar only when auth is enabled.
- `web/src/lib/ws.ts`: on WS close caused by auth (server closes unauthenticated upgrades), fall back to the auth-required state rather than silent retry.

## Reverse proxy guidance (docs, not code)

New `docs/DEPLOYMENT.md`: a complete runbook for the supported topology. Caddy example (two lines: domain, reverse_proxy to 127.0.0.1:7321), nginx equivalent, a systemd unit for `apb dashboard --no-open` with `--bind` guidance (keep 127.0.0.1 when the proxy is on the same host; 0.0.0.0 only for a private network with a remote proxy), key issuance walkthrough, fail2ban snippet matching the auth_failed line, and the explicit statement that TLS and HSTS belong to the proxy. README security section and SECURITY.md safe-use section are updated to describe the authenticated mode instead of the current blanket "do not expose".

## New dependencies

Direct additions, latest stable verified at implementation time: `sha2`, `subtle`, `getrandom`, `base64` (all already present transitively in Cargo.lock, so tree impact is near zero). No argon2/bcrypt (wrong tool for 256-bit keys), no JWT crate, no rate-limiter crate.

## Testing

- apb-core: key issue/verify round trip, third-key refusal, revoke, file permission bits, constant-time path exercised, malformed file rejected.
- apb-server integration tests (the suite already spins routers in-process): auth disabled passes; with keys, each of bearer-pass, bearer-fail, login-cookie-pass, cookie-without-CSRF-header-403, exempt routes reachable, 401 shape, rate-limit 429 after 10 failures, WS upgrade rejected without credentials and accepted with cookie, non-loopback-bind-without-keys startup error.
- Frontend: vitest for the auth store transitions and http.ts 401 translation; an SSR smoke test for the login page.

## Out of scope

Multi-user accounts, roles and per-key scopes, passkeys or OIDC, TLS in-process, CIDR trusted-proxy ranges, persisting sessions across restarts, any change to the MCP stdio surface (it does not traverse HTTP), and the cloudflared tunnel implementation itself.

The inbound webhook listener in 2026-08-16-webhook-ingest-design.md builds on
this topology and reuses this spec's constant-time comparison and its
rate-limiting shape, on a second socket with its own router.
