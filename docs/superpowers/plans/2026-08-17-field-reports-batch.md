# Field Reports Batch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One combined patch closing every actionable item in issues #103, #85, #88, #89, #90, #91, #102.

**Architecture:** 14 sequential tasks over the existing crates; no new subsystems. The scheduler batch region lands in the order #91 fix, #89 shape, #90 tests. The `routes/runs.rs` items land as one pass. Everything else is independent.

**Tech Stack:** Rust workspace (edition 2024), axum, serde; YAML connector manifests.

**Spec:** `docs/superpowers/specs/2026-08-17-field-reports-batch-recon.md` - the recon report. Every task below cites its section by item number (e.g. "recon 102.6"). The recon carries the verified file:line anchors, the fix shapes, and the adopted product decisions (its "product decision" table's proposed defaults are all adopted as-is). Implementers MUST read their cited recon section before coding.

## Global Constraints

- Baseline: branch `fix/field-reports-batch` at `27084b7`.
- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings. No CJK anywhere.
- New `EventPayload` fields only with `#[serde(default)]`.
- State files written atomically via `apb_core::fsutil`.
- Never stage `.apb/profiles/developer/profile.yaml` or `.apb/profiles/facilitator/profile.yaml` (pre-existing local drift).
- Commits: `git commit --signoff`, Co-Authored-By trailer for the acting model.
- New validator codes start at V44 (V42/V43 are taken by ingest).
- Tests bound by construction (release files, marker files), never by widening a timeout (`docs/TESTING-GUIDELINES.md:90`). Engine e2e suites are one binary: take `common::env_lock()`.
- apb-server tests go through the real router built by `build_router` (auth middleware wraps everything); never hand-build a router in a test. Follow `crates/apb-server/tests/suite/runs_api_test.rs` patterns.
- Gates before each commit: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, targeted tests; full `cargo test --workspace` at least at Tasks 3, 8, 14. `code-ranker check .` before the final commit of the batch (warm cargo cache first with `cargo metadata --format-version 1 >/dev/null`).

---

### Task 1: #91 scan_control terminal-command starvation

**Files:**
- Modify: `crates/apb-engine/src/scheduler/control_apply.rs` (salvage arm ~:224-241)
- Test: `crates/apb-engine/tests/suite/stop_run_test.rs` (next to `a_stop_queued_behind_a_retry_still_aborts_the_run` at :1002)

**Requirements (recon #91):** Generalize the Abort-only salvage: when `blocked_by` is set by an unconsumable `Retry`/`ContinueFrom`, find the first terminal entry (`Abort` or `Pause`) anywhere in the pending tail, jump the cursor to its seq, journal the discarded Retry via the existing `retry_superseded_by_stop` shape (:210-218), and return `Terminal(...)`. Keep `run_cancel` as belt-and-braces for Abort but do not let it gate Pause. Out-of-order consumption is safe (only the drive thread advances the cursor).

- [ ] Write the failing test: Pause queued behind an unconsumable Retry pauses the run (twin of the :1002 abort test).
- [ ] Run it, verify it fails (run keeps executing today).
- [ ] Implement the generalized salvage arm.
- [ ] Test passes; the :1002 abort twin still passes; targeted stop_run_test suite green.
- [ ] Commit: `fix(engine): a terminal command queued behind an unconsumable Retry is always consumed`

### Task 2: #89 paired cancelled shape for all three write-off sites

**Files:**
- Modify: `crates/apb-engine/src/scheduler.rs` (join-any write-off :824-852; `stop_on_unhandled_failure` frontier cancel :133/140-146)
- Modify: `crates/apb-engine/src/scheduler/node.rs` (`advance_frontier` join-any sibling cancel :2601-2607)
- Test: `crates/apb-engine/tests/suite/parallel_concurrency_test.rs:381`, `crates/apb-engine/tests/suite/stop_run_test.rs:830`

**Requirements (recon #89, scope decision: all three sites):** Each site appends a `NodeStarted` immediately before its cancelled `NodeFinished` and sets `output: "cancelled"` (event and `batch_results` push where applicable). Rewrite the now-false justification comment at scheduler.rs:834-839. The four paired-start consumers are verified safe (recon lists them).

- [ ] Update `parallel_concurrency_test.rs:388-393`: replace the `!NodeStarted` assertion with the start-adjacent-to-cancelled-finish check modeled on `stop_run_test.rs:657-680`.
- [ ] Widen `stop_run_test.rs:830` to cover the join-any fixture.
- [ ] Implement the three paired shapes.
- [ ] Targeted suites green (`parallel_concurrency_test`, `parallel_e2e_test`, `parallel_cancel_test`, `stop_run_test`).
- [ ] Commit: `fix(engine): every cancellation write-off journals a paired start and a cancelled output`

### Task 3: #90 two missing regression tests

**Files:**
- Test: `crates/apb-engine/tests/suite/stop_run_test.rs`, `crates/apb-engine/tests/suite/progress_api_test.rs`

**Requirements (recon #90):** (1) Pause-during-final-chunk plus resume: copy `stop_run_test.rs:559`, swap `stop_run` for `post_supervisor_command(dir, &run_id, Control::Pause)` (as at :753), append the resume half from :782-828; assert deferred members run after resume and the run completes. (2) Live-batch progress drain: restructure `progress_api_test.rs:49` onto the `hold_script` harness (`stop_run_test.rs:489`), post `Control::Progress { node: Some("b") }` after `chunk1_started` appears and before the release file is written; assert the `RunProgress` event carries that member's `node_id`. Bound by construction. Run AFTER Tasks 1-2 so they pin final behavior.

- [ ] Write test 1, verify it passes against the Task 1+2 code (it pins, not fixes).
- [ ] Write test 2, same.
- [ ] Full `cargo test --workspace` green.
- [ ] Commit: `test(engine): pin pause-during-final-chunk resume and the live batch progress drain`

### Task 4: #102.4-B parked-on-wait runs are not Interrupted

**Files:**
- Modify: `crates/apb-engine/src/liveness.rs` (`reported_run_status` :636-643)
- Test: engine liveness/progress test suite

**Requirements (recon 102.4 cause B; decision: widen the repair, no new RunStatus variant):** A run whose only open work is a wait/signal park (derive via what `progress::from_run_dir` computes: `waiting_on`/`waiting_kind`) with a live driver pid reports `Running`, not `Interrupted`. `lost_nodes` behavior unchanged.

- [ ] Failing test: folded state Interrupted + open wait park + live driver reports Running.
- [ ] Implement; also a test that a dead driver still reports the dead-driver path (no regression).
- [ ] Commit: `fix(engine): a run parked on a wait with a live driver reports Running`

### Task 5: routes/runs.rs pass (#103.1, #103.2, #85.4/#102.4-A, #103.3 mitigation)

**Files:**
- Modify: `crates/apb-engine/src/review.rs` (:30-44), `crates/apb-engine/src/event.rs` (torn-tail tolerance), `crates/apb-server/src/routes/runs.rs`
- Test: `crates/apb-server/tests/suite/runs_api_test.rs`, engine review test

**Requirements (recon 103.1 with the 404/409 decision, 103.2, 85.4, 102.4-A, 103.3 scoped):**
1. `apb_engine::post_review` validates the node: not a `human_review` node of the run's playbook snapshot → `EngineError::NotFound`; exists but not currently pending → `EngineError::Conflict`. HTTP maps 404/409 like `run_playbook_handler` does; MCP `review_decide` inherits via existing ToolError conversion.
2. `list_runs_handler` takes `Query<WorkspaceQuery>`; `Some(workspace)` filters to that workspace (unknown id → 404 via `resolve_root`); no param keeps the aggregate.
3. Run detail: report `liveness::reported_run_status(&events)` instead of `run_state.run_status`, `reported_node_statuses(&events)` instead of `run_state.nodes`, and add `"driver_alive"` from `liveness::driver_alive`.
4. 103.3 mitigation: (a) regression test building a run detail whose node output and event payloads carry bytes 0x00-0x1f, asserting the body round-trips through `serde_json::from_slice`; (b) make the detail read tolerant of a torn trailing line of a concurrently-appended `events.jsonl` (lossy stop-at-first-unparsable-tail variant or retry) instead of 500.

- [ ] Engine: failing tests for post_review NotFound/Conflict; implement; green.
- [ ] Server: failing tests for review 404/409, `?workspace=` filter, detail liveness overlay + `driver_alive`, control-char round-trip, torn-tail tolerance; implement; green.
- [ ] Commit: `fix(server,engine): review node validation, workspace filter, live run detail (\#103, \#85.4, \#102.4)`

### Task 6: #102.1 connector permits for `type: playbook` children on HTTP and CLI

**Files:**
- Modify: `crates/apb-mcp/src/policy.rs`, `crates/apb-server/src/routes/playbooks.rs` (:246-265), `crates/apb-cli/src/run.rs` (:33-52, call sites :414/:581)
- Test: apb-server playbooks API test, apb-cli run test

**Requirements (recon 102.1):** Extract the existing recursive child-permit walk from `check_run` into a reusable `connector_permit_maps_with_children(root, playbook)` seam in `policy.rs` WITHOUT changing `check_run` behavior (`check_run` may call the new seam). Both the HTTP handler and the CLI call it and set `opts.expected_children`. NEVER reimplement the walk at call sites (anti-TOCTOU contract in CLAUDE.md: the gate returns the permit map in one pass). Tests: a parent delegating to a connector-binding child starts successfully via HTTP and via CLI (previously died fail-closed at prepare.rs:262-271).

- [ ] Failing HTTP test (parent-with-connector-child run start), failing CLI test.
- [ ] Extract the seam; wire both call sites; `check_run` behavior unchanged (existing policy tests stay green).
- [ ] Commit: `fix(policy): connector permits are computed for playbook children on HTTP and CLI start paths`

### Task 7: #102.5 WorkdirBusy maps to 429 with Retry-After

**Files:**
- Modify: `crates/apb-server/src/routes/playbooks.rs` (:266-281), any resume route with the same gap, `crates/apb-mcp/src/tools/mod.rs` (:48-57)
- Test: apb-server test

**Requirements (recon 102.5, decision: 429 now, no queue):** Arm before the catch-all: `WorkdirBusy` → 429 + `Retry-After: 5` (honest hint, not a guarantee). Grep `run_background|resume_run` across `crates/apb-server/src/routes/` and give the resume path the same arm. MCP: map `WorkdirBusy` → `ToolError::Conflict`.

- [ ] Failing test: second concurrent start answers 429 with the header.
- [ ] Implement all arms; green.
- [ ] Commit: `fix(server,mcp): workdir busy answers 429 with Retry-After instead of 500`

### Task 8: #102.6 connector max_calls floors at the current attempt

**Files:**
- Modify: `crates/apb-engine/src/connector/call/mod.rs` (`prior_call_count` :931-951, error text :374-377), possibly `crates/apb-engine/src/scheduler/node.rs` (AttemptStarted flush)
- Test: `crates/apb-engine/tests/suite/connector_call.rs`

**Requirements (recon 102.6, decision a2 with a1 fallback):** FIRST verify whether `AttemptStarted` is on disk before the `apb connector call` subprocess reads the log (execute_node buffers events in a returned Vec). If on disk (or a small flush in node.rs makes it so): floor the scan at the last `AttemptStarted { node }`, falling back to the last `NodeStarted` (Script nodes never emit AttemptStarted). If the flush is not cheaply achievable: ship the per-visit floor (last `NodeStarted`) plus the error-text fix distinguishing "budget spent by a previous, failed attempt" and record the choice in the report. Existing tests :721/:901 seed no NodeStarted; keep the `unwrap_or(0)` floor for them or add synthetic anchors. New test: budget resets across a fallback step.

- [ ] Verify the flush question; state the finding in the report.
- [ ] Failing test (fallback executor gets a fresh budget); implement chosen floor; error text updated.
- [ ] Full `cargo test --workspace` green.
- [ ] Commit: `fix(engine): connector max_calls is not consumed by a prior failed attempt`

### Task 9: #102.8 apb import honors a free bundle version

**Files:**
- Modify: `crates/apb-core/src/bundle.rs` (:87, doc :69-72), `crates/apb-core/src/versioning.rs` (add the override parameter path)
- Test: core bundle/versioning tests

**Requirements (recon 102.8, decision: honor when free, error when taken, auto-assign when absent):** `import_bundle` passes `bundle.version` through; a taken version errors with a message naming the conflict; an absent version keeps today's auto-assign. Update the doc comment.

- [ ] Failing tests: import honors 1.0.0 on a fresh id; second import of the same version errors; version-less bundle auto-assigns.
- [ ] Implement; green.
- [ ] Commit: `feat(core): apb import honors the bundle version when it is free`

### Task 10: #102.9 human_review prompt field

**Files:**
- Modify: `crates/apb-core/src/schema.rs` (`NodeKind::HumanReview` :830-832), `crates/apb-engine/src/progress.rs` (:96, review instruction rendering)
- Test: core schema test, engine progress test

**Requirements (recon 102.9, decision: add the field, no generic unknown-key validator in this batch):** `prompt: Option<String>` (with `#[serde(default)]`), rendered into the review instruction surfaced to the operator (progress.rs:96 and wherever the pending-review payload is built). Template placeholders inside `prompt` are NOT rendered (document as literal text) unless an existing render path makes it trivial; state the choice.

- [ ] Failing test: a playbook with `prompt:` parses and the pending-review surface carries it.
- [ ] Implement; green. MCP/HTTP pending_review payloads include it where they already include options.
- [ ] Commit: `feat(core,engine): human_review carries an optional prompt shown at the gate`

### Task 11: #102.10 top-level field selector in templates

**Files:**
- Modify: `crates/apb-core/src/validate/templates.rs` (:25-44 grammar, :108-113 V38 arm, :144-146 namespaces), `crates/apb-engine/src/context.rs` (:496-522), lift `output_field_value` from `crates/apb-engine/src/parallel.rs:73-83` into apb-core beside `EdgeCondition::OutputField`
- Test: `crates/apb-core/tests/suite/validate_semantics_test.rs:211` (exact-string assertion updates in lockstep), engine context test
- Docs: `docs/HOWTO-authoring.md:296-310` (+ :318-328 if V38 arm widened); do NOT touch historical release notes or specs

**Requirements (recon 102.10):** `{{nodes.x.output.field}}` and `{{nodes.x.report.field}}` with exactly `EdgeCondition::OutputField` semantics (one top-level field; absent/non-JSON/non-scalar → empty string, never an error). The two traps: widen V38's `check_cross_branch_reads` arm so 4-part tokens do not escape the racy-read warning, and update the exact-string namespace assertion.

- [ ] Failing validator test (4-part token accepted), failing render test (field projected), V38 4-part coverage test.
- [ ] Lift the helper, implement all five sites, docs updated.
- [ ] Commit: `feat(core,engine): top-level field selector for node output templates`

### Task 12: #88 V39/V40 on_failure exemptions and pins

**Files:**
- Modify: `crates/apb-core/src/validate/graph.rs` (`check_conditions` :168-248)
- Test: `widened_condition_checks_tests` module in `crates/apb-core/src/validate/mod.rs` (:1258-1390)

**Requirements (recon 88a/88b, decision: exempt from both V39 and V40):** (a) Pinning test asserting the unconditional-edge shape yields V34 error and no V39, plus a one-line comment at graph.rs:195 recording why. (b) Hoist the `defaults.on_failure` handler id (mirror templates.rs:102-105); skip V40's non-condition arm for the handler node; suppress V39's missing-failure case when the policy is not `FailurePolicy::Route`. Silence pins modeled on `v38_silent_for_the_on_failure_handler` (mod.rs:851-871).

- [ ] Pinning test 88a; silence tests 88b (fail first for the V39/V40 halves).
- [ ] Implement; green.
- [ ] Commit: `fix(core): V39 and V40 respect the defaults.on_failure route`

### Task 13: #102.3 atrip ancillary order identifiers

**Files:**
- Modify: `connectors/atrip/connector.yaml` (:797-848), `connectors/atrip/tests.yaml` (:245, :255)

**Requirements (recon 102.3, decision: add all three per the reporter's contract):** `post_booking_ancillary_search`: add `ticket_order_no` → `ticketOrderNo` to the body, `required: [ticket_order_no]`. `post_booking_ancillary_order`: add `session_id` → `sessionId`, `ticket_order_no` → `ticketOrderNo`, `passengers` → `passengers`, `required: [session_id, ticket_order_no, passengers]`. Follow the `seat_availability` house style (:444-482) including `description: "vendor field <camelCase>"`; drop the disclaimer descriptions. Note the `order_no`/`ticketOrderNo` naming coexistence in the change. Extend both tests.yaml cases to assert the new keys.

- [ ] Extend the two contract cases (fail first), update the YAML, `apb connector test --dir connectors/atrip` green, official gate green.
- [ ] Commit: `fix(connectors): atrip post-booking ancillary functions carry the order identifiers`

### Task 14: docs honesty pass (#102.2, #102.7) and goal surfaced in run_report

**Files:**
- Modify: `docs/HOWTO-authoring.md` (:309-310 hooks; :915-916 goal marker), `crates/apb-mcp/src/tools/run.rs` (run_report includes the goal block as unevaluated contract text)
- Test: MCP run_report test

**Requirements (recon 102.2 decision + 102.7):** (1) `run.hooks.*` documented as the relative signal URL `/api/hooks/<run-id>/<secret>`, with one disambiguating sentence pointing at the connector ingest inbox (the other "hooks"). (2) The goal `marker` bullet carries the same "not wired into run verdicts" caveat the `script` bullet already has. (3) `run_report` output includes the playbook's goal block verbatim, labeled as an unevaluated contract, so a supervisor can check it by hand.

- [ ] run_report test (fail first), implement, docs edited (MCP howto follows via include_str).
- [ ] Full workspace gates: fmt, clippy debug+release, `cargo test --workspace`, `code-ranker check .`.
- [ ] Commit: `docs,feat(mcp): honest hooks and goal docs; run_report carries the unevaluated goal`

---

## Out of scope (recorded for issue comments, no code)

- #85.1/.2/.3: already fixed in PR #87 (0.15.0); close with pointers. Hermes `--yolo` verification stays an open manual item.
- #103.3 root cause: needs the reporter's raw response bytes; Task 5 ships the invariant pin and torn-tail tolerance.
- #102.5 accept-and-queue: separate spec if wanted.
- Generic unknown-key validator (V44): separate item; #102.9 ships the field instead.
- #102.2 real goal evaluation: separate feature spec.
