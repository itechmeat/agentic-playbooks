# Tasks: before running alone

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md` (its section "Decisions taken
in the closure pass" lists the ones made last). If something there does
not fit the code (a function was renamed, a line moved), find the site by
the name `design.md` gives, keep the decision, and note the difference in
the final report. Never invent a different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet. So every section builds in this order: types and changed
signatures first (each step compiles), then a skeleton whose functions do
nothing where a test file needs them to exist, then the red tests, then
the behaviour one piece at a time. A task names the tests that may pass
before its code exists; every other test it writes must be seen failing
first.

Line numbers are those of main e76aa42e. Find every site by the name the
task or the design gives; a line is a hint.

Search the code with zg before opening files: `zg --rg -F <symbol>
<path>` (exact text) or `zg "<question>"` (orientation only). Verify an
edit with plain `rg`, never through the index.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-before-running-alone-target TMPDIR=$HOME/.cache/apb-before-running-alone-tmp
mkdir -p "$TMPDIR"
```

Never build under `/tmp` or `/dev/shm` (a RAM disk shared with other
sessions), and never put a target dir, cache or temp data inside the
worktree. If `$HOME/.cache` is not writable, stop and report it.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never export
`APB_TEST_BILLING_DEFAULT`; the test helpers set it where a test needs
it. Never stop a process you did not start. Do not commit, push or open a
pull request unless the person who assigned the work says so. Do not
start subagents.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-engine --lib list::ledger`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   list_pilot_test`). The CLI package is named `apb`: `cargo test -p apb
   --test main people_cli_test`; its unit tests run with `cargo test -p
   apb --bin apb <module>` (the package has no library target).
3. `cargo test` takes one name filter per invocation: two modules are
   two commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` and, for list
tests, `use crate::list_run_test::{...}` (design D0 "Test helpers").

After any change to a type listed in `crates/apb-server/src/ts_contract.rs`
or registered there by this change, regenerate TypeScript:
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then
`cargo test -p apb-server --lib ts_contract` (must pass without the
variable). Never edit `web/src/lib/api.gen.ts` by hand.

UI work never uses a CSS utility framework: plain CSS over the existing
CSS variables only, in the component's `<style>` block.

## 0. Preconditions

- [x] 0.1 people-in-the-loop is merged and archived: the directory
  `openspec/changes/archive/2026-11-18-people-in-the-loop` exists and
  `openspec/changes/people-in-the-loop` does not exist in this tree. If
  not, stop and report it.
- [x] 0.2 Check these exist (one `rg -n` each; note the line):
  `pub(crate) fn await_decision` and `pub(crate) fn post_unattended` in
  `crates/apb-engine/src/scheduler/review_gate.rs`;
  `pub struct GateDetail` in `crates/apb-engine/src/review/form.rs`;
  `pub fn needs_review_detail` in `crates/apb-core/src/schema.rs`;
  `takes no fields` and `fn check_review_target` in
  `crates/apb-engine/src/review.rs`;
  `pub(super) fn run_batches` in `crates/apb-engine/src/scheduler/list/batch.rs`;
  `pub struct ListFold` in `crates/apb-engine/src/list/ledger.rs`;
  `pub struct PendingReview` and `fn pending_gate` in `crates/apb-engine/src/progress.rs`;
  `pub fn install` and `pub(crate) fn observe` in
  `crates/apb-engine/src/terminal_hook.rs`, its call in
  `EventLog::append_inner` (`crates/apb-engine/src/event.rs`), and
  `apb_engine::health::install()` in `crates/apb-cli/src/main.rs` and
  `crates/apb-engine/tests/main.rs`; `pub fn doctor_line` in `crates/apb-engine/src/start_key.rs`;
  `pub fn stop_run_with` and its `AlreadyTerminal` return in
  `crates/apb-engine/src/stop.rs`; `pub(crate) fn apply_patch` and
  `append_patch_rejected` in `crates/apb-engine/src/scheduler/patch.rs`;
  `pub const LOCAL_ENTRIES` in `crates/apb-core/src/workspace.rs`;
  `impl SignedDoc for CorrectionFile` in
  `crates/apb-core/src/corrections/store.rs`;
  `pub struct FitReport` in `crates/apb-engine/src/fit/report.rs`;
  `pub fn check_start` in `crates/apb-engine/src/gate/unattended.rs`;
  `crates/apb-cli/tests/suite/people_cli_test.rs`;
  `crates/apb-engine/tests/fixtures/journal/known_events.jsonl`.
  `GuardedFeature::ALL` in `crates/apb-core/src/validate/guarded.rs` has
  11 entries ending with `ReviewMaxScope`, and its count test is
  `all_has_eleven_entries_in_order`. If wait-park landed first, `ALL`
  has 12 ending with `WaitPark`: then this change makes it 13 and names
  the test `all_has_thirteen_entries_in_order` (design D0 "One new
  guarded feature"), and `policy_file.rs` already has `notify.remind`
  with `NotifyPolicy::validate(source)`: add the `unattended` section
  beside it without touching it. If anything else is missing, stop and
  report it.
- [x] 0.3 `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u` prints
  `"V243"` to `"V247"` and none of `"V240"`, `"V241"`, `"V242"`,
  `"V249"`, `"V250"`; `rg -n 'list_pilot|list_stopped|probation_required|probation_waived|readiness_required|require_ready|require-ready|waive-probation|ProbationFile|apb-probation|counts_excluding|load_definition' crates docs`
  finds nothing. If one appears, stop and report it.
- [x] 0.4 None of these exists: `crates/apb-core/src/readiness.rs`,
  `crates/apb-core/src/validate/pilot.rs`,
  `crates/apb-core/src/validate/readiness.rs`,
  `crates/apb-core/src/probation.rs`, `crates/apb-engine/src/probation.rs`,
  `crates/apb-engine/src/scheduler/list/pilot.rs`,
  `crates/apb-engine/src/fit/readiness.rs`. If one exists, it is a
  partial earlier attempt: review it against `design.md`.

## 1. Carried over from the review of #288 (design "Carried over from the review of #288", spec review-scope)

Read the design section first: its decisions are numbered 1 to 10, its
callers table rows 1 to 8, and its tests and wrong implementations are
named there. Plain gates must keep today's behaviour and journal shape
through every task of this section.

- [x] 1.1 Check the sites (one `rg -n` each; note the line):
  `pub fn is_stamped_gate`, `fn check_bound_decision`,
  `pub(crate) fn answers_stamped`, `fn check_review_target` and
  `fn check_review_request` in `crates/apb-engine/src/review.rs`;
  `Stamped { person_only: bool }` and `fn withdraw_if_rescoped` in
  `crates/apb-engine/src/scheduler/review_gate.rs`;
  `crate::review::is_stamped_gate(g)` and `fn scripted_review` in
  `crates/apb-engine/src/scheduler.rs`; `pub(super) fn latest_guard` in
  `crates/apb-engine/src/scheduler/patch.rs`; `is_stamped_gate` in
  `crates/apb-engine/src/run_doctor/review.rs`; `fn
  with_fake_passkey_enrolled` in
  `crates/apb-engine/tests/suite/passkey_gate_test.rs`. `rg -n
  'decide_again|DECIDE_AGAIN|consumes_stamped|requires_request|latest_run_guard' crates
  docs` finds nothing. If a site is missing or a name is taken, stop and
  report it.
- [x] 1.2 Tests first: the eight tests of design "Carried over from the
  review of #288" "Tests" item 1 in
  `crates/apb-engine/tests/suite/review_scope_test.rs`, the unit test of
  item 2 in the `mod tests` of `crates/apb-engine/src/review.rs` (with
  its `#[cfg(test)]` hook `AFTER_BOUND_READ`, a no-op until 1.5 calls
  it) and the one of item 3 in
  `crates/apb-engine/tests/suite/passkey_gate_test.rs`. They name nothing
  that does not exist yet (the refusal is matched by the text
  `decide_again`). Run `cargo test -p apb-engine --test main
  review_scope_test`, `cargo test -p apb-engine --lib review` and `cargo
  test -p apb-engine --test main passkey_gate_test`: every new test
  except the four the design names as passing on main fails; note each
  failure.
- [x] 1.3 Signatures (each step compiles, behaviour unchanged):
  `review::requires_request` (form gates only for now: its pilot clause
  comes in task 3.4), `review::consumes_stamped` and
  `review::DECIDE_AGAIN` (decisions 1 and 3); the `passkey: bool` parameter of `answers_stamped` and the
  `passkey` field of `Consume::Stamped`, every caller passing what keeps
  today's behaviour (`true` where the gate is a stamped gate) (decision
  4); `event::latest_run_guard` moved from `scheduler::patch::latest_guard`,
  `patch.rs` calling it (decision 6). `cargo build -p apb-engine`, then
  `cargo test -p apb-engine --lib scheduler::patch` and `cargo test -p
  apb-engine --test main passkey_gate_test`.
- [x] 1.4 The drive (decision 5) and the doctor (decision 7):
  `scheduler.rs` picks `Consume::Stamped { person_only, passkey:
  is_stamped_gate(g) }` for every gate where `consumes_stamped` holds;
  `run_doctor/review.rs` filters by `consumes_stamped`. Run `cargo test
  -p apb-engine --test main review_scope_test`:
  `an_unstamped_entry_never_decides_a_form_gate` passes now.
- [x] 1.5 The post check (decisions 2 and 3): `check_review_target`
  takes the post's `request` and passes it as `named` to
  `check_bound_decision`, which decides on its one journal read (the
  named seq compared with the open request, classified when it is not
  the open one, stamped when it is; a form gate's post that names none
  refused, the engine's excepted), calling `AFTER_BOUND_READ` right after
  that read; a gate where `consumes_stamped` holds goes through it, and
  `append_decision` calls `check_review_request` only for the other
  gates. The texts are
  the design's, each through `review::form::refusal_text`. Then callers
  row 7: `scripted_review` passes the open request's seq. Then change the
  existing engine tests that post on a form gate without a request to
  pass the open request's seq (`review_scope_test.rs`,
  `review_form_test.rs`, `loop_edges_test.rs`), never weakening an
  assertion. Run `cargo test -p apb-engine --test main
  review_scope_test`, `... review_form_test`, `... loop_edges_test`,
  `... passkey_gate_test`, `... scenario_capture_test` and `cargo test
  -p apb-engine --lib review`.
- [x] 1.6 An unreadable definition under a form guard (decision 6): the
  `None` arm of `check_review_target` reads `event::latest_run_guard`.
  Run `cargo test -p apb-engine --test main review_scope_test`: every
  test of 1.2 passes.
- [x] 1.7 The surfaces (design "Tests" items 4 to 6, tests first, each
  seen failing before its code): `review_decide_on_a_form_gate_needs_the_request`
  in `crates/apb-mcp/tests/suite/review_tool_test.rs`,
  `a_form_gate_review_without_request_is_409` in
  `crates/apb-server/tests/suite/runs_api_test.rs`,
  `review_without_request_on_a_form_gate_is_refused` in
  `crates/apb-cli/tests/suite/people_cli_test.rs`. These fail only if a
  surface drops the request or maps the refusal wrongly; if one passes
  at once, say so in the report (the engine change of 1.5 already made
  it pass) and keep it. Change the existing posts on a form gate in
  those three files to pass the request. The `request` doc of
  `ReviewDecideArgs` in `crates/apb-mcp/src/server/args.rs` (callers row
  3). Run `cargo test -p apb-mcp --test main review_tool_test`, `cargo
  test -p apb-server --test main runs_api_test` and `cargo test -p apb
  --test main people_cli_test`.
- [x] 1.8 The doc paragraph of design "Docs" item 1 in
  `docs/HOWTO-authoring.md`. Then `cargo clippy -p apb-engine -p apb-mcp
  -p apb-server -p apb --all-targets -- -D warnings`.

## 2. Shared pieces (design D0)

- [x] 2.1 `list::PilotSpec`, `PilotThen`, `PilotUnattended`, `StopWhen`
  (stub `holds` returning `None`), the constants, `ListSpec.pilot` and
  `stop_when`, `LIST_OUTPUT_FIELDS` with `"stopped"` in
  `crates/apb-core/src/list/mod.rs` (design D1 "Schema").
  `readiness::UnattendedPolicy`, `Unbounded`, `BoundaryDomain` and
  `Playbook.unattended` in a new `crates/apb-core/src/readiness.rs` with
  the four functions as stubs returning `false` or empty (design D5,
  `unbounded_actors` with its `isolation` parameter and `Enforced`), declared
  in `lib.rs`; fix every
  literal `Playbook { .. }` and `ListSpec { .. }` the compiler names. A
  test first in `crates/apb-core/tests/suite/schema_test.rs`:
  `the_unattended_block_parses` (`unattended: { after_attended_successes:
  3 }` reads, an unknown key in the block is refused). `cargo build -p
  apb-core` and `cargo test -p apb-core --test main schema_test`.
- [x] 2.2 The two events of design D0 in `crates/apb-engine/src/event.rs`
  after `ListItemSettled`, and the list arm of `RunState::fold`
  (`state.rs`, "A list visit's records"). One line of each event inside
  the list visit of `crates/apb-engine/tests/fixtures/journal/known_events.jsonl`
  (before that visit's `node_finished`, seqs kept in order). Add
  `PilotView` to `crates/apb-engine/src/list/view.rs` (design D1
  "Surfaces" item 1, with `of` as a stub returning `None`) and
  `PendingReview.pilot` in `progress.rs` (`None` in `pending_review`).
  `cargo build -p apb-engine`; then `cargo test -p apb-engine --test
  main journal_compat_test` and `cargo test -p apb-engine --test main
  review_scope_test` (both pass: nothing writes the new events yet).
- [x] 2.3 Tests first in `crates/apb-core/src/validate/guarded.rs`:
  `a_list_pilot_is_guarded`, update `a_deadline_is_a_guarded_feature`
  (12) and rename `all_has_eleven_entries_in_order` to
  `all_has_twelve_entries_in_order` with `ListPilot` last (design D0
  "One new guarded feature"; 0.2 for the wait-park case). Then the
  variant, `as_str`, `ALL`, `guarded_features` and the module doc
  clause. Run `cargo test -p apb-core --lib validate::guarded` and
  `cargo test -p apb-engine --lib scheduler::prepare`.
- [x] 2.4 Create `crates/apb-core/src/validate/pilot.rs` and
  `validate/readiness.rs`, each with a module doc line and an empty
  `pub(super) fn check_x(playbook: &Playbook, r: &mut ValidationReport)
  {}`; declare them and add the two calls in `validate/mod.rs` exactly as
  design D0 "Validator codes" shows. `cargo build -p apb-core`.

## 3. Pilot slice and early stop (design D1, spec list-pilot)

- [x] 3.1 Core tests first in `crates/apb-core/tests/suite/list_schema_test.rs`:
  `a_pilot_and_stop_when_parse`, `stop_when_holds_on_a_count_or_a_share`
  (the two sentences of design D1 and `min_settled`), `v240_pilot_items`
  (each message), `v241_stop_when_rules`, `v242_then_continue_without_check`.
  Then `StopWhen::holds` and `check_pilot`. Run `cargo test -p apb-core
  --test main list_schema_test`.
- [x] 3.2 Fold tests first in the `mod tests` of `list/ledger.rs` (line
  308): `a_pilot_slice_and_its_decision_fold`,
  `a_revise_keeps_its_note_as_guidance`,
  `a_reopen_clears_stopped_and_keeps_the_pilot`, `pilot_phase_table`
  (every row of design D1 "The one pure rule"). Then `PilotState`,
  `stopped`, the fold rules, `pilot_settled`, `pilot_needs_look`,
  `PilotPhase`, `pilot_phase`, `PILOT_REVISE_REASON`, and
  `ListFold::counts_excluding` (a test first:
  `counts_excluding_leaves_the_slice_out`). Run `cargo test -p
  apb-engine --lib list::ledger`.
- [x] 3.3 `End::PilotGate`, `End::Stopped`, the `run_queue` split with its
  `StopRule` (the rule before a batch is admitted and after a batch that
  ended with no `End`, the batch's own `End` honoured first; design D1
  "The runner" item 2), `run_batches` as design D1 "The runner" item 3
  (the slice's queue holds only the slice ids, `stop` `None` inside it),
  `scheduler/list/pilot.rs` with `step` and `gate_due`,
  `guidance_section` in `list/prompt.rs` and its use in `run_item`,
  `execute` and `compose` (items 4 to 7). `cargo build -p apb-engine`;
  then `cargo test -p apb-engine --test main list_run_test` and `cargo
  test -p apb-engine --test main list_resume_test` (must still pass: no
  list without `pilot` or `stop_when` changes).
- [x] 3.4 Engine tests first in a new
  `crates/apb-engine/tests/suite/list_pilot_test.rs` (declared in
  `crates/apb-engine/tests/main.rs`):
  `a_pilot_runs_its_slice_then_waits`, `continue_runs_the_rest`,
  `revise_reruns_the_slice_with_the_note`,
  `stop_fails_the_node_with_the_rest_pending`,
  `a_clean_pilot_with_then_continue_asks_nobody`,
  `a_rejected_pilot_item_retries_inside_the_slice`,
  `a_resume_after_the_pilot_continues_without_rerunning_it`,
  `a_resume_inside_the_slice_finishes_the_slice_first`,
  `a_supervisor_retry_keeps_the_decided_pilot`,
  `a_new_visit_under_rerun_not_passed_pilots_the_remaining_items`,
  `stop_when_ends_the_visit_between_batches_failed`,
  `an_unattended_pilot_fails_or_continues_as_declared` (the three cases
  of design D1 "Wrong implementations" item 6),
  `the_channel_refuses_an_approve_on_a_pilot_gate`,
  `the_pilot_gate_shows_its_slice` (no `review_detail` of the list node
  in the journal; the pending review carries `pilot` with round 1 and
  the three ids), and the tests of design D1 "Wrong implementations"
  items 9 to 13: `a_decision_for_a_withdrawn_pilot_request_never_answers_the_next`
  (the review's interleaving), `stop_when_waits_for_the_pilot_gate` (the
  review's example: pilot 3, batch size 2, threshold 1),
  `a_paused_batch_is_honoured_before_stop_when`,
  `a_crash_after_list_stopped_ends_the_node_stopped`,
  `a_crash_after_list_pilot_runs_the_same_slice`,
  `a_second_decision_for_the_same_pilot_request_is_refused` and
  `a_run_step_cannot_decide_its_own_pilot` (the last may pass at once
  through `review::refuse_own_run`; say so in the report). Every post in
  these tests names its request unless the test is about a post without
  one. Then: the pilot clause of `review::requires_request` and the guard
  name `list_pilot` in the definition-unreadable refusal (design
  "Carried over from the review of #288", decisions 1 and 6), the drive
  branch with `Consume::Stamped { person_only: false, passkey: false }`
  (design D1 "The gate in the drive", with `decided_event` factored out
  of the human review branch), the channel (design D1 "The channel") and
  the pending gate in `progress.rs` (design D1 "Surfaces" item 4). Run
  `cargo test -p apb-engine --test main list_pilot_test`, then
  `review_test`, `review_scope_test`, `unattended_test`,
  `list_run_test` and `list_resume_test`.
- [x] 3.5 Next actions and scenarios, tests first:
  `crates/apb-engine/src/next_action/unattended.rs` has no test module;
  put `a_pilot_gate_reads_its_unattended_outcome` in
  `crates/apb-engine/tests/suite/list_pilot_test.rs` (a run paused at a
  pilot gate: `fail` gives `fails_run hitl_required`, `continue` over a
  clean slice gives `decides continue`). In
  `crates/apb-core/src/scenario/validate.rs` tests:
  `a_pilot_visit_takes_items_and_review` (one visit with `items` and
  `review: { decision: continue }` on a pilot list node: no V146, V147 or
  V148), `a_pilot_answer_outside_its_options_is_v148` (`approve` refused
  with the pilot text), `a_review_answer_on_a_list_without_pilot_does_not_fit`
  (V147 naming `items`) and `a_pilot_over_a_judge_check_takes_three_kinds`
  (`items`, `judge` and `review` pass V146; four kinds do not); update
  any existing assertion of the old V146 text. In `list_pilot_test.rs`:
  `a_scenario_answers_items_and_the_pilot_gate` (one scenario file whose
  answer for the list node's visit 1 carries the item answers and
  `review: { decision: continue }`: it validates, and the quick run ends
  succeeded with no missing and no extra answer) and
  `a_capture_of_a_pilot_list_names_the_list_gap`. Then the arms of design D1
  "Surfaces" item 5 and "Scenarios". Run `cargo test -p apb-core --lib
  scenario::validate`, `cargo test -p apb-engine --test main
  list_pilot_test`, `cargo test -p apb-engine --test main
  next_action_test` and `cargo test -p apb-engine --test main
  scenario_capture_test`.
- [x] 3.6 Surfaces: `PilotView::of`, `RunList`, `ListProgress` and
  `ListOutcome` fields with the test constructions (design D1 "Surfaces"
  items 1 to 3); a test first in the tests of `run_outcome/lists.rs`:
  `the_line_names_a_waiting_pilot_and_an_early_stop`. Register
  `PilotView` in `ts_contract.rs` and regenerate TypeScript. Web: tests
  first in `web/src/lib/listprogress.test.ts`: `pilotLine names the
  round and the counts`, `stoppedLine shows the detail`; then
  `listprogress.ts`, `ListProgressPanel.svelte` and the slice in
  `ReviewPanel.svelte`. Run `cargo test -p apb-engine --lib run_outcome`,
  `cargo test -p apb --bin apb run_items`, and `cd web && bun run test
  && bun run check`.
- [x] 3.7 Journal corpus row G1 (design "Journal corpus") in
  `crates/apb-cli/tests/suite/journal_corpus_test.rs`, with the module
  doc sentence. Run it the CI way: `APB_CORPUS_REQUIRED=1
  APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries/bin cargo nextest run
  --profile ci -p apb --test main journal_corpus` (when
  `$HOME/.cache/apb-old-binaries/bin/<tag>/apb` is missing for
  `v0.25.0` or `v1.0.0-beta.1`, build it with `scripts/old-apb.sh <tag>
  $HOME/.cache/apb-old-binaries/bin` first).
- [x] 3.8 Docs of design D1 "Docs". Section check: `cargo test -p apb-core
  --test main list_schema_test`, `cargo test -p apb-engine --lib
  list::ledger`, `cargo test -p apb-engine --test main list_pilot_test`.

## 4. Readiness and probation (design D5, spec unattended-readiness)

- [x] 4.1 Core tests first in `readiness.rs` (`#[cfg(test)] mod tests`):
  `a_script_check_and_an_enforced_script_goal_count`,
  `a_marker_is_not_a_deterministic_check`,
  `an_output_item_check_is_not_proof`, `a_judge_item_check_does_not_count`,
  `a_script_item_check_counts_only_when_needs_look_fails_the_node`,
  `a_goal_counts_only_when_enforced`,
  `a_read_only_node_with_an_open_connector_is_unbounded`,
  `a_bounded_connector_names_functions_and_max_calls`,
  `a_declared_external_effect_is_judged_in_the_network_domain`,
  `a_prompted_finish_is_an_actor_without_a_local_boundary`,
  `unbounded_actors_lack_a_tree_boundary` (a stand-in `isolation`
  answering per id),
  `ungated_irreversible_follows_paths` (an irreversible node behind a
  gate is not listed, one reachable around it is, a publish node never),
  `a_failure_before_the_gate_is_ungated`,
  `a_failure_after_the_gate_is_gated` and
  `a_fail_fast_fork_routes_from_its_branches` (design D5 "The
  irreversible-gate walk" and wrong implementation 12),
  `a_read_only_script_is_not_bounded_externally` and
  `an_isolated_agent_without_network_is_bounded_externally` (design D5
  "Boundaries per effect domain", the network domain). Then the four
  functions and `effects::node_inferred` made public (`graphutil.rs` is
  not changed). In
  `crates/apb-core/src/validate/readiness.rs` tests:
  `v249_probation_bounds`, `v250_only_with_an_unattended_block`; then
  `check_readiness`. Run `cargo test -p apb-core --lib readiness`,
  `cargo test -p apb-core --lib effects` and `cargo test -p apb-core
  --lib validate::readiness`; then `cargo test -p apb-core --lib
  validate` (V07 unchanged).
- [x] 4.2 Tests first in `crates/apb-core/tests/suite/policy_file_test.rs`:
  `the_unattended_section_reads`, `a_project_cannot_waive_probation`,
  `a_project_only_raises_the_probation_floor`; then `UnattendedSection`,
  its validation, `Policies::probation_floor` and
  `Policies::waives_probation`. Run `cargo test -p apb-core --test main
  policy_file_test`.
- [x] 4.3 The ledger in apb-core (design D5 "The ledger"), tests first
  in the `mod tests` of a new `crates/apb-core/src/probation.rs`:
  `add_writes_a_run_once`, `add_drops_the_oldest_beyond_the_cap`,
  `counted_takes_eligible_attended_successes_of_one_digest_pair` (an
  unattended, a failed, an ineligible entry and one of another trust
  digest or definition count 0), `an_unsigned_file_reads_unsigned` (the
  `SignedDoc` impl with a test key, as `corrections/store.rs` tests do).
  Then `ProbationFile`, `ProbationEntry`, `add`, `counted`, the
  `SignedDoc` impl, `pub mod probation;` in `lib.rs`, and `probation/`
  in `workspace::LOCAL_ENTRIES` with its comment (add `probation/` to
  the list its test `fresh_project_gets_every_local_entry` checks). Run `cargo test -p
  apb-core --lib probation` and `cargo test -p apb-core --lib workspace`.
- [x] 4.4 Engine tests first in a new
  `crates/apb-engine/tests/suite/probation_test.rs` (declared in
  `tests/main.rs`): `probation_done_admits_the_start`,
  `a_new_version_restarts_probation`, `an_edited_definition_does_not_count`,
  `a_changed_script_resets_the_count`, `only_attended_successes_count`,
  `a_patched_run_is_not_eligible`, `an_overridden_run_is_not_eligible`,
  `one_run_counts_once_across_a_prune`, `an_unverifiable_ledger_refuses`,
  `the_check_counts_the_version_the_start_loads`,
  `a_child_under_probation_refuses_the_parent_start`,
  `an_attended_parent_run_counts_for_its_child`,
  `a_waived_start_covers_its_children`,
  `require_ready_judges_the_loaded_version`,
  `require_ready_refuses_an_unready_child`,
  `only_the_machine_file_or_a_waived_start_passes` (the engine part: a
  `RunOptions.waive_probation` start passes, a start without it is
  refused `probation_required` with the per-project text, the machine
  file's `waive_probation` passes),
  `a_crash_before_run_finished_then_stop_is_never_eligible`,
  `a_crash_after_run_finished_leaves_the_run_uncounted`,
  `the_outcome_comes_from_the_terminal_event` (design D5 wrong
  implementation 14),
  `a_patch_on_an_unattended_run_is_held_to_probation`,
  `a_patch_on_a_require_ready_run_is_held_to_readiness`,
  `a_patch_to_a_proven_ready_definition_applies`,
  `a_patch_on_an_attended_run_checks_neither` (design D5 "A patch is held
  to the same checks"); and in the `mod tests` of a new
  `crates/apb-engine/src/probation.rs`:
  `a_ledger_is_written_only_by_the_terminal_observer` and
  `the_doctor_line_names_an_unverifiable_ledger`. Then, in this order:
  the second slot of `terminal_hook.rs` (`install_ledger`, `observe`
  calling the health observer, then the ledger observer),
  `probation::install` and its calls next to `health::install()` in
  `crates/apb-cli/src/main.rs` and `crates/apb-engine/tests/main.rs`;
  `probation::observe_ended` with the outcome read from the last terminal
  event, the eligibility test of design D5 "Eligible: the exact test" and
  `probation::doctor_line` (`run_end.rs` and its callers stay as they
  are);
  `ProbationView`, `probation`, `check_probation` (the ledger only),
  `PROBATION_REQUIRED`, `NOT_READY`, the re-exports,
  `ChildExpectation::load_definition` in `run_config.rs`,
  `RunOptions.waive_probation` and `RunOptions.require_ready`,
  `RunExecutionManifest.probation_waived` and `readiness_required`
  (written in `prepare.rs` where the manifest records `unattended`),
  their inheritance in `unattended_start`, and the two calls in
  `prepare_placed` after the import-approval check and before `let mut
  playbook = loaded.playbook.clone();` (design D5 "Where it runs": the
  probation check first, then `fit::readiness::start_check`, which comes
  in 4.5: until then `require_ready` tests stay red); then the patch
  check in `scheduler/patch.rs` `apply_patch` after
  `approval::patch_refusal` (design D5 "A patch is held to the same
  checks"; its readiness half also waits for 4.5). Run `cargo test -p
  apb-engine --test main probation_test`, `cargo test -p apb-engine --lib
  probation`, `cargo test -p apb-engine --test main unattended_test`,
  `cargo test -p apb-engine --test main subplaybook_run_test`, `cargo
  test -p apb-engine --test main corrections_test` and `cargo test -p
  apb-engine --test main known_problems_test`, `cargo test -p apb-engine
  --test main health_test` and `cargo test -p apb-engine --lib patch`
  (the last five must still pass: the terminal observer now also writes
  the ledger, and the patch arm has one more check).
- [x] 4.5 Tests first in `fit/readiness.rs` tests: `the_items_in_order`,
  `ready_needs_every_item`, `readiness_never_changes_fit_ready`,
  `not_ready_detail_joins_the_missing_items`,
  `a_best_effort_node_without_a_sandbox_is_not_bounded`,
  `an_unverifiable_ledger_is_a_probation_item`,
  `start_check_leaves_probation_out`. Then `Readiness`,
  `ReadinessItem`, their re-export, `FitReport.readiness`,
  `FitSummary.ready_alone`, `fit/readiness.rs` `assess`,
  `not_ready_detail` and `start_check`, and `unavailable`. Run `cargo
  test -p apb-engine --lib fit`, `cargo test -p apb-engine --test main
  probation_test` (every test passes now) and `cargo test -p apb-engine
  --test main next_action_test` (must still pass).
- [x] 4.6 Launch surfaces, tests first:
  `people_cli_test::require_ready_refuses_a_playbook_without_budget`
  (the refusal comes from preparation: `run failed: not_ready: ...`, exit
  2, no run directory),
  `people_cli_test::an_unready_unattended_start_warns` (the warning names
  the version the gate selected),
  `people_cli_test::waive_probation_needs_a_typed_confirmation` (no
  terminal, also for a playbook that asks for no probation, and inside a
  run step: exit 2 with the design's texts, no run created);
  `people_cli_test::doctor_names_an_unverifiable_probation_ledger`;
  `crates/apb-mcp/tests/suite/run_tools_test.rs`
  `an_unattended_start_under_probation_is_refused` and
  `an_unready_unattended_start_returns_its_readiness`;
  `crates/apb-server/tests/suite/launch_api_test.rs`
  `an_unattended_start_under_probation_is_refused` (422, text starting
  `probation_required`; these two may pass once task 4.4 is in, since the
  check lives in preparation; say so). Then the CLI flags
  (`--require-ready` setting `RunOptions.require_ready`; the waiver
  through `consent::ask_typed`, always asked, design D5 "The waiver"),
  the warning after `gate_run` on the selected version (design D5 "The
  unattended start warns"), the `probation` line of `apb doctor`, and the
  MCP `readiness` in `playbook_run`'s answer. No surface calls
  `check_probation` or `start_check` itself, and the CLI assesses nothing
  before the gate. Run `cargo test -p apb --test main
  people_cli_test`, `cargo test -p apb-mcp --test main run_tools_test`
  and `cargo test -p apb-server --test main launch_api_test`.
- [x] 4.7 Register `Readiness`, `ReadinessItem`, `ProbationView` in
  `ts_contract.rs` and regenerate TypeScript. Web: tests first in
  `web/src/lib/fit.test.ts`: `readinessLines lists what is missing`,
  `readinessLines collapses the ok items`; then `fit.ts` and
  `FitCard.svelte`. Run `cd web && bun run test && bun run check`.
- [x] 4.8 Docs of design D5 "Docs" (the older-binary paragraph included).

## 5. Records

- [x] 5.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both (only
  CLAUDE.md's "Claude-specific" section has no twin; read both edited
  passages side by side), limited to what this change adds: in the
  `apb-core` list `readiness.rs` (the deterministic-check rule and the
  top-level `unattended:` block), `probation.rs` (the signed probation
  ledger), `list`'s `pilot` and `stop_when`, `policy_file`'s
  `unattended:`, the validator
  modules `pilot` (V240 to V242) and `readiness` (V249, V250), and the
  scenario answer of a pilot gate; in the `apb-engine` list
  `scheduler/list/pilot.rs` (the pilot steps and the drive's gate check),
  `fit/readiness.rs` (the readiness section of the fit report and
  `not_ready_detail`, `start_check`), the two new list events,
  `probation.rs` (the ledger's writer, registered as the second
  observer of `terminal_hook`, and the doctor line), `terminal_hook`'s
  two observers, the probation and readiness check of a patch in
  `scheduler/patch.rs`, probation in `gate/unattended` and the pilot arm of
  `next_action/unattended`. Extend the **People in the loop**
  paragraph of "Concepts that span files" with: the pilot gate on a list
  node is a plain gate whose pending block reads the slice from
  `ListFold`, the early stop, both guarded by `list_pilot`; readiness in
  the fit report and probation over eligible attended successes of the
  exact definition and scripts in the same project, counted from the
  signed probation ledger the journal's terminal observer writes once
  per run, after the terminal event and from it (each
  sub-playbook on its own ledger, checked once in preparation on the
  loaded definition together with `--require-ready` over the pinned
  tree, and again on the definition of an in-run patch of an unattended
  run), waived by a typed confirmation always asked at an interactive
  terminal and inherited by every child run, or by the machine file, and
  not held by an older apb; readiness proof and boundaries per effect
  domain (the network domain bounded only by an enforced isolation with
  `network: none`), the irreversible-gate walk over failure transitions
  of reached nodes; the pilot gate's decision names its request like a form gate's,
  stamped with the seq checked on one journal read; and, carried over from
  the review of #288, a decision on a form gate names its request, is refused `decide_again` without it or
  with another request's (also while the definition snapshot does not
  read under a form guard), and is consumed only from an entry stamped
  for the open request (`review::consumes_stamped`).
- [x] 5.2 Write `docs/superpowers/specs/2026-11-DD-before-running-alone-design.md`
  (`DD` the day you write it): the content of `proposal.md` and
  `design.md` as one public record, with no mention of `openspec/` and
  no reference to any outside article, author or project.
- [x] 5.3 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![`
  and Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>`
  finds nothing).
- [x] 5.4 Review fixes, round 1, records and web: the older-binary record
  corrected (design decision 7, D5 "Older apb binaries", "Journal
  corpus", the release notes, the proposal, the public record,
  `docs/GUARDRAILS.md` with one paragraph in "Older apb binaries" and a
  pointer from the new section, `docs/HOWTO-authoring.md`): 1.0.0-beta.1
  and later older releases refuse a definition with `pilot` or
  `stop_when`, 0.25.0 drops the two fields and runs every item ungated,
  no guard added (M8, m18); the V240 to V242 and V249, V250 tables in
  the HOWTO (m18); the spec text of the round's behaviour fixes (A1 to
  A8, M2, M3, m3) in the design, the specs, GUARDRAILS and the HOWTO;
  "the the" in CLAUDE.md and AGENTS.md (m19); web (m17): the two named
  pilot tests, `pilotLine` in `ReviewPanel.svelte` with singular and
  plural, the probation line in `FitCard.svelte` from one derived
  `readinessLines`. Run `cd web && bun run check && bun run test && bun
  run build`.
- [x] 5.5 Review fixes, round 2 (design "Review fixes, round 2"): the
  mode of a probation entry, a patch and a child start from a record
  that exists (`manifest::recorded_mode`), the opening steps of a
  resumed list visit completed before its pilot slice, and every
  command printed for a gate that must name its request rendered by
  `people::instruction::review_command` with the request; the specs,
  `docs/HOWTO-authoring.md`, and the gates of section 6 run again.

## 6. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 6.1 `cargo fmt --all -- --check`
- [x] 6.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 6.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 6.4 The journal corpus the CI way: `APB_CORPUS_REQUIRED=1
  APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries/bin cargo nextest run
  --profile ci -p apb --test main journal_corpus`
- [x] 6.5 `cargo test --workspace --doc`
- [x] 6.6 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; the generated files are current)
- [x] 6.7 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 6.8 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 6.9 `scripts/third-party-notices.sh --check` (this change adds no
  dependency, so it must pass unchanged)
- [x] 6.10 `cargo clippy --release`. Pass criterion: no error, and no
  warning in a file this change touches that `cargo clippy --release`
  on main e76aa42e does not also print (run it once on main in a
  separate worktree or from the recorded output of an earlier run, and
  compare by file and lint name). Every other warning is listed in the
  report with its file and lint, as present on main.
- [x] 6.11 `openspec validate before-running-alone --strict`

## 7. Close

- [x] 7.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place (`cargo sweep --time 7` on it is enough).
  Stop only the processes you started (check `pgrep -f __pause-waker`
  and `pgrep -f __drive`).
- [x] 7.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to
  0.1 to 0.3, each gate of section 6 with its numbers, the user-visible
  changes for the release notes (design "Release notes"), the changed
  file count against the 83 to 85 of design "Files" (78 from the closure
  pass, up to 10 carried over from the review of #288, 8 net from review
  round 1, minus 10 net from review round 2, minus 1 net from review
  round 3), and anything left
  open, naming decision 10 of design "Carried over from the review of
  #288" as left for a later change.
