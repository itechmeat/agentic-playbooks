# Resilience and parallelism implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issues #75 (systemic parallel fan-out and join barriers), #71 (survive agent session interruptions), and #74 (six supervised-run findings) in one PR, per `docs/superpowers/specs/2026-08-05-resilience-and-parallelism-design.md`.

**Architecture:** Join semantics gain SCC-aware implicit barriers and liveness-aware readiness in `apb-core`/`apb-engine::parallel`; the scheduler's concurrent batch path opens to every run mode behind a `max_parallel` cap and reaches cache/artifact parity; the attempt lifecycle gains a status-file-first verdict, an interrupted classification, an infrastructure failure classifier with bounded backoff, and drive-entry reaping; edge routing gains a structured `output_field` condition; the finish composer always sees run context.

**Tech Stack:** Rust workspace edition 2024 (apb-core, apb-engine, apb-mcp, apb-cli, apb-server), TDD per `docs/TESTING-GUIDELINES.md`, build discipline per `docs/BUILD-OPTIMIZATION.md`.

## Global Constraints

- TDD: write the failing test first, watch it fail, implement, watch it pass. Integration tests go ONLY under `crates/<c>/tests/suite/` with one `mod` line in `tests/main.rs`; never a new `tests/*.rs` binary.
- Every test mutating process-global state (`APB_AGENT_CMD`, `APB_CONFIG_DIR`, `HOME`, PATH) takes the shared `common::env_lock()` and restores via a Drop guard. Every wait is bounded and names what it waited for. Stub agents are `#!/bin/sh` scripts via `APB_AGENT_CMD` (written with `common::write_sync`).
- New `EventPayload` fields only with `#[serde(default)]`. State files via `apb_core::fsutil` atomic writes. No em-dashes (U+2014), no exclamation marks in docs or user-facing strings; machine-facing fields in English.
- Scoped gates while iterating (`cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings`); ONE cargo invocation at a time, never in parallel with another; full `cargo test --workspace` at task boundaries that touched apb-core/apb-engine.
- Commit per completed task with DCO signoff (`git commit --signoff`) and a `Co-Authored-By:` trailer naming the acting model. Never push.
- Do not weaken existing assertions. Tests that pin behavior to preserve: `parallel_e2e_test.rs` (incl. `finish_in_one_branch_cancels_the_other` FIFO pin), `loop_edges_test.rs` first-arrival loop semantics, `failure_stop_test.rs` batch-order failure scan, `supervised_drive_test.rs` park/wake flow, `status_file_test.rs`, `retry_test.rs`, `cache_test.rs`.
- Research maps with verified anchors: `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/research-75.md` and `research-71-74.md` (session-local, read them before coding).

---

### Task 1: SCC helper, implicit joins, liveness-aware join readiness (#75, spec 1.1)

**Files:**
- Create: `crates/apb-core/src/graphutil.rs` (Tarjan SCC over playbook edges, lifted from `validate/graph.rs:245-346`; plus forward reachability)
- Modify: `crates/apb-core/src/validate/graph.rs` (check_cycles reuses the helper), `crates/apb-core/src/lib.rs`
- Modify: `crates/apb-engine/src/parallel.rs` (`is_join`, `join_readiness` take the active-node set; implicit all-join for acyclic fan-in; dead-source satisfaction), `crates/apb-engine/src/scheduler/node.rs` (`seed_successors`, `advance_frontier` call sites), `crates/apb-engine/src/scheduler.rs` (pass active set)
- Test: `parallel.rs` unit tests, `crates/apb-engine/tests/suite/parallel_e2e_test.rs` (extend)

**Interfaces:**
- Produces: `apb_core::graphutil::{sccs(&Playbook) -> Vec<Vec<String>>, reachable(&Playbook, from: &[&str]) -> BTreeSet<String>}` (exact naming may follow codebase idiom); `parallel::is_join(playbook, node) -> bool` now true also for multi-input acyclic fan-in without `join:`; `parallel::join_readiness(playbook, node, state, active: &[String])` where a non-terminal source unreachable from `active` counts as satisfied (dead).

- [ ] **Step 1:** Failing unit tests in `parallel.rs`: (a) diamond without `join:` reports `is_join == true`; (b) loop merge (`check -> tick -> check` shape) reports `is_join == false`; (c) `join_readiness` All with one source terminal and the other dead (not reachable from active set) is `ReadySuccess`; (d) with the other source still reachable it is `NotReady`.
- [ ] **Step 2:** Run `cargo test -p apb-engine --lib parallel` and watch them fail.
- [ ] **Step 3:** Implement `graphutil` in apb-core (move Tarjan, keep `check_cycles` green), then the parallel.rs changes and call-site plumbing.
- [ ] **Step 4:** Failing e2e in `parallel_e2e_test.rs`: a barrier-less diamond of two `prompt` branches must start the join only after both branches finished; an either-or conditional merge (only one branch selected) must still reach finish (no deadlock).
- [ ] **Step 5:** `cargo test -p apb-engine` green including `loop_edges_test`, `parallel_e2e_test`, `max_loops_test`; `cargo test -p apb-core` green.
- [ ] **Step 6:** Scoped clippy + fmt; commit.

### Task 2: Validator rules for join and cross-branch templates (#75, spec 1.2, 1.5-static)

**Files:**
- Modify: `crates/apb-core/src/validate/graph.rs`, `crates/apb-core/src/validate/templates.rs`, `crates/apb-core/src/validate/mod.rs`
- Test: validator unit tests beside the rules (existing pattern in `validate/mod.rs` tests)

**Interfaces:**
- Produces: V36 (Error): `Edge.join` value other than `all`/`any`. V37 (Warning): incoming edges of one node mix `all` and `any`. V38 (Warning): a template reads `nodes.<id>.output|report` where `<id>` is not guaranteed terminal before the reading node (reuse `reachable_from` per V10 pattern at `graph.rs:220-241`, now join-aware via Task 1 helpers).

- [ ] **Step 1:** Failing tests: `join: al` yields V36 error; mixed `all`+`any` yields V37 warning; a template read across un-barriered parallel branches yields V38 warning; a read behind a join (implicit or explicit) does not.
- [ ] **Step 2:** Watch fail, implement, watch pass (`cargo test -p apb-core`).
- [ ] **Step 3:** Scoped clippy + fmt; commit.

### Task 3: Concurrent batch in every run mode with max_parallel (#75, spec 1.3)

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`Defaults.max_parallel: Option<usize>`, `#[serde(default)]`)
- Modify: `crates/apb-engine/src/scheduler/entry.rs` (`RunOptions.max_parallel`), `crates/apb-engine/src/run_config.rs` (`RunConfig.max_parallel`, persisted), `crates/apb-engine/src/scheduler.rs` (remove `mode == Autonomous` gate at :572; chunked admission capped at resolved max_parallel, engine default 4; supervised batch-tail parking per failed node in batch order)
- Test: `crates/apb-engine/tests/suite/parallel_concurrency_test.rs` (extend), `supervised_drive_test.rs` (extend)

**Interfaces:**
- Consumes: nothing new from Task 1 beyond compiling together.
- Produces: precedence `node has none -> defaults.max_parallel -> RunOptions.max_parallel -> RunConfig.max_parallel -> 4` (mirror `node.rs:240` pattern); supervised runs batch non-interactive `agent_task`/`script`; failures park after the batch, in batch order.

- [ ] **Step 1:** Failing test: supervised-mode diamond with two sleeping `script` branches completes with wall clock proving overlap (mirror `parallel_script_branches_run_concurrently` but `RunMode::Supervised` with a pre-seeded control or auto-continue path that avoids parking).
- [ ] **Step 2:** Failing test: `max_parallel: 1` forces the same diamond to serialize (wall clock at least the sum of the two sleeps).
- [ ] **Step 3:** Failing test: supervised batch with one failing branch raises the `node_failed` wake after the batch completes and a posted `Retry` recovers (extend `supervised_drive_test.rs` helpers `run_in_background`/`wait_for_wake`).
- [ ] **Step 4:** Implement; keep `steps += batch.len()` accounting; keep batch-order failure scan determinism.
- [ ] **Step 5:** `cargo test -p apb-engine -p apb-mcp -p apb-server` green (engine touched); fmt + clippy; commit.

### Task 4: Batch parity (cache, artifacts) and missing-input observability (#75, spec 1.4, 1.5-runtime; #74 F4 groundwork)

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs` (factor the sequential per-node cache flow at :1393-1543 into a reusable unit; batch members use it; batch `NodeFinished` carries real artifacts instead of `Vec::new()` at :671-679)
- Modify: `crates/apb-engine/src/scheduler/node.rs` or `context.rs` call path (journal a missing-input anomaly when a rendered template references `nodes.<id>.*` with no successful record; rendered text stays empty-string so cache keys do not move)
- Test: `crates/apb-engine/tests/suite/cache_test.rs` (extend for batched cache hit), `parallel_e2e_test.rs` or a new suite module for the missing-input event

**Interfaces:**
- Produces: batch path admits/looks up cache and records artifacts identically to the sequential path; an observable journaled anomaly (reuse the existing anomaly mechanism, cf. empty-output anomaly at `node.rs:833-850`) naming reader node and missing reference.

- [ ] **Step 1:** Failing test: two-branch autonomous batch where one member has a cache context: second run hits the cache (no agent spawn; assert via stub-agent side-effect file) and its `NodeFinished.artifacts` is non-empty.
- [ ] **Step 2:** Failing test: a node whose prompt reads `{{nodes.never_ran.output}}` journals the missing-input anomaly while still rendering empty.
- [ ] **Step 3:** Implement the refactor (one shared per-node execution unit used by both arms); watch pass.
- [ ] **Step 4:** Full `cargo test -p apb-engine -p apb-mcp -p apb-server`; fmt + clippy; commit.

### Task 5: Status-file verdict over exit, require_verdict, interruption retry context (#71 items 1+3+5-context, #74 F1; spec 2.1, 2.2)

**Files:**
- Modify: `crates/apb-engine/src/scheduler/node.rs` (read status file on the adapter `Err` branch too, around :793-832 and :956-992; `require_verdict` resolution and interrupted classification; interruption note in retry prompt assembly), `crates/apb-engine/src/scheduler/status_file.rs` (note gating), `crates/apb-core/src/schema.rs` (`AgentTask.require_verdict: bool` + `Defaults.require_verdict: Option<bool>`, `#[serde(default)]`), `crates/apb-engine/src/event.rs` (AttemptFinished carries preserved partial output; new fields `#[serde(default)]`)
- Test: `crates/apb-engine/tests/suite/status_file_test.rs` (extend), `retry_test.rs` (extend)

**Interfaces:**
- Produces: adapter `Err` + valid success status file = succeeded attempt + journaled tail-failure anomaly; adapter `Err` + failure status file = failed attempt with agent outputs preserved; `require_verdict` in force + exit without valid status file = `AttemptFinished { status: "interrupted" }`, retry consumed, next attempt prompt carries the interruption note; `STATUS_FILE_NOTE` appended whenever `require_verdict` is in force (in addition to today's success_check gating).

- [ ] **Step 1:** Failing test: stub agent writes `{"status":"success","outputs":{...}}` to `$APB_STATUS_FILE` then `exit 1`; node succeeds, outputs flow downstream, an anomaly notes the tail exit.
- [ ] **Step 2:** Failing test: `require_verdict: true` node whose stub prints a mid-work message and exits 0 without a status file: attempt journaled `interrupted`, a retry fires, and the retry prompt (dump `"$@"` from the stub) contains the interruption note; second attempt writes the status file and the node succeeds.
- [ ] **Step 3:** Failing test: without `require_verdict`, exit 0 without a status file stays succeeded (existing default preserved; extend the existing absent-file test only if it does not already pin this).
- [ ] **Step 4:** Implement; watch pass; `cargo test -p apb-engine` then dependents; fmt + clippy; commit.

### Task 6: Failure classification, bounded backoff, non-transient fallback suppression, journal observability (#71 item 2, #74 F2; spec 2.3)

**Files:**
- Create: `crates/apb-engine/src/failure_class.rs` (curated pattern table -> `FailureKind { Transient, Auth, Budget, Agent }`)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (classification on adapter `Err`; infra retry budget of 2 with 5 s / 30 s tick-poll backoff honoring cancel/abort; Auth/Budget skip remaining retries and same-agent fallback steps), `crates/apb-engine/src/event.rs` (`AttemptFinished.failure_kind: Option<String>`, `FallbackTriggered.from_model/to_model`, all `#[serde(default)]`)
- Test: new suite module `crates/apb-engine/tests/suite/failure_class_test.rs` (unit matrix beside the table is also fine), `retry_test.rs` (extend)

**Interfaces:**
- Consumes: Task 5's Err-branch handling (classification runs after the status-file check: a written verdict wins).
- Produces: classifier is pure and unit-tested over stderr/stdout detail strings (network reset/refused/DNS/5xx/429/overloaded -> Transient; 401/unauthorized/token expired/re-login -> Auth; spend limit/quota/billing -> Budget; else Agent); backoff sleeps are bounded, tick-polled, and covered by a test that proves an abort lands during backoff.

- [ ] **Step 1:** Failing unit matrix for the classifier.
- [ ] **Step 2:** Failing e2e: stub fails twice with a transient-looking message then succeeds; node succeeds while consuming zero node `max_retries` (assert via a `max_retries: 0` node) and the journal shows the infra retries with `failure_kind`.
- [ ] **Step 3:** Failing e2e: stub fails with a spend-limit message; a same-agent fallback step is skipped (no `fallback_triggered` to the same agent), a different-agent step still runs; `FallbackTriggered` carries models.
- [ ] **Step 4:** Failing test: abort posted during backoff ends the run promptly (bounded wait, assert well under the 30 s backoff).
- [ ] **Step 5:** Implement; watch pass; scoped then dependent tests; fmt + clippy; commit.

### Task 7: Reap dead open attempts at drive entry (#71 item 4; spec 2.4)

**Files:**
- Modify: `crates/apb-engine/src/scheduler/entry.rs` and/or `scheduler/resume.rs` (on drive start over an existing run dir: open attempts with dead pid are journaled `AttemptFinished { status: "interrupted" }` and the node re-enters scheduling), reusing `liveness.rs` folds
- Test: `crates/apb-engine/tests/suite/` new module or extend `resume` coverage

**Interfaces:**
- Consumes: `liveness::open_attempts` + pid validation rules from TESTING-GUIDELINES (validate pid > 0, prefer a reaped child's pid as the dead fixture).
- Produces: resuming a run whose driver died mid-attempt retries the node automatically instead of reporting only `lost`.

- [ ] **Step 1:** Failing test: synthesize a run dir with an open `attempt_started` carrying a plausible-but-absent pid (spawn, reap, reuse the number per guidelines); drive/resume the run; the attempt closes as interrupted and the node re-executes to success via the stub agent.
- [ ] **Step 2:** Implement; watch pass; `cargo test -p apb-engine -p apb-mcp`; fmt + clippy; commit.

### Task 8: output_field edge condition, finish context fix, deliverable check (#74 F5, F6, F4; spec 2.5-2.7)

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`EdgeCondition::OutputField { node, field, equals }`), `crates/apb-engine/src/parallel.rs` (`edge_matches` arm: parse source output as JSON, compare top-level field as string), `crates/apb-core/src/validate/graph.rs` (source-node checks beside V09/V10 and the RouteKey arm at :383-406)
- Modify: `crates/apb-engine/src/context.rs` (`assemble_finish_answer_prompt` appends the terminal context when the prompt references neither `{{run.context}}` nor any `{{nodes.` read)
- Modify: `crates/apb-engine/src/scheduler.rs` + `scheduler/cache.rs` (artifact capture decoupled from cache: runs for every succeeded node declaring `outputs.files`; zero-match globs journal a deliverable-missing warning)
- Test: suite extensions: `status_file_test.rs` or new `output_field_test.rs`; `finish_context_test.rs`; `cache_test.rs`

**Interfaces:**
- Consumes: Task 4's decoupled capture unit.
- Produces: YAML `condition: { output_field: { node: verify, field: verdict, equals: failed } }` routes on status-file outputs; a finish prompt without placeholders still summarizes real upstream output; a succeeded node with unmatched declared globs journals an observable warning.

- [ ] **Step 1:** Failing e2e: verify-node stub writes `{"status":"success","outputs":{"verdict":"failed"}}`; the `output_field` edge routes to the fix branch; with `"verdict":"ok"` it routes to done.
- [ ] **Step 2:** Failing e2e: finish node with `prompt: "compose the closing answer"` (no placeholders): the composer stub dumps its prompt; assert it contains an upstream node's output section.
- [ ] **Step 3:** Failing e2e: succeeded node declaring `outputs.files: ["report-*.md"]` that writes nothing journals the deliverable warning; when the file exists, `NodeFinished.artifacts` names it (no cache config involved).
- [ ] **Step 4:** Validator tests for the new condition variant (unknown node, happens-before).
- [ ] **Step 5:** Implement; watch pass; `cargo test -p apb-core -p apb-engine -p apb-mcp -p apb-server`; fmt + clippy; commit.

### Task 9: Targeted interrupt through the MCP surface (#75, spec 1.6)

**Files:**
- Modify: `crates/apb-engine/src/control.rs` (or wherever `Control::Interrupt` lives; add `node: Option<String>` `#[serde(default)]`), `crates/apb-engine/src/scheduler/node.rs` (`observe_control` filter at :122-151)
- Modify: `crates/apb-mcp/src/tools/supervisor.rs` (:141-154, optional `node` arg), `crates/apb-mcp/src/server/supervisor.rs` (tool description + args struct in `server/args.rs`)
- Test: `crates/apb-engine/tests/suite/control_liveness_test.rs` (extend), `crates/apb-mcp/tests/suite/supervisor_tools_test.rs` (extend)

**Interfaces:**
- Produces: `supervisor_interrupt_attempt(run_id, reason?, node?)`; absent node keeps documented broadcast semantics; present node interrupts only that node's attempt.

- [ ] **Step 1:** Failing engine test: two concurrent long-running batch branches; a node-targeted interrupt kills only the named one (the other completes normally).
- [ ] **Step 2:** Failing MCP test: the tool accepts and forwards `node`.
- [ ] **Step 3:** Implement; watch pass; `cargo test -p apb-engine -p apb-mcp`; fmt + clippy; commit.

### Task 10: Documentation, release notes, full gates

**Files:**
- Modify: `docs/HOWTO-authoring.md` (join semantics incl. implicit barriers and `join` validation; `require_verdict`; `output_field`; deliverable warnings; orchestration guidance: commit early and often in worktrees), `docs/PROFILES.md` (hermetic guidance for production profiles), `docs/TESTING-GUIDELINES.md` only if new helper patterns emerged
- Create: `docs/release-notes/v0.14.0.md` (one paragraph per feature line, one line per paragraph, no hard wraps; no em-dashes, no exclamation marks)
- Modify: `CLAUDE.md` + `AGENTS.md` only if a concept summary there became stale (mirror rule)

- [ ] **Step 1:** Write the docs and release notes.
- [ ] **Step 2:** Full gates in order, each green before the next: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo nextest run --workspace` (no SLOW tests); `cargo test --workspace --doc`; `cargo metadata --format-version 1 >/dev/null && code-ranker check .`; `cargo clippy --release`.
- [ ] **Step 3:** The pre-PR rg sweeps from TESTING-GUIDELINES (unbounded waits, shelled-out kill) over the branch diff.
- [ ] **Step 4:** Commit.
