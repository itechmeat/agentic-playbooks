# Tasks: incremental lists and session fork

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by
the name `design.md` gives, keep the decision, and note the difference
in the final report. Never invent a different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet; the recovery and race tests must fail on a real assertion
before the code that makes them pass exists. So every section builds in
this order: types and changed signatures first (each step compiles),
then a skeleton whose functions do nothing where a test file needs them
to exist, then the red tests, then the behaviour one piece at a time.

Line numbers are those of main efae5822. `eval-doubles` and
`inbox-waits` are merged before this change and move some of them
(design D0 "Files shared with eval-doubles and inbox-waits"): find every
site by its name, and where they added an entry to the same list (the
guard array, the known-events fixture, the run-state fold's arm, the
corpus rows), add this change's entry after theirs.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-incremental-lists-and-session-fork-target TMPDIR=$HOME/.cache/apb-incremental-lists-and-session-fork-tmp
mkdir -p "$TMPDIR"
```

Never build under `/tmp` and never put a target dir, cache or temp data
inside the worktree. Before the first web command run `(cd web && bun
install --frozen-lockfile)` if `web/node_modules/.bin/vite` is missing;
it needs network access. If it fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get
one under `$TMPDIR` (the existing test helpers do this). Never start a
real agent or reach a real service. Never stop a process you did not
start. Do not commit, push or open a pull request. Do not start
subagents.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>` (for
   example `cargo test -p apb-core --lib list::items`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   list_skip_test`). The CLI package is named `apb`: `cargo test -p apb
   --test main journal_corpus_test`.
3. `cargo test` takes one name filter per invocation: two modules are two
   commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;`.

After any change to a type listed in `crates/apb-server/src/ts_contract.rs`
regenerate TypeScript: `APB_WRITE_TS=1 cargo test -p apb-server --lib
ts_contract`, then `cargo test -p apb-server --lib ts_contract` (must pass
without the variable). Never edit `web/src/lib/api.gen.ts` or
`consts.gen.ts` by hand.

Web work: plain CSS over the existing CSS variables only; no CSS
framework.

Search with `zg --rg -F <symbol> <path>` before reading a file.

## 0. Preconditions

- [x] 0.1 Find each symbol in the file named (one `zg --rg -F` each; note
  the line; if one is missing, stop and report it):
  1. `continue_session: Option<String>` and `pub enum FunctionsAllow` in
     `crates/apb-core/src/schema.rs`;
  2. `pub struct ListSpec`, `pub stop_when` and `pub const
     LIST_OUTPUT_FIELDS` in `crates/apb-core/src/list/mod.rs`;
  3. `pub fn items_digest` and `fn canonical` in
     `crates/apb-core/src/list/items.rs`;
  4. `pub(crate) fn check_lists` in `crates/apb-core/src/validate/list.rs`;
  5. `pub enum GuardedFeature`, `pub const ALL` and `pub fn
     guarded_features` in `crates/apb-core/src/validate/guarded.rs`;
  6. `pub(crate) fn check_session_handoff` and `let racing` in
     `crates/apb-core/src/validate/node_io.rs`;
  7. `pub fn cold_handoffs` in `crates/apb-core/src/preflight.rs`;
  8. `pub struct Forms` and `pub fn resume_argv` in
     `crates/apb-core/src/agents/forms.rs`; `pub fn form_flags` and
     `pub fn missing_flags` in `crates/apb-core/src/agents/check.rs`;
     `pub fn probe_output` in `crates/apb-core/src/detect.rs`;
  9. `fn open_visit`, `fn opening_steps`, `fn complete_opening`, `fn
     carry_passed`, `fn park_unknown_sends` and `fn compose` in
     `crates/apb-engine/src/scheduler/list/mod.rs`;
  10. `fn settle_stamped` and `pub(super) struct Settlement` in
      `crates/apb-engine/src/scheduler/list/run.rs`; `let outside` and
      `fn stop_when_holds` in `crates/apb-engine/src/scheduler/list/batch.rs`;
  11. `pub struct ItemRecord`, `pub struct ListCounts`, `pub fn
      counts_excluding` and `pub fn progress_label` in
      `crates/apb-engine/src/list/ledger.rs`; `pub struct RunList`,
      `pub struct ListProgress` and `pub fn run_lists` in
      `crates/apb-engine/src/list/view.rs`;
  12. `pub(crate) fn find`, `pub(crate) fn patch_refusal` and
      `pub(crate) struct HistoryUnreadable` in
      `crates/apb-engine/src/previous_success.rs` (called, not changed);
      `previous_success::patch_refusal(` in
      `crates/apb-engine/src/scheduler/patch.rs`; `pub(crate) fn
      prepare` in `crates/apb-engine/src/scheduler/cache.rs`; `pub fn
      tag_if_keyed` in `crates/apb-core/src/run_origin.rs`; `pub struct
      ResolvedInvocation` in `crates/apb-engine/src/invocation.rs`; `fn entry_of` and
      `let eligible` in `crates/apb-engine/src/probation.rs`;
  13. `let mut previous_success`, `previous_success = None;`,
      `crate::concurrency::admit(`, `Admission::Active` and
      `run_guard_of(&playbook` in
      `crates/apb-engine/src/scheduler/prepare.rs`; `fn after_the_turn`
      and `fn close_out_unclaimed` in
      `crates/apb-engine/src/scheduler/entry.rs`;
  14. `PreviousSuccessSet {`, `SessionHandoff {`, `ListItemSettled {`,
      `pub(crate) fn append_pair`, `fn append_lines` and `pub fn
      is_checkpoint` in `crates/apb-engine/src/event.rs`; `fn drive(` and
      `fn drive_inner` in `crates/apb-engine/src/scheduler.rs`; `pub(crate)
      fn apply_rebind` and `pub(crate) fn set_routed` in
      `crates/apb-engine/src/scheduler/rebind.rs`; `pub(crate) fn digest`
      in `crates/apb-engine/src/run_scripts.rs`; `pub fn digest` in
      `crates/apb-engine/src/run_definitions.rs`; `pub fn tag` in
      `crates/apb-core/src/run_origin.rs`; `pub(crate) fn for_run` in
      `crates/apb-engine/src/decision/mod.rs`; `pub instruction` in
      `crates/apb-engine/src/run_config.rs`;
      `let entry_step` in `crates/apb-engine/src/scheduler/node.rs`;
      `pub fn prepare_supervised_background` and `pub fn drive_prepared`
      in `crates/apb-engine/src/scheduler/entry.rs`; `pub struct
      StartRequestFields` in `crates/apb-core/src/start_key.rs`; `pub fn
      sensitive` in `crates/apb-core/src/run_params.rs`; `pub fn
      applied_supervisor_notes` in `crates/apb-engine/src/context.rs`;
      `pub fn is_scenario` in `crates/apb-engine/src/scenario/session.rs`;
      `enum Build` and `pub(crate) fn required` in
      `crates/apb-engine/src/event.rs`; `pub struct ManifestExecution` in
      `crates/apb-engine/src/manifest.rs`; `fn fingerprint` in
      `crates/apb-core/src/detect.rs`;
      `EventPayload::ListStopped { .. } => {}` in
      `crates/apb-engine/src/state.rs`;
  15. `pub fn resume_spec` in `crates/apb-engine/src/invocation.rs`;
      `pub(crate) fn decide` and `pub(crate) fn is_source` in
      `crates/apb-engine/src/scheduler/handoff.rs`; `enum KnownSession`,
      `fn resolve_session`, `let mut sessions`, `let mut
      pause_session`, `let attempt_prompt` and `let session_now` in
      `crates/apb-engine/src/scheduler/node.rs`;
  16. `pub struct ListOutcome` and `pub fn line` in
      `crates/apb-engine/src/run_outcome/lists.rs`; `fn item_line` in
      `crates/apb-cli/src/run_items.rs`;
  17. `fn g1_list_pilot_old_binaries_refuse_start_and_resume` in
      `crates/apb-cli/tests/suite/journal_corpus_test.rs`.
- [x] 0.2 `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u` does not print
  V283. If it does, stop and report it.
- [x] 0.3 None of these exists: `crates/apb-engine/src/list/skip.rs`,
  `crates/apb-engine/tests/suite/list_skip_test.rs`,
  `crates/apb-engine/tests/suite/session_fork_test.rs`,
  `crates/apb-engine/tests/fixtures/journal/plain_list_run.jsonl`. If one exists, it
  is a partial earlier attempt: review it against `design.md`.
- [x] 0.4 Note the length of `GuardedFeature::ALL` (14 when
  `eval-doubles` is merged, `eval_doubles` last). This change makes it
  one more, `list_skip` last.
- [x] 0.5 Note whether `eval-doubles` and `inbox-waits` are archived under
  `openspec/changes/archive/`. For each one that is, read its entries in
  the shared files (design D0) before sections 2 and 3. If one is not
  merged, build on main as `design.md` says and report it.
- [x] 0.6 Before any other change, on the unmodified tree: create
  `crates/apb-engine/tests/suite/list_skip_test.rs` with only
  `a_plain_list_run_journals_what_main_journals` (design D2 "Tests"
  item 9), declared in `tests/main.rs`; run it once with
  `APB_WRITE_FIXTURES=1` to write
  `crates/apb-engine/tests/fixtures/journal/plain_list_run.jsonl`, then
  without the variable (it passes). Never rewrite the fixture after this
  step: a later failure of this test is a defect in the change. The fork
  forms are fixed in design D3 "Fork forms": run no agent CLI.

## 1. Shared pieces (design D0)

- [x] 1.1 Add the `ListSkip` and `PassedIn` types, `ListSpec.skip`,
  `LIST_SKIP_MAX_ENTRIES` and `LIST_SKIP_MAX_BYTES` to
  `crates/apb-core/src/list/mod.rs` (design D2 "Schema"). Then test
  first in `crates/apb-core/src/validate/guarded.rs`:
  `list_skip_is_a_guarded_feature` (a playbook with `list.skip` lists
  `ListSkip`; one without does not; `list_skip` is the last name) and
  rename the count test `all_has_fifteen_entries_in_order` (`ALL` one
  entry more than 0.4 noted, `ALL[14] == ListSkip`); then
  `GuardedFeature::ListSkip`, its `as_str`, its rule in
  `guarded_features`, and the second count assertion. Run `cargo test -p
  apb-core --lib validate::guarded` and `cargo test -p apb-core --test
  main list_schema_test`.

## 2. Session fork (design D3; spec session-fork)

- [x] 2.1 `crates/apb-core/src/schema.rs`: tests first in its `mod tests`
  (`a_bare_continue_session_round_trips_as_a_string`,
  `the_object_form_reads_fork_and_continue`,
  `an_unknown_mode_is_a_parse_error`); then `SessionMode`,
  `ContinueSession` and its serde; change the field type; `cargo build
  --workspace` and fix every site with `.from()` (design D3 "Schema").
  Run `cargo test -p apb-core --lib schema`.
- [x] 2.2 `crates/apb-core/tests/suite/validate_node_io_test.rs` tests
  first (`two_forks_of_one_source_do_not_race`,
  `v45_still_warns_when_a_fork_races_a_continuation`); then the V45 race
  rule in `validate/node_io.rs`. Run `cargo test -p apb-core --test main
  validate_node_io_test`.
- [x] 2.3 Tests first: `form_flags_include_the_fork_flags` and
  `a_help_without_the_fork_flag_warns` in
  `crates/apb-core/tests/suite/agent_check_test.rs` and
  `fork_spec_inserts_the_fork_flag_after_the_session` in
  `crates/apb-engine/src/invocation.rs` `mod tests`; then `Forms.fork`,
  `Forms::fork_argv`, every agent's `forms()` (claude and opencode `Some`
  exactly as design D3 "Fork forms" fixes them, the eight others
  `None`), `form_flags`, and in the
  engine `fork_argv`, `fork_spec` and the shared private `session_spec`
  under an unchanged `resume_spec`. Run `cargo test -p apb-core --test
  main agent_check_test`, `cargo test -p apb-core --lib agents` and
  `cargo test -p apb-engine --lib invocation`.
- [x] 2.4 Preflight and taint: test first
  `a_fork_on_an_agent_without_a_fork_form_is_cold` in
  `crates/apb-core/tests/suite/validate_node_io_test.rs`; then
  `cold_handoffs` in `crates/apb-core/src/preflight.rs` and the `.from()`
  in `untrusted.rs` (design D3 "Validator and preflight" items 3 and 4).
  Run `cargo test -p apb-core --test main validate_node_io_test`.
- [x] 2.5 `crates/apb-engine/tests/suite/session_fork_test.rs` first
  (design D3 "Tests" item 5, all twelve tests:
  `two_forks_run_in_parallel_and_leave_the_source_unchanged`,
  `a_retry_after_a_fork_continues_the_fork`,
  `a_fork_that_reports_no_session_never_resumes_the_source`,
  `a_delayed_retry_forks_the_source_again`,
  `a_delayed_retry_on_a_fallback_step_forks_on_that_step`,
  `a_pause_and_resume_forks_the_source_again`,
  `a_driver_death_forks_the_source_again`,
  `a_fork_on_codex_starts_cold_with_the_reason`,
  `a_fork_on_a_cli_without_the_flag_starts_cold`,
  `the_help_probe_runs_once_per_process_and_binary`,
  `a_replaced_cli_at_the_same_path_is_probed_again`,
  `the_handoff_journals_mode_fork`), declared in
  `tests/main.rs`; then `handoff::decide` with the mode and
  `fork_help`, `KnownSession::Id.fork`, `resolve_session`, the handoff
  block on every entry, decided for the step a deferred entry resumes,
  the skipped pause seed, the help probe
  cached once per process and executable fingerprint in
  `handoff::fork_help` (over `apb_core::detect`'s `fingerprint`, made
  public), the
  spec choice, the fork prompt and the session left in `node.rs`
  (design D3 "The handoff" and "Re-entry"), `SessionHandoff.mode` (and
  `..` in the pattern of `session_handoff_test.rs` line 70). Run
  `cargo test -p apb-engine --test main session_fork_test`, `cargo test
  -p apb-engine --test main session_handoff_test` and `cargo test -p
  apb-engine --test main operator_pause_test`.
- [x] 2.6 Surfaces: tests first `a forked handoff says fork` in
  `web/src/lib/journal.test.ts` and `continue_session reads both shapes`
  in `web/src/lib/nodefields.test.ts`; then `journal.ts`, `nodefields.ts`,
  `NodePanel.svelte` (both shapes, the checkbox) and `NodeSummary.svelte`
  (design D3 "Surfaces"). Run `cd web && bun run test && bun run check`.
- [x] 2.7 Docs: `docs/HOWTO-authoring.md` (design D3 "Docs").

## 3. Incremental lists (design D2; spec incremental-lists)

- [x] 3.1 `crates/apb-core/src/list/items.rs` `mod tests` first:
  `fields_digest_names_the_fields`, `fields_digest_tells_absent_from_null`;
  then `fields_digest`, which encodes each field's presence. Run `cargo test -p apb-core --lib list::items`.
- [x] 3.2 V283: tests first in `crates/apb-core/tests/suite/list_schema_test.rs`
  (`v283_refuses_an_empty_field_list`, `v283_refuses_a_dotted_field`,
  `v283_refuses_a_repeated_field`, `a_valid_skip_is_clean`); then the
  rule in `validate/list.rs` and the comment in `validate/mod.rs`. Run
  `cargo test -p apb-core --test main list_schema_test`.
- [x] 3.3 `crates/apb-engine/src/scheduler/cache.rs`: test first,
  `work_inputs_takes_each_part` in its `mod tests` (line 1043; one
  assertion per part over the serialized `WorkInputs`, the removed
  start-request fields absent, a sensitive value present only as its
  tag); run it and see it fail. Then add `work_inputs`,
  `start_request_of` and `work_fingerprint` over exactly the six parts
  of design D2 "What `WorkInputs` covers, and what it leaves out": the
  whole definition (its latest journaled digest), the scripts digest
  pinned at start, the start request filled from `RunConfig` with
  `output_limits` and the named removals (every value
  `run_params::sensitive` marks only as `run_origin::tag_if_keyed`,
  `None` meaning `skip_no_key`), what the manifest resolved (its
  execution block whole; for the node the bundle and every resolved
  invocation of the chain and tiers, the canonical executable included
  and `executable_fingerprint` removed by name; connector grants with account identities and
  digests at the node and `<node>#check`), the effective non-secret
  decision configuration, and the identity of the resolved working
  tree. It is the one comparison of work; it shares no code with the
  node cache key, which does not change. `previous_success.rs` is not
  changed. Run `cargo test -p apb-engine --lib scheduler::cache` and
  `cargo test -p apb-engine --test main cache_test` (every existing
  test stays green).
- [x] 3.3a Skeleton of the skip module, before anything names it: create
  `crates/apb-engine/src/list/skip.rs` with `pub mod skip;` in
  `crates/apb-engine/src/list/mod.rs`, the state types (`SkipSet` with
  `refused`, `SkipView`, `Coverage`, design D2 "Reading the set") and
  every function of that section as a stub that compiles and does
  nothing (`unimplemented!` is not allowed: return an empty value or
  `Ok(())`). No behaviour here; it lands in 3.6. Run `cargo build -p
  apb-engine`.
- [x] 3.4 Journal: `EventLog::append_group_decided` on the required
  lock (the durable path of `append_lines`, `JournalLock::required`;
  `append_pair` as its two-event call; `cargo test -p apb-engine --lib event` stays green),
  `ListSkipRead { node, set }` with `SkipSet.refused`,
  `ListStarted.skip_refused`, `ListItemSettled.skipped_from` and
  `skip_digest` in `event.rs` (every full construction gets the two as
  `None`; the compiler lists them, design D2 "Where the set is read and
  journaled"), the no-op arm in `state.rs`, the four fixture lines in
  `known_events.jsonl`; `ItemRecord` fields and the fold; then
  `skipped_items_count_as_passed_and_skipped` and
  `visits_lists_every_visit_in_order` first in `list/ledger.rs` `mod
  tests`, then `ListCounts.skipped`, `skipped_ids`, `ListFold::visits`
  and `progress_label`. Run `cargo test -p apb-engine --lib
  list::ledger` and `cargo test -p apb-engine --test main
  journal_compat_test`.
- [x] 3.5 In the skeleton of 3.3a, write its `mod tests`
  (`the_set_keeps_entries_up_to_both_caps`,
  `recorded_is_none_without_the_event`,
  `the_skip_digest_covers_the_id_the_item_and_the_work`,
  `coverage_tells_all_none_and_partial`); then add to
  `crates/apb-engine/tests/suite/list_skip_test.rs` the other
  tests of design D2 "Tests" item 9 (the table test
  `every_configured_input_reruns_every_item`, the three reproducers
  `a_judge_turned_from_off_to_enforce_reruns_every_item`,
  `an_instruction_only_change_reruns_every_item` and
  `a_worktree_only_change_reruns_every_item`,
  `a_sensitive_param_enters_only_as_a_keyed_tag`,
  `a_supervisor_note_before_the_visit_skips_nothing`,
  `a_worktree_from_a_node_output_never_skips`,
  `a_visit_after_a_scripts_rewrite_skips_nothing`,
  `a_scripts_rewrite_mid_visit_stamps_no_digest`,
  `two_concurrent_entries_record_one_group`,
  `a_crashed_script_only_eval_reads_no_history`, the rebind cases
  `a_rebind_changing_only_the_executor_is_refused` and
  `a_rebind_whose_work_cannot_be_compared_is_refused`, the patch cases
  `a_patch_after_skips_is_refused` and
  `a_patch_before_any_skip_is_applied_and_skips_nothing`, the
  continuation case `a_continuation_after_a_cut_between_set_lines_is_refused`,
  `a_cli_to_host_change_changes_the_work_fingerprint`,
  `a_journal_lock_failure_refuses_the_set_write` and
  `a_crash_after_list_started_replays_the_recorded_skip_decision`,
  `a_crash_then_scripts_rewrite_fails_the_visit`,
  `a_program_only_change_reruns_every_item` and
  `a_crashed_run_with_an_unknown_origin_reads_no_history`,
  `the_second_loop_visit_decides_skipped_rows_again`,
  `a_note_between_visits_makes_the_skipped_item_work`,
  `a_scripts_change_after_the_second_list_started_fails_before_any_skipped_row`,
  `a_carried_item_after_restored_scripts_gets_no_stamp` and
  `a_rebind_changing_only_the_program_path_is_refused` included) (the recovery cases
  `a_queued_run_paused_before_its_turn_records_its_sets_when_resumed`,
  `a_crash_before_the_set_write_records_them_at_the_next_drive` and
  `a_partly_recorded_run_refuses_to_drive` included), and
  `a_list_that_worked_no_item_is_not_eligible` (all its cases, the
  oversized item, the cache restore and the unreached list included) in
  `crates/apb-engine/src/probation.rs` `mod tests`, and see every new
  test fail (the plain-list test of 0.6 stays green).
- [x] 3.6 `read_sets`, `empty_sets`, `recorded`, `coverage`,
  `ensure_recorded`, `skip_digest` and `patch_refusal`; in `prepare.rs`
  the skip read placed after the concurrency key is resolved (`let key =
  crate::concurrency::key_for_start(..)`, line 1132) and before
  `allocate_run` (line 1227), the read after an at-once admission with
  the lifted refused-admission cleanup, and the one grouped write of
  every node's line after `run_guard` (design D2 "Where the set is read
  and journaled"); `after_the_turn` calling `ensure_recorded`, failing
  like the window (`run_error`, `run_finished failed`); `ensure_recorded`
  first in `drive_inner` (`scheduler.rs`), so a queued run paused before
  its turn, a crash before the write and a resume from a named node
  record their sets before driving, and a partial record refuses
  `list_skip_incomplete`; the two refusals in `scheduler/patch.rs` (the
  patch refusal unconditional once the run has skip-derived settlements,
  `list_skip_work_changed`) and the rebind refusal in
  `scheduler/rebind.rs` comparing `work_fingerprint` before and after
  with the whole candidate resolved profile, the same part (d) input
  with every resolved invocation and its canonical executable, never
  `executor_digest` (`list_skip_work_unknown` when it cannot be
  computed); tier routing is not touched (design D2 "The
  patch and rebind refusals"); `ListRun::work`, `Settlement.skipped_from`
  and `skip_digest` in `settle_stamped` (a stamp minted only by a fresh
  settlement whose step executed in this visit under its work); `skip_passed` and the
  visit's skip decision computed in `open_visit` before the
  `ListStarted` append and stored in `skip_refused` (a recorded refusal,
  `work()` failing, the scripts pin, a supervisor note in force), read
  and never recomputed by `opening_steps` and `complete_opening`, the
  scripts pin checked again right before the skip settlements in both
  (a difference fails the visit `list_skip_scripts_changed`), the scripts pin for a skip definition in `prepare.rs`,
  `ensure_recorded` failing closed on the run type, with `carry_passed` as on main for worked rows (it
  leaves rows with `skipped_from` out, returns its ids and copies a
  carried row's `skip_digest` as it had it, never minting one); the stop rule's `outside` in `batch.rs`; `skip_lists_worked` in
  `probation.rs` `entry_of` (only for a node that skipped items, over
  `ListFold::visits`). Run `cargo test -p apb-engine --lib
  list::skip`, `cargo test -p apb-engine --lib probation`, `cargo test
  -p apb-engine --lib previous_success`, `cargo test -p apb-engine
  --test main list_skip_test`, `cargo test -p apb-engine --test main
  list_run_test`, `cargo test -p apb-engine --test main
  list_pilot_test`, `cargo test -p apb-engine --test main
  concurrency_test`, `cargo test -p apb-engine --test main rebind_test`,
  `cargo test -p apb-engine --test main review_form_test` and `cargo test
  -p apb-engine --test main review_scope_test` (the form gate's paired
  write through `append_group_decided`).
- [x] 3.7 Output and views: tests first `the_line_names_skipped_items`
  (`run_outcome/lists.rs` `mod tests`) and `a_skipped_item_names_its_run`
  (`crates/apb-cli/src/run_items.rs` `mod tests`); then `compose`,
  `LIST_OUTPUT_FIELDS`, `SkipView` and `view`, `RunList.skip`,
  `ListProgress.skipped`, `RunListItem.skipped_from`, `ListOutcome.skip`
  and its line, the CLI lines; regenerate TypeScript. Run `cargo test -p
  apb --bin apb run_items`, `cargo test -p apb-engine --lib run_outcome`,
  `cargo test -p apb-engine --test main list_skip_test` and `cargo test
  -p apb-server --lib ts_contract`.
- [x] 3.8 Web: tests first `the label names skipped items`
  (`listprogress.test.ts`), `a skipped row links its earlier run`
  (`ListProgressPanel.test.ts`) and `a skip read line names the node,
  the items and the run` (`journal.test.ts`); then `listprogress.ts`,
  `ListProgressPanel.svelte` and `journal.ts`. Run `cd web && bun run
  test && bun run check`.
- [x] 3.9 Journal corpus: rows L1 and F1 in
  `crates/apb-cli/tests/suite/journal_corpus_test.rs`
  (`l1_list_skip_old_binaries_refuse_start_and_resume`,
  `l1_list_skip_old_binaries_stop_but_never_pause`,
  `f1_fork_old_binaries_refuse_the_definition`; design D2 "Tests" item
  11 and D3 "Tests" item 6), modelled on row G1. The window case runs in this
  order, because a restore deletes the runs directory while the held
  run keeps its journal open: (1) prepare the run in process
  (`prepare_supervised_background`) and, holding it undriven, capture
  the pre-checkpoint snapshot; (2) consume it with `drive_prepared`
  until the gate after the list waits, and capture the gate snapshot;
  (3) only then restore either snapshot, a fresh copy per operation,
  for the old readers (design D2 "Tests" item 11). Build the two released
  readers once, the way CI's `journal-corpus` job does:
  `scripts/old-apb.sh v0.25.0 "$HOME/.cache/apb-old-binaries"` and
  `scripts/old-apb.sh v1.0.0-beta.1 "$HOME/.cache/apb-old-binaries"`
  (it needs the tags locally and network access to fetch them when they
  are missing; it keeps a binary already built). Then run
  `APB_CORPUS_REQUIRED=1 APB_OLD_BINARIES="$HOME/.cache/apb-old-binaries"
  cargo test -p apb --test main journal_corpus_test`. The cases are
  required: a missing binary fails the run, and a run without both
  binaries is not a pass. If the binaries cannot be built, stop and
  report it.
- [x] 3.10 Docs: `docs/HOWTO-authoring.md` and `docs/GUARDRAILS.md`
  (design D2 "Docs"), the trust sentence of item 2 included.

## 4. Records

- [x] 4.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both (only
  CLAUDE.md's "Claude-specific" section has no twin), limited to what
  this change adds: `list.skip` in the `apb-core` list module's entry
  (V283 in the `list` rules), `list_skip` in the `guarded` names,
  `Forms.fork` in the agents entry (the fork form, claude and opencode),
  in the `apb-engine` list entry `skip` (the history read and the
  journaled set), and in "List mode" one sentence on the skip.
- [x] 4.2 Write `docs/superpowers/specs/<date>-incremental-lists-and-session-fork-design.md`
  with `<date>` the day you write it (`date +%F`): the content of
  `proposal.md` and `design.md` as one public record, without the
  Review record, with no mention of `openspec/` and no reference to any
  outside article, author, product or project.
- [x] 4.3 Release notes: if `docs/release-notes/` has a file for the next
  version, add the lines of design "Release notes"; otherwise put them in
  the final report. Do not bump any version.
- [x] 4.4 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![` and
  Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds
  nothing).

## 5. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 5.1 `cargo fmt --all -- --check`
- [x] 5.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 5.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 5.4 `cargo test --workspace --doc`
- [x] 5.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; the generated files are current)
- [x] 5.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 5.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 5.8 `cargo clippy --release` (fix every error, clean warnings as far
  as practical)
- [x] 5.9 `scripts/third-party-notices.sh --check` (no dependency
  changes; it must stay clean)
- [x] 5.10 The journal corpus with the old binaries required, as in 3.9:
  `APB_CORPUS_REQUIRED=1 APB_OLD_BINARIES="$HOME/.cache/apb-old-binaries"
  cargo test -p apb --test main journal_corpus_test`
- [x] 5.11 `openspec validate incremental-lists-and-session-fork --strict`
- [x] 5.12 Remove the `$TMPDIR` contents you created; stop only the
  processes you started (`pgrep -f __drive`, `pgrep -f __pause-waker`).
  Final report: per section what was built, every place where the code
  differed from `design.md` and what you did, the answers to 0.1 to 0.6,
  each gate with its numbers, the release-note lines, the changed file
  count (design "Code map" forecasts 82), and anything left open. Keep
  `$HOME/.cache/apb-old-binaries` (a shared cache of released binaries).

## 6. Fix round 2

- [x] 6.1 Scripts at both ends of every item, a difference latching the
  visit (`ListRun::scripts_held`); test
  `a_scripts_rewrite_after_the_first_settlement_stamps_no_later_item`.
- [x] 6.2 The resolved runner in the work and in execution, pinned per run
  directory (`script::resolved_runner`, part 7) and the isolation of an
  isolated node (part 8, removed in Fix round 7); tests `a_changed_runner_runtime_reruns_every_item`
  and `a_run_keeps_the_runtime_it_resolved`.
- [x] 6.3 The canonical effective directory as part 6
  (`cache::execution_dir`, `scheduler::tree_dir`); tests
  `a_copied_project_works_every_item_in_its_new_directory` and
  `the_execution_dir_is_the_tree_or_the_nodes_own_workdir`.
- [x] 6.4 The fork compares the source's recorded binding; test
  `a_source_rebound_after_it_ran_is_not_forked`.
- [x] 6.5 The consumed journal judged on its own read on both paths
  (`skip::sets_from`, `skip_source_ineligible`); tests
  `a_source_reopened_after_the_lookup_gives_no_skip` and
  `a_window_source_no_longer_settled_gives_no_skip`.
- [x] 6.6 Skipped rows out of the item check evidence and its freshness,
  the source run in the list line; tests
  `an_all_skipped_list_claims_no_executed_check` and
  `skipped_items_are_never_executed_check_evidence`.
- [x] 6.7 The corpus window case prepared in a child process with an
  explicit environment (`l1_window_child`).
- [x] 6.8 Each test seen failing with its fix reverted, then the twelve
  gates on the final tree.

## 7. Fix round 3

- [x] 7.1 A skipped item carries its source output (`SkipSet.outputs`,
  `not_inherited`), within 2 KiB per item and the set's 192 KiB; tests
  `a_skipped_item_carries_its_output_down_the_chain` and
  `an_output_over_the_cap_means_the_item_is_worked`.
- [x] 7.2 The scripts copy of a skipping run read-only while it can execute
  (`run_scripts::protect`, `unprotect`, `writable_tree`); tests
  `a_script_writing_next_to_itself_fails_on_its_own_write` and
  `a_run_without_skip_keeps_its_scripts_writable`.
- [x] 7.3 The latch journaled once and shown (`list_item_settled.skip_latched`,
  `SkipView.stopped_stamping`); test `a_latch_is_journaled_once_and_shown`.
- [x] 7.4 `ensure_recorded` reads the history for an external-data run; tests
  `an_external_data_run_that_crashed_before_its_set_write_reads_the_history`
  and `a_queued_external_data_run_resumed_reads_the_history`.
- [x] 7.5 `freshness::item_check` reads a plain list as on main; test
  `a_plain_list_item_check_is_read_as_on_main`.
- [x] 7.6 Each test seen failing with its fix reverted, then the twelve
  gates on the final tree.
- [x] 7.7 The read-only copy held by a guard for each drive's life
  (`run_scripts::Protected`), restored on every drive exit; tests
  `a_paused_run_leaves_its_scripts_writable_and_a_resume_protects_them`,
  `a_now_pause_leaves_its_scripts_writable` and
  `a_parked_run_leaves_its_scripts_writable`.

## 8. Fix round 4

- [x] 8.1 One resolved execution per list execution (`cache::VisitExecution`):
  the work computed from it and every item of a skip visit executing with
  it (directory, runtime; the captured isolation was removed in Fix round
  7, see 11.1); tests `an_item_runs_in_the_directory_its_visit_resolved`,
  `a_relative_runner_runs_the_program_its_work_names`.
- [x] 8.2 A relative runner taken against the project root and exec'd by its
  canonical path.
- [x] 8.3 `same_profile` takes the execution entry's binding; test
  `a_deferred_retry_rebound_during_its_delay_is_not_forked`.
- [x] 8.4 The dashboard's skip label appends the stamping explanation on
  every branch.
- [x] 8.5 Finding 4 rejected with its reason, written as a stated limit in
  the design and the HOWTO.
- [x] 8.6 Rebased on main 3248c77f; the MCP tool approvals of granted
  accounts are part 9 of the work and reread at each settlement; test
  `a_granted_accounts_mcp_pin_is_part_of_the_work`.

## 9. Fix round 5

- [x] 9.1 The claim written in the design, the spec delta, the HOWTO and the
  skip line's help text (`SKIP_HELP`).
- [x] 9.2 The item check directory in `VisitExecution`, the work and the
  check; test `the_check_directory_is_part_of_the_work`.
- [x] 9.3 `PATH` in `VisitExecution`, the work and the children's env; test
  `path_is_part_of_the_work`.
- [x] 9.4 Secret values and other environment variables recorded as stated
  limits.
- [x] 9.5 The `no_skip` start option reported, not added (over the file
  bound); the HOWTO names a new version as the way.
- [x] 9.6 The deferred-fork test made deterministic.

## 10. Fix round 6

- [x] 10.1 The check policy flags captured, fingerprinted and used; test
  `the_check_policy_is_part_of_the_work`.
- [x] 10.2 Skipped rows of a continued or reopened visit rechecked against
  the work; tests `a_resumed_visit_works_a_skipped_item_whose_runtime_changed`
  and `a_resume_with_nothing_changed_keeps_the_skips`.
- [x] 10.3 The deferred-fork test released through retry-now, no timing.
- [x] 10.4 The future-work line for a start option that skips nothing.

## 11. Fix round 7

- [x] 11.1 V288 refuses `list.skip` on an isolated node; the captured
  isolation, part 8 and the round-4 sandbox plumbing removed; an isolated
  execution never stamps; tests `skip_on_an_isolated_node_is_refused` and
  `skip_holds_under_isolation_none_and_is_refused_when_isolated`.
- [x] 11.2 One rule for whether a visit may hold skips
  (`list::visit_refusal`), a note in force taking back retained skips;
  tests `a_note_in_force_takes_back_the_retained_skips_of_a_reopen` and
  `a_reopen_with_nothing_changed_keeps_the_retained_skips`.
- [x] 11.3 A row taken back keeps no inherited output; test
  `a_skip_taken_back_keeps_no_inherited_output`.
- [x] 11.4 The state table of the continued-visit path in D2, each
  reachable cell with its test.

## 12. Fix round 8

- [x] 12.1 `ListRun::previous` ignores skipped rows; test
  `a_skipped_row_is_never_the_items_earlier_attempt`; the state table row.
- [x] 12.2 The take-back wording from one source, singular handled.
- [x] 12.3 Stale isolation comments and `SKIP_HELP` corrected.
- [x] 12.4 The scripts protection only for a run that may skip; tests
  `an_eval_run_leaves_its_scripts_modes_untouched` and
  `a_scenario_run_leaves_its_scripts_modes_untouched`.
- [x] 12.5 Exact modes given back; test
  `a_drive_gives_the_scripts_copy_its_exact_modes_back`.

## 13. Fix round 9

- [x] 13.1 `a_scenario_run_leaves_its_scripts_modes_untouched` asserts
  through the terminal observer in a child test process instead of a
  polling thread; shown to fail with the scenario exclusion reverted.

## 14. Fix round 10

- [x] 14.1 V289: no skip over a prompt that reads run values; test
  `skip_with_a_prompt_that_reads_run_values_is_refused`.
- [x] 14.2 The runtime identity re-read at the work and every settlement;
  test `a_runtime_replaced_mid_visit_stamps_no_digest`.
- [x] 14.3 The sandboxed check starts `sh` under its configured name; test
  `a_sandboxed_check_starts_sh_under_its_configured_name`.
- [x] 14.4 The installation key created under the env lock in the skip
  tests.
- [x] 14.5 The Linux-only test helpers gated for macOS.
