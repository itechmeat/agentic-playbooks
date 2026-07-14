# Phase 6b - supervisor workflow_patch tool + capability + web version history Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the supervisor an entry point into the self-patch mechanism (MCP tool `workflow_patch`, gated by the `patch_workflow` capability) and show the patch-version history with provenance and a manual promote button in the web UI.

**Architecture:** The patch/migration/promote mechanism was implemented in Phase 6a (drive.apply_patch, Control::Patch, events, promote-on-finish). 6b is two thin wrappers on top of it: (1) the MCP tool creates a patch version via `create_patch_version` and posts `Control::Patch` (it does not write events.jsonl itself - the single-writer rule holds, drive writes the events); (2) HTTP + web expose version provenance and a manual promote button. No changes are needed to the drive/scheduler patch logic.

**Tech Stack:** Rust (wf-core versioning, wf-mcp rmcp 2.2.0, wf-server axum), Bun+Vite+Svelte 5 runes, vitest.

## Global Constraints

- The chat language with the operator is Russian; BUT this file (the plan) and all code/tests follow the project convention: comments and docstrings in Russian, error texts and code in English.
- NO em dashes (U+2014) and no exclamation marks in code, comments, docs, or UI strings. Check `grep -nP '\x{2014}|!' <file>` before committing (except `!=` and boolean `!` operators in code).
- The project stays at version 0.1.0, with no per-phase bumps.
- Any id/version/run_id coming from a client (MCP argument, HTTP path/query) must be sanitized via `is_safe_segment`/`is_safe_id` before being used in a path (path-traversal protection).
- Single-writer event invariant: only the drive stream writes `.wf/runs/<id>/events.jsonl`. The `workflow_patch` tool does NOT write events; it creates a version (in the workflow store) and posts a command to control.jsonl. Manual promote in the web UI moves `current` and edits the provenance sidecar - that's the workflow store, not events.
- The supervisor capability default = ALL (`observe`, `retry`, `patch_workflow`) - see spec §9.5 (lines 228, 664: "default: all"). This changes the current default `["observe","retry"]`. We do not introduce `edit_workspace` into the code (6b has no enforcement for it; it's a mode declaration, not a sandbox - it's introduced later together with a risk marker).
- `patch_workflow` is not included in the set if the workflow's policy set `capabilities` to an explicit list without it (deny stays precise).
- Patch classification is strictly `improvement | workaround`; the tool rejects any other value before creating a version.
- Before marking a task done, run `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets` (zero NEW warnings), and `code-ranker` (the quality gate).

---

## File Structure

- `crates/wf-core/src/versioning.rs` - add `VersionInfo` + `list_versions_with_provenance(root, id) -> Result<Vec<VersionInfo>, VersioningError>` (version + provenance + is_current). Reads `list_version_dirs`, `read_provenance`, the `current` file.
- `crates/wf-engine/src/scheduler.rs` - add `run_workflow_ref(root, run_id) -> Result<(String, String), EngineError>` (workflow id + the run's active version: the last `RunMigrated.to_version`, otherwise `RunStarted.version`).
- `crates/wf-mcp/src/tools.rs` - add `workflow_patch(root, run_id, yaml, classification, continue_from) -> Result<Value, ToolError>`; change the default in `supervisor_capabilities` to all capabilities.
- `crates/wf-mcp/src/server.rs` - add a `patch_workflow` branch to `capability_for_tool`; a new `#[tool] supervisor_patch_workflow` + the `SupervisorPatchArgs` args struct.
- `crates/wf-server/src/lib.rs` - the route `GET /api/workflows/{id}/versions` (versions+provenance) and `POST /api/workflows/{id}/versions/{version}/promote`.
- `web/src/lib/types.ts` - the `VersionInfo` types.
- `web/src/lib/api.ts` - `fetchVersions(id)`, `promoteVersion(id, version)`.
- `web/src/lib/versioninfo.ts` - a pure helper that formats the provenance string (for vitest).
- `web/src/pages/WorkflowView.svelte` - a version-history panel with provenance badges and a promote button.
- Tests: `crates/wf-core/tests/` (versioning), `crates/wf-mcp/tests/` or `#[cfg(test)]` (tool + gating), `crates/wf-server/tests/`, `web/src/lib/versioninfo.test.ts`.
- Docs: `CHANGELOG.md`, `docs/tasks.md`.

---

### Task 1: Core - list_versions_with_provenance

**Files:**
- Modify: `crates/wf-core/src/versioning.rs`
- Test: `crates/wf-core/tests/versions_provenance_test.rs` (create)

**Interfaces:**
- Consumes: `list_version_dirs` (private, in the same module), `read_provenance(root, id, version) -> Result<Option<VersionProvenance>, _>`, `VersionProvenance{created_by, run_id: Option<String>, classification: Option<String>, promoted: bool}`.
- Produces:
  ```rust
  #[derive(Debug, Clone, serde::Serialize)]
  pub struct VersionInfo {
      pub version: String,
      pub is_current: bool,
      pub provenance: Option<VersionProvenance>,
  }
  pub fn list_versions_with_provenance(root: &Path, id: &str) -> Result<Vec<VersionInfo>, VersioningError>;
  ```

- [ ] **Step 1: Write the failing test**

File `crates/wf-core/tests/versions_provenance_test.rs`:
```rust
use std::fs;
use wf_core::registry::init_project;
use wf_core::versioning::{create_patch_version, list_versions_with_provenance};

const WF: &str = r#"
schema: 1
id: demo
name: Demo
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let dir = root.join(".wf/workflows/demo/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workflow.yaml"), WF).unwrap();
    fs::write(root.join(".wf/workflows/demo/current"), "1.0.0").unwrap();
}

#[test]
fn lists_versions_with_current_flag_and_patch_provenance() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let patch = create_patch_version(dir.path(), "demo", "1.0.0", WF, "run-1", "improvement").unwrap();

    let infos = list_versions_with_provenance(dir.path(), "demo").unwrap();
    // Both versions are present.
    assert!(infos.iter().any(|i| i.version == "1.0.0"));
    let patched = infos.iter().find(|i| i.version == patch).expect("patch version listed");
    // current did not move on the patch bump - 1.0.0 stays current.
    assert!(infos.iter().find(|i| i.version == "1.0.0").unwrap().is_current);
    assert!(!patched.is_current);
    // The patch provenance is filled in.
    let prov = patched.provenance.as_ref().expect("patch has provenance");
    assert_eq!(prov.classification.as_deref(), Some("improvement"));
    assert_eq!(prov.run_id.as_deref(), Some("run-1"));
    assert!(!prov.promoted);
}

#[test]
fn unknown_workflow_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    assert!(list_versions_with_provenance(dir.path(), "nope").is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wf-core --test versions_provenance_test`
Expected: FAIL - `list_versions_with_provenance` / `VersionInfo` do not exist.

- [ ] **Step 3: Write minimal implementation**

In `versioning.rs` (next to the provenance functions). First confirm that `VersionProvenance` already has `#[derive(Serialize)]` - if not, add `Serialize` to the derive. Then:
```rust
/// A workflow version with a current flag and optional patch provenance.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub is_current: bool,
    pub provenance: Option<VersionProvenance>,
}

/// Lists workflow versions with provenance and a current marker.
/// Order matches `list_version_dirs` (lexicographic by folder name).
pub fn list_versions_with_provenance(
    root: &Path,
    id: &str,
) -> Result<Vec<VersionInfo>, VersioningError> {
    if !is_safe_segment(id) {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    let workflow_dir = root.join(".wf/workflows").join(id);
    if !workflow_dir.is_dir() {
        return Err(VersioningError::NotFound(id.to_string()));
    }
    let current = fs::read_to_string(workflow_dir.join("current"))
        .ok()
        .map(|s| s.trim().to_string());
    let mut out = Vec::new();
    for version in list_version_dirs(&workflow_dir)? {
        let is_current = current.as_deref() == Some(version.as_str());
        let provenance = read_provenance(root, id, &version)?;
        out.push(VersionInfo { version, is_current, provenance });
    }
    Ok(out)
}
```
Check whether the `is_safe_segment` import (from `crate::registry`) is already present in the module; if not, add `use crate::registry::is_safe_segment;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wf-core --test versions_provenance_test`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit** (do NOT commit - the operator commits manually after reviewing the larger chunk; instead of committing, leave the changes in the working tree and move on to the next task).

---

### Task 2: Engine - run_workflow_ref helper

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs`
- Test: `crates/wf-engine/tests/migrate_test.rs` (add a test; the file already exists)

**Interfaces:**
- Consumes: `read_all(run_dir)`, `EventPayload::{RunStarted{workflow,version}, RunMigrated{from_version,to_version,continue_from}}`, the existing run-path resolution logic (see `resolve_run_dir`/its equivalent in scheduler; if there is no public one - build `root.join(".wf/runs").join(run_id)` with an `is_safe_segment(run_id)` check).
- Produces:
  ```rust
  pub fn run_workflow_ref(root: &Path, run_id: &str) -> Result<(String, String), EngineError>;
  // Returns (workflow_id, active_version): the active version is the last
  // RunMigrated.to_version, otherwise RunStarted.version.
  ```

- [ ] **Step 1: Write the failing test**

Add to `crates/wf-engine/tests/migrate_test.rs` (using the already-existing helpers `seed`, `prepare`, `create_patch_version`, `post_supervisor_command`, `drive_in_background`, `wait_result`, `WF_PROMPTS`, `PATCH_IMPROVEMENT`):
```rust
#[test]
fn run_workflow_ref_reports_active_version_after_migration() {
    use wf_engine::scheduler::run_workflow_ref;
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path(), WF_PROMPTS);
    let (prepared, run_id, _run_dir) = prepare(dir.path());

    // Before migration, the active version is the starting one.
    let (id0, ver0) = run_workflow_ref(dir.path(), &run_id).unwrap();
    assert_eq!(id0, "migrate");
    assert_eq!(ver0, "1.0.0");

    let version = create_patch_version(dir.path(), "migrate", "1.0.0", PATCH_IMPROVEMENT, &run_id, "improvement").unwrap();
    post_supervisor_command(
        dir.path(),
        &run_id,
        Control::Patch { version: version.clone(), classification: "improvement".into(), continue_from: "p1".into() },
    ).unwrap();
    let result = wait_result(&drive_in_background(dir.path().to_path_buf(), prepared));
    assert_eq!(result.outcome, RunStatus::Succeeded);

    // After migration, the active version is the patched one.
    let (_id, ver1) = run_workflow_ref(dir.path(), &run_id).unwrap();
    assert_eq!(ver1, version);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wf-engine --test migrate_test run_workflow_ref_reports_active_version_after_migration`
Expected: FAIL - `run_workflow_ref` does not exist.

- [ ] **Step 3: Write minimal implementation**

In `scheduler.rs` (a public function next to the other run helpers). Check whether `is_safe_segment` is already imported (it was imported in Phase 6a: `use wf_core::registry::{is_safe_segment, Registry}`):
```rust
/// Returns (workflow id, the run's active version). The active version is
/// the last `RunMigrated.to_version`; if there were no migrations - `RunStarted.version`.
/// Used by the `workflow_patch` tool to choose the patch version's base.
pub fn run_workflow_ref(root: &Path, run_id: &str) -> Result<(String, String), EngineError> {
    if !is_safe_segment(run_id) {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let run_dir = root.join(".wf/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(run_id.to_string()));
    }
    let events = read_all(&run_dir)?;
    let mut id: Option<String> = None;
    let mut version: Option<String> = None;
    for event in &events {
        match &event.payload {
            EventPayload::RunStarted { workflow, version: v } => {
                id = Some(workflow.clone());
                version = Some(v.clone());
            }
            EventPayload::RunMigrated { to_version, .. } => {
                version = Some(to_version.clone());
            }
            _ => {}
        }
    }
    match (id, version) {
        (Some(id), Some(version)) => Ok((id, version)),
        _ => Err(EngineError::NotFound(format!("run `{run_id}` has no RunStarted event"))),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wf-engine --test migrate_test`
Expected: PASS (6 tests: 5 existing + the new one).

- [ ] **Step 5: Commit** (do NOT commit - see Task 1 Step 5.)

---

### Task 3: MCP tool fn - workflow_patch + capabilities default

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`
- Test: `crates/wf-mcp/tests/patch_tool_test.rs` (create)

**Interfaces:**
- Consumes: `wf_engine::scheduler::run_workflow_ref` (Task 2), `wf_core::versioning::create_patch_version(root, id, base_version, new_yaml, run_id, classification) -> Result<String, VersioningError>`, `post_supervisor_command(root, run_id, Control::Patch{version, classification, continue_from})`.
- Produces:
  ```rust
  pub fn workflow_patch(
      root: &Path,
      run_id: &str,
      yaml: &str,
      classification: &str,
      continue_from: &str,
  ) -> Result<Value, ToolError>;
  // Returns json!({"version": <new>, "posted_seq": <seq>}).
  ```

- [ ] **Step 1: Write the failing test**

File `crates/wf-mcp/tests/patch_tool_test.rs`. The tool only creates a version and posts a command - we do not run drive in the test, we check the side effects:
```rust
use std::fs;
use std::path::Path;
use wf_core::registry::init_project;
use wf_mcp::tools::{workflow_patch, supervisor_capabilities};
use wf_engine::scheduler::{prepare_supervised_background, RunMode, RunOptions};
use wf_engine::event::{read_all, EventPayload};

const WF: &str = r#"
schema: 1
id: demo
name: Demo
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "one" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

const PATCH: &str = r#"
schema: 1
id: demo
name: Demo
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: p1, type: prompt, prompt: "one improved" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: p1 }
  - { from: p1, to: done }
"#;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let dir = root.join(".wf/workflows/demo/1.0.0");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workflow.yaml"), WF).unwrap();
    fs::write(root.join(".wf/workflows/demo/current"), "1.0.0").unwrap();
}

fn prepared_run(root: &Path) -> String {
    let prepared = prepare_supervised_background(
        root, "demo", None,
        RunOptions { mode: RunMode::Supervised, ..Default::default() },
    ).unwrap();
    prepared.run_id().to_string()
    // prepared is dropped - control.jsonl and events are already on disk (RunStarted was recorded during prepare).
}

#[test]
fn workflow_patch_creates_version_and_posts_control() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = prepared_run(dir.path());

    let res = workflow_patch(dir.path(), &run_id, PATCH, "improvement", "p1").unwrap();
    let version = res["version"].as_str().unwrap().to_string();
    assert!(res["posted_seq"].is_number());

    // The patch version was created (folder exists), current is NOT moved.
    assert!(dir.path().join(".wf/workflows/demo").join(&version).join("workflow.yaml").is_file());
    assert_eq!(fs::read_to_string(dir.path().join(".wf/workflows/demo/current")).unwrap().trim(), "1.0.0");

    // The Patch command appeared in control.jsonl (there is NO Patch event yet - drive writes those).
    let control = fs::read_to_string(dir.path().join(".wf/runs").join(&run_id).join("control.jsonl")).unwrap();
    assert!(control.contains("\"cmd\":\"patch\"") || control.contains("patch"));
    assert!(!read_all(&dir.path().join(".wf/runs").join(&run_id)).unwrap()
        .iter().any(|e| matches!(e.payload, EventPayload::PatchApplied { .. })));
}

#[test]
fn workflow_patch_rejects_bad_classification() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let run_id = prepared_run(dir.path());
    assert!(workflow_patch(dir.path(), &run_id, PATCH, "nonsense", "p1").is_err());
}

#[test]
fn default_capabilities_include_patch_workflow() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let caps = supervisor_capabilities(dir.path(), "demo", None).unwrap();
    assert!(caps.contains(&"observe".to_string()));
    assert!(caps.contains(&"retry".to_string()));
    assert!(caps.contains(&"patch_workflow".to_string()));
}
```

Note for the implementer: the exact tag format for the command in control.jsonl is `#[serde(tag = "cmd", rename_all = "snake_case")]`, so `"cmd":"patch"`. If asserting on the raw string is fragile - parse the lines as `Control` via `serde_json` and match on `Control::Patch`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wf-mcp --test patch_tool_test`
Expected: FAIL - `workflow_patch` does not exist; `default_capabilities_include_patch_workflow` fails (the default does not include patch_workflow yet).

- [ ] **Step 3: Write minimal implementation**

In `tools.rs`:
```rust
/// Creates a workflow patch version from the patched YAML and posts a
/// run-migration command. Writes no events - drive will write them when it
/// applies `Control::Patch` (single-writer). The patch base is the run's active version.
pub fn workflow_patch(
    root: &Path,
    run_id: &str,
    yaml: &str,
    classification: &str,
    continue_from: &str,
) -> Result<Value, ToolError> {
    if !matches!(classification, "improvement" | "workaround") {
        return Err(ToolError::Engine(format!("invalid classification `{classification}`")));
    }
    let (id, base_version) = wf_engine::scheduler::run_workflow_ref(root, run_id)?;
    let version = create_patch_version(root, &id, &base_version, yaml, run_id, classification)?;
    let seq = post_supervisor_command(
        root,
        run_id,
        Control::Patch {
            version: version.clone(),
            classification: classification.to_string(),
            continue_from: continue_from.to_string(),
        },
    )?;
    Ok(json!({ "version": version, "posted_seq": seq }))
}
```
Imports: `create_patch_version` from `wf_core::versioning` (check/add to `use`), `Control` is already imported. And change the default in `supervisor_capabilities`:
```rust
        None => vec![
            "observe".to_string(),
            "retry".to_string(),
            "patch_workflow".to_string(),
        ],
```
Update the `supervisor_capabilities` docstring: "key is absent -> default `["observe", "retry", "patch_workflow"]` (all implemented capabilities, see spec 9.5)".

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wf-mcp --test patch_tool_test`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit** (do NOT commit - see Task 1 Step 5.)

---

### Task 4: MCP server - supervisor_patch_workflow tool + capability gate

**Files:**
- Modify: `crates/wf-mcp/src/server.rs`
- Test: in `crates/wf-mcp/src/server.rs` (`#[cfg(test)]` next to `supervisor_tool_rejects_unknown_token`, ~line 671)

**Interfaces:**
- Consumes: `capability_for_tool` (server.rs:173), `self.resolve_session(&token, tool_name)`, `tools::workflow_patch` (Task 3), the `#[tool]` handler pattern (server.rs:386-502).
- Produces: the MCP tool `supervisor_patch_workflow` with arguments `{token, yaml, classification, continue_from}`, gated via the `patch_workflow` capability.

- [ ] **Step 1: Write the failing test**

Add a test next to the existing server tests (which use `WfMcp::new`, `mint_token`, `resolve_session`). We check the gate at the `resolve_session` level plus the `capability_for_tool` mapping:
```rust
#[test]
fn patch_workflow_tool_maps_to_patch_capability() {
    assert_eq!(capability_for_tool("supervisor_patch_workflow"), "patch_workflow");
}

#[tokio::test]
async fn patch_workflow_rejected_without_capability() {
    let dir = tempfile::tempdir().unwrap();
    let server = WfMcp::new(dir.path().to_path_buf());
    // A session with only observe - patch_workflow is not granted.
    let token = server.mint_token("run-x".to_string(), vec!["observe".to_string()]);
    let err = server.resolve_session(&token, "supervisor_patch_workflow").unwrap_err();
    assert!(err.to_string().contains("patch_workflow"));
}

#[tokio::test]
async fn patch_workflow_allowed_with_capability() {
    let dir = tempfile::tempdir().unwrap();
    let server = WfMcp::new(dir.path().to_path_buf());
    let token = server.mint_token("run-x".to_string(), vec!["patch_workflow".to_string()]);
    assert_eq!(server.resolve_session(&token, "supervisor_patch_workflow").unwrap(), "run-x");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wf-mcp patch_workflow`
Expected: FAIL - `capability_for_tool` returns `"unknown"` for the new name.

- [ ] **Step 3: Write minimal implementation**

In `capability_for_tool` (server.rs:173) add a branch:
```rust
        "supervisor_patch_workflow" => "patch_workflow",
```
Add an args struct next to the other Supervisor*Args:
```rust
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SupervisorPatchArgs {
    pub token: String,
    pub yaml: String,
    pub classification: String,
    pub continue_from: String,
}
```
(Check which derives the neighboring args structs use - use the same ones.) Add a handler to `#[tool_router] impl WfMcp` next to the other supervisor tools:
```rust
    #[tool(
        description = "Patch the workflow of a supervised run: create a patch version from the given YAML and migrate the run onto it, continuing from the given node. classification is `improvement` or `workaround`. Requires the `patch_workflow` capability"
    )]
    async fn supervisor_patch_workflow(
        &self,
        Parameters(SupervisorPatchArgs { token, yaml, classification, continue_from }): Parameters<
            SupervisorPatchArgs,
        >,
    ) -> CallToolResult {
        let run_id = match self.resolve_session(&token, "supervisor_patch_workflow") {
            Ok(r) => r,
            Err(e) => return to_call_tool_result(Err(e)),
        };
        to_call_tool_result(tools::workflow_patch(&self.root, &run_id, &yaml, &classification, &continue_from))
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wf-mcp`
Expected: PASS (new + existing server tests; check that the existing default-capabilities test, if it asserts `["observe","retry"]`, is updated for the new default including `patch_workflow`).

- [ ] **Step 5: Commit** (do NOT commit - see Task 1 Step 5.)

---

### Task 5: HTTP - versions-with-provenance + manual promote

**Files:**
- Modify: `crates/wf-server/src/lib.rs`
- Test: `crates/wf-server/tests/versions_api_test.rs` (create; check the pattern of the existing server tests, e.g. `ws_test.rs`, and how `build_router`/`AppState` are set up)

**Interfaces:**
- Consumes: `wf_core::versioning::{list_versions_with_provenance, promote_version}`, `is_safe_id`, `versioning_error(e)` (the mapper in lib.rs).
- Produces: `GET /api/workflows/{id}/versions` -> `Json(Vec<VersionInfo>)`; `POST /api/workflows/{id}/versions/{version}/promote` -> `Json({"promoted": version})`.

- [ ] **Step 1: Write the failing test**

File `crates/wf-server/tests/versions_api_test.rs`. Use the same way of calling the router as the existing tests (via `tower::ServiceExt::oneshot`, or whatever the crate already uses - check). Skeleton:
```rust
// Check ws_test.rs for the helpers that set up the router and AppState.
// Seed demo/1.0.0 + a patch version, hit GET versions and POST promote.
#[tokio::test]
async fn get_versions_returns_provenance() { /* ... */ }

#[tokio::test]
async fn post_promote_moves_current() { /* ... */ }
```
Assertions:
- GET `/api/workflows/demo/versions` -> 200, the body contains an object with `version` == the patch version and `provenance.classification` == "improvement", and an object `1.0.0` with `is_current: true`.
- POST `/api/workflows/demo/versions/<patch>/promote` -> 200; after it, GET versions shows `is_current: true` on the patch version and `provenance.promoted: true`.
- POST with a nonexistent version -> 404 (via `versioning_error`).
- GET with an unsafe id (`..`) -> 404.

For the implementer: if there is no server-test infrastructure yet, add a minimal helper that sets up `build_router(AppState{root})` and send requests via `oneshot`. Read bodies with `axum::body::to_bytes`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p wf-server --test versions_api_test`
Expected: FAIL - the routes do not exist (404 on the unknown path for GET versions).

- [ ] **Step 3: Write minimal implementation**

In `build_router` (lib.rs:34) add:
```rust
        .route("/api/workflows/{id}/versions", get(list_versions_handler))
        .route(
            "/api/workflows/{id}/versions/{version}/promote",
            post(promote_version_handler),
        )
```
(Check that `post` is imported from `axum::routing`.) Handlers:
```rust
async fn list_versions_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match wf_core::versioning::list_versions_with_provenance(&state.root, &id) {
        Ok(infos) => Json(infos).into_response(),
        Err(e) => versioning_error(e),
    }
}

async fn promote_version_handler(
    State(state): State<AppState>,
    AxPath((id, version)): AxPath<(String, String)>,
) -> impl IntoResponse {
    if !is_safe_id(&id) || !is_safe_id(&version) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match wf_core::versioning::promote_version(&state.root, &id, &version) {
        Ok(()) => Json(serde_json::json!({ "promoted": version })).into_response(),
        Err(e) => versioning_error(e),
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p wf-server`
Expected: PASS.

- [ ] **Step 5: Commit** (do NOT commit - see Task 1 Step 5.)

---

### Task 6: Web - version history panel + promote button

**Files:**
- Modify: `web/src/lib/types.ts`, `web/src/lib/api.ts`
- Create: `web/src/lib/versioninfo.ts`, `web/src/lib/versioninfo.test.ts`
- Modify: `web/src/pages/WorkflowView.svelte`

**Interfaces:**
- Consumes: `GET /api/workflows/{id}/versions`, `POST /api/workflows/{id}/versions/{version}/promote` (Task 5).
- Produces: `fetchVersions(id): Promise<VersionInfo[]>`, `promoteVersion(id, version): Promise<{promoted: string}>`, a version-history component in `WorkflowView`.

- [ ] **Step 1: Write the failing test (pure formatter)**

File `web/src/lib/versioninfo.test.ts`:
```ts
import { describe, it, expect } from 'vitest'
import { provenanceLabel } from './versioninfo'
import type { VersionInfo } from './types'

describe('provenanceLabel', () => {
  it('labels a user minor version with no provenance', () => {
    const v: VersionInfo = { version: '1.1.0', is_current: true, provenance: null }
    expect(provenanceLabel(v)).toBe('minor, current')
  })
  it('labels a promoted improvement patch from a run', () => {
    const v: VersionInfo = {
      version: '1.0.1',
      is_current: false,
      provenance: { created_by: 'supervisor', run_id: 'run-1', classification: 'improvement', promoted: true },
    }
    expect(provenanceLabel(v)).toBe('patch: improvement, promoted, run run-1')
  })
  it('labels an unpromoted workaround patch', () => {
    const v: VersionInfo = {
      version: '1.0.2',
      is_current: false,
      provenance: { created_by: 'supervisor', run_id: null, classification: 'workaround', promoted: false },
    }
    expect(provenanceLabel(v)).toBe('patch: workaround, not promoted')
  })
})
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd web && bunx vitest run src/lib/versioninfo.test.ts`
Expected: FAIL - the `versioninfo` module does not exist.

- [ ] **Step 3: Write minimal implementation**

`web/src/lib/types.ts` - add:
```ts
export interface VersionProvenance {
  created_by: string
  run_id: string | null
  classification: string | null
  promoted: boolean
}
export interface VersionInfo {
  version: string
  is_current: boolean
  provenance: VersionProvenance | null
}
```
`web/src/lib/versioninfo.ts`:
```ts
import type { VersionInfo } from './types'

// A human-readable version-provenance string for the history panel.
export function provenanceLabel(v: VersionInfo): string {
  const parts: string[] = []
  if (v.provenance) {
    parts.push(`patch: ${v.provenance.classification ?? 'unknown'}`)
    parts.push(v.provenance.promoted ? 'promoted' : 'not promoted')
    if (v.provenance.run_id) parts.push(`run ${v.provenance.run_id}`)
  } else {
    parts.push('minor')
    if (v.is_current) parts.push('current')
  }
  return parts.join(', ')
}
```
`web/src/lib/api.ts` - add:
```ts
export const fetchVersions = (id: string) =>
  getJson<VersionInfo[]>(`/api/workflows/${encodeURIComponent(id)}/versions`)

export const promoteVersion = (id: string, version: string) =>
  requestJson<{ promoted: string }>(
    `/api/workflows/${encodeURIComponent(id)}/versions/${encodeURIComponent(version)}/promote`,
    { method: 'POST' },
  )
```
(Check the signatures of `getJson`/`requestJson` in api.ts and add a `VersionInfo` import at the top.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cd web && bunx vitest run src/lib/versioninfo.test.ts`
Expected: PASS (3 tests).

- [ ] **Step 5: Wire the panel into WorkflowView.svelte**

Add a "Version History" panel to `WorkflowView.svelte`:
- on page load (and after a promote) call `fetchVersions(id)` into `$state<VersionInfo[]>([])`;
- render the list: `version` + `provenanceLabel(v)`; mark the current one with a badge;
- for unpromoted patch versions (`provenance && !provenance.promoted`) - a "Promote" button; on click `await promoteVersion(id, v.version)` then reload the list;
- show promote errors as text (no alert). Do not use exclamation marks in UI strings.
Check the component's existing style (runes `$state`/`$derived`, how the other panels are structured). Headings and labels follow the project's style.

- [ ] **Step 6: Build web to verify it compiles**

Run: `cd web && bun run build`
Expected: a successful build with no type errors.

- [ ] **Step 7: Commit** (do NOT commit - see Task 1 Step 5.)

---

### Task 7: Docs + final gate

**Files:**
- Modify: `CHANGELOG.md`, `docs/tasks.md`

- [ ] **Step 1: CHANGELOG** - add a line under the current section:
```
- Supervisor tool workflow_patch (creates a patch version and migrates the run) gated by the patch_workflow capability; HTTP endpoints for the version list with provenance and manual promote; a version-history panel in the web UI with a promote button.
```
- [ ] **Step 2: docs/tasks.md** - mark `[x]` "workflow_patch tool (6b)" and "Version history in the web UI (6b)". Remove the (6b) markers if Phase 6 is fully closed.
- [ ] **Step 3: Final gates** - `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets` (zero NEW warnings), `cd web && bun run build`, and `code-ranker` on the diff. Confirm that `grep -rnP '\x{2014}' crates web/src` is empty for the new lines.

---

## Self-Review

**Spec coverage:**
- §13 `workflow_patch` (args classification+continue_from, creates a patch version, migrates) -> Tasks 2-4.
- §9.5 capability `patch_workflow`, default = all -> Task 3 (default), Task 4 (gate).
- §10.5 promote: manual promote in the web UI -> Tasks 5-6.
- §12 version history/provenance in the web UI -> Tasks 5-6.

**Type consistency:** `VersionInfo`/`VersionProvenance` are aligned between Rust (serde serializes the fields as snake_case: `is_current`, `created_by`, `run_id`, `classification`, `promoted`) and TS (`types.ts`). Confirm that `VersionProvenance` in Rust serializes exactly these names (snake_case) - the frontend expects them verbatim.

**Placeholder scan:** concrete code in every step except the Task 5 server tests (a skeleton + an explicit instruction to check the crate's existing pattern - there is no ready-made helper there, the shape depends on the code in place).

**Open question for the operator (before committing, does not block implementation):** changing the supervisor capability default from `["observe","retry"]` to all three (adding `patch_workflow`). This matches spec §9.5, but it is a behavior change - every supervised run without an explicit policy will get the right to patch itself. Raise this for operator confirmation together with the review of the larger chunk.
