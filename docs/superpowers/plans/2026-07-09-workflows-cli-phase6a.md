# Workflows CLI, Phase 6a (patch versions, run migration, promote-on-success) - plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development or superpowers:executing-plans. Steps are checkboxes (`- [ ]`).

**Goal:** the engine machinery for supervisor-agent self-fixes. The supervisor agent fixes a run with a patch version: the engine creates a patch version (bumping the patch component, without moving `current`), validates the migration against the 10.3 rules, switches the in-progress run onto it (`run_migrated`, preserving the state of already-executed nodes) and continues from the given node; on a successful finish, an improvement patch gets promoted (`current` moves), otherwise it stays an unpromoted candidate. There's a `max_patches_per_run` limit. This is pure machinery in the engine/core, directly testable (without the MCP tool or a live agent - those are in 6b).

**Architecture:** Patch versions are an extension of the 5a version machinery: `create_patch_version` bumps the patch component (`X.Y.Z -> X.Y.(Z+1)`), does NOT move `current` (unlike a user's minor edits), and writes provenance. A version's provenance (who created it, from which run, classification, whether it's promoted) is a mutable sidecar OUTSIDE the immutable version folder (`.wf/workflows/<id>/meta/<version>.yaml`), since the promoted flag changes. The Phase 4 event-sourcing invariant is preserved: all patch events (`PatchApplied`/`PatchRejected`/`RunMigrated`/`VersionPromoted`) are written ONLY by the drive loop; the supervisor side (the tool in 6b) creates the patch version (version machinery, not events) and sends the `Control::Patch` control command. Migration on the fly: drive holds the workflow as an OWNED value (not a borrowed reference), so it can be swapped out on `run_migrated`; node matching is by id (state folding is already keyed by id, so completed nodes stay completed). Promotion happens on a successful run finish according to the `promote_supervisor_patches` policy (default `on_success`).

**Tech Stack:** Rust (edition 2024), serde/serde_json/serde_yaml_ng; wf-core (versioning/schema/validate), wf-engine (event/state/control/scheduler). No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` - 9.4 (the workflow_patch tool - implemented in 6b), 9.5 (max_patches_per_run, patch boundaries), 10.1 (patch = a supervisor-agent edit), 10.3 (migration: 3 rules), 10.5 (promote-on-success, improvement/workaround, policy). Builds on Phase 4 (supervisor/drive/control) and Phase 5a (version machinery).

## Global Constraints

- Comments/docs are in Russian; error texts and code are in English. NO em dashes (U+2014) and no exclamation marks in code/docs.
- The PROJECT version stays at `0.1.0` (not to be confused with workflow versions).
- TDD: a failing test first, then the implementation. Commit at the end of each task. Do not add new dependencies.
- Immutability invariant: version folders are not rewritten - a patch creates a NEW version. Provenance lives in a mutable sidecar outside the version folder.
- Event-sourcing invariant: `events.jsonl` is written only by the drive loop (including patch/migrate/promote events). The supervisor side writes `control.jsonl` and version folders, but NOT `events.jsonl`.
- `id`/`version`/`continue_from` coming from the client go through `is_safe_segment`.
- Existing behavior is unchanged: autonomous and supervised runs without patches, the 5a minor versions, all tests stay green.
- code-ranker before marking a task done; navigation via codegraph.

## Current setup (mirror it)

- `crates/wf-core/src/versioning.rs`: `create_version(root, id, new_yaml, base_version, make_current) -> String` (minor bump, moves current), `next_minor_version(base, existing)`, `VersioningError {NotFound, Validation(Vec<String>), Schema, Conflict, Io}`, `is_safe_segment`, private `workflows_dir/read_current/list_version_dirs/commit_version_dir` (atomic rename). Structure: `.wf/workflows/<id>/{current, layouts/<v>.yaml, <v>/workflow.yaml}`.
- `crates/wf-core/src/schema.rs`: `Workflow {schema,id,name,version,params,executors,defaults,supervisor:Option<Supervisor>,nodes,edges}`, `Node {id,title,kind}`, `Supervisor {executor, policy: Option<serde_yaml_ng::Value>}`.
- `crates/wf-engine/src/event.rs`: `EventPayload` (RunStarted..SupervisorLost, `#[serde(tag="type", rename_all="snake_case")]`), `EventLog`, `read_all`, `now_millis`.
- `crates/wf-engine/src/state.rs`: `RunState::fold` (nodes: BTreeMap<id, NodeStatus>), `NodeStatus`, `RunStatus`.
- `crates/wf-engine/src/control.rs`: `Control {Retry, ContinueFrom, Pause, Abort, ContextAppend}` (`#[serde(tag="cmd")]`), `post_control`, `read_control_after`, `post_supervisor_command` (scheduler).
- `crates/wf-engine/src/scheduler.rs`: `drive(...)` (the Phase 4 supervisor loop: top-of-loop honors Pause/Abort/ContextAppend, `await_control` on wake applies Retry/ContinueFrom/Pause/Abort/ContextAppend). IMPORTANT: `drive` currently takes `wf: &Workflow` (borrowed) - migration will need an owned copy.

---

### Task 1: Version provenance (sidecar)

**Files:** Modify `crates/wf-core/src/versioning.rs`, `lib.rs`; Test: `crates/wf-core/tests/provenance_test.rs`.

**Interfaces:**
- `#[derive(Serialize, Deserialize, Clone, PartialEq)] pub struct VersionProvenance { pub created_by: String /*"user"|"supervisor"*/, pub run_id: Option<String>, pub classification: Option<String> /*"improvement"|"workaround"*/, pub promoted: bool }`.
- `pub fn write_provenance(root, id, version, prov: &VersionProvenance) -> Result<(), VersioningError>` - `atomic_write(.wf/workflows/<id>/meta/<version>.yaml)`; a mutable sidecar (overwriting is allowed).
- `pub fn read_provenance(root, id, version) -> Result<Option<VersionProvenance>, VersioningError>` - None if the sidecar doesn't exist.
- `pub fn set_promoted(root, id, version, promoted: bool) -> Result<(), VersioningError>` - reads, sets the flag, writes it back (if the sidecar doesn't exist - create one with created_by="supervisor"? no: if it doesn't exist, return NotFound; only a known version can be promoted).
- All gated by `is_safe_segment` on id/version.
- Extend `create_version` (5a, a user's minor version): after success, write provenance `{created_by:"user", run_id:None, classification:None, promoted:true}` (a user's minor version immediately becomes current). Do NOT break the 5a signature/tests.

- [ ] Step 1: test - write/read round-trip; reading a missing one -> None; set_promoted changes the flag; traversal id/version -> NotFound; after `create_version` there's provenance created_by=user, promoted=true.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): version provenance sidecar (created_by, run_id, classification, promoted)`.

---

### Task 2: Creating a patch version

**Files:** Modify `crates/wf-core/src/versioning.rs`, `lib.rs`; Test: extend the versioning tests.

**Interfaces:**
- `pub fn next_patch_version(base: &str, existing: &[String]) -> String` - `X.Y.Z -> X.Y.(Z+1)`; on collision, increment the patch component; an invalid base -> a `Conflict`-safe default (e.g. returning `base` unchanged is not acceptable - take `X.Y.(Z+1)` of the parsed value, otherwise error out at the create step).
- `pub fn create_patch_version(root, id, base_version: &str, new_yaml: &str, run_id: &str, classification: &str) -> Result<String, VersioningError>`:
  - `is_safe_segment` on id/base_version; classification in {"improvement","workaround"}, otherwise `Validation`.
  - Compute the patch number from `base_version` among the existing ones.
  - Parse new_yaml, OVERWRITE version with the new number and id; validate (the V01-V15 gate) -> `Validation(codes)` on errors.
  - Assemble in a temp folder (workflow.yaml + a copy of scripts/ from the base), atomic rename into `<id>/<N>` (reuse the private `commit_version_dir`).
  - Copy the layout from the base (`copy_parent_layout`).
  - Do NOT move `current`.
  - Write provenance `{created_by:"supervisor", run_id:Some, classification:Some, promoted:false}`.
  - Return the new number.
- Differences from `create_version`: the patch component instead of minor, `current` is NOT moved, provenance is supervisor. Factor out the shared logic (assembling the folder/copying) into reusable private functions, if not already done.

- [ ] Step 1: test - `next_patch_version("1.2.0", &["1.2.0"])` -> "1.2.1"; collision -> skip past; `create_patch_version` from an existing version with a valid edit -> a new patch number, the version folder is created, `current` did NOT change, provenance supervisor/run_id/classification/promoted=false; invalid classification -> Validation; invalid YAML -> Validation; traversal -> NotFound.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): create supervisor patch version (patch bump, no current move, provenance)`.

---

### Task 3: Migration validator (10.3 rules)

**Files:** Modify `crates/wf-core/src/versioning.rs` (or a new `migration.rs`), `lib.rs`; Test: `crates/wf-core/tests/migration_test.rs`.

**Interfaces:**
- `#[derive(Debug, thiserror::Error)] pub enum MigrationError { ExecutedNodeChanged(String), ExecutedNodeRemoved(String), ContinueFromMissing(String) }`.
- `pub fn validate_migration(base: &Workflow, patched: &Workflow, executed_node_ids: &[String], continue_from: &str) -> Result<(), MigrationError>`:
  - Rule 2: every id from `executed_node_ids` must be present in `patched.nodes` (otherwise `ExecutedNodeRemoved`).
  - Rule 1: for every executed id whose node definition CHANGED between base and patched (comparing the serialized nodes), that is allowed ONLY if id == `continue_from`; otherwise `ExecutedNodeChanged` (an already-executed node cannot be changed, except at the continuation point).
  - `continue_from` must exist in `patched.nodes` (otherwise `ContinueFromMissing`).
  - Rule 3 (id is the key): a rename = a removal+addition; for an executed node, a rename is caught by rule 2. No explicit "the id list matches" check is needed - new/removed UNexecuted nodes are allowed.
  - Node comparison: `serde_json::to_value(node_base) != serde_json::to_value(node_patched)` (by id).

- [ ] Step 1: test scenarios: (valid) editing an unexecuted node; editing a node == continue_from while it's executed; adding a new node; (invalid) editing an executed node != continue_from -> ExecutedNodeChanged; removing an executed node -> ExecutedNodeRemoved; continue_from missing from patched -> ContinueFromMissing.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): migration validator (10.3 three rules)`.

---

### Task 4: Promote-on-success logic

**Files:** Modify `crates/wf-core/src/versioning.rs`, `lib.rs`; Test: extend.

**Interfaces:**
- `pub fn promote_version(root, id, version) -> Result<(), VersioningError>` - `atomic_write(current, version)` + `set_promoted(root,id,version,true)`; the version must exist.
- Policy parser: `pub fn promote_policy(wf: &Workflow) -> PromotePolicy` from `wf.supervisor.policy.promote_supervisor_patches` (the string `on_success`|`manual`|`always` or a map `after_n_successes: N`); default `OnSuccess`. `enum PromotePolicy { OnSuccess, AfterNSuccesses(u32), Manual, Always }`.
- A pure decision function: `pub fn should_promote(policy: PromotePolicy, classification: &str, run_succeeded: bool, changed_nodes_succeeded: bool, prior_successes: u32) -> bool`:
  - `workaround` -> always false (never promoted even on success).
  - `Manual` -> false (promotion is manual only).
  - `Always` -> true (for improvement).
  - `OnSuccess` -> `run_succeeded && changed_nodes_succeeded`.
  - `AfterNSuccesses(n)` -> `run_succeeded && changed_nodes_succeeded && prior_successes + 1 >= n`.

- [ ] Step 1: test - `promote_version` moves current + sets promoted; `promote_policy` parses all variants + the default; `should_promote` truth table (workaround always false; on_success on success; always; after_n_successes; manual false).
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): promote-by-success policy and promote_version`.

---

### Task 5: Patch/migration events + Control::Patch + run_migrated in drive

**Files:** Modify `crates/wf-engine/src/{event.rs,state.rs,control.rs,scheduler.rs,lib.rs}`; Test: `crates/wf-engine/tests/migrate_test.rs`.

**Interfaces:**
- `EventPayload` +: `PatchApplied { version: String, classification: String, continue_from: String }`, `PatchRejected { reason: String }`, `RunMigrated { from_version: String, to_version: String, continue_from: String }`, `VersionPromoted { version: String }`. All of these are log-only - a no-op on node statuses in `RunState::fold` (but see below about the version change).
- `Control` +: `Patch { version: String, classification: String, continue_from: String }` (tag `patch`). The supervisor (6b) first creates the patch version via `create_patch_version`, then posts `Control::Patch`.
- `RunConfig` +: `#[serde(default)] pub max_patches_per_run: Option<u32>` (default when None = 5).
- **drive refactor:** make `wf` owned (`let mut wf: Workflow = ...`) instead of `&Workflow`, so it can be swapped out on migration. Update `run`/`resume`/`run_background`/`drive_prepared` accordingly (clone when passing it). Verify that the autonomous/supervised paths behave unchanged.
- **Handling `Control::Patch`** (in `await_control` on wake and/or top-of-loop):
  1. Patch counter = the number of `PatchApplied` + `PatchRejected` in events; if `>= max_patches_per_run` -> `RunPaused { reason: "max patches per run exhausted: <diagnosis>" }`, return Paused (stop with a diagnosis, 9.5).
  2. Load the patched workflow from `.wf/workflows/<id>/<version>/workflow.yaml` (id from RunStarted). `is_safe_segment(version)`.
  3. `executed = RunState.fold(...).nodes` with a terminal status (succeeded/failed/timed_out/... - not pending/running).
  4. `validate_migration(&current_wf, &patched_wf, &executed_ids, &continue_from)`. On error -> `PatchRejected { reason }`, increment the counter (the event already increments it), keep waiting (do not migrate).
  5. Success: `snapshot_workflow(run_dir, patched_yaml)` (overwrite the run's snapshot with the patched one), `wf = patched_wf`, log `PatchApplied{version,classification,continue_from}` + `RunMigrated{from,to,continue_from}`, `current = continue_from`, continue drive on the new `wf`. Completed nodes' state is preserved (fold by id).
- **Promote on finish:** on reaching a successful finish, if a `PatchApplied` occurred during the run (take the last one), compute `changed_nodes_succeeded` (the continue_from node and the changed nodes in succeeded status), `prior_successes` (in 6a this can be 0 - defer the per-version success history), read the policy and classification (from the version's provenance or from the PatchApplied event), and if `should_promote(...)` -> `promote_version(root, id, last_patch_version)` + log `VersionPromoted{version}`. Otherwise the version stays an unpromoted candidate (provenance promoted=false - already the case).

- [ ] Step 1: test `migrate_test.rs` (without a real agent; a workflow with prompt nodes, supervised mode, threads+polling+timeout as in Phase 4): (A) a supervised run, on wake we post `Control::Patch` for a valid patch version created ahead of time (via `create_patch_version`) with a continue_from -> `PatchApplied`+`RunMigrated` in events, the run continues on the patched workflow and reaches success; improvement + policy on_success -> `current` is moved to the patch version (promoted), `VersionPromoted` in events, provenance promoted=true. (B) an invalid patch (editing an executed node != continue_from) -> `PatchRejected`, current does NOT move, the run doesn't migrate. (C) a workaround patch on success -> NOT promoted (current unchanged, provenance promoted=false). (D) the limit: max_patches_per_run=1, a second Patch -> RunPaused with a diagnosis. (E) regression: autonomous/supervised runs without a Patch are unchanged.
- [ ] Step 2-3: failing test -> implementation (be careful with the drive refactor to an owned wf).
- [ ] Step 4: `cargo test --workspace` (ALL green, including the Phase 4/5 tests); commit `feat(engine): run_migrated on supervisor patch, patch events, promote-by-success, max_patches`.

---

### Task 6: code-ranker, status, changelog

**Files:** Modify `docs/tasks.md`, `CHANGELOG.md`.

- [ ] Step 1: `cargo metadata --format-version 1 >/dev/null && code-ranker check .`; worst-first fix for any violations.
- [ ] Step 2: tasks.md "Phase 6" - mark `[x]` the implemented engine core (patch versions, migration, promote, limit); the `workflow_patch` tool and version history in the web -> `[ ]`/`[~]` (6b). CHANGELOG `### Added` - a line about the patch-version/migration/promote machinery.
- [ ] Step 3: commit `docs: mark phase 6a (patch-version machinery + run migration + promote) done`.

---

## Phase 6b (outline, a separate plan later) - the workflow_patch tool + version history in the web

On top of the 6a machinery.
- **workflow_patch (a supervisor MCP tool, part of the Phase 4b set):** arguments `{ patched_yaml (or edits), classification: improvement|workaround, continue_from }`. Logic: `create_patch_version(root, id, base=run_version, patched_yaml, run_id, classification)` -> `post_supervisor_command(Control::Patch{version, classification, continue_from})`. Gated by a new `patch_workflow` capability (add it to the Phase 4b capability model: currently observe/retry; add patch_workflow). Returns `{version}`. Validation that fails before the version is created (e.g. broken YAML) is returned as a tool error; the migration validation is done by drive (the PatchRejected event).
- **Version history in the web:** on the workflow/editor page, show the list of versions with provenance (created_by user|supervisor, run_id, classification improvement|workaround, promoted|candidate). HTTP endpoint `GET /api/workflows/{id}/versions` (list + provenance), backed by `read_provenance`. A "created by the supervisor agent in run X, unconfirmed" label for unpromoted patch versions. Support for manual promotion (the `manual` policy) - a button -> the endpoint `POST /api/workflows/{id}/promote?version=` -> `promote_version`.

## What is deliberately NOT part of Phase 6 (for the reviewer)

- Major versions (an explicit user action) - later.
- `after_n_successes` with a real per-version success count from the run history (6a: the base case, prior_successes=0 or from a provenance counter) - can be strengthened later.
- Manually rolling `current` back to an arbitrary version from the web (other than a manual promotion) - later/via the editor.
- Export/import, `node_slow`/`run_stuck` - other phases.
