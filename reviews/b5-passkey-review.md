# Review: `feat/b5-passkey` (passkeys for person-only gates and approvals)

Scope: `git diff origin/main...origin/feat/b5-passkey` (44 files, commit
`79a8a5a`). This is an adversarial security and correctness review against the
task's own requirements. No code was changed. Line numbers refer to the
reviewed commit.

## Summary

The verifier primitives (`crates/apb-server/src/webauthn.rs`) are mostly
sound. The feature around them does not work and is not secure:

1. **No ceremony works end to end.** The server rejects every assertion token
   it issued itself (purpose digest mismatch), the default local dashboard
   address yields a broken RP origin, the browser posts `clientDataJSON` in
   base64url while the server parses it as raw JSON, and the decide flow never
   sends the assertion at all. The unit tests mock exactly those seams, so
   they all pass. Once one passkey is enrolled, every person-only gate and
   approval on the machine can no longer be decided.
2. **The protection can be bypassed.** The engine accepts any non-empty
   `passkey` string on a review channel line, and the channel file is plain
   JSONL that a same-user process can write. `count()` treats a corrupt store
   as "no passkey", which switches the gate off. Enrollment hands its one-time
   code to whatever process runs `apb passkey enroll`.
3. **Regressions.** Unattended and enforced engine decisions on bound
   (`approves`) gates are never consumed once a passkey exists, so those runs
   hang.

Verdict at the end: **changes needed**.

## Confirmation of the functional blockers

I appended a throwaway unit test to `routes/passkeys.rs`, ran it with
`cargo test -p apb-server --lib review_probe`, then reverted the file. It
printed:

```
PROBE enroll value={"decision":null,"enroll_new":{},"remove":null} equal=false
PROBE remove equal=false
PROBE decision equal=false
PROBE rp localhost:7777 = Err("origin \"http://localhost7777; https is required except on a loopback http host\" ...")
PROBE verify body without assertion fields ok=false
```

## Findings

### F1. Critical: assertion tokens never match any consumer (purpose digest mismatch)

- `crates/apb-server/src/routes/passkeys.rs:51` (`AssertPurpose::value`)
  serializes the struct with no `skip_serializing_if`, so the digest is taken
  over `{"decision":null,"enroll_new":{},"remove":null}`-shaped JSON. The
  consumers hash a single-key object instead:
  `routes/passkeys.rs:223` (`{"enroll_new":{}}`), `routes/passkeys.rs:472`
  (`{"remove":label}`) and `routes/runs.rs:710-722` (`{"decision":{...}}`).
- Failing scenario: enroll a passkey, then click Decide on a person-only gate.
  `assert/verify` mints a token under digest A, and `POST .../review` looks it
  up under digest B, so the answer is "that passkey assertion is wrong, used or
  expired". Adding a second passkey and removing one fail the same way. The
  probe above confirms all three.
- Combined with F5, enrolling one passkey locks every person-only gate and
  approval on the machine: CLI, MCP and the dashboard are all refused.
- Fix: compute every digest from the same typed value through one function,
  for example `AssertPurpose::decision(..)` and `AssertPurpose::remove(..)`
  built on both sides, with `#[serde(skip_serializing_if = "Option::is_none")]`.
  Add a route-level test that issues a token through `assert_verify_handler`
  and consumes it through `post_review_handler`, `enroll_start_handler` and
  `remove_handler`.

### F2. Critical: the RP origin is malformed for the default local dashboard

- `crates/apb-server/src/routes/passkeys.rs:143-147` builds the origin with
  `format!("http://{}{}", name, port)`, which drops the `:`. `Host:
  localhost:7777` becomes `http://localhost7777`, which is not loopback, so
  the request fails with `BadOrigin`.
- Failing scenario: run `apb serve` on any port other than 80 without
  `server.public_base_url` and open `http://localhost:<port>/`. Enroll/start,
  enroll/finish and assert/verify all return 400, so no passkey can be enrolled
  locally (confirmed by the probe).
- Related problems in the same function:
  - The RP is derived from the attacker-chosen `Host` header. That is
    acceptable only because the auth layer filters `Host`; the code relies on
    this without asserting it.
  - Behind a TLS proxy listed in `server.allowed_hosts` with no
    `public_base_url`, the scheme is always assumed to be `http`, so a correct
    https origin is refused.
  - `GlobalConfig::load()` errors are swallowed (`.ok()`), which silently
    changes the RP.
- Fix: `format!("http://{name}:{port}")` when a port is present. Derive the
  scheme from configuration, not assumption. Return an error when the config
  does not load. Add unit tests for `rp_for_request` with
  `localhost:<port>`, `127.0.0.1:<port>` (must give the IP-literal message)
  and a `public_base_url`.

### F3. Critical: the browser posts `clientDataJSON` in base64url; the server parses it as raw JSON

- `web/src/pages/Passkeys.svelte:95,122` and `web/src/lib/runposts.ts:73`
  send `clientDataJSON: b64u(r.clientDataJSON)`.
  `crates/apb-server/src/webauthn.rs:139-142` runs `serde_json::from_str` on
  that string, and `webauthn.rs:438` hashes its bytes as the clientDataHash.
- Failing scenario: any real registration or assertion from a browser returns
  403 "client data is invalid: clientDataJSON is not valid JSON". The comment
  in `web/src/lib/passkeys.ts:6-7` ("posted verbatim") is wrong.
- Fix: decode base64url on the server, then parse and hash the decoded bytes
  (hash the exact bytes, never a re-serialization). Rust tests should feed the
  same wire format the browser sends.

### F4. Critical: the decide flow throws the assertion away

- `web/src/lib/runposts.ts:85-86`: `verifyAssertion` posts only
  `{ purpose, challenge }`. `web/src/lib/passkeys.ts:115-121` computes the
  assertion and then discards it with `void assertion`.
- Failing scenario: `AssertVerifyBody` (`routes/passkeys.rs:111-120`)
  requires `id`, `clientDataJSON`, `authenticatorData` and `signature`, so
  axum returns 422. A person can never decide a person-only gate or approval
  from the run page (the probe shows the body does not deserialize).
- `web/src/lib/runposts.test.ts:41` and `web/src/lib/passkeys.test.ts:32`
  mock `verifyAssertion`, so no test can see this.
- Fix: pass the assertion into `verifyAssertion(purpose, challenge,
  assertion)` and spread it into the body. Add a test that asserts on the
  posted body, not on a mock's return value.

### F5. High: the engine trusts any non-empty `passkey` string; the stamp is forgeable

- `crates/apb-engine/src/review/channel.rs:101-103` defines
  `passkey_stamp_ok = count()==0 || passkey.is_some()`. It is used by
  `review.rs:383` and `scheduler/review_gate.rs:57`.
  `review.rs:572` `post_decision_passkey` is a `pub` engine function that
  stamps whatever string it is given. The engine never verifies anything.
- Failing scenario: an agent in a run step (or any same-user process without
  an isolated sandbox) appends this line to `<run>/reviews.jsonl` for the open
  request seq:

  ```
  {"seq":N,"node":"g","decision":"approve","request":S,"channel":"dashboard_session","passkey":"x"}
  ```

  The drive consumes it and the person-only gate is decided with no
  authenticator involved. The branch's own test
  `passkey_gate_test::a_dashboard_decision_with_a_passkey_stamp_decides_the_gate`
  does the equivalent with a made-up prefix `"Q0VER0lK"`.
- `docs/GUARDRAILS.md` claims "An agent that reads the config dir and the
  dashboard key file ... still cannot decide". That is false as built: the
  stamp is no stronger than the existing channel stamp.
- Fix direction: make the entry self-verifying. Derive the WebAuthn challenge
  deterministically from `H(run, node, request, nonce, decision, server
  nonce)`. Store credential id, authenticatorData, clientDataJSON and
  signature in the entry. Have the engine re-verify them against
  `passkeys.yaml` at consumption, binding the request seq so a stamp cannot be
  replayed onto another request. Failing that, MAC the entry with a key a run
  step cannot read. Either way, document the remaining trust in the config
  dir.

### F6. High: a corrupt or unreadable passkey store switches the protection off

- `crates/apb-core/src/passkey.rs:336-338`: `count()` returns 0 on a parse
  error. Every decision path (`review.rs:596`, `review/channel.rs:87,102`,
  `next_action/mod.rs:81`, the server routes) then falls back to "no passkey"
  behavior. This contradicts the "fail closed" comments the branch writes
  elsewhere (`review.rs:603-609`).
- Failing scenario: `echo x >> ~/.config/apb/passkeys.yaml` from a run step,
  or a half-written file left by a crash on a filesystem without atomic
  rename. Person-only gates are then decided from the terminal again, and
  `apb doctor` reports "no passkey is enrolled" instead of a broken store.
- The store also has no integrity binding, unlike remembered input (MAC) and
  known problems (signature). A same-user writer can add its own record
  directly and skip enrollment entirely.
- Fix:
  - Make `count()` tri-state, and treat an unreadable or unparsable file that
    exists as "passkeys required, none usable" (refuse with a clear message).
  - Have doctor report the parse error as Fail.
  - Consider signing the store with the installation key, as known problems
    are, and refusing records whose signature does not verify.

### F7. High: enrollment does not require a person; the one-time code goes to whoever ran the command

- `crates/apb-cli/src/passkey.rs:59-68,99-133`: the only guard is
  `StepCaller::from_env()`, which reads environment variables. The code is
  printed to stdout with no terminal check.
- Failing scenario: a coding agent that is not inside a run step (or that runs
  `env -u APB_RUN_ID -u APB_HOST_TASK apb passkey enroll`) reads the code from
  stdout. It then calls `POST /api/passkeys/enroll/start` and `/finish` itself
  with a software P-256 key and a hand-written `clientDataJSON` naming the
  dashboard origin. Nothing on the server distinguishes a browser from curl.
  The agent now holds a passkey and can decide every person-only gate.
- The code file `passkey-enroll-code.yaml` is plain YAML with no MAC, so a
  same-user writer can also mint its own code without running the CLI.
- Fix:
  - Require an interactive terminal, as `--feedback-loop yes` and
    `consent::ask_typed` already do.
  - Better, reverse the direction: the dashboard shows the code and the
    person types it at the terminal, so the secret never crosses a pipe the
    agent reads.
  - MAC the code file.
  - Document that the notify notice is the only detection.

### F8. High: engine decisions on bound gates are never consumed once a passkey exists (regression)

- `crates/apb-engine/src/review.rs:684` exempts engine and scenario posters at
  append time but stores `passkey: None`. The consumers (`review.rs:383` via
  `answers_stamped` for `Consume::Stamped`, and `scheduler/review_gate.rs:57`)
  then call `passkey_stamp_ok`, which is false whenever `count() > 0`.
- Failing scenario: an unattended run with an `approves:` gate declaring
  `unattended: { decide: approve }`, or an enforced review recommendation on
  such a gate. On a machine with one passkey the run waits forever; the doctor
  lists the entry as "no passkey assertion".
- The same applies to scenario runs: `apb eval --level quick` with a bound
  gate on a dev machine with a passkey. CI does not see it because the config
  dir is isolated.
- This contradicts both GUARDRAILS ("exempt") and the CLAUDE.md contract
  ("An unattended or enforced decision on a bound gate ... satisfies
  `requires_approval`").
- Fix: apply the exemption at consumption too, keyed on `by` or the channel
  (`unattended`, `model`, `scenario`). Add tests for both.

### F9. Medium: removing the "last" passkey from the dashboard can race to zero

- `crates/apb-server/src/routes/passkeys.rs:462-471` checks
  `all.len() <= 1` outside the store lock, and
  `apb_core::passkey::remove_by_label_into` re-reads under the lock without
  rechecking.
- Failing scenario: with two passkeys, two concurrent DELETEs with two valid
  remove tokens both see a count of 2 and both remove. The machine ends with
  zero passkeys, bypassing the terminal-code rule for the last one.
- Fix: move the "never the last one" rule into the locked core function, for
  example `remove_by_label_into(path, label, allow_last: bool)`.

### F10. Medium: CLI removal of a non-last passkey needs nothing; the dashboard needs an assertion

- `crates/apb-cli/src/passkey.rs:154`: only the last removal asks for a code.
- Failing scenario: with two passkeys, an agent outside a run step (same
  env-only guard as F7) runs `apb passkey remove laptop` with no terminal and
  no assertion. This weakens the boundary to whichever passkey remains, and
  the dashboard's assertion requirement for the same operation means little.
- Fix: ask for the typed terminal code on every CLI removal, or require a
  terminal at least.

### F11. Medium: the passkey path ignores the request channel and records a false one

- `crates/apb-server/src/routes/runs.rs:705-748`: when `passkey_required`, the
  route never consults `channel` and always posts
  `DecisionChannel::DashboardSession` (`review.rs:588`).
- Failing scenario: a bearer-key client or a keyless dashboard that holds a
  token is recorded as `dashboard_session`, falsifying the audit trail.
- Requirement 4 says a key-made session is never the person. Here the
  assertion is the only person proof, which is defensible, but the journal
  must not claim a channel the request did not use.
- Fix: stamp the real channel, plus the passkey proof. Decide explicitly
  whether a bearer request may carry a token, and test both.

### F12. Medium: next actions and the engine disagree on approvals

- `crates/apb-engine/src/next_action/mod.rs:78-87` returns
  `PasskeyRequired` only for `person_only` gates. `review::passkey_required`
  (`review.rs:596-621`) also covers bound approvals and publish nodes.
- Failing scenario: MCP `run_status` lists "decide" on an `approves:` gate as
  available to the host; `review_decide` is then refused `passkey_required`.
- Publish consents (`NodeKind::Publish`) are pulled under the passkey
  requirement although the task names only person-only gates and approvals.
  If that is intended, say so in the spec and in `docs/PUBLISH.md`.
- Fix: one predicate shared by both, with a test per gate kind.

### F13. Medium: `passkey_required` turns a wrong node into `passkey_required`

- `crates/apb-engine/src/review.rs:609`: an unknown node returns `true`.
- Failing scenario: `apb review <run> no-such-node --decision approve` on a
  machine with a passkey answers "needs a passkey" instead of the existing 404
  or unknown-node error. The web client then starts a ceremony for a gate that
  does not exist.
- Fix: run `check_review_target` before the passkey check, or return `false`
  for a node the playbook does not have. Keep fail-closed only for "playbook
  unreadable".

### F14. Low: verifier details

- `webauthn.rs:430`: equal non-zero sign counts are accepted. WebAuthn
  section 7.2 step 21 treats `new <= stored` with either value non-zero as a
  possible clone. Refuse equality unless both are 0.
- `webauthn.rs:165-209`: the ED flag (0x80) is not handled. Registration with
  extensions puts the extension CBOR after the COSE key, and it is refused as
  "trailing bytes". Assertions with extensions are accepted but unparsed.
  Parse one CBOR item and then the extensions map.
- `webauthn.rs:386`: a missing AT flag reports `AttestationNotNone`. That is
  the wrong message; use `BadAuthenticatorData`.
- `webauthn.rs:481`: trailing bytes after the attestation object are not
  rejected, unlike the COSE key.
- `webauthn.rs:557-563`: `verify` instead of `verify_strict` for Ed25519
  accepts small-order or malleable keys. Use `verify_strict`.
- `webauthn.rs:635-671`: `parse_origin` strips userinfo and drops a path, so
  `http://x@localhost:7777/p` normalizes equal to the dashboard origin. A
  browser never sends those; refuse anything that is not an exact
  `scheme://host[:port]`.
- `crossOrigin` and `topOrigin` in `clientDataJSON` are ignored. Refuse
  `crossOrigin: true`.
- `webauthn.rs:620-622`: the `default_port` computation is dead code (both
  arms give `host`).
- `crates/apb-core/src/passkey.rs:293-300`: the code comparison
  short-circuits. The code is 40 bits and lives 5 minutes, so the risk is low,
  but `ct_eq_str` exists. There is no attempt limit on
  `/api/passkeys/enroll/start`.

### F15. Low: license constraint not met

- `ed25519-dalek` and `curve25519-dalek` are `BSD-3-Clause`. The task allowed
  MIT or Apache-2.0 only. `about.toml` accepts BSD-3-Clause, so the tooling
  passes, but `docs/GUARDRAILS.md` says "all MIT or Apache-2.0", which is
  false.
- Everything else new is MIT and/or Apache-2.0: ciborium*, p256, ecdsa,
  elliptic-curve, ed25519, der, spki, pkcs8, sec1, rfc6979, primeorder, ff,
  group, crypto-bigint, base16ct, base64ct, const-oid, hmac, signature,
  rand_core, sha2, half and hostname. crunchy and fiat-crypto were not in the
  local registry, so I did not check them.
- Fix: get owner sign-off for BSD-3-Clause, or use an Ed25519 verifier under
  MIT or Apache-2.0 (or drop EdDSA). Correct the doc sentence either way.
- `scripts/third-party-notices.sh --check` was not run because cargo-about is
  not installed here.

### F16. Low: test quality

- No route-level test exists for any `/api/passkeys/*` handler or for the
  `passkey_token` branch of `post_review_handler`. F1 to F4 all live exactly
  there.
- The web tests mock `assertChallenge`, `verifyAssertion` and `getAssertion`
  wholesale (`runposts.test.ts:41`, `passkeys.test.ts:32`), so the missing
  body fields and the base64url mismatch cannot fail a test.
- The "fixed vectors" (`webauthn/tests.rs`) are produced by the same code
  under test from seeded keys, not captured from a real authenticator or
  browser. They cannot catch wire-format errors such as F3. Add at least one
  vector captured from a real browser (ES256), plus an RFC 8032 key for EdDSA.
- `replayed_challenge_refused` (`webauthn/tests.rs:237`) tests a different
  challenge, not a replay. Replay is covered only by the in-memory
  `PasskeyState` test, never through a route.
- The assertion path has no UV-missing or UP-missing test (only registration
  has).
- `passkey_gate_test` redirects `APB_CONFIG_DIR` under `common::env_lock()`.
  Other engine tests that decide person-only or bound gates without that lock
  can read a passkey count of 1 during the window, a source of flakes.
  `passkey::count()` would ideally take its directory as a parameter on
  engine paths.
- `an_unstamped_entry_is_never_consumed...` asserts a negative after a fixed
  300 ms sleep, which can pass vacuously on a slow machine.

### F17. Low: UI, docs and the remaining requirements

- `Passkeys.svelte` uses Tailwind utility classes (`mb-2`,
  `text-muted-foreground`). The task bans Tailwind for this work, though the
  existing code uses them.
- `bun run check` reports 4 unused-CSS warnings in `Passkeys.svelte`.
- `algName` mentions RS256 (-257), which the server never accepts.
- `docs/GUARDRAILS.md`, "Passkeys on person-only gates and approvals", needs
  three corrections:
  - It states the boundary holds against an agent that reads the config dir.
    It does not (F5, F6, F7).
  - It calls the out-of-scope case "a virtual authenticator". In fact a plain
    HTTP client with a software key is enough, since the server cannot tell a
    browser from a script.
  - It says "MCP and bearer stay refused as before". A bearer request with a
    token is accepted (F11).
- The notify notice from the server uses `std::env::current_dir()` as the
  project root (`routes/passkeys.rs:515`), which differs from how the
  machine's notify target is resolved elsewhere. Only the CLI path got the
  real root.

## What holds

- ES256 verification goes over `authenticatorData || SHA-256(clientDataJSON)`
  with a DER signature and the point checked on the curve. EdDSA is verified
  over the same message.
- `rpIdHash`, the ceremony type, the UP and UV flags and attestation `none`
  (empty `attStmt`) are all checked.
- COSE keys are re-canonicalized and restricted to EC2/P-256/-7 and
  OKP/Ed25519/-8.
- Challenges are 256-bit random, single use, purpose bound, expire after 2
  minutes and are kept in memory only.
- IP-literal RP IDs are refused with a clear message.
- The store is public material only, written 0600 and atomically under a
  lock. Nothing secret is logged.
- Ordinary review gates (`Consume::Nth`) are unaffected.
- The CLAUDE.md and AGENTS.md shared text is identical.

## Gates

Run on `origin/feat/b5-passkey` (`79a8a5a`) with rustc 1.99.1. I updated the
toolchain first because `mail-parser` needs rustc 1.98, and built `web/dist`
first because rust-embed needs it.

- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace --no-fail-fast` (nextest not installed): 20
  failures on the full parallel run, none of them in a passkey test. On a
  single-threaded rerun, 13 passed (parallel flakes). The other 7 fail
  alone too, and they fail the same way on `origin/main` in this container,
  so the branch did not cause them:
  - `doctor_agents_cli_test::a_redirected_config_dir_does_not_hide_a_billing_deny_from_the_probe`
  - `eval_cli_test::a_kept_tree_survives_the_scratch_cleanup`
  - `host_mode_cli_test::a_require_verdict_resume_re_exposes_only_the_open_task`
  - `host_mode_cli_test::a_resume_after_submission_consumes_it_without_a_new_task`
  - `host_mode_cli_test::a_resume_before_submission_re_exposes_the_same_task`
  - `operator_pause_test::resume_all_waits_out_a_busy_workdir_and_reports_a_run_it_cannot_mark`
  - `process_group_test::timeout_kills_whole_process_tree`

  All new tests pass: `passkey_gate_test`, `webauthn::tests`,
  `passkeys::tests` and `apb_core::passkey` tests. As F16 explains, they do
  not cover the paths that are broken.
- `cd web && bun install && bun run check && bun run test`: check passes with
  0 errors and 4 warnings (unused CSS selectors in `Passkeys.svelte`). Tests:
  113 files and 1112 tests passed.
- `scripts/third-party-notices.sh --check`: not run (cargo-about is not
  installed).
- `code-ranker check .`: not run (not installed here).

Blocking findings: F1, F2, F3 and F4 (the feature does not work, and
enrolling a passkey locks every person-only gate and approval), F5, F6 and F7
(the boundary can be bypassed), and F8 (unattended approvals hang).

Verdict: changes needed
