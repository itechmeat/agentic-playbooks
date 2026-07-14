# Agent Profiles Completion Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **MANDATORY, NO EXCEPTIONS.** Every task and every step in this document is
> required and must be completed in a single pass. Nothing here may be marked
> "deferred", "partial", "follow-up", "out of scope", or "keep minimal". Do not
> skip an item because it is large, because a test is hard to write, or because
> a review called something advisory. A task is done only when its code, its
> tests, and its docs are all present and the full workspace is green. If a task
> genuinely cannot be completed, STOP and escalate to the human rather than
> marking it done or silently narrowing it. Marking a task complete without its
> full specified deliverable is a plan failure.

**Goal:** Finish the agent-profiles feature to the point where removing the
executor path is actually safe to ship: close every gap left after the initial
implementation and the review rounds, so profiles are a complete, durable, and
honestly-documented replacement for executors across engine, MCP, CLI, and web.

**Context:** The profile core (types, digests, resolver, scopes, bundle trust,
run snapshot + manifest, declarative invocations, schema-2 refs, migrator,
`profile_*` MCP tools, detect, models table, advisory tools) is implemented and
committed at `e3d2aba`. The executor type is removed. What remains are the
cutover safety nets and refinements that were shipped incomplete: legacy
run-resume, isolated skill materialization, ephemeral executor override, CLI
profile write/edit, web profile API, a real stdio-MCP e2e, migrator global-scope
correctness, detect/models refinements, doctor/adopt diagnostics, onboarding
triggers, and the full supervisor profile contract.

**Architecture:** Rust workspace (edition 2024): wf-core, wf-engine, wf-mcp,
wf-cli, wf-server. Profiles live in `.wf/profiles/<name>/` (project) and
`<config_dir>/profiles/<name>/` (global). Runs snapshot into `runs/<id>/` with
an immutable `manifest.yaml`. Trust is per bundle (`profile.yaml` + `SOUL.md` +
skill digests).

**Tech Stack:** serde / serde_yaml_ng, sha2, tempfile already present. `libc`
is already a wf-engine dependency and may be added to wf-core if a task needs
process-group control. No other new external dependencies without explicit note.

## Global Constraints

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings.
- No CJK anywhere in code or prose. Machine fields English; user-facing chat
  messages in the user's chat language.
- New `EventPayload` fields are added only with `#[serde(default)]`.
- State files are written atomically (temp + rename), 0600 on unix, via
  `wf_core::fsutil`.
- Secret values (auth files) are never returned, logged, or cached.
- Skill content is never embedded into a prompt at any delivery level.
- Profile names: `[a-z0-9][a-z0-9-]*`, at most 64 chars, name == directory name,
  no case-fold collisions. Path segments validated with `is_safe_segment`.
- Every task ends green: `cargo test --workspace` passes, and
  `cargo clippy --workspace --all-targets -- -D warnings` and
  `cargo fmt --all -- --check` both pass (see Task 1).
- Before code is ready to commit: warm the cargo cache
  (`cargo metadata --format-version 1 >/dev/null`), then run `code-ranker check .`
  and fix every violation (read `code-ranker docs base <ID>` first) until clean.
- Before code is considered ready for release: run `cargo clippy --release` and
  fix every error in the logs; clean up warnings as far as practical.
- Commits only after the owner approves; the Commit step prepares (`git add`),
  the owner runs the actual approval.

---

### Task 1: Strict-green baseline (fmt + clippy -D warnings)

Make the whole workspace pass the strict gates first, so every later task can
rely on them and regressions surface immediately.

**Files:**
- Modify: any file flagged by `cargo fmt` or `cargo clippy --all-targets -- -D warnings`
  (notably `crates/wf-cli/src/main.rs` `&PathBuf` -> `&Path` ptr_arg,
  `crates/wf-core/tests/validate_profiles_test.rs`, and engine warnings).

- [ ] **Step 1:** Run `cargo fmt --all` and commit the formatting-only result.
- [ ] **Step 2:** Run `cargo clippy --workspace --all-targets -- -D warnings`;
  fix every warning (convert `&PathBuf` params to `&Path`, address test-crate
  warnings, engine warnings). Do not `#[allow]` to silence unless there is a
  documented reason.
- [ ] **Step 3:** Verify `cargo fmt --all -- --check` and
  `cargo clippy --workspace --all-targets -- -D warnings` both exit 0.
- [ ] **Step 4:** `cargo test --workspace` passes.
- [ ] **Step 5:** Commit `chore: strict fmt and clippy-deny-warnings baseline`.

---

### Task 2: Legacy run-resume shim (pre-profile runs resumable)

Runs created before profiles have a `runs/<id>/workflow.yaml` with `executors`
and no `manifest.yaml`. Resuming them must build an ephemeral manifest from the
snapshot executors instead of failing.

**Files:**
- Create: `crates/wf-engine/src/legacy_snapshot.rs` (self-contained legacy
  parser for run snapshots only; not the public schema).
- Modify: `crates/wf-engine/src/scheduler.rs` (resume path), `crates/wf-engine/src/manifest.rs`.
- Test: `crates/wf-engine/tests/profile_run_test.rs` (add
  `legacy_run_resume_via_ephemeral_snapshot`).

**Interfaces:**
- Produces: `legacy_snapshot::build_ephemeral_manifest(run_dir, snapshot_yaml) ->
  Result<RunExecutionManifest, EngineError>` that reads snapshot `executors` /
  `defaults.executor` / node `executor` / `supervisor.executor`, maps each to a
  `ManifestProfile` named `legacy-<key>` with empty SOUL, no skills, and a
  `chain` built via `invocation::resolve_invocation`.
- Consumes: `invocation::resolve_invocation`, `manifest::write`.

- [ ] **Step 1:** Write `legacy_run_resume_via_ephemeral_snapshot`: create a run
  dir whose `workflow.yaml` uses `executors` + `defaults.executor`, no manifest;
  call `resume_with`; assert the run drives to completion on the stub agent and
  a `manifest.yaml` now exists with a `legacy-*` profile bound to the node.
- [ ] **Step 2:** Run it; confirm it fails (resume rejects executors today).
- [ ] **Step 3:** Implement: on resume, if `manifest::read` returns `None` and
  the snapshot contains legacy executors, build and write the ephemeral manifest
  before `execute_node`. The snapshot parse uses a local legacy serde type (do
  not relax `Workflow::from_yaml`). `execute_node` keeps reading the node
  executor strictly from the manifest.
- [ ] **Step 4:** `cargo test -p wf-engine` passes.
- [ ] **Step 5:** Commit `feat(engine): ephemeral manifest shim for resuming pre-profile runs`.

---

### Task 3: Isolated skill materialization and skills_mode event

Deliver the accepted contract: for an isolated node the agent reads only the
snapshot skill copies; the actual delivery mode is recorded per attempt.

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs` (`execute_node` skill delivery,
  `workdir` setup), `crates/wf-engine/src/event.rs` (`AttemptStarted.skills_mode`).
- Test: `crates/wf-engine/tests/profile_run_test.rs`
  (`isolated_workdir_materializes_skill_copies`, plus assert the advisory case
  records `skills_mode: advisory`).

**Interfaces:**
- Produces: for `isolation: full | best_effort`, materialize each manifest skill
  from the run snapshot into an isolated per-node workdir at
  `<workdir>/.agents/skills/<name>/` (real copies, not symlinks) plus the
  `.claude/skills` bridge, and point the agent at that workdir. `AttemptStarted`
  gains `#[serde(default)] skills_mode: Option<String>` set to
  `materialized` or `advisory`.
- For `isolation: none`, keep advisory delivery and record `skills_mode: advisory`.

- [ ] **Step 1:** Write `isolated_workdir_materializes_skill_copies`: profile
  with a skill, node `isolation: full`; run on a stub that dumps its workdir
  listing; assert `<workdir>/.agents/skills/<name>/SKILL.md` exists and is a
  regular file (not a symlink), and the attempt event carries
  `skills_mode: materialized`.
- [ ] **Step 2:** Add an advisory-mode assertion: `isolation: none` yields
  `skills_mode: advisory` and no materialized copy. Run both; confirm they fail.
- [ ] **Step 3:** Implement materialization from the run snapshot (never from the
  live `.agents/skills`) into the isolated workdir; keep advisory for `none`.
  Update PROFILES.md to state the immutability guarantee now holds for isolated
  runs and remains advisory-only for `none`.
- [ ] **Step 4:** `cargo test -p wf-engine` passes.
- [ ] **Step 5:** Commit `feat(engine): materialize snapshot skills for isolated nodes, record skills_mode`.

---

### Task 4: overrides v2 ephemeral executor

Restore the accepted run-local ephemeral executor override: same role and skills
as the node profile, a single-invocation chain, recorded in the manifest.

**Files:**
- Modify: `crates/wf-core/src/overrides.rs` (`NodeOverride.ephemeral_executor`),
  `crates/wf-engine/src/scheduler.rs` (`build_run_manifest` honors it),
  `crates/wf-engine/src/manifest.rs` (`ManifestProfile.ephemeral: bool` with
  `#[serde(default)]`).
- Test: `crates/wf-core/tests/overrides_test.rs`
  (`ephemeral_executor_parses`), `crates/wf-engine/tests/overrides_run_test.rs`
  (`ephemeral_executor_recorded_in_manifest`).

**Interfaces:**
- Produces: `NodeOverride { profile: Option<QualifiedProfileRef>,
  ephemeral_executor: Option<EphemeralExecutor { agent, model }> }`. When set,
  `build_run_manifest` resolves the node profile for SOUL and skills but replaces
  the chain with a single invocation for the ephemeral agent/model, and the
  manifest entry is keyed per node and flagged `ephemeral: true`.

- [ ] **Step 1:** Write both tests (parse; and a run whose manifest shows the
  node bound to an ephemeral single-invocation chain flagged ephemeral, with the
  profile SOUL/skills preserved). Confirm they fail.
- [ ] **Step 2:** Implement the field, the manifest keying (ephemeral entries are
  per-node, not deduped by `<scope>/<name>`), and the chain replacement.
- [ ] **Step 3:** `cargo test -p wf-core -p wf-engine` passes.
- [ ] **Step 4:** Commit `feat: run-local ephemeral executor override recorded in the manifest`.

---

### Task 5: Migrator global executors become global-scope profiles

Materialize global executors as global-scope profiles with qualified refs,
rather than project profiles, so a migrated reference to a global executor
resolves globally and other projects are unaffected.

**Files:**
- Modify: `crates/wf-core/src/schema_migrate.rs` (scope-aware `Reg`, `apply`
  writes global profiles to `<config_dir>/profiles`, refs rewritten to
  `{ name, scope: global }`).
- Test: `crates/wf-core/tests/schema_migrate_config_test.rs` (extend), new
  `global_executor_becomes_global_profile_and_ref_is_qualified`.

**Interfaces:**
- Produces: `Reg` tracks a scope per registered profile; a profile materialized
  from a global executor gets `scope: "global"` and is written to the global
  profiles dir in `apply`; `rewrite_doc` emits `{ name, scope: global }` for such
  refs. Project-origin inline and named-local executors stay project scope.

- [ ] **Step 1:** Write the test: a workflow whose `defaults.executor` names a
  global executor migrates to `defaults.profile: { name, scope: global }`, the
  profile is created under `<config_dir>/profiles/<name>`, and the migrated
  workflow loads and resolves.
- [ ] **Step 2:** Confirm it fails (currently materialized as project profile).
- [ ] **Step 3:** Implement scope-aware registration and global-dir writes;
  keep the global `config.yaml` untouched (no strip); keep path/name validation.
- [ ] **Step 4:** `cargo test -p wf-core` passes.
- [ ] **Step 5:** Commit `feat(core): migrate global executors to global-scope profiles with qualified refs`.

---

### Task 6: Detect refinements (custom agents, source cache, interpreter PATH, auth kinds)

**Files:**
- Modify: `crates/wf-core/src/detect.rs`, `crates/wf-core/src/config.rs`
  (`AgentDef.probe: Option<bool>` and bins).
- Test: `crates/wf-core/tests/detect_test.rs` (extend).

**Interfaces / required behaviors:**
- Custom agents: `AgentDef` gains an opt-in `probe` and optional `bins`; `detect`
  scans presence for configured agents in addition to the builtin five.
- Sanitized PATH keeps the interpreter reachable: build the child PATH from
  trusted system dirs plus the canonical parent of the found binary (so a
  `#!/usr/bin/env node` CLI still runs), still filtering project-local entries.
- Cache key includes fingerprints of the consulted config/auth sources
  (`~/.codex/config.toml`, `~/.claude.json`, opencode auth.json), not just the
  binary; a change to any invalidates before TTL.
- Codex auth is classified account/oauth vs api-key (not always api-key).
- stdout truncation adds a note; stderr is captured for the note on failure.

- [ ] **Step 1:** Tests: configured custom agent gets a presence result; a
  `#!/usr/bin/env fake-runtime` stub with the runtime beside the agent probes
  successfully; editing a config source invalidates the cache before TTL;
  truncation adds a note. Confirm they fail.
- [ ] **Step 2:** Implement. Keep probes free (no model calls, no network) and
  non-hanging (bounded reads already in place).
- [ ] **Step 3:** `cargo test -p wf-core` passes.
- [ ] **Step 4:** Commit `feat(core): detect custom agents, source-aware cache, interpreter-safe PATH, auth kinds`.

---

### Task 7: Models table accuracy, size, and partial overlay merge

**Files:**
- Modify: `assets/models.yaml`, `crates/wf-core/src/models_table.rs`.
- Test: `crates/wf-core/tests/models_table_test.rs` (extend).

**Interfaces / required behaviors:**
- Expand to 20-30 models. Each row gains `source_url`, `checked_at`, and
  `price_basis` (for example `list` or `launch-until-YYYY-MM-DD`). Verify prices
  and current model identifiers against primary sources using WebFetch during
  implementation; correct the values the last review flagged.
- Overlay is a partial `ModelPatch` with field-wise merge: overriding one price
  field must not reset the other fields to defaults.
- An unreadable or invalid overlay, and a corrupt onboarding state file, surface
  a structured error and are never silently replaced. `write_subscriptions`
  distinguishes `NotFound` from other IO errors.
- CI test: every `purposes[*].model` exists in `models`; every row has
  `source_url` and `checked_at`.

- [ ] **Step 1:** Tests: field-wise overlay merge preserves untouched fields;
  corrupt overlay yields an error not builtin; integrity (all purpose models
  exist, all rows carry source/checked_at). Confirm they fail.
- [ ] **Step 2:** Verify and correct `assets/models.yaml` against primary
  sources (WebFetch); expand to 20-30 with source fields.
- [ ] **Step 3:** Implement `ModelPatch` merge and error surfacing.
- [ ] **Step 4:** `cargo test -p wf-core` passes.
- [ ] **Step 5:** Commit `feat(core): verified models table with sources, partial overlay merge, error surfacing`.

---

### Task 8: Doctor and adopt diagnostics

**Files:**
- Modify: `crates/wf-mcp/src/advisory_tools.rs` (`workflow_adopt_report`),
  `crates/wf-core/src/doctor.rs`.
- Test: `crates/wf-mcp/tests/advisory_tools_test.rs`,
  `crates/wf-core/tests/doctor_test.rs`.

**Interfaces / required behaviors:**
- `workflow_adopt_report(id)` for an explicit id returns a not-found / load-error
  finding instead of an empty `{workflows: []}`; all-mode emits a per-workflow
  diagnostic for each unloadable workflow.
- Doctor resolves the qualified profile refs of each loaded workflow (including
  global-scope refs), not only the flat project profile list, and normalizes
  agent ids the same way the invocation resolver does (so `claude-code` maps to
  the `claude` detect probe rather than being checked as a `claude-code` binary).
  Doctor prints the detection authority for each agent's models.

- [ ] **Step 1:** Tests: explicit missing id -> error finding; a workflow that
  fails to load -> diagnostic in all-mode; `claude-code` in a profile does not
  produce a false "not found" warning; a global-scope ref is resolved. Confirm
  they fail.
- [ ] **Step 2:** Implement.
- [ ] **Step 3:** `cargo test -p wf-core -p wf-mcp` passes.
- [ ] **Step 4:** Commit `feat: adopt/doctor per-workflow diagnostics, qualified-ref resolution, agent-id normalization`.

---

### Task 9: Supervisor full profile contract

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs` (`spawn_supervisor_agent`),
  `crates/wf-engine/src/adapter.rs`, `crates/wf-engine/src/manifest.rs` if the
  supervisor binding needs SOUL/skills recorded.
- Test: `crates/wf-engine/tests/background_supervisor_test.rs`.

**Interfaces / required behaviors:**
- The supervisor uses its profile fully: SOUL is delivered per the invocation
  `soul_delivery`, relevant/materialized skills are provided, and the fallback
  chain is tried on spawn failure. The `supervisor` manifest binding is created
  from `supervisor.profile` OR `defaults.profile` even when there is no
  `supervisor` section, so `--supervise` with only `defaults.profile` still
  spawns the agent. An event or diagnostic records the chain element actually
  used.

- [ ] **Step 1:** Tests: `--supervise` with only `defaults.profile` spawns a
  supervisor; SOUL is delivered; a primary spawn failure falls back to the next
  chain element and the chosen element is recorded. Confirm they fail.
- [ ] **Step 2:** Implement (build_run_manifest supervisor binding from
  defaults.profile; spawn honors SOUL and skills; ordered fallback on spawn
  error).
- [ ] **Step 3:** `cargo test -p wf-engine` passes.
- [ ] **Step 4:** Commit `feat(engine): full supervisor profile contract (soul, skills, fallbacks, defaults)`.

---

### Task 10: Onboarding triggers and honest wording

**Files:**
- Modify: `crates/wf-cli/src/main.rs` (survey prefill, triggers on `wf profile`),
  `crates/wf-mcp/src/server.rs` and `docs/MCP.md`, `docs/PROFILES.md` (network
  wording).
- Test: `crates/wf-cli/tests/advisory_cli_test.rs`.

**Interfaces / required behaviors:**
- The onboarding offer is gated on `stdin().is_terminal()` (interactive session),
  and fires on `wf profile *` as well as `wf detect` / `wf adopt` when the state
  is Uninitialized. The interactive survey prefills from detection auth hints.
- Wording in CLI, MCP tool descriptions, and docs states honestly: wf itself
  makes no network requests; it does not control whether a spawned third-party
  CLI does. Remove any blanket "no network" claim that implies the launched
  agent is offline.

- [ ] **Step 1:** Tests (non-TTY): `wf profile list` does not prompt and does not
  change state; declined state is never re-offered; `--set`/`--decline` paths
  unchanged. Confirm the trigger-coverage assertions fail.
- [ ] **Step 2:** Implement stdin gating, `wf profile` trigger, prefill, wording.
- [ ] **Step 3:** `cargo test -p wf-cli` passes; grep docs and user strings for a
  bare "no network" claim and fix.
- [ ] **Step 4:** Commit `feat(cli): onboarding triggers on profile commands, prefill, honest network wording`.

---

### Task 11: CLI profile write and edit

**Files:**
- Modify: `crates/wf-cli/src/main.rs` (`wf profile write`, `wf profile edit`).
- Test: `crates/wf-cli/tests/profile_cli_test.rs` (extend).

**Interfaces / required behaviors:**
- `wf profile write --scope <s> --agent <a> --model <m> [--fallback a:m ...]
  [--skill name ...] [--soul <file>] [--description ...]` creates or updates a
  profile through the same `wf_core`/`profile_tools` logic the MCP tool uses,
  including bundle auto-approve.
- `wf profile edit <name> [--scope]` opens `profile.yaml` (and SOUL.md) in
  `$EDITOR`, then writes with a CAS check against the digest read before editing;
  a digest mismatch (concurrent change) is a reported conflict, not a clobber.

- [ ] **Step 1:** Tests: write creates a profile that `profile show` returns;
  write then write with a stale expected digest -> conflict; edit round-trip
  (simulate `$EDITOR` with a script that rewrites the file) succeeds and a
  concurrent change is a conflict. Confirm they fail.
- [ ] **Step 2:** Implement, reusing profile-write logic (no duplicate CAS).
- [ ] **Step 3:** `cargo test -p wf-cli` passes.
- [ ] **Step 4:** Commit `feat(cli): wf profile write and edit with CAS and $EDITOR`.

---

### Task 12: Web profile API and node profile selector

**Files:**
- Modify: `crates/wf-server/src/lib.rs` (`GET /api/profiles`, `POST /api/profiles`),
  frontend node form (profile select) in the server's served assets.
- Test: `crates/wf-server/tests/api_test.rs` (extend) or a new
  `profiles_api_test.rs`.

**Interfaces / required behaviors:**
- `GET /api/profiles` returns the project and global profiles with trust status
  (same shape as `profile_list`). `POST /api/profiles` creates or updates a
  profile through the shared logic, returning digests and trust result. The
  agent-node form uses a profile selector bound to these endpoints.

- [ ] **Step 1:** API tests: GET lists a seeded profile; POST creates one and a
  subsequent GET shows it as trusted. Confirm they fail.
- [ ] **Step 2:** Implement handlers (reuse `profile_tools`) and the node-form
  selector.
- [ ] **Step 3:** `cargo test -p wf-server` passes.
- [ ] **Step 4:** Commit `feat(web): profile list/create API and node profile selector`.

---

### Task 13: Real stdio-MCP end-to-end

**Files:**
- Test: `crates/wf-cli/tests/profile_e2e_test.rs` or
  `crates/wf-mcp/tests/stdio_profile_e2e_test.rs` (model on the existing stdio
  harness used by `mcp_supervise_test.rs` / `mcp_cli_test.rs`).

**Interfaces / required behaviors:**
- Drive the actual stdio MCP server: call `profile_write`, `workflow_create`
  with a profile ref, `workflow_approve`, then `workflow_run` on a stub agent and
  assert Succeeded. Then edit a skill file on disk and assert a second
  `workflow_run` without `acknowledge_untrusted` is refused with
  `untrusted_profile_requires_acknowledge`. The test must exercise the real
  tool schemas and the server policy-to-permit handoff, not in-process Rust calls.

- [ ] **Step 1:** Write the stdio e2e using the existing transport harness.
  Confirm it drives the full path.
- [ ] **Step 2:** Fix anything it surfaces at the server boundary.
- [ ] **Step 3:** `cargo test -p wf-cli` (or wf-mcp) passes.
- [ ] **Step 4:** Commit `test: stdio MCP end-to-end for profile write, run, and skill-edit refusal`.

---

### Task 14: Docs sync and final full-green verification

**Files:**
- Modify: `docs/PROFILES.md`, `docs/HOWTO-authoring.md`, `docs/MCP.md`.

- [ ] **Step 1:** Sync docs with the shipped surface: CLI now has
  `profile write|edit`; web has the profile API; skill immutability holds for
  isolated runs; the models table carries sources; the network wording is
  honest. Remove any claim not backed by shipped behavior.
- [ ] **Step 2:** Full verification: `cargo test --workspace` passes;
  `cargo clippy --workspace --all-targets -- -D warnings` exits 0;
  `cargo fmt --all -- --check` exits 0; grep docs and user-facing strings for
  em-dashes, exclamation marks, and CJK and confirm none.
- [ ] **Step 3:** Commit `docs: sync profile docs with the completed surface`.

---

## Order and dependencies

```
Task 1 (strict baseline)
  -> 2 legacy resume
  -> 3 skill materialization -> 9 supervisor (uses materialized skills)
  -> 4 ephemeral override
  -> 5 migrator global scope
  -> 6 detect -> 7 models -> 8 doctor/adopt
  -> 10 onboarding
  -> 11 CLI write/edit -> 12 web API -> 13 stdio e2e
  -> 14 docs + final green
```

Every task above is mandatory and must be finished in this pass. Do not stop
after a subset. When all fourteen are green, the executor removal is complete and
shippable.
