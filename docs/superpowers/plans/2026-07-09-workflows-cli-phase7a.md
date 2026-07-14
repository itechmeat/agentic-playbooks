# Phase 7a - branching completeness + human_review Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring branching to completeness (`review_status`, `output_match` in `next_node`) and implement execution of the `human_review` node (pause until a human/agent decision via CLI, MCP, and web).

**Architecture:** The schema already parses `HumanReview{options}` and all `EdgeCondition` variants. Execution is what's missing. human_review = drive blocks in a poll loop on the decisions channel `reviews.jsonl` (mirroring the existing `await_control` for control.jsonl): on entering the node, drive writes a `ReviewRequested` event, waits for a decision record for that node, writes `ReviewDecided`, marks the node succeeded (output = the decision) and continues. Single-writer holds: only drive writes events; the decision-makers (`wf review`, the MCP `review_decide` tool, HTTP) write to `reviews.jsonl`. The decision is available to branching as `review_status` and in context as `{{nodes.<id>.review_note}}`.

**Tech Stack:** Rust (wf-core schema, wf-engine scheduler/state/event/review, wf-mcp, wf-server axum, wf-cli), Bun+Vite+Svelte 5 runes, vitest.

## Global Constraints

- Chat language is Russian; code/tests follow the project conventions: comments and docstrings in Russian, error text and code in English.
- NO em-dashes (U+2014) and no exclamation marks in code, comments, docs, or UI strings.
- The project stays at version 0.1.0.
- Sanitize any client-supplied id/run_id/node through `is_safe_segment`/`is_safe_id` before using it in a path.
- Single-writer invariant: only the drive stream writes `events.jsonl`. The decision-makers write the `reviews.jsonl` decisions channel; drive only reads it and, based on it, writes the `ReviewDecided` event.
- `output_match` in 7a is substring containment (`contains`), with no regex dependency (the spec allows a substring match; regex can come later if needed). Flag this in the code and in the CHANGELOG.
- A review decision is not a supervisor capability: `review_decide`/`wf review`/HTTP are ordinary run actions (like resume), not gated by a supervisor token.
- Before marking this ready: `cargo build/test --workspace`, `cargo clippy --workspace --all-targets` (zero new warnings), `cd web && bun run build` + `bunx vitest run`, `code-ranker check .`.

---

## File Structure

- `crates/wf-engine/src/event.rs` - events `ReviewRequested{node, options}`, `ReviewDecided{node, decision, note}`.
- `crates/wf-engine/src/state.rs` - `RunState.reviews: BTreeMap<String, ReviewDecision>` (decision+note); fold `ReviewDecided`.
- `crates/wf-engine/src/review.rs` (create) - the decisions channel: `ReviewCommand{node, decision, note}`, `post_review`, `read_reviews_after` (mirrors control.rs).
- `crates/wf-engine/src/scheduler.rs` - extend `next_node` to handle `ReviewStatus`/`OutputMatch` (takes `&RunState`); drive: a `HumanReview` branch (ReviewRequested + await the decision + ReviewDecided); export `post_review` through lib.
- `crates/wf-engine/src/context.rs` - `{{nodes.<id>.review_note}}` in the template renderer (if context already renders per-node fields; otherwise add it).
- `crates/wf-engine/src/lib.rs` - re-export `post_review`, `read_reviews_after`, `ReviewCommand`.
- `crates/wf-cli/src/main.rs` - the `wf review <run-id> <node-id> --decision X [--note Y]` subcommand.
- `crates/wf-mcp/src/{tools.rs,server.rs}` - the `review_decide{run_id, node, decision, note}` tool (an ordinary tool, not a supervisor one).
- `crates/wf-server/src/lib.rs` - `POST /api/runs/{id}/review` (body: node, decision, note).
- `web/src/lib/api.ts`, `web/src/lib/types.ts` - `postReview`, the decision type.
- `web/src/pages/RunView.svelte` - decision buttons on a node that is waiting for review.
- Tests: `crates/wf-engine/tests/review_test.rs` (engine), server/mcp tests, web tests are optional (the buttons are thin).
- Docs: `CHANGELOG.md`, `docs/tasks.md`.

---

### Task 1: Events + RunState.reviews

**Files:** Modify `crates/wf-engine/src/event.rs`, `crates/wf-engine/src/state.rs`. Test: `crates/wf-engine/tests/review_state_test.rs` (create).

**Interfaces:**
- Produces:
  ```rust
  // event.rs EventPayload:
  ReviewRequested { node: String, options: Vec<String> },
  ReviewDecided { node: String, decision: String, note: String },
  // state.rs:
  #[derive(Debug, Clone)]
  pub struct ReviewDecision { pub decision: String, pub note: String }
  // RunState { ..., pub reviews: BTreeMap<String, ReviewDecision> }
  ```

- [ ] **Step 1: Failing test** (`review_state_test.rs`): fold a manually built vector of events with `NodeFinished(human node succeeded)` + `ReviewDecided{node:"gate", decision:"approved", note:"ok"}` and check that `RunState::fold(...).reviews["gate"].decision == "approved"`.
- [ ] **Step 2:** `cargo test -p wf-engine --test review_state_test` - FAIL (the field/variant does not exist yet).
- [ ] **Step 3:** Add the variants to `EventPayload` (after `VersionPromoted`), the `ReviewDecision` type + `reviews` on `RunState` (Default-compatible via derive: `reviews: BTreeMap::new()`), and in `fold` handle `ReviewRequested` (a no-op for state) and `ReviewDecided` (record into `reviews`). Add both new branches to the existing fold `match` (otherwise it is non-exhaustive). Update any other `match EventPayload` (e.g. in context.rs/build_context) for the new variants.
- [ ] **Step 4:** test PASSES.
- [ ] **Step 5:** do NOT commit.

### Task 2: reviews channel (review.rs)

**Files:** Create `crates/wf-engine/src/review.rs`; modify `crates/wf-engine/src/lib.rs` (mod + re-export). Test: a unit test inside review.rs.

**Interfaces:** mirrors `control.rs`:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommand { pub node: String, pub decision: String, pub note: String }
pub fn post_review(run_dir: &Path, cmd: &ReviewCommand) -> Result<u64, EngineError>; // seq
pub struct ReviewEntry { pub seq: u64, pub cmd: ReviewCommand }
pub fn read_reviews_after(run_dir: &Path, after: Option<u64>) -> Result<Vec<ReviewEntry>, EngineError>;
```
File: `reviews.jsonl`, seq numbering as in control.rs (look at the control.rs implementation and replicate its seq + atomic-append structure one-to-one).

- [ ] Step 1: round-trip test (post two commands, read_reviews_after(None) returns two with increasing seq; read_reviews_after(Some(seq1)) returns only the second).
- [ ] Step 2: FAIL. Step 3: implement following the control.rs pattern. Step 4: PASS. Step 5: do not commit.

### Task 3: next_node - ReviewStatus + OutputMatch

**Files:** Modify `crates/wf-engine/src/scheduler.rs`. Test: `crates/wf-engine/tests/branching_test.rs` (create).

**Interfaces:** change the signature to `next_node(wf, from, state: &RunState)` (it currently takes `&BTreeMap<String, NodeStatus>`). Update both call sites in drive (replace `&statuses`/`&RunState::fold(...).nodes` with `&state`).
- `OutputMatch { node, pattern }` -> `state.outputs.get(node).is_some_and(|o| o.contains(pattern))`.
- `ReviewStatus { equals }` -> true if there is a review decision equal to `equals` among the `state.reviews` values. Important: does `review_status` refer to the decision of the edge's source node, or to any decision? Per spec 6.5: the human_review decision. The edge originates from the human_review node `from`, so compare against `state.reviews.get(from)`. Use `from`.

- [ ] Step 1: a test on a graph with a human_review node `gate` and two outgoing edges `review_status equals approved` -> A, `equals rejected` -> B; with `state.reviews["gate"]={approved}` `next_node` returns A; for output_match - a node with output "BUILD OK" and edges `output_match pattern "OK"` -> A, fallback -> B, returns A.
- [ ] Step 2: FAIL. Step 3: implementation + update the call sites. Step 4: PASS (+ run the existing scheduler tests to make sure the signature change breaks nothing). Step 5: do not commit.

### Task 4: drive - human_review execution

**Files:** Modify `crates/wf-engine/src/scheduler.rs`. Test: `crates/wf-engine/tests/review_test.rs` (create; a background thread + poll, as in migrate_test).

**Design:** In the main drive loop, when `node_kind` is `HumanReview{options}` (before the generic NodeStarted/execute_node):
1. if a decision already exists for `current` in `reviews.jsonl` (after review_cursor) - consume it: NodeStarted, ReviewDecided (node, decision, note), NodeFinished(current, succeeded, output=decision), advance `review_cursor`, `current = next_node(...)`, continue.
2. otherwise: if `ReviewRequested` has not been written yet for this visit - write `ReviewRequested{node, options}`; then blocking-poll (at the AWAIT_CONTROL_POLL interval) `read_reviews_after(run_dir, review_cursor)` until a decision for `current` shows up. Once it does - as in step 1. An Abort/Pause from control.jsonl during the wait must interrupt it (check the top-of-loop scan before sleeping, or inside the wait loop). It is enough to have the wait return control to the top of the loop with a short sleep, so the control scan (Abort/Pause/Patch) still runs. Do not write `ReviewRequested` again (idempotent: check that the last event for this node is already a `ReviewRequested` with no matching `ReviewDecided`).

Single writer: only drive writes ReviewRequested/ReviewDecided. The decision is placed into reviews.jsonl by the decision-makers.

- [ ] Step 1: test `human_review_pauses_then_decision_routes` - workflow start -> gate(human_review options [approved,rejected]) -> (approved)->done_ok / (rejected)->done_fail. Run drive in the background (autonomous). Poll until the ReviewRequested event. `post_review(run_dir, {gate, approved, "lgtm"})`. wait_result -> Succeeded. Check the ReviewRequested+ReviewDecided(approved) events, and that the finish is the success branch. A second test - a rejected decision leads to the failure finish.
- [ ] Step 2: FAIL. Step 3: implementation. Step 4: PASS. Step 5: do not commit.

### Task 5: CLI wf review

**Files:** Modify `crates/wf-cli/src/main.rs`. Test: covered e2e through the engine; CLI parsing - manual smoke test.

**Interfaces:** `wf review <run-id> <node-id> --decision <d> [--note <n>]` -> resolves the run_dir, calls `post_review`. Sanitize run-id/node via is_safe_segment (the engine's post_review already builds the path, but the CLI should reject unsafe input up front with a clear error).

- [ ] Step 1-4: add the subcommand to the parser (look at how resume/run are set up), call `wf_engine::post_review`. Check `cargo build`. Manual smoke test: start a run with human_review, and from a second terminal run `wf review`. Step 5: do not commit.

### Task 6: MCP review_decide

**Files:** Modify `crates/wf-mcp/src/{tools.rs,server.rs}`. Test: a tools test (post + check reviews.jsonl).

**Interfaces:** `tools::review_decide(root, run_id, node, decision, note) -> Result<Value, ToolError>` (sanitize, then post_review). Server: `#[tool] review_decide` with args `{run_id, node, decision, note}` - an ordinary tool (NOT through resolve_session; it takes run_id directly, like workflow_run/resume). Update the tool-counter test (it goes up by 1).

- [ ] Step 1-4: implementation + test. Step 5: do not commit.

### Task 7: HTTP + web buttons

**Files:** Modify `crates/wf-server/src/lib.rs`, `web/src/lib/{api.ts,types.ts}`, `web/src/pages/RunView.svelte`. Test: `crates/wf-server/tests/` (add to api_test.rs or runs_api_test.rs).

**Interfaces:** `POST /api/runs/{id}/review` body `{node, decision, note}` -> post_review -> `{posted_seq}`. web: `postReview(runId, node, decision, note)`. RunView: if a node is waiting for review (its last event is a ReviewRequested with no ReviewDecided, or the run is in a state with a pending review) - show buttons for the options, and on click call postReview + reload.

Determining pending-review on the frontend: in RunDetail events, look for the last `review_requested` with no subsequent `review_decided` for the same node; take options from that event. (Verify that ReviewRequested events are exposed in RunDetail.events with an options field; the payload serialization is already flat snake_case.)

- [ ] Step 1: server test: POST review returns 200 + posted_seq; an unknown run -> 404; traversal -> 404. Step 2: FAIL. Step 3: the route + handler (following get_run_report_handler + is_safe_id). web api/types/RunView. Step 4: server test PASSES, `bun run build` ok. Step 5: do not commit.

### Task 8: Docs + gates

- [ ] CHANGELOG: a line about branching completeness (review_status/output_match) and human_review (pause + decision via CLI/MCP/web).
- [ ] docs/tasks.md: mark the corresponding Phase 7 items (human_review, branching) as done (flag them as 7a).
- [ ] Gates: cargo build/test/clippy, web build+vitest, code-ranker, an em-dash scan of the new lines.

---

## Self-Review
- Branching: `next_node` covers None/NodeStatus (already there) + ReviewStatus + OutputMatch (new). The fallback edge remains the safety net.
- Single-writer: the ReviewRequested/ReviewDecided events are written ONLY by drive; reviews.jsonl is the decision-makers' channel (like control.jsonl for the supervisor).
- Resumability: ReviewDecided in the log means the decision survives a replay; on resume, drive will see the already-made decision and will not wait again (the idempotency check from Task 4).
- Open question for the operator (before committing): human_review is implemented as a blocking poll inside drive (mirroring the supervised await), rather than pause-return-resume. For background runs this holds the thread/process, but it is consistent with the existing supervised-await approach. Flag for confirmation.
- Deferred to 7b/7c: wait (timer/webhook), parallel branches + join. output_match is a substring match, not regex (7a).
