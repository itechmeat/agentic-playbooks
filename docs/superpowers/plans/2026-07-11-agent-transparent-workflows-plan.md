# Agent-Transparent Workflows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement agent-transparent workflows per the spec `docs/superpowers/specs/2026-07-11-agent-transparent-workflows-design.md` (rev 3) as a single plan: scopes, the workspace registry, the catalog, the run policy, capture, trial, and cross-project support.

**Architecture:** Instructions are the UX layer, the server holds the structural guarantees. We split definition origin from execution target in the core, add a trust model (digest) and effects on top, then MCP tools (`workflow_catalog`, `workflow_capture`, `workflow_prepare_run`/`execute_plan`) and a static tier 0 in the server instructions.

**Tech Stack:** Rust workspace (edition 2024): wf-core, wf-engine, wf-mcp (rmcp 2.2.0), wf-cli. New dependencies: `sha2` (digest, HMAC-signing the plan_token via `hmac`), `uuid` v4 (workspace id).

**By the product owner's decision**, decomposing the spec into 5 sub-specs (its section 13) is replaced by this single plan; the task order follows the order the spec is implemented in (the policy gate goes in before the first transparent run).

## Global Constraints

- No em dashes and no exclamation marks in documentation or user-visible strings.
- Machine-facing workflow fields (id, tags, `trigger.when`/`avoid_when`) are English; display fields are free-form; user-facing messages are in the chat language (a tier-0 rule already applies).
- `workflow_list` is NOT broken: the current summary format is kept; the compact catalog is only the new `workflow_catalog`.
- All new tools carry rmcp annotations: `annotations(read_only_hint=true)` or `annotations(destructive_hint=true)`, following the pattern of the existing 22 tools in `crates/wf-mcp/src/server.rs`.
- Tests are hermetic: global state only via `WF_CONFIG_DIR` (already supported in `wf_core::config::config_dir()`), temp directories via `tempfile`.
- Event backward compatibility: new `EventPayload` fields only with `#[serde(default)]` (old `events.jsonl` files must still parse).
- Global state files (`projects.json`, `trust.json`, `dismissed.json`): atomic write via temp+rename, 0600 permissions (unix), a `schema_version` field.
- Secret values are never written to any state or provenance file.

---

### Task 0: Compatibility spike - server instructions across target hosts

Tier 0 lives in `ServerInfo.instructions`; MCP gives no guarantee about how a host uses it. Before writing code, we need an actual matrix.

**Files:**
- Create: `docs/superpowers/research/2026-07-XX-host-instructions-matrix.md` (date = the day this is done)

**Interfaces:**
- Produces: a matrix of "host x (does it read instructions? where does it put them? does it survive compaction? how does it show tool approvals?)", used by Task 7 when choosing tier-0 wording.

- [ ] **Step 1: Prepare marker instructions**

On a temporary branch, replace the string at `crates/wf-mcp/src/server.rs:581` with text carrying a unique marker:

```rust
.with_instructions("WF-SPIKE-MARKER-7321: if you can read this, mention the marker when asked about wf capabilities")
```

`cargo build -p wf-cli`, hook up the binary as an MCP server to each host.

- [ ] **Step 2: Check each host**

For Claude Code, opencode, Hermes, Pi (whichever are installed locally; mark anything missing as `not tested`):

1. Ask the host "what wf capabilities do you have" - is the marker visible.
2. Bloat the context up to a summarization point (paste a large file, ask for a recap), ask again - did the marker survive compaction.
3. Call `workflow_delete` on a test workflow - does the host show a confirmation for a destructive tool.

Record in the table: host, version, instructions visible, survives compaction, tool approvals UX.

- [ ] **Step 3: Write up the findings and remove the marker**

In the research doc: a final recommendation (which hosts get tier 0 from instructions, which need a CLAUDE.md fallback). Do not merge the branch with the marker.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/research/
git commit -m "docs: host server-instructions compatibility matrix (spike)"
```

---

### Task 1: wf-core - Scope, WorkflowRef, digest

**Files:**
- Create: `crates/wf-core/src/scope.rs`
- Modify: `crates/wf-core/src/lib.rs` (add `pub mod scope;`)
- Modify: `crates/wf-core/Cargo.toml` (add `sha2 = "0.10"`)
- Test: unit tests inside `scope.rs`

**Interfaces:**
- Produces: `Scope`, `Origin`, `WorkflowRef`, `digest_str(yaml: &str) -> String` (format `sha256:<hex>`). Used by every subsequent task.

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_and_prefixed() {
        let d = digest_str("id: x\n");
        assert!(d.starts_with("sha256:"));
        assert_eq!(d, digest_str("id: x\n"));
        assert_ne!(d, digest_str("id: y\n"));
    }

    #[test]
    fn workflow_ref_roundtrips_json() {
        let r = WorkflowRef {
            origin: Origin::Project { workspace_id: Some("ws-1".into()) },
            id: "review".into(),
            version: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: WorkflowRef = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }
}
```

- [ ] **Step 2: Confirm the tests fail**

Run: `cargo test -p wf-core scope` - compile error (module doesn't exist).

- [ ] **Step 3: Implement the module**

```rust
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Storage scope of a definition (spec 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope { Project, Global }

/// Origin of a definition. Project without workspace_id means "the caller's
/// current workspace" (spec 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Global,
    Project {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
    },
}

/// Address of a workflow definition (spec 3). version=None means current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRef {
    pub origin: Origin,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Content fingerprint of a definition; the basis for the trust binding (spec 3.1).
pub fn digest_str(yaml: &str) -> String {
    let mut h = Sha256::new();
    h.update(yaml.as_bytes());
    format!("sha256:{:x}", h.finalize())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-core scope` - PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/wf-core
git commit -m "feat(core): Scope, Origin, WorkflowRef and content digest"
```

---

### Task 2: Splitting definition origin from execution target

Right now `Registry::open(root)` requires `<root>/.wf`, and `prepare_run` (crates/wf-engine/src/scheduler.rs:711) takes the definition, lock, runs, and scripts from the same root. We split these apart.

**Files:**
- Modify: `crates/wf-core/src/registry.rs`
- Create: `crates/wf-core/src/store.rs`
- Modify: `crates/wf-core/src/lib.rs`
- Modify: `crates/wf-engine/src/scheduler.rs`
- Modify: `crates/wf-engine/src/event.rs` (provenance in RunStarted)
- Test: `crates/wf-engine/tests/global_scope_run_test.rs`

**Interfaces:**
- Consumes: `WorkflowRef`, `digest_str` from Task 1.
- Produces:
  - `Registry::open_dir(workflows_parent: &Path) -> Result<Registry, RegistryError>` - opens a directory containing `workflows/`, without requiring `.wf`;
  - `store::global_dir() -> Option<PathBuf>` = `config_dir()`, `store::global_workflows_parent()` - the same path (the global store puts `workflows/` directly in the config dir);
  - `store::resolve(project_root: &Path, wref: &WorkflowRef) -> Result<ResolvedWorkflow, RegistryError>` where `ResolvedWorkflow { definition_parent: PathBuf, execution_root: PathBuf, id: String, version: String, digest: String }`;
  - engine: `prepare_run` internally takes `definition_parent` + `execution_root`; the public `run`, `run_background(root, id, ...)` keep their signatures unchanged (definition=execution=root);
  - a new public `run_background_resolved(resolved: &ResolvedWorkflow, opts: RunOptions) -> Result<String, EngineError>`;
  - `EventPayload::RunStarted` gains the fields `#[serde(default)] origin: Option<String>` (`"global"` / `"project"`), `#[serde(default)] digest: Option<String>`, `#[serde(default)] execution_root: Option<String>`.

- [ ] **Step 1: Registry - test and refactor the opening**

Test in `registry.rs`: `open_dir` opens a directory with `workflows/` without `.wf`; `open(root)` remains equivalent to `open_dir(root.join(".wf"))`.

```rust
#[test]
fn open_dir_works_without_dot_wf() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("workflows")).unwrap();
    assert!(Registry::open_dir(tmp.path()).is_ok());
    assert!(Registry::open(tmp.path()).is_err()); // no .wf
}
```

Implementation: `Registry { base: PathBuf }`, `workflows_dir() = base.join("workflows")`; `open(root)` checks `root/.wf` and calls `open_dir(root.join(".wf"))`. All existing calls to `Registry::open` stay unchanged.

- [ ] **Step 2: store.rs - the resolver**

```rust
use crate::registry::{Registry, RegistryError};
use crate::scope::{digest_str, Origin, WorkflowRef};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ResolvedWorkflow {
    pub definition_parent: PathBuf,
    pub execution_root: PathBuf,
    pub id: String,
    pub version: String,
    pub digest: String,
}

/// Directory of the global store: `<config_dir>` (workflows/ inside it).
pub fn global_workflows_parent() -> Option<PathBuf> {
    crate::config::config_dir()
}

/// Resolves a WorkflowRef into a definition plus a place to execute. For
/// Origin::Project { workspace_id: Some(_) } the caller supplies the
/// workspace path (wf-mcp via the registry, Task 5); here project means
/// project_root.
pub fn resolve(project_root: &Path, wref: &WorkflowRef) -> Result<ResolvedWorkflow, RegistryError> {
    let definition_parent = match &wref.origin {
        Origin::Global => global_workflows_parent()
            .ok_or_else(|| RegistryError::NotFound("global config dir".into()))?,
        Origin::Project { .. } => project_root.join(".wf"),
    };
    let reg = Registry::open_dir(&definition_parent)?;
    let loaded = reg.load(&wref.id, wref.version.as_deref())?;
    Ok(ResolvedWorkflow {
        definition_parent,
        execution_root: project_root.to_path_buf(),
        id: wref.id.clone(),
        version: loaded.version.clone(),
        digest: digest_str(&loaded.yaml),
    })
}
```

Test: a global workflow (seeded in a `WF_CONFIG_DIR` tempdir) resolves with `execution_root` = the project tempdir.

- [ ] **Step 3: Engine - separate the paths**

In `scheduler.rs`, factor out an internal struct and switch the internals of `prepare_run`/`prepare_supervised_background`:

```rust
struct EngineTarget { definition_parent: PathBuf, execution_root: PathBuf }
```

Split rule: the definition (Registry, profiles) comes from `definition_parent`; the workdir lock, `.wf/runs`, events, and copying `scripts/` come from `execution_root/.wf` and `execution_root/scripts`. The public `run`/`run_background(root, ...)` build `EngineTarget { definition_parent: root.join(".wf"), execution_root: root }` - behavior is unchanged. Add `run_background_resolved(resolved, opts)`.

In `event.rs`, extend `RunStarted` with the fields from Interfaces (only `#[serde(default)]`), filling them in during prepare.

- [ ] **Step 4: Integration test**

`crates/wf-engine/tests/global_scope_run_test.rs`: seed a minimal workflow (a start node) in the global store (a tempdir via `WF_CONFIG_DIR`), a project tempdir with an empty `.wf/`, `run_background_resolved`, wait for a terminal status; verify the run lives under `<project>/.wf/runs/`, and that `events.jsonl` contains `"origin":"global"` and a digest.

- [ ] **Step 5: Full run and commit**

Run: `cargo test --workspace` - PASS (existing tests must not break: public function signatures were not changed).

```bash
git add crates/wf-core crates/wf-engine
git commit -m "feat(engine): split definition origin from execution target"
```

---

### Task 3: Lifecycle and trust store

**Files:**
- Create: `crates/wf-core/src/trust.rs`
- Modify: `crates/wf-core/src/lib.rs`, `crates/wf-core/src/versioning.rs`
- Test: unit tests in `trust.rs`

**Interfaces:**
- Produces:
  - `Lifecycle { Draft, Active, Retired }`; `read_lifecycle(workflow_dir) -> Lifecycle` (the file `<...>/workflows/<id>/lifecycle`; absence = Active - backward compatible), `write_lifecycle(...)`;
  - `TrustStore::load() -> TrustStore` (the file `<config_dir>/trust.json`, 0600, schema_version), `is_approved(&self, digest) -> bool`, `approve(digest, id, origin_kind)`, `revoke(digest)`, atomic write;
  - `OriginKind { Bundled, AgentGenerated, LocallyApproved, RepositoryProvided }`;
  - rule: `create_version`, after successfully committing a version, automatically calls `TrustStore::approve(digest, id, LocallyApproved)` - a local creation through wf (CLI/UI/MCP update) counts as local approval; the digest is taken from the version's final YAML.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn lifecycle_defaults_to_active() { /* a tempdir with no file -> Active */ }

#[test]
fn approve_then_check_survives_reload() {
    // WF_CONFIG_DIR=tempdir; approve("sha256:aa", "review", OriginKind::LocallyApproved);
    // a fresh TrustStore::load() -> is_approved == true; a different digest == false
}

#[test]
fn create_version_auto_approves_digest() {
    // create_version in a tempdir project; the digest of the resulting yaml is approved
}
```

- [ ] **Step 2: Implementation**

`trust.json`: `{ "schema_version": 1, "approved": { "<digest>": { "id": "...", "origin_kind": "locally_approved", "approved_at_ms": 0 } } }`. Write: a temp file + `std::fs::rename`, `PermissionsExt::set_mode(0o600)` under `#[cfg(unix)]`. No background processes; lazy loading. A corrupt file - a stderr warning and an empty store (the data can be rebuilt by later approve calls).

In `create_version` (crates/wf-core/src/versioning.rs, after `write_provenance`): read the written version's `workflow.yaml`, `TrustStore::approve(digest_str(&yaml), id, OriginKind::LocallyApproved)`; approve errors are best-effort warnings and do not fail version creation.

- [ ] **Step 3: Tests and commit**

Run: `cargo test -p wf-core trust` - PASS; `cargo test --workspace` - PASS.

```bash
git add crates/wf-core
git commit -m "feat(core): workflow lifecycle and digest-bound trust store"
```

---

### Task 4: Structured triggers, requires, and the effects model

**Files:**
- Modify: `crates/wf-core/src/schema.rs`
- Create: `crates/wf-core/src/effects.rs`
- Modify: `crates/wf-core/src/validate.rs` (field limits; the exact validator file name is wherever `validate(&wf, &ctx)` lives, called from `prepare_run`)
- Test: unit tests in `effects.rs` and in the validator

**Interfaces:**
- Produces:
  - on `Workflow`: `#[serde(default)] pub trigger: Option<Trigger>`, `#[serde(default)] pub requires: Option<Requires>`, `#[serde(default)] pub effects: Vec<Effect>` (declared);
  - `Trigger { when: Vec<String>, avoid_when: Vec<String>, examples: Vec<String> }`;
  - `Requires { files: Vec<String>, commands: Vec<String> }`;
  - `Effect { FsRead, FsWrite, Network, External, Secrets, Irreversible }` (serde snake_case);
  - `effects::inferred(wf: &Workflow) -> BTreeSet<Effect>`, `effects::effective(wf: &Workflow) -> BTreeSet<Effect>` = inferred union declared. Narrowing via declaration is structurally impossible (a union), fixed by a test;
  - validation: every line in the trigger fields must be <= 120 characters, <= 5 items per field; a violation is Severity::Error.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn script_and_agent_nodes_infer_pessimistic_effects() {
    // a workflow with one script node: inferred contains FsRead, FsWrite, Network, External
}

#[test]
fn declared_cannot_narrow_inferred() {
    // a workflow with a script node and declared effects: [fs_read];
    // effective still contains FsWrite and Network
}

#[test]
fn trigger_lines_are_length_capped() {
    // trigger.when with a 200-character line -> validation Severity::Error
}
```

- [ ] **Step 2: Implement inference**

In `effects.rs`, match on `NodeKind` (exact variants live in `crates/wf-core/src/schema.rs`): nodes that run scripts or agents produce `{FsRead, FsWrite, Network, External}`; start/human_review-like nodes produce nothing. Unknown/new enum variants are treated pessimistically like script (default arm). `effective = inferred union wf.effects`.

- [ ] **Step 3: Tests, full run, commit**

Run: `cargo test -p wf-core` - PASS; `cargo test --workspace` - PASS (serde defaults do not break existing YAML).

```bash
git add crates/wf-core
git commit -m "feat(core): structured triggers, requires and effects model"
```

---

### Task 5: Workspace identity and the projects registry

**Files:**
- Create: `crates/wf-core/src/workspace.rs`, `crates/wf-core/src/projects.rs`
- Modify: `crates/wf-core/src/lib.rs`, `crates/wf-core/Cargo.toml` (`uuid = { version = "1", features = ["v4"] }`)
- Modify: `crates/wf-cli/src/main.rs` (touch on every run inside a project; the exact point is right after resolving root), `crates/wf-mcp/src/server.rs` (`WfMcp::new` calls touch)
- Test: unit tests in `projects.rs`

**Interfaces:**
- Produces:
  - `workspace::ensure_id(root) -> String`: reads/creates `<root>/.wf/workspace.local` (uuid v4); ensures `<root>/.wf/.gitignore` exists and contains the line `workspace.local` (create/append);
  - `workspace::fingerprint(root) -> Option<String>`: a best-effort hash of `git config --get remote.origin.url`; no git -> None;
  - `projects::touch(root)`: best-effort registration (never returns an error to the caller, never slows the command down; skipped if the env var `CI` is set or the config has `registry: false` / env `WF_NO_REGISTRY=1`);
  - `projects::list_active() -> Vec<ProjectEntry>`, `ProjectEntry { workspace_id, fingerprint: Option<String>, path, name, last_seen_ms, workflow_count, state }`;
  - `State { Active, Unreachable { since_ms }, Tombstoned { since_ms } }`;
  - `projects::mark_unreachable(workspace_id)`, `projects::resolve_root(workspace_id) -> Result<PathBuf, ProjectAccessError>` - checks the path and `.wf`, on failure moves the entry to Unreachable and returns a structured error; Unreachable older than 14 days is moved to Tombstoned on the next read; Tombstoned older than 90 days is purged on write. The thresholds are `GlobalConfig` fields with these defaults;
  - concurrency: a lock file `projects.json.lock` (create_new + retry every 25 ms, a 2 s timeout), atomic write via temp+rename, 0600, `schema_version: 1`; a corrupt file is recreated empty with a warning.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn touch_registers_and_updates_path_by_workspace_id() {
    // WF_CONFIG_DIR=tmp; project A with a .wf: touch -> list_active contains A;
    // a "move": copy .wf with the same workspace.local into a new tempdir,
    // touch -> a single entry, the new path
}

#[test]
fn unreachable_then_tombstoned_by_time_only() {
    // an entry with a nonexistent path: resolve_root -> Err, state Unreachable;
    // fake since_ms to 15 days ago -> after the next read, Tombstoned
    // and absent from list_active
}

#[test]
fn ci_env_skips_registration() { /* env CI=1 -> touch writes nothing */ }
```

For "faking since_ms", the tests need direct access to the entry: make `projects::load_raw`/`store_raw` `pub(crate)` + a `#[cfg(test)]` helper.

- [ ] **Step 2: Implementation** per Interfaces. Time resolution - via a `now_ms()`-style helper in wf-core's `projects.rs`, analogous to `wf_engine::event::now_millis`, on `SystemTime`.

- [ ] **Step 3: Wire up touch**

`wf-cli/src/main.rs`: after successfully resolving the project root (all subcommands operating inside a project) - `wf_core::projects::touch(&root)`. `WfMcp::new(root)`: the same. Server (`wf-server`): at the startup point where root is known.

- [ ] **Step 4: Tests and commit**

Run: `cargo test -p wf-core projects` - PASS; `cargo test --workspace` - PASS.

```bash
git add crates/wf-core crates/wf-cli crates/wf-mcp
git commit -m "feat(core): workspace identity and auto-registered projects registry"
```

---

### Task 6: MCP - workflow_catalog, projects_list, workflow_howto

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`, `crates/wf-mcp/src/server.rs`
- Create: `crates/wf-mcp/src/catalog.rs`, `docs/HOWTO-authoring.md` (embedded via `include_str!`)
- Test: `crates/wf-mcp/tests/catalog_tools_test.rs`

**Interfaces:**
- Consumes: Registry/store (Task 2), trust+lifecycle (Task 3), triggers/effects (Task 4), projects (Task 5).
- Produces:
  - `workflow_catalog(workspace: Option<String>, revision: Option<String>, limit: Option<usize>) -> Value`:
    - entries: `{ ref: WorkflowRef, lifecycle, trusted: bool, trigger, effective_effects, requires, shadowed: bool }` - both definitions on an id collision, the shadowed one marked with `shadowed: true`;
    - trust-aware precedence (spec 5.1): the project one wins over the global one only if it is active+approved; an untrusted/draft local one does not hide an approved global one (in that case `shadowed: true` is set on the local one);
    - `catalog_revision`: `digest_str` of a canonical concatenation of `(id, version, digest, lifecycle, trusted)` across all entries; if it matches a passed-in `revision`, the response is `{ "unchanged": true, "catalog_revision": ... }`;
    - a broken definition doesn't crash the catalog: it's skipped and lands in `diagnostics: [{id, error}]` (use `Registry::workflow_ids` + an independent load per id, per the comment on `workflow_ids` in registry.rs:107);
    - `dismissed_patterns: [String]` from the dismiss store (Task 8; an empty array before Task 8);
    - stable ordering: scope (project, global), then id.
  - `projects_list() -> Value`: entries from `projects::list_active()` excluding tombstoned;
  - `workflow_howto() -> Value`: the content of `docs/HOWTO-authoring.md` (authoring YAML, English-language trigger field rules, scope rules, parameterization, the SecretRef contract).

- [ ] **Step 1: Write the HOWTO** - real text matching the existing schema (`schema.rs`): the structure of workflow.yaml, node types, params, trigger/requires/effects, the rule "machine fields in English", "never put secret values in workflows or synopses; use env/config key names".

- [ ] **Step 2: Failing tests**

Following the pattern of the existing wf-mcp tests (`crates/wf-mcp/tests/read_tools_test.rs` - seeding a workflow in a tempdir):

```rust
#[test]
fn catalog_lists_both_scopes_with_trust_aware_shadowing() {
    // global "review" approved + project "review" draft:
    // project entry shadowed:true, global shadowed:false
}

#[test]
fn catalog_revision_unchanged_roundtrip() { /* a second call with revision -> unchanged */ }

#[test]
fn broken_workflow_lands_in_diagnostics_not_error() { /* one id's yaml is broken */ }
```

- [ ] **Step 3: Implementation + tool registration** with `annotations(read_only_hint=true)` for all three.

- [ ] **Step 4: Tests and commit**

Run: `cargo test -p wf-mcp` - PASS.

```bash
git add crates/wf-mcp docs/HOWTO-authoring.md
git commit -m "feat(mcp): workflow_catalog, projects_list and workflow_howto tools"
```

---

### Task 7: Tier 0 and the server-side policy gate

**Files:**
- Create: `crates/wf-mcp/src/instructions.rs` (static text), `crates/wf-mcp/src/policy.rs`
- Modify: `crates/wf-mcp/src/server.rs` (`get_info`, `workflow_run`)
- Test: `crates/wf-mcp/tests/policy_test.rs`

**Interfaces:**
- Produces:
  - `instructions::TIER0: &str` - static English text (~15 lines) per spec section 4: that wf exists, the rule for calling the catalog (once per user task), criteria for offering to save, the format of the single question with a movable recommendation marker, the run policy (the section 9 matrix in 4 lines, including the effects boundary for a direct request), a pointer to projects_list, the language rule (outbound in the user's chat language);
  - `get_info()` -> `.with_instructions(instructions::TIER0)`;
  - `policy::check_run(root, wref, resolved) -> Result<(), PolicyRefusal>` - server-checked:
    - lifecycle Draft or Retired - a refusal `{"policy":"draft_requires_trial"}` (a run is only possible via trial, Task 9);
    - the digest is not approved - a refusal `{"policy":"untrusted_requires_acknowledge"}` unless the argument `acknowledge_untrusted: true` was passed (the agent must ask the user; the server records the ack in the run event);
    - `Origin::Project { workspace_id: Some(other) }` - a refusal `{"policy":"cross_workspace_requires_plan"}` (the two-phase contract, Task 11);
    - preflight `requires`: missing files/commands - a refusal with the list.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn draft_workflow_is_refused_by_workflow_run() { /* lifecycle=draft -> policy error */ }

#[test]
fn untrusted_digest_needs_acknowledge() {
    // swap the version's yaml on disk after create_version (the digest drifts):
    // run -> refused; run with acknowledge_untrusted=true -> ok
}

#[test]
fn cross_workspace_run_is_refused() { /* a wref with a foreign workspace_id -> refused */ }

#[test]
fn requires_preflight_reports_missing() { /* requires.files: ["Cargo.toml"] in an empty tempdir */ }
```

- [ ] **Step 2: Implementation**, wire `policy::check_run` into the start of `workflow_run` (both modes, including background) and into the future trial/execute.

- [ ] **Step 3: Tests and commit**

Run: `cargo test -p wf-mcp` - PASS; `cargo test --workspace` - PASS.

```bash
git add crates/wf-mcp
git commit -m "feat(mcp): tier-0 instructions and server-side run policy gate"
```

---

### Task 8: workflow_capture and suggestion_dismiss

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`, `crates/wf-mcp/src/server.rs`
- Create: `crates/wf-core/src/dismiss.rs`
- Test: `crates/wf-mcp/tests/capture_tools_test.rs`

**Interfaces:**
- Produces:
  - `workflow_capture` args: `{ synopsis: Synopsis, selected_scope: "project"|"global", yaml: String }`; `Synopsis { title, steps: Vec<String>, params: Vec<ParamDraft{name, description}>, trigger: Trigger }`. v1: `yaml` is required (the agent writes it itself after `workflow_howto`); the field will become optional once a meta-workflow for generation appears - the interface will not change;
  - server side: (1) dedup - id and trigger.when are checked against the catalog in both scopes; a matching id is a refusal `duplicate_id`, a close trigger (the same normalized when string) is `possible_duplicate` with a candidate ref; (2) a secret heuristic over the synopsis and yaml - a regex set (`(?i)(api[_-]?key|secret|token|password)\s*[:=]\s*\S{8,}`, base64/hex strings of length 32+) - a refusal `secret_like_value` pointing at the string (the value is masked in the response); (3) creating a version in the chosen scope via `create_version` into the matching definition_parent, then `TrustStore::revoke(digest)` (undoes the auto-approve from Task 3: capture is not local approval), `write_lifecycle(Draft)`, and rewriting provenance with `created_by: "agent-capture"`;
  - `suggestion_dismiss` args `{ pattern: String }` - an English kebab-slug for the suggestion pattern; stored in `<config_dir>/dismissed.json` (schema_version, 0600, atomic write): `{ pattern, created_ms, ttl_days }`, default ttl 90, expired entries are cleaned up on load; the catalog (Task 6) returns the still-live patterns in `dismissed_patterns`.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn capture_creates_draft_with_provenance() {
    // capturing a valid yaml -> lifecycle=draft, digest not approved,
    // provenance.created_by == "agent-capture", workflow_run refuses with draft_requires_trial
}

#[test]
fn capture_rejects_secret_like_values() { /* yaml with token: "abcd1234efgh5678" -> secret_like_value */ }

#[test]
fn capture_rejects_duplicate_id() { /* the id already exists in the global store */ }

#[test]
fn dismiss_roundtrip_visible_in_catalog() { /* dismiss -> catalog.dismissed_patterns contains it */ }
```

- [ ] **Step 2: Implementation**; `destructive_hint=true` annotations for both tools.

- [ ] **Step 3: Tests and commit**

Run: `cargo test -p wf-mcp` - PASS.

```bash
git add crates/wf-core crates/wf-mcp
git commit -m "feat(mcp): workflow_capture draft flow and suggestion_dismiss store"
```

---

### Task 9: Trial and activation

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`, `crates/wf-mcp/src/server.rs`
- Test: `crates/wf-mcp/tests/trial_tools_test.rs`

**Interfaces:**
- Consumes: `run_background_resolved` (Task 2), effects (Task 4), trust (Task 3).
- Produces:
  - `workflow_trial` args `{ id, version?, params }`: a matrix based on `effective_effects` (spec 8.3):
    - contains `Irreversible` - a refusal `trial_forbidden_irreversible` (only an explicit user confirmation plus `workflow_approve` can proceed);
    - contains `FsWrite` and the execution_root is a git repository: `git worktree add <scratch> HEAD` (scratch in the system tempdir), run with `execution_root = worktree`, on completion `git -C <worktree> status --porcelain` + `git diff` (truncated to 64 KiB) go into the response, the worktree is removed with `git worktree remove --force`;
    - `FsWrite` without git - a refusal `trial_needs_git_worktree` (left for the user to handle manually);
    - `Network`/`External` without `FsWrite` - the run is not isolated, but the response is marked `"external_effects_executed": true` (the agent was required to confirm with the user before the call - a tier-0 rule);
    - result: `{ run_id, status, diff?, notes }`;
  - `workflow_approve` args `{ id, version? }`: `write_lifecycle(Active)` + `TrustStore::approve(digest, id, AgentGenerated)`; the policy gate (Task 7) then lets a normal run through.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn trial_of_fs_writing_workflow_runs_in_worktree_and_reports_diff() {
    // tempdir = a git repo (git init, an initial commit), a draft workflow with a
    // script node that writes a file out.txt; trial -> the diff contains out.txt;
    // out.txt is absent from the project itself
}

#[test]
fn approve_activates_and_unlocks_run() {
    // after approve: lifecycle=active, digest approved, workflow_run passes the gate
}

#[test]
fn irreversible_effects_forbid_trial() { /* declared effects: [irreversible] */ }
```

- [ ] **Step 2: Implementation**; `workflow_trial` and `workflow_approve` are `destructive_hint=true`.

- [ ] **Step 3: Tests and commit**

Run: `cargo test -p wf-mcp` - PASS; `cargo test --workspace` - PASS.

```bash
git add crates/wf-mcp
git commit -m "feat(mcp): trial runs in worktree and digest-bound activation"
```

---

### Task 10: Cross-workspace reads (RunRef across the surface)

**Files:**
- Modify: `crates/wf-mcp/src/server.rs` (args structs), `crates/wf-mcp/src/tools.rs`
- Test: `crates/wf-mcp/tests/cross_workspace_read_test.rs`

**Interfaces:**
- Produces: an optional field `#[serde(default)] workspace: Option<String>` (workspace_id) on the args of: `workflow_list`, `workflow_get`, `workflow_validate`, `workflow_catalog`, `runs_list`, `run_status`, `run_events`, `run_report`. Resolution: `projects::resolve_root(workspace_id)`; a structured unreachability error (`{"error":"workspace_unreachable","state":...}`) is passed back to the agent as-is (the 404-style machinery from Task 5 fires inside resolve_root). Absence of the field means the current root - full backward compatibility. Supervisor tools stay bound to the current workspace (cross-workspace supervision is out of scope for this plan; note it in docs/MCP.md).

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn run_status_resolves_foreign_workspace() {
    // two tempdir projects, both touched into the registry (shared WF_CONFIG_DIR);
    // a run in project B; from a WfMcp with root=A: run_status{workspace: B_id} finds the run
}

#[test]
fn unreachable_workspace_returns_structured_error() { /* B's path is removed */ }
```

- [ ] **Step 2: Implementation** - a shared helper `fn effective_root(&self, workspace: &Option<String>) -> Result<PathBuf, ToolError>` in server.rs, used across all the listed tools.

- [ ] **Step 3: Tests and commit**

Run: `cargo test -p wf-mcp` - PASS.

```bash
git add crates/wf-mcp
git commit -m "feat(mcp): cross-workspace read surface via workspace-qualified refs"
```

---

### Task 11: Two-phase cross-workspace execution (plan_token)

**Files:**
- Create: `crates/wf-mcp/src/plan.rs`
- Modify: `crates/wf-mcp/src/server.rs`, `crates/wf-mcp/Cargo.toml` (`hmac = "0.12"`, sha2 already comes in via wf-core - add it directly to wf-mcp too)
- Test: `crates/wf-mcp/tests/plan_flow_test.rs`

**Interfaces:**
- Produces:
  - `workflow_prepare_run` args `{ id, version?, workspace: String, params }` -> `{ plan: {...}, plan_token }`; the plan: target path, WorkflowRef, digest, params, effective_effects, preflight (requires + the policy gate from Task 7, except for the cross-workspace refusal - that refusal is exactly this flow); no durable mutations - `read_only_hint=true`;
  - `plan_token`: payload `{ workspace_id, id, version, digest, params_hash, effects, exp_ms, nonce }`; encoding `base64(json) + "." + hex(hmac_sha256(process_key, json))`; `process_key` - 32 random bytes per process (via `getrandom` through two `uuid::Uuid::new_v4` calls concatenated to get 32 bytes; no need to pull in `rand`); a 10-minute TTL;
  - `workflow_execute_plan` args `{ plan_token }` (`destructive_hint=true`): checks the signature, exp, that the nonce hasn't been used (an in-memory `HashSet<String>` on `WfMcp`, following the pattern of `sessions`); a repeat preflight - the on-disk digest must match the token, requires must still hold, otherwise a refusal `plan_stale`; runs via `run_background_resolved` with the target workspace's execution_root; response `{ run_ref: { workspace_id, run_id } }`; an audit event in the target workspace's run (RunStarted already carries origin/digest/execution_root - Task 2);
  - `workflow_run` with a foreign workspace_id keeps refusing (Task 7), pointing at this flow.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn prepare_then_execute_runs_in_target_workspace() { /* happy path, the run lands in B/.wf/runs */ }

#[test]
fn token_is_single_use() { /* a second execute with the same token -> plan_replayed */ }

#[test]
fn digest_drift_invalidates_plan() { /* swap the yaml between prepare and execute -> plan_stale */ }

#[test]
fn tampered_token_is_rejected() { /* flip a byte in the signature -> invalid */ }
```

- [ ] **Step 2: Implementation** per Interfaces.

- [ ] **Step 3: Tests and commit**

Run: `cargo test -p wf-mcp` - PASS; `cargo test --workspace` - PASS.

```bash
git add crates/wf-mcp
git commit -m "feat(mcp): two-phase cross-workspace execution with signed plan tokens"
```

---

### Task 12: CLI `wf projects`, documentation, final review

**Files:**
- Modify: `crates/wf-cli/src/main.rs` (a `projects` subcommand - list/remove, following the pattern of existing subcommands)
- Modify: `docs/MCP.md` (catalog, run policy, capture/trial, cross-workspace, the plan flow; extend the read-only/destructive tables with the new tools)
- Create: `docs/HOST-INTEGRATION.md` (fallback: the tier-0 text for CLAUDE.md/AGENTS.md, a link to the matrix from Task 0)
- Test: `crates/wf-cli/tests/projects_cli_test.rs`

- [ ] **Step 1: CLI test**

```rust
#[test]
fn projects_list_and_remove() {
    // WF_CONFIG_DIR=tmp, touch two projects; `wf projects` prints both;
    // `wf projects remove <id>` removes it from the list
}
```

- [ ] **Step 2: CLI implementation + documentation** (in the docs - no exclamation marks or em dashes; the note that advisory annotations are not enforcement already exists in MCP.md - reference it).

- [ ] **Step 3: Full verification**

Run: `cargo test --workspace && cargo clippy --workspace -- -D warnings` - PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/wf-cli docs
git commit -m "feat(cli): wf projects command; docs for agent-transparent workflows"
```

- [ ] **Step 5: Final whole-branch review** per superpowers:requesting-code-review, with this focus list: injection hygiene (no project text ends up in instructions), no secrets get written to any store, backward compatibility for events.jsonl and workflow_list, the policy gate sits in front of every run path (run, background, trial, execute_plan).

---

## Mapping to the spec's implementation order (section 13)

| Spec step | Plan task |
| --- | --- |
| 1. Compatibility spike | Task 0 |
| 2. Origin/target in the core | Tasks 1-2 |
| 3. Triggers + effects + catalog | Tasks 4, 6 (+3 trust as a dependency of the catalog) |
| 4. Policy gate before the first transparent run | Task 7 |
| 5. Capture draft | Task 8 |
| 6. Trial and activation | Task 9 |
| 7. Global scope + shadowing | Tasks 2, 6 (stores and the trust-aware catalog) |
| 8. Registry and cross-workspace reads | Tasks 5, 10 |
| 9. Cross-workspace execution with a contract | Task 11 |
| 10. auto_safe / trusted_auto | Out of scope for this plan (deferred by the spec, section 14) |

Out of scope for this plan (deferred by spec section 14): baking the catalog into instructions, a security-grade consent token, the `workflow_search` FTS stage (minimal matching telemetry is added at the first sign it's needed), a meta-workflow for YAML generation (the capture interface is already ready for it), "run experience memory", project<->global promotion, the product's name.
