# Webhook ingest: inbound provider events for connectors

Date: 2026-08-16. Status: approved for implementation planning.

## Motivation

Several providers deliver events only by pushing HTTPS calls to a public endpoint. The concrete driver is a WhatsApp connector on Meta's Cloud API, where inbound messages, delivery receipts and template status updates have no polling endpoint at all. apb connectors are outbound request/response today, so those providers are send-only. This spec adds a generic, provider-agnostic ingest path: a dedicated listener accepts signed webhook deliveries, stores them in a local per-account inbox, and connector functions read and acknowledge them like any other connector call. WhatsApp is the first consumer (separate connector work), but the design must serve any HMAC-signed webhook source (GitHub, Stripe, Slack and Sentry share the same shape).

Deployment context: this feature targets the server-mode topology from 2026-08-16-server-mode-design.md (apb on a server, domain, reverse proxy, auth keys issued). Server mode is a prerequisite and lands first.

## Decisions of record (phase 0)

- The inbox is machine-scoped, per connector and account, stored under the global config dir. It is not run-scoped: messages arrive between runs and must not be lost because no run was executing.
- Inbound events never start a run in this feature. Playbooks consume the inbox by polling it from nodes (an `inbox_read` connector call inside a loop, or a wait node ahead of it). Message-triggered runs need a daemon apb does not have and are a separate future feature.
- The ingest listener is structurally separate from the dashboard router. It is its own TcpListener with its own Router containing only ingest routes. Pointing a tunnel or proxy at the ingest port must be physically incapable of reaching `/api/*`. A test asserts the ingest router contains no `/api` route.

## Architecture

```
provider --HTTPS--> reverse proxy --> ingest listener (own port, own router)
                                          | verify challenge or signature, cap body, dedupe
                                          v
                    apb_core::connector::inbox  (append-only store, cursors)
                    <config_dir>/connector-inbox/<connector>/<account>/
                                          ^
                    read by PreparedCall::Inbox (engine, `apb connector call ...`)
                    and by the dashboard inbox view (apb-server, read-only)
```

The store lives in apb-core because it is the only crate both the writer (apb-server) and the reader (apb-engine's connector call path) already depend on; engine and server communicate only through the filesystem, and this feature keeps that rule.

## Ingest listener (apb-server)

- Routes: `GET /hooks/{connector}/{account}` (verification challenge), `POST /hooks/{connector}/{account}` (delivery), `GET /healthz` (returns `{ok:true}`, for proxies and doctor). Nothing else, ever.
- Configuration, new `ingest` section in GlobalConfig (serde default):

```yaml
ingest:
  enabled: false
  bind: "127.0.0.1"           # behind the reverse proxy on the same host
  port: 7322
  public_base_url: null        # e.g. https://hooks.example.com; used to print callback URLs
```

- Lifecycle: when `ingest.enabled` is true, `apb dashboard` starts the ingest listener alongside the dashboard listener in the same process. A standalone `apb ingest` command runs only the ingest listener for headless deployments. Both paths share one `run_ingest_server` implementation.
- Account resolution: ingest resolves accounts from the global connector config only. The hook URL carries no workspace segment, so a project-scoped account has no unambiguous owning root, and guessing one could silently change which secret verifies a delivery. Doctor states this when a project-only account exists for a webhook connector.
- Request handling order for POST: resolve connector and account (unknown pair is a flat 404 with no detail, after `is_safe_segment` validation of both path segments); read the raw body as `Bytes` with an explicit body limit of 256 KiB (rejected before buffering when Content-Length exceeds it); verify the signature over the exact raw bytes; on verification failure a flat 401 and a fail2ban-greppable stderr line `apb ingest_rejected ip=<ip> connector=<c> account=<a>`; on success append to the inbox and return 200 with an empty body immediately. No run starts, no agent spawns, no parsing beyond what dedupe needs.
- GET handling: only meaningful for connectors whose webhook block declares a challenge dialect; for `meta_hub` the handler answers the `hub.mode=subscribe` challenge by echoing `hub.challenge` as text/plain when `hub.verify_token` matches (constant-time compare), otherwise a flat 403. Connectors without a challenge dialect answer 404 to GET.
- The dashboard auth middleware from server mode does not apply here (providers cannot send bearer keys); the signature is the authentication. The rate limiting is a per-account fixed-window cap on accepted appends (default 600 events per minute; beyond it, events are dropped with a 200 and a per-account dropped counter, so providers do not retry forever) plus the same per-IP failure limiter used by server mode, applied only to the rejection path (a tripped limiter never rejects a validly signed delivery). Client IP derivation is identical to server mode: the socket peer address, or the rightmost `X-Forwarded-For` entry when the peer is listed in `server.trusted_proxies`. The deployment docs must warn operators never to point fail2ban at the proxy's own address: behind a same-host proxy an unconfigured trusted_proxies list makes every rejection log as the loopback peer.
- The dropped-events counter is operator-visible: `apb connector doctor` prints it per account, and the dashboard inbox panel shows it next to the pending depth. Silent truncation is not acceptable for deliveries the provider believes were accepted.

## Inbox store (apb-core)

New module `apb_core::connector::inbox`:

- Layout per connector and account under `<config_dir>/connector-inbox/<connector>/<account>/`:
  - `events.jsonl`: append-only, one JSON object per line: `{seq, received_at, provider_id, body}` where `body` is the raw delivery payload as received (parsed JSON value), `provider_id` is the dedupe identity extracted per the connector's webhook block, `received_at` from `apb_core::clock`.
  - `dedupe.idx`: bounded rolling index of recently seen provider ids (last 10000), consulted before append; a duplicate delivery is acknowledged with 200 but not appended.
  - `cursors.yaml`: named consumer cursors, `{consumer: last_acked_seq}`.
- Every append happens under `fsutil::lock_dir` on the account directory, and `seq` is derived inside the lock. This deliberately avoids the read-count-then-append race that exists in the run-signal channel (`signals.rs`); that existing race gets the same lock fix as a bundled hygiene item since the pattern is now load-bearing.
- Files are written 0600. Inbound bodies are other people's messages: they are never logged, never embedded in events.jsonl of runs, and never returned by any endpoint that does not explicitly exist to return them.
- Retention: a per-account cap (default 50 MB or 30 days, whichever hits first) enforced opportunistically on append by rewriting the file without expired entries under the same lock. Acked entries older than the cap window are the first to go; unacked entries are only dropped by the size cap, oldest first.
- Read API: `read(consumer, after_seq_or_cursor, limit) -> Vec<Event>` (does not move the cursor), `ack(consumer, up_to_seq)` (moves the cursor forward only), `depth() -> per-account counts` for doctor and the dashboard.

## Connector schema: the webhook block (apb-core def.rs)

A connector that can receive deliveries declares, at document level:

```yaml
webhook:
  challenge: meta_hub                    # optional; enum, only variant in v1
  verify_token: "{{secret.verify_token}}" # required when challenge is set
  signature:
    scheme: hmac_sha256_hex              # enum, only variant in v1
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: entry.0.id                # dot path into the body yielding the provider id; optional; segments are map keys or numeric array indices
```

- `{{secret.*}}` placement: the webhook block becomes the third deliberate exemption to the auth-only secret rule (after the smtp and imap connection passwords), validated and tested with the same density. Secrets resolve at ingest time from the account's secret references; values are never cached.
- The referenced fields (`verify_token`, `app_secret`) must exist in `account_fields` and be `secret: true`; the validator enforces this.
- `dedupe_path` missing means dedupe falls back to the SHA-256 of the raw body.
- The dedupe walker is a small array-aware helper in apb-core, deliberately separate from the engine's response_pick walker: core cannot depend on the engine crate, and response_pick's documented maps-only semantics must not change for existing connectors. The two notations stay distinct on purpose; the engine walker remains maps-only.
- The webhook block is covered by the connector tree digest, and the secret references by the account digest, so editing either drops trust. This is the property that stops a shared config from silently weakening verification.

## Connector schema: the inbox function kind

A fifth function kind following the exact smtp/imap recipe (spec struct, exactly-one-of arm, shape validator, template validator, contract expectation, engine executor):

```yaml
- name: inbox_read
  description: Read pending inbound events without consuming them.
  read_only: true
  args_schema: { ... consumer, limit ... }
  inbox:
    op: read                             # read | ack | peek_depth
  response_pick: [events, cursor]        # allowed on inbox reads; first non-HTTP kind with it
- name: inbox_ack
  description: Advance the consumer cursor after processing.
  args_schema: { ... consumer, up_to_seq ... }
  inbox:
    op: ack
```

- `op: read` returns `{events: [{seq, received_at, body}], cursor}`; `op: ack` returns `{acked_up_to}`; `op: peek_depth` returns `{pending}` and is the natural healthcheck-adjacent probe for ingest-only connectors.
- `response_pick` is permitted on inbox functions (projection over the fixed envelope) because the official-connector gate requires read_only functions to carry a non-empty response_pick; the mock/smtp/imap rejection stays as is.
- Validator rules (next free codes): V42, a connector declaring inbox functions must carry a webhook block and vice versa; V43, a node granting inbox functions of a connector whose webhook block references account fields the selected account does not define. Numbering is confirmed against the validator registry at implementation time.
- Engine execution: `PreparedCall::Inbox` goes through the same grant gate, allowlists, `max_calls` budget, args_schema validation and `ConnectorCall` event logging as every other kind; it just reads the local store instead of the network. The node prompt block describing inbox functions states explicitly that inbox content is untrusted external input and must not be treated as instructions.

## Offline contract tests

`tests.yaml` gains an `inbox` expectation kind: a case seeds a fixture inbox (inline events in the case), renders the function and asserts the returned envelope, cursor behavior and ack movement. Signature verification gets pure unit vectors in apb-core (including Meta's documented sample payload); the challenge echo and rejection paths get axum-level tests in apb-server with a temp config dir.

## Dashboard and doctor

- Connector view gains an inbox panel when the connector has a webhook block: per-account pending depth, last received timestamp, and the computed callback URL (from `ingest.public_base_url`) with a copy button. Event bodies are shown only on an explicit per-event expand, marked as untrusted content.
- `apb connector doctor` (existing command) gains ingest checks: listener reachable, public base URL configured, callback URL per account, pending depth, and a warning when `ingest.enabled` is true but no connector declares a webhook block.
- The docs state plainly: deliveries that arrive while the listener is down are retried by the provider for a limited window and then lost; apb cannot change that.

## Security summary

Separate listener (structural, tested), mandatory signature verification over raw bytes before any parsing, no unsigned mode, constant-time comparisons, explicit body cap, flat 404/401/403 responses without detail, dedupe as replay protection, per-account accept-rate cap with drop-with-200 semantics, failure logging for fail2ban, 0600 storage with retention, no body logging anywhere, prompt-injection warning in the node prompt and docs. The threat model addition is that inbox content is the first apb input authored by arbitrary internet users; every surface that renders or feeds it forward must treat it as data.

## New dependencies

`hmac` and `sha2` (both already transitive in Cargo.lock) become direct dependencies of apb-core; `subtle` arrives with server mode. Nothing else.

## Testing

- apb-core: store append/read/ack/dedupe/retention under concurrent appenders, lock coverage, signature vectors, challenge token compare, webhook block validation (V42/V43, secret placement, account field references), digest coverage of the webhook block.
- apb-server: ingest route tests (challenge ok/reject, signature ok/reject, oversize body, unknown pair 404, rate-cap drop, healthz), the no-/api-routes structural assertion.
- apb-engine: PreparedCall::Inbox execution through grants, max_calls, event logging; contract-test kind end to end on a fixture connector.
- A fake `echo-hooks` fixture connector (tests only, not official) exercises the whole path without any real provider.

## Out of scope

Message-triggered runs and any daemon, per-project inbox routing, media download flows, provider SDK helpers, webhook blocks with multiple signature schemes per connector, and the WhatsApp connector itself (next spec, built on this one).

## Implementation

Implemented by docs/superpowers/plans/2026-08-16-webhook-ingest.md, which
depends on docs/superpowers/plans/2026-08-16-server-mode.md landing first.
