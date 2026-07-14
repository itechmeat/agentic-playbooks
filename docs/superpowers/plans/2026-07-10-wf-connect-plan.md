# Feature 2: `wf connect` (Remote Control-Plane) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Status: FUTURE WORK. Do not start implementation until explicitly scheduled.** This is the largest remote-access feature and carries a mandatory independent cryptographic review gate (Phase G) before any release.

**Goal:** Provide a full remote control-plane: open the wf web UI from anywhere (including a phone), trigger runs, and confirm review gates on a remote machine, via a user-deployed Cloudflare Worker + Durable Object, with end-to-end encryption, so the relay (Cloudflare) never sees plaintext.

**Architecture:** An E2E-encrypted reverse proxy on top of the ALREADY existing REST+WS API of `wf-server` (design doc section 13.4, not a bespoke command protocol). Flow: `browser <-E2E-> Durable Object (blind relay) <-E2E-> wf-remote daemon <-localhost-> wf-server`. The new `wf-remote` crate holds the relay client, the crypto (`wf-pair-v1`), and the reverse proxy. The Cloudflare part (`cloudflare/`) is a TypeScript Worker + Durable Object built with wrangler. Runs execute locally; the single-writer invariant is not violated.

**Tech Stack:** Rust (edition 2024): `x25519-dalek`, `hkdf`, `sha2`, `aes-gcm`, `hmac`, `tokio-tungstenite`, `axum` (the existing wf-server); TypeScript + Cloudflare Workers + Durable Objects + wrangler; browser: Svelte 5 + native Web Crypto (SubtleCrypto). Tests: `cargo test`, vitest + miniflare.

## Global Constraints

- Do not use em dashes anywhere: code, comments, or documentation. Only a regular hyphen, a comma, or rephrasing.
- Do not use exclamation marks in documentation or messages.
- Comments in this project are written in Russian (follow the existing style of the files).
- **Execution happens only locally.** Cloudflare is the control-plane/relay, not compute. No agents and no script nodes run on the worker.
- **Single-writer invariant.** Only the local `wf-server`/engine writes `events.jsonl`. The daemon is a proxy; it never writes events.
- **Blind relay.** The Durable Object forwards opaque encrypted frames and NEVER decrypts the payload (design doc sections 7.5, 8.7).
- **The `wf-pair-v1` crypto is a bespoke construction, NOT Noise** (design doc section 8.2). Both sides (Rust and the browser) implement the exact same canonical wire format (see below). Before release, an independent crypto review is mandatory (Phase G).
- **`WF_RELAY_TOKEN` is never handed to the browser.** The browser authenticates with a one-time, short-lived admission token (design doc section 7.4).
- **Browser requirement:** native X25519 in Web Crypto (Chrome/Edge 133+, Firefox 130+, Safari/iOS 17+). Below that, fail closed with a clear message, no WASM crypto.
- Local-side secrets go in `<config_dir>/remote-secrets.yaml` with `0600` permissions, separate from `config.yaml`.
- Every task ends with green tests for the affected component (`cargo test -p <crate>` / `vitest`) and `cargo clippy --workspace --all-targets` with no new warnings; crypto tasks additionally require `cargo audit` with no new advisories.

## `wf-pair-v1` wire-format canon (normative for Phase C and Phase E)

Parties: **B** (browser, initiator) and **D** (daemon, responder). `PSK` is a 32-byte pairing secret (design doc sections 7.4/8.3). All keys are X25519/ephemeral.

Handshake (through the relay; on top of the encrypted relay channel alone is not enough - this IS the E2E layer):

1. `B -> D`: `eph_pub_B` (32 bytes, X25519 public).
2. `D -> B`: `eph_pub_D` (32) `|| confirm_D` (32).
3. `B -> D`: `confirm_B` (32).

Key derivation (identical on both sides):

```
ss   = X25519(eph_priv_self, eph_pub_peer)                      # 32 bytes
okm  = HKDF-SHA-256(salt = PSK, ikm = ss,
                    info = "wf-pair-v1" || eph_pub_B || eph_pub_D)  # expand -> 96 bytes
k_b2d = okm[0..32]     # AEAD key for the browser -> daemon direction
k_d2b = okm[32..64]    # AEAD key for the daemon -> browser direction
kc    = okm[64..96]    # key-confirmation key
```

Key confirmation (checked BEFORE sending any plaintext; a mismatch means abort):

```
confirm_D = HMAC-SHA256(kc, "wf-pair-v1|D" || eph_pub_B || eph_pub_D)
confirm_B = HMAC-SHA256(kc, "wf-pair-v1|B" || eph_pub_B || eph_pub_D)
```

Transport frames (post-handshake): AES-256-GCM, a per-direction key (`k_b2d` / `k_d2b`).
- Nonce (12 bytes) = `dir_salt` (4 bytes, fixed for the session and direction, from an `okm` extension or a per-direction constant) `|| counter` (8 bytes, big-endian, monotonic starting at 0). Never random, never reused.
- The counter must not overflow: abort and re-handshake once it reaches `2^48` messages per key (with plenty of headroom before `2^64`).
- AEAD AAD = the frame's cleartext envelope (`channel_id`, `direction`, `seq`), so the envelope itself is authenticated.

Threat model and the main risk (design doc section 8.6): the relay can swap the ephemeral keys; the guarantee rests on mixing the PSK into the HKDF salt, including both `eph_pub` values in `info`, and checking key confirmation before any plaintext.

---

## File Structure

- `crates/wf-server/src/lib.rs` (modify) - the `POST /api/runs` route (start a run; also covers starting from the local UI).
- `crates/wf-server/tests/post_runs_test.rs` (create) - starting a run over HTTP.
- `crates/wf-core/src/config.rs` (modify) - `RemoteConfig.relay_url` already exists (Feature 1); add reading/types for the secrets file if needed.
- `crates/wf-remote/` (create) - a new crate:
  - `Cargo.toml`, `src/lib.rs`
  - `src/secrets.rs` - `remote-secrets.yaml` (relay token, pairing secret), `0600` permissions.
  - `src/crypto.rs` - `wf-pair-v1` (X25519/HKDF/AES-GCM/HMAC, nonce counters, key confirmation).
  - `src/relay.rs` - the relay client (outbound WSS, the relay token, reconnect/backoff).
  - `src/proxy.rs` - the reverse proxy: decrypt a frame -> request to the local `wf-server` -> encrypt the response.
  - `src/pairing.rs` - generating a pairing secret + an admission token, printing a QR code/string.
- `crates/wf-cli/src/main.rs` (modify) - the `connect`, `remote setup`, `remote pair` subcommands (alongside `remote dismiss/suggest` from Feature 1).
- `cloudflare/` (create) - `wrangler.toml`, `src/worker.ts` (assets + WS upgrade), `src/relay-do.ts` (a Durable Object, blind relay, hibernation, admission/relay-token gate), `test/relay.test.ts` (vitest + miniflare), `package.json`, `tsconfig.json`.
- `web/src/lib/remote/` (create) - `wfpair.ts` (the browser side of `wf-pair-v1` on Web Crypto), `transport.ts` (an E2E reverse-proxy client: wraps the app's fetch/WS in an encrypted channel), the pairing-screen integration.
- `docs/INSTALL.md` (modify) - deploying the worker (`wrangler deploy`, `wrangler secret put`), `wf remote setup`, pairing.

---

## Phase A: `POST /api/runs` in wf-server

Self-contained and also useful for the local UI (right now starting a run only exists in the CLI/MCP). The reverse proxy (Phase F) reuses this endpoint.

### Task A1: start a run over HTTP

**Files:**
- Modify: `crates/wf-server/src/lib.rs`
- Test: `crates/wf-server/tests/post_runs_test.rs`

**Interfaces:**
- Consumes: `wf_engine::{run_background, RunOptions, RunMode}`.
- Produces: `POST /api/runs` with the body `{ "workflow": String, "version": Option<String>, "params": Map<String,String>, "instruction": Option<String> }` -> `{ "run_id": String }` (202-like, non-blocking).

- [ ] **Step 1: Write a failing test**

Create `crates/wf-server/tests/post_runs_test.rs`. Bring up `build_router` with a temporary project (as in the existing server tests; check their setup), send a `POST /api/runs` via `tower::ServiceExt::oneshot`, verify a 200 and the presence of `run_id`. Use a no-agent workflow (start -> prompt -> finish) so no agent is needed.

```rust
// Skeleton: seed a no-agent workflow "demo", POST /api/runs, wait for run_id.
// (Exact router/oneshot setup: copy from the nearest server test.)
#[tokio::test]
async fn post_runs_starts_run_and_returns_run_id() {
    // 1. tempdir + init_project + no-agent workflow "demo".
    // 2. build_router(AppState::new(root)).
    // 3. oneshot POST /api/runs {"workflow":"demo"}.
    // 4. assert status 200 and a non-empty body.run_id.
}
```

- [ ] **Step 2: Run it - it fails** (`cargo test -p wf-server --test post_runs_test`; the route is missing -> 404/405).

- [ ] **Step 3: Implement**

In `build_router`, replace the runs line with:

```rust
        .route("/api/runs", get(list_runs_handler).post(post_run_handler))
```

Add the request body and the handler (following the pattern of `post_review_handler`):

```rust
#[derive(Deserialize)]
struct StartRunBody {
    workflow: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    params: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    instruction: Option<String>,
}

/// Starts a run in the background (autonomous), returns run_id immediately.
/// Non-blocking, like the MCP `workflow_run background:true` - the client/UI
/// then polls for status.
async fn post_run_handler(
    State(state): State<AppState>,
    Json(body): Json<StartRunBody>,
) -> impl IntoResponse {
    if !is_safe_id(&body.workflow) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let opts = wf_engine::RunOptions {
        instruction: body.instruction,
        params: body.params,
        allow_shared_workdir: false,
        mode: wf_engine::RunMode::Autonomous,
        supervisor_expected: false,
        max_patches_per_run: None,
        context_max_bytes: None,
        context_compact_model: None,
        overrides: None,
    };
    match wf_engine::run_background(&state.root, &body.workflow, body.version.as_deref(), opts) {
        Ok(run_id) => Json(serde_json::json!({ "run_id": run_id })).into_response(),
        Err(wf_engine::EngineError::NotFound(_)) =>
            (StatusCode::NOT_FOUND, format!("workflow `{}` not found", body.workflow)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
```

(Check the exact import paths for `RunOptions`/`RunMode`/`run_background` in `wf_engine`.)

- [ ] **Step 4: Run it - green.**
- [ ] **Step 5: Clippy + commit** (`git commit -m "feat(server): POST /api/runs to start a run"`).

---

## Phase B: `wf-remote` crate + secrets

### Task B1: crate skeleton + secrets file

**Files:**
- Create: `crates/wf-remote/Cargo.toml`, `crates/wf-remote/src/lib.rs`, `crates/wf-remote/src/secrets.rs`
- Modify: the root `Cargo.toml` (workspace members)
- Test: `crates/wf-remote/tests/secrets_test.rs`

**Interfaces:**
- Produces:
  - `pub struct RemoteSecrets { pub relay_token: String, pub pairing_secret: [u8; 32] }`
  - `RemoteSecrets::load() -> Result<Option<Self>, String>` (None if the file doesn't exist)
  - `RemoteSecrets::save(&self) -> Result<(), String>` (writes `<config_dir>/remote-secrets.yaml`, chmod `0600` on Unix)

- [ ] **Step 1: Test** - a save/load round trip via `WF_CONFIG_DIR`; on Unix, verify the file's permissions are `0o600` (via `metadata().permissions().mode() & 0o777`).
- [ ] **Step 2: Run it - it fails.**
- [ ] **Step 3: Implement** - a serde struct; `pairing_secret` (de)serializes as a base64/hex string; `save` writes atomically (`wf_core::fsutil::atomic_write`), then on Unix calls `set_permissions(0o600)`.
- [ ] **Step 4: Green.**
- [ ] **Step 5: Clippy + commit.**

---

## Phase C: `wf-pair-v1` crypto (security-critical)

Implement the wire-format canon (see the section above) on top of Rust primitives. This is the side the browser must match (Phase E). An isolated, fully unit-testable module.

### Task C1: key derivation + key confirmation

**Files:**
- Create: `crates/wf-remote/src/crypto.rs`
- Test: `crates/wf-remote/tests/crypto_handshake_test.rs`

**Interfaces:**
- Produces:
  - `pub struct HandshakeKeys { pub k_b2d: [u8;32], pub k_d2b: [u8;32], pub confirm_b: [u8;32], pub confirm_d: [u8;32] }`
  - `pub fn derive_keys(psk: &[u8;32], eph_pub_b: &[u8;32], eph_pub_d: &[u8;32], ss: &[u8;32]) -> HandshakeKeys`
  - initiator/responder helpers that generate an ephemeral pair and compute `ss`.

- [ ] **Step 1: Test** - round trip: B and D each generate an ephemeral pair, exchange public keys, each calls `derive_keys`, the keys match (`k_b2d`/`k_d2b`), `confirm_*` match on both sides; with DIFFERENT PSKs, the keys and confirm values diverge (a MITM that swaps eph keys will not pass key confirmation).
- [ ] **Step 2: It fails.**
- [ ] **Step 3: Implement** using `x25519-dalek` (ephemeral keys), `hkdf`+`sha2` (HKDF-SHA-256, expand to 96 bytes with `info` per the canon), `hmac`+`sha2` (confirm). The order `eph_pub_B || eph_pub_D` in `info` and in the HMAC must strictly follow the canon.
- [ ] **Step 4: Green.**
- [ ] **Step 5: Clippy + `cargo audit` + commit.**

### Task C2: AEAD framing + nonce counter

**Files:**
- Modify: `crates/wf-remote/src/crypto.rs`
- Test: `crates/wf-remote/tests/crypto_frame_test.rs`

**Interfaces:**
- Produces:
  - `pub struct SealDir { key, dir_salt: [u8;4], counter: u64 }` with `seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Vec<u8>`
  - `pub struct OpenDir { key, dir_salt, next_counter }` with `open(&mut self, aad, ciphertext) -> Result<Vec<u8>, CryptoError>`
  - Abort once `counter == 2^48`.

- [ ] **Step 1: Test** - seal/open round trip; the sequential counter increases; open with a wrong AAD (a tampered envelope) is rejected; a repeated/skipped counter is rejected; an overflow (artificially set the counter near `2^48`) returns an error, not a panic.
- [ ] **Step 2: It fails.**
- [ ] **Step 3: Implement** using `aes-gcm` (AES-256-GCM), nonce = `dir_salt || counter.to_be_bytes()`, AAD = the passed-in envelope.
- [ ] **Step 4: Green.**
- [ ] **Step 5: Clippy + audit + commit.**

### Task C3: full handshake state machine + MITM test

**Files:**
- Modify: `crates/wf-remote/src/crypto.rs`
- Test: `crates/wf-remote/tests/crypto_mitm_test.rs`

**Interfaces:**
- Produces: an `Initiator`/`Responder` state machine: `step` functions that produce the 3 canon messages; they finalize into `(SealDir, OpenDir)` only after key confirmation succeeds.

- [ ] **Step 1: Test** - (a) an honest round trip between B and D over an in-memory channel yields working transport keys; (b) a MITM swaps `eph_pub` in message 1 or 2 -> key confirmation does NOT match -> finalization returns an error, no transport keys are handed out, no plaintext is sent.
- [ ] **Step 2: It fails.**
- [ ] **Step 3: Implement** the state machine strictly per the canon; confirm is checked before `SealDir` is ever handed out.
- [ ] **Step 4: Green.**
- [ ] **Step 5: Clippy + audit + commit.** Note in the task report: this module requires an independent crypto review (Phase G).

---

## Phase D: Cloudflare Worker + Durable Object (blind relay)

### Task D1: worker skeleton + wrangler

**Files:**
- Create: `cloudflare/package.json`, `cloudflare/tsconfig.json`, `cloudflare/wrangler.toml`, `cloudflare/src/worker.ts`, `cloudflare/src/relay-do.ts`
- Test: `cloudflare/test/relay.test.ts`

- [ ] **Step 1: Test (vitest + miniflare)** - the worker responds on a health path; the UI's static assets are served; `/relay` upgrades to a WebSocket. (Mocking the UI assets is sufficient.)
- [ ] **Step 2: It fails / no project yet.**
- [ ] **Step 3: Implement** `worker.ts`: serving assets (`web/dist`, uploaded as the worker's assets) + the `/relay` route -> `env.RELAY_DO.get(...)` (one DO per user, its id derived from a fixed name or a hash of the relay token). `relay-do.ts`: the WebSocket Hibernation API, accepting two connection roles (daemon, browser).
- [ ] **Step 4: Green.**
- [ ] **Step 5: Commit.**

### Task D2: access gates (a relay token for the daemon, an admission token for the browser) + blind forwarding

**Files:**
- Modify: `cloudflare/src/relay-do.ts`
- Test: `cloudflare/test/relay.test.ts` (extend)

**Interfaces:**
- DO: the daemon upgrades with `Authorization: Bearer <WF_RELAY_TOKEN>` (a worker secret); the browser upgrades with a one-time admission token that the daemon pre-registers with the DO over its own (authenticated) channel. An invalid/used token -> the upgrade is refused.

- [ ] **Step 1: Test** - (a) a daemon without a valid relay token is rejected; (b) a browser without a valid admission token is rejected; (c) the admission token is single-use: a second upgrade with the same token is rejected; (d) a frame from the browser reaches the daemon BYTE-FOR-BYTE (the DO does not decrypt or mutate the payload).
- [ ] **Step 2: It fails.**
- [ ] **Step 3: Implement** the `WF_RELAY_TOKEN` check, registering/consuming admission tokens in the DO's state, and blind forwarding of binary frames between the two sides by `channel_id` from the cleartext envelope. The payload is never touched.
- [ ] **Step 4: Green.**
- [ ] **Step 5: Commit.**

---

## Phase E: browser side of `wf-pair-v1` + the E2E reverse-proxy client

### Task E1: `wfpair.ts` (Web Crypto) + feature detection

**Files:**
- Create: `web/src/lib/remote/wfpair.ts`
- Test: `web/src/lib/remote/wfpair.test.ts` (vitest)

**Interfaces:**
- Produces: the same canon primitives on `SubtleCrypto` (X25519 `deriveBits`, HKDF `deriveBits`, AES-256-GCM, HMAC-SHA256), byte-for-byte compatible with Rust (Phase C). Feature-detect X25519; if absent, throw a clear error (fail closed).

- [ ] **Step 1: Test** - compatibility vectors: the same inputs (PSK, ephemeral keys) fixed by the Rust test (Task C1) produce the same `k_b2d`/`k_d2b`/`confirm_*`. Record the shared test vector in both tests.
- [ ] **Step 2: It fails.**
- [ ] **Step 3: Implement** on Web Crypto, strictly per the canon.
- [ ] **Step 4: Green.**
- [ ] **Step 5: Commit.**

### Task E2: pairing screen + the E2E transport client

**Files:**
- Create: `web/src/lib/remote/transport.ts`, a pairing component
- Test: `web/src/lib/remote/transport.test.ts`

**Interfaces:**
- Produces: a client that (a) accepts a pairing secret (scan a QR code / paste a string) and an admission token, (b) opens a WS connection to `/relay`, (c) runs the `wf-pair-v1` handshake, (d) wraps the app's REST+WS calls in the encrypted channel (a fetch-like interface on top of E2E).

- [ ] **Step 1: Test** - against an in-memory relay stub: the handshake succeeds, an encrypted request/response round trip works; a wrong pairing secret -> abort at key confirmation.
- [ ] **Step 2..5** similarly; commit.

---

## Phase F: daemon reverse proxy + CLI

### Task F1: relay client (outbound WSS, reconnect)

**Files:** `crates/wf-remote/src/relay.rs` (+ a test against an in-process mock relay).
- Produces: connects to `relay_url` with the relay token, sends/receives frames, reconnects with exponential backoff, connection status.
- TDD: connect/reconnect against a mock; token authentication.

### Task F2: reverse proxy (frame -> local wf-server -> frame)

**Files:** `crates/wf-remote/src/proxy.rs` (+ a test).
- Produces: for each browser channel - a `wf-pair-v1` handshake (responder = the daemon), then: decrypt the frame -> an HTTP request or a WS message to the local `wf-server` (started by the daemon) -> encrypt the response/events back. Check: the engine's single-writer invariant is not violated (the daemon never writes events directly).
- TDD: reverse-proxy round trip (REST `POST /api/runs` and WS `/api/ws`) through a mock relay; verify the prod path only goes through `wf-server`.

### Task F3: pairing + CLI `wf connect` / `wf remote setup` / `wf remote pair`

**Files:** `crates/wf-remote/src/pairing.rs`, `crates/wf-cli/src/main.rs` (+ a CLI test).
- Produces:
  - `wf remote setup` - generates a `relay_token` and a `pairing_secret`, writes `remote-secrets.yaml` (0600), prints the `wrangler deploy` and `wrangler secret put WF_RELAY_TOKEN <...>` steps, saves `relay_url` in `config.yaml`.
  - `wf remote pair` - generates a one-time admission token, registers it via the daemon channel (or prints it for manual handoff within the session), prints the pairing secret as a QR code and a string for the browser.
  - `wf connect [--relay URL]` - starts the local `wf-server`, opens the relay connection, serves the reverse proxy until Ctrl-C.
- TDD: `wf connect` without configured secrets -> a clear error; constructing commands/messages without real network IO.
- Commit after each task.

---

## Phase G: review gates (mandatory before release)

### Task G1: independent cryptographic review of `wf-pair-v1`

- [ ] Assemble a package for the reviewer: the wire-format canon (section above), the threat model (design doc section 8.6), the sources of `crates/wf-remote/src/crypto.rs` and `web/src/lib/remote/wfpair.ts`, the compatibility test vectors.
- [ ] Conduct an independent crypto review (an external reviewer or a separate crypto-focused agent reviewer), checking the PSK binding, the key-confirmation ordering, nonce discipline, and the absence of timing leaks in the confirm comparison (constant-time).
- [ ] Fix the findings, repeat until a clean verdict. The feature does not ship without this.

### Task G2: final whole-branch review

- [ ] `superpowers:requesting-code-review` on the entire branch (the most capable model): correctness of the reverse proxy, the relay's blindness, single-use admission tokens, no leakage of `WF_RELAY_TOKEN` to the browser, single-writer.
- [ ] Fix Critical/Important findings, then `superpowers:finishing-a-development-branch`.

---

## Final check (after all phases)

- [ ] `cargo test --workspace` - green.
- [ ] `cargo clippy --workspace --all-targets` - no new warnings.
- [ ] `cargo audit` - no new advisories.
- [ ] `cd cloudflare && vitest` and `cd web && vitest` - green; `bun run build` (web) passes.
- [ ] The `wf-pair-v1` test vectors match between Rust (Phase C) and the browser (Phase E).
- [ ] The crypto review (G1) and the whole-branch review (G2) are closed.
- [ ] `grep -rnP "\x{2014}"` over the changed files - no em dashes.
- [ ] Manual end-to-end: `wf remote setup` -> `wrangler deploy` -> `wf connect` -> open the UI from a phone via the worker's URL -> pairing -> start a run -> confirm a review gate; the relay (DO logs) sees no plaintext.
