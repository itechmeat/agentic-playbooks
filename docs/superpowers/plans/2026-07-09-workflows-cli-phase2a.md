# Workflows CLI, Phase 2a (execution engine, headless/CLI) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A deterministic run engine with event sourcing: `wf run/runs/resume` execute workflows (start, prompt, condition, agent_task via headless Claude Code, script on sh), with named executors, retry and fallbacks, an accumulating context, a one-off instruction, and an immutable version snapshot per run. Replaces the minimal runner in `wf-cli/src/run.rs`.

**Architecture:** A new crate `wf-engine` (depends on `wf-core`), a synchronous scheduler (there are no parallel branches in Phase 2, so async is not needed - the web monitor from Phase 2b will wrap calls in a thread). Run state is a pure fold over the append-only `events.jsonl`. Agents sit behind the `AgentAdapter` trait (the first and only one is `ClaudeAdapter`, headless). All run state lives under `.wf/runs/<run-id>/`.

**Tech Stack:** Rust (edition 2024), serde + serde_json + serde_yaml_ng, thiserror; process execution via std::process; tests use tempfile, with a stub agent via an environment variable.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (sections 7.1-7.2, 8.1-8.3, 8.5-8.7, 20 - the line about Phase 2).

## Global Constraints

- Binary name: `wf`. Project folder: `.wf/`. Run state: `.wf/runs/<run-id>/`.
- Rust edition 2024. TDD is required: a failing test first, then the implementation. Commit at the end of each task.
- Engine error messages are in English (product code); documentation and comments are in Russian.
- Control files are written atomically via `wf_core::fsutil::atomic_write`; `events.jsonl` is written by append, which is its normal mode.
- Do not pull an async runtime into `wf-engine`: the scheduler is synchronous. Parallel branches, human_review, wait, ACP, and the supervisor agent are out of scope for Phase 2 (phases 3A/5).
- Do not invoke the real `claude` in unit and integration tests: the adapter takes the command from `WF_AGENT_CMD` (a stub script), defaulting to `claude`.
- Pull current dependency versions via `cargo add`; workspace server versions are already pinned from Phase 1.

---

### Task 1: The wf-engine crate and error type

**Files:**
- Modify: `Cargo.toml` (add member)
- Create: `crates/wf-engine/Cargo.toml`, `crates/wf-engine/src/lib.rs`, `crates/wf-engine/src/error.rs`

**Interfaces:**
- Produces: the `wf-engine` crate, buildable with `cargo build`; `wf_engine::error::EngineError` (thiserror) with variants `NotFound(String)`, `Invalid(String)`, `WorkdirBusy(String)`, `Adapter(String)`, `Script(String)`, `Anomaly(String)`, `Io(#[from] std::io::Error)`, `Schema(#[from] wf_core::schema::SchemaError)`, `Registry(#[from] wf_core::registry::RegistryError)`, `Yaml(String)`.

- [ ] **Step 1: Add the crate to the workspace**

In the root `Cargo.toml`, add `"crates/wf-engine"` to `members`:

```toml
members = ["crates/wf-core", "crates/wf-server", "crates/wf-cli", "crates/wf-engine"]
```

`crates/wf-engine/Cargo.toml`:

```toml
[package]
name = "wf-engine"
version.workspace = true
edition.workspace = true

[dependencies]
wf-core = { path = "../wf-core" }
serde.workspace = true
serde_json.workspace = true
serde_yaml_ng.workspace = true
thiserror.workspace = true

[dev-dependencies]
tempfile = "3.27.0"
```

- [ ] **Step 2: Error module and crate root**

`crates/wf-engine/src/error.rs`:

```rust
use wf_core::registry::RegistryError;
use wf_core::schema::SchemaError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("workdir busy: {0}")]
    WorkdirBusy(String),
    #[error("agent adapter error: {0}")]
    Adapter(String),
    #[error("script error: {0}")]
    Script(String),
    #[error("anomaly: {0}")]
    Anomaly(String),
    #[error("yaml error: {0}")]
    Yaml(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
}
```

`crates/wf-engine/src/lib.rs`:

```rust
pub mod error;
pub mod event;
pub mod state;
pub mod executor;
pub mod adapter;
pub mod script;
pub mod proc;
pub mod context;
pub mod run_config;
pub mod workdir;
pub mod scheduler;

pub use error::EngineError;
pub use scheduler::{list_runs, resume, run, RunOptions, RunResult, RunSummary};
```

For now, create empty stub files for the listed modules (each just the line `//! see plan`), except `error.rs`. This lets the crate compile after Task 1, and subsequent tasks will fill in the modules. Temporarily comment out the stub `pub use` from `scheduler` until Task 9, or it will not build; instead, for Task 1 leave only the `pub mod ...;` lines and `pub use error::EngineError;` in `lib.rs`.

Final `lib.rs` for Task 1:

```rust
pub mod error;
pub mod event;
pub mod state;
pub mod executor;
pub mod adapter;
pub mod script;
pub mod proc;
pub mod context;
pub mod run_config;
pub mod workdir;
pub mod scheduler;

pub use error::EngineError;
```

- [ ] **Step 3: Verify the build**

Run: `cargo build -p wf-engine`
Expected: build succeeds with no errors (modules are empty but declared).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore(engine): wf-engine crate scaffold and EngineError"
```

---

### Task 2: Event model and the append-only log

**Files:**
- Create: `crates/wf-engine/src/event.rs`, `crates/wf-engine/tests/event_test.rs`

**Interfaces:**
- Produces:
  - `event::EventPayload` (enum, `#[serde(tag = "type", rename_all = "snake_case")]`) with variants: `RunStarted { workflow: String, version: String }`, `NodeStarted { node: String, attempt: u32 }`, `AttemptStarted { node: String, attempt: u32, agent: String }`, `AttemptFinished { node: String, attempt: u32, status: String }`, `NodeFinished { node: String, status: String, attempt: u32, output: String }`, `RetryStarted { node: String, attempt: u32 }`, `FallbackTriggered { node: String, from: String, to: String }`, `RunPaused { reason: String }`, `RunFinished { outcome: String }`.
  - `event::Event { pub seq: u64, pub ts: u128, #[serde(flatten)] pub payload: EventPayload }`.
  - `event::now_millis() -> u128`.
  - `event::EventLog` with `create(run_dir: &Path) -> Result<EventLog, EngineError>` (fresh, seq starting at 0), `open(run_dir: &Path) -> Result<EventLog, EngineError>` (for resume, seq continues from the existing log), `append(&mut self, payload: EventPayload) -> Result<Event, EngineError>`, and a free function `read_all(run_dir: &Path) -> Result<Vec<Event>, EngineError>`.

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/event_test.rs`:

```rust
use wf_engine::event::{read_all, EventLog, EventPayload};

#[test]
fn appends_and_reads_events_with_increasing_seq() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::create(dir.path()).unwrap();
    log.append(EventPayload::RunStarted { workflow: "ping".into(), version: "1.0.0".into() }).unwrap();
    log.append(EventPayload::NodeFinished {
        node: "start".into(), status: "succeeded".into(), attempt: 1, output: String::new(),
    }).unwrap();

    let events = read_all(dir.path()).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].seq, 0);
    assert_eq!(events[1].seq, 1);
    assert!(matches!(events[0].payload, EventPayload::RunStarted { .. }));
    // serialization tags the type in snake_case
    let json = serde_json::to_value(&events[1]).unwrap();
    assert_eq!(json["type"], "node_finished");
    assert_eq!(json["node"], "start");
}

#[test]
fn open_continues_seq_for_resume() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::create(dir.path()).unwrap();
        log.append(EventPayload::RunStarted { workflow: "w".into(), version: "1.0.0".into() }).unwrap();
    }
    let mut log = EventLog::open(dir.path()).unwrap();
    let ev = log.append(EventPayload::RunFinished { outcome: "succeeded".into() }).unwrap();
    assert_eq!(ev.seq, 1);
    assert_eq!(read_all(dir.path()).unwrap().len(), 2);
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test event_test`
Expected: FAIL (compile error: module/types do not exist).

- [ ] **Step 3: Implement**

`crates/wf-engine/src/event.rs`:

```rust
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    RunStarted { workflow: String, version: String },
    NodeStarted { node: String, attempt: u32 },
    AttemptStarted { node: String, attempt: u32, agent: String },
    AttemptFinished { node: String, attempt: u32, status: String },
    NodeFinished { node: String, status: String, attempt: u32, output: String },
    RetryStarted { node: String, attempt: u32 },
    FallbackTriggered { node: String, from: String, to: String },
    RunPaused { reason: String },
    RunFinished { outcome: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts: u128,
    #[serde(flatten)]
    pub payload: EventPayload,
}

pub fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

pub struct EventLog {
    file: File,
    next_seq: u64,
}

impl EventLog {
    pub fn create(run_dir: &Path) -> Result<Self, EngineError> {
        std::fs::create_dir_all(run_dir)?;
        Self::open(run_dir)
    }

    pub fn open(run_dir: &Path) -> Result<Self, EngineError> {
        let path = run_dir.join("events.jsonl");
        let next_seq = if path.is_file() {
            read_all(run_dir)?.last().map(|e| e.seq + 1).unwrap_or(0)
        } else {
            0
        };
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { file, next_seq })
    }

    pub fn append(&mut self, payload: EventPayload) -> Result<Event, EngineError> {
        let event = Event { seq: self.next_seq, ts: now_millis(), payload };
        let line = serde_json::to_string(&event).map_err(|e| EngineError::Yaml(e.to_string()))?;
        writeln!(self.file, "{line}")?;
        self.file.flush()?;
        self.next_seq += 1;
        Ok(event)
    }
}

pub fn read_all(run_dir: &Path) -> Result<Vec<Event>, EngineError> {
    let path = run_dir.join("events.jsonl");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for line in BufReader::new(File::open(&path)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let ev: Event = serde_json::from_str(&line).map_err(|e| EngineError::Yaml(e.to_string()))?;
        out.push(ev);
    }
    Ok(out)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test event_test`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): event model and append-only event log"
```

---

### Task 3: Fold of the run state

**Files:**
- Create: `crates/wf-engine/src/state.rs`, `crates/wf-engine/tests/state_test.rs`

**Interfaces:**
- Consumes: `event::{Event, EventPayload}`.
- Produces:
  - `state::NodeStatus` (enum, `#[serde(rename_all = "snake_case")]`): `Pending, Ready, Running, Succeeded, Failed, Unknown, TimedOut, Interrupted, Skipped, Cancelled`; methods `as_str(&self) -> &'static str` and `from_str(&str) -> NodeStatus` (unknown -> `Unknown`).
  - `state::RunStatus` (enum, snake_case): `Created, Running, Paused, Succeeded, Failed, Aborted, Interrupted`; `as_str`.
  - `state::RunState { pub run_status: RunStatus, pub nodes: BTreeMap<String, NodeStatus>, pub attempts: BTreeMap<String, u32>, pub outputs: BTreeMap<String, String>, pub last_node: Option<String> }`.
  - `RunState::fold(events: &[Event]) -> RunState`.

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/state_test.rs`:

```rust
use wf_engine::event::{Event, EventPayload};
use wf_engine::state::{NodeStatus, RunState, RunStatus};

fn ev(seq: u64, payload: EventPayload) -> Event {
    Event { seq, ts: 0, payload }
}

#[test]
fn folds_finished_run() {
    let events = vec![
        ev(0, EventPayload::RunStarted { workflow: "w".into(), version: "1.0.0".into() }),
        ev(1, EventPayload::NodeFinished { node: "start".into(), status: "succeeded".into(), attempt: 1, output: String::new() }),
        ev(2, EventPayload::NodeFinished { node: "ping".into(), status: "succeeded".into(), attempt: 1, output: "pong".into() }),
        ev(3, EventPayload::RunFinished { outcome: "succeeded".into() }),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.run_status, RunStatus::Succeeded);
    assert_eq!(s.nodes.get("ping"), Some(&NodeStatus::Succeeded));
    assert_eq!(s.outputs.get("ping").map(String::as_str), Some("pong"));
    assert_eq!(s.last_node.as_deref(), Some("ping"));
}

#[test]
fn open_attempt_marks_interrupted() {
    // attempt_started without attempt_finished => the node and the run are interrupted
    let events = vec![
        ev(0, EventPayload::RunStarted { workflow: "w".into(), version: "1.0.0".into() }),
        ev(1, EventPayload::AttemptStarted { node: "ping".into(), attempt: 1, agent: "claude-code".into() }),
    ];
    let s = RunState::fold(&events);
    assert_eq!(s.nodes.get("ping"), Some(&NodeStatus::Interrupted));
    assert_eq!(s.run_status, RunStatus::Interrupted);
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test state_test`
Expected: FAIL (types do not exist).

- [ ] **Step 3: Implement**

`crates/wf-engine/src/state.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Pending, Ready, Running, Succeeded, Failed, Unknown, TimedOut, Interrupted, Skipped, Cancelled,
}

impl NodeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeStatus::Pending => "pending",
            NodeStatus::Ready => "ready",
            NodeStatus::Running => "running",
            NodeStatus::Succeeded => "succeeded",
            NodeStatus::Failed => "failed",
            NodeStatus::Unknown => "unknown",
            NodeStatus::TimedOut => "timed_out",
            NodeStatus::Interrupted => "interrupted",
            NodeStatus::Skipped => "skipped",
            NodeStatus::Cancelled => "cancelled",
        }
    }
    pub fn from_str(s: &str) -> NodeStatus {
        match s {
            "pending" => NodeStatus::Pending,
            "ready" => NodeStatus::Ready,
            "running" => NodeStatus::Running,
            "succeeded" => NodeStatus::Succeeded,
            "failed" => NodeStatus::Failed,
            "timed_out" => NodeStatus::TimedOut,
            "interrupted" => NodeStatus::Interrupted,
            "skipped" => NodeStatus::Skipped,
            "cancelled" => NodeStatus::Cancelled,
            _ => NodeStatus::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Created, Running, Paused, Succeeded, Failed, Aborted, Interrupted,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Created => "created",
            RunStatus::Running => "running",
            RunStatus::Paused => "paused",
            RunStatus::Succeeded => "succeeded",
            RunStatus::Failed => "failed",
            RunStatus::Aborted => "aborted",
            RunStatus::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct RunState {
    pub run_status: RunStatus,
    pub nodes: BTreeMap<String, NodeStatus>,
    pub attempts: BTreeMap<String, u32>,
    pub outputs: BTreeMap<String, String>,
    pub last_node: Option<String>,
}

impl Default for RunStatus {
    fn default() -> Self { RunStatus::Created }
}

impl RunState {
    pub fn fold(events: &[Event]) -> RunState {
        let mut s = RunState::default();
        // Open attempts: node -> attempt. Closed by attempt_finished/node_finished.
        let mut open: BTreeSet<String> = BTreeSet::new();
        for e in events {
            match &e.payload {
                EventPayload::RunStarted { .. } => s.run_status = RunStatus::Running,
                EventPayload::NodeStarted { node, .. } => {
                    s.nodes.insert(node.clone(), NodeStatus::Running);
                }
                EventPayload::AttemptStarted { node, attempt, .. } => {
                    s.nodes.insert(node.clone(), NodeStatus::Running);
                    s.attempts.insert(node.clone(), *attempt);
                    open.insert(node.clone());
                }
                EventPayload::AttemptFinished { node, .. } => {
                    open.remove(node);
                }
                EventPayload::NodeFinished { node, status, output, .. } => {
                    open.remove(node);
                    s.nodes.insert(node.clone(), NodeStatus::from_str(status));
                    s.outputs.insert(node.clone(), output.clone());
                    s.last_node = Some(node.clone());
                }
                EventPayload::RunPaused { .. } => s.run_status = RunStatus::Paused,
                EventPayload::RunFinished { outcome } => {
                    s.run_status = match outcome.as_str() {
                        "succeeded" => RunStatus::Succeeded,
                        _ => RunStatus::Failed,
                    };
                }
                EventPayload::RetryStarted { .. } | EventPayload::FallbackTriggered { .. } => {}
            }
        }
        // Attempts left open after the last event: interrupted.
        if !open.is_empty() {
            for node in &open {
                s.nodes.insert(node.clone(), NodeStatus::Interrupted);
            }
            if !matches!(s.run_status, RunStatus::Succeeded | RunStatus::Failed) {
                s.run_status = RunStatus::Interrupted;
            }
        }
        s
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test state_test`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): run state as a fold of the event log"
```

---

### Task 4: Executor resolution

**Files:**
- Create: `crates/wf-engine/src/executor.rs`, `crates/wf-engine/tests/executor_test.rs`

**Interfaces:**
- Consumes: `wf_core::schema::{Workflow, ExecutorRef, NodeKind}`.
- Produces:
  - `executor::AgentModel { pub agent: String, pub model: String }` (derive Clone, Debug, PartialEq).
  - `executor::ResolvedExecutor { pub primary: AgentModel, pub fallbacks: Vec<AgentModel> }`; method `chain(&self) -> Vec<AgentModel>` (primary + fallbacks in order).
  - `executor::resolve(wf: &Workflow, node_executor: Option<&ExecutorRef>) -> Result<ResolvedExecutor, EngineError>` (the node's executor first, then `defaults.executor`; a `Name` is looked up in `wf.executors`).

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/executor_test.rs`:

```rust
use wf_core::schema::Workflow;
use wf_engine::executor::resolve;

const YAML: &str = r#"
schema: 1
id: t
name: T
version: 1.0.0
executors:
  main:
    agent: claude-code
    model: haiku
    fallbacks:
      - { agent: codex, model: gpt-5.2-codex }
defaults:
  executor: main
nodes:
  - { id: start, type: start }
  - { id: a, type: agent_task, prompt: hi }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

#[test]
fn resolves_default_executor_with_fallbacks() {
    let wf = Workflow::from_yaml(YAML).unwrap();
    let node = wf.node("a").unwrap();
    let node_exec = match &node.kind {
        wf_core::schema::NodeKind::AgentTask { executor, .. } => executor.as_ref(),
        _ => None,
    };
    let r = resolve(&wf, node_exec).unwrap();
    assert_eq!(r.primary.agent, "claude-code");
    assert_eq!(r.primary.model, "haiku");
    assert_eq!(r.fallbacks.len(), 1);
    assert_eq!(r.fallbacks[0].agent, "codex");
    let chain = r.chain();
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[1].model, "gpt-5.2-codex");
}

#[test]
fn missing_executor_is_error() {
    let bad = YAML.replace("executor: main", "executor: ghost");
    let wf = Workflow::from_yaml(&bad).unwrap();
    assert!(resolve(&wf, None).is_err());
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test executor_test`
Expected: FAIL (function does not exist).

- [ ] **Step 3: Implement**

`crates/wf-engine/src/executor.rs`:

```rust
use wf_core::schema::{Executor, ExecutorRef, Workflow};

use crate::error::EngineError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModel {
    pub agent: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedExecutor {
    pub primary: AgentModel,
    pub fallbacks: Vec<AgentModel>,
}

impl ResolvedExecutor {
    pub fn chain(&self) -> Vec<AgentModel> {
        let mut v = vec![self.primary.clone()];
        v.extend(self.fallbacks.iter().cloned());
        v
    }
}

fn from_executor(ex: &Executor) -> ResolvedExecutor {
    ResolvedExecutor {
        primary: AgentModel { agent: ex.agent.clone(), model: ex.model.clone() },
        fallbacks: ex.fallbacks.iter()
            .map(|f| AgentModel { agent: f.agent.clone(), model: f.model.clone() })
            .collect(),
    }
}

pub fn resolve(wf: &Workflow, node_executor: Option<&ExecutorRef>) -> Result<ResolvedExecutor, EngineError> {
    let chosen = node_executor
        .or(wf.defaults.executor.as_ref())
        .ok_or_else(|| EngineError::Invalid("no executor on node and no defaults.executor".into()))?;
    match chosen {
        ExecutorRef::Inline(ex) => Ok(from_executor(ex)),
        ExecutorRef::Name(name) => wf.executors.get(name)
            .map(from_executor)
            .ok_or_else(|| EngineError::Invalid(format!("executor `{name}` is not defined"))),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test executor_test`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): executor resolution (primary + fallback chain)"
```

---

### Task 5: The AgentAdapter trait and ClaudeAdapter

**Files:**
- Create: `crates/wf-engine/src/adapter.rs`, `crates/wf-engine/tests/adapter_test.rs`

**Interfaces:**
- Consumes: `state::NodeStatus`.
- Produces:
  - `adapter::ErrorClass` (enum): `Transport, ProcessExit, StructuredOutputMissing, AgentReportedFailure`.
  - `adapter::AgentTask<'a> { pub prompt: &'a str, pub model: &'a str, pub workdir: &'a Path }`.
  - `adapter::AgentReport { pub status: NodeStatus, pub summary: String, pub raw: String }`.
  - `adapter::AgentAdapter` (trait): `fn run(&self, task: &AgentTask) -> Result<AgentReport, (ErrorClass, String)>`.
  - `adapter::ClaudeAdapter { pub program: String }` with `from_env() -> ClaudeAdapter` (takes `WF_AGENT_CMD` or `"claude"`); implements `AgentAdapter`.
  - `adapter::adapter_for(agent: &str) -> Result<Box<dyn AgentAdapter>, EngineError>` (mapping `claude-code`/`claude` -> `ClaudeAdapter::from_env()`; otherwise an error).

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/adapter_test.rs`:

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;
use wf_engine::adapter::{adapter_for, AgentAdapter, AgentTask, ClaudeAdapter, ErrorClass};
use wf_engine::state::NodeStatus;

// Prepares a stub agent: a shell script with the given body.
fn stub_agent(dir: &std::path::Path, body: &str) -> String {
    let path = dir.join("stub-agent.sh");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perm = fs::metadata(&path).unwrap().permissions();
    perm.set_mode(0o755);
    fs::set_permissions(&path, perm).unwrap();
    path.to_string_lossy().to_string()
}

// Tests that spawn a process construct ClaudeAdapter directly with an explicit program,
// to avoid mutating the global WF_AGENT_CMD (a race under parallel tests).
#[test]
fn claude_adapter_success_via_stub() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter { program: stub_agent(dir.path(), "echo pong") };
    let report = ad.run(&AgentTask { prompt: "ping", model: "haiku", workdir: dir.path() }).unwrap();
    assert_eq!(report.status, NodeStatus::Succeeded);
    assert_eq!(report.summary, "pong");
}

#[test]
fn claude_adapter_nonzero_exit_is_process_exit() {
    let dir = tempfile::tempdir().unwrap();
    let ad = ClaudeAdapter { program: stub_agent(dir.path(), "echo boom 1>&2\nexit 3") };
    let err = ad.run(&AgentTask { prompt: "ping", model: "haiku", workdir: dir.path() }).unwrap_err();
    assert!(matches!(err.0, ErrorClass::ProcessExit));
}

#[test]
fn adapter_for_maps_known_and_rejects_unknown() {
    assert!(adapter_for("claude-code").is_ok());
    assert!(adapter_for("borg").is_err());
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test adapter_test`
Expected: FAIL.

- [ ] **Step 3: Implement**

`crates/wf-engine/src/adapter.rs`:

```rust
use std::path::Path;
use std::process::Command;

use crate::error::EngineError;
use crate::state::NodeStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    Transport,
    ProcessExit,
    StructuredOutputMissing,
    AgentReportedFailure,
}

pub struct AgentTask<'a> {
    pub prompt: &'a str,
    pub model: &'a str,
    pub workdir: &'a Path,
}

pub struct AgentReport {
    pub status: NodeStatus,
    pub summary: String,
    pub raw: String,
}

pub trait AgentAdapter {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, (ErrorClass, String)>;
}

pub struct ClaudeAdapter {
    pub program: String,
}

impl ClaudeAdapter {
    pub fn from_env() -> Self {
        let program = std::env::var("WF_AGENT_CMD").unwrap_or_else(|_| "claude".to_string());
        Self { program }
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn run(&self, task: &AgentTask) -> Result<AgentReport, (ErrorClass, String)> {
        let output = Command::new(&self.program)
            .arg("-p").arg(task.prompt)
            .arg("--model").arg(task.model)
            .current_dir(task.workdir)
            .output()
            .map_err(|e| (ErrorClass::ProcessExit, format!("spawn `{}` failed: {e}", self.program)))?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if !output.status.success() {
            return Err((ErrorClass::ProcessExit,
                format!("agent exited with {:?}: {stderr}", output.status.code())));
        }
        // Phase 2a: we do not parse a structured block, stdout is both summary and raw.
        Ok(AgentReport { status: NodeStatus::Succeeded, summary: stdout.clone(), raw: stdout })
    }
}

pub fn adapter_for(agent: &str) -> Result<Box<dyn AgentAdapter>, EngineError> {
    match agent {
        "claude-code" | "claude" => Ok(Box::new(ClaudeAdapter::from_env())),
        other => Err(EngineError::Adapter(format!("unsupported agent `{other}` (phase 2a: only claude-code)"))),
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test adapter_test`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): AgentAdapter trait and headless ClaudeAdapter"
```

---

### Task 6: Process execution with a timeout and the script node

**Files:**
- Create: `crates/wf-engine/src/proc.rs`, `crates/wf-engine/src/script.rs`, `crates/wf-engine/tests/script_test.rs`

**Interfaces:**
- Produces:
  - `proc::Captured { pub status: Option<std::process::ExitStatus>, pub stdout: String, pub stderr: String }` (status = `None` on timeout).
  - `proc::run_capture(cmd: std::process::Command, timeout: Option<std::time::Duration>) -> std::io::Result<Captured>`.
  - `script::ScriptResult { pub status: NodeStatus, pub stdout: String }`.
  - `script::run_script(version_dir: &Path, workdir: &Path, script_rel: &str, runner: &str, timeout: Option<Duration>) -> Result<ScriptResult, EngineError>` (runner `sh`/`bash` -> `sh`, `ts` -> `bun run`, `py` -> `python3`; the script path is relative to `version_dir`; status `Succeeded` on exit code 0, `Failed` on nonzero, `TimedOut` on timeout).

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/script_test.rs`:

```rust
use std::fs;
use std::time::Duration;
use wf_engine::script::run_script;
use wf_engine::state::NodeStatus;

fn write_script(dir: &std::path::Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, body).unwrap();
}

#[test]
fn sh_script_success_captures_stdout() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/ok.sh", "echo hello-script");
    let r = run_script(ver.path(), work.path(), "scripts/ok.sh", "sh", Some(Duration::from_secs(10))).unwrap();
    assert_eq!(r.status, NodeStatus::Succeeded);
    assert_eq!(r.stdout, "hello-script");
}

#[test]
fn sh_script_nonzero_is_failed() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/bad.sh", "echo oops; exit 2");
    let r = run_script(ver.path(), work.path(), "scripts/bad.sh", "sh", Some(Duration::from_secs(10))).unwrap();
    assert_eq!(r.status, NodeStatus::Failed);
}

#[test]
fn sh_script_timeout_is_timed_out() {
    let ver = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    write_script(ver.path(), "scripts/slow.sh", "sleep 5");
    let r = run_script(ver.path(), work.path(), "scripts/slow.sh", "sh", Some(Duration::from_millis(300))).unwrap();
    assert_eq!(r.status, NodeStatus::TimedOut);
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test script_test`
Expected: FAIL.

- [ ] **Step 3: Implement proc and script**

`crates/wf-engine/src/proc.rs`:

```rust
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

pub struct Captured {
    pub status: Option<ExitStatus>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs the command, captures stdout/stderr; if the timeout is exceeded, kills the process
/// and returns status = None. Without a timeout, waits for completion.
pub fn run_capture(mut cmd: Command, timeout: Option<Duration>) -> std::io::Result<Captured> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    // Read the streams on separate threads to avoid blocking on full pipes.
    let mut out_pipe = child.stdout.take().expect("piped stdout");
    let mut err_pipe = child.stderr.take().expect("piped stderr");
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    thread::spawn(move || { let mut s = String::new(); let _ = out_pipe.read_to_string(&mut s); let _ = tx_out.send(s); });
    thread::spawn(move || { let mut s = String::new(); let _ = err_pipe.read_to_string(&mut s); let _ = tx_err.send(s); });

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if let Some(limit) = timeout {
            if start.elapsed() >= limit {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
        thread::sleep(Duration::from_millis(50));
    };

    let stdout = rx_out.recv().unwrap_or_default().trim().to_string();
    let stderr = rx_err.recv().unwrap_or_default().trim().to_string();
    Ok(Captured { status, stdout, stderr })
}
```

`crates/wf-engine/src/script.rs`:

```rust
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::error::EngineError;
use crate::proc::run_capture;
use crate::state::NodeStatus;

pub struct ScriptResult {
    pub status: NodeStatus,
    pub stdout: String,
}

pub fn run_script(
    version_dir: &Path,
    workdir: &Path,
    script_rel: &str,
    runner: &str,
    timeout: Option<Duration>,
) -> Result<ScriptResult, EngineError> {
    let script_path = version_dir.join(script_rel);
    if !script_path.is_file() {
        return Err(EngineError::Script(format!("script not found: {script_rel}")));
    }
    let mut cmd = match runner {
        "sh" | "bash" => { let mut c = Command::new("sh"); c.arg(&script_path); c }
        "ts" => { let mut c = Command::new("bun"); c.arg("run").arg(&script_path); c }
        "py" => { let mut c = Command::new("python3"); c.arg(&script_path); c }
        other => return Err(EngineError::Script(format!("unsupported runner `{other}`"))),
    };
    cmd.current_dir(workdir);
    let captured = run_capture(cmd, timeout)?;
    let status = match captured.status {
        None => NodeStatus::TimedOut,
        Some(s) if s.success() => NodeStatus::Succeeded,
        Some(_) => NodeStatus::Failed,
    };
    Ok(ScriptResult { status, stdout: captured.stdout })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test script_test`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): process capture with timeout and sh script node"
```

---

### Task 7: Run context and template substitution

**Files:**
- Create: `crates/wf-engine/src/context.rs`, `crates/wf-engine/tests/context_test.rs`

**Interfaces:**
- Consumes: `event::{Event, EventPayload}`.
- Produces:
  - `context::build_context(events: &[Event]) -> String` (one section per `NodeFinished` in `seq` order: heading `## <node> (<status>, attempt <n>)` + output).
  - `context::render(text: &str, params: &BTreeMap<String, String>, instruction: Option<&str>, outputs: &BTreeMap<String, String>, context: &str) -> String` (substitutes `{{params.<name>}}`, `{{run.instruction}}`, `{{run.context}}`, `{{nodes.<id>.output}}`, `{{nodes.<id>.report}}`; unknown references are replaced with an empty string).

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/context_test.rs`:

```rust
use std::collections::BTreeMap;
use wf_engine::context::{build_context, render};
use wf_engine::event::{Event, EventPayload};

fn ev(seq: u64, p: EventPayload) -> Event { Event { seq, ts: 0, payload: p } }

#[test]
fn builds_context_sections_in_seq_order() {
    let events = vec![
        ev(0, EventPayload::NodeFinished { node: "lint".into(), status: "failed".into(), attempt: 1, output: "2 errors".into() }),
        ev(1, EventPayload::NodeFinished { node: "fix".into(), status: "succeeded".into(), attempt: 1, output: "patched".into() }),
    ];
    let ctx = build_context(&events);
    let lint_at = ctx.find("lint").unwrap();
    let fix_at = ctx.find("fix").unwrap();
    assert!(lint_at < fix_at, "sections must follow seq order");
    assert!(ctx.contains("2 errors"));
    assert!(ctx.contains("failed"));
}

#[test]
fn renders_all_template_refs() {
    let mut params = BTreeMap::new();
    params.insert("task".to_string(), "ship it".to_string());
    let mut outputs = BTreeMap::new();
    outputs.insert("lint".to_string(), "2 errors".to_string());
    let text = "T: {{params.task}} | I: {{run.instruction}} | O: {{nodes.lint.output}} | R: {{nodes.lint.report}} | ctx: {{run.context}}";
    let out = render(text, &params, Some("be careful"), &outputs, "CTXBODY");
    assert_eq!(out, "T: ship it | I: be careful | O: 2 errors | R: 2 errors | ctx: CTXBODY");
}

#[test]
fn unknown_refs_become_empty() {
    let out = render("[{{params.ghost}}]", &BTreeMap::new(), None, &std::collections::BTreeMap::new(), "");
    assert_eq!(out, "[]");
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test context_test`
Expected: FAIL.

- [ ] **Step 3: Implement**

`crates/wf-engine/src/context.rs`:

```rust
use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::event::{Event, EventPayload};

pub fn build_context(events: &[Event]) -> String {
    let mut out = String::new();
    for e in events {
        if let EventPayload::NodeFinished { node, status, attempt, output } = &e.payload {
            let _ = write!(out, "## {node} ({status}, attempt {attempt})\n\n{output}\n\n");
        }
    }
    out
}

/// Manual scan of `{{ ... }}` without regex; substitutes known references, unknown ones become "".
pub fn render(
    text: &str,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
    outputs: &BTreeMap<String, String>,
    context: &str,
) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        if i + 1 < bytes.len() && &bytes[i..i + 2] == b"{{" {
            if let Some(end) = text[i + 2..].find("}}") {
                let key = text[i + 2..i + 2 + end].trim();
                out.push_str(&resolve(key, params, instruction, outputs, context));
                i += 2 + end + 2;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn resolve(
    key: &str,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
    outputs: &BTreeMap<String, String>,
    context: &str,
) -> String {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["params", name] => params.get(*name).cloned().unwrap_or_default(),
        ["run", "instruction"] => instruction.unwrap_or("").to_string(),
        ["run", "context"] => context.to_string(),
        ["nodes", id, "output"] | ["nodes", id, "report"] => outputs.get(*id).cloned().unwrap_or_default(),
        _ => String::new(),
    }
}
```

Note: `render` works with ASCII boundaries via a byte-by-byte pass, but non-English text between templates is not preserved correctly, because non-`{{` bytes are copied as-is via `push(bytes[i] as char)` - no, this breaks UTF-8. Replace the loop body with a char-by-char pass over slices: see the correct version below.

Correct version (replaces the `render` function entirely):

```rust
pub fn render(
    text: &str,
    params: &BTreeMap<String, String>,
    instruction: Option<&str>,
    outputs: &BTreeMap<String, String>,
    context: &str,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        if let Some(close) = after.find("}}") {
            let key = after[..close].trim();
            out.push_str(&resolve(key, params, instruction, outputs, context));
            rest = &after[close + 2..];
        } else {
            out.push_str(&rest[open..]);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}
```

Use exactly this second version of `render`; do not write the first (byte-based) version into the file - it is shown only to illustrate the UTF-8 problem.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test context_test`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): context.md builder and template rendering"
```

---

### Task 8: run.yaml, version snapshot, workdir lock

**Files:**
- Create: `crates/wf-engine/src/run_config.rs`, `crates/wf-engine/src/workdir.rs`, `crates/wf-engine/tests/workdir_test.rs`

**Interfaces:**
- Consumes: `wf_core::fsutil::atomic_write`.
- Produces:
  - `run_config::RunConfig { pub params: BTreeMap<String, String>, pub instruction: Option<String> }` (derive Serialize/Deserialize/Default).
  - `run_config::write_run_config(run_dir: &Path, cfg: &RunConfig) -> Result<(), EngineError>`, `run_config::read_run_config(run_dir: &Path) -> Result<RunConfig, EngineError>`.
  - `run_config::snapshot_workflow(run_dir: &Path, yaml: &str) -> Result<(), EngineError>` (writes `runs/<id>/workflow.yaml`), `run_config::snapshot_dir(run_dir: &Path) -> PathBuf` (the snapshot folder is `run_dir` itself; scripts are looked up in `run_dir` relative to the version record - see below).
  - `workdir::WorkdirGuard` (RAII, releases the lock in `Drop`); `workdir::acquire(root: &Path, allow_shared: bool) -> Result<Option<WorkdirGuard>, EngineError>` (a `.wf/workdir.lock` lock with a pid; if it exists and the process is alive - `WorkdirBusy`; if `allow_shared` - return `None` without a lock; a stale lock is overwritten).

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/workdir_test.rs`:

```rust
use std::collections::BTreeMap;
use wf_engine::error::EngineError;
use wf_engine::run_config::{read_run_config, write_run_config, RunConfig};
use wf_engine::workdir::acquire;

#[test]
fn run_config_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut params = BTreeMap::new();
    params.insert("task".to_string(), "do it".to_string());
    let cfg = RunConfig { params, instruction: Some("careful".into()) };
    write_run_config(dir.path(), &cfg).unwrap();
    let back = read_run_config(dir.path()).unwrap();
    assert_eq!(back.params.get("task").map(String::as_str), Some("do it"));
    assert_eq!(back.instruction.as_deref(), Some("careful"));
}

#[test]
fn second_writer_is_refused_but_shared_allowed() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".wf")).unwrap();
    let guard = acquire(root.path(), false).unwrap();
    assert!(guard.is_some());
    // a second acquire without allow_shared - refused
    match acquire(root.path(), false) {
        Err(EngineError::WorkdirBusy(_)) => {}
        other => panic!("expected WorkdirBusy, got {other:?}"),
    }
    // with allow_shared - allowed (without a guard)
    assert!(acquire(root.path(), true).unwrap().is_none());
    // after releasing the first lock, acquiring again is possible
    drop(guard);
    assert!(acquire(root.path(), false).unwrap().is_some());
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test workdir_test`
Expected: FAIL.

- [ ] **Step 3: Implement**

`crates/wf-engine/src/run_config.rs`:

```rust
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wf_core::fsutil::atomic_write;

use crate::error::EngineError;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    #[serde(default)]
    pub instruction: Option<String>,
}

pub fn write_run_config(run_dir: &Path, cfg: &RunConfig) -> Result<(), EngineError> {
    let yaml = serde_yaml_ng::to_string(cfg).map_err(|e| EngineError::Yaml(e.to_string()))?;
    atomic_write(&run_dir.join("run.yaml"), yaml.as_bytes())?;
    Ok(())
}

pub fn read_run_config(run_dir: &Path) -> Result<RunConfig, EngineError> {
    let path = run_dir.join("run.yaml");
    if !path.is_file() {
        return Ok(RunConfig::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    serde_yaml_ng::from_str(&raw).map_err(|e| EngineError::Yaml(e.to_string()))
}

pub fn snapshot_workflow(run_dir: &Path, yaml: &str) -> Result<(), EngineError> {
    atomic_write(&run_dir.join("workflow.yaml"), yaml.as_bytes())?;
    Ok(())
}

/// The version snapshot folder inside the run (scripts copied at start also live here).
pub fn snapshot_dir(run_dir: &Path) -> PathBuf {
    run_dir.to_path_buf()
}
```

`crates/wf-engine/src/workdir.rs`:

```rust
use std::path::{Path, PathBuf};
use std::process::Command;

use wf_core::fsutil::atomic_write;

use crate::error::EngineError;

pub struct WorkdirGuard {
    lock_path: PathBuf,
}

impl Drop for WorkdirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

fn pid_alive(pid: u32) -> bool {
    // unix: `kill -0 <pid>` succeeds if the process exists.
    Command::new("kill").arg("-0").arg(pid.to_string())
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

pub fn acquire(root: &Path, allow_shared: bool) -> Result<Option<WorkdirGuard>, EngineError> {
    if allow_shared {
        return Ok(None);
    }
    let lock_path = root.join(".wf/workdir.lock");
    if lock_path.is_file() {
        let raw = std::fs::read_to_string(&lock_path).unwrap_or_default();
        let pid: u32 = raw.trim().parse().unwrap_or(0);
        if pid != 0 && pid_alive(pid) {
            return Err(EngineError::WorkdirBusy(format!(
                "another write-run holds the workdir (pid {pid}); use worktree or --allow-shared-workdir"
            )));
        }
        // stale lock - overwrite it
    }
    atomic_write(&lock_path, std::process::id().to_string().as_bytes())?;
    Ok(Some(WorkdirGuard { lock_path }))
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test workdir_test`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): run.yaml, version snapshot, workdir single-writer lock"
```

---

### Task 9: Scheduler - the main execution loop

**Files:**
- Create: `crates/wf-engine/src/scheduler.rs`, `crates/wf-engine/tests/scheduler_test.rs`
- Modify: `crates/wf-engine/src/lib.rs` (uncomment `pub use scheduler::...`)

**Interfaces:**
- Consumes: everything from tasks 2-8, `wf_core::registry::Registry`, `wf_core::validate::{validate, ValidationContext}`, `wf_core::schema::{NodeKind, Outcome, StatusEq, EdgeCondition, Edge, Workflow}`.
- Produces:
  - `scheduler::RunOptions { pub instruction: Option<String>, pub params: BTreeMap<String, String>, pub allow_shared_workdir: bool }` (derive Default).
  - `scheduler::RunResult { pub run_id: String, pub outcome: RunStatus }`.
  - `scheduler::run(root: &Path, id: &str, version: Option<&str>, opts: RunOptions) -> Result<RunResult, EngineError>`.
  - Internal `next_node(wf, from, statuses) -> Result<Option<String>, EngineError>` (logic carried over from `wf-cli/src/run.rs`, statuses are now `NodeStatus`; mapping `StatusEq::Success -> Succeeded`, `Failure -> Failed|TimedOut`).

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/scheduler_test.rs`:

```rust
use std::fs;
use wf_core::registry::init_project;
use wf_engine::event::{read_all, EventPayload};
use wf_engine::scheduler::{run, RunOptions};
use wf_engine::state::RunStatus;

// A workflow without agent_task: start -> prompt -> finish. No real agent needed.
const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hello {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".wf/workflows/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), NOAGENT).unwrap();
    fs::write(root.join(".wf/workflows/noagent/current"), "1.0.0").unwrap();
}

#[test]
fn runs_linear_no_agent_workflow_to_success() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let mut opts = RunOptions::default();
    opts.params.insert("who".into(), "world".into());
    let res = run(dir.path(), "noagent", None, opts).unwrap();
    assert_eq!(res.outcome, RunStatus::Succeeded);

    // events recorded, version snapshot in place
    let run_dir = dir.path().join(".wf/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::RunStarted { .. })));
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::RunFinished { outcome } if outcome == "succeeded")));
    assert!(run_dir.join("workflow.yaml").is_file());
    assert!(run_dir.join("context.md").is_file());
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test scheduler_test`
Expected: FAIL.

- [ ] **Step 3: Implement the main loop (without retry/fallback - those are in Task 10)**

In `crates/wf-engine/src/lib.rs`, uncomment the line `pub use scheduler::{list_runs, resume, run, RunOptions, RunResult, RunSummary};` (the types `list_runs`, `resume`, `RunSummary` will appear in Task 11; for Task 9 to build, temporarily export only what exists: `pub use scheduler::{run, RunOptions, RunResult};`, and extend it in Task 11).

`crates/wf-engine/src/scheduler.rs`:

```rust
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use wf_core::registry::Registry;
use wf_core::schema::{Edge, EdgeCondition, NodeKind, Outcome, StatusEq, Workflow};
use wf_core::validate::{validate, Severity, ValidationContext};

use crate::adapter::{adapter_for, AgentTask};
use crate::context::{build_context, render};
use crate::error::EngineError;
use crate::event::{now_millis, read_all, EventLog, EventPayload};
use crate::executor::resolve;
use crate::run_config::{snapshot_workflow, write_run_config, RunConfig};
use crate::script::run_script;
use crate::state::{NodeStatus, RunState, RunStatus};
use crate::workdir::acquire;

#[derive(Debug, Default)]
pub struct RunOptions {
    pub instruction: Option<String>,
    pub params: BTreeMap<String, String>,
    pub allow_shared_workdir: bool,
}

#[derive(Debug)]
pub struct RunResult {
    pub run_id: String,
    pub outcome: RunStatus,
}

fn status_matches(node_status: NodeStatus, equals: StatusEq) -> bool {
    match equals {
        StatusEq::Success => node_status == NodeStatus::Succeeded,
        StatusEq::Failure => matches!(node_status, NodeStatus::Failed | NodeStatus::TimedOut),
    }
}

fn next_node(
    wf: &Workflow,
    from: &str,
    statuses: &BTreeMap<String, NodeStatus>,
) -> Result<Option<String>, EngineError> {
    let out: Vec<&Edge> = wf.edges.iter().filter(|e| e.from == from).collect();
    if out.is_empty() {
        return Ok(None);
    }
    let matches = |e: &Edge| -> bool {
        match &e.condition {
            None => true,
            Some(EdgeCondition::NodeStatus { node, equals }) => {
                statuses.get(node).map(|s| status_matches(*s, *equals)).unwrap_or(false)
            }
            Some(_) => false,
        }
    };
    if let Some(e) = out.iter().find(|e| !e.fallback && matches(e)) {
        return Ok(Some(e.to.clone()));
    }
    if let Some(e) = out.iter().find(|e| e.fallback) {
        return Ok(Some(e.to.clone()));
    }
    Err(EngineError::Invalid(format!("no outgoing edge from `{from}` matched current statuses")))
}

/// A single execution of a node. Returns (NodeStatus, output). Retry/fallback are added in Task 10.
fn execute_node(
    wf: &Workflow,
    run_dir: &Path,
    workdir: &Path,
    node_id: &str,
    log: &mut EventLog,
    state: &RunState,
    cfg: &RunConfig,
) -> Result<(NodeStatus, String), EngineError> {
    let node = wf.node(node_id).ok_or_else(|| EngineError::NotFound(node_id.into()))?;
    let context = build_context(&read_all(run_dir)?);
    match &node.kind {
        NodeKind::Start => Ok((NodeStatus::Succeeded, String::new())),
        NodeKind::Prompt { prompt } => {
            let text = render(prompt, &cfg.params, cfg.instruction.as_deref(), &state.outputs, &context);
            Ok((NodeStatus::Succeeded, text))
        }
        NodeKind::Condition { .. } => Ok((NodeStatus::Succeeded, String::new())),
        NodeKind::AgentTask { prompt, executor, timeout_seconds, .. } => {
            let _ = timeout_seconds; // agent timeout - Task 10
            let resolved = resolve(wf, executor.as_ref())?;
            let am = &resolved.primary;
            let text = render(prompt, &cfg.params, cfg.instruction.as_deref(), &state.outputs, &context);
            let adapter = adapter_for(&am.agent)?;
            log.append(EventPayload::AttemptStarted { node: node_id.into(), attempt: 1, agent: am.agent.clone() })?;
            let result = adapter.run(&AgentTask { prompt: &text, model: &am.model, workdir });
            match result {
                Ok(report) => {
                    log.append(EventPayload::AttemptFinished { node: node_id.into(), attempt: 1, status: report.status.as_str().into() })?;
                    Ok((report.status, report.summary))
                }
                Err((_class, msg)) => {
                    log.append(EventPayload::AttemptFinished { node: node_id.into(), attempt: 1, status: "failed".into() })?;
                    log.append(EventPayload::RunPaused { reason: msg.clone() })?;
                    Ok((NodeStatus::Failed, msg))
                }
            }
        }
        NodeKind::Script { script, runner, timeout_seconds } => {
            let timeout = timeout_seconds.map(|s| Duration::from_secs(s));
            let r = run_script(run_dir, workdir, script, runner, timeout)?;
            Ok((r.status, r.stdout))
        }
        NodeKind::Finish { .. } => Ok((NodeStatus::Succeeded, String::new())),
        NodeKind::HumanReview { .. } | NodeKind::Wait { .. } => {
            Err(EngineError::Invalid(format!("node `{node_id}` kind is out of phase-2 scope")))
        }
    }
}

pub fn run(root: &Path, id: &str, version: Option<&str>, opts: RunOptions) -> Result<RunResult, EngineError> {
    let reg = Registry::open(root)?;
    let loaded = reg.load(id, version)?;
    let wf = loaded.workflow.clone();

    // Gate: do not run an invalid workflow.
    let ctx = ValidationContext { global_executors: vec![], profiles: reg.profiles() };
    let report = validate(&wf, &ctx);
    if report.issues.iter().any(|i| i.severity == Severity::Error) {
        return Err(EngineError::Invalid(format!("workflow `{id}` is invalid")));
    }

    let start = wf.nodes.iter().find(|n| matches!(n.kind, NodeKind::Start))
        .ok_or_else(|| EngineError::Invalid("no start node".into()))?;

    // Does the run write to the workdir (has agent_task/script)?
    let is_write = wf.nodes.iter().any(|n| matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }));
    let _guard = if is_write { acquire(root, opts.allow_shared_workdir)? } else { None };

    let run_id = format!("{id}-{}", now_millis());
    let run_dir = root.join(".wf/runs").join(&run_id);
    let mut log = EventLog::create(&run_dir)?;
    snapshot_workflow(&run_dir, &loaded.yaml)?;
    let cfg = RunConfig { params: opts.params.clone(), instruction: opts.instruction.clone() };
    write_run_config(&run_dir, &cfg)?;
    log.append(EventPayload::RunStarted { workflow: id.into(), version: loaded.version.clone() })?;

    let workdir = root.to_path_buf();
    let mut current = start.id.clone();
    let max_steps = 10_000usize;
    let mut outcome = Outcome::Failure;

    for _ in 0..max_steps {
        let state = RunState::fold(&read_all(&run_dir)?);
        let node = wf.node(&current).ok_or_else(|| EngineError::NotFound(current.clone()))?;

        if let NodeKind::Finish { outcome: o } = &node.kind {
            outcome = *o;
            let s = match o { Outcome::Success => "succeeded", Outcome::Failure => "failed" };
            log.append(EventPayload::NodeFinished { node: current.clone(), status: "succeeded".into(), attempt: 1, output: String::new() })?;
            log.append(EventPayload::RunFinished { outcome: s.into() })?;
            break;
        }

        log.append(EventPayload::NodeStarted { node: current.clone(), attempt: 1 })?;
        let (status, output) = execute_node(&wf, &run_dir, &workdir, &current, &mut log, &state, &cfg)?;
        log.append(EventPayload::NodeFinished { node: current.clone(), status: status.as_str().into(), attempt: 1, output })?;

        // Rebuild context.md as a materialized view.
        let ctx_md = build_context(&read_all(&run_dir)?);
        wf_core::fsutil::atomic_write(&run_dir.join("context.md"), ctx_md.as_bytes())?;

        // unknown/interrupted stop progression (in Phase 2, without a supervisor agent - pause the run).
        if matches!(status, NodeStatus::Unknown | NodeStatus::Interrupted) {
            log.append(EventPayload::RunPaused { reason: format!("node `{current}` ended with status {}", status.as_str()) })?;
            return Ok(RunResult { run_id, outcome: RunStatus::Paused });
        }

        let statuses = RunState::fold(&read_all(&run_dir)?).nodes;
        match next_node(&wf, &current, &statuses)? {
            Some(next) => current = next,
            None => return Err(EngineError::Invalid(format!("node `{current}` has no outgoing edge and is not finish"))),
        }
    }

    let outcome = match outcome { Outcome::Success => RunStatus::Succeeded, Outcome::Failure => RunStatus::Failed };
    Ok(RunResult { run_id, outcome })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test scheduler_test`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): scheduler core loop with event log and context materialization"
```

---

### Task 10: Retry, fallbacks, and the agent_task timeout

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs` (replace `execute_node` for agent_task)
- Create: `crates/wf-engine/tests/retry_test.rs`

**Interfaces:**
- Consumes: `adapter::{adapter_for, AgentTask, ErrorClass}`, `executor::resolve`.
- Produces: an updated `execute_node`, where agent_task walks the executor chain with retry per `max_retries` (default from `defaults.max_retries`, otherwise 0), falling over to a fallback agent once retries are exhausted; `RetryStarted` and `FallbackTriggered` events are written. An agent timeout via `proc::run_capture` is not introduced in Phase 2a (the agent is invoked via `.output()`); the retry test uses a stub that fails N times.

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/retry_test.rs`:

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;
use wf_core::registry::init_project;
use wf_engine::event::{read_all, EventPayload};
use wf_engine::scheduler::{run, RunOptions};
use wf_engine::state::RunStatus;

// Stub agent: fails until the marker file is created; creates it on the first call.
// So: 1st call - fail, 2nd - success. Verify that retry pushes it through.
fn flaky_agent(dir: &std::path::Path) -> String {
    let marker = dir.path_marker();
    let path = dir.join("flaky.sh");
    let body = format!(
        "#!/bin/sh\nif [ -f '{m}' ]; then echo ok; exit 0; else touch '{m}'; echo firstfail 1>&2; exit 1; fi\n",
        m = marker.display()
    );
    fs::write(&path, body).unwrap();
    let mut p = fs::metadata(&path).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&path, p).unwrap();
    path.to_string_lossy().to_string()
}

trait Marker { fn path_marker(&self) -> std::path::PathBuf; }
impl Marker for std::path::Path { fn path_marker(&self) -> std::path::PathBuf { self.join("flaky.marker") } }

const WF: &str = r#"
schema: 1
id: retryflow
name: Retry
version: 1.0.0
executors:
  main: { agent: claude-code, model: haiku }
defaults:
  executor: main
  max_retries: 1
nodes:
  - { id: start, type: start }
  - { id: work, type: agent_task, prompt: "do" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: work }
  - { from: work, to: done }
"#;

#[test]
fn retry_recovers_flaky_agent() {
    let dir = tempfile::tempdir().unwrap();
    init_project(dir.path()).unwrap();
    let vdir = dir.path().join(".wf/workflows/retryflow/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), WF).unwrap();
    fs::write(dir.path().join(".wf/workflows/retryflow/current"), "1.0.0").unwrap();

    let prog = flaky_agent(dir.path());
    unsafe { std::env::set_var("WF_AGENT_CMD", &prog); }
    let res = run(dir.path(), "retryflow", None, RunOptions::default()).unwrap();
    unsafe { std::env::remove_var("WF_AGENT_CMD"); }

    assert_eq!(res.outcome, RunStatus::Succeeded);
    let run_dir = dir.path().join(".wf/runs").join(&res.run_id);
    let events = read_all(&run_dir).unwrap();
    assert!(events.iter().any(|e| matches!(&e.payload, EventPayload::RetryStarted { .. })),
        "expected a retry_started event");
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test retry_test`
Expected: FAIL (the current execute_node does not retry - the agent fails once, the node is Failed, next_node finds no edge for failure -> error/not Succeeded).

- [ ] **Step 3: Implement retry/fallback in execute_node**

In `crates/wf-engine/src/scheduler.rs`, replace the `NodeKind::AgentTask { .. }` branch in `execute_node` with a loop over the executor chain and retry. Add the import `use crate::adapter::ErrorClass;` (if not already imported - import `adapter_for, AgentTask, ErrorClass`).

The block to replace (the entire AgentTask branch):

```rust
        NodeKind::AgentTask { prompt, executor, max_retries, .. } => {
            let resolved = resolve(wf, executor.as_ref())?;
            let text = render(prompt, &cfg.params, cfg.instruction.as_deref(), &state.outputs, &context);
            let retries = max_retries
                .or(wf.defaults.max_retries)
                .unwrap_or(0);

            let chain = resolved.chain();
            let mut attempt: u32 = 0;
            let mut last_msg = String::new();
            for (idx, am) in chain.iter().enumerate() {
                if idx > 0 {
                    let from = &chain[idx - 1].agent;
                    log.append(EventPayload::FallbackTriggered {
                        node: node_id.into(), from: from.clone(), to: am.agent.clone(),
                    })?;
                }
                let adapter = adapter_for(&am.agent)?;
                // attempts: 1 primary + `retries` retries per executor
                for try_i in 0..=retries {
                    attempt += 1;
                    if try_i > 0 {
                        log.append(EventPayload::RetryStarted { node: node_id.into(), attempt })?;
                    }
                    log.append(EventPayload::AttemptStarted { node: node_id.into(), attempt, agent: am.agent.clone() })?;
                    match adapter.run(&AgentTask { prompt: &text, model: &am.model, workdir }) {
                        Ok(report) => {
                            log.append(EventPayload::AttemptFinished { node: node_id.into(), attempt, status: report.status.as_str().into() })?;
                            if report.status == NodeStatus::Succeeded {
                                return Ok((NodeStatus::Succeeded, report.summary));
                            }
                            last_msg = report.summary;
                        }
                        Err((class, msg)) => {
                            log.append(EventPayload::AttemptFinished { node: node_id.into(), attempt, status: "failed".into() })?;
                            last_msg = msg;
                            // transport error breaks retry on this executor and goes to fallback
                            if class == ErrorClass::Transport {
                                break;
                            }
                        }
                    }
                }
            }
            Ok((NodeStatus::Failed, last_msg))
        }
```

Note: `state` is already in the `execute_node` signature; this block replaces the `_class` stub from Task 9 and now uses `ErrorClass`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine --test retry_test`
Expected: 1 passed. Then `cargo test -p wf-engine` - all previous engine tests green.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): agent_task retry and fallback across executor chain"
```

---

### Task 11: Resume and run listing

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs`, `crates/wf-engine/src/lib.rs`
- Create: `crates/wf-engine/tests/resume_test.rs`

**Interfaces:**
- Produces:
  - `scheduler::RunSummary { pub run_id: String, pub workflow: String, pub status: String, pub started_ts: u128 }` (derive Serialize).
  - `scheduler::list_runs(root: &Path) -> Result<Vec<RunSummary>, EngineError>` (scans folders `.wf/runs/*`, status from the fold, sorted by `started_ts` descending).
  - `scheduler::resume(root: &Path, run_id: &str, from_node: Option<&str>) -> Result<RunResult, EngineError>` (loads snapshot `runs/<id>/workflow.yaml`, restores state by replay, continues from `from_node` or `last_node`).
  - In `lib.rs` expand `pub use scheduler::{list_runs, resume, run, RunOptions, RunResult, RunSummary};`.

- [ ] **Step 1: Failing test**

`crates/wf-engine/tests/resume_test.rs`:

```rust
use std::fs;
use wf_core::registry::init_project;
use wf_engine::scheduler::{list_runs, resume, run, RunOptions};
use wf_engine::state::RunStatus;

const WF: &str = r#"
schema: 1
id: lin
name: Lin
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: a, type: prompt, prompt: "x" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: a }
  - { from: a, to: done }
"#;

fn seed(root: &std::path::Path) {
    init_project(root).unwrap();
    let vdir = root.join(".wf/workflows/lin/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), WF).unwrap();
    fs::write(root.join(".wf/workflows/lin/current"), "1.0.0").unwrap();
}

#[test]
fn lists_runs_after_a_run() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    let runs = list_runs(dir.path()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run_id, res.run_id);
    assert_eq!(runs[0].workflow, "lin");
    assert_eq!(runs[0].status, "succeeded");
}

#[test]
fn resume_from_node_reaches_finish() {
    let dir = tempfile::tempdir().unwrap();
    seed(dir.path());
    let res = run(dir.path(), "lin", None, RunOptions::default()).unwrap();
    // second pass from node `a` completes successfully (version snapshot inside the run)
    let again = resume(dir.path(), &res.run_id, Some("a")).unwrap();
    assert_eq!(again.run_id, res.run_id);
    assert_eq!(again.outcome, RunStatus::Succeeded);
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-engine --test resume_test`
Expected: FAIL.

- [ ] **Step 3: Implement**

Add to the end of `crates/wf-engine/src/scheduler.rs` (and extract the common execution loop from `run` into a private `drive` so `resume` can reuse it):

Add `use serde::Serialize;` to the scheduler module imports (at the top of the file, near other `use` statements). Then at the end of the file:

```rust
#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub workflow: String,
    pub status: String,
    pub started_ts: u128,
}

pub fn list_runs(root: &Path) -> Result<Vec<RunSummary>, EngineError> {
    let runs_dir = root.join(".wf/runs");
    let mut out = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&runs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().to_string();
        let events = read_all(&entry.path())?;
        if events.is_empty() {
            continue;
        }
        let state = RunState::fold(&events);
        let (workflow, started_ts) = events.iter().find_map(|e| match &e.payload {
            EventPayload::RunStarted { workflow, .. } => Some((workflow.clone(), e.ts)),
            _ => None,
        }).unwrap_or_else(|| (run_id.clone(), 0));
        out.push(RunSummary { run_id, workflow, status: state.run_status.as_str().into(), started_ts });
    }
    out.sort_by(|a, b| b.started_ts.cmp(&a.started_ts));
    Ok(out)
}

pub fn resume(root: &Path, run_id: &str, from_node: Option<&str>) -> Result<RunResult, EngineError> {
    let run_dir = root.join(".wf/runs").join(run_id);
    if !run_dir.is_dir() {
        return Err(EngineError::NotFound(format!("run `{run_id}`")));
    }
    let yaml = std::fs::read_to_string(run_dir.join("workflow.yaml"))?;
    let wf = Workflow::from_yaml(&yaml)?;
    let cfg = crate::run_config::read_run_config(&run_dir)?;
    let mut log = EventLog::open(&run_dir)?;

    let state = RunState::fold(&read_all(&run_dir)?);
    let start_node = match from_node {
        Some(n) => n.to_string(),
        None => state.last_node.clone().ok_or_else(|| EngineError::Invalid("nothing to resume from".into()))?,
    };
    log.append(EventPayload::RunPaused { reason: format!("resume from `{start_node}`") })?;
    let is_write = wf.nodes.iter().any(|n| matches!(n.kind, NodeKind::AgentTask { .. } | NodeKind::Script { .. }));
    let _guard = if is_write { acquire(root, false)? } else { None };
    drive(&wf, &run_dir, root, &mut log, &cfg, start_node, run_id.to_string())
}
```

Refactor `run`: extract the execution loop (starting from `let workdir = ...;` to the end) into a private function:

```rust
fn drive(
    wf: &Workflow,
    run_dir: &Path,
    root: &Path,
    log: &mut EventLog,
    cfg: &RunConfig,
    start_node: String,
    run_id: String,
) -> Result<RunResult, EngineError> {
    let workdir = root.to_path_buf();
    let mut current = start_node;
    let max_steps = 10_000usize;
    let mut outcome = RunStatus::Failed;
    for _ in 0..max_steps {
        let state = RunState::fold(&read_all(run_dir)?);
        let node = wf.node(&current).ok_or_else(|| EngineError::NotFound(current.clone()))?;
        if let NodeKind::Finish { outcome: o } = &node.kind {
            let s = match o { Outcome::Success => "succeeded", Outcome::Failure => "failed" };
            outcome = match o { Outcome::Success => RunStatus::Succeeded, Outcome::Failure => RunStatus::Failed };
            log.append(EventPayload::NodeFinished { node: current.clone(), status: "succeeded".into(), attempt: 1, output: String::new() })?;
            log.append(EventPayload::RunFinished { outcome: s.into() })?;
            break;
        }
        log.append(EventPayload::NodeStarted { node: current.clone(), attempt: 1 })?;
        let (status, output) = execute_node(wf, run_dir, &workdir, &current, log, &state, cfg)?;
        log.append(EventPayload::NodeFinished { node: current.clone(), status: status.as_str().into(), attempt: 1, output })?;
        let ctx_md = build_context(&read_all(run_dir)?);
        wf_core::fsutil::atomic_write(&run_dir.join("context.md"), ctx_md.as_bytes())?;
        if matches!(status, NodeStatus::Unknown | NodeStatus::Interrupted) {
            log.append(EventPayload::RunPaused { reason: format!("node `{current}` ended with status {}", status.as_str()) })?;
            return Ok(RunResult { run_id, outcome: RunStatus::Paused });
        }
        let statuses = RunState::fold(&read_all(run_dir)?).nodes;
        match next_node(wf, &current, &statuses)? {
            Some(next) => current = next,
            None => return Err(EngineError::Invalid(format!("node `{current}` has no outgoing edge and is not finish"))),
        }
    }
    Ok(RunResult { run_id, outcome })
}
```

And replace the tail of `run` (the entire block from `let workdir = root.to_path_buf();` inclusive to `Ok(RunResult { ... })`) with:

```rust
    drive(&wf, &run_dir, root, &mut log, &cfg, start.id.clone(), run_id)
```

In `crates/wf-engine/src/lib.rs` expand the export:

```rust
pub use scheduler::{list_runs, resume, run, RunOptions, RunResult, RunSummary};
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p wf-engine`
Expected: all engine tests green (event, state, executor, adapter, script, context, workdir, scheduler, retry, resume).

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(engine): resume from node and run listing"
```

---

### Task 12: Integrate with CLI and remove minimal runner

**Files:**
- Modify: `crates/wf-cli/Cargo.toml`, `crates/wf-cli/src/main.rs`
- Delete: `crates/wf-cli/src/run.rs`
- Create: `crates/wf-cli/tests/run_cli_test.rs`

**Interfaces:**
- Consumes: `wf_engine::{run, resume, list_runs, RunOptions, RunResult, RunSummary}`.
- Produces: commands `wf run <name> [--version V] [--instruction TEXT] [--param k=v]... [--allow-shared-workdir]`, `wf runs`, `wf resume <run-id> [--from-node ID]`. Exit codes: 0 success, 1 run finished with failure/pause, 2 environment error.

- [ ] **Step 1: Add dependency and failing test**

Run: `cargo add wf-engine --path crates/wf-engine -p wf-cli`
(adds `wf-engine = { path = "../wf-engine" }` to wf-cli dependencies).

`crates/wf-cli/tests/run_cli_test.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

const NOAGENT: &str = r#"
schema: 1
id: noagent
name: No Agent
version: 1.0.0
nodes:
  - { id: start, type: start }
  - { id: note, type: prompt, prompt: "hi {{params.who}}" }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: note }
  - { from: note, to: done }
"#;

fn wf() -> Command { Command::cargo_bin("wf").unwrap() }

fn seeded() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    wf().arg("init").current_dir(dir.path()).assert().success();
    let vdir = dir.path().join(".wf/workflows/noagent/1.0.0");
    fs::create_dir_all(&vdir).unwrap();
    fs::write(vdir.join("workflow.yaml"), NOAGENT).unwrap();
    fs::write(dir.path().join(".wf/workflows/noagent/current"), "1.0.0").unwrap();
    dir
}

#[test]
fn run_succeeds_and_writes_events() {
    let dir = seeded();
    wf().args(["run", "noagent", "--param", "who=world"])
        .current_dir(dir.path())
        .assert().success()
        .stdout(predicate::str::contains("succeeded"));
    // a run was created
    let runs_dir = dir.path().join(".wf/runs");
    let count = fs::read_dir(&runs_dir).unwrap().count();
    assert_eq!(count, 1);
}

#[test]
fn runs_command_lists_the_run() {
    let dir = seeded();
    wf().args(["run", "noagent"]).current_dir(dir.path()).assert().success();
    wf().arg("runs").current_dir(dir.path())
        .assert().success()
        .stdout(predicate::str::contains("noagent"))
        .stdout(predicate::str::contains("succeeded"));
}

#[test]
fn run_without_project_fails_env() {
    let dir = tempfile::tempdir().unwrap();
    wf().args(["run", "ghost"]).current_dir(dir.path()).assert().code(2);
}
```

- [ ] **Step 2: Confirm the test fails**

Run: `cargo test -p wf-cli --test run_cli_test`
Expected: FAIL (no run/runs commands via engine; old `run` uses minimal runner and does not understand `--param`).

- [ ] **Step 3: Rewrite CLI to use engine**

Delete the minimal runner file:

```bash
git rm crates/wf-cli/src/run.rs
```

In `crates/wf-cli/src/main.rs` remove `mod run;` and the `Outcome` import; replace the command declaration and handlers. Fully replace the enum `Command` variants `Run`/`Serve` and the `run_run` function:

The part of enum to replace (variant `Run` and add `Runs`, `Resume`):

```rust
    /// Run a workflow
    Run {
        name: String,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        instruction: Option<String>,
        /// key=value, repeatable
        #[arg(long = "param", value_name = "K=V")]
        params: Vec<String>,
        #[arg(long)]
        allow_shared_workdir: bool,
    },
    /// List runs
    Runs,
    /// Resume a paused/interrupted run
    Resume {
        run_id: String,
        #[arg(long)]
        from_node: Option<String>,
    },
```

In `match cli.command` replace the `Run` branch and add `Runs`/`Resume`:

```rust
        Some(Command::Run { name, version, instruction, params, allow_shared_workdir }) =>
            run_cmd(&root, &name, version.as_deref(), instruction, params, allow_shared_workdir),
        Some(Command::Runs) => runs_cmd(&root),
        Some(Command::Resume { run_id, from_node }) => resume_cmd(&root, &run_id, from_node.as_deref()),
```

Replace the `run_run` function with three functions:

```rust
use std::collections::BTreeMap;
use wf_engine::{list_runs, resume, run, RunOptions};
use wf_core::registry::Registry;
use wf_engine::state::RunStatus;

fn run_cmd(
    root: &PathBuf,
    name: &str,
    version: Option<&str>,
    instruction: Option<String>,
    params: Vec<String>,
    allow_shared_workdir: bool,
) -> ExitCode {
    if Registry::open(root).is_err() {
        eprintln!("no project here (run `wf init`)");
        return ExitCode::from(2);
    }
    let mut parsed = BTreeMap::new();
    for p in params {
        match p.split_once('=') {
            Some((k, v)) => { parsed.insert(k.to_string(), v.to_string()); }
            None => { eprintln!("bad --param `{p}` (expected key=value)"); return ExitCode::from(2); }
        }
    }
    let opts = RunOptions { instruction, params: parsed, allow_shared_workdir };
    match run(root, name, version, opts) {
        Ok(res) => {
            println!("run {} finished: {}", res.run_id, res.outcome.as_str());
            match res.outcome {
                RunStatus::Succeeded => ExitCode::SUCCESS,
                _ => ExitCode::from(1),
            }
        }
        Err(e) => { eprintln!("run failed: {e}"); ExitCode::from(2) }
    }
}

fn runs_cmd(root: &PathBuf) -> ExitCode {
    match list_runs(root) {
        Ok(runs) if runs.is_empty() => { println!("no runs yet"); ExitCode::SUCCESS }
        Ok(runs) => {
            for r in runs {
                println!("{}\t{}\t{}", r.run_id, r.workflow, r.status);
            }
            ExitCode::SUCCESS
        }
        Err(e) => { eprintln!("runs failed: {e}"); ExitCode::from(2) }
    }
}

fn resume_cmd(root: &PathBuf, run_id: &str, from_node: Option<&str>) -> ExitCode {
    match resume(root, run_id, from_node) {
        Ok(res) => {
            println!("resume {} finished: {}", res.run_id, res.outcome.as_str());
            match res.outcome {
                RunStatus::Succeeded => ExitCode::SUCCESS,
                _ => ExitCode::from(1),
            }
        }
        Err(e) => { eprintln!("resume failed: {e}"); ExitCode::from(2) }
    }
}
```

Note: `wf-cli` now depends on `wf_engine::state::RunStatus`; ensure that `wf-engine` re-exports the `state` module (it is `pub mod state;` in lib.rs - yes).

- [ ] **Step 4: Run tests and entire workspace**

Run: `cargo test -p wf-cli --test run_cli_test`
Expected: 3 passed.

Run: `cargo test --workspace`
Expected: all tests green (Phase 1 + engine + CLI).

- [ ] **Step 5: End-to-end check on real ping (manual, optional)**

```bash
cd /Users/techmeat/www/projects/omniteamhq/workflows
cargo build -p wf-cli
./target/debug/wf run ping
```
Expected: real call to claude (haiku), output `succeeded`, event in `.wf/runs/<id>/events.jsonl`, file `context.md` with node `ping` section.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(cli): wf run/runs/resume on wf-engine; drop minimal runner"
```

---

### Task 13: Code-ranker run and status update

**Files:**
- Modify: `docs/tasks.md`

**Interfaces:**
- Produces: updated phase checklist (Phase 2 - completed engine items marked).

- [ ] **Step 1: Run code-ranker gate**

Run:
```bash
cargo metadata --format-version 1 >/dev/null
code-ranker check .
```
Expected: `no violations`. If there are any - run `code-ranker report . --output.scorecard --focus <ID> --top 1`, read `code-ranker docs base <ID>`, fix, and retry.

- [ ] **Step 2: Update docs/tasks.md**

In the "Phase 2" section, mark items implemented by Phase 2a: minimal runner replaced with full engine; event sourcing with replay/resume; AgentAdapter (claude-code); retry; fallbacks; script/prompt nodes; template substitution on run; `wf run/runs/resume`; shared context; one-off instruction. Leave unmarked what moved to 2b/phases 5: parallel branches, human_review/wait, web monitor (2b), worktree isolation, ACP.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "docs: mark phase-2a engine tasks done in tasks.md"
```

---

## What is intentionally NOT in Phase 2a (for reviewers)

- Web run monitor (endpoints `/api/runs`, event stream over WS, run page) - this is Phase 2b, separate plan.
- Parallel branches with join, human_review, wait, ACP transport - phases 5.
- Supervisor agent (SA) - phases 3A/3B.
- Worktree isolation (`workspace: worktree`) - deferred; Phase 2a has only strict single-writer workdir lock.
- Context compression (`context_compact.md`), agent_task timeout, multiple versions in one run, workflow overrides (only version snapshot is implemented).
- ts/py runners will not be tested (implemented via mapping, test only on sh).
