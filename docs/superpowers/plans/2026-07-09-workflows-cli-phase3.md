# Workflows CLI, Phase 3 (MCP transport + read/run tools) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `wf mcp` - a stdio MCP server that gives the coding agent (Claude Code) access to the workflows in the current folder: read tools (list, get, validate) and run tools (run, status, events, report, resume). This is the transport foundation on which Phase 4 will build the supervisor-agent tools.

**Architecture:** A new crate `wf-mcp` on top of `wf-core` + `wf-engine`. Each tool's logic is a pure function `(root, params) -> serde_json::Value`, fully unit-testable without the MCP protocol. A thin layer on `rmcp` (the official Rust MCP SDK) registers these functions as MCP tools and serves stdio. Phase 3's mode is boot-core: `wf mcp` resolves `.wf/` from cwd and works with the engine directly. `workflow_run` in this phase is synchronous (blocks until finish, returns run_id + outcome); background runs and supervisor tokens are Phase 4.

**Tech Stack:** Rust (edition 2024), rmcp (MCP SDK), serde + serde_json, tokio; wf-core, wf-engine.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (sections 13 MCP, 14 CLI). Builds on Phases 1-2.

## Global Constraints

- Binary name: `wf`. The project folder resolves from cwd (`.wf/`). MCP is a subcommand `wf mcp` (stdio).
- Rust edition 2024. TDD is mandatory: a failing test first, then the implementation. Commit at the end of each task.
- Error texts are in English; comments/documentation are in Russian. No em dashes and no exclamation marks in docs.
- Project version stays at `0.1.0` (in development); it is not bumped per phase.
- Check dependency versions online at the time of `cargo add` and pin them (project rule). For `rmcp`, take the current stable version, check its current API (via Context7/docs) before writing the binding; the macro/trait API shown below is schematic and must be brought in line with the actual API of the installed version.
- Do not call the real `claude` in tests: test the run tools on a workflow without agent_task (start -> prompt -> finish).
- Run code-ranker before marking a task done; navigate the code via codegraph.
- Single engine: if `wf serve` is already running in this folder, `wf mcp` becoming a full thin client of its API comes later (Phase 4+); Phase 3 is boot-core mode, so this doc should warn against running `wf mcp` and `wf serve` in the same folder at the same time for write runs (the working-folder lock serializes them regardless, but this avoids confusion).

---

### Task 1: The wf-mcp crate and read tools

**Files:**
- Modify: `Cargo.toml` (workspace member)
- Create: `crates/wf-mcp/Cargo.toml`, `crates/wf-mcp/src/lib.rs`, `crates/wf-mcp/src/tools.rs`, `crates/wf-mcp/tests/read_tools_test.rs`

**Interfaces:**
- Consumes: `wf_core::registry::Registry`, `wf_core::validate::{validate, Severity, ValidationContext}`, `wf_core::schema::Workflow`.
- Produces:
  - `wf_mcp::tools::ToolError` (thiserror): `NotFound(String)`, `Engine(String)`.
  - `tools::workflow_list(root: &Path) -> Result<serde_json::Value, ToolError>` -> `[{id,name,description,current,versions}]`.
  - `tools::workflow_get(root: &Path, id: &str, version: Option<&str>) -> Result<serde_json::Value, ToolError>` -> `{id,version,yaml,workflow,layout}`; NotFound if missing.
  - `tools::workflow_validate(root: &Path, id: &str) -> Result<serde_json::Value, ToolError>` -> `{valid: bool, issues: [{code,severity,message,node}]}`.

- [ ] **Step 1: Crate and workspace**

In the root `Cargo.toml`, add `"crates/wf-mcp"` to `members`.

`crates/wf-mcp/Cargo.toml`:

```toml
[package]
name = "wf-mcp"
version.workspace = true
edition.workspace = true

[dependencies]
wf-core = { path = "../wf-core" }
wf-engine = { path = "../wf-engine" }
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[dev-dependencies]
tempfile = "3.27.0"
```

`crates/wf-mcp/src/lib.rs`:

```rust
pub mod tools;
```

- [ ] **Step 2: Failing test**

`crates/wf-mcp/tests/read_tools_test.rs`:

```rust
use std::fs;
use std::path::Path;
use wf_mcp::tools::{workflow_get, workflow_list, workflow_validate};

const VALID: &str = include_str!("../../wf-core/tests/fixtures/valid.yaml");

fn seed(root: &Path) {
    wf_core::registry::init_project(root).unwrap();
    let vdir = root.join(".wf/workflows/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), VALID).unwrap();
    fs::write(root.join(".wf/workflows/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".wf/profiles/architect")).unwrap();
}

#[test]
fn list_returns_workflow() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = workflow_list(dir.path()).unwrap();
    assert_eq!(v[0]["id"], "implement-task");
    assert_eq!(v[0]["current"], "1.0.0");
}

#[test]
fn get_returns_yaml_and_model() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = workflow_get(dir.path(), "implement-task", None).unwrap();
    assert_eq!(v["version"], "1.0.0");
    assert_eq!(v["workflow"]["nodes"][0]["type"], "start");
    assert!(v["yaml"].as_str().unwrap().contains("implement-task"));
}

#[test]
fn validate_reports_ok() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let v = workflow_validate(dir.path(), "implement-task").unwrap();
    assert_eq!(v["valid"], true);
    assert!(v["issues"].as_array().unwrap().is_empty());
}

#[test]
fn get_unknown_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    assert!(workflow_get(dir.path(), "ghost", None).is_err());
}
```

- [ ] **Step 3: Confirm the test fails**

Run: `cargo test -p wf-mcp --test read_tools_test`
Expected: FAIL (the crate/functions do not exist yet).

- [ ] **Step 4: Implement the read tools**

`crates/wf-mcp/src/tools.rs`:

```rust
use std::path::Path;

use serde_json::{json, Value};
use wf_core::registry::{Registry, RegistryError};
use wf_core::validate::{validate, Severity, ValidationContext};

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("engine error: {0}")]
    Engine(String),
}

impl From<RegistryError> for ToolError {
    fn from(e: RegistryError) -> Self {
        match e {
            RegistryError::NotFound(w) => ToolError::NotFound(w),
            other => ToolError::Engine(other.to_string()),
        }
    }
}

fn open(root: &Path) -> Result<Registry, ToolError> {
    Registry::open(root).map_err(ToolError::from)
}

pub fn workflow_list(root: &Path) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let list = reg.list().map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(serde_json::to_value(list).map_err(|e| ToolError::Engine(e.to_string()))?)
}

pub fn workflow_get(root: &Path, id: &str, version: Option<&str>) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, version)?;
    Ok(json!({
        "id": id,
        "version": loaded.version,
        "yaml": loaded.yaml,
        "workflow": loaded.workflow,
        "layout": loaded.layout,
    }))
}

pub fn workflow_validate(root: &Path, id: &str) -> Result<Value, ToolError> {
    let reg = open(root)?;
    let loaded = reg.load(id, None)?;
    let ctx = ValidationContext { global_executors: vec![], profiles: reg.profiles() };
    let report = validate(&loaded.workflow, &ctx);
    let issues: Vec<Value> = report.issues.iter().map(|i| json!({
        "code": i.code,
        "severity": match i.severity { Severity::Error => "error", Severity::Warning => "warning" },
        "message": i.message,
        "node": i.node,
    })).collect();
    Ok(json!({ "valid": report.is_valid(), "issues": issues }))
}
```

- [ ] **Step 5: Run the tests and commit**

Run: `cargo test -p wf-mcp --test read_tools_test`
Expected: 4 passed.

```bash
git add -A
git commit -m "feat(mcp): wf-mcp crate with read tools (list, get, validate)"
```

---

### Task 2: Run tools

**Files:**
- Modify: `crates/wf-mcp/src/tools.rs`
- Create: `crates/wf-mcp/tests/run_tools_test.rs`

**Interfaces:**
- Consumes: `wf_engine::{run, resume, list_runs, RunOptions}`, `wf_engine::event::read_all`, `wf_engine::state::RunState`, `wf_engine::run_config::read_run_config`.
- Produces:
  - `tools::workflow_run(root, id, version: Option<&str>, params: BTreeMap<String,String>, instruction: Option<String>) -> Result<Value, ToolError>` -> `{run_id, outcome}` (synchronous: blocks until finish).
  - `tools::runs_list(root) -> Result<Value, ToolError>` -> an array of RunSummary.
  - `tools::run_status(root, run_id) -> Result<Value, ToolError>` -> `{run_id, run_status, nodes, outputs}`; NotFound if the folder is missing.
  - `tools::run_events(root, run_id, from_seq: Option<u64>) -> Result<Value, ToolError>` -> `{events: [...]}` with seq >= from_seq.
  - `tools::run_report(root, run_id) -> Result<Value, ToolError>` -> `{run_status, nodes}` (a full supervisor-agent report is Phase 4).
  - `tools::run_resume(root, run_id, from_node: Option<&str>) -> Result<Value, ToolError>` -> `{run_id, outcome}`.

- [ ] **Step 1: Failing test**

`crates/wf-mcp/tests/run_tools_test.rs`:

```rust
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use wf_mcp::tools::{run_events, run_status, runs_list, workflow_run};

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed(root: &Path) {
    wf_core::registry::init_project(root).unwrap();
    let vdir = root.join(".wf/workflows/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".wf/workflows/noagent/current"), "1.0.0").unwrap();
}

#[test]
fn run_then_inspect() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut params = BTreeMap::new();
    params.insert("who".to_string(), "world".to_string());
    let run = workflow_run(dir.path(), "noagent", None, params, None).unwrap();
    assert_eq!(run["outcome"], "succeeded");
    let run_id = run["run_id"].as_str().unwrap().to_string();

    let listed = runs_list(dir.path()).unwrap();
    assert_eq!(listed[0]["run_id"], run_id.as_str());

    let status = run_status(dir.path(), &run_id).unwrap();
    assert_eq!(status["run_status"], "succeeded");
    assert_eq!(status["nodes"]["note"], "succeeded");

    let ev = run_events(dir.path(), &run_id, None).unwrap();
    assert!(ev["events"].as_array().unwrap().len() >= 3);
    // pagination: from_seq cuts off earlier events
    let ev2 = run_events(dir.path(), &run_id, Some(2)).unwrap();
    let first_seq = ev2["events"][0]["seq"].as_u64().unwrap();
    assert!(first_seq >= 2);
}

#[test]
fn status_unknown_run_is_error() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    assert!(run_status(dir.path(), "ghost-1").is_err());
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-mcp --test run_tools_test`
Expected: FAIL.

- [ ] **Step 3: Implement the run tools**

Add to `crates/wf-mcp/src/tools.rs` (extend the imports at the top):

```rust
use std::collections::BTreeMap;
use wf_engine::event::read_all;
use wf_engine::run_config::read_run_config;
use wf_engine::state::RunState;
use wf_engine::{list_runs, resume, run, RunOptions};

fn run_dir(root: &Path, run_id: &str) -> std::path::PathBuf {
    root.join(".wf/runs").join(run_id)
}

pub fn workflow_run(
    root: &Path,
    id: &str,
    version: Option<&str>,
    params: BTreeMap<String, String>,
    instruction: Option<String>,
) -> Result<Value, ToolError> {
    let opts = RunOptions { instruction, params, allow_shared_workdir: false };
    let res = run(root, id, version, opts).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "run_id": res.run_id, "outcome": res.outcome.as_str() }))
}

pub fn runs_list(root: &Path) -> Result<Value, ToolError> {
    let runs = list_runs(root).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(serde_json::to_value(runs).map_err(|e| ToolError::Engine(e.to_string()))?)
}

pub fn run_status(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    let dir = run_dir(root, run_id);
    if !dir.is_dir() { return Err(ToolError::NotFound(format!("run `{run_id}`"))); }
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let state = RunState::fold(&events);
    let nodes: BTreeMap<String, String> = state.nodes.iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string())).collect();
    Ok(json!({
        "run_id": run_id,
        "run_status": state.run_status.as_str(),
        "nodes": nodes,
        "outputs": state.outputs,
    }))
}

pub fn run_events(root: &Path, run_id: &str, from_seq: Option<u64>) -> Result<Value, ToolError> {
    let dir = run_dir(root, run_id);
    if !dir.is_dir() { return Err(ToolError::NotFound(format!("run `{run_id}`"))); }
    let events = read_all(&dir).map_err(|e| ToolError::Engine(e.to_string()))?;
    let from = from_seq.unwrap_or(0);
    let filtered: Vec<&_> = events.iter().filter(|e| e.seq >= from).collect();
    Ok(json!({ "events": serde_json::to_value(filtered).map_err(|e| ToolError::Engine(e.to_string()))? }))
}

pub fn run_report(root: &Path, run_id: &str) -> Result<Value, ToolError> {
    // There is no supervisor agent in Phase 3: the report is a lightweight state summary. The full supervisor report is Phase 4.
    run_status(root, run_id)
}

pub fn run_resume(root: &Path, run_id: &str, from_node: Option<&str>) -> Result<Value, ToolError> {
    let res = resume(root, run_id, from_node).map_err(|e| ToolError::Engine(e.to_string()))?;
    Ok(json!({ "run_id": res.run_id, "outcome": res.outcome.as_str() }))
}
```

Note: `read_run_config` is imported for future use (instruction/params in the status) - if the linter complains about an unused import in Phase 3, remove it and bring it back in Phase 4.

- [ ] **Step 4: Run the tests and commit**

Run: `cargo test -p wf-mcp`
Expected: read (4) + run (2) tests green.

```bash
git add -A
git commit -m "feat(mcp): run tools (run, runs, status, events, report, resume)"
```

---

### Task 3: MCP server on rmcp (stdio)

**Files:**
- Modify: `crates/wf-mcp/Cargo.toml` (rmcp, tokio)
- Create: `crates/wf-mcp/src/server.rs`
- Modify: `crates/wf-mcp/src/lib.rs`

**Interfaces:**
- Consumes: `tools::*`.
- Produces: `wf_mcp::server::serve_stdio(root: PathBuf) -> anyhow::Result<()>` (blocking async) - starts an MCP server on stdio, registers the tools `workflow_list/workflow_get/workflow_validate/workflow_run/runs_list/run_status/run_events/run_report/run_resume`, translates them into JSON-RPC MCP tools. `ToolError::NotFound` -> a tool error with a clear message.

- [ ] **Step 1: Add rmcp (check the version online)**

Check the current stable version of `rmcp` and its API (Context7: resolve-library-id "rmcp" / "rust mcp sdk", then query-docs). Then:

Run: `cargo add rmcp -p wf-mcp` (take the current version; enable the stdio-transport and server-macro features per that version's docs, e.g. `--features server,transport-io` - check the exact feature set against the docs) and `cargo add tokio anyhow -p wf-mcp` (tokio with features rt-multi-thread, macros, io-std).

- [ ] **Step 2: Implement the server (bring it in line with rmcp's actual API)**

`crates/wf-mcp/src/server.rs` - the structure below is SCHEMATIC (bring the rmcp macro/trait names in line with the installed version; the tool logic - calls into `tools::*` - does not need to change):

```rust
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;

use crate::tools;

/// Server state: the project root (resolved from cwd by the caller).
#[derive(Clone)]
pub struct WfMcp {
    root: Arc<PathBuf>,
}

// Tool parameters - via rmcp's #[tool(...)] arguments; shown here as structs.
#[derive(Deserialize)]
pub struct GetArgs { pub id: String, pub version: Option<String> }
#[derive(Deserialize)]
pub struct RunArgs {
    pub id: String,
    pub version: Option<String>,
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, String>,
    pub instruction: Option<String>,
}
#[derive(Deserialize)]
pub struct RunRef { pub run_id: String }
#[derive(Deserialize)]
pub struct EventsArgs { pub run_id: String, pub from_seq: Option<u64> }
#[derive(Deserialize)]
pub struct ResumeArgs { pub run_id: String, pub from_node: Option<String> }

impl WfMcp {
    pub fn new(root: PathBuf) -> Self { Self { root: Arc::new(root) } }

    // Each method is a thin wrapper over tools::*, serializing the result/error into an MCP response.
    // Example body (the real signature/attributes follow rmcp's API):
    //   let v = tools::workflow_list(&self.root).map_err(to_mcp_err)?;
    //   Ok(CallToolResult::success(json_content(v)))
    // Register 9 tools: workflow_list, workflow_get, workflow_validate,
    // workflow_run, runs_list, run_status, run_events, run_report, run_resume.
}

/// Start the stdio MCP server and serve until stdin closes.
pub async fn serve_stdio(root: PathBuf) -> anyhow::Result<()> {
    // Pseudocode per rmcp's API:
    //   let service = WfMcp::new(root);
    //   let transport = (rmcp::transport::stdio());
    //   service.serve(transport).await?.waiting().await?;
    //   Ok(())
    todo!("wire to rmcp per its current API")
}
```

In `crates/wf-mcp/src/lib.rs`, add `pub mod server;`.

Implement it so that:
- each of the 9 tools calls the corresponding `tools::*` and returns its `Value` as the text/JSON content of the MCP result;
- `ToolError::NotFound` -> an MCP tool error (is_error) with a message; `ToolError::Engine` is also a tool error;
- the tool names and descriptions match the spec (section 13): `workflow_list`, `workflow_get`, `workflow_validate`, `workflow_run`, `run_status`, `run_events`, `run_report`, `run_cancel` (not implemented in Phase 3 - do not register), `run_resume`. Additionally `runs_list` for listing runs.

- [ ] **Step 3: Tool-registration test**

The full e2e stdio protocol is verified in Task 4. Here it's a service-level test: if rmcp provides an in-memory/local transport or a direct call into the tool handler, verify that `tools/list` returns the expected names and that calling `workflow_list` on a seeded project returns valid JSON. If the installed version's API doesn't offer a convenient in-memory test, document that in the report and rely on the e2e test from Task 4 (in that case it's enough for this task to have `cargo build -p wf-mcp` succeed with no errors, plus a unit test for the `ToolError -> is_error` mapping if it's factored out into a pure function).

Run: `cargo test -p wf-mcp` and `cargo build -p wf-mcp`
Expected: a clean build, available tests green.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(mcp): rmcp stdio server exposing read and run tools"
```

---

### Task 4: The `wf mcp` subcommand and an end-to-end check

**Files:**
- Modify: `crates/wf-cli/Cargo.toml`, `crates/wf-cli/src/main.rs`
- Create: `crates/wf-cli/tests/mcp_cli_test.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `wf_mcp::server::serve_stdio`.
- Produces: the `wf mcp` command - starts a stdio MCP server for the `.wf/` resolved from cwd.

- [ ] **Step 1: Dependency and test**

Run: `cargo add wf-mcp --path crates/wf-mcp -p wf-cli`

`crates/wf-cli/tests/mcp_cli_test.rs` - e2e via raw JSON-RPC over stdio (initialize + tools/list + tools/call), without a real agent:

```rust
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::fs;

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
params:
  - { name: who, type: text }
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

#[test]
fn mcp_initialize_and_list_tools() {
    let dir = tempfile::tempdir().unwrap();
    // init + seed
    Command::new(env!("CARGO_BIN_EXE_wf")).arg("init").current_dir(dir.path()).output().unwrap();
    let vdir = dir.path().join(".wf/workflows/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".wf/workflows/noagent/current"), "1.0.0").unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_wf"))
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // Send MCP initialize and tools/list, one JSON-RPC message per line.
    // NOTE: the exact framing format (newline-delimited vs Content-Length) and the
    // initialize fields must match the protocol used by the installed rmcp version.
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#;
    writeln!(stdin, "{init}").unwrap();
    let list = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    writeln!(stdin, "{list}").unwrap();
    stdin.flush().unwrap();

    // Read the responses, looking for the tool list.
    let mut saw_tools = false;
    for _ in 0..10 {
        let mut line = String::new();
        if stdout.read_line(&mut line).unwrap() == 0 { break; }
        if line.contains("workflow_list") && line.contains("workflow_run") {
            saw_tools = true; break;
        }
    }
    let _ = child.kill();
    assert!(saw_tools, "tools/list must include workflow_list and workflow_run");
}
```

Note: if rmcp's actual framing/handshake differs from newline-delimited (e.g. it requires a full MCP handshake or a different protocol version), adjust the test to match the installed version's real protocol; the test's goal is to prove that `wf mcp` starts up and responds to `tools/list` with the list of expected tools.

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-cli --test mcp_cli_test`
Expected: FAIL (the `mcp` subcommand does not exist).

- [ ] **Step 3: Implement the subcommand**

In `crates/wf-cli/src/main.rs`, add a variant to the `Command` enum:

```rust
    /// Start stdio MCP server for the current project
    Mcp,
```

In `match cli.command`, add a branch:

```rust
        Some(Command::Mcp) => mcp_cmd(&root),
```

And a function (an async bridge via tokio, like in `serve`):

```rust
fn mcp_cmd(root: &PathBuf) -> ExitCode {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(wf_mcp::server::serve_stdio(root.clone())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("mcp failed: {e}"); ExitCode::from(2) }
    }
}
```

- [ ] **Step 4: Run the tests and the whole workspace**

Run: `cargo test -p wf-cli --test mcp_cli_test`
Expected: 1 passed.

Run: `cargo test --workspace`
Expected: all tests green.

- [ ] **Step 5: README - connecting to Claude Code**

In `README.md`, add a section (regular hyphens, no em dashes):

```markdown
## MCP (coding agent)

`wf mcp` starts a stdio MCP server for the workflows in the current folder. To connect in Claude Code:

    claude mcp add wf -- /absolute/path/to/wf mcp

or a project .mcp.json:

    { "mcpServers": { "wf": { "command": "wf", "args": ["mcp"] } } }

Tools: workflow_list, workflow_get, workflow_validate, workflow_run, run_status, run_events, run_report, run_resume, runs_list.
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(cli): wf mcp subcommand and Claude Code integration docs"
```

---

### Task 5: Real check with Claude Code, code-ranker, status

**Files:**
- Modify: `docs/tasks.md`, `CHANGELOG.md`

- [ ] **Step 1: Manual check of the Claude Code connection**

```bash
cargo build -p wf-cli
# in a temp project with .wf and the noagent workflow
claude mcp add wf-test -- /Users/techmeat/www/projects/omniteamhq/workflows/target/debug/wf mcp
```
From a Claude Code session in this folder, verify that the `wf` tools are visible and that `workflow_list` returns the workflow, `workflow_run` runs noagent and returns run_id + outcome. Detach: `claude mcp remove wf-test`. (This step is manual; if unavailable in a CI environment, document that and rely on the e2e test from Task 4.)

- [ ] **Step 2: code-ranker**

```bash
cargo metadata --format-version 1 >/dev/null
code-ranker check .
```
Expected: no violations. Otherwise - scorecard worst-first, `docs base <ID>`, fix, repeat.

- [ ] **Step 3: CHANGELOG and tasks.md**

In `CHANGELOG.md`, in the `## [0.1.0] - in development` section -> `### Added`, add a line:
`- MCP server (wf mcp, stdio): read tools (list/get/validate) and run tools (run/runs/status/events/report/resume) for the coding agent.`

In `docs/tasks.md`, in the "Phase 3" section, mark the implemented items `[x]`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs: mark phase 3 (mcp read/run tools) done in tasks and changelog"
```

---

## What is deliberately NOT part of Phase 3 (for the reviewer)

- Supervisor tools and tokens (`supervisor_wait_event`, `run_inspect`, `node_retry`, `run_continue_from`, `context_append`, `supervisor_report`), the background supervisor agent, `supervise: self` - Phase 4.
- Write tools (`workflow_create/update/delete`) - Phase 5 (on the editor's shared minor-version machinery).
- `run_cancel` and a background (non-blocking) `workflow_run` - Phase 4 (needed for observing a run in progress).
- `review_decide` - Phase 7 (together with human_review).
- Thin-client mode (attaching to a running `wf serve` via a `.wf/serve.lock` handshake) - later; Phase 3 is boot-core.
