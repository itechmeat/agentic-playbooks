# Combined-patch recon: issues #103, #85, #88, #89, #90, #91, #102

Baseline: `27084b7` on branch `fix/field-reports-batch` - this is the squash of the
server-mode / webhook-ingest / WhatsApp stack (PR #104) landed directly on top of
`a0ebc33` (apb 0.19.0). Every file:line below is verified against that tree, not
against the anchors quoted in the issues.

Read-only recon. Nothing was modified, built, or tested.

---

## #103 - apb-server HTTP API field report

### 103.1 - `POST /api/runs/{id}/review` accepts a nonexistent node

**STATUS: still-present.**
`post_review_handler` - `crates/apb-server/src/routes/runs.rs:155-181`. It validates
`is_safe_id(&id)` (:161) and that the run dir exists (:169), then hands the body
straight to `apb_engine::post_review` (:177). Nothing looks at `body.node`.

The engine is where the hole actually is - `crates/apb-engine/src/review.rs:30-44`:

```rust
pub fn post_review(run_dir: &Path, cmd: ReviewCommand) -> Result<u64, EngineError> {
    std::fs::create_dir_all(run_dir)?;
    let seq = read_reviews_after(run_dir, None)?.len() as u64;
    ...
```

Two corrections to the issue text, both load-bearing for the fix:

1. **`posted_seq: 0` does not mean "not consumed".** It is just the index of the
   record in `reviews.jsonl` (`.len()` of what was there before). The first
   *correct* decision on a fresh run also returns 0. The reporter's proposed
   fallback ("document that `posted_seq: 0` means not consumed") would document
   something false. Their own repro shows it: the bogus post got 0 and the correct
   post afterwards got 1 - 1 only because the bogus record was already on disk.
2. **The same hole is in MCP.** `review_decide` - `crates/apb-mcp/src/tools/run.rs:413-436`
   - does the identical `is_safe_segment` + `run_dir.is_dir()` pair and then calls
   `post_review` with no node check. So this is not an HTTP-surface defect; it is an
   engine-contract defect with two callers.

**FIX SHAPE.** Put the check in `apb_engine::post_review` so both surfaces inherit
it: load the run's playbook snapshot (`progress::load_run_playbook`) plus the folded
`RunState`, and reject a node that is not a `human_review` node of that playbook with
a new `EngineError::NotFound`; optionally reject a node that is not *currently*
pending with `EngineError::Conflict`. `routes/runs.rs:177` then maps `NotFound` → 404
and `Conflict` → 409 the way `run_playbook_handler` already maps them
(`crates/apb-server/src/routes/playbooks.rs:270-278`); MCP's `ToolError` conversion
already carries `NotFound`.

**RISK/SIZE: S** (~30-40 lines engine + ~10 mapping + 2 tests). Decide "exists" vs
"pending" - see the product-decision list. Interaction: none with the auth layer;
`post_review_handler` is behind `auth_middleware` (`crates/apb-server/src/lib.rs:160-163`)
but that is orthogonal.

### 103.2 - `GET /api/runs` ignores `?workspace=`

**STATUS: still-present.**
`list_runs_handler` - `crates/apb-server/src/routes/runs.rs:9-25`. Its only extractor
is `State(state)`; there is no `Query<WorkspaceQuery>`. It loops
`enumerate_workspaces(&state)` unconditionally and stamps each row with
`workspace_id` / `project`.

The asymmetry the reporter describes is real: `get_run_handler` (:27-31) does take
`Query<WorkspaceQuery>` and resolves through `resolve_root`
(`crates/apb-server/src/state.rs`), so a foreign listing row 404s on detail unless the
caller re-sends that row's own `workspace_id`.

**FIX SHAPE.** Add `Query(q): Query<WorkspaceQuery>` to the handler and, when
`q.workspace` is `Some`, keep only the tuple whose `workspace_id` matches (or resolve
via `resolve_root` and compare roots, which also gives a 404 for an unknown id).
Aggregate stays the no-param default - the dashboard calls `fetchRuns()` with no
query (`web/src/lib/api/core.ts:142`), so nothing in the web client regresses.

**RISK/SIZE: S** (~10 lines + 1 test). No interaction with the new auth middleware.

### 103.3 - in-flight run detail emits invalid JSON (unescaped U+0000-U+001F)

**STATUS: not confirmed from source - needs a captured raw body before any code
change.** This is the one item I could not corroborate, and I think the reporter's
diagnosis ("a raw agent/event fragment embedded without JSON string escaping") is
wrong.

What I checked:

- The whole response is built by `serde_json::json!` and returned through
  `axum::response::Json` - `crates/apb-server/src/routes/runs.rs:126-144`. Every
  field (`events`, `outputs`, `model`, `progress`, `answer`, `layout`, `hooks`,
  `children`) goes through serde. serde_json escapes U+0000-U+001F in every string it
  writes, without exception.
- There is **no raw-JSON embedding site**: no `serde_json::value::RawValue` anywhere
  in the workspace, no custom `Serialize` impl, no `serialize_str`/`collect_str`, no
  `arbitrary_precision` feature (`serde_json = "1.0"`, default features, `Cargo.toml:24`).
- There is no compression / transcoding layer on the router
  (`crates/apb-server/src/lib.rs:31-165` - only `auth_middleware`), so nothing
  post-processes the body.
- The upstream `String::from_utf8_lossy` sites that *do* produce control bytes
  (`crates/apb-engine/src/adapter.rs:1229-1230`, `connector/call/response.rs:54-56`)
  all deposit them into ordinary Rust `String`s, which serde then escapes on the way
  out and again on the way back in through `event::read_all`
  (`crates/apb-engine/src/event.rs:687-703`, strict `serde_json::from_str` per line).

So the 200-body path cannot emit what was reported. Ranked hypotheses for what was
actually observed, all of which fit "in-flight only, clean once terminal":

1. **The in-flight response was not a 200 JSON body at all**, but one of the
   text/plain error arms - most plausibly `routes/runs.rs:43-46`, where
   `event::read_all` fails on a torn tail line of a concurrently-appended
   `events.jsonl` and the handler answers 500 with `e.to_string()`. This has exactly
   the reported timing profile (only while a writer is active; 0/73 terminal runs).
   The client's parser error message would differ from the one quoted, but the
   observable ("every poll during execution failed to parse") matches perfectly.
2. Transport framing captured by their proxy (chunk markers landing inside a string).
3. Something genuinely upstream that I cannot see from the source.

**FIX SHAPE.** Do *not* ship a blind "escape control chars" patch - there is nothing
to escape. Two cheap, correct actions instead: (a) add a regression test that builds a
run detail whose node output and event payloads carry `\x00`-`\x1f` and asserts the
serialized body round-trips through `serde_json::from_slice` (pins the invariant
forever, ~25 lines, `crates/apb-server/tests/suite/runs_api_test.rs`); (b) make the
detail endpoint tolerant of a torn tail line rather than 500-ing the whole read -
either a lossy variant of `event::read_all` that stops at the first unparsable trailing
line, or a retry. (b) is a real in-flight robustness bug regardless of whether it is
the reporter's bug. Then ask the reporter for the raw bytes (`curl -sS --output
body.bin` plus `xxd | grep` for the offending offset) before doing anything else.

**RISK/SIZE: S** for (a)+(b) as scoped above; **unknown** for the real defect until a
raw body exists. Do not let this item block the batch.

---

## #85 - opencode headless stall, non-resumable sessions, first-exec tax, driverless runs

Headline: **findings 1-4 were all fixed in PR #87 (shipped 0.15.0)**; the issue text
is stale by four releases. One genuine gap survives, in finding 4.

### 85.1 - empty `autonomous_args`

**STATUS: already-fixed** (PR #87 / commit 9ae3dfe). `crates/apb-engine/src/invocation.rs:32`
`builtin()`, arms at :55-157. Current state: `claude` `["--permission-mode","bypassPermissions"]`,
`agy` `["--dangerously-skip-permissions"]`, `codex`
`["--dangerously-bypass-approvals-and-sandbox"]`, `opencode` `["--auto"]`, `grok`
`["--permission-mode","bypassPermissions"]`, `cursor` `["--output-format","text","--force"]`,
`qoder` `["--permission-mode","bypass_permissions"]`.

Only `hermes` still carries `&[]`, and that is a deliberate, commented, test-locked
decision (invocation.rs:102-108; guard `builtin_hermes_carries_no_unverified_autonomous_flag`
at invocation.rs:624): hermes documents `--yolo` at the same level as the one-shot
`-z`, the combination could not be verified against a local binary, and shipping an
unverified flag into every hermes invocation was judged worse than the gap.

The suggested `doctor` warning also already exists -
`crates/apb-engine/src/run_doctor.rs:239-259` (`bare_links`), category `"autonomy"`,
walking the whole fallback chain, with a fixture built from live `invocation::builtin`
so it cannot drift. Prose duplicate at `docs/PROFILES.md:162-172`.

**FIX SHAPE:** none. The only open question is whether to verify `hermes --yolo -z`
against a live binary (web/manual verification, not a code task). **RISK/SIZE: S**
(close the finding; optionally one line if hermes is verified).

### 85.2 - opencode session-not-created doc row

**STATUS: already-fixed (docs).** `docs/INTERACTIVE-AGENTS.md:54` now spells out both
cases numbered and explicitly says case (2) is strictly harder than case (1) and would
survive an upstream fix of (1). Reinforced at :61. **FIX SHAPE:** none. **SIZE: S** (close).

### 85.3 - first-exec scan tax

**STATUS: already-fixed, and better than the suggestion.** `spawn_ms` exists as an
`Option<u64>` on `EventPayload::AttemptStarted` (`crates/apb-engine/src/event.rs:88`,
`#[serde(default)]`, doc at :80-87 naming the security scan). Measured around the spawn
in both adapter paths (`adapter.rs:1139/1149` headless, `:1342/1352` ACP), emitted at
`scheduler/node.rs:892-904` and `:1785-1795`.

More importantly the premise is void: **the attempt clock starts after the spawn
returns**, so the scan is not charged to `timeout_seconds` -
`adapter.rs:1157` and `:1402` rebind `started = Instant::now()` after
`spawn_in_group`/`on_spawn`, and `check_cancel_timeout` (adapter.rs:1041-1060) reads
that later clock.

No runtime binary warm-up exists and none is needed. The `warm_binary()` `OnceLock`
lives only in `crates/apb-cli/tests/init_interactive_test.rs:30` as a test-side pattern.

**FIX SHAPE:** none. **SIZE: S** (close).

### 85.4 - driverless-run discovery

**STATUS: partially-present - one real gap.** The primitive and every listing surface
are already wired: `liveness::dead_open_attempts` (`crates/apb-engine/src/liveness.rs:581`,
predicate :594), `pid_alive` :86, `pid_is_live` :233, `driver_alive` :321,
`reported_run_status` :636, `reported_node_statuses` :648. `RunSummary.driver_dead`
(`crates/apb-engine/src/scheduler/listing.rs:27`, populated :63-66 with a deliberate
`Some(false)` match so an unprobeable pid is never called dead) flows through MCP
`runs_list` (`crates/apb-mcp/src/tools/run.rs:107`), MCP `run_status` (:124-127, emitted
:172-188 including `"driver_alive"` at :177), CLI `apb runs`
(`crates/apb-cli/src/run.rs:688-691`), the HTTP listing (`routes/runs.rs:15`, pass-through
test `crates/apb-server/tests/suite/runs_api_test.rs:241-299`), and the dashboard
(`web/src/lib/status.ts:16` `showsDriverDead`, badge at `web/src/pages/RunList.svelte:102-104`).

**The gap:** the HTTP *single-run detail* endpoint has no liveness overlay at all.
`get_run_handler` folds with the pure `RunState::fold` (`routes/runs.rs:47`) and maps
nodes straight from `run_state.nodes` (:99-103). So the dashboard's run list shows
"needs resume" while the run detail view still reads a driverless run as `running`.

**FIX SHAPE.** In `routes/runs.rs`: add `"driver_alive"` to the JSON at :126 from
`apb_engine::liveness::driver_alive(&run_dir, &id)`, and swap `run_state.nodes` (:99)
for `apb_engine::liveness::reported_node_statuses(&events)`. No new struct anywhere.

**RISK/SIZE: S** (~15 lines + 1 test). **Interaction: this is the same edit as #102.4
- do them as one change.**

---

## #88 - V39/V40 over-approximation

File: `crates/apb-core/src/validate/graph.rs` (588 lines). `check_conditions` at
:168-248; the V39 coverage set at :186-191 with the fire site :195-210 (V39 arm
:203-208); V40 reachability loop :213-246 with the decision at :222-224 and the arm at
:235-242.

### 88a - "an unconditional edge should count as V39 coverage"

**STATUS: not-a-code-change (the described shape is already impossible).** This is the
finding that changes the plan.

V39's `covered` set does only collect `EdgeCondition::NodeStatus{equals}` and honors
`fallback: true` (graph.rs:186-195). But the shape the issue describes - a
`node_status: success` edge plus an unconditional catch-all - is already a **V34
error** (`check_duplicate_route_edges`, graph.rs:543-558: `has_unconditional &&
!shadowed.is_empty()`), V34 runs first (`validate/mod.rs:131`), and `check_conditions`
only runs inside `if r.is_valid()` (`validate/mod.rs:137-140`, `is_valid` = no
`Severity::Error`, mod.rs:85-87). So V39 can never fire on that playbook - the author
sees V34 instead. The only way to have an unconditional edge and still reach
`check_conditions` is when every conditional edge is `fallback: true`, and
`has_fallback` already suppresses V39.

**FIX SHAPE.** Write the pinning test the issue asks for, but assert the *actual*
behavior (V34 error, no V39), and add a one-line comment at graph.rs:195 recording why
the unconditional case needs no handling. Changing the coverage rule would be dead code.

**RISK/SIZE: S** (~20 lines of test + comment).

### 88b - `defaults.on_failure` handler exempt from V40's cannot-execute-before arm

**STATUS: still-present, and V39 has the same blind spot.**

- V40: `adjacency` (`crates/apb-core/src/graphutil.rs:22-31`) is built purely from
  `playbook.edges`, and `reachable_from` (:36-47) is a plain forward BFS over it -
  **no `defaults.on_failure` seed**. `check_reachability` explicitly compensates for
  exactly this (graph.rs:108-116, with the comment "The failure policy is a route like
  any other, it just has no edge drawn for it"); `check_conditions` does not, and never
  reads `playbook.defaults` at all. So a handler node reading the failed node in a
  condition is falsely told the source cannot execute before it.
- V39 shares it: a node whose failure is routed by `defaults.on_failure` still gets the
  missing-`failure`-branch warning.
- The precedent for the exemption is V38, already shipped and pinned:
  implementation `crates/apb-core/src/validate/templates.rs:97-109`, silence test
  `v38_silent_for_the_on_failure_handler` at `crates/apb-core/src/validate/mod.rs:851-871`
  (explicitly labelled a SILENCE rule).

**FIX SHAPE.** Hoist the handler id once at the top of `check_conditions`, mirroring
templates.rs:102-105:
`let failure_handler = match &playbook.defaults.on_failure { FailurePolicy::Node(t) => Some(t.as_str()), _ => None };`
Then skip the V40 non-condition arm (graph.rs:235) when `Some(n.id.as_str()) ==
failure_handler`, and suppress V39's missing-`failure` case when
`!matches!(playbook.defaults.on_failure, FailurePolicy::Route)` (`FailurePolicy` at
`crates/apb-core/src/schema.rs:410-425`; both `Stop` and `Node(_)` handle an unrouted
failure, `Route` does not).

Existing coverage all lives in the file-local `widened_condition_checks_tests` at
`crates/apb-core/src/validate/mod.rs:1258-1390` (helpers `pb_yaml` :1262, `codes`
:1267); there are **no** V39/V40 tests under `crates/apb-core/tests/`. New silence pins
go in that module.

**RISK/SIZE: S** (~15 lines of code, ~35 lines of test/comment).

---

## #89 - join:any write-off journals an unpaired cancelled NodeFinished

**STATUS: still-present.** Anchors drifted: the write-off is at
`crates/apb-engine/src/scheduler.rs:824-852` (inside the
`for chunk in batch.chunks(max_parallel)` admission loop in `drive`), and it journals
exactly one `NodeFinished { status: "cancelled", attempt: 1, output: String::new() }`
with no preceding `NodeStarted`. The comment naming the other unaligned cases is at
:834-839.

**The unified shape lives in `scheduler.rs:796-823`, not `node.rs`** - it is the
immediately preceding `run_cancel` branch, which appends a paired `NodeStarted` and sets
`output: "cancelled"`. `node.rs` carries only the `"cancelled"` output-text half of PR
#87 (`node.rs:773-780`, `:1271-1278`, `:1556-1565`) - those are in-flight kill paths that
return an `AttemptOutcome`, not journal writers.

**Two more siblings of the same bug** (both named in the :834-839 comment, both
`output: String::new()` and unpaired): `advance_frontier`'s join:any sibling cancel at
`crates/apb-engine/src/scheduler/node.rs:2601-2607`, and `stop_on_unhandled_failure`'s
frontier cancel at `crates/apb-engine/src/scheduler.rs:133/140-146`. Decide whether the
fix covers all three or only the one the issue names.

The "four paired-start consumers" claim (scheduler.rs:798-802) resolves to:
`cache::verify_connector_calls` (`scheduler/cache.rs:239`, window :248-254),
`journal::current_visit_start_seq` (`scheduler/journal.rs:272`),
`progress::node_durations_seconds` (`progress.rs:873`), and the interactive re-entry
guard `node_started_count`/`node_finished_count` (`scheduler/journal.rs:117/124`).
**Adding the paired start is safe for all four**: 1 and 2 take the *last* start, 3 records
a ~0 ms duration, 4 sees started == finished. The run-level path already exercises all of
them.

**FIX SHAPE.** In `scheduler.rs:824-852`: append `NodeStarted` before the finish, change
`output` to `"cancelled"` in both the event and the `batch_results` push, and rewrite the
now-false justification comment.

**One test blocks it** - `crates/apb-engine/tests/suite/parallel_concurrency_test.rs:381`
`a_queued_branch_is_cancelled_when_an_any_join_is_already_won`, whose first assertion
(:388-393) is literally `!events.iter().any(|e| matches!(NodeStarted { node } if node == "c"))`.
Replace it with the start-immediately-followed-by-its-own-cancelled-finish adjacency
check that the run_cancel side already uses for exactly this reason -
`crates/apb-engine/tests/suite/stop_run_test.rs:657-680`, which even carries the comment
explaining why `!NodeStarted` "is no longer the honest property". Also worth widening
`stop_run_test.rs:830` (`a_cancelled_member_journals_a_paired_start_and_a_cancelled_output`)
to cover the join-any fixture. Everything else stays green: `parallel_e2e_test.rs:657`,
`:645-654`, `parallel_cancel_test.rs:73` all assert on status only.

**RISK/SIZE: S** (~12-16 lines in `scheduler.rs`, ~20 lines of test; ~30-40 total across
two files; +~10 lines each if the two siblings are included). Interaction: **#91 closes
the degenerate window that makes duplicate pairs reachable and #90's first test drives
the same batch path - land #91, then #89, then #90's tests.**

---

## #90 - two missing regression tests

**STATUS: still-present (both).**

1. **Pause during the final chunk + resume.** The two existing pause-batch tests use
   `seed_batch` (multi-chunk, so the *admission gate* sees the pause):
   `stop_run_test.rs:740` and `:782`. The final/only-chunk shape - where no admission
   gate runs and only the batch tail's `stop_now` read saves the run - is pinned for
   **abort only**, via `seed_single_chunk` (`stop_run_test.rs:534-541`) and
   `a_stop_during_the_last_chunk_of_a_batch_still_aborts_the_run` (`:559`, `#[cfg(unix)]`).
   No Pause counterpart exists in any suite file. Note the tail already reads `halt`
   (`crates/apb-engine/src/scheduler.rs:1091-1095`), so the Pause path is *meant* to be
   caught there - it is simply unpinned.
2. **Live-batch progress drain e2e.** `progress_api_test.rs:49`
   (`a_named_progress_report_for_a_batch_member_is_attributed_to_that_member`) posts the
   `Control::Progress` **before the drive starts** - its own doc says so at :42-47. So
   the batch tail's `drain_progress_after_execute(..., None)`
   (`scheduler.rs:1066`) is never exercised with a report landing while members are in
   flight. `grep Progress` over `parallel_e2e_test.rs` / `parallel_concurrency_test.rs`
   returns zero hits. Unit coverage exists at `scheduler/supervisor.rs:535/577/612/651`.

**FIX SHAPE.** Test 1: copy `stop_run_test.rs:559`, swap `stop_run(...)` for
`post_supervisor_command(dir, &run_id, Control::Pause)` (as at :753) and append the
`resume(...)` half from `:782-828`; assert the deferred members run after resume and the
run completes. Test 2: restructure `progress_api_test.rs:49` onto the live `hold_script`
harness (`stop_run_test.rs:489`), posting `Control::Progress { node: Some("b") }` after
`chunk1_started` appears and before the release file is written, then assert the
`RunProgress` event carries that member's `node_id`.

Harness: `crates/apb-engine/tests/suite/common/mod.rs` (`env_lock()` :22 - mandatory,
the suite is one binary; `write_sync()` :31 for any exec'd script; `seed_profile()` :41).
Bound by construction with the release file, never by a sleep
(`docs/TESTING-GUIDELINES.md:90`).

**RISK/SIZE: M** (two live e2e tests, ~120-180 lines total). Interaction: test 1 shares
the batch-stop path with #89 and #91 - write it after those land so it pins the final
behavior, not the intermediate one.

---

## #91 - `scan_control` starvation behind an unconsumable Retry

**STATUS: still-present, and worse than the issue says.**

`scan_control` - `crates/apb-engine/src/scheduler/control_apply.rs:27-249`. It reads
the pending tail once in seq order (`read_control_after`, :65) and processes strictly in
file order (:66). The blocking arm, verbatim (:190-195):

```rust
Control::Retry { ref node, .. } | Control::ContinueFrom { ref node } => {
    // Valid only inside await_control, in response to a wake -
    // we do not advance the cursor, the command remains unconsumed.
    blocked_by = Some(node.clone());
    break;
}
```

`Retry`/`ContinueFrom` are unconsumable **by kind, not by a failed precondition**: they
are only valid in answer to a wake inside `await_control`
(`scheduler/supervisor.rs:119-158`). Because the cursor is a scalar
(`control.rs:181/199`) and the loop `break`s, everything at a later seq is invisible to
that scan.

There is already a partial salvage at :224-241 - but it handles **`Abort` only**, and
it is additionally gated on `run_cancel`, which is Abort-only by contract
(`crates/apb-engine/src/stop.rs:28-38`, `scheduler.rs:483-490`). So a **`Pause`** queued
behind an unconsumable Retry hits neither the loop nor the salvage: `blocked_by` is set,
`run_cancel` is false, the function falls through to `Ok(ControlScan::Proceed)` (:248),
and the run keeps executing nodes despite the operator's pause. `halt` is set on Pause
by the watcher (`stop.rs:363-367`) but `scan_control` does not take `halt` as a parameter
at all. That is the live defect; the duplicate cancelled pairs the issue describes are
the downstream symptom.

Terminal for the scan: `Abort` (:68-80), `Pause` (:81-88), and `Patch` when it yields
`PatchResult::Paused` (:133). `Control` enum at `crates/apb-engine/src/control.rs:11-85`.
Latches: `cancel_short_circuited` `scheduler.rs:506-510`, `batch_stop_short_circuited`
`:511-514`, used at `:1091-1095` and `:1964-1967`.

**FIX SHAPE.** Generalize the salvage arm (control_apply.rs:224) from "Abort +
`run_cancel`" to "the first terminal entry (`Abort` or `Pause`) anywhere in
`pending_control`, whenever `blocked_by` is set": jump the cursor to that entry's seq,
journal the discarded Retry the way :210-218 already does
(`retry_superseded_by_stop`), and return `Terminal(...)`. Keep `run_cancel` as a
belt-and-braces for the Abort path but do not let it gate Pause (or pass `halt` in
alongside it).

Out-of-order consumption is safe: only `scan_control` (control_apply.rs:65),
`await_control` (supervisor.rs:127) and `drain_progress_after_execute`
(supervisor.rs:185) advance the cursor and all three run on the drive thread; the
`StopWatcher` (stop.rs:350), the attempt-level interrupt poll
(`scheduler/node.rs:104,144`) and `run_doctor` (:377-378) are observe-only. The cost -
skipping entries between the Retry and the terminal command - is unchanged and already
accepted for Abort.

Pin goes next to `a_stop_queued_behind_a_retry_still_aborts_the_run`
(`crates/apb-engine/tests/suite/stop_run_test.rs:1002`), as its Pause twin.

**RISK/SIZE: S** (~10-15 lines + 1 test). Interaction: closes #89's degenerate
duplicate-pair window; land **before** #89.

---

## #102 - 0.19.0 field report (10 items)

### 102.1 - connector permits not computed for `type: playbook` children on HTTP and CLI

**STATUS: still-present (blocker).**

- HTTP: `run_playbook_handler` - `crates/apb-server/src/routes/playbooks.rs:246-265`.
  Walks only `loaded.playbook.nodes` for `connector_bindings()` (:248-252), calls
  `apb_mcp::policy::connector_permit_maps` (:254) for the top level only, and never sets
  `opts.expected_children`.
- CLI: `connector_permits_for` - `crates/apb-cli/src/run.rs:33-52`, same top-level-only
  walk; call sites `:414` and `:581`, both with `expected_children: None` (`:436`, `:603`).
- `apb_mcp::policy::connector_permit_maps` (`crates/apb-mcp/src/policy.rs:296-305`) is by
  construction top-level-only - its own doc (:290-295) says it exists for the dashboard
  start path and runs "the EXACT SAME resolution and trust checks `check_run` runs for
  its own connector step", which is true but only for the parent.
- The child machinery already exists, in `check_run`'s recursive gate:
  `crates/apb-mcp/src/policy.rs:~500-580` resolves each `type: playbook` child, re-runs
  lifecycle / digest-trust / `requires`, calls `check_connectors(root, &loaded.playbook,
  acknowledge_untrusted)` for the child (:554-556) and threads the child's own verified
  maps onto the pin - with a long comment (:537-553) explaining why the child's keys are
  deliberately NOT merged into the parent's permit. The pin rides through
  `expected_children` (`crates/apb-engine/src/run_config.rs:120`,
  `crates/apb-engine/src/scheduler/entry.rs:57`), is read at
  `crates/apb-engine/src/scheduler/node.rs:2116` and enforced fail-closed at
  `crates/apb-engine/src/scheduler/prepare.rs:262-271`.

**FIX SHAPE.** Extend the public seam rather than duplicating the walk: add a
`connector_permit_maps_with_children(root, playbook) -> (ConnectorPermitMaps,
BTreeMap<String, ChildExpectation>)` in `apb-mcp/src/policy.rs` that reuses the same
recursive child resolution `check_run` already runs, and call it from both
`routes/playbooks.rs:254` and `apb-cli/src/run.rs:50`, assigning the result to
`opts.expected_children`. **Never** reimplement the child walk at either call site -
policy.rs:293-295 says so explicitly and the anti-TOCTOU contract in CLAUDE.md depends on
the gate returning the permit map in one pass.

**RISK/SIZE: M** (~80-150 lines: mostly extracting the existing child loop into a
reusable function without changing `check_run`'s behavior, plus one HTTP test and one
CLI test). Highest-value item in the batch - it is what forced the reporter into a
leaf-only architecture.

### 102.2 - `goal.criteria` never evaluated at runtime

**STATUS: needs-product-decision** (the code claim is confirmed).
`GoalCheck` / `GoalCriterion` / `Goal` - `crates/apb-core/src/schema.rs:75-105`. Consumers:
validator V41 (`crates/apb-core/src/validate/nodes.rs:75-112`) and two display-only
reads (`crates/apb-mcp/src/catalog.rs:90`, `crates/apb-mcp/src/tools/playbook.rs:231-235`).
`grep goal` over `crates/apb-engine/src`, `crates/apb-cli/src`, `crates/apb-server/src`
returns zero hits - confirmed against the current tree.

The docs are half-honest already: `docs/HOWTO-authoring.md:918-920` says of `script`
"Script execution is not wired into run verdicts yet; the field records the contract",
and the schema doc comment at `schema.rs:76-77` says the same. But `:915-916` describes
`marker` as "expected in the run result" with no such caveat, which is what misled the
reporter. See the product-decision list.

### 102.3 - atrip post-booking ancillary functions drop the order identifiers

**STATUS: still-present.** `connectors/atrip/connector.yaml`:
`post_booking_ancillary_search` at **:797-823** (body **:813**, one key
`ancillaryCategory: "{{args.ancillary_category}}"`) and `post_booking_ancillary_order`
at **:825-848** (body **:838**, identical). Both `args_schema`s carry
`additionalProperties: true` **and** a `description` (:819, :844) that explicitly admits
the order-identifying field is not stated - so the extra args are accepted and then
dropped, because a body template enumerates keys explicitly and there is no spread
mechanism. Neither function has a `required:` list.

House style to copy - `seat_availability` at **:444-482**, body :459-463: snake_case arg
→ camelCase vendor key, `description: "vendor field <camelCase>"`, explicit `required:`.

Naming precedents in the same file: `session_id` → `sessionId` (already at :116, 145,
269, 307, 462, 469, 698); `passengers` → `passengers` verbatim as an array of objects
(:267, 288, 323, ...); `order_no` → `orderNo` (:357/364, 392/400, 429/434, 547/552).
`ticketOrderNo` **appears nowhere in the repo** - so `ticket_order_no` → `ticketOrderNo`
is the consistent choice, but it sits awkwardly next to the existing `order_no`/`orderNo`
pair and should be called out in the change.

Contract tests: `connectors/atrip/tests.yaml:245` and `:255` assert only
`body_contains: { ancillaryCategory: BAGGAGE }`, and `body_contains` is a **subset**
match (`crates/apb-engine/src/connector/contract_test.rs:183-191`), so adding body keys
will not break them - but the cases should be extended. The CI gate
(`crates/apb-cli/tests/suite/official_connectors_gate.rs:164-181`) only requires one case
per function, which already exists.

**FIX SHAPE.** Add the three fields to both body templates and `args_schema`s following
the `seat_availability` shape, set `required:` appropriately (search: `ticket_order_no`;
order: `session_id` + `ticket_order_no` + `passengers`), and extend the two tests.yaml
cases. **RISK/SIZE: S** (~40 lines of YAML). **Needs the vendor contract confirmed** -
see the product-decision list.

### 102.4 - run status flickers `interrupted` for healthy runs

**STATUS: still-present. Two distinct causes, both found.**

Cause A (the detail-vs-list disagreement the reporter saw first): the HTTP run-detail
endpoint has **no liveness overlay**, while the listing does. `get_run_handler` folds
with pure `RunState::fold` (`crates/apb-server/src/routes/runs.rs:47`) and reports
`run_state.run_status` verbatim (:130) and `run_state.nodes` (:99-103). The listing goes
through `RunSummary` which carries `driver_dead`
(`crates/apb-engine/src/scheduler/listing.rs:27`). **This is the same edit as #85.4.**

Cause B (parked on a webhook wait, both endpoints): `RunState::fold` sets
`RunStatus::Interrupted` whenever any attempt is still open at the end of the journal -
`crates/apb-engine/src/state.rs:301-320`. The repair for that lives in
`liveness::reported_run_status` (`crates/apb-engine/src/liveness.rs:636-643`):

```rust
let pure = RunState::fold(events).run_status;
if matches!(pure, RunStatus::Interrupted) && !live_open_nodes(events).is_empty() {
    return RunStatus::Running;
}
```

`live_open_nodes` requires a **probeable, live pid** on the open attempt. A run parked on
a `wait_for: webhook` node has no live agent process to point at, so `live_open_nodes` is
empty, the repair does not fire, and the pure `Interrupted` stands even on the surfaces
that *do* use the overlay. Symmetrically `lost_nodes` does not claim it either (it needs
a probeable pid that is dead), so the run is neither "running" nor "lost" - it falls
through to the worst label.

**FIX SHAPE.** (A) In `routes/runs.rs`: report
`apb_engine::liveness::reported_run_status(&events)` instead of
`run_state.run_status` (:130), `reported_node_statuses(&events)` instead of
`run_state.nodes` (:99), and add `"driver_alive"`. (B) Extend `reported_run_status` so a
run whose only open work is a wait/signal park is not reported `Interrupted` - the
cleanest predicate reuses what `progress::from_run_dir` already computes
(`ProgressSummary.waiting_on` / `waiting_kind`, `crates/apb-engine/src/progress.rs:256+`):
if the run is waiting on a node and the driver pid is live, report `Running` (or a new
`Waiting`). A new `RunStatus::Waiting` variant is the reporter's preference but is a
wire-format change across MCP, CLI, HTTP and the web badge - see the product-decision list.

**RISK/SIZE: S** for (A); **M** for (B) if a new status variant is chosen, **S** if the
existing `Running` repair is just widened. Interaction: do (A) together with #85.4.

### 102.5 - concurrent run start returns 500 `workdir busy`

**STATUS: still-present, and it is a one-line-arm fix.**
`EngineError::WorkdirBusy` already exists (`crates/apb-engine/src/error.rs:15-16`),
raised at `crates/apb-engine/src/workdir.rs:78-80` (and re-raised by the handover retry
loop at :113-116). It is produced synchronously - `scheduler/prepare.rs:436-441` calls
`acquire` before `run_background` spawns the drive thread
(`scheduler/entry.rs:263-278`) - so it reaches the handler's `match` intact.

The mapping at `crates/apb-server/src/routes/playbooks.rs:266-281` has arms for
`NotFound` → 404, `Conflict` → 409 and `Invalid` → 422, and **no arm for `WorkdirBusy`**,
so it falls into `Err(e) => INTERNAL_SERVER_ERROR` at :279. No new error variant is
needed anywhere.

CLI and MCP are equally undifferentiated: `crates/apb-cli/src/run.rs:442-454` prints
`run failed: {e}` and exits 2 (the retry hint is baked into the Display string
precisely because nothing else carries it), and `crates/apb-mcp/src/tools/mod.rs:48-57`
drops `WorkdirBusy` into the generic `ToolError::Engine`.

**FIX SHAPE (minimal).** Insert one arm before the catch-all at playbooks.rs:278:
`Err(EngineError::WorkdirBusy(what)) => (StatusCode::TOO_MANY_REQUESTS, [("retry-after","5")], what).into_response()`.
Grep `run_background|resume_run` across `crates/apb-server/src/routes/` first - the
resume path also calls `acquire` (`scheduler/resume.rs:469`) and likely needs the same
arm. Optionally map `WorkdirBusy` → `ToolError::Conflict` in the MCP conversion so an
agent gets a retry affordance too. Caveat to be honest about: the 5 s `HANDOVER_WAIT`
(`workdir.rs:13`) applies only to `acquire_handover`, not `acquire`, so `Retry-After: 5`
is an honest hint, not a guarantee.

**RISK/SIZE: S** (~10-25 lines, 1-2 files, plus one 429 test). Accept-and-queue (shape b)
is genuinely large - it needs a persisted pending-run queue, a run identity that exists
before `prepare` mints the run_id (`prepare.rs:443`), a lock-released signal (today the
guard releases via `Drop`, `workdir.rs:44-50`), and a "queued" state on every surface. Do
not ride it on this batch; (a) does not foreclose it.

The reporter's real pain - "an inbound client message cannot start the intake run that
would forward the reply to the run that is waiting for it" - is **now partly solved by
the ingest stack that just landed**: a webhook delivery goes to the machine-scoped inbox
(`crates/apb-core/src/connector/inbox.rs`, ingest router
`crates/apb-server/src/ingest.rs:470/633`) whether or not a run is executing, so the
front door is no longer blocked by the workdir lock for connector deliveries. Worth
saying so when the issue is answered; it does not remove the 500-vs-429 defect.

### 102.6 - connector `max_calls` consumed by failed executor attempts

**STATUS: still-present, mechanism fully confirmed.**
Schema field `max_calls: Option<u32>` on `ConnectorBinding` -
`crates/apb-core/src/schema.rs:597` (doc :584-591 calls it "an optional per-run call
budget"); snapshotted into the manifest at `crates/apb-engine/src/manifest.rs:100`.

**There is no in-memory counter.** The count is derived from the event log on every call
- `prior_call_count`, `crates/apb-engine/src/connector/call/mod.rs:931-951`, filtering
`EventPayload::ConnectorCall` by `(node_id, connector)` **with no seq floor, no attempt
field, no `NodeStarted` anchor**. Enforced at `call/mod.rs:366-380`.

The contrast that proves the omission is in the same crate: `scheduler/cache.rs:248-254`
floors its scan at `rposition(NodeStarted for this node)` precisely so "a resume never
re-judges a prior execution's calls" (comment at cache.rs:236-237). `prior_call_count`
makes no such claim and counts calls "of any outcome".

Neither loop in `scheduler/node.rs` touches any budget: the fallback chain
(`node.rs:683`) and the retry budget inside each step (`node.rs:766`, where `infra_used`
means the real attempt count can exceed `retries+1`). So a `max_calls: 2` grant whose
first executor attempt burns both calls and then dies leaves every retry and every
fallback step guaranteed to fail - reported as `CallErrorCode::Permission`, which is
actively misleading.

**Good news for the fix:** the state is the journal, not a struct, so a reset is a change
to one filter predicate, not cross-process plumbing. Rejections are deliberately never
journaled (`event.rs:343-345`, pinned at `tests/suite/connector_call.rs:764-766`), so the
count is already "real calls only".

**FIX SHAPE.**
- **(a1) per-visit floor** - copy the cache.rs pattern: floor at the last `NodeStarted`
  for the node. One function, `call/mod.rs:931-951`, ~12-15 lines. Fixes the loop/resume
  case cleanly but **not** retries within one visit, which is the actual complaint.
- **(a2) per-attempt floor** - floor at the last `AttemptStarted { node }`, falling back
  to `NodeStarted`. Same function, ~18-22 lines. The fallback is load-bearing:
  `NodeKind::Script` never emits `AttemptStarted` (noted at `stop_run_test.rs:659-662`).
  **Verify before committing to this:** `execute_node` accumulates events in a returned
  `Vec<EventPayload>` that the drive appends, so an `AttemptStarted` may not be on disk
  when the `apb connector call` subprocess reads the log. If it is buffered, (a2)
  silently degrades to (a1) and the drive must flush `AttemptStarted` before spawning the
  executor - a small but real change in `node.rs`.
- Existing tests `connector_call.rs:721` and `:901` seed a run dir with no `NodeStarted`,
  so the `unwrap_or(0)` floor keeps them green; each needs a synthetic anchor if the
  fallback is tightened, plus one new test proving the budget resets across a fallback
  (~40-60 test lines).

**RISK/SIZE: M** for (a2) (~60-80 lines, two files) contingent on the flush question; **S**
for (a1) or for documenting only. If documenting only, at minimum change the error text at
`call/mod.rs:374-377` to distinguish "budget spent by a previous, failed attempt" from
"budget spent by this attempt" - a `Permission` error for someone else's dead attempt is
the worst part of the current behavior.

### 102.7 - stale doc `{{run.hooks.*}}`

**STATUS: still-present (docs only), and the doc is flatly wrong.**
`docs/HOWTO-authoring.md:309-310` says `run.hooks.*` is "the payload last posted to a
`wait` node's webhook, by key". Actual behavior: the map handed to `render` is built at
`crates/apb-engine/src/scheduler/node.rs:25-27` from `hooks::hook_path`
(`crates/apb-engine/src/hooks.rs:53-55` → `format!("/api/hooks/{run_id}/{secret}")`) and
resolved at `crates/apb-engine/src/context.rs:511`
(`["run","hooks",key] => hooks.get(*key)...`). So `{{run.hooks.<key>}}` renders the
**relative signal URL**, never a payload. `post_hook_handler`
(`crates/apb-server/src/routes/runs.rs:225-229`) has no body extractor at all and calls
`post_signal` with the key only (:262). The original design doc
(`docs/superpowers/plans/2026-07-10-workflows-cli-phase7b.md:30`) and `CHANGELOG.md:26`
both state the correct behavior - only the HOWTO drifted. Test evidence:
`crates/apb-engine/tests/suite/context_test.rs:64`.

**Interaction with the just-landed stack - this got worse, not better.** There are now
two `post_hook_handler`s in two separate routers: the run-signal one above
(`/api/hooks/{run_id}/{secret}`, mounted `crates/apb-server/src/lib.rs:153`) and the
connector-ingest one (`crates/apb-server/src/ingest.rs:633`, mounted `:470` at
`/hooks/{connector}/{account}`), and the ingest one **does** take `body: Bytes` (:638)
and store it - into a machine-scoped inbox, deliberately not bound to a run
(`crates/apb-core/src/connector/inbox.rs:1-8`). So the stale sentence now reads as if
`run.hooks.*` were the way to read an ingested delivery body.

**FIX SHAPE.** Correct :309-310 to "the relative signal URL `/api/hooks/<run-id>/<secret>`
for the wait node's hook key", and add one disambiguating sentence pointing at the
connector inbox (already documented at `docs/HOWTO-authoring.md:901-904` and
`docs/CONNECTORS.md:217-219`). **RISK/SIZE: S** (~5 lines). Free bonus: the MCP
`playbook_howto` text is `include_str!` of this file
(`crates/apb-mcp/src/tools/playbook.rs:84-86`), so agents get the fix automatically.

### 102.8 - `apb import` ignores the bundle version

**STATUS: still-present, but it is a deliberate design note, not an oversight.**
`import_cmd` - `crates/apb-cli/src/manage.rs:266` (dispatch `main.rs:330`) →
`apb_core::bundle::import_bundle`. Version assignment at
`crates/apb-core/src/bundle.rs:87`: `create_version(root, &bundle.id, &bundle.playbook,
None, make_current)?` - `bundle.version` is never read, and the `None` is
`base_version`, not an override (`crates/apb-core/src/versioning.rs:118-124`), so there
is no parameter through which the bundle version *could* be honored. The doc comment at
`bundle.rs:69-72` says the omission is intentional ("does not force the version from the
bundle, to avoid collisions").

`PlaybookBundle` - `bundle.rs:27-37`, with two version fields: `apb_bundle` (format
version, `BUNDLE_SCHEMA = 1` :39, checked :79-84) and `version` (playbook version,
ignored). `from_json` at :46-48 is plain `serde_json::from_str`, no
`deny_unknown_fields`. `export` **does** write it (`bundle.rs:63`). There is no server
or MCP import route, so `version` is write-only across the entire codebase - the only
asymmetric field in the struct.

**STATUS refinement: needs-product-decision** (honor / reject / document). See the list.
**RISK/SIZE: S** either way.

### 102.9 - `human_review` silently drops a `prompt:` key

**STATUS: still-present, and it is universal rather than `human_review`-specific.**
`NodeKind::HumanReview { options: Vec<String> }` - `crates/apb-core/src/schema.rs:830-832`.
`NodeKind` is `#[serde(tag = "type")]` (:761-763) and flattened into `Node`
(`#[serde(flatten)]`, :712-714). **serde's `flatten` cannot be combined with
`deny_unknown_fields` at all** - flatten buffers into a map and forwards the leftovers -
so unknown keys are silently dropped on **every** node kind by construction, not just
this one. `Node` (:688) carries no `deny_unknown_fields`; the only two in `schema.rs` are
:521 and :604, neither on `Node`/`NodeKind`.

No unknown-key validator exists (grep "unknown" over `crates/apb-core/src/validate/`
returns only reference-resolution messages, graph.rs:65/85).

The mechanism a warning needs already has a precedent: `Playbook::from_yaml`
(`schema.rs:163-174`) already does a **second parse to `serde_yaml_ng::Value`** for
exactly this class of check (`has_legacy_executors`, :335-351). But `from_yaml` returns
`SchemaError` (a hard error, not a warning), and `validate(playbook, ctx)`
(`crates/apb-core/src/validate/mod.rs:123`) takes no raw text - so a *warning* requires
either carrying the raw YAML on `Playbook`, or adding it to `ValidationContext`, or doing
the check at the three call sites where both already coexist
(`crates/apb-core/src/versioning.rs:161/165`, `:250/253`, `:725/737`).

Highest existing validator code is **V43** (`crates/apb-core/src/validate/connectors.rs:182`);
**V44+ is free** (codes are string literals, no central enum).

**FIX SHAPE / decision.** Two options, see the product-decision list. The cheapest honest
fix is the narrow one: a `V44` warning for unknown keys on nodes, computed at the
versioning call sites where the raw YAML is in hand, scoped to node maps only.
**RISK/SIZE: M** (~80-120 lines including the raw-YAML plumbing and tests) for the
warning; **S** if instead a `prompt`/`body` field is simply added to `HumanReview`.

### 102.10 - no field selector in the template grammar

**STATUS: still-present. Much cheaper than the issue assumes - the semantics already
exist and are tested elsewhere.**

- V13 grammar - `crates/apb-core/src/validate/templates.rs:25-44`: a hand-rolled
  `split('.')` match, not a regex. `{{nodes.x.output.field}}` is 4 parts, falls to
  `_ => false`, hard V13 error. Token extraction `template_refs` at :148+.
- Render side - `crates/apb-engine/src/context.rs:496-522`, the same `split('.')` match;
  outputs are `BTreeMap<String,String>`, so node output is an opaque string at render
  time; the unknown arm returns `String::new()` (which is why V13 exists as the guard).
- The precedent - `EdgeCondition::OutputField` already parses output JSON and projects
  one top-level field, as a total function with settled semantics:
  `crates/apb-engine/src/parallel.rs:73-83` (`output_field_value`), consumed at :105-112
  in `edge_matches` (:87); schema variant `crates/apb-core/src/schema.rs:956-972`, whose
  doc comment already states the exact rule ("ONE top-level field ... every shape the
  condition cannot read is a NON-match, never an error").

**FIX SHAPE.** Reuse `output_field_value` verbatim. Five code sites:
1. `templates.rs:25-44` - one new 4-part arm `["nodes", nid, "output"|"report", _field]`.
2. `templates.rs:144-146` - extend `V13_KNOWN_NAMESPACES`. **This breaks the exact-string
   assertion at `crates/apb-core/tests/suite/validate_semantics_test.rs:211`** - update in
   lockstep.
3. `templates.rs:108-113` - V38's `check_cross_branch_reads` matches
   `["nodes", source, "output"|"report"]` and `continue`s otherwise, so a 4-part token
   would silently escape the racy-read warning unless this arm is widened too. Easy to miss.
4. `crates/apb-engine/src/context.rs:496-522` - the resolving arm, calling
   `output_field_value(...).unwrap_or_default()`. `output_field_value` is a private `fn`
   in `apb-engine::parallel`; cleanest is to lift it into `apb-core` next to
   `EdgeCondition::OutputField` so the validator and both consumers share one definition.
5. `docs/HOWTO-authoring.md:296-310` (namespace list; cross-reference the `output_field`
   edge condition at :361-375) and :318-328 if site 3 is widened. The MCP howto needs no
   code change (`include_str!`, `crates/apb-mcp/src/tools/playbook.rs:84-86`).

Do **not** touch the verbatim quotes of the namespace list in
`docs/release-notes/v0.7.0.md:7` or the design specs - those are historical records.

**RISK/SIZE: S-M** (~4 small edits + the lifted helper + docs + tests; the issue's "M/L"
guess is pessimistic). The two traps are site 3 and the exact-string test at site 2.

---

## Cross-cutting: interactions with the just-landed server-mode / ingest / WhatsApp stack

1. **Auth middleware now wraps every route touched by #103.** `build_router`
   (`crates/apb-server/src/lib.rs:160-163`) applies `auth::auth_middleware` over the whole
   router including the static fallback, deliberately so `ClientCtx` is present on every
   request and exemptions are decided inside the middleware. Any new test for #103.1/.2 or
   #102.4-A must go through the auth path the existing suites use
   (`crates/apb-server/tests/suite/runs_api_test.rs`, `auth_endpoints_test.rs`), not a bare
   router. No behavioral conflict - just do not build a router by hand in a new test.
2. **Two different things are now called "hooks".** `/api/hooks/{run_id}/{secret}` (run
   signal, body discarded, `routes/runs.rs:225`) and `/hooks/{connector}/{account}`
   (connector ingest, body stored, `ingest.rs:633`, separate router on a separate socket,
   `ingest.rs:470`). #102.7 sits exactly on that ambiguity and the doc fix must name both.
3. **#102.5's worst symptom is partly obsoleted by ingest.** Inbound provider deliveries no
   longer need a run to be startable at that instant - they land in the machine-scoped
   inbox (`crates/apb-core/src/connector/inbox.rs:1-8`). The 500-vs-429 defect stands; the
   "front door blocked by the run that is waiting" scenario is now avoidable by design.
4. **New validator codes must start at V44** - the ingest work took V42 and V43
   (`crates/apb-core/src/validate/connectors.rs:119/149/182`). Relevant to #102.9.
5. **`routes/runs.rs` is touched by three separate items** - #103.1, #103.2, #85.4/#102.4-A.
   Sequence them in one pass over that file to avoid conflicts.
6. **`scheduler.rs`'s batch region is touched by three items** - #91 (fix), #89 (shape), #90
   (tests). Land in that order.

---

## Items that genuinely need the owner's product decision

| # | Decision | Proposed default |
|---|---|---|
| 103.1 | Reject a review for a node that merely *exists* as a `human_review` node, or only for the *currently pending* gate? | **Both, distinctly:** 404 when the node is not a `human_review` node of the run's playbook; 409 when it exists but is not currently pending. A pre-posted decision for a gate not yet reached is a plausible automation pattern and should not be silently accepted, but it also should not look like a typo. |
| 88b | Should the `defaults.on_failure` handler be exempt from V39 as well as V40, or only V40? | **Both.** V38 already set the precedent that the policy route is a real route with no drawn edge; applying it to only one of the two leaves the same false warning class alive. |
| 89 | Extend the paired cancelled shape to the join-any race loser, or declare a race loser "not a cancellation" and document two shapes? | **Extend the paired shape.** One shape for consumers is worth more than the semantic nicety, and the code comments already promise paired starts to four consumers. |
| 102.2 | Wire `goal.criteria` into the run verdict, or declare it declarative-only? | **Declarative-only for now, stated honestly.** Fix `docs/HOWTO-authoring.md:915-916` to carry the same "not wired into run verdicts" caveat the `script` bullet already has, and add the goal block to `run_report` output as *unevaluated contract text* so a supervisor can check it by hand. Wiring marker evaluation into the verdict is a real feature with its own spec, not a field-report fix. |
| 102.3 | The vendor contract for `ticketOrderNo` / `sessionId` / `passengers` on the two ancillary endpoints. | **Add all three per the reporter's contract**, `required: [ticket_order_no]` on search and `required: [session_id, ticket_order_no, passengers]` on order, naming the vendor field in each `description` as the file's other functions do. The reporter has live vendor experience we do not; if that is not enough evidence, add them as optional and drop the schema `description` disclaimers. |
| 102.4 | Introduce a distinct `RunStatus::Waiting`, or just stop reporting `Interrupted` for a parked run? | **Stop reporting `Interrupted`** (widen the existing `reported_run_status` repair to cover a parked-on-wait run with a live driver). `waiting_on` / `waiting_kind` already carry the distinction for any UI that wants it, and a new status variant is a wire-format change across MCP, CLI, HTTP and the web badge for little gain. |
| 102.5 | 429 + `Retry-After`, or accept-and-queue? | **429 + `Retry-After` now**, queueing as a separate spec'd feature. Queueing changes the meaning of a 200 from "run started" to "run accepted", which every existing client would have to relearn. |
| 102.6 | Reset connector `max_calls` per attempt (a2), per node visit (a1), or document only? | **Per attempt (a2), with the per-visit floor as the fallback.** The budget exists to bound *playbook logic*; an executor dying mid-node is not playbook logic, and today it turns a recoverable failure into a guaranteed one across the whole fallback chain. The count is derived from the journal, so no history is rewritten - only the scan floor moves. If the `AttemptStarted`-on-disk check comes back unfavorable, ship (a1) plus the error-message fix and revisit. |
| 89 (scope) | Does the fix also cover the two sibling unpaired shapes (`node.rs:2601-2607` `advance_frontier`, `scheduler.rs:140-146` `stop_on_unhandled_failure`)? | **Yes, all three.** The whole point of the issue is that consumers see one shape; fixing one of three leaves the inconsistency. It is ~10 lines each and the four paired-start consumers are safe for all of them. |
| 102.8 | Honor the bundle version, reject bundles carrying one, or document the current behavior? | **Honor it when free, error when taken**, keeping auto-assign as the fallback when the bundle omits `version`. Rejecting a field `apb export` itself writes would be perverse, and re-installing a corrected 1.0.0 is a real workflow. |
| 102.9 | A generic unknown-key validator warning, or add a `prompt`/`body` field to `human_review`? | **Add the field** (`prompt: Option<String>`, rendered into the review instruction at `crates/apb-engine/src/progress.rs:96`) - it is what authors actually want, it is ~S, and it does not require plumbing raw YAML through the validator. Keep the generic V44 unknown-key warning as a separate, later item, since the flatten limitation makes it a real piece of work and it would fire across every node kind at once. |
| 102.10 | Ship a top-level-only `.field` selector, or keep whole-output-only? | **Ship it, top-level-only**, with exactly the `EdgeCondition::OutputField` semantics (absent/non-JSON/non-scalar → empty string, never an error). The semantics are already decided and tested; refusing it keeps a documented prompt-fragility class alive for no benefit. |

---

## Size roll-up

| Size | Count | Items |
|---|---|---|
| **S** | 17 | 103.1, 103.2, 103.3 (scoped mitigation only), 85.1, 85.2, 85.3 (all three: close as fixed), 85.4, 88a, 88b, 89, 91, 102.3, 102.4-A, 102.5, 102.7, 102.8, 102.10 (lower bound) |
| **M** | 5 | 90, 102.1, 102.4-B (only if a new `RunStatus` variant is chosen), 102.6, 102.9 |
| **L** | 0 | none |

Nothing in the batch is L. The two biggest are **#102.1** (extract the child-permit walk
out of `check_run` into a reusable seam - the highest-value item, it is what forced the
reporter into a leaf-only architecture) and **#90** (two live e2e tests).

Four items need no behavior change at all: #85.1/.2/.3 are already fixed and should just be
closed with a pointer to PR #87, and #88a's described shape is unreachable behind V34.

**Suggested landing order** (three files are contended):
1. `crates/apb-engine/src/scheduler*`: #91 → #89 → #90's tests.
2. `crates/apb-server/src/routes/runs.rs`: #103.1 + #103.2 + (#85.4 / #102.4-A, same edit) in one pass.
3. Everything else is independent.

**Open verification items** (cannot be settled by reading source):
- #103.3 - need the raw response bytes from the reporter before any code change.
- #102.3 - need the atrip vendor contract confirmed for the three field names.
- #102.6 (a2) - need to check whether `AttemptStarted` is on disk before the executor
  subprocess reads the log.
- #85.1 - `hermes --yolo` combined with the one-shot `-z` still unverified against a live
  binary (deliberate, documented).
