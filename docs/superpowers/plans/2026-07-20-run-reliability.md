# Supervised Run Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all 13 verified field-report findings in one release: structured validation errors, honest docs, spawn-time attempt journaling, resume that never re-runs finished work, persistent control cursor, detached run drivers, a real stop, bounded loop edges, fallback sameness guard, liveness in run_status, doctor --run, apb note, and playbook_trial instruction. Ships as 0.7.0.

**Architecture:** All changes ride existing rails: the append-only event journal gains two variants and three fields; the drive loop gains an event sink, a run-level cancel flag with a control watcher, and a start mode; MCP long-lived runs re-exec the apb binary as a detached driver the way `__drive-supervised` already does. Spec: `docs/superpowers/specs/2026-07-20-run-reliability-design.md` (section references below point there).

**Tech Stack:** Rust workspace edition 2024, no new dependencies. Web: svelte + vitest (tolerance tests only).

## Global Constraints

- Branch: `feat/run-reliability`. One PR. Never push (the controller handles push and release).
- Every commit: `git commit --signoff`, message ends with the trailer line `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- No em-dashes (U+2014), no exclamation marks, no CJK anywhere. English machine-facing text.
- Gates per task before DONE: `cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` green (workspace-wide, not only the touched crate).
- Testing rules (docs/TESTING-GUIDELINES.md): integration tests only in the crate's single binary (`tests/main.rs` + `tests/suite/<name>.rs` + `mod` line); unit tests inline in src. Tempdirs only. No real network. Executable stub scripts are written with the existing synced helpers (create + write_all + sync_all before exec).
- New `EventPayload` fields only with `#[serde(default)]` (CLAUDE.md rule). State files atomic via `apb_core::fsutil`.
- EXACT event changes: `AttemptStarted` gains `#[serde(default)] pub pid: Option<u32>`; `AttemptFinished` gains `#[serde(default)] pub duration_ms: Option<u64>`; new variants `RunResumed { from_node: String }` (tag `run_resumed`) and `EdgeTraversed { from: String, to: String }` (tag `edge_traversed`), both snake_case like the rest of the enum in `crates/apb-engine/src/event.rs`.
- EXACT schema change: `Edge` in `crates/apb-core/src/schema.rs` gains `#[serde(default, skip_serializing_if = "Option::is_none")] pub max_traversals: Option<u32>`.
- EXACT validation-error shape: `VersioningError::Validation(Vec<apb_core::validate::Issue>)`; the MCP rendering is `validation failed:` followed by one line per issue `- <code> <severity> (node \`<id>\`): <message>` where the `(node ...)` segment is omitted when `issue.node` is `None`.
- EXACT V13 suffix appended to the unresolved-template message: `; known namespaces: params.*, nodes.<id>.output, nodes.<id>.report, nodes.<id>.review_note, run.instruction, run.context, run.hooks.*`.
- EXACT resume API (`crates/apb-engine/src/scheduler.rs` or a new `scheduler/resume.rs`):
  `pub enum StartMode { Rerun, After }`;
  `pub enum ResumeReason { InterruptedRestart, AdvancePastFinished, ParallelFallback, ExplicitFromNode }`;
  `pub struct ResumeDecision { pub start_node: String, pub mode: StartMode, pub reason: ResumeReason }`;
  `pub fn plan_resume(root: &Path, run_id: &str, from_node: Option<&str>) -> Result<ResumeDecision, EngineError>`.
  No-argument resume of a succeeded run returns Err whose Display contains `--from-node`.
- EXACT MCP run_resume ack: `{ "run_id": <id>, "resumed_from": <node>, "reason": "interrupted_restart" | "advance_past_finished" | "parallel_fallback" | "explicit_from_node", "detached": true }`.
- Control cursor file: `runs/<id>/control.cursor`, decimal last-applied control seq, written atomically after each batch of applied control entries; drive initializes its cursor from it.
- Driver pid file: `runs/<id>/driver.pid`, decimal pid, written atomically when `drive` starts, removed on clean drive exit. Applies to every drive invocation (CLI, in-process, detached).
- Hidden CLI driver subcommand: `apb __drive-run --root <path> --run-id <id> [--from-node <node>] [--resume]` (hidden like `__drive-supervised`), which re-opens the prepared run from `runs/<id>` and drives it to completion (fresh prepared run without `--resume`; resume path with it).
- New MCP tool `run_stop { run_id, workspace? }`; new CLI subcommands `apb stop <run_id>` and `apb note <run_id> <text>`; new CLI flag `apb doctor --run <id>`.
- Bounded loop semantics (spec section 6): a bounded edge whose folded traversal count has reached `max_traversals` is treated as non-matching during edge selection; traversing a bounded edge journals `edge_traversed`; `RunState` gains `pub edge_counts: BTreeMap<(String, String), u32>` folded from those events; a node that already has a `node_finished` in the folded state skips the result-cache lookup on re-execution.
- V11 changes to reject only cycles containing no edge with `max_traversals`; its message names `max_traversals` as the remedy. New V30 error when `max_traversals` is `Some(0)`, message `max_traversals must be at least 1`.
- Fallback guard: before executing chain step idx > 0, skip the step when its resolved (agent, model) pair equals the pair of the step that just failed; skipped steps emit no events; an exhausted chain fails the node exactly as an ended chain does today.
- run_status additions (non-breaking): top-level `driver_alive: bool | null` (null when `driver.pid` is absent); new map `node_times: { <id>: { "started_ms": u64, "attempt_age_ms": u64 | null, "attempt_pid": u32 | null } }`; a running node whose journaled attempt pid is dead reports status string `lost` in the existing `nodes` map.
- Version bump to 0.7.0 with `docs/release-notes/v0.7.0.md` titled `apb 0.7.0: supervised run reliability`.

---

### Task 1: Structured validation errors and template scanning

**Files:**
- Modify: `crates/apb-core/src/validate.rs` (V13 suffix in `check_templates`, scan playbook-node `instruction`, derive `Clone` on `Issue` if missing)
- Modify: `crates/apb-core/src/versioning.rs:714-726, 793-809` (`VersioningError::Validation(Vec<Issue>)`, both collapse sites keep full issues, errors only as today)
- Modify: `crates/apb-mcp/src/tools.rs:53-63` (render per Global Constraints)
- Test: `crates/apb-core/tests/suite/` (existing validate/versioning suite files) and `crates/apb-mcp/tests/suite/` (existing tools suite)

**Interfaces:**
- Produces: `VersioningError::Validation(Vec<Issue>)` with `Issue { code, severity, message, node }` intact end to end. Task 10 documents the namespaces; nothing else depends on this task.

- [ ] **Step 1: Write failing tests.** Core: a playbook whose prompt references `{{outputs.plan}}` produces a V13 issue whose message contains both the variable and the exact namespaces suffix; a playbook node with `instruction: "{{outputs.x}}"` produces V13 (currently passes validation silently, so this asserts the new scan); `create_version` on an invalid playbook returns `VersioningError::Validation(issues)` where issues expose message and node. MCP: `playbook_update` with an invalid playbook returns an error whose text contains `validation failed:` and a line matching `- V13 error (node \`<id>\`):` with the human message, not a bare code list.
- [ ] **Step 2: Run to verify failure, implement.** Move nothing; change the two collapse sites to keep `Vec<Issue>` (filtering to `Severity::Error` as today), update the enum, update every `match` on `VersioningError::Validation` (grep the workspace; the CLI printer at `crates/apb-cli/src/run.rs` may format the codes today and must render the same line format as MCP).
- [ ] **Step 3: Run workspace tests to green, gates, commit** - `feat(core): structured validation errors with messages and node paths`.

---

### Task 2: Spawn-time attempt journaling

**Files:**
- Modify: `crates/apb-engine/src/event.rs` (two fields per Global Constraints)
- Modify: `crates/apb-engine/src/adapter.rs` (on-spawn callback carrying child pid through `run_cancellable`; follow the existing option-struct or parameter style of the file)
- Modify: `crates/apb-engine/src/scheduler/node.rs` and `crates/apb-engine/src/scheduler.rs` (event sink: sequential path hands `execute_node` direct log access; the parallel batch path in `scheduler/parallel.rs` wraps the log in a `Mutex`; `attempt_started` appended at spawn with pid, `attempt_finished` appended at return with `duration_ms` measured from spawn)
- Test: `crates/apb-engine/tests/suite/` (scheduler/adapter suites)

**Interfaces:**
- Consumes: nothing new.
- Produces: `attempt_started` in the journal at spawn time with `pid: Option<u32>`; `attempt_finished.duration_ms`. Tasks 3, 8, 9 rely on spawn-time presence and the pid.

- [ ] **Step 1: Write failing tests.** (a) Run a single stub-agent node; assert the journal orders `attempt_started` strictly before `attempt_finished` with distinct timestamps and `attempt_started.pid` is Some; assert `attempt_finished.duration_ms` is Some. (b) Crash simulation: a stub agent that sleeps; kill the drive mid-attempt is not portable in-process, so instead simulate by writing the journal state a crash produces - drive a stub that spawns, then have the stub exit while asserting that folding a journal containing `attempt_started` without `attempt_finished` yields the interrupted state (`state.rs:184-192` fires). A direct fold unit test on a hand-built journal is acceptable and required; label it clearly.
- [ ] **Step 2: Implement.** Keep `execute_node`'s return-batch shape for all non-lifecycle events; only `attempt_started`/`attempt_finished` move to the sink. The adapter callback runs after a successful `Command::spawn` and receives `child.id()`.
- [ ] **Step 3: Workspace tests, gates, commit** - `feat(engine): journal attempt lifecycle at spawn time with pid and duration`.

---

### Task 3: Resume rework

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs:1454-1554` (extract `plan_resume` per Global Constraints; `resume_inner` uses it; journal `run_resumed` instead of the `RunPaused` marker; drive gains `StartMode` - `After` seeds the frontier by evaluating the start node's edges against folded status and outputs instead of re-executing it)
- Modify: `crates/apb-engine/src/event.rs` (`RunResumed` variant), `crates/apb-engine/src/state.rs` (fold `RunResumed` to running; keep the old `RunPaused` fold unchanged)
- Modify: `crates/apb-mcp/src/tools.rs:888-891` and `crates/apb-mcp/src/server/run.rs:455-468` (run_resume returns the ack shape; actual detachment arrives in Task 7 - until then the tool may still drive synchronously after computing the ack, returning the ack fields plus the current blocking behavior, clearly commented as replaced in the detach task)
- Test: `crates/apb-engine/tests/suite/` resume suite

**Interfaces:**
- Consumes: Task 2 spawn-time `attempt_started`.
- Produces: `plan_resume`, `ResumeDecision`, `StartMode`, `ResumeReason` (exact shapes in Global Constraints) - Task 7 calls `plan_resume` for the ack before spawning the driver.

- [ ] **Step 1: Write failing tests** (drive stub playbooks through real runs, then resume): (a) interrupted node (journal ends with its `node_started`) resumes at that node with reason `InterruptedRestart`; (b) journal ending in `node_finished` for node X resumes at X's successor, X is not re-executed (assert exactly one `node_finished` for X in the final journal), reason `AdvancePastFinished`; (c) two interrupted nodes resume from `last_node`, reason `ParallelFallback`; (d) no-arg resume of a succeeded run errors mentioning `--from-node`; (e) `from_node` override works on a failed terminal run, reason `ExplicitFromNode`; (f) after resume the folded run status is running (via `run_resumed`), not paused.
- [ ] **Step 2: Implement, workspace tests, gates, commit** - `feat(engine): resume restarts interrupted work and never re-runs finished nodes`.

---

### Task 4: Control cursor, apb note, run instruction in context

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs:553, 594` (cursor init from `runs/<id>/control.cursor`, atomic write after applying entries)
- Modify: `crates/apb-engine/src/scheduler/supervisor.rs:193-197` (`rebuild_context_md` emits a leading `## run instruction` section when the run config carries a non-empty instruction)
- Modify: `crates/apb-cli/src/main.rs` (new subcommand `note <run_id> <text>` dispatching to `apb_engine::scheduler::post_supervisor_command(root, run_id, Control::ContextAppend { note })`)
- Test: `crates/apb-engine/tests/suite/` (control/supervisor suite), `crates/apb-cli/tests/suite/` (CLI suite)

**Interfaces:**
- Consumes: existing `post_supervisor_command` (`scheduler.rs:495-508`).
- Produces: cursor file consumed by Task 9's doctor (unapplied-control check).

- [ ] **Step 1: Write failing tests.** (a) Post one ContextAppend, drive to completion, resume-drive again: the journal contains exactly one `supervisor_action` for that note (today it contains two). (b) A posted Pause consumed by drive N does not re-pause drive N+1. (c) A run started with an instruction produces a `context.md` whose first section is `## run instruction` with the text; a run without one has no such section. (d) CLI: `apb note <run_id> hello` appends a ContextAppend entry to `control.jsonl`.
- [ ] **Step 2: Implement, workspace tests, gates, commit** - `feat(engine): persist the control cursor, surface the run instruction, add apb note`.

---

### Task 5: Bounded loop edges

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`max_traversals` per Global Constraints), `crates/apb-core/src/validate.rs` (`check_cycles` V11 relaxation, V30; register V30 in whatever code table exists)
- Modify: `crates/apb-engine/src/event.rs` (`EdgeTraversed`), `crates/apb-engine/src/state.rs` (`edge_counts` fold), `crates/apb-engine/src/scheduler.rs` (`advance_frontier` cap check + journaling; result-cache lookup skip for nodes with a folded `node_finished` - find the cache lookup site via codegraph `node result cache`)
- Test: `crates/apb-core/tests/suite/` validate suite, `crates/apb-engine/tests/suite/` scheduler suite

**Interfaces:**
- Consumes: nothing from other tasks (state.rs edits coordinate with Task 3's fold changes; run `git pull` of the branch state and merge textually).
- Produces: `edge_counts` on `RunState`; Task 10 documents the YAML, Task 11 renders a cyclic graph.

- [ ] **Step 1: Write failing validator tests.** Cycle without any bounded edge rejects with V11 and the message mentions `max_traversals`; the same cycle with `max_traversals: 3` on the back edge validates; `max_traversals: 0` rejects with V30 message `max_traversals must be at least 1`; schema YAML round-trip preserves the field and omits it when absent.
- [ ] **Step 2: Write failing engine tests.** A three-node loop playbook (review -> fix on failure with `max_traversals: 2`, fix -> review, review -> done on success) where the review stub fails every time: the run executes review exactly 3 times (initial + 2 loop iterations), fix exactly 2 times, journals exactly 2 `edge_traversed` events, then review's failure with the exhausted edge non-matching leaves the existing no-edge behavior (run fails at review) - assert that terminal shape. A variant where review succeeds on the second pass proceeds to done. A resume mid-loop preserves the count (kill after first traversal by driving a journal fixture, resume, assert the cap still limits total traversals). Cache bypass: with the node result cache enabled, the second execution of review runs the agent again instead of replaying the first verdict (assert two distinct `attempt_started` for review).
- [ ] **Step 3: Implement, workspace tests, gates, commit** - `feat: bounded loop edges with max_traversals`.

---

### Task 6: Fallback sameness guard

**Files:**
- Modify: `crates/apb-engine/src/scheduler/node.rs:128-137, 197-205` (skip identical consecutive binding; same guard in `execute_finish_answer` at `node.rs:445-453`)
- Test: `crates/apb-engine/tests/suite/` profile-run/fallback suite

**Interfaces:**
- Consumes: manifest chain entries (agent + model already resolved there).
- Produces: nothing downstream.

- [ ] **Step 1: Write failing tests.** (a) A profile whose chain is claude(model X) -> claude(model X), stub agent always fails: the journal contains attempts only for the first step and no `fallback_triggered` event, and the node fails exhausted. (b) claude(model X) -> claude(model Y) still falls back (one `fallback_triggered`, attempts for both). Use the existing stub-agent fixtures of the fallback tests.
- [ ] **Step 2: Implement, workspace tests, gates, commit** - `fix(engine): skip fallback steps with an identical agent and model binding`.

---

### Task 7: Detached driver

**Files:**
- Modify: `crates/apb-cli/src/main.rs` + `crates/apb-cli/src/run.rs` (hidden `__drive-run` per Global Constraints, modeled on `__drive-supervised` at `run.rs:355-408`; a public helper `spawn_detached_driver(root, run_id, from_node: Option<&str>, resume: bool) -> io::Result<u32>` is NOT possible from apb-mcp since mcp cannot depend on cli - put the spawn helper in `crates/apb-engine/src/driver.rs` (new) using `std::env::current_exe()`, stdio null, and have both cli and mcp call it)
- Modify: `crates/apb-engine/src/scheduler.rs` (driver.pid write/remove inside `drive`/`drive_prepared` per Global Constraints; a function `drive_run_from_dir(root, run_id) -> Result<...>` that re-opens a prepared, not-yet-driven run from `runs/<id>` - the manifest and journal are already on disk after prepare)
- Modify: `crates/apb-mcp/src/tools.rs` + `crates/apb-mcp/src/server/run.rs` (background:true, supervise:self, and run_resume spawn the detached driver after the policy gate and manifest snapshot complete in-process; run_resume computes `plan_resume` first and returns the Task 3 ack with `detached: true`)
- Test: `crates/apb-engine/tests/suite/` and `crates/apb-cli/tests/suite/`

**Interfaces:**
- Consumes: Task 3 `plan_resume`.
- Produces: `apb_engine::driver::spawn_detached_driver`, `runs/<id>/driver.pid`. Tasks 8 and 9 read driver.pid.

- [ ] **Step 1: Write failing tests.** Engine: after a normal in-process run completes, `driver.pid` was removed (assert absent) but a probe mid-run sees it (drive a stub that blocks on a fifo/file; while blocked, assert driver.pid exists and contains a live pid). CLI integration: invoke the real apb binary (`env!("CARGO_BIN_EXE_apb")`) with `__drive-run` against a prepared tempdir run, let the parent test process merely wait on the child - then a harder assertion: spawn via `spawn_detached_driver`, exit nothing (the test stays alive), and poll the run dir until `run_finished` appears, proving the driver runs the play to completion without the caller driving it. MCP: `run_resume` on a paused fixture returns the ack shape immediately (bounded wall-clock, assert under 5s while the stub node sleeps 10s) and the run completes afterward (poll the journal).
- [ ] **Step 2: Implement, workspace tests, gates, commit** - `feat: detached run drivers that survive the launching process`.

---

### Task 8: Stop that interrupts in-flight work

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs` (run-level `Arc<AtomicBool>` cancel replacing the fresh `AtomicBool` at `scheduler.rs:1142`; watcher thread polling `read_control_after` every 200 ms for `Control::Abort`, setting the flag; watcher joins/stops when drive ends)
- Modify: `crates/apb-engine/src/driver.rs` or a new `crates/apb-engine/src/stop.rs` (`pub fn stop_run(root, run_id) -> Result<StopOutcome, EngineError>`: post Abort; when `driver.pid` is absent or dead and the folded state is non-terminal, append `run_aborted` directly under the event lock; `pub enum StopOutcome { SignaledLiveDriver, FinalizedDeadRun, AlreadyTerminal }`)
- Modify: `crates/apb-mcp/src/tools.rs` + server (new tool `run_stop` calling `stop_run`), `crates/apb-cli/src/main.rs` (`apb stop <run_id>`)
- Test: `crates/apb-engine/tests/suite/`, `crates/apb-mcp/tests/suite/`, `crates/apb-cli/tests/suite/`

**Interfaces:**
- Consumes: Task 7 driver.pid.
- Produces: `stop_run`, `StopOutcome` (doctor in Task 9 mentions nothing of these; independent).

- [ ] **Step 1: Write failing tests.** (a) In-flight interrupt: drive (on a thread) a stub agent that sleeps 30s; post Abort via `stop_run`; assert the drive returns well under the sleep (agent tree killed via the wired cancel) and the journal ends with `run_aborted`; total test wall-clock bounded. (b) Dead-driver finalize: build a run dir whose journal ends mid-run with no driver.pid; `stop_run` returns `FinalizedDeadRun` and the journal gains `run_aborted`. (c) Terminal run: `AlreadyTerminal`, journal untouched. (d) MCP `run_stop` and CLI `apb stop` smoke against fixture (b).
- [ ] **Step 2: Implement, workspace tests, gates, commit** - `feat: run stop interrupts in-flight nodes and finalizes dead runs`.

---

### Task 9: Liveness in run_status and doctor --run

**Files:**
- Modify: `crates/apb-engine/src/progress.rs` / `crates/apb-mcp/src/tools.rs:761-795` (run_status additions per Global Constraints; `pid_alive` is in `crates/apb-engine/src/workdir.rs:19-29` - re-export or move it to a shared spot in engine rather than duplicating)
- Create: `crates/apb-engine/src/run_doctor.rs` (`pub struct RunCheck { pub status: &'static str, pub subject: String, pub detail: String }`, `pub fn diagnose_run(root: &Path, run_id: &str) -> Result<Vec<RunCheck>, EngineError>` covering: folded run/node statuses, open attempts with pid liveness, driver.pid liveness, workdir lock holder liveness, control entries with seq beyond the persisted cursor, duplicate supervisor_action count)
- Modify: `crates/apb-cli/src/main.rs` (`doctor` gains `--run <id>` routing to `diagnose_run` printing one line per check like core doctor does)
- Test: `crates/apb-mcp/tests/suite/` run_status suite, `crates/apb-cli/tests/suite/` doctor suite

**Interfaces:**
- Consumes: Task 2 `attempt_started.pid`, Task 4 cursor file, Task 7 driver.pid.
- Produces: nothing downstream.

- [ ] **Step 1: Write failing tests.** run_status on a hand-built journal (attempt_started with a dead pid, no attempt_finished) reports that node as `lost`, includes `node_times` with `started_ms` and null-able fields, and `driver_alive: null` absent a pid file. A live run fixture (stub sleeping) reports `attempt_age_ms` growing and `driver_alive: true`. Doctor: a wedged fixture (dead attempt pid + stale driver.pid + one unapplied control entry) prints checks flagging all three; a healthy completed run prints all ok.
- [ ] **Step 2: Implement, workspace tests, gates, commit** - `feat: liveness in run_status and a per-run doctor`.

---

### Task 10: Trial instruction and documentation

**Files:**
- Modify: `crates/apb-mcp/src/server/args.rs:218-227` (`instruction: Option<String>` on `PlaybookTrialArgs`), `crates/apb-mcp/src/tools.rs:383-386` (thread into `RunOptions.instruction`)
- Modify: `docs/HOWTO-authoring.md` (fix `{{outputs.plan}}` at line 114 to `{{nodes.plan.output}}`; new sections per spec 3.2: Template variables, Human review and conditional edges with a worked example, Bounded loops with the spec section 6 YAML example; mention trial instruction where trial is described)
- Modify: `docs/MCP.md` (run_stop, run_resume ack shape and detached lifecycle, wait_event `after_seq`/`timeout_ms` cursor pattern, trial instruction), `README.md` command table (note, stop, doctor --run)
- Test: `crates/apb-mcp/tests/suite/` (trial suite)

**Interfaces:**
- Consumes: ack shape from Task 3, tools from Tasks 8-9 (documentation only).
- Produces: nothing downstream.

- [ ] **Step 1: Failing test:** trial of a playbook whose node prompt is exactly `{{run.instruction}}` with `instruction: "PING"` produces a node output containing `PING` (stub agent echoing its prompt).
- [ ] **Step 2: Implement + docs. Verify docs contain no em-dash, no exclamation marks, and that every documented namespace matches the Task 1 suffix verbatim. Workspace tests, gates, commit** - `feat(mcp): trial instruction and reliability docs`.

---

### Task 11: Web tolerance

**Files:**
- Modify/Test: `web/src/lib/` run-view and graph components and their vitest suites (locate via `grep -r "attempt_started\|node_finished" web/src`)

**Interfaces:**
- Consumes: event shapes from Tasks 2, 3, 5 (pid, duration_ms, run_resumed, edge_traversed), cyclic graphs from Task 5.

- [ ] **Step 1: Write failing/characterization tests:** the run journal view renders (no throw, items listed) a fixture journal containing `run_resumed`, `edge_traversed`, `attempt_started` with `pid`, `attempt_finished` with `duration_ms`; the graph layout does not throw on a playbook with a cycle (review/fix/done fixture) and renders all three nodes.
- [ ] **Step 2: Fix whatever breaks (unknown event kinds must render generically, cycle layout must not infinite-loop). Run `bun run test` and `bun run check` clean, commit** - `web: tolerate reliability events and cyclic graphs`.

---

### Task 12: Version 0.7.0 and release notes

**Files:**
- Modify: root `Cargo.toml` (0.6.0 -> 0.7.0) and every inter-crate `version = "0.6.0"` pin (`grep -rn '0\.6\.0' Cargo.toml crates/*/Cargo.toml`); refresh `Cargo.lock` via a build
- Create: `docs/release-notes/v0.7.0.md`

- [ ] **Step 1: Bump versions, build, `cargo test --workspace`.**
- [ ] **Step 2: Release notes** heading style copied from `docs/release-notes/v0.6.0.md`, title `## apb 0.7.0: supervised run reliability`, one paragraph = one line, no AI-authorship markers, no em-dashes, no exclamation marks. Sections: what the field report was and the headline fixes (structured validation errors, spawn-time journaling, resume that never re-runs finished work, detached drivers, real stop, bounded loops); supervision improvements (liveness and timestamps in run_status, doctor --run, apb note, control cursor, run instruction in every context, fallback sameness guard, trial instruction); Known limitations (no in-flight note delivery - stop plus note plus resume is the pattern; no token/cost capture; no automatic respawn of lost attempts).
- [ ] **Step 3: Gates, commit** - `chore: bump workspace version to 0.7.0 for the run reliability release`.

---

## Final verification (controller)

`cargo metadata --format-version 1 >/dev/null && code-ranker check .`; `cargo clippy --release --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cd web && bun run test && bun run check`; whole-branch review; PR; merge; tag v0.7.0.
