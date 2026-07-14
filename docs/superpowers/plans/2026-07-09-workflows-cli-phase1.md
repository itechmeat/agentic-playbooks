# Workflows CLI, Phase 1 (core and viewer) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working CLI `wf`: a workflow.yaml schema with a full validator, the `.wf/` structure, `init/list/validate/serve` commands, a web interface with a list of workflows and read-only graph rendering via svelte-flow, live updates from disk (watcher + WebSocket).

**Architecture:** A Cargo workspace of three crates (`wf-core` - domain and validator, `wf-server` - axum API + WS + static assets, `wf-cli` - the `wf` binary) and a Svelte app `web/`, built by Vite into static assets and baked into the binary via rust-embed. Source of truth is the YAML on disk; the server only reads and streams changes.

**Tech Stack:** Rust (edition 2024, rustc >= 1.85), tokio, axum, clap, serde + serde_yaml_ng, notify, rust-embed; Bun, Vite, Svelte 5, TypeScript, @xyflow/svelte, @dagrejs/dagre, vitest.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (sections 3-6, 10.4, 12-14, first-version invariants).

## Global Constraints

- Binary name: `wf`. Project folder: `.wf/`. Global config: `~/.config/wf/config.yaml` (not created in phase 1, only the path is reserved).
- Default web server port: `7321`, listen only on `127.0.0.1`.
- Rust edition 2024. TDD is mandatory: a failing test first, then the implementation. Commit at the end of each task.
- Dependency versions checked online on 2026-07-09 and pinned: tokio `1.52`, axum `0.8` (0.8.9), clap `4.6`, serde `1.0`, serde_yaml_ng `0.10`, thiserror `2.0`, notify `8.2`, rust-embed `8.12`, tower-http `0.7`; svelte `^5.56`, vite `^8.1`, @sveltejs/vite-plugin-svelte `^7.2`, @xyflow/svelte `^1.6`, @dagrejs/dagre `^3.0`, vitest `^4.1`. Additional small packages should be installed via `cargo add` / `bun add` (they will pick up current versions) and compatibility checked at install time.
- Workflow version folders are immutable; the canvas layout lives in `layouts/<version>.yaml`, not in `workflow.yaml`.
- Control files must be written atomically only: temp file + rename (utility from task 5).
- All validator error texts are in English (product code); documentation is in Russian.

---

### Task 1: Git repository and Cargo workspace

**Files:**
- Create: `.gitignore`, `Cargo.toml`, `crates/wf-core/Cargo.toml`, `crates/wf-core/src/lib.rs`, `crates/wf-server/Cargo.toml`, `crates/wf-server/src/lib.rs`, `crates/wf-cli/Cargo.toml`, `crates/wf-cli/src/main.rs`

**Interfaces:**
- Produces: a workspace that compiles with `cargo test`; a `wf` binary that prints its version.

- [ ] **Step 1: Initialize git and .gitignore**

```bash
cd /Users/techmeat/www/projects/omniteamhq/workflows
git init
```

`.gitignore`:

```gitignore
/target
node_modules
web/dist
.DS_Store
```

- [ ] **Step 2: Create the workspace**

`Cargo.toml` (root):

```toml
[workspace]
resolver = "2"
members = ["crates/wf-core", "crates/wf-server", "crates/wf-cli"]

[workspace.package]
version = "0.1.0"
edition = "2024"

[workspace.dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
serde_yaml_ng = "0.10"
thiserror = "2.0"
tokio = { version = "1.52", features = ["full"] }
```

`crates/wf-core/Cargo.toml`:

```toml
[package]
name = "wf-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
serde_yaml_ng.workspace = true
thiserror.workspace = true
```

`crates/wf-core/src/lib.rs`:

```rust
pub mod schema;
pub mod validate;
pub mod fsutil;
pub mod registry;
```

For now, create empty module files `schema.rs`, `validate.rs`, `fsutil.rs`, `registry.rs` (each with a single comment line `//! see plan`) so the workspace compiles.

`crates/wf-server/Cargo.toml`:

```toml
[package]
name = "wf-server"
version.workspace = true
edition.workspace = true

[dependencies]
wf-core = { path = "../wf-core" }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
axum = { version = "0.8", features = ["ws"] }
tower-http = { version = "0.7", features = ["cors"] }
notify = "8.2"
```

`crates/wf-server/src/lib.rs`: empty for now (`//! see plan`).

`crates/wf-cli/Cargo.toml`:

```toml
[package]
name = "wf-cli"
version.workspace = true
edition.workspace = true

[[bin]]
name = "wf"
path = "src/main.rs"

[dependencies]
wf-core = { path = "../wf-core" }
wf-server = { path = "../wf-server" }
tokio.workspace = true
clap = { version = "4.6", features = ["derive"] }
anyhow = "1.0"
```

`crates/wf-cli/src/main.rs`:

```rust
fn main() {
    println!("wf {}", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 3: Verify the build**

Run: `cargo build && cargo run -p wf-cli`
Expected: build without errors, output `wf 0.1.0`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: cargo workspace scaffold (wf-core, wf-server, wf-cli)"
```

---

### Task 2: Domain model of the workflow.yaml schema

**Files:**
- Create: `crates/wf-core/src/schema.rs`, `crates/wf-core/tests/fixtures/valid.yaml`, `crates/wf-core/tests/schema_test.rs`

**Interfaces:**
- Produces: `wf_core::schema::{Workflow, Node, NodeKind, Edge, EdgeCondition, Executor, ExecutorRef, Param, SchemaError}`; `Workflow::from_yaml(&str) -> Result<Workflow, SchemaError>`; serializing `Workflow` to JSON gives `type` in snake_case for nodes (needed by the server and the frontend).

- [ ] **Step 1: Write the fixture and a failing test**

`crates/wf-core/tests/fixtures/valid.yaml`:

```yaml
schema: 1
id: implement-task
name: Implement Task
description: Example from the spec, shortened
version: 1.0.0

params:
  - { name: task, type: text, label: "Task" }

executors:
  main:
    agent: claude-code
    model: claude-fable-5
    fallbacks:
      - { agent: codex, model: gpt-5.2-codex }

defaults:
  executor: main
  max_retries: 1
  timeout_seconds: 3600

nodes:
  - id: start
    type: start
    title: Start
  - id: plan
    type: agent_task
    title: Plan
    profile: architect
    prompt: |
      Draft a plan: {{params.task}}
  - id: lint
    type: script
    title: Lint
    script: scripts/node-lint.sh
    runner: sh
    timeout_seconds: 300
  - id: check
    type: condition
    title: Passed?
    max_loops: 3
  - id: fix
    type: agent_task
    title: Fix
    prompt: |
      Fix: {{nodes.lint.output}}
  - id: done
    type: finish
    outcome: success
  - id: failed
    type: finish
    outcome: failure

edges:
  - { from: start, to: plan }
  - { from: plan, to: lint }
  - { from: lint, to: check }
  - { from: check, to: done, condition: { type: node_status, node: lint, equals: success } }
  - { from: check, to: fix,  condition: { type: node_status, node: lint, equals: failure } }
  - { from: fix, to: lint }
```

`crates/wf-core/tests/schema_test.rs`:

```rust
use wf_core::schema::{NodeKind, Workflow};

const VALID: &str = include_str!("fixtures/valid.yaml");

#[test]
fn parses_valid_workflow() {
    let wf = Workflow::from_yaml(VALID).expect("must parse");
    assert_eq!(wf.id, "implement-task");
    assert_eq!(wf.version, "1.0.0");
    assert_eq!(wf.nodes.len(), 7);
    assert_eq!(wf.edges.len(), 6);
    assert!(matches!(wf.nodes[0].kind, NodeKind::Start));
    match &wf.nodes[1].kind {
        NodeKind::AgentTask { prompt, profile, .. } => {
            assert!(prompt.contains("{{params.task}}"));
            assert_eq!(profile.as_deref(), Some("architect"));
        }
        other => panic!("expected agent_task, got {other:?}"),
    }
    assert!(wf.executors.contains_key("main"));
}

#[test]
fn rejects_unknown_node_type() {
    let bad = VALID.replace("type: start", "type: warp");
    let err = Workflow::from_yaml(&bad).unwrap_err();
    assert!(err.to_string().contains("warp"));
}

#[test]
fn json_uses_snake_case_type_tag() {
    let wf = Workflow::from_yaml(VALID).unwrap();
    let json = serde_json::to_value(&wf).unwrap();
    assert_eq!(json["nodes"][1]["type"], "agent_task");
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-core --test schema_test`
Expected: FAIL (compile error: `schema::Workflow` does not exist).

- [ ] **Step 3: Implement the model**

`crates/wf-core/src/schema.rs`:

```rust
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    #[error("yaml parse error: {0}")]
    Yaml(#[from] serde_yaml_ng::Error),
}

fn default_schema() -> u32 { 1 }

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub version: String,
    #[serde(default)]
    pub params: Vec<Param>,
    #[serde(default)]
    pub executors: BTreeMap<String, Executor>,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub supervisor: Option<Supervisor>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Workflow {
    pub fn from_yaml(s: &str) -> Result<Self, SchemaError> {
        Ok(serde_yaml_ng::from_str(s)?)
    }
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String, // text | enum | int | bool
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub default: Option<serde_yaml_ng::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Executor {
    pub agent: String,
    pub model: String,
    #[serde(default)]
    pub fallbacks: Vec<Fallback>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Fallback {
    pub agent: String,
    pub model: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ExecutorRef {
    Name(String),
    Inline(Executor),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Defaults {
    #[serde(default)]
    pub executor: Option<ExecutorRef>,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Supervisor {
    #[serde(default)]
    pub executor: Option<ExecutorRef>,
    #[serde(default)]
    pub policy: Option<serde_yaml_ng::Value>, // detail added in phase 3A
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Node {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(flatten)]
    pub kind: NodeKind,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    Start,
    AgentTask {
        prompt: String,
        #[serde(default)]
        profile: Option<String>,
        #[serde(default)]
        executor: Option<ExecutorRef>,
        #[serde(default)]
        max_retries: Option<u32>,
        #[serde(default)]
        timeout_seconds: Option<u64>,
        #[serde(default)]
        workdir: Option<String>,
    },
    Script {
        script: String,
        runner: String,
        #[serde(default)]
        timeout_seconds: Option<u64>,
    },
    Prompt { prompt: String },
    Condition {
        #[serde(default)]
        max_loops: Option<u32>,
    },
    HumanReview { options: Vec<String> },
    Wait {
        wait_for: WaitFor,
        timeout_seconds: u64,
        #[serde(default)]
        scope: Option<String>,
    },
    Finish { outcome: Outcome },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WaitFor {
    Timer { seconds: u64 },
    Webhook { key: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome { Success, Failure }

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub condition: Option<EdgeCondition>,
    #[serde(default)]
    pub fallback: bool,
    #[serde(default)]
    pub join: Option<String>, // all | any; executed in phase 2, already parsed now
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EdgeCondition {
    NodeStatus { node: String, equals: StatusEq },
    ReviewStatus { equals: String },
    OutputMatch { node: String, pattern: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusEq { Success, Failure }
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-core --test schema_test`
Expected: 3 passed.

Note: if `deny_unknown_fields` conflicts with `#[serde(flatten)]` on `Node` (a known serde limitation), remove `deny_unknown_fields` only from the `Workflow` struct and verify that the `rejects_unknown_node_type` test still passes thanks to the enum tag.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): workflow schema model with yaml parsing"
```

---

### Task 3: Validator, part 1 - graph structure

**Files:**
- Create: `crates/wf-core/src/validate.rs`, `crates/wf-core/tests/validate_structure_test.rs`

**Interfaces:**
- Consumes: `schema::Workflow`.
- Produces: `validate::{validate, ValidationContext, ValidationReport, Issue, Severity}`; signature `pub fn validate(wf: &Workflow, ctx: &ValidationContext) -> ValidationReport`; `ValidationReport { pub issues: Vec<Issue> }` with methods `is_valid()` (no Error) and `errors()`; `Issue { code: &'static str, severity: Severity, message: String, node: Option<String> }`; codes `V01..V15` (see steps).

- [ ] **Step 1: Failing tests for structural rules**

`crates/wf-core/tests/validate_structure_test.rs`:

```rust
use wf_core::schema::Workflow;
use wf_core::validate::{validate, Severity, ValidationContext};

const VALID: &str = include_str!("fixtures/valid.yaml");

fn ctx() -> ValidationContext {
    ValidationContext {
        global_executors: vec![],
        profiles: vec!["architect".into(), "fullstack".into()],
    }
}

fn codes(yaml: &str) -> Vec<&'static str> {
    let wf = Workflow::from_yaml(yaml).unwrap();
    validate(&wf, &ctx())
        .issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code)
        .collect()
}

#[test]
fn valid_fixture_has_no_errors() {
    assert!(codes(VALID).is_empty(), "expected no errors");
}

#[test]
fn v01_duplicate_node_id() {
    let bad = VALID.replace("id: fix", "id: plan");
    assert!(codes(&bad).contains(&"V01"));
}

#[test]
fn v03_missing_start() {
    let bad = VALID
        .replace("type: start", "type: prompt\n    prompt: x");
    assert!(codes(&bad).contains(&"V03"));
}

#[test]
fn v04_start_with_incoming_edge() {
    let bad = format!("{VALID}  - {{ from: plan, to: start }}\n");
    assert!(codes(&bad).contains(&"V04"));
}

#[test]
fn v05_finish_with_outgoing_edge() {
    let bad = format!("{VALID}  - {{ from: done, to: plan }}\n");
    assert!(codes(&bad).contains(&"V05"));
}

#[test]
fn v06_edge_to_unknown_node() {
    let bad = format!("{VALID}  - {{ from: plan, to: ghost }}\n");
    assert!(codes(&bad).contains(&"V06"));
}

#[test]
fn v07_unreachable_node() {
    let bad = format!(
        "{VALID}  - {{ from: orphan, to: done }}\n"
    )
    .replace(
        "nodes:",
        "nodes:\n  - id: orphan\n    type: prompt\n    prompt: island",
    );
    // orphan has an outgoing edge but is unreachable from start
    assert!(codes(&bad).contains(&"V07"));
}
```

- [ ] **Step 2: Confirm the tests fail**

Run: `cargo test -p wf-core --test validate_structure_test`
Expected: FAIL (compile error: `validate` does not exist).

- [ ] **Step 3: Implement the structural rules**

`crates/wf-core/src/validate.rs`:

```rust
use std::collections::{HashMap, HashSet, VecDeque};

use crate::schema::{EdgeCondition, ExecutorRef, NodeKind, Workflow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity { Error, Warning }

#[derive(Debug)]
pub struct Issue {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    pub node: Option<String>,
}

#[derive(Debug, Default)]
pub struct ValidationReport { pub issues: Vec<Issue> }

impl ValidationReport {
    pub fn is_valid(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }
    fn error(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.issues.push(Issue { code, severity: Severity::Error, message: msg, node: node.map(String::from) });
    }
    fn warn(&mut self, code: &'static str, node: Option<&str>, msg: String) {
        self.issues.push(Issue { code, severity: Severity::Warning, message: msg, node: node.map(String::from) });
    }
}

#[derive(Debug, Default)]
pub struct ValidationContext {
    pub global_executors: Vec<String>,
    pub profiles: Vec<String>,
}

pub fn validate(wf: &Workflow, ctx: &ValidationContext) -> ValidationReport {
    let mut r = ValidationReport::default();
    check_unique_ids(wf, &mut r);            // V01, V02
    check_start_finish(wf, &mut r);          // V03, V04, V05
    check_edges_exist(wf, &mut r);           // V06
    if r.is_valid() {
        check_reachability(wf, &mut r);      // V07, V08
        check_conditions(wf, &mut r);        // V09, V10
        check_cycles(wf, &mut r);            // V11
        check_scripts(wf, &mut r);           // V12
        check_templates(wf, &mut r);         // V13
        check_refs(wf, ctx, &mut r);         // V14, V15
    }
    r
}

fn adjacency(wf: &Workflow) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in &wf.nodes { adj.entry(n.id.as_str()).or_default(); }
    for e in &wf.edges { adj.entry(e.from.as_str()).or_default().push(e.to.as_str()); }
    adj
}

fn check_unique_ids(wf: &Workflow, r: &mut ValidationReport) {
    let mut seen = HashSet::new();
    for n in &wf.nodes {
        if !seen.insert(n.id.as_str()) {
            r.error("V01", Some(&n.id), format!("duplicate node id `{}`", n.id));
        }
    }
    let mut pseen = HashSet::new();
    for p in &wf.params {
        if !pseen.insert(p.name.as_str()) {
            r.error("V02", None, format!("duplicate param name `{}`", p.name));
        }
    }
}

fn check_start_finish(wf: &Workflow, r: &mut ValidationReport) {
    let starts: Vec<_> = wf.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Start)).collect();
    if starts.len() != 1 {
        r.error("V03", None, format!("expected exactly one start node, found {}", starts.len()));
    }
    for e in &wf.edges {
        if let Some(to) = wf.node(&e.to) {
            if matches!(to.kind, NodeKind::Start) {
                r.error("V04", Some(&e.to), "start node must not have incoming edges".into());
            }
        }
        if let Some(from) = wf.node(&e.from) {
            if matches!(from.kind, NodeKind::Finish { .. }) {
                r.error("V05", Some(&e.from), "finish node must not have outgoing edges".into());
            }
        }
    }
}

fn check_edges_exist(wf: &Workflow, r: &mut ValidationReport) {
    for e in &wf.edges {
        for id in [&e.from, &e.to] {
            if wf.node(id).is_none() {
                r.error("V06", Some(id), format!("edge references unknown node `{id}`"));
            }
        }
    }
}

fn check_reachability(wf: &Workflow, r: &mut ValidationReport) {
    let Some(start) = wf.nodes.iter().find(|n| matches!(n.kind, NodeKind::Start)) else { return };
    let adj = adjacency(wf);
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([start.id.as_str()]);
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() { q.push_back(next); }
        }
    }
    for n in &wf.nodes {
        if !seen.contains(n.id.as_str()) {
            r.error("V07", Some(&n.id), format!("node `{}` is unreachable from start", n.id));
        }
    }
    // V08: from every reachable node some finish node must be reachable (otherwise warning)
    let finishes: HashSet<&str> = wf.nodes.iter()
        .filter(|n| matches!(n.kind, NodeKind::Finish { .. }))
        .map(|n| n.id.as_str()).collect();
    for n in &wf.nodes {
        if !seen.contains(n.id.as_str()) { continue; }
        let mut vis = HashSet::new();
        let mut q = VecDeque::from([n.id.as_str()]);
        let mut ok = false;
        while let Some(id) = q.pop_front() {
            if finishes.contains(id) { ok = true; break; }
            if vis.insert(id) {
                for next in adj.get(id).into_iter().flatten() { q.push_back(next); }
            }
        }
        if !ok {
            r.warn("V08", Some(&n.id), format!("no path from `{}` to any finish node", n.id));
        }
    }
}
```

Add the functions `check_conditions`, `check_cycles`, `check_scripts`, `check_templates`, `check_refs` as stubs in this task, so it compiles:

```rust
fn check_conditions(_wf: &Workflow, _r: &mut ValidationReport) {}
fn check_cycles(_wf: &Workflow, _r: &mut ValidationReport) {}
fn check_scripts(_wf: &Workflow, _r: &mut ValidationReport) {}
fn check_templates(_wf: &Workflow, _r: &mut ValidationReport) {}
fn check_refs(_wf: &Workflow, _ctx: &ValidationContext, _r: &mut ValidationReport) {}
```

(The real implementation is task 4; that's also where `EdgeCondition` and `ExecutorRef` from the import get used.)

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-core --test validate_structure_test`
Expected: 7 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): validator part 1 - graph structure rules V01-V08"
```

---

### Task 4: Validator, part 2 - conditions, cycles, templates, references

**Files:**
- Modify: `crates/wf-core/src/validate.rs` (replace the stubs)
- Create: `crates/wf-core/tests/validate_semantics_test.rs`

**Interfaces:**
- Consumes: everything from tasks 2-3.
- Produces: the full validator contract V01-V15 (the "Minimal validator contract" section of the spec).

- [ ] **Step 1: Failing tests**

`crates/wf-core/tests/validate_semantics_test.rs`:

```rust
use wf_core::schema::Workflow;
use wf_core::validate::{validate, Severity, ValidationContext};

const VALID: &str = include_str!("fixtures/valid.yaml");

fn ctx() -> ValidationContext {
    ValidationContext {
        global_executors: vec!["default".into()],
        profiles: vec!["architect".into(), "fullstack".into()],
    }
}

fn error_codes(yaml: &str) -> Vec<&'static str> {
    let wf = Workflow::from_yaml(yaml).unwrap();
    validate(&wf, &ctx()).issues.iter()
        .filter(|i| i.severity == Severity::Error)
        .map(|i| i.code).collect()
}

#[test]
fn v09_condition_not_covering_outcomes() {
    // remove the failure branch from check
    let bad = VALID.replace(
        "  - { from: check, to: fix,  condition: { type: node_status, node: lint, equals: failure } }\n",
        "",
    );
    assert!(error_codes(&bad).contains(&"V09"));
}

#[test]
fn v10_condition_references_downstream_only_node() {
    // condition references a node that cannot be reached before check
    let bad = VALID.replace(
        "condition: { type: node_status, node: lint, equals: success }",
        "condition: { type: node_status, node: done, equals: success }",
    );
    assert!(error_codes(&bad).contains(&"V10"));
}

#[test]
fn v11_cycle_without_max_loops() {
    let bad = VALID.replace("    max_loops: 3\n", "");
    assert!(error_codes(&bad).contains(&"V11"));
}

#[test]
fn v12_script_path_escapes_version_dir() {
    let bad = VALID.replace("scripts/node-lint.sh", "../../etc/passwd");
    assert!(error_codes(&bad).contains(&"V12"));
}

#[test]
fn v13_template_references_unknown_param() {
    let bad = VALID.replace("{{params.task}}", "{{params.ghost}}");
    assert!(error_codes(&bad).contains(&"V13"));
}

#[test]
fn v13_template_references_unknown_node() {
    let bad = VALID.replace("{{nodes.lint.output}}", "{{nodes.ghost.output}}");
    assert!(error_codes(&bad).contains(&"V13"));
}

#[test]
fn v14_unknown_executor_reference() {
    let bad = VALID.replace("executor: main", "executor: ghost");
    assert!(error_codes(&bad).contains(&"V14"));
}

#[test]
fn v14_unknown_profile_reference() {
    let bad = VALID.replace("profile: architect", "profile: ghost");
    assert!(error_codes(&bad).contains(&"V14"));
}

#[test]
fn v15_duplicate_agent_in_fallbacks() {
    let bad = VALID.replace(
        "      - { agent: codex, model: gpt-5.2-codex }",
        "      - { agent: claude-code, model: claude-opus-4-8 }",
    );
    assert!(error_codes(&bad).contains(&"V15"));
}
```

- [ ] **Step 2: Confirm the tests fail**

Run: `cargo test -p wf-core --test validate_semantics_test`
Expected: FAIL on all tests (the stubs report nothing).

- [ ] **Step 3: Implement the rules**

Replace the stubs in `crates/wf-core/src/validate.rs`:

```rust
fn reachable_from<'a>(adj: &HashMap<&'a str, Vec<&'a str>>, from: &'a str) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut q = VecDeque::from([from]);
    while let Some(id) = q.pop_front() {
        if seen.insert(id) {
            for next in adj.get(id).into_iter().flatten() { q.push_back(next); }
        }
    }
    seen
}

fn check_conditions(wf: &Workflow, r: &mut ValidationReport) {
    let adj = adjacency(wf);
    for n in &wf.nodes {
        if !matches!(n.kind, NodeKind::Condition { .. }) { continue; }
        let out: Vec<_> = wf.edges.iter().filter(|e| e.from == n.id).collect();
        let has_fallback = out.iter().any(|e| e.fallback);
        // V09: node_status branches must cover success and failure (or declare a fallback)
        let mut covered = HashSet::new();
        for e in &out {
            if let Some(EdgeCondition::NodeStatus { equals, .. }) = &e.condition {
                covered.insert(*equals);
            }
        }
        let uses_node_status = out.iter()
            .any(|e| matches!(e.condition, Some(EdgeCondition::NodeStatus { .. })));
        if uses_node_status && covered.len() < 2 && !has_fallback {
            r.error("V09", Some(&n.id),
                "condition edges must cover both success and failure or declare a fallback edge".into());
        }
        // V10: a condition may only reference nodes from which this condition node is reachable
        for e in &out {
            let referenced = match &e.condition {
                Some(EdgeCondition::NodeStatus { node, .. }) => Some(node),
                Some(EdgeCondition::OutputMatch { node, .. }) => Some(node),
                _ => None,
            };
            if let Some(dep) = referenced {
                let ok = wf.node(dep).is_some()
                    && reachable_from(&adj, dep.as_str()).contains(n.id.as_str());
                if !ok {
                    r.error("V10", Some(&n.id),
                        format!("condition references node `{dep}` that cannot execute before `{}`", n.id));
                }
            }
        }
    }
}

fn check_cycles(wf: &Workflow, r: &mut ValidationReport) {
    // Every cycle must pass through a condition node with max_loops.
    // It's enough to check the SCC: a component with a cycle must contain such a node.
    let ids: Vec<&str> = wf.nodes.iter().map(|n| n.id.as_str()).collect();
    let adj = adjacency(wf);
    // iterative Tarjan
    let index_of: HashMap<&str, usize> = ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let n = ids.len();
    let mut index = vec![usize::MAX; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut sccs: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if index[root] != usize::MAX { continue; }
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(v, ei)) = call.last() {
            if ei == 0 {
                index[v] = counter; low[v] = counter; counter += 1;
                stack.push(v); on_stack[v] = true;
            }
            let neigh: Vec<usize> = adj.get(ids[v]).into_iter().flatten()
                .filter_map(|t| index_of.get(t).copied()).collect();
            if ei < neigh.len() {
                call.last_mut().expect("frame exists").1 += 1;
                let w = neigh[ei];
                if index[w] == usize::MAX {
                    call.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                if low[v] == index[v] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false; comp.push(w);
                        if w == v { break; }
                    }
                    sccs.push(comp);
                }
                call.pop();
                if let Some(&(parent, _)) = call.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }

    let self_loop: HashSet<&str> = wf.edges.iter()
        .filter(|e| e.from == e.to).map(|e| e.from.as_str()).collect();
    for comp in sccs {
        let cyclic = comp.len() > 1 || self_loop.contains(ids[comp[0]]);
        if !cyclic { continue; }
        let has_guard = comp.iter().any(|&i| {
            matches!(wf.node(ids[i]).map(|n| &n.kind),
                Some(NodeKind::Condition { max_loops: Some(_) }))
        });
        if !has_guard {
            let members: Vec<&str> = comp.iter().map(|&i| ids[i]).collect();
            r.error("V11", Some(members[0]),
                format!("cycle [{}] must pass through a condition node with max_loops", members.join(", ")));
        }
    }
}

fn check_scripts(wf: &Workflow, r: &mut ValidationReport) {
    for n in &wf.nodes {
        if let NodeKind::Script { script, .. } = &n.kind {
            let escapes = script.starts_with('/')
                || script.split('/').any(|seg| seg == "..");
            if escapes {
                r.error("V12", Some(&n.id),
                    format!("script path `{script}` must stay inside the version directory"));
            }
        }
    }
}

fn check_templates(wf: &Workflow, r: &mut ValidationReport) {
    let params: HashSet<&str> = wf.params.iter().map(|p| p.name.as_str()).collect();
    let nodes: HashSet<&str> = wf.nodes.iter().map(|n| n.id.as_str()).collect();
    let hooks: HashSet<&str> = wf.nodes.iter().filter_map(|n| match &n.kind {
        NodeKind::Wait { wait_for: crate::schema::WaitFor::Webhook { key }, .. } => Some(key.as_str()),
        _ => None,
    }).collect();

    let mut check_text = |owner: &str, text: &str, r: &mut ValidationReport| {
        for cap in template_refs(text) {
            let parts: Vec<&str> = cap.split('.').collect();
            let ok = match parts.as_slice() {
                ["params", p] => params.contains(p),
                ["nodes", nid, "output" | "report" | "review_note"] => nodes.contains(nid),
                ["run", "instruction" | "context"] => true,
                ["run", "hooks", key] => hooks.contains(key),
                _ => false,
            };
            if !ok {
                r.error("V13", Some(owner), format!("template `{{{{{cap}}}}}` cannot be resolved"));
            }
        }
    };

    for n in &wf.nodes {
        match &n.kind {
            NodeKind::AgentTask { prompt, .. } | NodeKind::Prompt { prompt } => {
                check_text(&n.id, prompt, r)
            }
            _ => {}
        }
    }
}

fn template_refs(text: &str) -> Vec<String> {
    // without a regex dependency: manual scan for {{ ... }}
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if &bytes[i..i + 2] == b"{{" {
            if let Some(end) = text[i + 2..].find("}}") {
                out.push(text[i + 2..i + 2 + end].trim().to_string());
                i += 2 + end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn check_refs(wf: &Workflow, ctx: &ValidationContext, r: &mut ValidationReport) {
    let known_exec = |name: &str| {
        wf.executors.contains_key(name) || ctx.global_executors.iter().any(|g| g == name)
    };
    let mut check_ref = |owner: &str, exec: &ExecutorRef, r: &mut ValidationReport| match exec {
        ExecutorRef::Name(name) if !known_exec(name) => {
            r.error("V14", Some(owner), format!("executor `{name}` is not defined locally or globally"));
        }
        _ => {}
    };
    if let Some(e) = &wf.defaults.executor { check_ref("defaults", e, r); }
    if let Some(s) = &wf.supervisor {
        if let Some(e) = &s.executor { check_ref("supervisor", e, r); }
    }
    for n in &wf.nodes {
        if let NodeKind::AgentTask { executor, profile, .. } = &n.kind {
            if let Some(e) = executor { check_ref(&n.id, e, r); }
            if let Some(p) = profile {
                if !ctx.profiles.iter().any(|x| x == p) {
                    r.error("V14", Some(&n.id), format!("profile `{p}` not found in .wf/profiles"));
                }
            }
        }
    }
    // V15: no repeated agent in fallbacks (including the primary one)
    for (name, ex) in &wf.executors {
        let mut agents = HashSet::from([ex.agent.as_str()]);
        for f in &ex.fallbacks {
            if !agents.insert(f.agent.as_str()) {
                r.error("V15", None,
                    format!("executor `{name}`: duplicate agent `{}` in fallback chain", f.agent));
            }
        }
    }
}
```

- [ ] **Step 4: Run all wf-core tests**

Run: `cargo test -p wf-core`
Expected: all schema + validate tests (structure and semantics) green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): validator part 2 - conditions, cycles, templates, refs V09-V15"
```

---

### Task 5: Atomic writes and .wf initialization

**Files:**
- Create: `crates/wf-core/src/fsutil.rs`, `crates/wf-core/tests/fsutil_test.rs`
- Modify: `crates/wf-core/Cargo.toml` (dev-deps)

**Interfaces:**
- Produces: `fsutil::atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()>`; `registry::init_project(root: &Path) -> std::io::Result<()>` (creates `.wf/{workflows,profiles,runs}` and `.wf/config.yaml`, idempotently) - the init implementation for this task goes into `registry.rs`.

- [ ] **Step 1: Add the tempfile dev-dependency**

Run: `cargo add --dev tempfile -p wf-core`
Expected: the current tempfile version appears in `[dev-dependencies]` (check in Cargo.toml that the version resolved).

- [ ] **Step 2: Failing tests**

`crates/wf-core/tests/fsutil_test.rs`:

```rust
use std::fs;
use wf_core::fsutil::atomic_write;
use wf_core::registry::init_project;

#[test]
fn atomic_write_creates_file_with_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("current");
    atomic_write(&path, b"1.0.0").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "1.0.0");
    // a repeat write overwrites atomically
    atomic_write(&path, b"1.1.0").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "1.1.0");
    // no temp files left behind
    let leftovers: Vec<_> = fs::read_dir(dir.path()).unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn init_creates_wf_structure_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    for sub in ["workflows", "profiles", "runs"] {
        assert!(dir.path().join(".wf").join(sub).is_dir(), "missing {sub}");
    }
    assert!(dir.path().join(".wf/config.yaml").is_file());
    // a repeat init does not fail and does not overwrite the config
    fs::write(dir.path().join(".wf/config.yaml"), "port: 9999\n").unwrap();
    init_project(dir.path()).unwrap();
    assert_eq!(fs::read_to_string(dir.path().join(".wf/config.yaml")).unwrap(), "port: 9999\n");
}
```

- [ ] **Step 3: Confirm the tests fail**

Run: `cargo test -p wf-core --test fsutil_test`
Expected: FAIL (functions do not exist).

- [ ] **Step 4: Implement**

`crates/wf-core/src/fsutil.rs`:

```rust
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

/// Control files are only ever written this way: temp + fsync + atomic rename (spec 4.3).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}
```

In `crates/wf-core/src/registry.rs` (start of the module; the rest is task 6):

```rust
use std::io;
use std::path::Path;

use crate::fsutil::atomic_write;

const DEFAULT_CONFIG: &str = "# wf project config\n# server:\n#   port: 7321\n";

pub fn init_project(root: &Path) -> io::Result<()> {
    let wf = root.join(".wf");
    for sub in ["workflows", "profiles", "runs"] {
        std::fs::create_dir_all(wf.join(sub))?;
    }
    let config = wf.join("config.yaml");
    if !config.exists() {
        atomic_write(&config, DEFAULT_CONFIG.as_bytes())?;
    }
    Ok(())
}
```

- [ ] **Step 5: Run the tests and commit**

Run: `cargo test -p wf-core --test fsutil_test`
Expected: 2 passed.

```bash
git add -A
git commit -m "feat(core): atomic file writes and .wf project init"
```

---

### Task 6: Workflow registry

**Files:**
- Modify: `crates/wf-core/src/registry.rs`
- Create: `crates/wf-core/tests/registry_test.rs`

**Interfaces:**
- Consumes: `schema::Workflow`, `fsutil::atomic_write`.
- Produces:
  - `registry::Registry` with methods:
    - `Registry::open(root: &Path) -> Result<Registry, RegistryError>` (root - the project folder containing `.wf`),
    - `list(&self) -> Result<Vec<WorkflowSummary>, RegistryError>`,
    - `load(&self, id: &str, version: Option<&str>) -> Result<LoadedWorkflow, RegistryError>`,
    - `profiles(&self) -> Vec<String>` (subfolder names under `.wf/profiles`).
  - `WorkflowSummary { id: String, name: String, description: String, current: String, versions: Vec<String> }`
  - `LoadedWorkflow { workflow: Workflow, yaml: String, layout: Option<serde_json::Value>, version: String }`
  - `RegistryError` (thiserror): `NotFound`, `NoCurrent`, `VersionMismatch { file: String, dir: String }`, `Io`, `Schema`.

- [ ] **Step 1: Failing tests**

`crates/wf-core/tests/registry_test.rs`:

```rust
use std::fs;
use std::path::Path;
use wf_core::registry::{init_project, Registry, RegistryError};

const VALID: &str = include_str!("fixtures/valid.yaml");

fn seed(root: &Path) {
    init_project(root).unwrap();
    let vdir = root.join(".wf/workflows/implement-task/1.0.0");
    fs::create_dir_all(vdir.join("scripts")).unwrap();
    fs::write(vdir.join("workflow.yaml"), VALID).unwrap();
    fs::write(root.join(".wf/workflows/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(root.join(".wf/workflows/implement-task/layouts")).unwrap();
    fs::write(
        root.join(".wf/workflows/implement-task/layouts/1.0.0.yaml"),
        "nodes:\n  - { id: plan, x: 10, y: 20 }\n",
    ).unwrap();
    fs::create_dir_all(root.join(".wf/profiles/architect")).unwrap();
}

#[test]
fn lists_workflows_with_versions() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let list = reg.list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "implement-task");
    assert_eq!(list[0].current, "1.0.0");
    assert_eq!(list[0].versions, vec!["1.0.0"]);
    assert_eq!(list[0].name, "Implement Task");
}

#[test]
fn loads_current_version_with_layout() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    let loaded = reg.load("implement-task", None).unwrap();
    assert_eq!(loaded.version, "1.0.0");
    assert_eq!(loaded.workflow.id, "implement-task");
    let layout = loaded.layout.expect("layout must load");
    assert_eq!(layout["nodes"][0]["id"], "plan");
    assert_eq!(reg.profiles(), vec!["architect".to_string()]);
}

#[test]
fn version_mismatch_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let vdir = dir.path().join(".wf/workflows/implement-task/1.0.0");
    let patched = VALID.replace("version: 1.0.0", "version: 9.9.9");
    fs::write(vdir.join("workflow.yaml"), patched).unwrap();
    let reg = Registry::open(dir.path()).unwrap();
    match reg.load("implement-task", None) {
        Err(RegistryError::VersionMismatch { file, dir }) => {
            assert_eq!(file, "9.9.9");
            assert_eq!(dir, "1.0.0");
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[test]
fn unknown_workflow_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let reg = Registry::open(dir.path()).unwrap();
    assert!(matches!(reg.load("ghost", None), Err(RegistryError::NotFound(_))));
}
```

- [ ] **Step 2: Confirm the tests fail**

Run: `cargo test -p wf-core --test registry_test`
Expected: FAIL (types do not exist).

- [ ] **Step 3: Implement the registry**

Extend `crates/wf-core/src/registry.rs`:

```rust
use std::fs;
use std::path::PathBuf;

use serde::Serialize;

use crate::schema::{SchemaError, Workflow};

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("workflow `{0}` not found")]
    NotFound(String),
    #[error("workflow `{0}` has no current pointer")]
    NoCurrent(String),
    #[error("version in workflow.yaml (`{file}`) does not match directory (`{dir}`)")]
    VersionMismatch { file: String, dir: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error("layout parse error: {0}")]
    Layout(String),
}

#[derive(Debug, Serialize)]
pub struct WorkflowSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub current: String,
    pub versions: Vec<String>,
}

#[derive(Debug)]
pub struct LoadedWorkflow {
    pub workflow: Workflow,
    pub yaml: String,
    pub layout: Option<serde_json::Value>,
    pub version: String,
}

pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn open(root: &Path) -> Result<Self, RegistryError> {
        let wf = root.join(".wf");
        if !wf.is_dir() {
            return Err(RegistryError::NotFound(".wf".into()));
        }
        Ok(Self { root: root.to_path_buf() })
    }

    fn workflows_dir(&self) -> PathBuf { self.root.join(".wf/workflows") }

    pub fn list(&self) -> Result<Vec<WorkflowSummary>, RegistryError> {
        let mut out = Vec::new();
        let dir = self.workflows_dir();
        if !dir.is_dir() { return Ok(out); }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() { continue; }
            let id = entry.file_name().to_string_lossy().to_string();
            let current = self.read_current(&id)?;
            let mut versions: Vec<String> = fs::read_dir(entry.path())?
                .filter_map(Result::ok)
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|name| name != "layouts")
                .collect();
            versions.sort();
            let loaded = self.load(&id, Some(&current))?;
            out.push(WorkflowSummary {
                id,
                name: loaded.workflow.name.clone(),
                description: loaded.workflow.description.clone(),
                current,
                versions,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn read_current(&self, id: &str) -> Result<String, RegistryError> {
        let p = self.workflows_dir().join(id).join("current");
        if !p.is_file() { return Err(RegistryError::NoCurrent(id.into())); }
        Ok(fs::read_to_string(p)?.trim().to_string())
    }

    pub fn load(&self, id: &str, version: Option<&str>) -> Result<LoadedWorkflow, RegistryError> {
        let base = self.workflows_dir().join(id);
        if !base.is_dir() { return Err(RegistryError::NotFound(id.into())); }
        let version = match version {
            Some(v) => v.to_string(),
            None => self.read_current(id)?,
        };
        let yaml_path = base.join(&version).join("workflow.yaml");
        if !yaml_path.is_file() { return Err(RegistryError::NotFound(format!("{id}@{version}"))); }
        let yaml = fs::read_to_string(&yaml_path)?;
        let workflow = Workflow::from_yaml(&yaml)?;
        if workflow.version != version {
            return Err(RegistryError::VersionMismatch {
                file: workflow.version.clone(),
                dir: version,
            });
        }
        let layout_path = base.join("layouts").join(format!("{version}.yaml"));
        let layout = if layout_path.is_file() {
            let raw = fs::read_to_string(&layout_path)?;
            let val: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw)
                .map_err(|e| RegistryError::Layout(e.to_string()))?;
            Some(serde_json::to_value(val).map_err(|e| RegistryError::Layout(e.to_string()))?)
        } else {
            None
        };
        Ok(LoadedWorkflow { workflow, yaml, layout, version })
    }

    pub fn profiles(&self) -> Vec<String> {
        let dir = self.root.join(".wf/profiles");
        let mut out: Vec<String> = fs::read_dir(dir).ok().into_iter().flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-core`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(core): workflow registry - versions, current pointer, layouts, profiles"
```

---

### Task 7: CLI - init, list, validate

**Files:**
- Modify: `crates/wf-cli/src/main.rs`, `crates/wf-cli/Cargo.toml`
- Create: `crates/wf-cli/tests/cli_test.rs`

**Interfaces:**
- Consumes: `wf_core::registry::{init_project, Registry}`, `wf_core::validate::{validate, ValidationContext}`.
- Produces: the commands `wf init`, `wf list`, `wf validate [name]`, `wf --version`. Exit codes: 0 success, 1 validation error, 2 environment error (no .wf, etc.).

- [ ] **Step 1: Add dev-dependencies**

Run: `cargo add --dev assert_cmd predicates tempfile -p wf-cli`
Expected: versions resolved (current as of install time; check with `cargo tree -p wf-cli --depth 1 -e dev`).

- [ ] **Step 2: Failing tests**

`crates/wf-cli/tests/cli_test.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

const VALID: &str = include_str!("../../wf-core/tests/fixtures/valid.yaml");

fn wf() -> Command { Command::cargo_bin("wf").unwrap() }

fn seeded_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    wf().arg("init").current_dir(dir.path()).assert().success();
    let vdir = dir.path().join(".wf/workflows/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), VALID).unwrap();
    fs::write(dir.path().join(".wf/workflows/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(dir.path().join(".wf/profiles/architect")).unwrap();
    dir
}

#[test]
fn init_creates_structure() {
    let dir = tempfile::tempdir().unwrap();
    wf().arg("init").current_dir(dir.path())
        .assert().success()
        .stdout(predicate::str::contains(".wf"));
    assert!(dir.path().join(".wf/workflows").is_dir());
}

#[test]
fn list_shows_workflow() {
    let dir = seeded_dir();
    wf().arg("list").current_dir(dir.path())
        .assert().success()
        .stdout(predicate::str::contains("implement-task"))
        .stdout(predicate::str::contains("1.0.0"));
}

#[test]
fn validate_ok_workflow() {
    let dir = seeded_dir();
    wf().args(["validate", "implement-task"]).current_dir(dir.path())
        .assert().success()
        .stdout(predicate::str::contains("OK"));
}

#[test]
fn validate_broken_workflow_fails_with_code() {
    let dir = seeded_dir();
    let vdir = dir.path().join(".wf/workflows/implement-task/1.0.0");
    let bad = VALID.replace("{{params.task}}", "{{params.ghost}}");
    fs::write(vdir.join("workflow.yaml"), bad).unwrap();
    wf().args(["validate", "implement-task"]).current_dir(dir.path())
        .assert().code(1)
        .stdout(predicate::str::contains("V13"));
}

#[test]
fn list_without_wf_dir_fails() {
    let dir = tempfile::tempdir().unwrap();
    wf().arg("list").current_dir(dir.path()).assert().code(2);
}
```

- [ ] **Step 3: Confirm the tests fail**

Run: `cargo test -p wf-cli`
Expected: FAIL (commands are not implemented).

- [ ] **Step 4: Implement the CLI**

`crates/wf-cli/src/main.rs`:

```rust
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use wf_core::registry::{init_project, Registry};
use wf_core::validate::{validate, Severity, ValidationContext};

#[derive(Parser)]
#[command(name = "wf", version, about = "Workflows CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Create empty .wf structure
    Init,
    /// List workflows and versions
    List,
    /// Validate workflow schema
    Validate { name: Option<String> },
    /// Start web server (see Task 8/13)
    Serve {
        #[arg(long, default_value_t = 7321)]
        port: u16,
        #[arg(long)]
        no_open: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = std::env::current_dir().expect("cwd");
    match cli.command {
        Some(Command::Init) => run_init(&root),
        Some(Command::List) => run_list(&root),
        Some(Command::Validate { name }) => run_validate(&root, name),
        Some(Command::Serve { port, no_open }) => serve(root, port, no_open),
        None => serve(root, 7321, false),
    }
}

fn run_init(root: &PathBuf) -> ExitCode {
    match init_project(root) {
        Ok(()) => {
            println!("initialized {}", root.join(".wf").display());
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("init failed: {e}"); ExitCode::from(2) }
    }
}

fn open_registry(root: &PathBuf) -> Result<Registry, ExitCode> {
    Registry::open(root).map_err(|e| {
        eprintln!("no project here: {e} (run `wf init`)");
        ExitCode::from(2)
    })
}

fn run_list(root: &PathBuf) -> ExitCode {
    let reg = match open_registry(root) { Ok(r) => r, Err(c) => return c };
    match reg.list() {
        Ok(list) if list.is_empty() => { println!("no workflows in .wf/workflows"); ExitCode::SUCCESS }
        Ok(list) => {
            for wfs in list {
                println!("{}\t{}\t(current: {}, versions: {})",
                    wfs.id, wfs.name, wfs.current, wfs.versions.join(", "));
            }
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("list failed: {e}"); ExitCode::from(2) }
    }
}

fn run_validate(root: &PathBuf, name: Option<String>) -> ExitCode {
    let reg = match open_registry(root) { Ok(r) => r, Err(c) => return c };
    let names: Vec<String> = match name {
        Some(n) => vec![n],
        None => match reg.list() {
            Ok(l) => l.into_iter().map(|w| w.id).collect(),
            Err(e) => { eprintln!("list failed: {e}"); return ExitCode::from(2); }
        },
    };
    let ctx = ValidationContext {
        global_executors: vec![], // global agent config - phase 2
        profiles: reg.profiles(),
    };
    let mut failed = false;
    for id in names {
        match reg.load(&id, None) {
            Ok(loaded) => {
                let report = validate(&loaded.workflow, &ctx);
                for issue in &report.issues {
                    let sev = match issue.severity { Severity::Error => "error", Severity::Warning => "warning" };
                    println!("{id}: {sev} {} {}{}", issue.code, issue.message,
                        issue.node.as_ref().map(|n| format!(" (node `{n}`)")).unwrap_or_default());
                }
                if report.is_valid() {
                    println!("{id}: OK");
                } else {
                    failed = true;
                }
            }
            Err(e) => { println!("{id}: error {e}"); failed = true; }
        }
    }
    if failed { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn serve(_root: PathBuf, _port: u16, _no_open: bool) -> ExitCode {
    eprintln!("serve is implemented in Task 13");
    ExitCode::from(2)
}
```

- [ ] **Step 5: Run the tests and commit**

Run: `cargo test -p wf-cli`
Expected: 5 passed.

```bash
git add -A
git commit -m "feat(cli): init, list, validate commands"
```

---

### Task 8: HTTP API and serve.lock

**Files:**
- Modify: `crates/wf-server/src/lib.rs`, `crates/wf-server/Cargo.toml`
- Create: `crates/wf-server/tests/api_test.rs`

**Interfaces:**
- Consumes: `Registry`, `validate`.
- Produces:
  - `wf_server::AppState::new(root: PathBuf) -> AppState` (inside: `broadcast::Sender<String>` for WS, field `pub events: broadcast::Sender<String>`),
  - `wf_server::build_router(state: AppState) -> axum::Router`,
  - `wf_server::lock::{write_lock, remove_lock, LockInfo}`; `LockInfo { port: u16, pid: u32, root_fingerprint: String, instance_id: String }`, file `.wf/serve.lock` (JSON).
  - Endpoints: `GET /api/health` -> `{"status":"ok"}`; `GET /api/workflows` -> an array of `WorkflowSummary`; `GET /api/workflows/{id}` (+`?version=`) -> `{ "id", "version", "yaml", "workflow": <json>, "layout": <json|null>, "validation": [{code,severity,message,node}] }`.

- [ ] **Step 1: Add dev-dependencies**

Run: `cargo add --dev tower tempfile http-body-util -p wf-server && cargo add uuid --features v4 -p wf-server`
Expected: versions resolved.

- [ ] **Step 2: Failing tests**

`crates/wf-server/tests/api_test.rs`:

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::fs;
use tower::ServiceExt;
use wf_server::{build_router, AppState};

const VALID: &str = include_str!("../../wf-core/tests/fixtures/valid.yaml");

fn seed() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    wf_core::registry::init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".wf/workflows/implement-task/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), VALID).unwrap();
    fs::write(dir.path().join(".wf/workflows/implement-task/current"), "1.0.0").unwrap();
    fs::create_dir_all(dir.path().join(".wf/profiles/architect")).unwrap();
    dir
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app.oneshot(Request::get(uri).body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn health_ok() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn workflows_list() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/workflows").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json[0]["id"], "implement-task");
    assert_eq!(json[0]["current"], "1.0.0");
}

#[tokio::test]
async fn workflow_detail_includes_model_and_validation() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, json) = get_json(app, "/api/workflows/implement-task").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["version"], "1.0.0");
    assert_eq!(json["workflow"]["nodes"][0]["type"], "start");
    assert!(json["yaml"].as_str().unwrap().contains("implement-task"));
    assert!(json["validation"].as_array().is_some());
}

#[tokio::test]
async fn unknown_workflow_404() {
    let dir = seed();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let (status, _) = get_json(app, "/api/workflows/ghost").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn lock_write_and_remove() {
    let dir = seed();
    let info = wf_server::lock::write_lock(dir.path(), 7321).unwrap();
    assert_eq!(info.port, 7321);
    let raw = fs::read_to_string(dir.path().join(".wf/serve.lock")).unwrap();
    assert!(raw.contains("root_fingerprint"));
    wf_server::lock::remove_lock(dir.path()).unwrap();
    assert!(!dir.path().join(".wf/serve.lock").exists());
}
```

- [ ] **Step 3: Confirm the tests fail**

Run: `cargo test -p wf-server`
Expected: FAIL (modules do not exist).

- [ ] **Step 4: Implement**

`crates/wf-server/src/lib.rs`:

```rust
pub mod lock;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use tokio::sync::broadcast;
use wf_core::registry::{Registry, RegistryError};
use wf_core::validate::{validate, Severity, ValidationContext};

#[derive(Clone)]
pub struct AppState {
    pub root: Arc<PathBuf>,
    pub events: broadcast::Sender<String>,
}

impl AppState {
    pub fn new(root: PathBuf) -> Self {
        let (events, _) = broadcast::channel(64);
        Self { root: Arc::new(root), events }
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/workflows", get(list_workflows))
        .route("/api/workflows/{id}", get(get_workflow))
        .with_state(state)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

fn registry(state: &AppState) -> Result<Registry, (StatusCode, String)> {
    Registry::open(&state.root)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn list_workflows(State(state): State<AppState>) -> impl IntoResponse {
    let reg = match registry(&state) { Ok(r) => r, Err(e) => return e.into_response() };
    match reg.list() {
        Ok(list) => Json(list).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct DetailQuery { version: Option<String> }

async fn get_workflow(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    Query(q): Query<DetailQuery>,
) -> impl IntoResponse {
    let reg = match registry(&state) { Ok(r) => r, Err(e) => return e.into_response() };
    match reg.load(&id, q.version.as_deref()) {
        Ok(loaded) => {
            let ctx = ValidationContext { global_executors: vec![], profiles: reg.profiles() };
            let report = validate(&loaded.workflow, &ctx);
            let validation: Vec<serde_json::Value> = report.issues.iter().map(|i| serde_json::json!({
                "code": i.code,
                "severity": match i.severity { Severity::Error => "error", Severity::Warning => "warning" },
                "message": i.message,
                "node": i.node,
            })).collect();
            Json(serde_json::json!({
                "id": id,
                "version": loaded.version,
                "yaml": loaded.yaml,
                "workflow": loaded.workflow,
                "layout": loaded.layout,
                "validation": validation,
            })).into_response()
        }
        Err(RegistryError::NotFound(what)) => (StatusCode::NOT_FOUND, what).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
```

`crates/wf-server/src/lock.rs`:

```rust
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wf_core::fsutil::atomic_write;

#[derive(Debug, Serialize, Deserialize)]
pub struct LockInfo {
    pub port: u16,
    pub pid: u32,
    pub root_fingerprint: String,
    pub instance_id: String,
}

fn fingerprint(root: &Path) -> String {
    let mut h = DefaultHasher::new();
    root.to_string_lossy().hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn write_lock(root: &Path, port: u16) -> io::Result<LockInfo> {
    let info = LockInfo {
        port,
        pid: std::process::id(),
        root_fingerprint: fingerprint(root),
        instance_id: uuid::Uuid::new_v4().to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&info).map_err(io::Error::other)?;
    atomic_write(&root.join(".wf/serve.lock"), &bytes)?;
    Ok(info)
}

pub fn remove_lock(root: &Path) -> io::Result<()> {
    let p = root.join(".wf/serve.lock");
    if p.exists() { std::fs::remove_file(p)?; }
    Ok(())
}
```

- [ ] **Step 5: Run the tests and commit**

Run: `cargo test -p wf-server`
Expected: 5 passed.

```bash
git add -A
git commit -m "feat(server): http api (health, workflows, detail) and serve.lock"
```

---

### Task 9: File watcher and WebSocket

**Files:**
- Modify: `crates/wf-server/src/lib.rs`
- Create: `crates/wf-server/src/watch.rs`, `crates/wf-server/tests/ws_test.rs`

**Interfaces:**
- Consumes: `AppState.events` (broadcast from task 8).
- Produces:
  - `watch::spawn_watcher(root: PathBuf, tx: broadcast::Sender<String>) -> notify::Result<notify::RecommendedWatcher>` - watches `.wf/workflows` and `.wf/profiles`, sends the string `{"type":"workflows_changed"}` into the channel on any event;
  - WS endpoint `GET /api/ws`: on connect, subscribes to the channel and forwards messages to the client.

- [ ] **Step 1: Failing tests**

`crates/wf-server/tests/ws_test.rs`:

```rust
use std::fs;
use std::time::Duration;
use wf_server::{build_router, AppState};

#[tokio::test]
async fn watcher_publishes_on_file_change() {
    let dir = tempfile::tempdir().unwrap();
    wf_core::registry::init_project(dir.path()).unwrap();
    let state = AppState::new(dir.path().to_path_buf());
    let mut rx = state.events.subscribe();
    let _watcher = wf_server::watch::spawn_watcher(
        dir.path().to_path_buf(),
        state.events.clone(),
    ).unwrap();
    // give the watcher time to initialize
    tokio::time::sleep(Duration::from_millis(300)).await;
    fs::create_dir_all(dir.path().join(".wf/workflows/demo/1.0.0")).unwrap();
    fs::write(dir.path().join(".wf/workflows/demo/1.0.0/workflow.yaml"), "id: demo").unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await.expect("timeout waiting for event").expect("channel closed");
    assert!(msg.contains("workflows_changed"));
}

#[tokio::test]
async fn ws_route_exists() {
    // sanity: the /api/ws route responds with an upgrade error to a plain GET, not a 404
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    wf_core::registry::init_project(dir.path()).unwrap();
    let app = build_router(AppState::new(dir.path().to_path_buf()));
    let res = app.oneshot(Request::get("/api/ws").body(Body::empty()).unwrap()).await.unwrap();
    assert_ne!(res.status(), StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Confirm the tests fail**

Run: `cargo test -p wf-server --test ws_test`
Expected: FAIL (`watch` does not exist, /api/ws returns 404).

- [ ] **Step 3: Implement**

`crates/wf-server/src/watch.rs`:

```rust
use std::path::PathBuf;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;

pub fn spawn_watcher(
    root: PathBuf,
    tx: broadcast::Sender<String>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if res.is_ok() {
            // Ignore the send error: no subscribers means nothing to send.
            let _ = tx.send(r#"{"type":"workflows_changed"}"#.to_string());
        }
    })?;
    for sub in ["workflows", "profiles"] {
        let p = root.join(".wf").join(sub);
        if p.is_dir() {
            watcher.watch(&p, RecursiveMode::Recursive)?;
        }
    }
    Ok(watcher)
}
```

In `crates/wf-server/src/lib.rs` add the module and route:

```rust
pub mod watch;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};

// add to build_router:
//   .route("/api/ws", get(ws_handler))

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_loop(socket, state))
}

async fn ws_loop(mut socket: WebSocket, state: AppState) {
    let mut rx = state.events.subscribe();
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => {
                if incoming.is_none() { break; } // client disconnected
            }
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-server`
Expected: all green (including task 8).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(server): file watcher and websocket change stream"
```

---

### Task 10: Frontend scaffold

**Files:**
- Create: `web/` (Vite scaffold), `web/vite.config.ts`, `web/src/lib/api.ts`, `web/src/lib/types.ts`

**Interfaces:**
- Produces: a `bun run build` build -> `web/dist`; a `bun run dev` dev server with a proxy `/api` -> `http://127.0.0.1:7321`; types `WorkflowSummary`, `WorkflowDetail`, `WfNode`, `WfEdge`; functions `fetchWorkflows(): Promise<WorkflowSummary[]>`, `fetchWorkflow(id: string): Promise<WorkflowDetail>`.

- [ ] **Step 1: Create the project and install dependencies**

```bash
cd /Users/techmeat/www/projects/omniteamhq/workflows
bun create vite web --template svelte-ts
cd web
bun install
bun add @xyflow/svelte @dagrejs/dagre
bun add -d vitest
```

Expected: a `web/` folder with Svelte 5 + Vite 8 (check versions against Global Constraints; on a major-version mismatch, stop and check the changelog).

- [ ] **Step 2: Configure the proxy and the test script**

`web/vite.config.ts`:

```ts
import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:7321',
        ws: true,
      },
    },
  },
})
```

In `web/package.json`, add `"test": "vitest run"` to `scripts`.

- [ ] **Step 3: Types and API client**

`web/src/lib/types.ts`:

```ts
export interface WorkflowSummary {
  id: string
  name: string
  description: string
  current: string
  versions: string[]
}

export interface WfNode {
  id: string
  type: string
  title?: string | null
  [key: string]: unknown
}

export interface WfEdge {
  from: string
  to: string
  condition?: { type: string; [key: string]: unknown } | null
  fallback?: boolean
}

export interface LayoutNode { id: string; x: number; y: number }

export interface WorkflowDetail {
  id: string
  version: string
  yaml: string
  workflow: { id: string; name: string; nodes: WfNode[]; edges: WfEdge[] }
  layout: { nodes?: LayoutNode[] } | null
  validation: { code: string; severity: string; message: string; node?: string | null }[]
}
```

`web/src/lib/api.ts`:

```ts
import type { WorkflowDetail, WorkflowSummary } from './types'

async function getJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`)
  return res.json() as Promise<T>
}

export const fetchWorkflows = () => getJson<WorkflowSummary[]>('/api/workflows')
export const fetchWorkflow = (id: string) =>
  getJson<WorkflowDetail>(`/api/workflows/${encodeURIComponent(id)}`)
```

- [ ] **Step 4: Verify the build**

Run: `cd web && bun run build`
Expected: `web/dist/index.html` created, build without errors.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): svelte + vite scaffold with api client and proxy"
```

---

### Task 11: Converting the model into a svelte-flow graph + auto-layout

**Files:**
- Create: `web/src/lib/graph.ts`, `web/src/lib/graph.test.ts`

**Interfaces:**
- Consumes: types from `web/src/lib/types.ts`.
- Produces: `toFlow(workflow: WorkflowDetail['workflow'], layout: WorkflowDetail['layout']): { nodes: FlowNode[]; edges: FlowEdge[] }`, where `FlowNode = { id: string; position: { x: number; y: number }; data: { title: string; kind: string }; type: 'wfNode' }`, `FlowEdge = { id: string; source: string; target: string; label?: string }`. If a layout is absent or does not cover a node, dagre computes the positions (top-to-bottom direction).

- [ ] **Step 1: Failing tests**

`web/src/lib/graph.test.ts`:

```ts
import { describe, expect, it } from 'vitest'
import { toFlow } from './graph'

const workflow = {
  id: 'demo',
  name: 'Demo',
  nodes: [
    { id: 'start', type: 'start', title: 'Start' },
    { id: 'a', type: 'agent_task', title: 'A' },
    { id: 'done', type: 'finish', title: null },
  ],
  edges: [
    { from: 'start', to: 'a' },
    { from: 'a', to: 'done', condition: { type: 'node_status', node: 'a', equals: 'success' } },
  ],
}

describe('toFlow', () => {
  it('maps nodes and edges', () => {
    const { nodes, edges } = toFlow(workflow, null)
    expect(nodes).toHaveLength(3)
    expect(nodes[0]).toMatchObject({ id: 'start', type: 'wfNode', data: { kind: 'start', title: 'Start' } })
    // a node without a title gets its id as the title
    expect(nodes[2].data.title).toBe('done')
    expect(edges).toHaveLength(2)
    expect(edges[1]).toMatchObject({ source: 'a', target: 'done' })
    expect(edges[1].label).toContain('success')
  })

  it('uses stored layout positions when present', () => {
    const layout = { nodes: [{ id: 'a', x: 111, y: 222 }] }
    const { nodes } = toFlow(workflow, layout)
    const a = nodes.find((n) => n.id === 'a')!
    expect(a.position).toEqual({ x: 111, y: 222 })
  })

  it('auto-layouts nodes without stored positions', () => {
    const { nodes } = toFlow(workflow, null)
    const ys = nodes.map((n) => n.position.y)
    // dagre spreads ranks vertically: start above a, a above done
    expect(ys[0]).toBeLessThan(ys[1])
    expect(ys[1]).toBeLessThan(ys[2])
  })
})
```

- [ ] **Step 2: Confirm the tests fail**

Run: `cd web && bun run test`
Expected: FAIL (`./graph` does not exist).

- [ ] **Step 3: Implement**

`web/src/lib/graph.ts`:

```ts
import dagre from '@dagrejs/dagre'
import type { WorkflowDetail } from './types'

export interface FlowNode {
  id: string
  position: { x: number; y: number }
  data: { title: string; kind: string }
  type: 'wfNode'
}

export interface FlowEdge {
  id: string
  source: string
  target: string
  label?: string
}

type WfModel = WorkflowDetail['workflow']
type WfLayout = WorkflowDetail['layout']

const NODE_W = 200
const NODE_H = 64

function edgeLabel(e: WfModel['edges'][number]): string | undefined {
  const c = e.condition
  if (!c) return undefined
  if (c.type === 'node_status') return `${c.node}: ${c.equals}`
  if (c.type === 'review_status') return `review: ${c.equals}`
  if (c.type === 'output_match') return `match: ${c.pattern}`
  return c.type
}

export function toFlow(workflow: WfModel, layout: WfLayout): { nodes: FlowNode[]; edges: FlowEdge[] } {
  const stored = new Map<string, { x: number; y: number }>()
  for (const n of layout?.nodes ?? []) stored.set(n.id, { x: n.x, y: n.y })

  const needAuto = workflow.nodes.some((n) => !stored.has(n.id))
  const auto = new Map<string, { x: number; y: number }>()
  if (needAuto) {
    const g = new dagre.graphlib.Graph()
    g.setGraph({ rankdir: 'TB', nodesep: 40, ranksep: 60 })
    g.setDefaultEdgeLabel(() => ({}))
    for (const n of workflow.nodes) g.setNode(n.id, { width: NODE_W, height: NODE_H })
    for (const e of workflow.edges) g.setEdge(e.from, e.to)
    dagre.layout(g)
    for (const n of workflow.nodes) {
      const pos = g.node(n.id)
      auto.set(n.id, { x: pos.x - NODE_W / 2, y: pos.y - NODE_H / 2 })
    }
  }

  const nodes: FlowNode[] = workflow.nodes.map((n) => ({
    id: n.id,
    type: 'wfNode',
    position: stored.get(n.id) ?? auto.get(n.id) ?? { x: 0, y: 0 },
    data: { title: n.title ?? n.id, kind: n.type },
  }))

  const edges: FlowEdge[] = workflow.edges.map((e, i) => ({
    id: `e${i}-${e.from}-${e.to}`,
    source: e.from,
    target: e.to,
    label: edgeLabel(e),
  }))

  return { nodes, edges }
}
```

- [ ] **Step 4: Run the tests**

Run: `cd web && bun run test`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): workflow model to svelte-flow conversion with dagre auto-layout"
```

---

### Task 12: List and graph screens + live reload over WS

**Files:**
- Modify: `web/src/App.svelte`
- Create: `web/src/lib/WfNode.svelte`, `web/src/pages/WorkflowList.svelte`, `web/src/pages/WorkflowView.svelte`, `web/src/lib/ws.ts`

**Interfaces:**
- Consumes: `fetchWorkflows`, `fetchWorkflow`, `toFlow`, types from tasks 10-11.
- Produces: hash routing `#/` (list) and `#/wf/<id>` (graph); a `subscribeChanges(cb: () => void): () => void` subscription to `/api/ws`.

- [ ] **Step 1: WS subscription**

`web/src/lib/ws.ts`:

```ts
export function subscribeChanges(cb: () => void): () => void {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws'
  const ws = new WebSocket(`${proto}://${location.host}/api/ws`)
  ws.onmessage = () => cb()
  return () => ws.close()
}
```

- [ ] **Step 2: Node component and pages**

`web/src/lib/WfNode.svelte`:

```svelte
<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte'
  let { data }: { data: { title: string; kind: string } } = $props()
</script>

<div class="wf-node" data-kind={data.kind}>
  <Handle type="target" position={Position.Top} />
  <span class="kind">{data.kind}</span>
  <strong>{data.title}</strong>
  <Handle type="source" position={Position.Bottom} />
</div>

<style>
  .wf-node {
    padding: 8px 12px;
    border: 1px solid #8884;
    border-radius: 8px;
    background: var(--wf-node-bg, #fff);
    min-width: 160px;
  }
  .kind { display: block; font-size: 11px; opacity: 0.6; }
  [data-kind='start'] { border-color: #22a06b; }
  [data-kind='finish'] { border-color: #d97706; }
  [data-kind='condition'] { border-style: dashed; }
</style>
```

`web/src/pages/WorkflowList.svelte`:

```svelte
<script lang="ts">
  import { fetchWorkflows } from '../lib/api'
  import { subscribeChanges } from '../lib/ws'
  import type { WorkflowSummary } from '../lib/types'

  let items = $state<WorkflowSummary[]>([])
  let error = $state<string | null>(null)

  async function load() {
    try {
      items = await fetchWorkflows()
      error = null
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<h1>Workflows</h1>
{#if error}<p class="error">{error}</p>{/if}
{#if items.length === 0 && !error}<p>No workflows in .wf/workflows yet.</p>{/if}
<ul>
  {#each items as w (w.id)}
    <li>
      <a href={`#/wf/${w.id}`}><strong>{w.name}</strong></a>
      <span>({w.id}, current {w.current}, versions: {w.versions.join(', ')})</span>
      <p>{w.description}</p>
    </li>
  {/each}
</ul>
```

`web/src/pages/WorkflowView.svelte`:

```svelte
<script lang="ts">
  import { SvelteFlow, Background, Controls } from '@xyflow/svelte'
  import '@xyflow/svelte/dist/style.css'
  import { fetchWorkflow } from '../lib/api'
  import { toFlow, type FlowEdge, type FlowNode } from '../lib/graph'
  import { subscribeChanges } from '../lib/ws'
  import WfNode from '../lib/WfNode.svelte'

  let { id }: { id: string } = $props()

  let nodes = $state.raw<FlowNode[]>([])
  let edges = $state.raw<FlowEdge[]>([])
  let validation = $state<{ code: string; severity: string; message: string }[]>([])
  let error = $state<string | null>(null)

  const nodeTypes = { wfNode: WfNode }

  async function load() {
    try {
      const detail = await fetchWorkflow(id)
      const flow = toFlow(detail.workflow, detail.layout)
      nodes = flow.nodes
      edges = flow.edges
      validation = detail.validation
      error = null
    } catch (e) {
      error = String(e)
    }
  }

  $effect(() => {
    load()
    return subscribeChanges(load)
  })
</script>

<p><a href="#/">← all workflows</a></p>
{#if error}<p class="error">{error}</p>{/if}
{#if validation.length > 0}
  <ul class="issues">
    {#each validation as v}
      <li class={v.severity}>{v.code}: {v.message}</li>
    {/each}
  </ul>
{/if}
<div style="height: 80vh;">
  <SvelteFlow bind:nodes bind:edges {nodeTypes} fitView
    nodesDraggable={false} nodesConnectable={false} elementsSelectable={false}>
    <Background />
    <Controls />
  </SvelteFlow>
</div>
```

- [ ] **Step 3: Routing in App.svelte**

`web/src/App.svelte` (a full replacement of the template content):

```svelte
<script lang="ts">
  import WorkflowList from './pages/WorkflowList.svelte'
  import WorkflowView from './pages/WorkflowView.svelte'

  let hash = $state(location.hash)
  $effect(() => {
    const onHash = () => (hash = location.hash)
    window.addEventListener('hashchange', onHash)
    return () => window.removeEventListener('hashchange', onHash)
  })

  const wfId = $derived(hash.startsWith('#/wf/') ? decodeURIComponent(hash.slice(5)) : null)
</script>

<main>
  {#if wfId}
    <WorkflowView id={wfId} />
  {:else}
    <WorkflowList />
  {/if}
</main>
```

- [ ] **Step 4: Manual verification via vite dev**

We cannot yet spin up an API "stub" in one terminal (serve is ready in task 13), so we verify the build and unit tests instead:

Run: `cd web && bun run test && bun run build`
Expected: tests green, build successful. Full manual verification of the pages happens in task 13.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(web): list and read-only graph pages with ws live reload"
```

---

### Task 13: Baking in static assets and wf serve

**Files:**
- Modify: `crates/wf-server/src/lib.rs`, `crates/wf-server/Cargo.toml`, `crates/wf-cli/src/main.rs`, `crates/wf-cli/Cargo.toml`

**Interfaces:**
- Consumes: `build_router`, `watch::spawn_watcher`, `lock::{write_lock, remove_lock}`, the built `web/dist`.
- Produces: `wf serve [--port N] [--no-open]` and `wf` with no arguments: init if `.wf` is missing, watcher, lock file, serving static assets from the binary, opening the browser. `wf_server::run_server(root: PathBuf, port: u16) -> anyhow::Result<()>` (blocking async).

- [ ] **Step 1: Add rust-embed and static assets**

Run: `cargo add rust-embed@8.12 mime_guess -p wf-server && cargo add open anyhow -p wf-cli`

In `crates/wf-server/src/lib.rs`, add:

```rust
use axum::http::header;
use axum::response::Response;

#[derive(rust_embed::Embed)]
#[folder = "../../web/dist"]
struct WebAssets;

async fn static_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let asset = WebAssets::get(path).or_else(|| WebAssets::get("index.html"));
    match asset {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], content.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "web assets not built").into_response(),
    }
}

// add a fallback to build_router:
//   .fallback(static_handler)

pub async fn run_server(root: PathBuf, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState::new(root.clone());
    let _watcher = watch::spawn_watcher(root.clone(), state.events.clone())?;
    let _lock = lock::write_lock(&root, port)?;
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("wf serve: http://127.0.0.1:{port}");
    let result = axum::serve(listener, app).await;
    lock::remove_lock(&root)?;
    result?;
    Ok(())
}
```

Note: rust-embed's `#[folder]` resolves relative to the `CARGO_MANIFEST_DIR` of the `wf-server` crate; if the `web/dist` folder does not exist, build the frontend (`cd web && bun run build`) before `cargo build`. In a debug build rust-embed reads files from disk, in release it bakes them in.

- [ ] **Step 2: Wire serve into the CLI**

Replace the `serve` function in `crates/wf-cli/src/main.rs`:

```rust
fn serve(root: PathBuf, port: u16, no_open: bool) -> ExitCode {
    if !root.join(".wf").is_dir() {
        if let Err(e) = init_project(&root) {
            eprintln!("init failed: {e}");
            return ExitCode::from(2);
        }
        println!("initialized {}", root.join(".wf").display());
    }
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if !no_open {
        let url = format!("http://127.0.0.1:{port}");
        let _ = open::that_detached(&url);
    }
    match rt.block_on(wf_server::run_server(root, port)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("serve failed: {e}"); ExitCode::from(2) }
    }
}
```

- [ ] **Step 3: Build everything and verify compilation with tests**

```bash
cd web && bun run build && cd ..
cargo test --workspace
```

Expected: all tests green, build successful.

- [ ] **Step 4: End-to-end manual verification**

```bash
mkdir -p /tmp/wf-demo && cd /tmp/wf-demo
/Users/techmeat/www/projects/omniteamhq/workflows/target/debug/wf serve --no-open &
sleep 1
mkdir -p .wf/workflows/implement-task/1.0.0
cp /Users/techmeat/www/projects/omniteamhq/workflows/crates/wf-core/tests/fixtures/valid.yaml \
   .wf/workflows/implement-task/1.0.0/workflow.yaml
echo -n "1.0.0" > .wf/workflows/implement-task/current
mkdir -p .wf/profiles/architect
curl -s http://127.0.0.1:7321/api/workflows | head -c 300
```

Expected: JSON with a list containing `implement-task`. Open `http://127.0.0.1:7321` in a browser: the list is visible; clicking a workflow shows a graph of 7 nodes with auto-layout; editing `workflow.yaml` on disk redraws the page without a reload (WS). Stop the background server process (`kill %1`), verify that `.wf/serve.lock` is removed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(cli,server): wf serve with embedded web assets, watcher and lock"
```

---

### Task 14: Version 0.1.0, CHANGELOG, final verification

**Files:**
- Create: `CHANGELOG.md`, `README.md`

**Interfaces:**
- Produces: the phase 1 release point; `wf --version` prints `wf 0.1.0`.

- [ ] **Step 1: CHANGELOG and README**

`CHANGELOG.md`:

```markdown
# Changelog

Format: [Keep a Changelog], versions: semver (major - breaking changes to the workflow.yaml schema or the CLI, minor - new features, patch - fixes).

## [0.1.0] - phase 1

### Added
- workflow.yaml schema (schema: 1) and validator V01-V15.
- .wf/ structure (workflows/versions/current/layouts/profiles/runs), atomic writes for control files.
- CLI: wf init, wf list, wf validate, wf serve.
- Web: list of workflows, read-only graph (svelte-flow, dagre auto-layout), live updates from disk (watcher + WebSocket).
- serve.lock with a root fingerprint and instance id.
```

`README.md`:

```markdown
# wf - Workflows CLI

A local runner for agentic workflows: YAML descriptions, web visualization via svelte-flow, semver versioning.

Status: phase 1 (core and viewer). Spec: docs/superpowers/specs/2026-07-08-workflows-cli-design.md.

## Quick start

    cargo build --release          # first: cd web && bun install && bun run build
    ./target/release/wf serve      # brings up the web UI on http://127.0.0.1:7321

## Commands

    wf init            create the .wf structure
    wf list            workflows and versions
    wf validate [name] check the schema
    wf serve           web server (port 7321)

## Development

    cargo test --workspace        # core, server, CLI tests
    cd web && bun run test        # frontend tests (vitest)
    cd web && bun run dev         # dev frontend with proxy to :7321
```

- [ ] **Step 2: Final full run**

```bash
cargo test --workspace
cd web && bun run test && bun run build && cd ..
cargo run -p wf-cli -- --version
```

Expected: all tests green; output `wf 0.1.0`.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "docs: changelog and readme for 0.1.0 (phase 1)"
git tag v0.1.0
```

---

## What is deliberately NOT part of phase 1 (for the reviewer)

Execution (the engine, event log, agent adapters), the supervisor agent, the MCP server, a visual editor with recording, human_review/wait, parallel branches, `wf doctor`, `wf dev`, the global config `~/.config/wf/config.yaml` (the validator accepts an empty list of global executors) - all of this is phases 2-5, each with its own plan. The `join` field and the wait/human_review node types are parsed and validated already now, so the schema is complete from the first release.
