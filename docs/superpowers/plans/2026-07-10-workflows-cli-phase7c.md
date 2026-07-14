# Phase 7c - parallel branches + join Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Parallel branches (several unconditional outgoing edges from a node executed as separate branches) and convergence (`join: all|any`), with cancellation of sibling branches on finish/any. Built on top of a frontier model introduced into drive without regressing linear runs (supervisor, patch, human_review, wait).

**Architecture (broken down into increments - each one correct and tested):**
- 7c-1: pure fork/join logic (the `parallel.rs` module) + moving drive to a frontier model in AUTONOMOUS mode, with fork/join/finish-cancel semantics. The linear path (a frontier of one node) executes exactly as before; the supervisor/patch path does not regress (for linear runs).
- 7c-2: real concurrency - slow nodes (agent_task/script) in different active branches execute at the same time, with a single event writer (drive) collecting the completions.
- 7c-3: cancelling agent processes on join:any/finish (needs kill support in the adapter).

This plan details 7c-1. 7c-2/7c-3 will be planned separately after 7c-1.

**Tech Stack:** Rust (wf-core schema/validate, wf-engine scheduler + the new parallel.rs). Web rendering of parallel branches in the monitor already works (the graph draws all nodes/edges); it needs no separate task.

## Global Constraints

- Chat language is Russian; code/tests follow the project conventions (comments/docstrings in Russian, errors/code in English).
- NO em-dashes (U+2014) and no exclamation marks in code/comments/docs/UI.
- The project is at 0.1.0.
- Single-writer: only drive writes events.jsonl (in 7c-1 it remains a single thread).
- Do not regress: linear runs, supervised mode, patch/migration, human_review, wait - all existing tests must pass with no change in semantics.
- `join` is taken from the field of the edges coming into a node (Edge.join); a node with more than 1 incoming edge is a join node. Default join = all.
- Gates: cargo build/test/clippy (0 new warnings), code-ranker.

## 7c-1 File Structure

- `crates/wf-core/src/schema.rs` - (if needed) a helper for accessing a node's join mode.
- `crates/wf-engine/src/parallel.rs` (create) - PURE logic: `successors(wf, from, &RunState) -> Vec<String>` (fork: all unconditional targets; conditional: chosen by the next_node logic), `join_mode(wf, node) -> Option<JoinMode>` (from the incoming edges), `join_ready(wf, node, &RunState) -> bool` (all: every source of the incoming edges is terminal; any: at least one is), `is_join(wf, node) -> bool`. No side effects, just the graph + the statuses.
- `crates/wf-engine/src/scheduler.rs` - drive: replace the single `current` with `frontier: Vec<String>`; on each tick, take an active node from the frontier, execute it (using the existing per-kind branches), on completion compute its successors via parallel::successors, add the ready ones to the frontier (a join node only when join_ready), and on finish - clear the frontier and end the run; join:any - cancel the remaining sources (mark them Cancelled, remove them from the frontier). Keep the supervisor/patch/human_review/wait behavior for the linear case.
- `crates/wf-core/src/validate.rs` - (if needed) a validator: join nodes with incompatible edge join modes, unreachable branches. Minimal in 7c-1.
- Tests: `crates/wf-engine/tests/parallel_test.rs` (unit tests for parallel.rs via its pub functions + E2E fork/join through run()).

## 7c-1 Tasks

### Task 1: pure parallel logic (parallel.rs)
- Introduce `JoinMode { All, Any }` (parsed from the string "all"/"any", default All).
- `successors(wf, from, state) -> Result<Vec<String>, EngineError>`: collect the outgoing edges of `from`. If any of them are unconditional (condition None, not fallback) - return ALL unconditional targets (fork). If there are conditional ones - apply the selection logic (as in next_node: the first non-fallback match, else fallback). A mix of unconditional and conditional edges from the same node - we treat it as: unconditional = fork (all of them), conditional edges are ignored during the fork (the validator will warn about this later). An empty list of outgoing edges - return empty (a dead end/finish is handled by the caller).
- `incoming(wf, node) -> Vec<&Edge>`; `is_join(wf, node)` = incoming.len() > 1.
- `join_mode(wf, node) -> JoinMode`: from the join field of the incoming edges (the first one that is set; default All).
- `join_ready(wf, node, state)`: All - every source of the incoming edges is in a terminal status (Succeeded for the all semantics; a failed source is a separate case, see spec 8.4: any branch failing = the join node fails -> in 7c-1, join:all marks failure when a source fails; minimally: ready when all sources are terminal, and the join's success/failure is decided by the statuses). Any - at least one source Succeeded.
- Tests (unit): fork returns both targets; join_ready all - false until all are done, true once all are; any - true on the first one; is_join/join_mode are correct.

### Task 2: drive frontier model (autonomous)
- Replace `current` with `frontier: Vec<String>` (invariant: a linear run = a frontier of one node -> identical behavior).
- Tick: pick a node from the frontier (for determinism - the first one; the order does not affect the correctness of fork/join). Execute it as now (Finish/human_review/wait/execute). On completion: compute the successors; for each target - if it is a join node, add it to the frontier ONLY if join_ready (otherwise the branch "waits" at the join, with the source simply marked terminal in the statuses); otherwise add it immediately. Deduplicate the frontier.
- Finish: clear the frontier, RunFinished, return (a finish in one branch ends the whole run).
- join:any: when adding the join node, mark the other unfinished sources as Cancelled (a NodeFinished event with status cancelled, or a separate event) and remove them from the frontier.
- Supervisor/patch: keep this working for the linear case. patch mutates the frontier (continue_from -> frontier=[continue_from]). Wake on failure - as now (for the specific node). Parallel + supervisor - conservatively: wake on the failed node, the rest of the frontier waits; a full model comes later.
- E2E tests: a diamond fork/join all (both branches execute, join after both, success); join any (one branch, finish); a fork with a finish in one branch ends the run; regression - all previous tests (linear, supervised, patch, review, wait) stay green.

### Task 3: validate + docs + gates
- Validator (minimum): a warning about mixing unconditional and conditional outgoing edges from the same node; a join node with conflicting edge join modes.
- CHANGELOG + tasks.md (mark parallel branches 7c as done, flag that concurrency/process cancellation are 7c-2/7c-3).
- Gates.

## Self-Review
- Single-writer is preserved (7c-1 is a single thread).
- Linear invariant: a frontier of one node reproduces the current behavior -> regression tests protect the supervisor/patch/review/wait paths.
- Deferred: 7c-2 (real concurrency), 7c-3 (killing processes). In 7c-1 the branches execute cooperatively (interleaved), and the fork/join semantics are correct.
- Spec 8.4 ultimately wants concurrent processes - we get there by first nailing correct semantics (7c-1) and then performance (7c-2), without risking the core in a single pass.
