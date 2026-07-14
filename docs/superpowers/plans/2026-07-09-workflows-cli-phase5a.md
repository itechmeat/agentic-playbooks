# Workflows CLI, Phase 5a (minor-version machinery + write tools/API) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to execute this plan task-by-task. Steps are marked with checkboxes (`- [ ]`).

**Goal:** give the visual editor its backend foundation - the machinery for creating workflow minor versions (immutable version folders, atomic number allocation, copying scripts/ and the layout, a validation gate), soft deletion into trash, layout persistence, and a structural and text diff between versions; and, on top of this shared machinery, MCP write tools (`workflow_create/update/delete`) and HTTP write endpoints for the web. The visual editor in the browser (canvas, forms, CodeMirror, two-way YAML sync, version switcher) is Phase 5b on top of this API.

**Architecture:** The shared version machinery lives in `wf-core` (a new `versioning` module), so that MCP, HTTP, and the future supervisor `workflow_patch` (Phase 6) all go through the same code (spec 10.2: "Only the core issues the new version number"). Invariant: version folders are strictly immutable (spec 4.1); any edit = a new version. The number is allocated atomically: the new version's contents are assembled in a temp folder, then an atomic `rename` claims `.wf/workflows/<id>/<N>` (spec 4.3/10.2); a number collision -> increment and retry. A user edit = a new minor version, and `current` is moved to point to it (user edits are authoritative; supervisor-agent patches, conversely, do not move `current` - that's Phase 6). The layout (`layouts/<version>.yaml`) is mutable and is copied from the parent when a version is created (spec 10.4). Deletion is soft, into `.wf/trash/` (spec 10.6). Diffs are pure functions over two YAML snapshots.

**Tech Stack:** Rust (edition 2024), serde/serde_json/serde_yaml_ng, wf-core, wf-engine (for validation/schema), wf-mcp (rmcp 2.2.0), wf-server (axum). No new external dependencies (the text YAML diff is a simple line-by-line diff, no diff crate; if a crate turns out to be needed, check the current version online and pin it, but first assess whether the line-by-line approach is sufficient).

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` - 4.1 (structure), 4.3 (atomicity), 10.1-10.2 (version semantics/mechanics), 10.4 (layout), 10.6 (deletion), 13 (MCP write tools), 12 (editor screens - implemented in 5b). Builds on Phases 1-4.

## Global Constraints

- Error texts are in English; comments/documentation are in Russian. No em dashes (U+2014) and no exclamation marks in code/docs/UI.
- The PROJECT version stays at `0.1.0` (not to be confused with the workflow versions this phase manages). The project is not bumped per phase.
- TDD is mandatory: a failing test first, then the implementation. Commit at the end of each task. Check dependency versions online (if any are added) and pin them.
- Immutability invariant: existing version folders are NEVER rewritten. Only new ones are created. Existing tests and behavior (load/list/run/resume/supervisor) stay green.
- Any `id`/`version` coming from the client that ends up in a path goes through `wf_core::registry::is_safe_segment` (path-traversal protection - as in the registry and the MCP/server handlers).
- Atomic writes of control files (`current`, layouts) go through `wf_core::fsutil::atomic_write`. Version creation finishes with an atomic `rename` from a temp folder.
- Run code-ranker before marking a task done; navigate the code via codegraph.

## Current setup (for the implementer, mirror it, don't break it)

- `crates/wf-core/src/registry.rs`: `Registry::open(root)`, `reg.load(id, version: Option<&str>) -> LoadedWorkflow { workflow: Workflow, yaml: String, layout: Option<serde_json::Value>, version: String }`, `reg.list() -> Vec<WorkflowSummary>`, `reg.profiles()`. `RegistryError`. `pub fn is_safe_segment(s: &str) -> bool` (non-empty, without `/`,`\`,`..`). `init_project(root)`. Structure: `.wf/workflows/<id>/{current, layouts/<v>.yaml, <v>/workflow.yaml, <v>/scripts/}`.
- `crates/wf-core/src/schema.rs`: `Workflow { schema, id, name, description, version, params, executors, defaults, supervisor, nodes: Vec<Node>, edges: Vec<Edge> }`, `Workflow::from_yaml(&str)`, `wf.node(id)`. `Node { id, title, #[serde(flatten)] kind: NodeKind }`. Everything derives `Serialize`.
- `crates/wf-core/src/validate.rs`: `validate(&Workflow, &ValidationContext) -> ValidationReport { issues: Vec<Issue{code,severity,message,node}> }`, `report.is_valid()`, `Severity::{Error,Warning}`, `ValidationContext { global_executors: Vec<String>, profiles: Vec<String> }`.
- `crates/wf-core/src/fsutil.rs`: `atomic_write(path, bytes) -> io::Result<()>` (temp + fsync + rename, creates parent directories).
- MCP: `crates/wf-mcp/src/tools.rs` (pure functions `-> Result<Value, ToolError{NotFound,Engine}>`), `crates/wf-mcp/src/server.rs` (`#[tool_router] impl WfMcp`, tools are registered with `#[tool]`, there's a tool-count test - currently 9 read/run + 8 supervisor = 17). `to_call_tool_result`.
- HTTP: `crates/wf-server/src/lib.rs` `build_router` (axum), `is_safe_id`, handlers `get_workflow`/`get_run_handler` etc. The watcher already sends a `workflows_changed` WS message on disk changes (edits made through the API will reach the web live through the existing watcher).

---

### Task 1: Version machinery in wf-core (`versioning`)

**Files:**
- Create: `crates/wf-core/src/versioning.rs`
- Modify: `crates/wf-core/src/lib.rs` (`pub mod versioning;`)
- Test: `crates/wf-core/tests/versioning_test.rs`

**Interfaces (Produces):**
- `#[derive(Debug, thiserror::Error)] pub enum VersioningError { NotFound(String), Validation(Vec<String> /*error codes*/), Schema(String), Conflict(String), Io(#[from] std::io::Error) }`.
- `pub fn next_minor_version(base: &str, existing: &[String]) -> String` - from `base` = `X.Y.Z`, the candidate is `X.(Y+1).0`; while the candidate is present in `existing`, increment the minor component; return the free one. If `base` doesn't parse as `A.B.C` of numbers -> start from `base` + `.1`? No: require valid semver for existing versions; on an invalid base, return `1.0.0` as the safe default (document this).
- `pub fn create_version(root: &Path, id: &str, new_yaml: &str, base_version: Option<&str>, make_current: bool) -> Result<String, VersioningError>`:
  1. `is_safe_segment(id)`, otherwise `NotFound`.
  2. Determine the base: `base_version` or read `current`. For a NEW workflow (no `<id>` folder), there is no base: this is the initial creation, version `1.0.0`, `make_current` is ignored (always true for the first version).
  3. Collect the list of existing versions (number-named folders under `<id>`, excluding `layouts`). Compute the target number `N` (`next_minor_version(base, existing)`; for a new workflow - `1.0.0`).
  4. Parse `new_yaml` into a `Workflow` (otherwise `Schema`); OVERWRITE the `version` field with `N` and `id` with `id` (the machinery is the source of the number; serialize it back to YAML deterministically). Validate via `validate` with `ValidationContext { global_executors: vec![], profiles: reg profiles }`; on Error-severity issues -> `Validation(codes)` (do NOT create the version).
  5. Assemble the contents in a temp folder `<id>/.tmp-<N>-<nanos-or-counter>`: write `workflow.yaml` (the validated YAML with version=N), copy `scripts/` from the base version's folder (if any; for a new workflow, there is none).
  6. Atomically claim the number: `std::fs::rename(tmp, <id>/<N>)`. If the target folder already exists (a race/collision): remove/reuse the tmp folder, increment `N`, retry (cap it, e.g. 100 attempts, otherwise `Conflict`). IMPORTANT: never leave a partially-visible version folder - only an atomic rename of finished contents. (Cross-process serialization is provided precisely by the atomicity of rename; additionally it's fine to take an in-process `Mutex` on `id` for the server, but rename is the primary mechanism.)
  7. Copy the parent's layout: if `layouts/<base>.yaml` exists, write it as `layouts/<N>.yaml` (`atomic_write`).
  8. If `make_current` (or it's the first version) - `atomic_write(<id>/current, N)`.
  9. Return `N`.
- A note on immutability: if the target `<id>/<N>` already exists - NEVER overwrite it; only pick the next free number.

- [ ] **Step 1: Failing test** `crates/wf-core/tests/versioning_test.rs`
  Scenarios (use `tempfile` + `init_project`, seed a workflow like in `registry_test.rs`; take valid YAML from `crates/wf-core/tests/fixtures/valid.yaml`):
  1. `next_minor_version("1.3.42", &["1.3.42".into(),"1.2.0".into()])` -> `"1.4.0"`; collision: `next_minor_version("1.0.0", &["1.0.0".into(),"1.1.0".into()])` -> `"1.2.0"` (skip past the taken 1.1.0). Invalid base -> `"1.0.0"`.
  2. `create_version` on an existing workflow with a valid edit (e.g. changing the `name`/a node's prompt): returns the new number (minor+1), a `<id>/<N>/workflow.yaml` folder is created with `version: N`, `current` is moved to `N` (when make_current=true), `layouts/<N>.yaml` is copied from the base (if it existed), the base version's original folder is NOT changed (immutability - compare contents before/after).
  3. `create_version` with an INVALID workflow (e.g. two start nodes -> V03) -> `Err(Validation(codes))`, the version folder is NOT created, `current` is not moved.
  4. `create_version` for a NEW id (no folder) -> version `1.0.0`, `current` = `1.0.0`.
  5. Traversal id -> `NotFound`.
  6. the base version's scripts/ is copied into the new one (seed `<base>/scripts/x.sh`, check it's present at `<N>/scripts/x.sh`).

- [ ] **Step 2: Confirm it fails** - `cargo test -p wf-core --test versioning_test`.
- [ ] **Step 3: Implementation** `versioning.rs` + `pub mod` in lib.rs.
- [ ] **Step 4: Regression + commit** - `cargo test --workspace`. Commit: `feat(core): minor-version creation machinery (atomic number, scripts+layout copy, validation gate)`.

---

### Task 2: Soft deletion into trash + restore

**Files:** Modify `crates/wf-core/src/versioning.rs`, `lib.rs`; Test: extend `versioning_test.rs` or a new `trash_test.rs`.

**Interfaces:**
- `pub fn delete_workflow(root: &Path, id: &str, ts_millis: u128) -> Result<PathBuf, VersioningError>` - `is_safe_segment`; if `<id>` doesn't exist -> `NotFound`; move (`std::fs::rename`) `.wf/workflows/<id>` to `.wf/trash/<id>-<ts_millis>` (create `.wf/trash/` if needed); return the path in trash. (ts is passed as an argument - deterministic in tests; the caller passes `now_millis`.)
- `pub fn list_trash(root: &Path) -> Result<Vec<String>, VersioningError>` - the folder names under `.wf/trash/`.
- `pub fn restore_workflow(root: &Path, trash_name: &str) -> Result<String /*id*/, VersioningError>` - `is_safe_segment(trash_name)`; compute the original `id` from `<trash_name>` (the part before the last `-<ts>`); if `.wf/workflows/<id>` already exists -> `Conflict`; move it back; return `id`.
- Runs are NEVER touched (the workflow snapshot lives in the run's folder). 5a delete is soft only (into trash); physical deletion and force-delete for versions referenced by runs are later.

- [ ] Step 1: test - deleting an existing one -> into trash, `.wf/workflows/<id>` disappeared, `list_trash` contains the entry; restore -> it's back, a repeated restore while `<id>` exists -> Conflict; deleting a nonexistent one -> NotFound; traversal -> NotFound.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): soft-delete workflow to trash and restore`.

---

### Task 3: Layout persistence

**Files:** Modify `crates/wf-core/src/versioning.rs`, `lib.rs`; Test: extend.

**Interfaces:**
- `pub fn save_layout(root: &Path, id: &str, version: &str, layout_yaml: &str) -> Result<(), VersioningError>` - `is_safe_segment(id)` and `is_safe_segment(version)`; the `<id>` folder must exist (otherwise NotFound); parse `layout_yaml` as `serde_yaml_ng::Value` to validate the format (otherwise `Schema`); `atomic_write(<id>/layouts/<version>.yaml, layout_yaml)`. The layout is mutable - overwriting an existing layout is ALLOWED (unlike version folders).

- [ ] Step 1: test - save_layout writes `layouts/<v>.yaml`; a repeated overwrite changes the contents; invalid YAML -> Schema; traversal id/version -> NotFound; a subsequent `reg.load(id, Some(v)).layout` reflects what was written.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): save mutable canvas layout for a version`.

---

### Task 4: Version diff (structural + text)

**Files:** Modify `crates/wf-core/src/versioning.rs`, `lib.rs`; Test: extend.

**Interfaces:**
- `#[derive(Debug, Serialize)] pub struct VersionDiff { pub nodes_added: Vec<String>, pub nodes_removed: Vec<String>, pub nodes_changed: Vec<String>, pub edges_added: Vec<String>, pub edges_removed: Vec<String>, pub yaml_diff: String }`.
- `pub fn version_diff(root: &Path, id: &str, from: &str, to: &str) -> Result<VersionDiff, VersioningError>` - `is_safe_segment` on everything; load both `workflow.yaml` files (otherwise NotFound); parse both into `Workflow`. Structural: `nodes_added` = node ids in `to` but not in `from`; `nodes_removed` = the reverse; `nodes_changed` = ids present in both but whose serialized `Node` differs; edges - keyed by the `(from,to)` pair as the string `"a->b"`. Text: a simple line-by-line unified-like diff of the two YAML files (no external crate: mark lines `-`/`+`; a primitive approach is fine - just added/removed lines by set difference, or a simple LCS, implementer's choice, but deterministic and covered by a test).

- [ ] Step 1: test - two versions differing by one node (a changed prompt) and one edge: `version_diff` gives correct nodes_changed/edges_*; `yaml_diff` is non-empty and contains the changed line; traversal/missing -> NotFound.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(core): structural and text version diff`.

---

### Task 5: MCP write tools

**Files:** Modify `crates/wf-mcp/src/tools.rs`, `crates/wf-mcp/src/server.rs`; Test: `crates/wf-mcp/tests/write_tools_test.rs` + update the tool-count in the server.rs test.

**Interfaces (tools.rs, `-> Result<Value, ToolError>`):**
- `workflow_create(root, id, yaml) -> {id, version}` - `create_version(root, id, yaml, None, true)` (a new workflow or a new version of an existing one; if the id already exists, this is effectively an update). Map `VersioningError` -> `ToolError` (Validation/Schema/Conflict -> Engine with a clear message; NotFound -> NotFound).
- `workflow_update(root, id, yaml) -> {id, version}` - like create, for an existing id; if `<id>` doesn't exist -> `NotFound`.
- `workflow_delete(root, id) -> {trashed: path}` - `delete_workflow(root, id, now_millis)`.
- Add `impl From<VersioningError> for ToolError` in tools.rs.
- In server.rs, register `#[tool] workflow_create/workflow_update/workflow_delete` (regular user-facing tools, WITHOUT a supervisor token - alongside the read/run tools). Update the count test: it was 17, becomes 20.

- [ ] Step 1: tests - create for a new workflow -> {id, version:"1.0.0"}, then load finds it; update -> a new minor version; delete -> into trash, load after delete -> NotFound; invalid yaml -> Engine(Validation); server test: tool count = 20 and includes the three new names.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(mcp): write tools (workflow_create/update/delete) on version machinery`.

---

### Task 6: HTTP write endpoints for the web

**Files:** Modify `crates/wf-server/src/lib.rs`; Test: extend `crates/wf-server/tests/api_test.rs`.

**Interfaces (axum, all gated by `is_safe_id`, errors -> 400/404/409/500 as appropriate):**
- `POST /api/workflows` body `{id, yaml}` -> `create_version(..., None, true)` -> `201/200 {id, version}`.
- `PUT /api/workflows/{id}` body `{yaml}` -> update (NotFound if missing) -> `{id, version}`.
- `DELETE /api/workflows/{id}` -> trash -> `{trashed}`.
- `PUT /api/workflows/{id}/layout` (query `?version=`) body `{layout}` (a yaml string or json, converted to a yaml string) -> `save_layout` -> `204/200`.
- `GET /api/workflows/{id}/diff?from=&to=` -> `version_diff` -> JSON `VersionDiff`.
- Version-creation validation errors -> `400` with a body containing the codes. The watcher will already notify the web over WS about disk changes.

- [ ] Step 1: tests (mirror the existing api tests): POST creates a workflow (then GET finds it); PUT creates a new version; DELETE -> into trash (GET -> 404); GET diff between two versions -> 200 with fields; traversal id -> 404; invalid yaml in POST -> 400.
- [ ] Step 2-3: failing test -> implementation.
- [ ] Step 4: `cargo test --workspace`; commit `feat(server): workflow write endpoints (create/update/delete/layout/diff)`.

---

### Task 7: code-ranker, status, changelog

**Files:** Modify `docs/tasks.md`, `CHANGELOG.md`, `README.md`.

- [ ] Step 1: `cargo metadata --format-version 1 >/dev/null && code-ranker check .`; worst-first fix for any violations, `cargo test --workspace` after the fixes.
- [ ] Step 2: README - a section about versions: an edit = a new minor version, immutable version folders, trash, MCP write tools and HTTP endpoints. Regular hyphens.
- [ ] Step 3: tasks.md "Phase 5" - mark `[x]` "Version machinery", "Layout persistence", "Version diffs", "MCP write tools"; "Workflow CRUD from the web" -> `[~]` (backend API ready, UI is 5b); add a line/note about section 5a (backend) / 5b (visual editor). CHANGELOG `### Added` - a line about the version machinery, trash, diff, write tools/endpoints.
- [ ] Step 4: commit `docs: mark phase 5a (version machinery + write tools/API) done`.

---

## Phase 5b (outline, a separate plan later) - visual editor in the browser

On top of the 5a API. Screens/features from spec 12: an svelte-flow canvas with drag-and-drop of nodes from a palette and edge connection; a node properties panel (forms per type: prompt, executor+fallbacks, profile, timeouts); a YAML panel with two-way sync (editing the YAML redraws the graph and vice versa; YAML is the source of truth); a script and prompt editor on CodeMirror 6 (check the current version online, pin it); live validation with highlighting on nodes and in the YAML; save = a `PUT/POST` call (a new minor version); layout persistence on drag (`PUT .../layout`); a version switcher showing the diff (`GET .../diff`, structural + text); CRUD actions (create/duplicate/delete-to-trash) on the list cards. Stack: Svelte 5 + @xyflow/svelte + CodeMirror 6 + vitest. This is a separate plan 5b, written after 5a ships.

## What is deliberately NOT part of Phase 5 (for the reviewer)

- `workflow_patch`/patch versions/`run_migrated`/promote-on-success/improvement-vs-workaround classification - Phase 6 (on top of this same version machinery, but these are supervisor-agent edits, not user edits; `current` moves on success, not immediately).
- Major versions (an explicit user action "new major version") and physical deletion with force for versions referenced by runs - later (5a has only minor + soft trash).
- Exporting/importing a workflow as a single file (ui.xyflow) - Phase 9.
- Run-level overrides (the effective workflow) - laid out in spec 11, not in this phase.
