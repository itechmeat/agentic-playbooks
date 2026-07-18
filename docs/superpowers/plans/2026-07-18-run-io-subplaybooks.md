# Run input, finish answer, and sub-playbooks - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. There are NO commit steps: the controller commits after each task's review; implementers never touch git.

**Goal:** Make a playbook callable like a function. Part A gives the Start node an editable run "input prompt" (autosaved draft that does not version the playbook, snapshotted immutably into each run). Part B lets a Finish node carry a prompt+profile so an agent composes the run's answer from the accumulated context, surfaced on the dashboard and over MCP. Part C adds a `playbook` node kind that runs another playbook as a full in-process child run: the node's rendered instruction becomes the child's input (A), the child's finish answer (B) becomes the node's output.

**Architecture:** Additive to schema 2, no migration. The draft lives outside versioned content at `<registry>/playbooks/<id>/meta/instruction-draft.md` and is read at run start when no explicit instruction was passed (precedence resolved once in `prepare_run_target`). `NodeKind::Finish` gains optional `prompt`/`profile`; a finish-with-prompt executes like a reduced `agent_task` (profile chain + SOUL from the manifest, full render context, engine-default timeout/retries, no skills, no success_check, no isolation) inside `drive`, and its output is the run answer, derived by fold. `NodeKind::Playbook` runs a child via `run_playbook_node` inside `drive` (never `execute_node`), so the parent's single writer records `ChildRunStarted` before driving the child; the child runs with `allow_shared_workdir: true` so it never deadlocks on the workdir lock the parent already holds. The policy gate walks the sub-playbook tree recursively in one pass, pins each child in an extended `RunPermit`, detects cycles, and blocks on an untrusted child; the engine receives the pins verbatim (`expected_children`) and rejects drift.

**Tech Stack:** Rust workspace (edition 2024; apb-core, apb-engine, apb-mcp, apb-server), serde/serde_yaml_ng, rmcp, axum + rust-embed; Svelte 5 runes, shadcn-svelte + Tailwind v4, bun + vite + vitest.

## Global Constraints
- schema stays **2**; every new field/variant is additive and `#[serde(default)]`. Playbooks without the new fields behave byte-for-byte as before (Finish instant/empty, no drafts, no children). No migration.
- New `EventPayload` variants/fields follow the house rule: `#[serde(default)]` only.
- No em-dashes (U+2014) and no exclamation marks anywhere in docs, strings, or prose. No CJK. Machine-facing fields are English; user-facing chat is in the user's language.
- State files are written atomically via `apb_core::fsutil` (temp + rename, 0600 on unix).
- Validator codes: **V21** and **V22** are the next codes. V19/V20 are already taken (duration validation). **V15 is retired and MUST NOT be reused.**
- Secret values are never returned/logged/cached; skill content is never embedded in a prompt.
- Profile/playbook names: `[a-z0-9][a-z0-9-]*`, at most 64 chars; path segments validated with `is_safe_segment`.
- Single-writer invariant: only the `drive` loop appends to `events.jsonl`. `ChildRunStarted` is appended by `drive` (via `run_playbook_node`, which is called on the drive thread with `&mut EventLog`), never by a tool and never by `execute_node`.
- TDD: every task writes a failing test first, runs it to confirm the failure, then the minimal implementation, then re-runs to green.
- Gates (run at the end of every task): `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; for web changes `cd web && bun run check` and `bun run test`. Before the controller commits: warm the code-ranker cache with `cargo metadata --format-version 1 >/dev/null`, then `code-ranker check .` must pass (read `code-ranker docs base <ID>` for any violation).

---

## Part A - run input prompt on the Start node

### Task A1: core registry instruction-draft read/write/clear

**Files:**
- Modify: `crates/apb-core/src/registry.rs` (`Registry::read_instruction_draft`, `Registry::write_instruction_draft`, private `meta_dir`)
- Test: new `crates/apb-core/tests/instruction_draft_test.rs`

**Interfaces (produced):**
- `Registry::read_instruction_draft(&self, id: &str) -> Result<Option<String>, RegistryError>` - reads `<base>/playbooks/<id>/meta/instruction-draft.md`; `Ok(None)` when the file is absent.
- `Registry::write_instruction_draft(&self, id: &str, text: &str) -> Result<(), RegistryError>` - atomically writes the draft; an empty `text` deletes the file (clears the draft). Never touches any version dir or digest.

Steps:

- [ ] Write the failing test. Create `crates/apb-core/tests/instruction_draft_test.rs`:
```rust
use apb_core::registry::Registry;
use std::fs;

fn seed(base: &std::path::Path, id: &str) {
    let vdir = base.join("playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        format!("schema: 2\nid: {id}\nname: {id}\nversion: 1.0.0\nnodes:\n  - {{ id: s, type: start }}\nedges: []\n"),
    )
    .unwrap();
    fs::write(base.join("playbooks").join(id).join("current"), "1.0.0").unwrap();
}

#[test]
fn draft_roundtrip_and_clear() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), "p");
    let reg = Registry::open_dir(tmp.path()).unwrap();

    assert_eq!(reg.read_instruction_draft("p").unwrap(), None);

    reg.write_instruction_draft("p", "translate the plan").unwrap();
    assert_eq!(
        reg.read_instruction_draft("p").unwrap().as_deref(),
        Some("translate the plan")
    );
    // The draft lives outside any version dir.
    assert!(
        tmp.path()
            .join("playbooks/p/meta/instruction-draft.md")
            .is_file()
    );

    // Empty text clears the draft file.
    reg.write_instruction_draft("p", "").unwrap();
    assert_eq!(reg.read_instruction_draft("p").unwrap(), None);
    assert!(!tmp.path().join("playbooks/p/meta/instruction-draft.md").is_file());

    // Unsafe id is rejected.
    assert!(reg.read_instruction_draft("../x").is_err());
    assert!(reg.write_instruction_draft("../x", "z").is_err());
}
```
- [ ] Run it, expect failure (methods do not exist): `cargo test -p apb-core --test instruction_draft_test`. Expected: compile error `no method named read_instruction_draft`.
- [ ] Implement in `crates/apb-core/src/registry.rs`, inside `impl Registry` (after `fn read_current`):
```rust
    fn meta_dir(&self, id: &str) -> PathBuf {
        self.playbooks_dir().join(id).join("meta")
    }

    /// Reads the run "input prompt" draft for a playbook (spec A). The draft is
    /// plain text stored at `<base>/playbooks/<id>/meta/instruction-draft.md`,
    /// a non-version sibling (the version listing excludes `layouts` and
    /// `meta`), so it never collides with a version dir and is not part of any
    /// digest. `Ok(None)` when no draft has been saved.
    pub fn read_instruction_draft(&self, id: &str) -> Result<Option<String>, RegistryError> {
        if !is_safe_segment(id) {
            return Err(RegistryError::NotFound(id.into()));
        }
        let p = self.meta_dir(id).join("instruction-draft.md");
        if !p.is_file() {
            return Ok(None);
        }
        Ok(Some(fs::read_to_string(p)?))
    }

    /// Writes (or, for an empty `text`, clears) the run input draft. Atomic via
    /// `fsutil::atomic_write`. Bypasses the definition-change path entirely: no
    /// version bump, no digest change, no freeze interaction (a frozen playbook
    /// still accepts draft edits - the draft is run input, not definition
    /// content).
    pub fn write_instruction_draft(&self, id: &str, text: &str) -> Result<(), RegistryError> {
        if !is_safe_segment(id) {
            return Err(RegistryError::NotFound(id.into()));
        }
        let pb_dir = self.playbooks_dir().join(id);
        if !pb_dir.is_dir() {
            return Err(RegistryError::NotFound(id.into()));
        }
        let p = self.meta_dir(id).join("instruction-draft.md");
        if text.is_empty() {
            if p.is_file() {
                fs::remove_file(&p)?;
            }
            return Ok(());
        }
        fs::create_dir_all(self.meta_dir(id))?;
        atomic_write(&p, text.as_bytes())?;
        Ok(())
    }
```
- [ ] Run: `cargo test -p apb-core --test instruction_draft_test`. Expected: pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.

---

### Task A2: server input-draft endpoints (GET/PUT)

**Files:**
- Modify: `crates/apb-server/src/lib.rs` (route + `get_input_draft_handler`, `put_input_draft_handler`, `InputDraftBody`)
- Test: new `crates/apb-server/tests/input_draft_api_test.rs`

**Interfaces (produced):**
- `GET /api/playbooks/:id/input-draft` (workspace selection via the existing `?workspace=` query, matching every other route) -> `{ "instruction": string | null }`.
- `PUT` same path, body `{ "instruction": string | null }`, stores it (empty/absent clears) -> `{ "instruction": string | null }`.

Steps:

- [ ] Write the failing test. Create `crates/apb-server/tests/input_draft_api_test.rs`:
```rust
use apb_server::{AppState, build_router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    apb_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\nedges: []\n",
    )
    .unwrap();
    fs::write(dir.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();
    dir
}

async fn body_json(router: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test]
async fn input_draft_get_put_clear() {
    let dir = seed();
    let router = build_router(AppState::pinned(dir.path().to_path_buf()));

    // empty on a fresh playbook
    let (st, v) = body_json(
        &router,
        Request::get("/api/playbooks/p/input-draft").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["instruction"].is_null());

    // put a draft
    let (st, _) = body_json(
        &router,
        Request::put("/api/playbooks/p/input-draft")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"instruction":"do the thing"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // get it back
    let (_, v) = body_json(
        &router,
        Request::get("/api/playbooks/p/input-draft").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(v["instruction"], "do the thing");

    // clear with an empty string
    body_json(
        &router,
        Request::put("/api/playbooks/p/input-draft")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"instruction":""}"#))
            .unwrap(),
    )
    .await;
    let (_, v) = body_json(
        &router,
        Request::get("/api/playbooks/p/input-draft").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(v["instruction"].is_null());
}
```
Note: confirm the `AppState::pinned` constructor name by reading the top of `crates/apb-server/tests/runs_api_test.rs` and mirror whatever that file uses to build a pinned-root `AppState`; use the identical helper here.
- [ ] Run: `cargo test -p apb-server --test input_draft_api_test`. Expected: 404/route-missing failure.
- [ ] Register the route in `build_router` in `crates/apb-server/src/lib.rs`, right after the `/api/playbooks/{id}/frozen` route:
```rust
        .route(
            "/api/playbooks/{id}/input-draft",
            get(get_input_draft_handler).put(put_input_draft_handler),
        )
```
- [ ] Add the handlers near `set_frozen_handler` in `crates/apb-server/src/lib.rs`:
```rust
async fn get_input_draft_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    match reg.read_instruction_draft(&id) {
        Ok(v) => Json(serde_json::json!({ "instruction": v })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct InputDraftBody {
    #[serde(default)]
    instruction: Option<String>,
}

async fn put_input_draft_handler(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<WsQuery>,
    Json(body): Json<InputDraftBody>,
) -> impl IntoResponse {
    if !is_safe_id(&id) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let root = match resolve_root(&state, q.workspace.as_deref()) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let reg = match Registry::open(&root) {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let text = body.instruction.unwrap_or_default();
    match reg.write_instruction_draft(&id, &text) {
        Ok(()) => {
            let out = if text.is_empty() { None } else { Some(text) };
            Json(serde_json::json!({ "instruction": out })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
```
- [ ] Run: `cargo test -p apb-server --test input_draft_api_test`. Expected: pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-server --all-targets -- -D warnings`.

---

### Task A3: run-start instruction precedence (explicit beats draft beats none)

**Files:**
- Modify: `crates/apb-engine/src/scheduler/prepare.rs` (`prepare_run_target`, resolve `instruction` before building `cfg`)
- Test: new `crates/apb-engine/tests/instruction_precedence_test.rs`

**Interfaces (consumed):** `Registry::read_instruction_draft` (Task A1). No new public interface: the resolved value flows into the persisted `RunConfig.instruction` exactly as today (already immutable per run; nothing rereads the draft after start).

Steps:

- [ ] Write the failing test. Create `crates/apb-engine/tests/instruction_precedence_test.rs` (a control-only playbook: a `prompt` node, so the run completes with no agent, no workdir lock, no manifest):
```rust
use apb_core::registry::{Registry, init_project};
use apb_engine::run_config::read_run_config;
use apb_engine::scheduler::{RunOptions, run};
use std::fs;
use std::path::Path;

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(
        vdir.join("playbook.yaml"),
        "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.instruction}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n",
    )
    .unwrap();
    fs::write(root.join(".apb/playbooks/p/current"), "1.0.0").unwrap();
}

fn instruction_of(root: &Path, run_id: &str) -> Option<String> {
    read_run_config(&root.join(".apb/runs").join(run_id)).unwrap().instruction
}

#[test]
fn draft_used_when_no_explicit_instruction() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    Registry::open(dir.path())
        .unwrap()
        .write_instruction_draft("p", "from draft")
        .unwrap();

    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();
    assert_eq!(instruction_of(dir.path(), &res.run_id).as_deref(), Some("from draft"));
}

#[test]
fn explicit_instruction_beats_draft() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    Registry::open(dir.path())
        .unwrap()
        .write_instruction_draft("p", "from draft")
        .unwrap();

    let opts = RunOptions {
        instruction: Some("explicit".into()),
        ..Default::default()
    };
    let res = run(dir.path(), "p", None, opts).unwrap();
    assert_eq!(instruction_of(dir.path(), &res.run_id).as_deref(), Some("explicit"));
}

#[test]
fn none_when_no_draft_and_no_explicit() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "p", None, RunOptions::default()).unwrap();
    assert_eq!(instruction_of(dir.path(), &res.run_id), None);
}
```
- [ ] Run: `cargo test -p apb-engine --test instruction_precedence_test`. Expected: `draft_used_when_no_explicit_instruction` fails (draft ignored today).
- [ ] In `crates/apb-engine/src/scheduler/prepare.rs`, inside `prepare_run_target`, resolve the instruction just before the `let cfg = RunConfig { ... }` block. `reg` is already in scope (`Registry::open_dir(&t.definition_parent)`):
```rust
    // Run input precedence (spec A): an explicitly passed instruction wins;
    // otherwise the playbook's autosaved draft, read at start time; otherwise
    // None. Resolved once here so every surface (MCP, CLI, server, and a Part C
    // sub-playbook child) shares the rule. A blank draft is treated as absent.
    let instruction = match opts.instruction.clone() {
        Some(i) => Some(i),
        None => reg
            .read_instruction_draft(id)
            .ok()
            .flatten()
            .filter(|s| !s.trim().is_empty()),
    };
```
Then in the `RunConfig { ... }` literal replace `instruction: opts.instruction.clone(),` with `instruction,`.
- [ ] Run: `cargo test -p apb-engine --test instruction_precedence_test`. Expected: all three pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-engine --all-targets -- -D warnings`.

---

## Part B - the Finish node composes the run answer

### Task B1: schema - Finish `prompt` + `profile`

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`NodeKind::Finish` fields)
- Test: `#[cfg(test)] mod tests` in `crates/apb-core/src/schema.rs`

**Interfaces (produced):** `NodeKind::Finish { outcome: Outcome, prompt: Option<String>, profile: Option<QualifiedProfileRef> }`; both new fields `#[serde(default)]`. Existing playbooks parse unchanged (no `prompt`/`profile` -> `None`).

Steps:

- [ ] Write the failing test. Append to the existing `#[cfg(test)] mod tests` in `crates/apb-core/src/schema.rs`:
```rust
    #[test]
    fn finish_parses_with_and_without_prompt_profile() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: f1, type: finish, outcome: success }
  - { id: f2, type: finish, outcome: success, prompt: "compose", profile: writer }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        let f1 = &pb.node("f1").unwrap().kind;
        let f2 = &pb.node("f2").unwrap().kind;
        assert!(matches!(f1, NodeKind::Finish { prompt: None, profile: None, .. }));
        match f2 {
            NodeKind::Finish { prompt: Some(p), profile: Some(pr), .. } => {
                assert_eq!(p, "compose");
                assert_eq!(pr.name, "writer");
            }
            _ => panic!("expected finish with prompt+profile"),
        }
    }
```
- [ ] Run: `cargo test -p apb-core --lib finish_parses`. Expected: compile failure (no `prompt`/`profile` fields on the pattern).
- [ ] In `crates/apb-core/src/schema.rs`, extend the `Finish` variant of `NodeKind`:
```rust
    Finish {
        outcome: Outcome,
        /// Optional finish prompt (spec B). When set, an agent composes the run
        /// answer from the accumulated run context; the agent's output becomes
        /// this node's output. Absent -> instant, free, empty output (unchanged).
        #[serde(default)]
        prompt: Option<String>,
        /// Profile binding for the finish agent (spec B). Meaningful only with
        /// `prompt`; falls back to `defaults.profile`. Validator V21 errors on a
        /// profile without a prompt (a binding that can never execute).
        #[serde(default)]
        profile: Option<QualifiedProfileRef>,
    },
```
- [ ] Run: `cargo test -p apb-core --lib finish_parses`. Expected: pass. (No other apb-core match breaks: `effects::inferred` and `expected_seconds` match `NodeKind::Finish { .. }`, which still compiles; they are refined in Task B2/B3.)
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.

---

### Task B2: validator V21 + finish-with-prompt duration default and V19

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`Node::expected_seconds` gains a finish-with-prompt arm)
- Modify: `crates/apb-core/src/validate.rs` (extend V19 to finish-with-prompt; add `check_finish` for V21; wire into `validate`)
- Test: extend `crates/apb-core/tests/validate_duration_test.rs`; add `Node::expected_seconds` assertion in `crates/apb-core/src/schema.rs` tests

**Interfaces (produced):** V21 (Error): a `finish` node with `profile` set but no `prompt`. `Node::expected_seconds()` returns `DEFAULT_TASK_SECONDS` for a finish-with-prompt (0 for a finish without one, unchanged). V19 (Warning) additionally covers finish-with-prompt.

Steps:

- [ ] Write the failing tests. Append to `crates/apb-core/tests/validate_duration_test.rs`:
```rust
#[test]
fn v21_errors_on_finish_profile_without_prompt() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: f, type: finish, outcome: success, profile: writer }
edges:
  - { from: s, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(!r.is_valid());
    assert!(r.issues.iter().any(|i| i.code == "V21" && i.node.as_deref() == Some("f")));
}

#[test]
fn v19_warns_on_finish_with_prompt_without_estimate() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
defaults: { profile: x }
nodes:
  - { id: s, type: start }
  - { id: f, type: finish, outcome: success, prompt: "compose" }
edges:
  - { from: s, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(r.is_valid(), "V19 is a warning");
    assert!(r.issues.iter().any(|i| i.code == "V19" && i.node.as_deref() == Some("f")));
}
```
And append to the `#[cfg(test)] mod tests` in `crates/apb-core/src/schema.rs`:
```rust
    #[test]
    fn finish_with_prompt_defaults_to_task_seconds() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: f1, type: finish, outcome: success }
  - { id: f2, type: finish, outcome: success, prompt: "x" }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        assert_eq!(pb.node("f1").unwrap().expected_seconds(), 0);
        assert_eq!(pb.node("f2").unwrap().expected_seconds(), 120);
    }
```
- [ ] Run: `cargo test -p apb-core --lib finish_with_prompt_defaults` and `cargo test -p apb-core --test validate_duration_test v21 v19_warns_on_finish`. Expected: failures.
- [ ] In `crates/apb-core/src/schema.rs`, change `Node::expected_seconds` to match by reference and add the finish arm:
```rust
    pub fn expected_seconds(&self) -> u64 {
        if let Some(ed) = &self.expected_duration
            && let Some(s) = ed.parsed()
        {
            return s;
        }
        match &self.kind {
            NodeKind::AgentTask { .. } | NodeKind::Script { .. } => {
                crate::duration::DEFAULT_TASK_SECONDS
            }
            NodeKind::Finish { prompt: Some(_), .. } => crate::duration::DEFAULT_TASK_SECONDS,
            _ => 0,
        }
    }
```
- [ ] In `crates/apb-core/src/validate.rs`, extend the V19 arm of `check_expected_duration`. Replace the existing `None if matches!(...)` arm with:
```rust
            None if matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. })
                || matches!(n.kind, NodeKind::Finish { prompt: Some(_), .. }) =>
            {
                r.warn(
                    "V19",
                    Some(&n.id),
                    format!(
                        "node `{}` has no expected_duration; progress will use the {}s default",
                        n.id,
                        crate::duration::DEFAULT_TASK_SECONDS
                    ),
                );
            }
```
- [ ] Add `check_finish` to `crates/apb-core/src/validate.rs` (next to `check_expected_duration`):
```rust
/// V21 (error): a finish node that binds a `profile` but has no `prompt`. A
/// profile without a prompt can never execute (a finish without a prompt is
/// instant and free), so it is an authoring mistake.
fn check_finish(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::Finish {
            prompt: None,
            profile: Some(_),
            ..
        } = &n.kind
        {
            r.error(
                "V21",
                Some(&n.id),
                format!(
                    "finish node `{}` binds a profile but has no prompt; a profile without a prompt can never execute",
                    n.id
                ),
            );
        }
    }
}
```
- [ ] Wire it into `validate` unconditionally, right after the `check_expected_duration(playbook, &mut r);` line:
```rust
    check_finish(playbook, &mut r); // V21
```
- [ ] Run: `cargo test -p apb-core --lib` and `cargo test -p apb-core --test validate_duration_test`. Expected: pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy -p apb-core --all-targets -- -D warnings`.

---

### Task B3: engine - Finish composes the answer (execution + policy + manifest)

**Files:**
- Modify: `crates/apb-engine/src/scheduler/node.rs` (new `execute_finish_answer`)
- Modify: `crates/apb-engine/src/scheduler.rs` (the Finish interception in `drive`)
- Modify: `crates/apb-engine/src/scheduler/prepare.rs` (`build_run_manifest` binds a finish-with-prompt profile)
- Modify: `crates/apb-mcp/src/policy.rs` (`collect_profile_refs` includes a finish-with-prompt profile)
- Modify: `crates/apb-core/src/effects.rs` (`inferred` splits finish-with-prompt into the acting group)
- Test: new `crates/apb-engine/tests/finish_answer_test.rs` (stub agent)

**Interfaces (produced):** `pub(crate) fn execute_finish_answer(playbook, run_dir, workdir, node_id, run_id, state, cfg, prompt) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError>`. On success the finish node's `NodeFinished.output` is the agent's answer text; a failed finish agent fails the run.

Steps:

- [ ] Write the failing test. Create `crates/apb-engine/tests/finish_answer_test.rs` (mirrors the stub-agent pattern of `profile_run_test.rs`):
```rust
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use apb_core::registry::init_project;
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};

mod common;

static ENV_LOCK: Mutex<()> = Mutex::new(());
fn lock() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
struct EnvGuard;
impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("APB_AGENT_CMD");
            std::env::remove_var("APB_CONFIG_DIR");
            std::env::remove_var("HOME");
        }
    }
}
fn make_stub(dir: &Path, body: &str) -> String {
    let path = dir.join("stub.sh");
    common::write_sync(&path, &format!("#!/bin/sh\n{body}\n"));
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn finish_with_prompt_stores_answer_as_output() {
    let _l = lock();
    let _g = EnvGuard;
    let proj = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();

    init_project(proj.path()).unwrap();
    common::seed_profile(proj.path(), "writer", "claude", "haiku", &[]);
    let src = "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults:\n  profile: writer\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success, prompt: \"compose the answer\" }\nedges:\n  - { from: s, to: f }\n";
    let vdir = proj.path().join(".apb/playbooks/p/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), src).unwrap();
    fs::write(proj.path().join(".apb/playbooks/p/current"), "1.0.0").unwrap();

    unsafe {
        std::env::set_var("APB_AGENT_CMD", make_stub(bin.path(), "echo FINAL_ANSWER"));
        std::env::set_var("HOME", home.path());
        std::env::set_var("APB_CONFIG_DIR", cfg.path());
    }

    let res = run(proj.path(), "p", None, RunOptions::default()).expect("run ok");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let run_dir = proj.path().join(".apb/runs").join(&res.run_id);
    let events = apb_engine::event::read_all(&run_dir).unwrap();
    let state = RunState::fold(&events);
    assert_eq!(state.outputs.get("f").map(|s| s.as_str()), Some("FINAL_ANSWER"));
}
```
- [ ] Run: `cargo test -p apb-engine --test finish_answer_test`. Expected: failure (finish ignores the prompt; output empty).
- [ ] Add `execute_finish_answer` to `crates/apb-engine/src/scheduler/node.rs` (after `execute_node`). It is a reduced `agent_task`: no skills, no success_check, no isolation, engine-default timeout/retries:
```rust
/// Composes the run answer for a finish-with-prompt (spec B). A reduced
/// `agent_task`: the profile chain + SOUL come from the run manifest (identical
/// resolution/trust to an agent_task), the prompt renders with the full
/// standard context, but no skills are delivered and there is no success_check
/// and no isolation. Timeout/retries fall back to `defaults`. Returns
/// (status, answer, events); drive writes the events (single writer).
#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_finish_answer(
    playbook: &Playbook,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    run_id: &str,
    state: &RunState,
    cfg: &RunConfig,
    prompt: &str,
) -> Result<(NodeStatus, String, Vec<EventPayload>), EngineError> {
    let context = build_context_for_render(run_dir, &read_all(run_dir)?)?;
    let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
        .into_iter()
        .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
        .collect();
    let text = render(
        prompt,
        &cfg.params,
        cfg.instruction.as_deref(),
        &state.outputs,
        &state.reviews,
        &hooks,
        &context,
    );
    let retries = playbook.defaults.max_retries.unwrap_or(0);
    let timeout = playbook.defaults.timeout_seconds.map(Duration::from_secs);
    let grant_autonomy = apb_core::effects::effective(playbook)
        .iter()
        .any(|e| !matches!(e, apb_core::schema::Effect::FsRead));

    let manifest = crate::manifest::read(run_dir)?.ok_or_else(|| {
        EngineError::Invalid(format!("finish node `{node_id}` has no execution manifest"))
    })?;
    let entry = manifest.for_node(node_id).cloned().ok_or_else(|| {
        EngineError::Invalid(format!("no manifest entry for finish node `{node_id}`"))
    })?;
    if entry.chain.is_empty() {
        return Err(EngineError::Invalid(format!(
            "finish node `{node_id}` has an empty executor chain"
        )));
    }

    let cancel = AtomicBool::new(false);
    let mut events: Vec<EventPayload> = Vec::new();
    let mut attempt: u32 = 0;
    let mut last_msg = String::new();
    let mut last_timed_out = false;
    for (idx, ri) in entry.chain.iter().enumerate() {
        if idx > 0 {
            events.push(EventPayload::FallbackTriggered {
                node: node_id.into(),
                from: entry.chain[idx - 1].agent_id.clone(),
                to: ri.agent_id.clone(),
                profile: Some(entry.key()),
            });
        }
        let adapter = crate::adapter::ClaudeAdapter {
            program: ri.canonical_executable.to_string_lossy().into_owned(),
            spec: ri.spec.clone(),
        };
        for try_i in 0..=retries {
            attempt += 1;
            if try_i > 0 {
                events.push(EventPayload::RetryStarted { node: node_id.into(), attempt });
            }
            events.push(EventPayload::AttemptStarted {
                node: node_id.into(),
                attempt,
                agent: ri.agent_id.clone(),
                soul_delivery: Some(soul_delivery_str(ri.soul_delivery)),
                skills_mode: None,
            });
            let stream_log = run_dir
                .join("agent-stream")
                .join(format!("{node_id}-{attempt}.jsonl"));
            let task = AgentTask {
                prompt: &text,
                model: &ri.model,
                workdir,
                timeout,
                stream_log: Some(&stream_log),
                soul: Some(entry.soul.as_str()),
                grant_autonomy,
            };
            match adapter.run_cancellable(&task, &cancel) {
                Ok(report) => {
                    events.push(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: report.status.as_str().into(),
                    });
                    if report.status == NodeStatus::Succeeded {
                        return Ok((NodeStatus::Succeeded, report.summary, events));
                    }
                    last_msg = report.summary;
                    last_timed_out = false;
                }
                Err((class, msg)) => {
                    last_timed_out = class == ErrorClass::Timeout;
                    events.push(EventPayload::AttemptFinished {
                        node: node_id.into(),
                        attempt,
                        status: if last_timed_out { "timed_out" } else { "failed" }.into(),
                    });
                    last_msg = msg;
                    if class == ErrorClass::Transport || class == ErrorClass::Timeout {
                        break;
                    }
                }
            }
        }
    }
    let final_status = if last_timed_out {
        NodeStatus::TimedOut
    } else {
        NodeStatus::Failed
    };
    Ok((final_status, last_msg, events))
}
```
- [ ] Rework the Finish interception in `drive` (`crates/apb-engine/src/scheduler.rs`). Replace the whole `if let NodeKind::Finish { outcome: o } = &node_kind { ... }` block with:
```rust
        if let NodeKind::Finish {
            outcome: o,
            prompt,
            profile: _,
        } = &node_kind
        {
            // A finish-with-prompt composes the run answer via an agent (spec
            // B); a finish without a prompt is instant with an empty output
            // (unchanged). NodeStarted + attempt events are written only for the
            // agent path.
            let answer_output = if let Some(p) = prompt {
                let state = RunState::fold(&read_all(run_dir)?);
                log.append(EventPayload::NodeStarted {
                    node: current.clone(),
                    attempt: 1,
                })?;
                let (st, out, evs) =
                    execute_finish_answer(&playbook, run_dir, &workdir, &current, &run_id, &state, cfg, p)?;
                for ev in evs {
                    log.append(ev)?;
                }
                if st != NodeStatus::Succeeded {
                    log.append(EventPayload::NodeFinished {
                        node: current.clone(),
                        status: st.as_str().into(),
                        attempt: 1,
                        output: out.clone(),
                    })?;
                    log.append(EventPayload::RunFinished {
                        outcome: "failed".into(),
                    })?;
                    return Ok(RunResult {
                        run_id,
                        outcome: RunStatus::Failed,
                    });
                }
                out
            } else {
                String::new()
            };
            let outcome = match o {
                Outcome::Success => RunStatus::Succeeded,
                Outcome::Failure => RunStatus::Failed,
            };
            let s = match o {
                Outcome::Success => "succeeded",
                Outcome::Failure => "failed",
            };
            log.append(EventPayload::NodeFinished {
                node: current.clone(),
                status: "succeeded".into(),
                attempt: 1,
                output: answer_output,
            })?;
            if outcome == RunStatus::Succeeded
                && let Some(applied) = last_applied_patch.as_ref()
            {
                promote_applied_patch(root, run_dir, log, &playbook, applied)?;
            }
            log.append(EventPayload::RunFinished { outcome: s.into() })?;
            return Ok(RunResult { run_id, outcome });
        }
```
- [ ] In `crates/apb-engine/src/scheduler/prepare.rs`, extend `build_run_manifest`'s bindings loop to bind a finish-with-prompt profile. Replace the node loop `for n in &playbook.nodes { if let NodeKind::AgentTask { profile, .. } = &n.kind && ... }` with:
```rust
    for n in &playbook.nodes {
        let pref = match &n.kind {
            NodeKind::AgentTask { profile, .. } => {
                profile.clone().or_else(|| playbook.defaults.profile.clone())
            }
            NodeKind::Finish { prompt: Some(_), profile, .. } => {
                profile.clone().or_else(|| playbook.defaults.profile.clone())
            }
            _ => None,
        };
        if let Some(pref) = pref {
            bindings.push((n.id.clone(), pref));
        }
    }
```
- [ ] In `crates/apb-mcp/src/policy.rs`, extend `collect_profile_refs`'s node loop the same way. Replace the `for n in &playbook.nodes { if let NodeKind::AgentTask { profile, .. } ... }` with:
```rust
    for n in &playbook.nodes {
        let pref = match &n.kind {
            NodeKind::AgentTask { profile, .. } => {
                profile.clone().or_else(|| playbook.defaults.profile.clone())
            }
            NodeKind::Finish { prompt: Some(_), profile, .. } => {
                profile.clone().or_else(|| playbook.defaults.profile.clone())
            }
            _ => None,
        };
        if let Some(p) = pref {
            refs.push(p);
        }
    }
```
- [ ] In `crates/apb-core/src/effects.rs`, split the `Finish` arm of `inferred`. Change the no-op group line to end with `NodeKind::Finish { prompt: None, .. } => {}` and add finish-with-prompt to the acting group:
```rust
            NodeKind::Start
            | NodeKind::Prompt { .. }
            | NodeKind::Condition { .. }
            | NodeKind::HumanReview { .. }
            | NodeKind::Wait { .. }
            | NodeKind::Finish { prompt: None, .. } => {}
            NodeKind::AgentTask { .. }
            | NodeKind::Script { .. }
            | NodeKind::Finish { prompt: Some(_), .. } => {
                set.insert(Effect::FsRead);
                set.insert(Effect::FsWrite);
                set.insert(Effect::Network);
                set.insert(Effect::External);
            }
```
- [ ] Run: `cargo test -p apb-engine --test finish_answer_test`; then `cargo test -p apb-core --lib effects`. Expected: pass. Run `cargo build --workspace` to confirm the exhaustive `execute_node` `NodeKind::Finish { .. }` arm still compiles (it stays a defensive no-op; finish is handled in `drive`).
- [ ] Parity check: compare the replaced Finish interception against the ORIGINAL block in git history for the no-prompt path. A finish without a prompt must emit exactly the same event sequence as before this task (if the original did not append `NodeFinished` for finish nodes, do not add one on the no-prompt path). If `execute_finish_answer`'s attempt/fallback loop duplicates a logic block that `execute_node`'s agent_task arm already has, extracting a shared private helper in `node.rs` is allowed and preferred over verbatim duplication (the DRY gate in code-ranker flags it otherwise); keep behavior identical.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

### Task B4: run answer on MCP `run_status`/`run_report` and server run detail

**Files:**
- Modify: `crates/apb-engine/src/progress.rs` (new `run_answer`)
- Modify: `crates/apb-mcp/src/tools.rs` (`run_status`, `run_report` add `answer`)
- Modify: `crates/apb-server/src/lib.rs` (`get_run_handler` adds `answer`)
- Test: new `crates/apb-engine/src/progress.rs` unit test for `run_answer`; extend `crates/apb-mcp/tests/run_tools_test.rs`

**Interfaces (produced):** `apb_engine::progress::run_answer(run_dir: &Path, events: &[Event]) -> Option<String>` - the non-empty output of the succeeded finish node, else `None`. `run_status`/`run_report` (MCP) and the server run-detail JSON gain `"answer": string | null`. `RunSummary` is NOT changed (the list stays lean).

Steps:

- [ ] Write the failing tests. Add to `crates/apb-engine/src/progress.rs` `#[cfg(test)] mod tests`:
```rust
    #[test]
    fn run_answer_is_the_finish_output() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success, prompt: \"c\" }\nedges:\n  - { from: s, to: f }\n",
        )
        .unwrap();
        let events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::NodeFinished { node: "f".into(), status: "succeeded".into(), attempt: 1, output: "THE ANSWER".into() }),
        ];
        assert_eq!(run_answer(&run_dir, &events).as_deref(), Some("THE ANSWER"));
    }

    #[test]
    fn run_answer_none_for_empty_finish() {
        let tmp = tempfile::tempdir().unwrap();
        let run_dir = tmp.path().join("run");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n",
        )
        .unwrap();
        let events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::NodeFinished { node: "f".into(), status: "succeeded".into(), attempt: 1, output: String::new() }),
        ];
        assert_eq!(run_answer(&run_dir, &events), None);
    }
```
Add to `crates/apb-mcp/tests/run_tools_test.rs` (uses the existing `NOAGENT` control-only playbook there, whose finish has no prompt, so `answer` must be JSON null):
```rust
#[test]
fn run_status_carries_answer_key() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let started = playbook_run(dir.path(), "noagent", None, params, None, None, None).unwrap();
    let run_id = started["run_id"].as_str().unwrap();
    let status = run_status(dir.path(), run_id).unwrap();
    assert!(status.get("answer").is_some(), "answer key present");
    assert!(status["answer"].is_null(), "no-prompt finish -> null answer");
}
```
- [ ] Run: `cargo test -p apb-engine --lib run_answer` and `cargo test -p apb-mcp --test run_tools_test run_status_carries_answer_key`. Expected: failures.
- [ ] Add `run_answer` to `crates/apb-engine/src/progress.rs` (after `from_run_dir`):
```rust
/// The run answer (spec B): the non-empty output of the succeeded finish node.
/// Derived purely by fold from the run's events + snapshot; `None` when the
/// finish had no prompt (empty output), the finish has not run, or the snapshot
/// is missing. Multiple finish nodes: the first with a non-empty output wins.
pub fn run_answer(run_dir: &Path, events: &[Event]) -> Option<String> {
    let pb = load_run_playbook(run_dir)?;
    let state = RunState::fold(events);
    for n in &pb.nodes {
        if matches!(n.kind, NodeKind::Finish { .. })
            && state.nodes.get(&n.id).copied() == Some(NodeStatus::Succeeded)
            && let Some(out) = state.outputs.get(&n.id)
            && !out.is_empty()
        {
            return Some(out.clone());
        }
    }
    None
}
```
Note: the Succeeded check matters - a failed finish attempt's `NodeFinished` carries the agent's error message as `output`, and that text must never surface as the run answer. Check how `RunState::fold` tracks per-node status (`state.nodes`) and use the exact field/enum it exposes.
- [ ] In `crates/apb-mcp/src/tools.rs`, add `answer` to the `run_status` JSON. Insert before building the `json!` and add the key:
```rust
    let answer = apb_engine::progress::run_answer(&dir, &events);
```
and add `"answer": answer,` inside the returned `json!({ ... })`. Do the same in `run_report`: after `let progress = ...`, add `let answer = apb_engine::progress::run_answer(&dir, &events);` and add `"answer": answer,` to its base `json!`.
- [ ] In `crates/apb-server/src/lib.rs` `get_run_handler`, after `let progress = ...`, add:
```rust
    let answer = apb_engine::progress::run_answer(&run_dir, &events);
```
and add `"answer": answer,` to the returned `serde_json::json!({ ... })`.
- [ ] Run: `cargo test -p apb-engine --lib run_answer`; `cargo test -p apb-mcp --test run_tools_test`; `cargo test -p apb-server`. Expected: pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

## Part C - sub-playbook node

### Task C1: schema - `QualifiedPlaybookRef`, `NodeKind::Playbook`, validator V22

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`QualifiedPlaybookRef`, `NodeKind::Playbook`, `expected_seconds` arm)
- Modify: `crates/apb-core/src/validate.rs` (`check_playbook_ref` for V22)
- Modify: `crates/apb-core/src/effects.rs` (`inferred` acting-group arm for `Playbook`)
- Modify: `crates/apb-mcp/src/tools.rs` (`node_kind_label` arm)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (`execute_node` defensive arm)
- Test: `#[cfg(test)] mod tests` in `schema.rs`; extend `validate_duration_test.rs`

**Interfaces (produced):**
- `apb_core::schema::QualifiedPlaybookRef { pub id: String, pub scope: ProfileScope }` - two-form YAML (bare string -> `scope: Auto`; object `{ id, scope }`), `deny_unknown_fields` on the object form, always serialized as an object.
- `NodeKind::Playbook { playbook: QualifiedPlaybookRef, instruction: Option<String> }` (`instruction` `#[serde(default)]`).
- V22 (Error): a `playbook` node whose reference id is empty or not a safe segment.
- `Node::expected_seconds()` returns `DEFAULT_TASK_SECONDS` for a `playbook` node.

Steps:

- [ ] Write the failing tests. Append to `crates/apb-core/src/schema.rs` tests:
```rust
    #[test]
    fn playbook_node_parses_both_ref_forms() {
        let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: c1, type: playbook, playbook: child, instruction: "go" }
  - { id: c2, type: playbook, playbook: { id: child, scope: global } }
  - { id: f, type: finish, outcome: success }
edges: []
"#;
        let pb = Playbook::from_yaml(yaml).unwrap();
        match &pb.node("c1").unwrap().kind {
            NodeKind::Playbook { playbook, instruction } => {
                assert_eq!(playbook.id, "child");
                assert_eq!(playbook.scope, crate::profile::ProfileScope::Auto);
                assert_eq!(instruction.as_deref(), Some("go"));
            }
            _ => panic!("c1 not a playbook node"),
        }
        match &pb.node("c2").unwrap().kind {
            NodeKind::Playbook { playbook, .. } => {
                assert_eq!(playbook.scope, crate::profile::ProfileScope::Global);
            }
            _ => panic!("c2 not a playbook node"),
        }
        assert_eq!(pb.node("c1").unwrap().expected_seconds(), 120);
    }
```
And to `crates/apb-core/tests/validate_duration_test.rs`:
```rust
#[test]
fn v22_errors_on_empty_playbook_reference() {
    let yaml = r#"
schema: 2
id: p
name: p
version: 1.0.0
nodes:
  - { id: s, type: start }
  - { id: c, type: playbook, playbook: "" }
  - { id: f, type: finish, outcome: success }
edges:
  - { from: s, to: c }
  - { from: c, to: f }
"#;
    let pb = Playbook::from_yaml(yaml).unwrap();
    let r = validate(&pb, &ValidationContext::default());
    assert!(!r.is_valid());
    assert!(r.issues.iter().any(|i| i.code == "V22" && i.node.as_deref() == Some("c")));
}
```
- [ ] Run: `cargo test -p apb-core --lib playbook_node_parses`; `cargo test -p apb-core --test validate_duration_test v22`. Expected: failures.
- [ ] In `crates/apb-core/src/schema.rs`, add `use crate::profile::ProfileScope;` to the imports (alongside `QualifiedProfileRef`) and insert the ref type just above `pub struct Node`:
```rust
/// A reference to a playbook (spec C): id + scope. Two YAML forms - a bare
/// string (shorthand, `scope: auto`) or an object `{ id, scope }`. Always
/// serialized as an object. Mirrors `QualifiedProfileRef` but keyed by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualifiedPlaybookRef {
    pub id: String,
    pub scope: ProfileScope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaybookRefFull {
    id: String,
    #[serde(default)]
    scope: ProfileScope,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PlaybookRefForm {
    Short(String),
    Full(PlaybookRefFull),
}

impl<'de> Deserialize<'de> for QualifiedPlaybookRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match PlaybookRefForm::deserialize(d)? {
            PlaybookRefForm::Short(id) => Self { id, scope: ProfileScope::Auto },
            PlaybookRefForm::Full(PlaybookRefFull { id, scope }) => Self { id, scope },
        })
    }
}
```
- [ ] Add the `Playbook` variant to `NodeKind` (after `Finish`):
```rust
    Playbook {
        /// The child playbook to run (spec C). `scope: auto` resolves the
        /// parent's origin registry first, then global.
        playbook: QualifiedPlaybookRef,
        /// Template rendered with the parent run's context; the result becomes
        /// the child run's `instruction` (Part A precedence: this explicit value
        /// wins over the child's draft). Absent -> the child falls back to its
        /// own draft.
        #[serde(default)]
        instruction: Option<String>,
    },
```
- [ ] Add the `Playbook` arm to `Node::expected_seconds` (next to the finish arm, before `_ => 0`):
```rust
            NodeKind::Playbook { .. } => crate::duration::DEFAULT_TASK_SECONDS,
```
- [ ] In `crates/apb-core/src/validate.rs`, extend V19 to also warn on a `playbook` node with no estimate. Add `|| matches!(n.kind, NodeKind::Playbook { .. })` to the same `None if ...` guard used in Task B2. Then add `check_playbook_ref`:
```rust
/// V22 (error): a playbook node whose reference id is empty or not a safe path
/// segment. Resolvability of the reference is a gate/adopt concern (the offline
/// validator cannot see other playbooks).
fn check_playbook_ref(playbook: &Playbook, r: &mut ValidationReport) {
    for n in &playbook.nodes {
        if let NodeKind::Playbook { playbook: pref, .. } = &n.kind
            && (pref.id.is_empty() || !crate::registry::is_safe_segment(&pref.id))
        {
            r.error(
                "V22",
                Some(&n.id),
                format!("playbook node `{}` has an empty or invalid playbook reference", n.id),
            );
        }
    }
}
```
Wire it into `validate` right after `check_finish(playbook, &mut r);`:
```rust
    check_playbook_ref(playbook, &mut r); // V22
```
- [ ] In `crates/apb-core/src/effects.rs`, add `NodeKind::Playbook { .. }` to the acting group of `inferred` (a child run can do anything):
```rust
            NodeKind::AgentTask { .. }
            | NodeKind::Script { .. }
            | NodeKind::Finish { prompt: Some(_), .. }
            | NodeKind::Playbook { .. } => {
```
- [ ] In `crates/apb-mcp/src/tools.rs`, add the `node_kind_label` arm: `Playbook { .. } => "playbook",`.
- [ ] In `crates/apb-engine/src/scheduler/node.rs`, add a defensive arm to `execute_node`'s match (playbook nodes are handled in `drive`, never here):
```rust
        NodeKind::Playbook { .. } => Err(EngineError::Invalid(format!(
            "node `{node_id}` (playbook) must be handled by drive"
        ))),
```
- [ ] Run: `cargo build --workspace` (surfaces any remaining non-exhaustive `NodeKind` match), then `cargo test -p apb-core`. Expected: build clean, tests pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

### Task C2: policy - recursive permit, cycle detection, untrusted child, effects union

**Files:**
- Modify: `crates/apb-engine/src/run_config.rs` (define `ChildExpectation`)
- Modify: `crates/apb-mcp/src/policy.rs` (`RunPermit.children`, recursive walk in `check_run`, cycle + untrusted-child errors)
- Test: new `crates/apb-mcp/tests/subplaybook_policy_test.rs`

**Interfaces (produced):**
- `apb_engine::run_config::ChildExpectation { pub id: String, pub scope: String, pub version: String, pub playbook_digest: String, pub profile_bundles: BTreeMap<String, String>, pub children: BTreeMap<String, ChildExpectation> }` - derives `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`. `children` keyed by the child's own playbook-node id (recursive pin).
- `policy::RunPermit` gains `pub children: BTreeMap<String, ChildExpectation>` keyed by THIS playbook's playbook-node id.
- Refusals: `sub_playbook_cycle` (names the cycle), and the existing `untrusted_profile_requires_acknowledge` / `profile_unresolved` extend to children.

Defined here (do not re-derive elsewhere):
- `scope: "project" | "global"` string on `ChildExpectation`; a `playbook` node's `scope: auto` resolves the parent origin registry first, then global, and the resolved scope is recorded.
- Cycle set = the transitive closure of `(origin, id)` pairs across pinned children; a repeated pair on the current path is a cycle.
- Effects: `check_run`'s consent covers `apb_core::effects::effective(parent) UNION each pinned child's effective effects (recursively)`; the union is reported in the refusal path effects the same way the parent's are (surfaced by the caller through the existing effects channel).

Steps:

- [ ] Write the failing test. Create `crates/apb-mcp/tests/subplaybook_policy_test.rs`:
```rust
use apb_core::registry::init_project;
use apb_core::scope::{Origin, PlaybookRef};
use apb_core::trust::TrustStore;
use apb_mcp::policy::check_run;
use std::fs;
use std::path::Path;

fn write_pb(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks").join(id).join("current"), "1.0.0").unwrap();
}

fn approve(root: &Path, id: &str) {
    // Approve a playbook's own digest so trust is not the thing under test.
    let reg = apb_core::registry::Registry::open(root).unwrap();
    let loaded = reg.load(id, None).unwrap();
    TrustStore::load()
        .approve(&apb_core::scope::digest_str(&loaded.yaml))
        .unwrap();
}

const PARENT: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
const CHILD_OK: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: f }\n";
const CHILD_CYCLE: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: parent }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";

fn wref() -> PlaybookRef {
    PlaybookRef { origin: Origin::Project { workspace_id: None }, id: "parent".into(), version: None }
}

#[test]
fn recursive_permit_pins_child() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_OK);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");

    let permit = check_run(dir.path(), &wref(), false, false).expect("permit");
    let child = permit.children.get("c").expect("child pinned at node c");
    assert_eq!(child.id, "child");
    assert_eq!(child.version, "1.0.0");
    assert!(!child.playbook_digest.is_empty());
}

#[test]
fn cycle_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD_CYCLE);
    approve(dir.path(), "parent");
    approve(dir.path(), "child");

    let refusal = check_run(dir.path(), &wref(), false, false).unwrap_err();
    assert_eq!(refusal["policy"], "sub_playbook_cycle");
}
```
Confirm `TrustStore::approve` (or the equivalent approve method) name by reading `crates/apb-core/src/trust.rs`; use whatever method the trust store exposes to mark a digest approved, and mirror it in the helper.
- [ ] Run: `cargo test -p apb-mcp --test subplaybook_policy_test`. Expected: failure (`children` field missing, no cycle handling).
- [ ] Add `ChildExpectation` to `crates/apb-engine/src/run_config.rs` (top-level, after the imports):
```rust
/// Anti-TOCTOU pin of one sub-playbook child, verified in the parent's policy
/// gate and handed to the engine verbatim (spec C). The engine starts the child
/// against this pinned version and rejects any digest/bundle drift. `children`
/// recursively pins the child's own sub-playbook nodes, keyed by the child's
/// node id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildExpectation {
    pub id: String,
    pub scope: String,
    pub version: String,
    pub playbook_digest: String,
    #[serde(default)]
    pub profile_bundles: BTreeMap<String, String>,
    #[serde(default)]
    pub children: BTreeMap<String, ChildExpectation>,
}
```
- [ ] Add the `children` field to `RunPermit` in `crates/apb-mcp/src/policy.rs`:
```rust
#[derive(Debug, Clone)]
pub struct RunPermit {
    pub playbook_digest: String,
    pub profile_bundles: std::collections::BTreeMap<String, String>,
    /// Verified sub-playbook pins, keyed by this playbook's playbook-node id
    /// (spec C). The engine receives it verbatim and rejects drift.
    pub children: std::collections::BTreeMap<String, apb_engine::run_config::ChildExpectation>,
}
```
- [ ] Add a recursive resolver to `crates/apb-mcp/src/policy.rs`. It walks the sub-playbook nodes of a loaded playbook, resolving each child's version/digest/bundles and its own children, tracking the visited `(scope, id)` path for cycle detection and accumulating untrusted keys:
```rust
use apb_engine::run_config::ChildExpectation;

/// Recursively collects and verifies the sub-playbook pins of `playbook`.
/// `origin` is where THIS playbook's definition came from (drives `scope: auto`
/// resolution of its children). `path` holds the `(scope, id)` pairs on the
/// current branch for cycle detection. On an untrusted child bundle the key is
/// pushed to `untrusted` (the caller turns a non-empty list into the standard
/// refusal). Returns the node-id -> ChildExpectation map for `playbook`.
fn collect_children(
    root: &Path,
    playbook: &Playbook,
    origin: &Origin,
    acknowledge_untrusted: bool,
    path: &mut Vec<(String, String)>,
    untrusted: &mut Vec<String>,
) -> Result<std::collections::BTreeMap<String, ChildExpectation>, Value> {
    use apb_core::profile::ProfileScope;
    let mut out = std::collections::BTreeMap::new();
    for n in &playbook.nodes {
        let NodeKind::Playbook { playbook: pref, .. } = &n.kind else {
            continue;
        };
        // Resolve scope: an explicit scope pins it; `auto` prefers the parent's
        // origin, then global.
        let child_origin = match pref.scope {
            ProfileScope::Global => Origin::Global,
            ProfileScope::Project => Origin::Project { workspace_id: None },
            ProfileScope::Auto => origin.clone(),
        };
        let child_ref = PlaybookRef {
            origin: child_origin.clone(),
            id: pref.id.clone(),
            version: None,
        };
        let resolved = apb_core::store::resolve(root, &child_ref)
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        let scope_str = if matches!(child_origin, Origin::Global) { "global" } else { "project" };
        let pair = (scope_str.to_string(), resolved.id.clone());
        if path.contains(&pair) {
            let mut cycle: Vec<String> = path.iter().map(|(s, i)| format!("{s}/{i}")).collect();
            cycle.push(format!("{scope_str}/{}", resolved.id));
            return Err(json!({ "policy": "sub_playbook_cycle", "cycle": cycle }));
        }
        // Load the child definition to walk its own children + collect bundles.
        let reg = Registry::open_dir(&resolved.definition_parent)
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        let loaded = reg
            .load(&resolved.id, Some(&resolved.version))
            .map_err(|e| json!({ "policy": "not_found", "detail": e.to_string() }))?;
        let worigin = if matches!(child_origin, Origin::Global) {
            PlaybookOrigin::Global
        } else {
            PlaybookOrigin::Project
        };
        // Child profile bundles (nodes + finish-with-prompt), trust-checked.
        let mut bundles = std::collections::BTreeMap::new();
        let store = TrustStore::load();
        for r in collect_profile_refs(&loaded.playbook, false) {
            match profile_store::compute_bundle(root, worigin, &r) {
                Ok((lp, _pairs, bundle)) => {
                    let key = format!("{}/{}", profile_store::scope_str(lp.scope), lp.name);
                    if !acknowledge_untrusted && !store.is_approved(&bundle) && !untrusted.contains(&key) {
                        untrusted.push(key.clone());
                    }
                    bundles.insert(key, bundle);
                }
                Err(e) => return Err(json!({ "policy": "profile_unresolved", "detail": e.to_string() })),
            }
        }
        // Recurse into the child's own sub-playbook nodes.
        path.push(pair);
        let grand = collect_children(root, &loaded.playbook, &child_origin, acknowledge_untrusted, path, untrusted)?;
        path.pop();

        out.insert(
            n.id.clone(),
            ChildExpectation {
                id: resolved.id.clone(),
                scope: scope_str.to_string(),
                version: resolved.version.clone(),
                playbook_digest: resolved.digest.clone(),
                profile_bundles: bundles,
                children: grand,
            },
        );
    }
    Ok(out)
}
```
- [ ] In `check_run`, after `check_profile_bundles` succeeds and before `check_requires`, add the recursive child walk and fold it into the permit:
```rust
    // Sub-playbook pins (spec C): walk the reference tree in the same gate pass,
    // detect cycles, and trust-check each child's bundles alongside the parent's.
    let mut cpath: Vec<(String, String)> = vec![(
        if matches!(wref.origin, Origin::Global) { "global" } else { "project" }.to_string(),
        wref.id.clone(),
    )];
    let mut child_untrusted: Vec<String> = Vec::new();
    let children = collect_children(
        root,
        &loaded.playbook,
        &wref.origin,
        acknowledge_untrusted,
        &mut cpath,
        &mut child_untrusted,
    )?;
    if !child_untrusted.is_empty() {
        return Err(json!({
            "policy": "untrusted_profile_requires_acknowledge",
            "profiles": child_untrusted,
            "detail": "a sub-playbook binds an untrusted profile bundle; run again with acknowledge_untrusted: true after user confirmation",
        }));
    }
```
Then extend the returned permit:
```rust
    Ok(RunPermit {
        playbook_digest: digest,
        profile_bundles,
        children,
    })
```
- [ ] Effects union (spec C): read how `check_run` computes and surfaces the run's effective effects for user consent today (`apb_core::effects::effective` plus wherever the result reaches the caller or a refusal). Extend that exact spot so the consented set is the union of the parent's effective effects and every pinned child's effective effects, recursively (load each pinned child's playbook once during `collect_children` and accumulate, rather than re-resolving afterwards). Add a test to `subplaybook_policy_test.rs`: a parent whose own nodes are control-only but whose child contains an `agent_task` must surface the child's acting effects (fs_write/network/external) in the parent's gate output; assert on the same field/shape the existing effects tests use.
- [ ] Run: `cargo test -p apb-mcp --test subplaybook_policy_test`. Expected: pass. Fix all call sites that build `RunPermit` literally (grep `RunPermit {` across the workspace) to add `children` (any test constructing it directly).
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

### Task C3: engine plumbing - `parent_run`, `depth`, `expected_children`, `ChildRunStarted`, manifest carry

**Files:**
- Modify: `crates/apb-engine/src/run_config.rs` (`RunConfig` gains `parent_run`, `depth`, `expected_children`)
- Modify: `crates/apb-engine/src/scheduler.rs` (`RunOptions` gains `parent_run`, `depth`, `expected_children`; `MAX_SUBPLAYBOOK_DEPTH`)
- Modify: `crates/apb-engine/src/scheduler/prepare.rs` (`prepare_run_target` copies the three into `RunConfig`)
- Modify: `crates/apb-engine/src/event.rs` (`EventPayload::ChildRunStarted`)
- Modify: `crates/apb-engine/src/state.rs` (`fold` no-op arm)
- Test: extend `crates/apb-engine/tests/event_test.rs` (or new `child_run_event_test.rs`)

**Interfaces (produced):**
- `RunConfig` gains `#[serde(default)] pub parent_run: Option<String>`, `#[serde(default)] pub depth: usize`, `#[serde(default)] pub expected_children: Option<BTreeMap<String, ChildExpectation>>`.
- `RunOptions` gains `pub parent_run: Option<String>`, `pub depth: usize`, `pub expected_children: Option<BTreeMap<String, ChildExpectation>>`.
- `apb_engine::scheduler::MAX_SUBPLAYBOOK_DEPTH: usize = 5`.
- `EventPayload::ChildRunStarted { #[serde(default)] node_id: String, #[serde(default)] run_id: String }`.

Steps:

- [ ] Write the failing test. Create `crates/apb-engine/tests/child_run_event_test.rs`:
```rust
use apb_engine::event::{Event, EventPayload};

#[test]
fn child_run_started_roundtrips_and_defaults() {
    let e = Event {
        seq: 3,
        ts: 0,
        payload: EventPayload::ChildRunStarted { node_id: "c".into(), run_id: "child-1".into() },
    };
    let line = serde_json::to_string(&e).unwrap();
    assert!(line.contains("\"type\":\"child_run_started\""));
    // Old logs with a bare variant still deserialize (all fields default).
    let bare: Event = serde_json::from_str("{\"seq\":0,\"ts\":0,\"type\":\"child_run_started\"}").unwrap();
    match bare.payload {
        EventPayload::ChildRunStarted { node_id, run_id } => {
            assert!(node_id.is_empty() && run_id.is_empty());
        }
        _ => panic!("wrong variant"),
    }
}
```
- [ ] Run: `cargo test -p apb-engine --test child_run_event_test`. Expected: failure (variant missing).
- [ ] Add the variant to `crates/apb-engine/src/event.rs` (after `RunProgress`, before `EnvironmentDriftAccepted`):
```rust
    /// A sub-playbook node started a full child run (spec C). Written by drive
    /// (via run_playbook_node) before it drives the child, so a resume can
    /// reattach to a still-running child by its `run_id`. Fields default so old
    /// logs read unchanged.
    ChildRunStarted {
        #[serde(default)]
        node_id: String,
        #[serde(default)]
        run_id: String,
    },
```
- [ ] Add the no-op fold arm to `crates/apb-engine/src/state.rs` `RunState::fold` (next to `RunProgress`):
```rust
                // A child-run marker is an audit record; the node's own status
                // events carry the run-state effect.
                EventPayload::ChildRunStarted { .. } => {}
```
- [ ] Extend `RunConfig` in `crates/apb-engine/src/run_config.rs` (after the `overrides` field):
```rust
    /// The parent run id, when this run is a Part C sub-playbook child.
    #[serde(default)]
    pub parent_run: Option<String>,
    /// Sub-playbook nesting depth (spec C). A top-level run is 0; each child is
    /// parent depth + 1. Enforced against `MAX_SUBPLAYBOOK_DEPTH`.
    #[serde(default)]
    pub depth: usize,
    /// Verified sub-playbook pins from the policy gate, keyed by this run's
    /// playbook-node id (spec C). `None` on the CLI path (no gate) -> children
    /// resolve live without a drift check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_children: Option<BTreeMap<String, ChildExpectation>>,
```
- [ ] Extend `RunOptions` in `crates/apb-engine/src/scheduler.rs` (after `expected_profile_bundles`), and add the const near the top of the file:
```rust
    /// Parent run id when this run is a sub-playbook child (spec C).
    pub parent_run: Option<String>,
    /// Sub-playbook nesting depth of THIS run (0 for a top-level run).
    pub depth: usize,
    /// Verified sub-playbook pins from the gate, keyed by playbook-node id.
    pub expected_children: Option<BTreeMap<String, crate::run_config::ChildExpectation>>,
```
```rust
/// Defense-in-depth backstop for sub-playbook nesting (spec C). A child that
/// would exceed this depth fails its parent node.
pub const MAX_SUBPLAYBOOK_DEPTH: usize = 5;
```
- [ ] In `crates/apb-engine/src/scheduler/prepare.rs`, add the three fields to the `RunConfig { ... }` literal in `prepare_run_target`:
```rust
        parent_run: opts.parent_run.clone(),
        depth: opts.depth,
        expected_children: opts.expected_children.clone(),
```
- [ ] Run: `cargo test -p apb-engine --test child_run_event_test`; then `cargo build -p apb-engine` to surface any `RunOptions { .. }` struct-literal callers needing the new fields (the tools/server pass `..Default::default()` in most; add the fields to any exhaustive literal). Expected: pass/clean.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

### Task C4: engine - drive executes a `playbook` node (start / reattach / abort / depth)

**Files:**
- Modify: `crates/apb-engine/src/scheduler/node.rs` (new `run_playbook_node`, `latest_child_run`, `run_is_terminal`)
- Modify: `crates/apb-engine/src/scheduler.rs` (drive arm for `NodeKind::Playbook`; `run_cancel` recurses into children; `abort_children`)
- Test: new `crates/apb-engine/tests/subplaybook_run_test.rs`

**Interfaces (produced):** `pub(crate) fn run_playbook_node(root, run_dir, log, playbook, cfg, run_id, node_id, child_ref, node_instruction) -> Result<(NodeStatus, String), EngineError>`. Child succeeded -> `(Succeeded, child_answer_or_empty)`; child failed/aborted -> `(Failed, diagnostic naming the child run id)`. `run_cancel` posts Abort to the run and recursively to its non-terminal children.

**Notes on the deadlock hazard (read before coding):** the parent holds the workdir lock (`WorkdirGuard`, `.apb/workdir.lock`, keyed by PID) for its whole synchronous run. The child runs IN THE SAME PROCESS from inside the parent's `drive`. `acquire` keys the lock by PID and treats a live-PID lock as busy, so a child that tried to `acquire` would get `WorkdirBusy` (not a deadlock, but a hard failure). Therefore the child is always started with `allow_shared_workdir: true` - `acquire` then returns `None` immediately and no second lock is taken. A `playbook` node is NOT `is_agent_or_script`, so it never enters the parallel fast path; it always runs on the drive thread, so `run_playbook_node` may take `&mut EventLog` and append `ChildRunStarted` itself (single writer preserved).

Steps:

- [ ] Write the failing test. Create `crates/apb-engine/tests/subplaybook_run_test.rs` (child is control-only: a prompt echoing the instruction, and a finish WITHOUT a prompt, so the whole tree runs with no agent):
```rust
use apb_core::registry::init_project;
use apb_engine::event::{read_all, EventPayload};
use apb_engine::scheduler::{RunOptions, run};
use apb_engine::state::{RunState, RunStatus};
use std::fs;
use std::path::Path;

fn write_pb(root: &Path, id: &str, yaml: &str) {
    let vdir = root.join(".apb/playbooks").join(id).join("1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("playbook.yaml"), yaml).unwrap();
    fs::write(root.join(".apb/playbooks").join(id).join("current"), "1.0.0").unwrap();
}

const PARENT: &str = "schema: 2\nid: parent\nname: parent\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child, instruction: \"child input\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: f }\n";
// Child: prompt renders the run instruction, finish WITHOUT a prompt -> empty answer.
const CHILD: &str = "schema: 2\nid: child\nname: child\nversion: 1.0.0\nnodes:\n  - { id: s, type: start }\n  - { id: n, type: prompt, prompt: \"{{run.instruction}}\" }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: n }\n  - { from: n, to: f }\n";

#[test]
fn parent_runs_child_and_records_child_run_started() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    write_pb(dir.path(), "parent", PARENT);
    write_pb(dir.path(), "child", CHILD);

    let res = run(dir.path(), "parent", None, RunOptions::default()).expect("parent runs");
    assert_eq!(res.outcome, RunStatus::Succeeded);

    let events = read_all(&dir.path().join(".apb/runs").join(&res.run_id)).unwrap();
    let started: Vec<&str> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if node_id == "c" => Some(run_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1, "one child run started for node c");

    // The child run persisted with parent_run set, and it reached a terminal state.
    let child_dir = dir.path().join(".apb/runs").join(started[0]);
    let child_cfg = apb_engine::run_config::read_run_config(&child_dir).unwrap();
    assert_eq!(child_cfg.parent_run.as_deref(), Some(res.run_id.as_str()));
    assert_eq!(child_cfg.instruction.as_deref(), Some("child input"));
    let child_state = RunState::fold(&read_all(&child_dir).unwrap());
    assert_eq!(child_state.run_status, RunStatus::Succeeded);

    // The parent node `c` succeeded (empty answer: child finish has no prompt).
    let parent_state = RunState::fold(&events);
    assert_eq!(parent_state.outputs.get("c").map(|s| s.as_str()), Some(""));
}
```
- [ ] Run: `cargo test -p apb-engine --test subplaybook_run_test`. Expected: failure (playbook node hits the defensive `execute_node` error).
- [ ] Add helpers + `run_playbook_node` to `crates/apb-engine/src/scheduler/node.rs`:
```rust
/// The run id of the latest ChildRunStarted for `node_id`, if any.
pub(crate) fn latest_child_run(events: &[Event], node_id: &str) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::ChildRunStarted { node_id: n, run_id } if n == node_id => Some(run_id.clone()),
        _ => None,
    })
}

/// Whether a run directory has reached a terminal run status.
pub(crate) fn run_is_terminal(root: &Path, run_id: &str) -> bool {
    let dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&dir).unwrap_or_default();
    matches!(
        RunState::fold(&events).run_status,
        RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
    )
}

/// Executes a `playbook` node (spec C): starts (or, on resume, reattaches to) a
/// full child run and maps its terminal state to this node's status/output. The
/// child runs in-process, synchronously, with `allow_shared_workdir: true` (the
/// parent already holds the workdir lock; see the module notes). ChildRunStarted
/// is appended here (drive thread, single writer) BEFORE the child is driven.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_playbook_node(
    root: &Path,
    run_dir: &Path,
    log: &mut EventLog,
    playbook: &Playbook,
    cfg: &RunConfig,
    run_id: &str,
    node_id: &str,
    child_ref: &apb_core::schema::QualifiedPlaybookRef,
    node_instruction: Option<&str>,
) -> Result<(NodeStatus, String), EngineError> {
    // Depth backstop.
    if cfg.depth + 1 > crate::MAX_SUBPLAYBOOK_DEPTH {
        return Ok((
            NodeStatus::Failed,
            format!("sub-playbook depth limit ({}) exceeded", crate::MAX_SUBPLAYBOOK_DEPTH),
        ));
    }

    // Resume reattach: a still-running child from a prior ChildRunStarted is
    // resumed, not restarted (the event log is the source of truth).
    let events = read_all(run_dir)?;
    if let Some(existing) = latest_child_run(&events, node_id)
        && !run_is_terminal(root, &existing)
    {
        let res = resume(root, &existing, None)?;
        return Ok(map_child_outcome(root, &existing, res.outcome));
    }

    // Render the node instruction with the parent context; the result is the
    // child's explicit instruction (Part A precedence). Absent -> None (child
    // falls back to its own draft).
    let child_instruction = match node_instruction {
        Some(t) => {
            let context = build_context_for_render(run_dir, &read_all(run_dir)?)?;
            let hooks: BTreeMap<String, String> = crate::hooks::read_hooks(run_dir)?
                .into_iter()
                .map(|(k, secret)| (k, crate::hooks::hook_path(run_id, &secret)))
                .collect();
            let state = RunState::fold(&read_all(run_dir)?);
            Some(render(t, &cfg.params, cfg.instruction.as_deref(), &state.outputs, &state.reviews, &hooks, &context))
        }
        None => None,
    };

    // Resolve the child reference (scope: auto prefers the parent origin, then
    // global). Pins from the gate (cfg.expected_children) enforce anti-TOCTOU.
    use apb_core::profile::ProfileScope;
    use apb_core::scope::{Origin, PlaybookRef};
    let pin = cfg.expected_children.as_ref().and_then(|m| m.get(node_id));
    let child_origin = match child_ref.scope {
        ProfileScope::Global => Origin::Global,
        ProfileScope::Project => Origin::Project { workspace_id: None },
        ProfileScope::Auto => Origin::Project { workspace_id: None },
    };
    let wref = PlaybookRef {
        origin: child_origin,
        id: child_ref.id.clone(),
        version: pin.map(|p| p.version.clone()),
    };
    let resolved = apb_core::store::resolve(root, &wref)
        .map_err(|e| EngineError::Invalid(format!("sub-playbook `{}`: {e}", child_ref.id)))?;

    let opts = RunOptions {
        instruction: child_instruction,
        allow_shared_workdir: true,
        parent_run: Some(run_id.to_string()),
        depth: cfg.depth + 1,
        expected_digest: pin.map(|p| p.playbook_digest.clone()),
        expected_profile_bundles: pin.map(|p| p.profile_bundles.clone()),
        expected_children: pin.map(|p| p.children.clone()),
        ..Default::default()
    };

    // Prepare (get the run id) -> record ChildRunStarted -> drive to terminal.
    let t = PrepareTarget {
        definition_parent: resolved.definition_parent.clone(),
        execution_root: resolved.execution_root.clone(),
        origin_label: resolved.origin_label,
    };
    let mut cp = prepare_run_target(&t, &resolved.id, Some(&resolved.version), opts)?;
    let child_run_id = cp.run_id.clone();
    log.append(EventPayload::ChildRunStarted {
        node_id: node_id.to_string(),
        run_id: child_run_id.clone(),
    })?;
    let res = drive(
        cp.playbook.clone(),
        &cp.run_dir,
        &resolved.execution_root,
        &mut cp.log,
        &cp.cfg,
        cp.start_node.clone(),
        cp.run_id.clone(),
        RunMode::Autonomous,
        cp.supervisor_expected,
    )?;
    Ok(map_child_outcome(root, &child_run_id, res.outcome))
}

/// Maps a child run's terminal status to the parent node's (status, output).
fn map_child_outcome(root: &Path, child_run_id: &str, outcome: RunStatus) -> (NodeStatus, String) {
    match outcome {
        RunStatus::Succeeded => {
            let dir = root.join(".apb/runs").join(child_run_id);
            let answer = crate::progress::run_answer(&dir, &read_all(&dir).unwrap_or_default())
                .unwrap_or_default();
            (NodeStatus::Succeeded, answer)
        }
        other => (
            NodeStatus::Failed,
            format!("sub-playbook child run `{child_run_id}` ended {}", other.as_str()),
        ),
    }
}
```
Note: `PrepareTarget`, `prepare_run_target`, `drive`, and `Prepared`'s fields are in scope via `use super::*` (`node` is a submodule of `scheduler`, and `Prepared` is a private struct of that module, so its fields are reachable). Match the `drive(...)` argument list to the exact one the existing `run` entry point uses; if `Prepared`'s real field names differ from `cp.playbook`/`cp.run_dir`/`cp.log`/`cp.cfg`/`cp.start_node`/`cp.supervisor_expected`, follow the real names.
- [ ] Add the drive arm in `crates/apb-engine/src/scheduler.rs`. In the `let (status, output) = if let NodeKind::HumanReview ... else if ... else { execute_node }` chain, add a `playbook` arm before the final `else`:
```rust
        } else if let NodeKind::Playbook {
            playbook: child_ref,
            instruction: node_instr,
        } = &node_kind
        {
            steps += 1;
            log.append(EventPayload::NodeStarted {
                node: current.clone(),
                attempt: 1,
            })?;
            run_playbook_node(
                root,
                run_dir,
                log,
                &playbook,
                cfg,
                &run_id,
                &current,
                child_ref,
                node_instr.as_deref(),
            )?
        } else {
```
- [ ] Make `run_cancel` recurse into children. In `crates/apb-engine/src/scheduler.rs`, after `crate::control::post_control(&run_dir, Control::Abort { .. })?;`, add `abort_children(root, run_id)?;` and define:
```rust
/// Posts Abort to every non-terminal sub-playbook child of `run_id`, recursively
/// (spec C). Best-effort per child; a child that no longer exists is skipped.
/// This is how an operator abort of the parent reaches a child that is blocking
/// the parent (e.g. a child paused on human_review): the child's own drive loop
/// scans its control.jsonl at every iteration boundary and returns Aborted, which
/// the parent maps to a failed node.
fn abort_children(root: &Path, run_id: &str) -> Result<(), EngineError> {
    let run_dir = root.join(".apb/runs").join(run_id);
    let events = read_all(&run_dir)?;
    for e in &events {
        if let EventPayload::ChildRunStarted { run_id: child, .. } = &e.payload {
            let child_dir = root.join(".apb/runs").join(child);
            if child_dir.is_dir()
                && !matches!(
                    RunState::fold(&read_all(&child_dir)?).run_status,
                    RunStatus::Succeeded | RunStatus::Failed | RunStatus::Aborted
                )
            {
                let _ = crate::control::post_control(&child_dir, Control::Abort { reason: "parent aborted".into() });
                abort_children(root, child)?;
            }
        }
    }
    Ok(())
}
```
- [ ] Run: `cargo test -p apb-engine --test subplaybook_run_test`. Expected: pass. Then `cargo test -p apb-engine` to confirm no regression in resume/abort suites.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

### Task C5: progress child-credit, runs list `parent_run`, MCP `run_status.children`

**Files:**
- Modify: `crates/apb-engine/src/progress.rs` (factor `weighted`; `from_run_dir` credits a running child)
- Modify: `crates/apb-engine/src/scheduler.rs` (`RunSummary.parent_run`; `list_runs` fills it)
- Modify: `crates/apb-mcp/src/tools.rs` (`run_status` adds `children`)
- Test: extend `crates/apb-engine/src/progress.rs` tests; extend `crates/apb-mcp/tests/run_tools_test.rs`

**Interfaces (produced):**
- `RunSummary` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub parent_run: Option<String>` (from the child's `RunConfig.parent_run`).
- `run_status` (MCP) gains `"children": [{ "node_id", "run_id", "status" }]`.
- `progress::from_run_dir` adds fractional credit `child_percent * expected_seconds(node)` for a RUNNING `playbook` node whose child is non-terminal (the pure `compute` fold is untouched).

Steps:

- [ ] Write the failing tests. Add to `crates/apb-engine/src/progress.rs` tests:
```rust
    #[test]
    fn running_child_contributes_fractional_credit() {
        // A parent with one playbook node (expected 100s) plus a 100s task, so a
        // half-done child contributes 50s of 200s -> 25 percent.
        let tmp = tempfile::tempdir().unwrap();
        let parent_dir = tmp.path().join(".apb/runs/parent-1");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::write(
            parent_dir.join("playbook.yaml"),
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: c, type: playbook, playbook: child, expected_duration: 100 }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: c }\n  - { from: c, to: a }\n  - { from: a, to: f }\n",
        )
        .unwrap();
        // A child run at 50 percent (one 100s task node done of two).
        let child_dir = tmp.path().join(".apb/runs/child-1");
        std::fs::create_dir_all(&child_dir).unwrap();
        std::fs::write(
            child_dir.join("playbook.yaml"),
            "schema: 2\nid: child\nname: child\nversion: 1.0.0\ndefaults: { profile: x }\nnodes:\n  - { id: s, type: start }\n  - { id: a, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: b, type: agent_task, prompt: hi, expected_duration: 100 }\n  - { id: f, type: finish, outcome: success }\nedges:\n  - { from: s, to: a }\n  - { from: a, to: b }\n  - { from: b, to: f }\n",
        )
        .unwrap();
        std::fs::write(
            child_dir.join("events.jsonl"),
            "{\"seq\":0,\"ts\":0,\"type\":\"run_started\",\"playbook\":\"child\",\"version\":\"1.0.0\"}\n{\"seq\":1,\"ts\":0,\"type\":\"node_finished\",\"node\":\"a\",\"status\":\"succeeded\",\"attempt\":1,\"output\":\"\"}\n{\"seq\":2,\"ts\":0,\"type\":\"node_started\",\"node\":\"b\",\"attempt\":1}\n",
        )
        .unwrap();
        let parent_events = vec![
            ev(0, EventPayload::RunStarted { playbook: "p".into(), version: "1.0.0".into() }),
            ev(1, EventPayload::NodeStarted { node: "c".into(), attempt: 1 }),
            ev(2, EventPayload::ChildRunStarted { node_id: "c".into(), run_id: "child-1".into() }),
        ];
        // Root is the temp dir; from_run_dir must find child-1 under root/.apb/runs.
        let p = from_run_dir_with_root(tmp.path(), &parent_dir, &parent_events).unwrap();
        assert_eq!(p.percent, 25);
    }
```
Because the enrichment needs the run root to locate child dirs, add a root-aware helper (below) and keep `from_run_dir` delegating to it with the run dir's grandparent (`.apb/runs/..`) as root.
- [ ] Run: `cargo test -p apb-engine --lib running_child_contributes`. Expected: failure (no helper / no child credit).
- [ ] Refactor `crates/apb-engine/src/progress.rs`: extract the weighted totals so both `compute` and enrichment share them.
```rust
/// Weighted (done, total) seconds for the run, the raw numerator/denominator
/// behind `compute`'s percent. Pure fold - no child awareness.
fn weighted(playbook: &Playbook, events: &[Event]) -> (u128, u128) {
    // Move the existing done/total accumulation loop of `compute` into here and
    // return (done, total). `compute` then calls `weighted` for its percent and
    // keeps computing label/waiting/plan_key exactly as before.
    // ... (identical body to the current accumulation) ...
}
```
Have `compute` call `let (done, total) = weighted(playbook, events);` for its percent while retaining the label/waiting/plan_key computation. Then add the root-aware enrichment:
```rust
/// Progress for a run dir with child credit (spec C): base weighted totals plus,
/// for each RUNNING `playbook` node whose latest child is non-terminal, a
/// fractional credit `child_percent/100 * expected_seconds(node)` added to done.
/// The pure `compute` fold stays untouched; this enrichment lives here.
pub fn from_run_dir_with_root(root: &Path, run_dir: &Path, events: &[Event]) -> Option<ProgressSummary> {
    let pb = load_run_playbook(run_dir)?;
    let mut summary = compute(&pb, events);
    let (done, total) = weighted(&pb, events);
    if total == 0 {
        return Some(summary);
    }
    let state = RunState::fold(events);
    let mut extra: u128 = 0;
    for n in &pb.nodes {
        if !matches!(n.kind, NodeKind::Playbook { .. }) {
            continue;
        }
        // A node currently Running with a non-terminal child.
        if state.nodes.get(&n.id).copied() != Some(NodeStatus::Running) {
            continue;
        }
        let Some(child) = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ChildRunStarted { node_id, run_id } if node_id == &n.id => Some(run_id.clone()),
            _ => None,
        }) else {
            continue;
        };
        let child_dir = root.join(".apb/runs").join(&child);
        let child_events = crate::event::read_all(&child_dir).unwrap_or_default();
        if let Some(cp) = from_run_dir_with_root(root, &child_dir, &child_events) {
            extra += (cp.percent as u128) * (n.expected_seconds() as u128) / 100;
        }
    }
    let enriched = (done + extra).min(total);
    summary.percent = (enriched.saturating_mul(100) / total).min(100) as u8;
    if matches!(state.run_status, RunStatus::Succeeded) {
        summary.percent = 100;
    }
    Some(summary)
}
```
Change the existing `from_run_dir` to delegate:
```rust
pub fn from_run_dir(run_dir: &Path, events: &[Event]) -> Option<ProgressSummary> {
    // The run root is `<...>/.apb/runs/<id>` -> two parents up is the project root.
    let root = run_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or(run_dir);
    from_run_dir_with_root(root, run_dir, events)
}
```
- [ ] Add `parent_run` to `RunSummary` in `crates/apb-engine/src/scheduler.rs`:
```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run: Option<String>,
```
Fill it in `list_runs` (inside the loop, before pushing): `let parent_run = crate::run_config::read_run_config(&entry.path()).ok().and_then(|c| c.parent_run);` and add `parent_run,` to the `RunSummary { .. }` literal.
- [ ] Add `children` to `run_status` in `crates/apb-mcp/src/tools.rs`. Before the `json!`, build:
```rust
    let children: Vec<Value> = events
        .iter()
        .filter_map(|e| match &e.payload {
            apb_engine::event::EventPayload::ChildRunStarted { node_id, run_id } => {
                let child_dir = dir.parent().map(|p| p.join(run_id));
                let status = child_dir
                    .and_then(|d| read_all(&d).ok())
                    .map(|ev| RunState::fold(&ev).run_status.as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                Some(json!({ "node_id": node_id, "run_id": run_id, "status": status }))
            }
            _ => None,
        })
        .collect();
```
and add `"children": children,` to the returned `json!`.
- [ ] Add an MCP test to `crates/apb-mcp/tests/run_tools_test.rs`:
```rust
#[test]
fn run_status_children_empty_for_childless_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let started = playbook_run(dir.path(), "noagent", None, params, None, None, None).unwrap();
    let status = run_status(dir.path(), started["run_id"].as_str().unwrap()).unwrap();
    assert_eq!(status["children"].as_array().unwrap().len(), 0);
}
```
- [ ] Run: `cargo test -p apb-engine --lib running_child_contributes`; `cargo test -p apb-mcp --test run_tools_test`; then `cargo test -p apb-engine --lib progress` (existing progress tests must stay green after the `weighted` extraction). Expected: pass.
- [ ] Gates: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`.

---

## Web UI

### Task W1: Start node "Input prompt" editor + autosave + run view instruction

**Files:**
- Modify: `web/src/lib/api.ts` (`fetchInputDraft`, `saveInputDraft`)
- Modify: `web/src/lib/NodePanel.svelte` (Start-node "Input prompt" textarea, debounced autosave)
- Modify: `web/src/pages/RunView.svelte` (read-only run input when the Start node context is shown)
- Test: extend `web/src/lib/api.test.ts` (draft endpoint URL shape)

**Interfaces (produced):**
- `fetchInputDraft(id: string, workspace?: string): Promise<{ instruction: string | null }>`
- `saveInputDraft(id: string, instruction: string, workspace?: string): Promise<{ instruction: string | null }>`

Steps:

- [ ] Write the failing test. Add to `web/src/lib/api.test.ts` (mirror the existing fetch-mock style in that file):
```ts
it('input draft endpoints hit the right URLs', async () => {
  const calls: string[] = []
  const orig = globalThis.fetch
  globalThis.fetch = (async (url: string, init?: RequestInit) => {
    calls.push(`${init?.method ?? 'GET'} ${url}`)
    return new Response(JSON.stringify({ instruction: 'x' }), { status: 200 })
  }) as unknown as typeof fetch
  const { fetchInputDraft, saveInputDraft } = await import('./api')
  await fetchInputDraft('p')
  await saveInputDraft('p', 'hi')
  globalThis.fetch = orig
  expect(calls).toContain('GET /api/playbooks/p/input-draft')
  expect(calls).toContain('PUT /api/playbooks/p/input-draft')
})
```
- [ ] Run: `cd web && bun run test api`. Expected: failure (helpers missing).
- [ ] Add to `web/src/lib/api.ts` (near `fetchPlaybook`):
```ts
export const fetchInputDraft = (id: string, workspace = '') =>
  getJson<{ instruction: string | null }>(`${pb(id)}/input-draft${qs({ workspace })}`)

export const saveInputDraft = (id: string, instruction: string, workspace = '') =>
  requestJson<{ instruction: string | null }>(`${pb(id)}/input-draft${qs({ workspace })}`, {
    method: 'PUT',
    headers: jsonHeaders,
    body: JSON.stringify({ instruction }),
  })
```
- [ ] Add the Start-node "Input prompt" field to `web/src/lib/NodePanel.svelte`. Import the helpers (`import { fetchInputDraft, saveInputDraft } from './api'`) and a playbook id prop. Since the panel currently takes only `node`, thread the playbook `id` through from `PlaybookEdit.svelte` (add `id: string` to the `NodePanel` props and pass `id={id}` at its call site in `PlaybookEdit.svelte`). Thread `workspace` the same way, from wherever the page's other API calls take it (read the page first; if a workspace value is already available in the panel or a shared store, reuse it instead of adding a prop). Then add, inside `<Field.FieldGroup>`, a block gated on the Start kind:
```svelte
    {#if kind === 'start'}
      <Field.Field>
        <Field.FieldLabel for="np-input">input prompt (run draft, not versioned)</Field.FieldLabel>
        <Textarea
          id="np-input"
          rows={4}
          value={draft}
          oninput={(e) => onDraftInput(e.currentTarget.value)}
        />
      </Field.Field>
    {/if}
```
with the script-side state and debounce (roughly 500 ms):
```svelte
  let draft = $state('')
  let draftTimer: ReturnType<typeof setTimeout> | undefined
  $effect(() => {
    void node.id
    if (node.type !== 'start') return
    let cancelled = false
    fetchInputDraft(id, workspace)
      .then((r) => {
        if (!cancelled) draft = r.instruction ?? ''
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  })
  function onDraftInput(v: string) {
    draft = v
    clearTimeout(draftTimer)
    draftTimer = setTimeout(() => {
      saveInputDraft(id, v, workspace).catch(() => {})
    }, 500)
  }
```
The draft edit does NOT go through `onChange` (it must not touch the YAML/version); it only calls `saveInputDraft`.
- [ ] Show the snapshotted run input in `web/src/pages/RunView.svelte`. Add, inside the sidebar `<aside>` block (near the other cards), a read-only input card when `detail.instruction` is set:
```svelte
    {#if detail?.instruction}
      <Card.Root>
        <Card.Header><Card.Title class="text-sm">Run input</Card.Title></Card.Header>
        <Card.Content>
          <pre class="whitespace-pre-wrap break-words text-xs text-muted-foreground">{detail.instruction}</pre>
        </Card.Content>
      </Card.Root>
    {/if}
```
- [ ] Run: `cd web && bun run test api` and `bun run check`. Expected: pass, type-clean.
- [ ] Gate recap for web: `cd web && bun run check`; `bun run test`.

---

### Task W2: Finish node form (prompt/profile) + run answer display

**Files:**
- Modify: `web/src/lib/playbookedit.ts` (allow `prompt`/`profile` on a finish node's editable set - no code change if `updateNode` is generic; verify)
- Modify: `web/src/lib/NodePanel.svelte` (finish: add prompt textarea + profile input)
- Modify: `web/src/lib/types.ts` (`RunDetail.answer`)
- Modify: `web/src/pages/RunView.svelte` (answer in the header/sidebar)
- Test: extend `web/src/lib/playbookedit.test.ts` (finish prompt/profile round-trips through the YAML AST)

**Interfaces (produced):** `RunDetail.answer?: string | null`.

Steps:

- [ ] Write the failing test. Add to `web/src/lib/playbookedit.test.ts` (mirror its existing `updateNode` cases):
```ts
it('sets prompt and profile on a finish node', () => {
  const src = [
    'schema: 2',
    'id: p',
    'name: p',
    'version: 1.0.0',
    'nodes:',
    '  - { id: f, type: finish, outcome: success }',
    'edges: []',
    '',
  ].join('\n')
  const doc = parseDocument(src)
  const next = updateNode(doc, 'f', { prompt: 'compose', profile: 'writer' })
  const yaml = next.toString()
  expect(yaml).toContain('prompt: compose')
  expect(yaml).toContain('profile: writer')
})
```
(Use the same `parseDocument`/`updateNode` imports the rest of `playbookedit.test.ts` uses.)
- [ ] Run: `cd web && bun run test playbookedit`. Expected: pass immediately if `updateNode` is generic over keys; if it whitelists keys, extend the whitelist to include `prompt` and `profile` for the finish kind, then it passes. (Read `updateNode` in `web/src/lib/playbookedit.ts` and confirm; adjust only if needed.)
- [ ] Add the finish fields to `web/src/lib/NodePanel.svelte`. In the `{:else if kind === 'finish'}` branch, after the `outcome` field, add a prompt textarea and a profile input (reuse the same `f.prompt`/`f.profile` local state already initialized in the sync effect, and the existing `apb-profile-options` datalist):
```svelte
      <Field.Field>
        <Field.FieldLabel for="np-finish-prompt">prompt (compose the run answer; optional)</Field.FieldLabel>
        <Textarea
          id="np-finish-prompt"
          rows={4}
          value={f.prompt}
          oninput={(e) => setStr('prompt', e.currentTarget.value)}
        />
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-finish-profile">profile</Field.FieldLabel>
        <Input
          id="np-finish-profile"
          list="apb-profile-options"
          placeholder="name (scope auto) or scope/name"
          value={f.profile}
          oninput={(e) => setProfile(e.currentTarget.value)}
        />
      </Field.Field>
```
- [ ] Add `answer` to `RunDetail` in `web/src/lib/types.ts` (after `instruction`): `answer?: string | null`.
- [ ] Show the answer in `web/src/pages/RunView.svelte`. Add a header strip below the existing progress strip:
```svelte
{#if detail?.answer}
  <div class="border-b border-border px-4 py-2">
    <div class="text-xs font-semibold text-muted-foreground">Answer</div>
    <pre class="mt-1 whitespace-pre-wrap break-words text-sm">{detail.answer}</pre>
  </div>
{/if}
```
- [ ] Run: `cd web && bun run test playbookedit` and `bun run check`. Expected: pass, type-clean.
- [ ] Gate recap for web: `cd web && bun run check`; `bun run test`.

---

### Task W3: `playbook` node editor form + child-run link + runs list child grouping

**Files:**
- Modify: `web/src/lib/playbookedit.ts` (`NODE_TYPES` gains `'playbook'`)
- Modify: `web/src/lib/NodePanel.svelte` (playbook: id dropdown + instruction textarea)
- Modify: `web/src/lib/types.ts` (`RunSummary.parent_run`)
- Modify: `web/src/pages/RunList.svelte` (indent child rows under their parent)
- Test: extend `web/src/lib/playbookedit.test.ts` (add + edit a playbook node)

**Interfaces (produced):** `RunSummary.parent_run?: string | null`.

Steps:

- [ ] Write the failing test. Add to `web/src/lib/playbookedit.test.ts`:
```ts
it('adds a playbook node and sets its reference and instruction', () => {
  const src = [
    'schema: 2',
    'id: p',
    'name: p',
    'version: 1.0.0',
    'nodes:',
    '  - { id: s, type: start }',
    'edges: []',
    '',
  ].join('\n')
  let doc = parseDocument(src)
  doc = addNode(doc, 'playbook', 'c')
  doc = updateNode(doc, 'c', { playbook: 'child', instruction: 'go' })
  const yaml = doc.toString()
  expect(yaml).toContain('type: playbook')
  expect(yaml).toContain('playbook: child')
  expect(yaml).toContain('instruction: go')
})
```
- [ ] Run: `cd web && bun run test playbookedit`. Expected: failure (`'playbook'` not an allowed node type).
- [ ] Add `'playbook'` to `NODE_TYPES` in `web/src/lib/playbookedit.ts`:
```ts
const NODE_TYPES = ['start', 'agent_task', 'script', 'condition', 'finish', 'playbook'] as const
```
Confirm `addNode`/`updateNode` do not whitelist per-kind fields in a way that drops `playbook`/`instruction`; if they do, add these keys.
- [ ] Add the `playbook`-kind form to `web/src/lib/NodePanel.svelte`. Extend the local-field sync (`f = { ... }`) with `playbook: str(n.playbook)` and `instruction: str(n.instruction)`, fetch the playbook list for a datalist (reuse the pattern used for profiles, hitting `/api/playbooks`), and add a branch:
```svelte
    {:else if kind === 'playbook'}
      <Field.Field>
        <Field.FieldLabel for="np-pb-ref">playbook</Field.FieldLabel>
        <Input
          id="np-pb-ref"
          list="apb-playbook-options"
          placeholder="id (scope auto) or use { id, scope } in YAML"
          value={f.playbook}
          oninput={(e) => setStr('playbook', e.currentTarget.value)}
        />
        <datalist id="apb-playbook-options">
          {#each playbookOptions as pbid (pbid)}
            <option value={pbid}></option>
          {/each}
        </datalist>
      </Field.Field>
      <Field.Field>
        <Field.FieldLabel for="np-pb-instr">instruction (rendered, becomes the child input)</Field.FieldLabel>
        <Textarea
          id="np-pb-instr"
          rows={4}
          value={f.instruction}
          oninput={(e) => setStr('instruction', e.currentTarget.value)}
        />
      </Field.Field>
```
Note: the object ref form `{ id, scope }` stays a YAML-editor affordance; the panel field sets the bare-string shorthand only. `setStr('playbook', ...)` writes `playbook: <string>` which the schema parses as `scope: auto`.
- [ ] Add `parent_run` to `RunSummary` in `web/src/lib/types.ts`: `parent_run?: string | null`.
- [ ] Indent child rows in `web/src/pages/RunList.svelte`: in the run-name cell, when `r.parent_run` is set, add a left indent + a small "child of" affordance, e.g. wrap the run id with `class={r.parent_run ? 'pl-4' : ''}` and a `{#if r.parent_run}<span class="text-muted-foreground">child of {r.parent_run}</span>{/if}`. Keep it minimal; the child is already a normal row with its own progress bar.
- [ ] Run: `cd web && bun run test playbookedit` and `bun run check`. Expected: pass, type-clean.
- [ ] Gate recap for web: `cd web && bun run check`; `bun run test`.

---

## Docs

### Task D1: HOWTO-authoring guidance for input prompt, finish answer, and sub-playbooks

**Files:**
- Modify: `docs/HOWTO-authoring.md` (three new sections; `playbook_howto` includes this file verbatim)

Steps:

- [ ] Add three sections to `docs/HOWTO-authoring.md`. Place them after the existing node-type / expected_duration guidance (no em-dashes, no exclamation marks, no CJK):
```markdown
## Run input prompt (Start node)

Every run can carry a free-form "input prompt": the text available to node
prompts as `{{run.instruction}}`. Edit it on the Start node in the web editor.
Typing autosaves a draft that is NOT part of the playbook definition: it does
not create a version and does not change trust, and a frozen playbook still
accepts draft edits. At run start the value is resolved once: an explicitly
passed instruction wins, otherwise the current draft, otherwise none. The chosen
value is snapshotted immutably into the run.

## Finish answer

A finish node may carry a `prompt` and an optional `profile`. With a prompt, an
agent composes the run's final answer from the accumulated run context (params,
instruction, node outputs, reviews, hooks, compacted context) and that text
becomes the run answer, shown on the dashboard and returned by run_status and
run_report. A finish without a prompt stays instant and free with an empty
answer. Do not set a profile without a prompt (validator V21). Estimate
expected_duration on a finish-with-prompt like any agent step.

## Sub-playbooks (the playbook node)

A `playbook` node runs another playbook as a full child run:

    - id: translate_book
      type: playbook
      playbook: book-translation      # or { id: book-translation, scope: global }
      instruction: "Translate the plan from {{outputs.plan}} chapter by chapter."
      expected_duration: 2h

The node's rendered instruction becomes the child's run input; the child's
finish answer becomes the node's output. The child is an ordinary playbook (any
playbook can be a child). The parent's policy gate walks the whole reference
tree once and pins each child, so you consent to the whole tree at parent start;
an untrusted child blocks the parent, and a reference cycle is refused. Nesting
is limited to 5 levels. Set expected_duration explicitly on a playbook node
(validator V19 nudges you): the parent cannot sum the child's own estimates.
```
- [ ] If `playbook_howto` composes text from constants in `crates/apb-mcp/src/tools.rs` or `crates/apb-mcp/src/instructions.rs` rather than embedding `docs/HOWTO-authoring.md` verbatim, add one short paragraph there naming the `playbook`/`finish` node kinds and the run-input draft, so the MCP `playbook_howto` output documents them too. Read those files first and match their existing style.
- [ ] Gates: confirm no em-dash (U+2014) or exclamation mark in the new text: `rg -n $'—|!' docs/HOWTO-authoring.md` returns nothing new.

---

## Final verification (run once after Task D1, before release)

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cd web && bun run check && bun run test && bun run build`
- [ ] `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .` (must exit 0; for any violation read `code-ranker docs base <ID>`, fix, re-run)
- [ ] `cargo clippy --release` clean before the work is considered release-ready
- [ ] Do not push or publish without the owner's explicit approval.

## Notes for implementers (cross-task invariants)
- Sub-playbook children NEVER take the workdir lock: they always start with `allow_shared_workdir: true` because the parent already holds it for the whole synchronous tree (`acquire` is PID-keyed and would return WorkdirBusy for a same-process second acquire). A `playbook` node is not `is_agent_or_script`, so it always runs on the drive thread, never a parallel worker thread - which is why `run_playbook_node` can hold `&mut EventLog` and append `ChildRunStarted` itself without breaking the single-writer invariant.
- `ChildRunStarted` is appended BEFORE the child is driven, so a resume can reattach: `run_playbook_node` reattaches to the latest non-terminal child (resume) instead of starting a fresh one.
- Anti-TOCTOU threads the whole tree: the gate pins each child (`RunPermit.children` -> `RunConfig.expected_children`), and each child, when started, receives its own subtree pins (`expected_children`) plus its `expected_digest`/`expected_profile_bundles`; the child's `prepare_run_target` enforces them exactly like the parent. On the CLI/engine-direct path there is no gate, so `expected_children` is `None` and children resolve live (no drift check), matching how the CLI already runs playbooks without the MCP gate.
- The run answer is derived purely by fold (`progress::run_answer`): no new storage. It is the non-empty output of the succeeded finish node.
- Progress stays a pure fold except the explicit, root-aware child-credit enrichment in `from_run_dir_with_root`; the pure `compute` fold is untouched, so a running child's fractional credit never leaks into replay determinism.
- All new schema fields and event fields are `#[serde(default)]`; schema stays 2; playbooks without the new fields behave byte-for-byte as before.
