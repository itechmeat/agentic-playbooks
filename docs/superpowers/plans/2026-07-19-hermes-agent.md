# Hermes Agent Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Hermes Agent (Nous Research, `hermes` CLI) as the sixth known executor agent in apb: detection probe, built-in invocation form, tests including a live smoke hook, docs, and the 0.6.0 release.

**Architecture:** Same rails as codex and opencode: a `Probe` entry in `apb-core/src/detect.rs`, a `builtin()` arm in `apb-engine/src/invocation.rs`, and the existing generic headless adapter does the rest. Verified live contract: `hermes -z PROMPT` prints only the final response text to stdout and exits 0; `-m MODEL` selects the model with provider auto-detection. Spec: `docs/superpowers/specs/2026-07-19-hermes-agent-design.md`.

**Tech Stack:** Rust workspace edition 2024. No new dependencies.

## Global Constraints

- Branch: `feat/hermes-agent`. One PR. Never push (the controller handles push and release).
- Every commit: `git commit --signoff`, message ends with the trailer line `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- No em-dashes (U+2014), no exclamation marks, no CJK anywhere. English machine-facing text.
- Gates per task before DONE: `cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; tests of touched crates green.
- Testing rules (docs/TESTING-GUIDELINES.md): integration tests only in the crate's single binary (`tests/main.rs` + `tests/suite/<name>.rs` + `mod` line); unit tests inline in src. Tempdirs only. No real network or real agent execution in normal tests; the live hermes test is `#[ignore]` and env-gated. Executable stub scripts are written with create + write_all + sync_all before exec (use the existing shared helpers).
- The invocation form is EXACTLY `["-z", "{prompt}", "-m", "{model}"]`, `SoulDelivery::Prefix`, no soul flag, `Transport::Headless`, empty `autonomous_args`.
- The detect probe is EXACTLY: id `hermes`, bins `["hermes"]`, `AgentCategory::Aggregator`, version args `["--version"]`, `ModelsSource::None`, new `AuthSource::Hermes` (presence of `<home>/.hermes/.env` maps to `AuthKind::ApiKey`; values never read).

---

### Task 1: Core detection probe

**Files:**
- Modify: `crates/apb-core/src/detect.rs` (builtin_probes, AuthSource enum + its match, the builtin agent-id set near line 613, "known five" doc comments)
- Test: `crates/apb-core/tests/suite/detect_test.rs`

**Interfaces:**
- Produces: `builtin_probes()` includes the hermes probe per Global Constraints; `AuthSource::Hermes` variant handled wherever AuthSource is matched. Task 2 relies on the agent id string `hermes`.

- [ ] **Step 1: Read detect.rs and detect_test.rs.** Understand how existing probes are tested: stub binaries on a temp PATH, fake HOME for auth sources, the DetectCache fingerprints. Note every place `AuthSource` is matched and every doc comment saying "five".

- [ ] **Step 2: Write failing tests** in `detect_test.rs`, following the file's existing stub-binary pattern (synced write helper, 0o755):

- `hermes_probe_detects_stub_binary`: temp PATH with a stub `hermes` that prints `Hermes Agent v0.18.2 (2026.7.7.2) · upstream e361c5e2` on `--version`; assert detection reports agent `hermes`, installed true, version containing `0.18.2` (store whatever the existing probes store, first line), category Aggregator serialization, `models` is None.
- `hermes_auth_hint_from_env_file`: fake HOME containing `.hermes/.env`; assert auth hint kind `ApiKey`. Without the file: no auth hint.
- `hermes_missing_binary_reports_not_installed`: empty PATH; installed false.

- [ ] **Step 3: Run to verify failure** - `cargo test -p apb-core --test main detect`.

- [ ] **Step 4: Implement**: the probe entry, the `AuthSource::Hermes` variant and its file check, add `"hermes"` to the builtin id set, update "five" wording to six in doc comments you touched.

- [ ] **Step 5: Run to green, gates, commit** - `feat(core): hermes agent detection probe`.

---

### Task 2: Engine invocation arm and adapter test

**Files:**
- Modify: `crates/apb-engine/src/invocation.rs` (builtin match arm + the doc comment listing known agents)
- Test: `crates/apb-engine/tests/suite/adapter_test.rs` (or the suite file where per-agent adapter runs live; follow the existing per-agent stub tests), plus a unit test beside `builtin()`'s existing tests if the file has them

**Interfaces:**
- Consumes: agent id `hermes` from Task 1.
- Produces: `builtin("hermes")` returning the exact form from Global Constraints; used by Task 3's live smoke.

- [ ] **Step 1: Write the failing unit test**: `builtin_hermes_form` asserting argv `["-z", "{prompt}", "-m", "{model}"]`, `SoulDelivery::Prefix`, `soul_flag` None, `Transport::Headless`, empty `autonomous_args`. Place it wherever `builtin()` behavior is already asserted (search for `builtin(` in engine tests and src test modules; follow that location).

- [ ] **Step 2: Run to verify failure**, implement the arm:

```rust
        // hermes one-shot mode prints only the final response text to
        // stdout and auto-bypasses approvals by design (script mode);
        // the SOUL travels as a prompt prefix like the other aggregators.
        "hermes" => Some(mk(
            &["-z", "{prompt}", "-m", "{model}"],
            SoulDelivery::Prefix,
            None,
            &[],
        )),
```

Update the "known five" doc comment on `builtin()` to six.

- [ ] **Step 3: Write the failing adapter integration test** in the engine suite, mirroring the existing stub-agent tests exactly (synced stub write, APB_AGENT_CMD or program override as the file's other tests do): a stub `hermes` shell script that prints its argv to stdout; run a node through a hermes-bound profile (or the adapter directly, whichever the neighboring tests do) and assert the captured argv contains `-z`, the prompt with the SOUL text prefixed, `-m`, and the model id, and that the node output equals the stub's stdout.

- [ ] **Step 4: Run to green, gates, commit** - `feat(engine): hermes builtin invocation form`.

---

### Task 3: Live smoke hook and docs

**Files:**
- Modify: the live-smoke suite `crates/apb-cli/tests/suite/live_smoke_test.rs` (APB_LIVE_TEST_ pattern), `docs/PROFILES.md`, `docs/MCP.md`, `docs/HOST-INTEGRATION.md`

**Interfaces:**
- Consumes: Tasks 1-2 (detection + invocation).

- [ ] **Step 1: Live smoke test** `live_hermes_oneshot`, `#[ignore]`, gated on `APB_LIVE_TEST_HERMES=1`, following the file's existing `require_env`/probe pattern: invoke the real `hermes` binary with `-z` and a trivial deterministic prompt (`Reply with exactly the text APB_OK and nothing else`) plus `-m` taken from env `APB_LIVE_HERMES_MODEL` (document in a comment at the call site: the user's configured hermes default model id, for example `glm-5.2`); assert exit success and stdout contains `APB_OK`. Keep the invocation consistent with `builtin("hermes")` (same flags), so the smoke actually exercises the shipped form.

- [ ] **Step 2: Docs.** Grep `docs/PROFILES.md`, `docs/MCP.md`, `docs/HOST-INTEGRATION.md` for the places agents are enumerated (search `opencode`) and add hermes with one line of context where the format calls for it (for example: detection, model suggestions fall back to the curated table, one-shot `-z` execution). Match each file's existing tone and depth; no new sections unless the file's structure demands one.

- [ ] **Step 3: Run gates** (`cargo test -p apb-cli`, fmt, clippy; `cargo test -p apb-cli -- --ignored --list` shows the new test), commit - `docs: hermes agent docs and live smoke hook`.

---

### Task 4: Version 0.6.0 and release notes

**Files:**
- Modify: root `Cargo.toml` (0.5.0 -> 0.6.0) and every inter-crate `version = "0.5.0"` pin (`grep -rn '0\.5\.0' Cargo.toml crates/*/Cargo.toml`); refresh `Cargo.lock` via a build
- Create: `docs/release-notes/v0.6.0.md`

- [ ] **Step 1: Bump versions, build, run `cargo test --workspace`.**

- [ ] **Step 2: Release notes** `docs/release-notes/v0.6.0.md`, heading style copied from `docs/release-notes/v0.5.0.md`, title `## apb 0.6.0: hermes agent support`, one paragraph = one line, no AI-authorship markers, no em-dashes, no exclamation marks. Sections: what hermes support is (sixth known agent, detection + profiles + one-shot execution through `hermes -z`), how model selection works (explicit `-m` from the profile, provider auto-detected by hermes, suggestions from the curated table), the trust posture note (approvals auto-bypassed in one-shot mode, same posture as codex exec and opencode run), and Known limitations (no hermes model inventory, no ACP transport, no session resume mapping, no usage-file cost capture).

- [ ] **Step 3: Gates, commit** - `chore: bump workspace version to 0.6.0 for the hermes release`.

---

## Final verification (controller)

`cargo metadata --format-version 1 >/dev/null && code-ranker check .`; `cargo clippy --release --workspace --all-targets -- -D warnings`; `cargo test --workspace`; web suite untouched but run `bun run test` anyway; run the live hermes smoke once locally (`APB_LIVE_TEST_HERMES=1 APB_LIVE_HERMES_MODEL=glm-5.2 cargo test -p apb-cli live_hermes -- --ignored`); whole-branch review; PR; merge; tag v0.6.0.
