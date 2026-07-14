# Remote access to WF: tunnel and remote control-plane

> Design document. Two independent optional features sharing a common
> discovery/opt-in mechanism. Implemented as two separate plans (one per
> feature), but a single spec, because the features share the recommendation
> mechanism and the access model.

**Date:** 2026-07-10
**Status:** approved, ready for implementation planning.

## 1. Goal and motivation

WF executes playbooks whose nodes spawn local coding agents as subprocesses:
the agents need the local filesystem, the git repository, keys, and the real
project context. That is why **execution is always local** and never moves to
the cloud. This constraint is deliberate design: code and keys never leak
outward.

The task is to give the user remote access to WF without breaking this
principle. Cloudflare acts as the control-plane (UI, coordination,
authorization, relay), but not as compute: agents never execute on
Cloudflare.

Two scenarios, two features:

- **Quick, secure access to the local UI** from outside, without opening
  ports. Nothing leaves the user's infrastructure, Cloudflare is only a
  reverse proxy. This is `wf tunnel` (Feature 1).
- **Full remote control-plane:** open the UI from a phone from anywhere,
  trigger runs on a remote machine, approve review gates. Especially valuable
  when the CLI is installed on a server that also has agents. This is
  `wf connect` (Feature 2).

Both features are optional and disabled by default. The CLI gently suggests
them once via MCP; the user can decline forever.

## 2. Non-goals

- Executing agents or script nodes on Cloudflare (see section 1).
- Multi-user / team access. In v1 the model is "one user = one Durable
  Object."
- Hosting WF as a public SaaS. The relay serves the specific user who
  deployed it to their own Cloudflare account.
- Portability of the relay to other platforms (a self-hosted VPS, etc.). The
  Cloudflare lock-in is deliberate (see section 4).

## 3. Architecture overview

```
Feature 1 (wf tunnel):
  browser (any) --HTTPS--> Cloudflare edge --cloudflared tunnel--> local wf server
                                (+ Cloudflare Access for named mode)

Feature 2 (wf connect):
  browser <--E2E--> Durable Object (blind relay) <--E2E--> wf-remote daemon --> wf-engine (local)
           ciphertext           forwards bytes only        ciphertext
  The Worker serves the UI static assets. events.jsonl stays on the local machine.
```

Crate split:

- New crate **`wf-remote`** (fronts the local `wf-server` and stands it up):
  relay client (outbound WebSocket), E2E crypto, E2E reverse proxy to the
  local `wf-server` (see 7.3 and 13.4). Isolates the networking and crypto
  logic.
- **`wf-cli`** gets the subcommands `tunnel`, `connect`, `remote` and calls
  `wf-remote`.
- **`wf-mcp`** gets the discovery note and the dismiss tool.
- **`wf-core`** gets a `remote` field in `GlobalConfig`.
- A **`cloudflare/`** directory at the repo root: `wrangler.toml` plus the
  TypeScript code for the worker and Durable Object. Versioned together with
  the CLI so the wire protocol and UI don't drift apart.

## 4. Key decisions and rationale

- **Cloudflare-native, template lives in the repo.** A Durable Object fits
  the task uniquely: one stateful object per user, WebSocket hibernation
  (sleeps and isn't billed while idle), strong consistency. A portable relay
  is YAGNI; there is no second real consumer.
- **E2E encryption is in v1** (not deferred). Agent I/O (prompts, output,
  code) flows through the relay, and Cloudflare must not see plaintext.
- **Discovery via MCP plus a global flag.** The user decides "I don't need
  this" once, for themselves, not per repository.
- **`wf tunnel` as a thin wrapper over `cloudflared`.** Ideal for a headless
  server: there's no browser, and the tunnel gives secure external access
  without punching holes in the firewall. `cloudflared` handles the
  networking side.

## 5. Section A. Discovery / opt-in (shared mechanism)

### 5.1. State storage

A new field in `GlobalConfig` (`crates/wf-core/src/config.rs`):

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteConfig {
    /// State of the remote-access recommendation.
    pub suggest: SuggestState,
    /// URL of the deployed relay (Feature 2); None means not configured.
    pub relay_url: Option<String>,
    // The relay token and pairing secret are NOT stored in the config in
    // cleartext; see section 7.4.
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestState {
    /// Not shown yet, or shown but the user hasn't decided.
    #[default]
    Unshown,
    /// The user declined; stop suggesting.
    Dismissed,
}
```

The field is added to `GlobalConfig` as `pub remote: RemoteConfig`.
`deny_unknown_fields` isn't violated since the field is known. A missing
field in existing configs gets the default (`Unshown`, relay not configured).

### 5.2. Showing the recommendation via MCP

When constructing the server, `wf-mcp` reads `GlobalConfig::load()`. If
`remote.relay_url.is_none()` and `remote.suggest != Dismissed`, the server
adds a short note to the `instructions` field of the `initialize` response.
Example content (the agent paraphrases it **in the user's chat language**,
not verbatim):

> WF can work remotely via Cloudflare. Quick access to the local UI:
> `wf tunnel`. Full remote control-plane (trigger runs and approve review
> gates from a phone): `wf connect --help`. If you don't need this, say so
> and I won't suggest it again.

The note is computed once at session start. If remote is already configured
or a dismissal is already in place, the note is not added.

### 5.3. Dismissal

Two paths, both write `remote.suggest = Dismissed` to `config.yaml`:

- MCP tool `remote_dismiss_suggestion` (no arguments). The agent calls it
  when the user says they don't need this. Returns a confirmation.
- CLI `wf remote dismiss`.

After dismissal, the note stops appearing in every project (the flag is
global). The reverse action: `wf remote suggest` resets the flag to
`Unshown`.

## 6. Section B. Feature 1 - `wf tunnel`

### 6.1. Command

```
wf tunnel [--port N] [--name <NAME>] [--no-open]
```

- Brings up the local `wf server` (reuses the existing server startup path
  and port resolution).
- Spawns `cloudflared` alongside it and prints the public URL.
- `--no-open`: don't open the browser (default for a headless server).

### 6.2. Two modes

- **quick (default):** `cloudflared tunnel --url http://localhost:N`. An
  ephemeral `*.trycloudflare.com` URL, zero configuration, no Cloudflare
  account needed. For one-off/demo access. The URL changes on every run,
  there's no authorization - we print a warning that the link is public and
  knowing it equals access.
- **named (`--name <NAME>`):** a stable URL for a pre-configured named
  tunnel (`cloudflared tunnel run <NAME>`) plus Cloudflare Access (SSO) in
  front of it. Requires the user to have done `cloudflared login` once and
  configured a tunnel with ingress to the local port (steps are in
  docs/INSTALL.md). WF does not auto-detect `cloudflared` configuration: if
  the tunnel isn't configured, `cloudflared` itself exits with a clear
  error, which we show as-is.

### 6.3. Environment checks

- Presence of `cloudflared` is checked via the `wf_core::config::program_in_path`
  helper (same as for agents). If missing, we print installation
  instructions and exit with a non-zero code.
- A `cloudflared` check is added to `wf doctor` (Warn if not installed, with
  an explanation that it's only needed for `wf tunnel`).

### 6.4. The "CLI on a server" case

Headless mode (without `--open`): prints the URL to stdout, the process
holds the tunnel open until Ctrl-C. No inbound ports on the server -
`cloudflared` maintains an outbound connection to the Cloudflare edge. The
user triggers runs on the server's agents through a mobile browser.

### 6.5. Subprocess management

`cloudflared` is spawned following the `dev_cmd` model (see
`crates/wf-cli/src/main.rs`): the local server comes up on a background
thread, `cloudflared` runs as a child process in the shared terminal process
group, and `wf tunnel` blocks until it exits. Ctrl-C reaches the whole
terminal process group and kills both the tunnel and the process. A
programmatic process-group kill (`spawn_in_group` / `kill_process_tree`) is
not needed here: this is a foreground command controlled by the terminal,
not the engine, which needs to kill an agent subtree on the fly. This keeps
`wf tunnel` consistent with the already-working `wf dev`.

### 6.6. Web

The UI is made mobile-responsive (panel and graph layout adapts to a narrow
screen). This is shared work, also useful for Feature 2.

## 7. Section C. Feature 2 - `wf connect` (remote control-plane)

### 7.1. Components

- **`cloudflare/worker.ts`:** serves the UI static assets (the same
  `web/dist` that's embedded in `wf-server`, but here uploaded as worker
  assets) and upgrades `/relay` to a WebSocket, handing the connection to
  the Durable Object.
- **`cloudflare/relay-do.ts`:** a Durable Object, one per user. Blind relay:
  forwards binary WebSocket frames between the daemon and browsers without
  decrypting them. Uses the WebSocket Hibernation API. Routes by the
  cleartext frame envelope (see 7.5). Gates the connection with a relay
  token (7.4).
- **`wf-remote` daemon:** launched by `wf connect`, opens an outbound WSS
  connection to `relay_url`, authenticates with the relay token, performs
  the E2E handshake with each browser, and acts as an E2E reverse proxy to
  the local `wf-server` (see 7.3 and 13.4): it proxies decrypted requests to
  the REST+WS API and encrypts responses and events back.

### 7.2. CLI commands

```
wf connect [--relay URL]      # start the daemon, connect to the relay
wf remote setup               # interactive setup: relay_url, generating
                              #   the relay token and pairing secret, hints
                              #   for `wrangler deploy` and `wrangler secret put`
wf remote pair                # print the pairing QR code and string for a new browser
wf remote dismiss / suggest   # see section 5.3
```

### 7.3. Data flow (E2E reverse proxy)

The transport is an E2E-encrypted reverse proxy over the already-existing
REST+WS API of `wf-server` (rationale and decision in 13.4), not a
standalone command protocol. A single application contract: the same
REST+WS that the local web UI uses. There are no separate `run.start` /
`review.resolve` frames - those were in the initial draft and have been
replaced by this reverse-proxy contract.

1. The browser loads the UI from the worker and talks to the same REST+WS
   API as locally; the transport path is browser <-E2E-> DO <-E2E-> daemon
   <-localhost-> `wf-server`.
2. The daemon is an E2E reverse proxy: it decrypts the frame, proxies the
   HTTP request or WS message to the local `wf-server`, and encrypts the
   response and events back.
3. The run executes locally; the single-writer property is preserved
   (`wf-server`/the engine is the sole writer), `events.jsonl` stays on the
   local machine.

Concrete operations are the regular `wf-server` endpoints: starting a run -
`POST /api/runs` (a new endpoint, see 13.4), review gate - the existing
`POST /api/runs/{id}/review`, streaming events - the regular WebSocket
`/api/ws`. Review gates and the `signals`/`review` infrastructure work the
same as in the local UI, with no remote-specific logic.

### 7.4. Secrets and access tokens

Three distinct values with different purposes and different holders:

- **Relay token (bearer, daemon + worker only):** gates the daemon's
  connection to the DO. Stored as a worker secret
  (`wrangler secret put WF_RELAY_TOKEN`) and in the daemon's local config.
  NOT handed to the browser (otherwise it would leak into localStorage /
  history). The relay checks it when the daemon upgrades the WebSocket.
  Doesn't protect against Cloudflare itself (it can see the token), only
  against outsiders.
- **Browser admission token (one-time, short-lived):** gates the browser's
  WebSocket upgrade without revealing `WF_RELAY_TOKEN` to it. During
  pairing, the daemon generates a short-lived, single-use admission token
  and registers it with the relay over its own authenticated channel; the
  browser presents the token on WS upgrade, the relay validates it and
  invalidates it after use. The exact issuance, transport, TTL, and
  single-use semantics are pinned down in the Feature 2 implementation
  plan.
- **E2E pairing secret (256 bits, high-entropy):** never crosses the relay.
  Pairs the browser and the daemon (E2E channel keys, section 8). Guarantees
  that Cloudflare cannot read the frame contents. Different from the
  admission token: admission gates access to the relay, the pairing secret
  encrypts the payload.

Storage of secrets on the local side: in a separate file
`<config_dir>/remote-secrets.yaml` with `0600` permissions (not in the
shared `config.yaml`), to avoid mixing secrets with settings. The exact file
schema is pinned down in the implementation plan.

### 7.5. Frame format (wire)

A frame is a binary WebSocket frame with a payload opaque to the DO:

```
[ cleartext envelope (for routing at the DO) ][ ciphertext (AES-256-GCM) ]
  envelope: channel_id, direction (daemon<->browser), seq
  ciphertext: encrypted fragment of the HTTP request/response or WS message
              to the local wf-server (reverse proxy, see 7.3 and 13.4)
```

The envelope is in cleartext because the DO must route without decrypting.
The envelope contains nothing sensitive (only the channel id, direction,
seq). The proxied payload (`wf-server`'s REST+WS traffic, including agent
I/O) lives only in ciphertext.

The DO's accepted message limit is 32 MiB. Large bodies (long agent output)
are chunked at the `wf-remote` level before sending.

### 7.6. Browser constraints

Requires native X25519 in Web Crypto: Chrome/Edge 133+, Firefox 130+,
Safari/iOS 17+. The browser feature-detects support and fails closed on
older versions with a clear message (E2E can't be established without
native X25519; we're not pulling in WASM crypto for v1).

## 8. Section D. E2E crypto

The rationale for the choice of primitives comes from research into current
(2025-2026) practice. The deciding criterion: the intersection of "native in
the browser (Web Crypto, no WASM) AND mature in Rust."

### 8.1. Primitives

- Key exchange: **X25519** (ephemeral, per-session).
- Key derivation: **HKDF-SHA-256**.
- Encryption: **AES-256-GCM** (the only AEAD that's native in the browser;
  ChaCha20-Poly1305 and HPKE aren't native in the browser and would require
  WASM).

All three are native in the browser (SubtleCrypto) and first-class in Rust.

### 8.2. Handshake (custom protocol `wf-pair-v1`, NOT Noise)

The scheme below is a one-shot, PSK-authenticated ephemeral X25519 + HKDF
construction, inspired by the idea of Noise `psk` patterns, but NOT
compatible with them and NOT Noise. Real Noise maintains a chaining key and
handshake hash and mixes in the PSK via `MixKeyAndHash`, not via a single
HKDF call. Therefore:

- We do NOT call this `Noise_NNpsk0` and do NOT use `snow`. Both sides (the
  Rust daemon and the browser) implement the same custom construction from
  primitives - otherwise a daemon on `snow`-based Noise and a hand-rolled
  browser wouldn't agree on the wire format.
- Rust: `x25519-dalek` + `hkdf` + `aes-gcm`. Browser: native Web Crypto
  (X25519 `deriveBits` + HKDF `deriveBits` + AES-GCM), no WASM crypto.
- The exact `wf-pair-v1` wire format is pinned down in the implementation
  plan as the canonical reference, identical on both sides.

The PSK is the pairing secret (7.4). Key derivation scheme:

```
ss   = X25519(my_eph_priv, their_eph_pub)
keys = HKDF-SHA-256(salt = PSK, ikm = ss,
                    info = "wf-pair-v1" || eph_pub_daemon || eph_pub_browser)
```

Separate send/recv keys for each direction. Since this is a homegrown crypto
construction, it MUST go through an independent cryptographic review before
Feature 2 ships (see 8.6).

### 8.3. Pairing

`wf remote pair` (or `wf connect` on first run) generates a 256-bit secret
and prints it as a QR code and as a copyable string. The user scans the QR
code or pastes the string into the browser. The PSK never goes over the
relay.

We deliberately don't implement PAKE (a short spoken code, magic-wormhole
style): it would require WASM in the browser (SPAKE2/CPace aren't native)
and isn't needed since the CLI can hand out a high-entropy code directly.

### 8.4. Forward secrecy

**Enabled.** An ephemeral X25519 per session costs one key generation and
one DH - negligible. The sensitivity of the data (code and prompts)
justifies it.

### 8.5. AEAD discipline

- Separate keys for each direction.
- 96-bit nonce from a monotonic counter (never random, never reused across
  keys).
- Rekey well before 2^32 messages per key.
- **Key-confirmation tag** over the handshake transcript before any
  plaintext is sent. A mismatch means the connection is dropped. This is the
  trap for a MITM.

### 8.6. Threat model and the main risk

The relay (Cloudflare) is untrusted and is a natural MITM: it can freely
substitute the ephemeral public keys during the handshake. The whole E2E
guarantee rests on three things:

1. The PSK is mixed into the HKDF salt.
2. Both ephemeral public keys are included in the transcript (`info`).
3. The key-confirmation tag is checked before any plaintext is sent.

A bug in binding the handshake to the PSK means the relay silently reads
everything while the channel "works perfectly." The second most important
risk is nonce reuse in AES-GCM (mitigated by per-direction keys and
monotonic counters).

Because `wf-pair-v1` is a homegrown construction (not an audited Noise
stack, and not Noise at all, see 8.2), it must go through an independent
cryptographic review before the feature ships; the reviewer is given the
threat model above and the canonical wire format from the implementation
plan.

### 8.7. Two faces of the relay (browser vs. cloud MCP)

An important nuance that came up while working through the plugin
integration (see section 13): the relay has two distinct faces with
different privacy models.

- **The browser UI face (Feature 2):** speaks our custom E2E protocol, the
  relay is blind, Cloudflare never sees plaintext. Everything above in
  section 8.
- **The cloud MCP face (Feature 3):** ChatGPT/Claude.ai speak standard MCP +
  OAuth and don't understand our E2E handshake. That means the relay
  terminates MCP and OAuth and sees plaintext MCP traffic. This is not a
  degradation of the threat model: the vendor's cloud (OpenAI/Anthropic)
  already sees all tool I/O by definition - its model is the one calling
  it. E2E blindness only protected against Cloudflare, and that guarantee
  is preserved on the browser face. Both faces live on the same worker.

## 9. Error handling

- **`cloudflared` not installed** (Feature 1): installation instructions,
  exit with a non-zero code, check in `wf doctor`.
- **Old browser without X25519** (Feature 2): fail closed with a clear
  message.
- **Invalid relay token:** the DO rejects the WebSocket upgrade, the
  daemon/browser show an authorization error.
- **Key-confirmation failure:** connection dropped, an explicit message
  about a possible MITM or an incorrect pairing code. No plaintext is ever
  sent.
- **Relay connection drop:** the daemon reconnects with backoff; the browser
  shows a "reconnecting" status.
- **Malformed config / secrets:** the parse error propagates upward (as
  `GlobalConfig::load` already does), never swallowed silently.

## 10. Testing

- **Crypto (Rust, `wf-remote`):** round-trip of the custom `wf-pair-v1`
  handshake (Rust side <-> a test double mirroring the browser construction
  with the same primitives); AEAD framing; nonce counter monotonicity and
  correctness; failure under MITM (substituting ephemeral keys -> failed
  key-confirmation -> connection dropped, no plaintext leaked).
- **Config (`wf-core`):** persisting `remote.suggest = Dismissed` and
  reading it back; defaults when the field is absent.
- **Discovery (`wf-mcp`):** the note is present when `Unshown` and the relay
  isn't configured; absent when `Dismissed` or the relay is configured; the
  `remote_dismiss_suggestion` tool writes the flag.
- **Tunnel (`wf-cli`):** checking for the presence of `cloudflared`; correct
  construction of the `cloudflared` command for quick and named modes
  (without actually spawning it).
- **Relay client (`wf-remote`):** reverse-proxy round-trip (a REST request
  and a WS message to the local `wf-server`) against an in-process mock
  relay; verifying the engine's single-writer property isn't broken on a
  remote-triggered run.
- **Worker (TypeScript, vitest + miniflare):** the DO forwards frames
  without decrypting them; the relay token gates the connection;
  hibernation doesn't drop active connections.

## 11. Deferred (future milestones)

- PAKE / a short spoken pairing code (only if the product needs it; would
  accept the WASM cost then).
- Multi-user / team access (for now one user = one DO).
- An offline command queue: the daemon is asleep, a command waits in the DO
  until reconnection. Valuable for the server case, but v1 only works with a
  live connection.
- A portable relay for third-party platforms (a self-hosted VPS, docker).

## 12. Implementation plan

Separate plans, in order:

0. **Near-term: MCP plugin-readiness** (section 13.5) - a non-blocking
   async run start + tool safety annotations + curating the tool surface.
   Doesn't depend on the relay, useful for every tier (including local)
   right away. Done first.
1. **Feature 1 (`wf tunnel`)** - smaller, self-contained, delivers value
   right away. Includes the shared discovery/opt-in mechanism (section A),
   since it's needed by both features and is easiest to shake out on the
   tunnel.
2. **Feature 2 (`wf connect`)** - the `wf-remote` crate, E2E crypto, the
   Cloudflare Worker + Durable Object, review-gate integration. Transport is
   the E2E reverse proxy over the existing REST+WS API (section 13.4).
3. **Feature 3 (hosted MCP endpoint)** - a future milestone: the MCP face of
   the relay + OAuth 2.1, unlocks ChatGPT Apps and Claude.ai connectors
   (section 13.3).

Mobile-responsive UI (6.6) - shared work, done as part of Feature 1. Tier 0
(packaging the stdio MCP server for Claude Desktop and local CLI agents,
section 13.2) - a lightweight branch, done alongside the near-term item 0.

## 13. Integrating WF as a plugin (ChatGPT / Claude / local agents)

This section pins down the direction for future implementation and
highlights the items that are useful right now. Based on research into
current (2026) platform documentation (dates and sources at the end of the
section).

### 13.1. Main conclusion and tiers

Both ChatGPT (Apps SDK, announced at DevDay Oct 2025) and Claude.ai (custom
connectors) integrate third-party tools via a **remote MCP server over
public HTTPS + OAuth 2.1**. WF already has an MCP server (`wf mcp`, stdio),
so protocol-wise the foundation is ready. From there the picture splits by
execution model:

- **Local agent tier** - consumes MCP over local stdio, WF works as-is, no
  relay (section 13.2).
- **Cloud product tier** - the model runs in the vendor's cloud and can only
  call a public HTTPS endpoint, so a hosted relay is needed = Feature 3
  (section 13.3).

### 13.2. Local agent tier (no relay)

Claude Desktop launches local stdio MCP servers as subprocesses (JSON config
or one-click Desktop Extension `.dxt`/MCPB). opencode, Hermes (v0.7.0+), Pi
are local CLI agents that also consume MCP over stdio. The existing `wf mcp`
plugs into all of them without a relay, with full local access to the
filesystem/git/keys.

Work for this tier (lightweight branch, near-term):
- A Desktop Extension package (`.dxt`/MCPB) for Claude Desktop with a
  manifest.
- Config snippets for opencode (`opencode.json`, `type: local`), Hermes, Pi.
- Documentation "WF as an MCP server" in docs/.
- The MCP-readiness items from 13.5 (needed here too).

Personal use is already available today: ChatGPT developer mode and
Claude.ai add-by-URL don't require submission.

### 13.3. Cloud product tier - Feature 3 (future)

Unlocks ChatGPT Apps and Claude.ai connectors. A layer on top of the relay
(Feature 2), the worker's MCP face (see 13.4). Platform requirements:

- **Transport:** MCP Streamable HTTP (a single endpoint, POST+GET; the SSE
  transport as a separate one is deprecated, spec revision 2025-06-18). Not
  stdio.
- **Authorization:** OAuth 2.1 (PKCE), Protected Resource Metadata
  (RFC 9728), Authorization Server Metadata (RFC 8414), Dynamic Client
  Registration (RFC 7591), audience-binding of tokens (`resource`,
  RFC 8707).
- **UI (optional):** a React component in a sandboxed iframe
  (`window.openai` bridge, JSON-RPC over postMessage; strict CSP). A
  tool-only app is also valid.
- **Async model is mandatory:** ChatGPT's tool-call timeout is ~60s, WF runs
  take minutes. `workflow_run` returns a run id immediately, a separate
  `run_status` polls it; review gates ride on the poll cycle. See 13.5.
- **Safety annotations:** tools that spawn agents and change code must be
  tagged `destructiveHint`; read-only ones - `readOnlyHint`. Otherwise the
  directory rejects the submission.
- **Distribution:** personal/dev use - immediately; public directories
  (`chatgpt.com/apps`, Claude Connectors Directory) - via review (privacy
  policy, annotations, minimal iframe).

Terminology precision: `learn.chatgpt.com/docs/build-plugins` is official
OpenAI content, but it's about **Codex plugins** (the `.codex-plugin`
package format for the Codex coding agent), not about consumer ChatGPT
Apps. The canonical Apps reference is `developers.openai.com/apps-sdk`.
Three different meanings of the word "plugin": ChatGPT Apps, Codex plugins,
Claude Code plugins - don't conflate them.

### 13.4. Feature 2 transport and its relation to the MCP face

Decision on Feature 2's transport: **an E2E reverse proxy over the existing
REST+WS API of `wf-server`** (rather than a bespoke command protocol, as in
the initial draft of §7.3/§7.5). The daemon = an E2E-encrypted reverse proxy
to the local `wf-server`; the DO wraps opaque HTTP/WS frames. The
application protocol (REST+WS) already exists and isn't being rewritten.
The only new application-level work is adding `POST /api/runs` (start a
run) to `wf-server` (currently starting a run only exists in the CLI and
MCP); as a bonus, starting runs will also become available in the local web
UI.

The Feature 3 MCP face lives on the same worker and proxies cloud MCP calls
down to the local `wf mcp`. Both faces (the browser E2E one and the cloud
MCP one) sit on the same Durable Object, see 8.7.

### 13.5. Near-term: MCP plugin-readiness (useful to every tier now)

These items improve WF as an MCP server independent of embedding, and every
tier needs them. Done first (plan 0). The current `wf-mcp` surface is
already almost async: `run_background`, `run_status`, `run_events`,
`review_decide` exist; the only thing still blocking is the main
`workflow_run`.

Doing now:
- **Non-blocking async run start.** A regular (non-supervisory) MCP client
  should be able to start a run and immediately get a run id, instead of
  waiting minutes. Implementation - reuse `run_background` for the
  autonomous mode; polling via the existing `run_status`/`run_events`,
  review via `review_decide`.
- **Tool safety annotations.** `readOnlyHint`/`destructiveHint`/`title`/
  `openWorldHint` on all `wf-mcp` tools. Improves the approval UX in any MCP
  client (Claude Desktop shows a confirmation based on hints) right away,
  and will be mandatory for directories later.
- **Curating the tool surface and descriptions.** A narrow, meaningful
  public set (list/get/validate/run/status/events/review), clear
  descriptions; supervisory tools stay behind the session gate.

Later (readiness for Feature 3, not now, YAGNI):
- Structured output (`outputSchema` / structured content).
- Run logs/report as MCP resources (response-size discipline).
- Aligning the run model with emerging MCP Tasks.
- OAuth 2.1 + Streamable HTTP hosting (this is Feature 3 / the relay).

### 13.6. Feasibility (summary)

- **ChatGPT** - only possible via a hosted relay (Feature 3). ChatGPT has no
  local execution mode.
- **Claude.ai web** - only possible via a hosted relay (Feature 3).
- **Claude Desktop** - possible WITHOUT a relay, using the current stdio
  `wf mcp` (tier 0).
- **opencode / Hermes / Pi** - possible WITHOUT a relay, using the current
  stdio (tier 0).

Conclusion: the relay is necessary and sufficient for cloud products; for
Claude Desktop and local CLI agents it isn't needed - the existing stdio
server is enough.

### 13.7. Sources (verified against primary documentation, 2026)

- OpenAI Apps SDK (MCP server, transport): developers.openai.com/apps-sdk/concepts/mcp-server
- OpenAI Apps SDK (UI, iframe, CSP): developers.openai.com/apps-sdk/build/chatgpt-ui
- OpenAI - Introducing apps in ChatGPT (DevDay, 06.10.2025): openai.com/index/introducing-apps-in-chatgpt/
- OpenAI - submit apps to ChatGPT (17.12.2025): openai.com/index/developers-can-now-submit-apps-to-chatgpt/
- Claude - custom connectors / remote MCP: claude.com/docs/connectors/custom/remote-mcp
- Claude - local MCP on Desktop (stdio/DXT): support.claude.com/en/articles/10949351
- MCP spec 2025-06-18 - transports and authorization: modelcontextprotocol.io/specification/2025-06-18
- opencode MCP servers: opencode.ai/docs/mcp-servers/
- Hermes Agent MCP: hermes-agent.nousresearch.com/docs/guides/use-mcp-with-hermes
