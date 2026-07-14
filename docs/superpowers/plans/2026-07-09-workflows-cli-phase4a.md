# Workflows CLI, Phase 4a (background run + wake/control channels) - implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** give the `wf-engine` engine background (non-blocking) run execution, a wake-event channel (node_failed / node_timeout / anomaly), and a control-command channel (retry / continue_from / pause / abort), plus `run_cancel`. This is pure machinery inside the engine, testable directly, with no MCP and no supervisor agent involved. Phase 4b will build supervisor MCP tools on top of it, and 4c the background supervisor agent.

**Architecture:** We preserve the event-sourcing invariant: the sole writer of `events.jsonl` is the run stream (drive). Wake events are written by that same stream as new `EventPayload` variants in `events.jsonl` (visible in the timeline, folded into state). Control commands are written by the supervisor into a SEPARATE append-only file, `control.jsonl`, in the run folder (the sole writer of that file is the supervisor side), and the drive stream only reads it at node boundaries. A background run is a `std::thread` into which the `WorkdirGuard` is moved; the caller gets the `run_id` immediately. The run mode (`Autonomous | Supervised`) determines the behavior on node failure: in `Autonomous` - as today (fallback edge or error), in `Supervised` - drive emits a wake and waits for a control command. `Abort` in `control.jsonl` is checked in BOTH modes (this is what `run_cancel` amounts to).

**Tech Stack:** Rust (edition 2024), std::thread, serde/serde_json; wf-core, wf-engine. No new external dependencies.

**Spec:** `docs/superpowers/specs/2026-07-08-workflows-cli-design.md` (section 9: 9.1 lifecycle, 9.3 triggers, 9.4 tools - this phase implements the engine foundation the tools sit on; section 8 event sourcing). Builds on Phases 1-3.

## Global Constraints

- Error texts are in English; comments/docs are in Russian. No em dashes or exclamation marks in code/docs.
- The project version stays at `0.1.0`; it is not bumped per phase.
- TDD is mandatory: a failing test first, then the implementation. Commit at the end of each task.
- Event-sourcing invariant: `events.jsonl` is written ONLY by the drive stream. Supervisor commands go to `control.jsonl` (written by the supervisor side, drive only reads it). Do not violate this.
- Existing behavior is unchanged: an unsupervised run (`RunMode::Autonomous`) must behave exactly as it does today; all existing engine tests stay green.
- `run_id` in any new paths/commands goes through `wf_core::registry::is_safe_segment` (path-traversal protection, same as in `resume`/MCP tools).
- Run code-ranker before marking a task done; navigate the code via codegraph.
- No real `claude` in tests: test agent_task nodes via the `WF_AGENT_CMD` stub (see existing engine tests), or use a workflow without agent_task to verify the control logic. Set an always-failing stub via env to trigger node_failed deterministically.

---

### Task 1: Wake events and control commands in the event model

**Files:**
- Modify: `crates/wf-engine/src/event.rs`
- Modify: `crates/wf-engine/src/state.rs`
- Test: `crates/wf-engine/tests/wake_events_test.rs`

**Interfaces:**
- Produces:
  - New enum `WakeTrigger` (serde snake_case): `NodeFailed`, `NodeTimeout`, `Anomaly`. (`NodeSlow`/`RunStuck` - later, needs duration history.)
  - New `EventPayload` variants:
    - `WakeRaised { trigger: WakeTrigger, node: String, detail: String }` - the engine calling the supervisor agent.
    - `SupervisorAction { action: String, node: Option<String>, detail: String }` - an applied intervention (for the 9.5/9.6 log).
    - `RunAborted { reason: String }` - the terminal cancellation event.
  - `RunState::fold` accounts for the new events: `RunAborted` -> `run_status = RunStatus::Aborted`; `WakeRaised`/`SupervisorAction` do not change node statuses (they are log-only), but must not break the fold.

- [ ] **Step 1: Failing test** `crates/wf-engine/tests/wake_events_test.rs`

```rust
use wf_engine::event::{EventLog, EventPayload, WakeTrigger, read_all};
use wf_engine::state::{RunState, RunStatus};

#[test]
fn wake_and_abort_round_trip_and_fold() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::create(dir.path()).unwrap();
    log.append(EventPayload::RunStarted { workflow: "w".into(), version: "1.0.0".into() }).unwrap();
    log.append(EventPayload::WakeRaised {
        trigger: WakeTrigger::NodeFailed, node: "impl".into(), detail: "exit 1".into(),
    }).unwrap();
    log.append(EventPayload::SupervisorAction {
        action: "node_retry".into(), node: Some("impl".into()), detail: "retry with hint".into(),
    }).unwrap();
    log.append(EventPayload::RunAborted { reason: "user cancel".into() }).unwrap();

    let events = read_all(dir.path()).unwrap();
    assert_eq!(events.len(), 4);
    // check that the type tag serializes to snake_case
    let raw = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
    assert!(raw.contains("\"type\":\"wake_raised\""));
    assert!(raw.contains("\"trigger\":\"node_failed\""));
    assert!(raw.contains("\"type\":\"supervisor_action\""));
    assert!(raw.contains("\"type\":\"run_aborted\""));

    let state = RunState::fold(&events);
    assert_eq!(state.run_status, RunStatus::Aborted);
}
```

- [ ] **Step 2: Confirm the test fails** - `cargo test -p wf-engine --test wake_events_test` (the variants/enum do not exist yet).

- [ ] **Step 3: Implementation.** In `event.rs` add the `WakeTrigger` enum (derive `Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize`, `#[serde(rename_all = "snake_case")]`) and the three new `EventPayload` variants. In `state.rs`, in `RunState::fold`, add an arm `EventPayload::RunAborted { .. } => s.run_status = RunStatus::Aborted;` and explicit no-op arms for `WakeRaised`/`SupervisorAction` (so the `match` stays exhaustive; node statuses are untouched). `RunStatus::Aborted` already exists.

- [ ] **Step 4: Tests + commit.** `cargo test -p wf-engine` (all green, including the new one). Commit: `feat(engine): wake and supervisor-action events, run_aborted fold`.

---

### Task 2: The `control.jsonl` control-command channel

**Files:**
- Create: `crates/wf-engine/src/control.rs`
- Modify: `crates/wf-engine/src/lib.rs` (`pub mod control;`)
- Test: `crates/wf-engine/tests/control_test.rs`

**Interfaces:**
- Produces (in `control.rs`):
  - `enum Control` (serde, `#[serde(tag = "cmd", rename_all = "snake_case")]`): `Retry { node: String, prompt_override: Option<String> }`, `ContinueFrom { node: String }`, `Pause`, `Abort { reason: String }`.
  - `struct ControlEntry { seq: u64, cmd: Control }` (append-numbered like EventLog).
  - `pub fn post_control(run_dir: &Path, cmd: Control) -> Result<u64, EngineError>` - atomically appends a line to `control.jsonl`, returns the seq of the written command. The sole writer is the caller (the supervisor side). Implement via `OpenOptions::append` + `writeln!` + `flush` (like EventLog); seq = the number of lines already written.
  - `pub fn read_control_after(run_dir: &Path, after_seq: Option<u64>) -> Result<Vec<ControlEntry>, EngineError>` - all commands with `seq > after_seq` (or all, if `None`). A missing file -> an empty vector.

- [ ] **Step 1: Failing test** `crates/wf-engine/tests/control_test.rs`

```rust
use wf_engine::control::{post_control, read_control_after, Control};

#[test]
fn post_and_read_control_with_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let s0 = post_control(dir.path(), Control::Pause).unwrap();
    let s1 = post_control(dir.path(), Control::Retry { node: "impl".into(), prompt_override: Some("hint".into()) }).unwrap();
    let s2 = post_control(dir.path(), Control::Abort { reason: "cancel".into() }).unwrap();
    assert_eq!((s0, s1, s2), (0, 1, 2));

    let all = read_control_after(dir.path(), None).unwrap();
    assert_eq!(all.len(), 3);
    assert!(matches!(all[0].cmd, Control::Pause));

    let tail = read_control_after(dir.path(), Some(0)).unwrap();
    assert_eq!(tail.len(), 2);
    assert_eq!(tail[0].seq, 1);
    assert!(matches!(&tail[0].cmd, Control::Retry { node, prompt_override: Some(p) } if node == "impl" && p == "hint"));

    // serialization: the cmd tag is snake_case
    let raw = std::fs::read_to_string(dir.path().join("control.jsonl")).unwrap();
    assert!(raw.contains("\"cmd\":\"continue_from\"") == false); // not written
    assert!(raw.contains("\"cmd\":\"abort\""));
}

#[test]
fn read_missing_control_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_control_after(dir.path(), None).unwrap().is_empty());
}
```

- [ ] **Step 2: Confirm it fails** - `cargo test -p wf-engine --test control_test`.

- [ ] **Step 3: Implementation of `control.rs`.** Line format: `{"seq":N,"cmd":"retry","node":"...","prompt_override":"..."}`. `ControlEntry` serializes flat: `#[derive(Serialize, Deserialize)] struct ControlEntry { seq: u64, #[serde(flatten)] cmd: Control }`. `post_control`: count the current number of lines (via `read_control_after(dir, None)?.len()` or reading the file), assign seq, append. Serialization errors -> `EngineError::Yaml` (as in event.rs).

- [ ] **Step 4: Tests + commit.** `cargo test -p wf-engine`. Commit: `feat(engine): control.jsonl channel (retry/continue_from/pause/abort)`.

---

### Task 3: Run mode and supervised drive

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs`
- Test: `crates/wf-engine/tests/supervised_drive_test.rs`

**Interfaces:**
- Produces:
  - `enum RunMode { Autonomous, Supervised }` (derive `Debug, Clone, Copy, PartialEq, Eq`; `Default` = `Autonomous`).
  - Field `pub mode: RunMode` on `RunOptions` (defaulting via `#[derive(Default)]` gives `Autonomous` - existing call sites are unaffected as long as `RunOptions` is constructed with `..Default::default()` or by field; CAUTION: check every place `RunOptions` is constructed - in `wf-mcp/src/tools.rs` and `wf-cli` - and add `mode: RunMode::Autonomous` wherever the struct is filled in by name without `..Default`).
  - `drive` behavior when a node finishes with status `Failed`/`TimedOut`:
    - `Autonomous`: unchanged - `next_node` (fallback edge or `EngineError::Invalid`). NOTHING changes here.
    - `Supervised`: record `WakeRaised { trigger, node, detail }` (trigger = `NodeTimeout` if the status is `TimedOut`, otherwise `NodeFailed`; detail = the node's output/message), then ENTER the `await_control` wait loop (see below) and apply the resulting command.
  - `Abort` check in BOTH modes: before executing each node, drive reads new `control.jsonl` commands; if there is an `Abort`, it writes `RunAborted { reason }` and returns `RunResult { outcome: RunStatus::Aborted }`.
  - Internal function `await_control(run_dir, cursor, poll) -> Result<(Control, u64), EngineError>`: polls `read_control_after` every `poll` (e.g. 50ms) until a command with seq > cursor appears; returns the first one and its seq. For testability, `poll` comes from a constant, and the test posts the command BEFORE entering the wait (see the test) or from a second thread.
  - Applying commands while waiting under supervision:
    - `Retry { node, prompt_override }`: record `SupervisorAction { action: "node_retry", .. }`, set `current = node`, and (if `prompt_override` is present) thread the one-shot prompt override into that node's execution (for 4a it is enough to: restart the node; implement `prompt_override` support as a one-shot map `node -> prompt`, applied in `execute_node` for `AgentTask`/`Prompt`; if this bloats the task, 4a could just record the SupervisorAction and restart WITHOUT the override, deferring full prompt_override to 4b - but then remove the field from the test). DECISION: implement the one-shot prompt_override, since it is the core value of retry.
    - `ContinueFrom { node }`: `SupervisorAction { action: "run_continue_from", .. }`, `current = node`, continue.
    - `Pause`: `RunPaused { reason: "supervisor pause" }`, return `RunStatus::Paused` (the supervisor will later resume/continue_from via a separate resume-run; in 4a Pause simply exits as Paused).
    - `Abort { reason }`: `RunAborted`, return `Aborted`.

- [ ] **Step 1: Failing test** `crates/wf-engine/tests/supervised_drive_test.rs`

A test workflow with an agent_task that FAILS (via `WF_AGENT_CMD` pointing at `false`/a script with exit 1), plus a second run where a `Retry` command is pre-posted and points at a stub that succeeds on the second attempt. Base this on the pattern of the existing `retry_test`/`scheduler` tests (the `WF_AGENT_CMD` env var, serialized via a `static Mutex` so parallel tests do not race on the env). Key checks:

```rust
// (pseudo-structure; adapt to the real helpers from the existing engine tests)
// 1) Supervised + node_failed with no command -> drive waits; a test thread
//    posts Abort -> the run finishes as RunStatus::Aborted, and events contain WakeRaised{node_failed}.
// 2) Supervised + node_failed, pre-posted (can't happen before drive starts, since drive starts first),
//    so: run drive on a separate thread, wait for WakeRaised to appear in events.jsonl
//    (poll read_all), then post_control(Retry{node, prompt_override:None}); with a stub
//    that succeeds on its second call, the run reaches finish -> Succeeded,
//    and events contain SupervisorAction{action:"node_retry"}.
// 3) Autonomous is unchanged: the same failing workflow with no fallback edge -> the same error/behavior as before Phase 4a.
// 4) Abort in Autonomous: post_control(Abort) before start -> the very first boundary check yields RunAborted/Aborted.
```

Implementer: flesh these scenarios out in detail on real helpers (a `WF_AGENT_CMD` stub with a counter via a counter file in a temp dir for scenario 2). Tests that use threads + polling of events.jsonl must have a reasonable timeout (e.g. 5s) and fail with a clear message rather than hang.

- [ ] **Step 2: Confirm it fails** - `cargo test -p wf-engine --test supervised_drive_test`.

- [ ] **Step 3: Implementation in `scheduler.rs`.** Add `RunMode`, the field on `RunOptions`, thread `mode` through `drive` (extend `drive`'s signature with a `mode: RunMode` parameter; update both call sites - `run` and `resume` - `resume` stays always `Autonomous` for now). Implement the Abort check at the start of each iteration, the supervised branch on Failed/TimedOut, `await_control`, applying commands, the one-shot prompt_override map. Do NOT touch the autonomous next_node branch. Update the places where `RunOptions` is constructed by name (wf-mcp `workflow_run` tool, wf-cli `run_cmd`) - add `mode: RunMode::Autonomous`.

- [ ] **Step 4: Regression + commit.** `cargo test --workspace` (ALL green, including the existing engine/mcp/cli ones). Commit: `feat(engine): supervised run mode with wake + control-driven interventions`.

---

### Task 4: Background run and `run_cancel`

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs`
- Modify: `crates/wf-engine/src/lib.rs` (re-export the new public functions)
- Test: `crates/wf-engine/tests/background_run_test.rs`

**Interfaces:**
- Produces:
  - `pub fn run_background(root: &Path, id: &str, version: Option<&str>, opts: RunOptions) -> Result<String, EngineError>` - performs PREPARATION synchronously (Registry::load, the validation gate, creating run_dir, snapshot, copy_scripts, write_run_config, RunStarted, acquiring the workdir guard), then spawns a `std::thread` into which `wf`, `cfg`, `guard`, `log`, `run_dir`, `mode` are moved, and which calls `drive(...)` through to completion. Returns the `run_id` IMMEDIATELY, without waiting for it to finish. Preparation errors are returned synchronously; errors inside the thread are written to the log as `RunFinished{outcome:"failed"}` or `RunAborted` (the thread must not panic outward; on an `Err` from drive, write a terminal event if one is not already present).
    - A subtlety with the guard: the `WorkdirGuard` must live for the whole background run - move it into the thread (a `move` closure). Make the thread detached (do not keep a JoinHandle in 4a; observation happens via events.jsonl).
    - A subtlety: the preparation portion of `run` is duplicated. Factor the shared preparation into a private `prepare_run(root, id, version, opts) -> Result<Prepared, EngineError>` (Prepared holds wf, run_id, run_dir, log, cfg, guard, mode) and reuse it in `run` (calls drive synchronously) and `run_background` (calls it on the thread).
  - `pub fn run_cancel(root: &Path, run_id: &str) -> Result<(), EngineError>` - validates via `is_safe_segment`, checks that run_dir exists (otherwise `NotFound`), posts `Control::Abort { reason: "run_cancel".into() }` to `control.jsonl`. Does NOT wait for the actual stop (drive will see the Abort at the next node boundary). Idempotent.

- [ ] **Step 1: Failing test** `crates/wf-engine/tests/background_run_test.rs`

Scenarios:
```rust
// A) run_background starts a run with no agent_task (start->prompt->finish),
//    returns run_id immediately; the test polls read_all(run_dir) until RunFinished{succeeded}
//    with a timeout; the final RunState::fold -> Succeeded.
// B) run_cancel on a nonexistent run -> Err(NotFound).
// C) run_cancel(run_id) on a slow run (a workflow with several nodes,
//    a script node with `sleep 0.2` between them could work, if supported; otherwise it is
//    enough to check that after run_cancel, control.jsonl contains Abort and that a background
//    run started under Supervised/Autonomous finishes as RunStatus::Aborted when polled).
//    If catching Aborted deterministically on a fast workflow is hard, scenario C
//    checks the contract instead: run_cancel posts Abort (read_control_after contains Abort)
//    and a run started with Abort already present finishes as Aborted (reuse
//    the check from supervised_drive_test scenario 4, but via run_background).
```
Implementer to flesh this out with timeouts and polling; tests must not hang.

- [ ] **Step 2: Confirm it fails** - `cargo test -p wf-engine --test background_run_test`.

- [ ] **Step 3: Implementation.** Refactor into `prepare_run` + `run_background` + `run_cancel`; re-export in `lib.rs` (`pub use scheduler::{..., run_background, run_cancel, RunMode};`). Confirm `run` (synchronous) still works through `prepare_run` with no change to observable behavior (existing tests green).

- [ ] **Step 4: Regression + commit.** `cargo test --workspace`. Commit: `feat(engine): background (non-blocking) run and run_cancel`.

---

### Task 5: Engine-level observation primitives for future MCP tools

**Files:**
- Modify: `crates/wf-engine/src/scheduler.rs` (or a new `crates/wf-engine/src/inspect.rs`)
- Modify: `crates/wf-engine/src/lib.rs`
- Test: `crates/wf-engine/tests/inspect_wait_test.rs`

**Interfaces:**
- Produces:
  - `pub fn wait_wake(root: &Path, run_id: &str, after_seq: Option<u64>, timeout: Duration) -> Result<Option<WakeEvent>, EngineError>` - a blocking primitive underlying `supervisor_wait_event` (4b). Polls `events.jsonl`; returns `Some(WakeEvent{ seq, trigger, node, detail })` for the first `WakeRaised` with `event.seq > after_seq`; if a terminal event (`RunFinished`/`RunAborted`) is encountered first, returns `Ok(None)` (the run is over, nothing left to wake); once `timeout` elapses, also returns `Ok(None)` (the calling tool decides whether to retry). `run_id` goes through `is_safe_segment`.
  - `struct WakeEvent { pub seq: u64, pub trigger: WakeTrigger, pub node: String, pub detail: String }` (Serialize).
  - `pub fn run_inspect(root: &Path, run_id: &str) -> Result<serde_json::Value, EngineError>` - a compiled summary for the supervisor agent: `{ run_status, nodes, outputs, context, events, wakes, actions }`, where `context` is the contents of `context.md` (if present), and `wakes`/`actions` are the extracted `WakeRaised`/`SupervisorAction` entries from the events. This is the engine-level aggregator; the `run_inspect` MCP tool (4b) will be a thin wrapper over it. `run_id` goes through `is_safe_segment`; a missing run -> `NotFound`.

- [ ] **Step 1: Failing test** `crates/wf-engine/tests/inspect_wait_test.rs`

```rust
// 1) wait_wake on a run with no wake events and a RunFinished already recorded -> Ok(None) immediately.
// 2) Manually write (via EventLog) RunStarted + WakeRaised{node_failed,"impl"}; wait_wake(after=None)
//    -> Some with trigger NodeFailed, node "impl". wait_wake(after=Some(that_wake_seq)) with a short
//    timeout followed by RunFinished -> Ok(None).
// 3) run_inspect on such a run -> JSON where wakes is non-empty, run_status is present,
//    context is a string (empty if context.md is absent).
// 4) wait_wake / run_inspect with a traversal run_id ("../../etc") -> Err(NotFound).
```

- [ ] **Step 2: Confirm it fails.**

- [ ] **Step 3: Implementation.** `wait_wake` loops with a `poll` interval (e.g. 50ms) and a deadline; reads via `read_all`, looks for the first matching `WakeRaised`, otherwise checks for terminal events. `run_inspect` assembles `RunState::fold` + reading `context.md` + filtering events.

- [ ] **Step 4: Tests + commit.** `cargo test -p wf-engine`. Commit: `feat(engine): wait_wake and run_inspect primitives for supervisor tools`.

---

### Task 6: code-ranker, status, changelog

**Files:**
- Modify: `docs/tasks.md`, `CHANGELOG.md`

- [ ] **Step 1: code-ranker gate.** `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .`. On violations - worst-first scorecard, `docs base <ID>`, fix, repeat; `cargo test --workspace` after the fixes.

- [ ] **Step 2: CHANGELOG.** Under `## [0.1.0] - in development` -> `### Added`:
`- Engine: background (non-blocking) run, run_cancel, wake-event and control-command channels, supervised run mode (groundwork for the supervisor agent).`

- [ ] **Step 3: tasks.md.** In the "Phase 4" section, mark as done the part of "Wake triggers ... wake-event queue in the engine" and add a line noting the background run/run_cancel/control channel as the completed 4a engine foundations. Mark that the supervisor MCP tools and background supervisor agent are 4b/4c (still `[ ]`). Follow the `[x]/[~]/[ ]` legend.

- [ ] **Step 4: Commit.** `docs: mark phase 4a (engine supervision machinery) done`.

---

## What is deliberately OUT of scope for Phase 4a (for the reviewer)

- The supervisor MCP tools (`supervisor_wait_event`, `run_inspect`, `node_retry`, `run_continue_from`, `run_pause`, `run_abort`, `context_append`, `supervisor_report`), the supervisor token, the capability model - Phase 4b (wrappers over the 4a engine primitives).
- `supervise: self` (the calling session becomes the supervisor agent) - Phase 4b.
- The background supervisor agent spawned by the engine; heartbeat/`supervisor_lost`; the final report + web log - Phase 4c.
- `node_slow`/`run_stuck` triggers (needs duration history, `runs/index.jsonl`) - later.
- `workflow_patch`/`run_migrated`/patch versions/promote-on-success - Phase 6 (needs the Phase 5 versioning machinery).
- Full resume after `Pause` via the same live run - in 4a, Pause simply ends the run as Paused; re-entry happens via the existing `resume` (a separate resume-run).
