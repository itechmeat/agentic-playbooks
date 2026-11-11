# Tasks: routine health

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
exist yet; the race, crash and concurrency tests must fail on a real
assertion before the code that makes them pass exists. So every section
builds in this order: types and changed signatures first (each step
compiles), then a skeleton whose functions do nothing where a test file
needs them to exist, then the red tests, then the behaviour one piece at
a time. A task names the tests that may pass before its code exists;
every other test it writes must be seen failing first.

Line numbers are those of main 88b7fd01.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-routine-health-target TMPDIR=$HOME/.cache/apb-routine-health-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-routine-health-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-routine-health-target`
and `TMPDIR=/tmp/apb-routine-health-tmp` instead, and before each build
check that `df -h /tmp` shows more than 4G free; if not, stop and report
it. `/tmp` is a RAM disk shared with other sessions. Never use `/dev/shm`,
and never put a target dir, cache or temp data inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Do not commit, push or open a pull request. Do not
start subagents.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-engine --lib concurrency`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   concurrency_test`). The CLI package is named `apb`: `cargo test -p apb
   --test main prune_cli_test`.
3. `cargo test` takes one name filter per invocation: two modules are
   two commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` (see
`crates/apb-engine/tests/suite/start_key_test.rs` lines 7 to 60 for a
project fixture and `notify_send_test.rs` lines 25 to 120 for a notify
connector stand-in built on `common::ScriptedHttpServer`).

After any change to a type listed in `crates/apb-server/src/ts_contract.rs`
or to `Transition`, regenerate TypeScript:
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then
`cargo test -p apb-server --lib ts_contract` (must pass without the
variable).

## 0. Preconditions

- [x] 0.1 Check these exist (one `rg -n` each; note the line): 
  `pub fn stop_run_with` and `pub struct StopRequest` in
  `crates/apb-engine/src/stop.rs`; `pub struct RunsSection` in
  `crates/apb-core/src/policy_file.rs`; `pub struct StartKeysFile` in
  `crates/apb-core/src/start_key.rs`; `pub fn transact` and
  `pub enum Unsigned` in `crates/apb-core/src/signed_store.rs`;
  `Deadline,` in `crates/apb-core/src/validate/guarded.rs`;
  `pub(crate) fn run_guard(` in
  `crates/apb-engine/src/scheduler/prepare.rs`;
  `pub const DEADLINE_EXCEEDED` in `crates/apb-engine/src/run_deadline.rs`;
  `pub fn chain_of` in `crates/apb-engine/src/step_start.rs`. If any is
  missing, stop and report it.
- [x] 0.2 `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u` prints
  exactly `"V200"`, `"V220"`, `"V221"`, `"V222"`. If V210, V211 or V212
  appears, stop and report it.
- [x] 0.3 None of these exists: `crates/apb-core/src/concurrency.rs`,
  `crates/apb-core/src/health.rs`, `crates/apb-engine/src/concurrency.rs`,
  `crates/apb-engine/src/health/`, `crates/apb-engine/src/retention/`.
  If one exists, it is a partial earlier attempt: review it against
  `design.md`.

## 1. Shared pieces (design D0)

- [x] 1.1 `crates/apb-core/src/workspace.rs` `LOCAL_ENTRIES` (line 27):
  add `"concurrency/"`, `"health/"`, `"runs-archive/"` after
  `"start-keys/"` (line 52), each with a one-line comment in the style of
  the others. Run `cargo test -p apb-core --lib workspace`.
- [x] 1.2 Create `crates/apb-core/src/validate/concurrency.rs` with
  `pub(super) fn check_concurrency(playbook: &Playbook, r: &mut
  ValidationReport) {}` and `crates/apb-core/src/validate/health.rs` with
  `pub(super) fn check_health(playbook: &Playbook, r: &mut
  ValidationReport) {}`, each with a module doc line. In
  `crates/apb-core/src/validate/mod.rs` add `mod concurrency;` and
  `mod health;` in alphabetical place and the two calls after line 456
  exactly as design D0 "Validator codes" shows. `cargo build -p
  apb-core`.
- [x] 1.3 `crates/apb-engine/src/deadline_hooks.rs` `Hook`: add the six
  variants of design D0 "Test barriers" with their doc lines (no call
  site yet). `cargo build -p apb-engine --features testing`.

## 2. Review channel (design D4, spec `review-channel`)

- [x] 2.1 Tests first in `crates/apb-engine/src/review.rs` `mod tests`
  (line 1017): the seven tests of design D4 "Tests", each asserting what
  it lists. Build a whole line with `serde_json::to_string(&ReviewEntry
  { .. })` from an entry `post_review` wrote in a scratch directory.
  Run `cargo test -p apb-engine --lib review::tests`; tests 1, 5, 6 and
  7 fail on their assertions, 2, 3 and 4 pass (they pin what must not
  change).
- [x] 2.2 Create `crates/apb-engine/tests/suite/review_torn_test.rs`
  with `a_gate_waits_through_a_torn_line` (design D4) and declare it in
  `crates/apb-engine/tests/main.rs`. Run `cargo test -p apb-engine
  --test main review_torn_test` and see it fail (the run ends `failed`).
- [x] 2.3 Reader: add `fn split_reviews(bytes: &[u8]) -> (Vec<&[u8]>,
  Option<&[u8]>)` in `review.rs` and rewrite `read_reviews_after` (line
  988) as design D4 item 1 says (`std::fs::read`, each part decoded on
  its own). Run `cargo test -p apb-engine --lib review::tests`: 1, 2, 3,
  4 and 6 pass, 5 and 7 still fail.
- [x] 2.4 Writer: `fn repair_tail(path: &Path)` (design D4 item 3) called
  in `append_decision` (line 801) right after the lock at line 814 and
  before `check_review_request`; replace `writeln!(file, "{line}")` with
  `file.write_all(format!("{line}\n").as_bytes())`. Run `cargo test -p
  apb-engine --lib review::tests` (all seven pass) and `cargo test -p
  apb-engine --test main review_torn_test` (passes).
- [x] 2.5 Run `cargo test -p apb-engine --test main notify_send_test`
  (it decides reviews) and `cargo test -p apb-server --test main
  auth_test` (it reads the channel at line 894); both pass.

## 3. Concurrency key (design D1, spec `concurrency-key`)

### 3A. Core

- [x] 3.1 Create `crates/apb-core/src/concurrency.rs` with the types and
  constants of design D1 "Schema" (`ConcurrencyBlock`, `OnBusy` with
  `as_str`, `KEY_MAX_CHARS`, `DISPLAY_MAX_CHARS`, `KEY_DOMAIN`,
  `SUPERSEDED`, `WHOLE_PLAYBOOK_KEY`) and `pub mod concurrency;` in
  `crates/apb-core/src/lib.rs`. Tests first in `concurrency.rs`:
  `a_block_round_trips` (YAML with key and `on_busy: supersede` parses
  and serializes back equal), `on_busy_defaults_to_queue_latest`,
  `an_unknown_field_is_refused`, and in `schema.rs`'s tests
  `a_playbook_reads_its_concurrency_block` (fails: the field does not
  exist yet). Then add `Playbook.concurrency`
  (`crates/apb-core/src/schema.rs`, after `deadline`, line 207) and
  `concurrency: None` in every `Playbook { .. }` literal `cargo build
  --workspace --all-targets` lists. Run `cargo test -p apb-core --lib
  concurrency` and `cargo test -p apb-core --lib schema`.
- [x] 3.2 V210 and V211 in `validate/concurrency.rs` (design D1 "Schema",
  messages 1 to 5). Tests first in the same file: `v210_empty_key`,
  `v210_key_over_500_characters`, `v210_reads_a_node_output`,
  `v210_reads_an_undeclared_param`, `v210_reads_a_secret_param`,
  `v210_reads_a_param_whose_name_looks_secret` (a param named
  `api_token`), `a_declared_plain_param_is_valid`,
  `an_absent_key_is_valid`, `v211_supersede_with_irreversible_effects`
  (warning, not error), `queue_latest_with_irreversible_effects_is_quiet`.
  Each asserts the code and severity of the one issue (or none). Run
  `cargo test -p apb-core --lib validate::concurrency`.
- [x] 3.3 `GuardedFeature::Concurrency` in
  `crates/apb-core/src/validate/guarded.rs` (enum after `Deadline` line
  46, `ALL` line 51 becomes `[GuardedFeature; 9]`, `as_str`, `parse`,
  `guarded_features` line 85, module doc). Tests first in that file:
  `a_concurrency_block_is_guarded`, `all_has_nine_entries_in_order`,
  `concurrency_parses_and_prints`. Run `cargo test -p apb-core --lib
  validate::guarded`.

### 3B. Journal and signatures

- [x] 3.4 `crates/apb-engine/src/event.rs`: `ConcurrencyRecord` (design
  D1 "Identity and display") next to `PreviousSuccess` (line 56);
  `RunProvenance.concurrency` after `start_key` (line 206);
  `EventPayload::PreviousSuccessSet`; `is_checkpoint` (line 3541) returns
  `false` for it. Add `concurrency: None` to every `RunProvenance { .. }`
  literal the compiler lists. Tests in `event.rs`'s test module:
  `run_provenance_without_concurrency_decodes_none`,
  `run_provenance_with_another_concurrency_shape_decodes_none` (a
  `"concurrency": 7`), `previous_success_set_round_trips`,
  `previous_success_set_is_not_a_checkpoint`,
  `previous_success_set_then_node_finished_reads` (design D1 test 25).
  Run `cargo test -p apb-engine --lib event`.
- [x] 3.5 `crates/apb-engine/src/error.rs`: `EngineError::Superseded(String)`
  with `#[error("superseded: {0}")]`. Fix every exhaustive match the
  compiler lists (a superseded error maps where `Conflict` maps). `cargo
  build --workspace --all-targets`.
- [x] 3.6 `crate::previous_success::recorded(events)` in
  `crates/apb-engine/src/previous_success.rs` (design D1). Tests first
  in `previous_success.rs`: `recorded_prefers_the_latest_set`,
  `recorded_falls_back_to_provenance`,
  `a_patch_is_judged_against_the_set_window` (a run whose provenance
  recorded `None` and whose `previous_success_set` recorded a window: a
  patch that reads `run.previous_success.started_at` is not refused),
  `the_fold_takes_a_later_previous_success_set`. Then use `recorded` in
  `patch_refusal` (line 339), `run_view::previous_success` (`run_view.rs`
  line 408) and the `continued_from` copy in `prepare_placed` (lines
  1032 to 1037), and add the `PreviousSuccessSet` arm to
  `RunState::fold` (`state.rs` line 570). Run `cargo test -p apb-engine
  --lib previous_success`.
- [x] 3.7 Signature before callers: `run_guard_of` (`prepare.rs` line
  1767) takes `concurrency: bool` (design D1 "Journal and guard"); the
  call at line 1460 passes `false` for now; `run_guard` (line 1757)
  passes whether the journal's `run_provenance` has a `concurrency`
  record. Test first in a new `#[cfg(test)] mod run_guard_tests` at the
  end of `prepare.rs`: `run_guard_of_names_concurrency_only_when_asked`
  (a playbook with the block: `false` gives no `concurrency` name, `true`
  gives it). Run `cargo test -p apb-engine --lib run_guard_tests`.
- [x] 3.8 `Prepared` (`scheduler/entry.rs` line 190) gets
  `slot_owner: Option<crate::concurrency::SlotOwner>` and `key_queued:
  bool`; to compile before the module exists, declare in a new
  `crates/apb-engine/src/concurrency.rs` (`pub mod concurrency;` in
  `lib.rs`) only `pub struct SlotOwner;` with a doc line. Move the body
  of `close_out_unclaimed` (line 267) into a free function
  `close_out_unclaimed(log: &mut EventLog, run_dir: &Path, e:
  &EngineError)` with the new `Superseded` arm (design D1 "Waiting for
  the turn" item 1), the method a one-line wrapper. Change
  `claim_queued_workdir` (line 218) to return `Result<Option<RunResult>,
  EngineError>` (always `Ok(None)` for now) and make its three callers
  (lines 298, 332, 449) return the answer when it is `Some`. Run `cargo
  build -p apb-engine` and `cargo test -p apb-engine --test main
  workdir_test` (unchanged, passing).

### 3C. Red end-to-end tests

Every wait in these tests has a timeout (10 s unless the test says
otherwise) and fails with a message naming what it waited for; a hook
that is never reached fails its test by that timeout, not by hanging.

- [x] 3.9 Create `crates/apb-engine/tests/suite/concurrency_test.rs`
  (declared in `tests/main.rs`) with design D1 tests 1, 2, 4, 5, 6
  (`a_live_preparation_keeps_its_key` only), 7, 9, 10, 12, 13, 14,
  17, 19, 20, 21, 22 and 24, each asserting exactly what the design
  says (test 11 needs routine health and is written in task 5.6). For
  the SIGTERM trap reuse the stand-in agent pattern of `stop_run_test.rs`
  `sleepy_agent` (line 83); for the marker holder, the script node of
  design D1's test preamble. Run `cargo test -p apb-engine --test main
  concurrency_test` and record which
  fail: all of them must fail except 12 and 17, which pass before the
  code exists (they guard a wrong implementation) and must keep passing.

### 3D. The engine module, one piece at a time

- [x] 3.10 In `crates/apb-engine/src/concurrency.rs`: `ConcurrencyFile`
  with its `impl signed_store::SignedDoc` (copy the shape of
  `impl SignedDoc for StartKeysFile`, `crates/apb-core/src/start_key.rs`
  line 224), `Slot`, `Holder`, `PendingStop`, `TURN_STEP`,
  `TREE_AFTER_TURN`, `RunCheck`, `RunDir` and `checked_run` (design D0
  "The run check"). Unit tests first (a temp project with seeded run
  directories: a journal with `run_started` and `run_provenance`, and
  the origin stamp through `apb_core::run_origin::stamp`):
  `a_valid_run_checks_valid`, `a_linked_run_directory_is_void`,
  `a_run_of_another_playbook_is_void`, `a_different_key_digest_is_void`,
  `a_missing_provenance_is_void`,
  `an_unknown_event_after_the_last_checkpoint_is_unreadable`,
  `a_torn_tail_is_unreadable`. Run `cargo test -p apb-engine --lib
  concurrency`.
- [x] 3.11 Seeded red tests, now that the types exist: add design D1
  tests 3, 6 (`a_seeded_provisional_holder_with_a_live_owner_is_kept`
  and `a_crash_before_provenance_frees_the_key`), 8, 15, 16 and 23 to
  `concurrency_test.rs`. Run `cargo test -p apb-engine --test main
  concurrency_test` and see the new ones fail.
- [x] 3.12 `SlotOwner` (the owner lock, design D1 "Store and slots") and
  `heal` with rules 1 to 5. Unit tests first:
  `a_holder_with_a_live_owner_is_kept_whatever_its_directory`,
  `a_provisional_active_holder_with_a_durable_start_is_confirmed`,
  `a_provisional_active_holder_without_one_is_cleared`,
  `a_provisional_resume_with_run_resumed_is_confirmed`,
  `an_unreadable_provisional_holder_is_kept`,
  `a_waiter_with_a_live_driver_claim_is_kept`,
  `a_waiter_whose_owner_and_driver_are_gone_is_released_and_stopped`
  (asserts a `stops` entry for it), `a_terminal_holder_is_cleared`,
  `a_paused_holder_keeps_the_slot`, `a_void_holder_is_cleared`. Run
  `cargo test -p apb-engine --lib concurrency`.
- [x] 3.13 `post_stops` (design D1 "Posting promised stops"). Unit tests
  first: `a_stop_is_posted_after_the_transaction`
  (`pending_stops_are_posted_then_dropped`: the seeded run ends
  `run_aborted` with the stop's reason and the entry is gone),
  `a_stop_against_a_provisional_holder_is_deferred`,
  `a_failed_post_keeps_the_entry`. Run `cargo test -p apb-engine --lib
  concurrency`.
- [x] 3.14 `key_for_start` (with its `KeyScope` argument: `scenario:
  bool, eval: bool, child: bool, depth: usize`), `lineage`, `admit`
  (rules 1 to 5 of design D1 "Admission") and `doctor_line(root) ->
  Option<String>` (the WARN detail for slot files that do not verify,
  built like `apb_engine::start_key::doctor_line`, `start_key.rs` line
  459). Unit tests first: `a_free_slot_admits_active`,
  `a_busy_slot_queues`, `a_second_waiter_replaces_the_first_with_a_stop`,
  `a_waiter_without_an_active_holder_is_replaced_and_the_new_start_is_active`,
  `supersede_promises_a_stop_of_the_active_run`,
  `refuse_on_a_busy_slot_writes_nothing` (the file bytes are unchanged),
  `refuse_counts_a_waiter_as_busy`,
  `the_starter_is_refused_as_own_run`,
  `an_unverified_file_refuses_admission`
  (`concurrency_state_unreadable`), `a_refusal_releases_the_owner_lock`,
  `a_sensitive_value_refuses_the_key` (the error text holds the param
  name, not the value), `display_is_scrubbed_and_clipped_to_120_characters`,
  `two_keys_with_one_display_have_two_digests`, `no_block_takes_no_slot`,
  `a_child_takes_no_slot`. Run `cargo test -p apb-engine --lib
  concurrency`.
- [x] 3.15 `SlotOwner::confirm`. Unit tests first:
  `confirm_clears_an_active_reservation_and_releases_the_lock`,
  `confirm_keeps_a_waiters_owner`,
  `confirm_fails_concurrency_lost_when_the_slot_changed`,
  `confirm_posts_a_deferred_stop`. Run `cargo test -p apb-engine --lib
  concurrency`.
- [x] 3.16 `wait_turn` and `Turn` (design D1 "Waiting for the turn").
  Unit tests first: `wait_turn_returns_for_the_active_run`,
  `wait_turn_promotes_the_waiter_when_the_slot_frees`,
  `wait_turn_reports_superseded_for_a_replaced_waiter`,
  `wait_turn_stops_at_a_posted_stop`,
  `wait_turn_answers_paused_for_a_journaled_pause_and_leaves_the_slot`,
  `wait_turn_goes_on_when_the_deadline_stop_is_not_due`,
  `wait_turn_without_an_owner_re_reserves_its_place`. Run `cargo test -p
  apb-engine --lib concurrency`.
- [x] 3.17 `reserve_resume`, `ResumeReservation` with `confirm` and
  `impl Drop`, `patch_refusal`. Unit tests first:
  `a_reservation_dropped_unconfirmed_frees_the_slot`,
  `a_reservation_is_refused_while_another_run_is_active`,
  `a_second_resume_of_the_same_run_is_refused`,
  `a_paused_holder_resuming_keeps_its_slot`,
  `a_dropped_second_reservation_leaves_the_first`,
  `a_patch_changing_the_block_is_refused`. Run `cargo test -p apb-engine
  --lib concurrency`.

### 3E. Wiring

- [x] 3.18 `prepare_placed` (`prepare.rs` line 666): steps 1 to 6 of
  design D1 "In preparation" (leave the hold check of step 1 for task
  5.12): the key, no tree lock for a keyed start with the run config's
  wait, the driver claim, `admit` with its cleanup, the `KeyAdmitted`
  barrier, `Prepared.slot_owner` and `key_queued`, the window rule,
  `run_provenance.concurrency`, the guard call passing `key.is_some()`,
  `confirm`. Make `look_up_previous_success` (line 1815) `pub(crate)`.
  Add to `crates/apb-engine/tests/suite/run_guard_test.rs`:
  `a_keyed_run_is_guarded_for_concurrency` and
  `a_child_of_a_keyed_playbook_is_not_guarded_for_concurrency` (fail
  first, then pass). Run `cargo test -p apb-engine --test main
  run_guard_test`, `cargo test -p apb-engine --test main start_key_test`,
  `cargo test -p apb-engine --test main workdir_test`, and `cargo test
  -p apb-engine --test main concurrency_test`: at least tests 5, 6, 13,
  16, 19 and 20 now pass.
- [x] 3.19 Step 7 of preparation (the supervisor spawn skipped when
  `key_queued`) and the two drive entries of design D1 "Waiting for the
  turn": `wait_turn` in `claim_queued_workdir` and in
  `drive_run_from_dir` with their `Turn::Paused` and `Err` handling, the
  supervisor spawn after the turn, and the window lookup with
  `PreviousSuccessSet` (design D1 "A queued routine reads its window").
  Run `cargo test -p apb-engine --test main concurrency_test` (all but
  8 and 15 pass) and `cargo test -p apb-engine --test main previous_success_test`.
- [x] 3.20 `scheduler/resume.rs` `resume_inner_checked` (line 1074):
  `reserve_resume` before `acquire_tree_for` (line 1222),
  `reservation.confirm()` after `journal_resumed` returned `Ok`, with the
  `concurrency_lost` stop. Run `cargo test -p apb-engine --test main
  concurrency_test` (all pass), `cargo test -p apb-engine --test main
  run_deadline_test` and `cargo test -p apb-engine --test main
  stop_run_test`.
- [x] 3.21 `crates/apb-engine/src/scheduler/patch.rs`: the concurrency
  refusal after line 123; `crates/apb-engine/src/forward_patch.rs`
  `protected_fields` (line 254) grows to 10 entries. Test first in
  `forward_patch.rs`'s tests: `a_forward_patch_may_not_change_concurrency`.
  Run `cargo test -p apb-engine --lib forward_patch` and `cargo test -p
  apb-engine --test main concurrency_test a_patch_cannot_drop_the_block`.

### 3F. Surfaces

- [x] 3.22 `ConcurrencyView`, `run_view::concurrency` and
  `concurrency_line` in `crates/apb-engine/src/run_view.rs` next to
  `deadline` (line 454). Unit tests first:
  `concurrency_view_of_an_active_run`, `concurrency_view_of_a_waiter`
  (`waiting_for` set), `no_record_no_view`. Test first in
  `crates/apb-engine/tests/suite/report_html_test.rs`:
  `the_report_shows_the_concurrency_line_when_recorded` (modelled on
  `the_report_shows_the_deadline_line_when_recorded`, line 402). Wire
  the four surfaces of design D1 "Surfaces" items 1 to 4
  (`crates/apb-cli/src/run.rs` after line 2136,
  `crates/apb-mcp/src/tools/run.rs` after line 369,
  `crates/apb-server/src/routes/runs.rs` `RunDetail` line 240 and line
  623, `crates/apb-engine/src/report_html/model.rs` line 112 and
  `sections.rs`). Run `cargo test -p apb-engine --lib run_view` and
  `cargo test -p apb-engine --test main report_html_test`.
- [x] 3.23 Register `ConcurrencyRecord` and `ConcurrencyView` in
  `crates/apb-server/src/ts_contract.rs` (beside line 151) and
  regenerate TypeScript (see Environment).
- [x] 3.24 Web: tests first in `web/src/lib/RunDriveNotes.svelte.test.ts`
  `shows the concurrency key and what it waits for`, and in
  `web/src/lib/journal.test.ts` `reads a superseded abort` and `reads
  previous_success_set`; then `web/src/pages/RunView.svelte` line 634
  passes `concurrency={detail.concurrency}`;
  `web/src/lib/RunDriveNotes.svelte` takes `concurrency?:
  ConcurrencyView` and shows `Key: <display> (<on_busy>)` and, when set,
  `waiting for run <id>`; `web/src/lib/journal.ts` `eventNote` (line
  129): `superseded` for a `run_aborted` whose reason starts
  `superseded:`, and the note of `previous_success_set`. Run `cd web &&
  bun run test && bun run check`. Plain CSS only.
- [x] 3.25 Docs: `docs/HOWTO-authoring.md` `## concurrency (one run per
  key)` before `## deadline` (line 4349), with the content design D1
  "Surfaces" lists (including: a keyed start waits for the tree only
  after its key, a paused waiting run pauses after its turn, a
  supervisor starts after the turn) and the V210, V211 rows in the
  format of the V177 row (line 249).
- [x] 3.26 Section check: `cargo test -p apb-core --lib validate`,
  `cargo test -p apb-engine --lib concurrency`,
  `cargo test -p apb-engine --test main concurrency_test`,
  `cargo test -p apb-engine --test main run_guard_test`,
  `cargo test -p apb-engine --test main start_key_test`,
  `cargo test -p apb-engine --test main stop_run_test` pass.

## 4. Run retention (design D3, spec `run-retention`)

- [x] 4.1 `crates/apb-core/src/policy_file.rs`: tests first in the
  file's `runs_tests` module: `retention_defaults_are_30_20_90`,
  `a_keep_value_out_of_range_is_refused` (0 and 3651),
  `a_project_only_lengthens_retention` (machine 30, project 60 gives 60;
  machine 60, project 30 gives 60). Then `RunsSection.keep_days`,
  `keep_last`, `keep_failed_days` (line 59), their ranges in
  `RunsSection::validate` (line 66), `RetentionSettings` and
  `Policies::retention`, the `summary` lines. Run `cargo test -p
  apb-core --lib policy_file`.
- [x] 4.2 `apb_core::health::RunClass` with `as_str`, `parse` and
  `class_of` only (design D2 "The facts a rule reads"; the rest of
  `health.rs` comes in 5.1): create `crates/apb-core/src/health.rs`
  (`pub mod health;` in `lib.rs`). Tests first: `a_person_abort_is_ignored`,
  `a_superseded_abort_is_ignored`, `a_deadline_abort_is_a_failure`,
  `an_unended_run_is_ignored`, `an_abort_without_by_is_ignored`,
  `class_words_round_trip`. Run `cargo test -p apb-core --lib health`.
- [x] 4.3 Skeleton that compiles: `crates/apb-engine/src/retention/mod.rs`
  and `retention/archive.rs` (`pub mod retention;` in `lib.rs`) with
  every type and signature of design D3 "The plan" and "The archive and
  its commit protocol", `PruneReport` deriving `Default`, and bodies
  that do nothing: `plan` returns `Ok(Vec::new())`, `apply` returns
  `Ok(PruneReport::default())`, `recover` returns `Ok(())`,
  `write_summary` returns `Ok(())`, `read_verified_locked` returns an
  empty `Vec`, `runs_view` lists `.apb/runs` with no archive, and
  `abort_code` returns `"other"`. `run_stats::build` (line 403) gets its
  second argument `archived: &[ArchivedRun]`, unused for now, and every
  caller the compiler lists passes `&[]`. `cargo build --workspace
  --all-targets`.
- [x] 4.4 Red tests: create `crates/apb-engine/tests/suite/retention_test.rs`
  with design D3 tests 1 to 4, 5 (`a_plan_writes_nothing`), 6 to 13, 15
  and 16, and add test 17 to `crates/apb-engine/src/run_stats/tests.rs`.
  Run `cargo test -p apb-engine --test main retention_test` and `cargo
  test -p apb-engine --lib run_stats`; every new test fails except
  `a_plan_writes_nothing` and `the_trash_is_not_used`, which pass
  against the empty skeleton and must keep passing.
- [x] 4.5 `archive.rs`: `abort_code`, `ArchivedFamily` with its MAC,
  `write_summary`, `read_verified_locked`, `runs_view` under the archive
  lock, and the lookup of a directory gone after the listing. Unit tests
  first: `abort_code_maps_each_known_reason` (deadline, superseded,
  operator stop, parent aborted), `a_secret_bearing_reason_is_other`,
  `a_family_round_trips_and_verifies`, `a_changed_byte_fails_verification`,
  `read_verified_skips_staging_and_lock_names`,
  `runs_view_drops_archived_runs_that_have_a_directory`. Run `cargo test
  -p apb-engine --lib retention::archive`.
- [x] 4.6 `retention/mod.rs` `plan`: containment and the keep rules 1 to
  12 in order with the bottom-up `family_kept` (design D3 "The plan").
  Make `previous_success::find` (line 33) `pub(crate)` if it is not.
  Unit tests first, one per rule: `keeps_an_unreadable_run`,
  `keeps_a_live_run`, `keeps_the_newest_runs`, `keeps_the_latest_success`,
  `keeps_a_pending_follow_up`, `keeps_a_graded_run`,
  `keeps_an_open_candidate_trial`, `keeps_an_unknown_send`,
  `keeps_a_start_key_run`, `keeps_a_concurrency_run`,
  `keeps_every_ancestor_of_a_kept_grandchild`, `keeps_a_young_run`,
  `removes_an_old_success`, `removes_a_parent_with_its_children`,
  `marks_a_linked_entry_unsafe`, `marks_a_file_entry_unsafe`,
  `marks_a_parent_with_an_unsafe_child_id_unsafe`,
  `marks_a_parent_whose_child_names_another_parent_unsafe`. Run `cargo
  test -p apb-engine --lib retention`.
- [x] 4.7 `retention::apply` and `archive::recover`: the prune lock,
  recovery, then per family the event locks, the `PruneFamilyLocked`
  barrier, the recheck of rules 1, 2 and 5 to 10 for every member, the
  archive lock with steps 2.1 to 2.4 and the `PruneAfterStage` barrier,
  and step 4 outside the locks; refuse a linked `.apb/runs-archive`.
  Unit tests first: `apply_removes_a_family_and_leaves_one_summary`,
  `recover_finishes_a_committed_removal`,
  `recover_undoes_a_staged_family`,
  `recover_deletes_a_summary_whose_runs_are_still_there`,
  `a_held_prune_lock_refuses_apply`. Run `cargo test -p apb-engine --lib
  retention` and `cargo test -p apb-engine --test main retention_test`
  (all pass except test 11, which needs health facts and passes in 5.8).
- [x] 4.8 `run_stats::build` uses `archived`: filtering, union groups,
  `ArchivedCounts` and `VersionStats.archived`
  (`crates/apb-engine/src/run_stats.rs` line 199), the note only when
  neither side matched, and the `archived:` line in `render_text`; the
  stats callers (`apb stats`, MCP `playbook_stats`, `GET /api/stats`)
  pass the combined view's archived runs. Register `ArchivedCounts` in
  `ts_contract.rs` and regenerate TypeScript. Run `cargo test -p
  apb-engine --lib run_stats` (test 17 passes) and `cargo test -p apb
  --test main stats_cli_test`.
- [x] 4.9 CLI: create `crates/apb-cli/tests/suite/prune_cli_test.rs`
  first: `prune_without_yes_writes_nothing` (the output names `would be
  removed`, the run directory and `.apb/runs-archive` are unchanged),
  `prune_yes_in_a_run_step_is_refused` (`APB_RUN_ID=x`, exit 2, nothing
  removed), `prune_yes_removes_and_reports` (exit 0, `removed 1 runs`),
  `apb_runs_with_a_run_id_still_works` (`apb runs <id>` prints the run);
  they fail to find the subcommand. Then create `crates/apb-cli/src/prune.rs`
  with `RunsAction { Prune(PruneArgs) }`, `PruneArgs { yes, json,
  playbook }` and `prune_cmd` (design D3 "Command");
  `crates/apb-cli/src/main.rs` `Command::Runs` (line 365) gets the
  attribute and the `action` field, every existing `Command::Runs` arm
  gets `action: None`, and the new arm goes first. Run `cargo test -p
  apb --test main prune_cli_test`.
- [x] 4.10 Docs: `docs/STATS.md` "Pruning runs" (what is kept and why,
  families, the archive, the `archived` field, one prune at a time);
  `docs/CI.md` the weekly prune line.
- [x] 4.11 Section check: `cargo test -p apb-engine --lib retention`,
  `cargo test -p apb --test main prune_cli_test`, `cargo test -p apb
  --test main stats_cli_test` pass.

## 5. Routine health (design D2, spec `routine-health`)

### 5A. Core

- [x] 5.1 `crates/apb-core/src/health.rs`: the rest of design D2 "Schema"
  and "The facts a rule reads" (`HealthBlock`, `FailureRate`, `OnTrip`,
  `Rule`, `DEFAULT_COOLDOWN`, `RunFact`, `RuleVerdict`, `evaluate`).
  Tests first: `consecutive_failures_trips_on_n`,
  `consecutive_failures_needs_n_in_a_row`,
  `failure_rate_trips_at_the_threshold`, `failure_rate_waits_for_min_runs`,
  `overdue_measures_from_the_last_success`,
  `overdue_gives_grace_from_its_reference`,
  `without_counted_since_every_fact_counts`,
  `facts_before_counted_since_do_not_count`,
  `ignored_facts_do_not_break_a_streak`. Then `Playbook.health` after
  `concurrency` and `health: None` in every literal (test first in
  `schema.rs`: `a_playbook_reads_its_health_block`). Run `cargo test -p
  apb-core --lib health` and `cargo test -p apb-core --lib schema`.
- [x] 5.2 V212 in `validate/health.rs` (design D2 "Schema"). Tests first
  in that file: `v212_no_rule`, `v212_consecutive_failures_out_of_range`
  (0 and 101), `v212_at_least_out_of_range` (0 and 1.5),
  `v212_window_out_of_range`, `v212_min_runs_over_window`,
  `v212_overdue_not_a_duration`, `v212_overdue_under_ten_minutes`,
  `v212_cooldown_over_ninety_days`, `a_full_block_is_valid`. Run `cargo
  test -p apb-core --lib validate::health`.
- [x] 5.3 `policy_file.rs`: tests first in a new `health_tests` module:
  `an_empty_list_is_valid_and_watches_nothing`,
  `an_absent_list_watches_every_playbook`, `the_project_narrows`,
  `an_unknown_health_key_is_refused`,
  `a_global_entry_matches_a_global_playbook`. Then `HealthSection`,
  `PolicyFile.health`, `Policies::health_watches`, the `summary` line.
  Run `cargo test -p apb-core --lib policy_file`.
- [x] 5.4 `crates/apb-core/src/notify/mod.rs`: tests first in
  `notify/mod.rs`: `health_is_in_all_and_default`,
  `health_parses_and_prints`. Then `Transition::Health` (line 25), `ALL`
  (line 48, 9 entries), `DEFAULT` (line 63, 5 entries), its doc; fix
  every exhaustive match the compiler lists in all crates. Regenerate
  TypeScript. Run `cargo test -p apb-core --lib notify` and `cargo test
  -p apb-engine --test main notify_transitions_test`.

### 5B. Engine skeleton and red tests

- [x] 5.5 Skeleton that compiles: `crates/apb-engine/src/health/mod.rs`,
  `health/state.rs`, `health/facts.rs` (`pub mod health;` in `lib.rs`)
  and `crates/apb-engine/src/notify/health.rs` (`pub mod health;` in
  `notify/mod.rs`) with every type and signature of design D2 ("The
  state file and its ledger", "One evaluation", "Sending", "Surfaces":
  `HealthView`, `HealthRuleView`, `HoldView`) and bodies that do
  nothing: `evaluate` returns a `HealthView` with `status: "ok"`, `view`
  returns `Ok(None)`, `hold_refusal` returns `Ok(())`, `clear` and
  `mute` return `Ok(())`, `sweep` returns an empty `Vec`,
  `observe_ended` returns at once, `facts` returns an empty `Vec`,
  `health_target` returns `None`, `send` returns
  `SendOutcome::NotSent("not built".into())`, `doctor_line` returns
  `None`. `cargo build -p apb-engine --features testing`.
- [x] 5.6 Red tests: create `crates/apb-engine/tests/suite/health_test.rs`
  with the notify connector stand-in (an `Env` like `notify_send_test.rs`
  line 38 over `common::ScriptedHttpServer`), design D2 tests 1 to 22,
  the engine test of 23, and design D1 test 11
  (`a_blocked_notifier_never_holds_the_slot`, which needs a watched
  playbook and passes after 5.13). Create the other three tests of item 23 in
  their crates: `a_held_playbook_is_refused_over_mcp` in
  `crates/apb-mcp/tests/suite/run_tools_test.rs`,
  `a_held_playbook_is_refused_over_http` in
  `crates/apb-server/tests/suite/launch_api_test.rs`, and
  `a_held_playbook_is_refused_by_apb_run` in a new
  `crates/apb-cli/tests/suite/health_cli_test.rs` (declared in that
  crate's `tests/main.rs`); each seeds the hold by writing a signed
  `HealthFile` through `transact`. Run `cargo test -p apb-engine --test
  main health_test`, `cargo test -p apb-mcp --test main run_tools_test
  a_held_playbook`, `cargo test -p apb-server --test main
  launch_api_test a_held_playbook` and `cargo test -p apb --test main
  health_cli_test`; every new test fails except
  `a_child_run_end_is_not_evaluated`, `a_person_stop_is_not_a_failure`,
  `superseded_runs_are_not_failures` and
  `an_unwatched_machine_sends_nothing`, which pass against the skeleton
  and must keep passing.

### 5C. Engine, one piece at a time

- [x] 5.7 `crate::run_history::operational_runs_of`
  (`crates/apb-engine/src/run_history.rs`, next to `all_runs_of` line
  144). Test first in that file's `mod tests`:
  `operational_runs_include_external_data_and_exclude_eval` (seed with
  the file's `seed` helper: one `Production`, one `ExternalData`, one
  `Eval` run; two are returned). Run `cargo test -p apb-engine --lib
  run_history`.
- [x] 5.8 `health/facts.rs` `facts` over `runs_view`, and the archive's
  `class` written by `retention::apply` from `class_of`. Unit tests
  first: `facts_read_directories_and_summaries_once`,
  `facts_skip_children_and_other_scopes`,
  `facts_classify_a_deadline_abort_as_failed`,
  `facts_skip_an_unreadable_journal`,
  `facts_skip_a_run_without_provenance`,
  `an_archived_class_is_read_not_derived`. Run `cargo test -p
  apb-engine --lib health::facts` and `cargo test -p apb-engine --test
  main retention_test` (test 11 now passes).
- [x] 5.9 `health/state.rs`: `HealthFile` with `impl SignedDoc`,
  `RuleState`, `Hold`, `NoticeRecord`, the per-process nonce. Unit tests
  first: `a_fresh_file_round_trips_signed`,
  `a_changed_byte_reads_unverified_under_refuse`,
  `the_ledger_keeps_fifty_dropping_final_records_first`. Run `cargo test
  -p apb-engine --lib health::state`.
- [x] 5.10 `crate::connector::call::play_call_pinned`
  (`crates/apb-engine/src/connector/call/mod.rs`, next to `play_call`
  line 1033); `notify/health.rs` `health_target`, `SendOutcome`, `send`
  (design D2 "Sending"); `health_notice` in
  `crates/apb-engine/src/notify/notice.rs`. Unit tests first in
  `notify/health.rs`: `classify_maps_each_outcome` (an ok answer, an ok
  answer with `truncated: true`, a `timeout` error, a `service` error
  with `http_status: 500`, an `http_status: 400` error, a `permission`
  error: `Sent`, `Sent`, `Unknown`, `Unknown`, `NotSent`, `NotSent`),
  `the_notice_names_the_clear_command_when_held`. Run `cargo test -p
  apb-engine --lib notify::health`.
- [x] 5.11 `health/mod.rs`: `evaluate` phases 1 to 3 with the notice
  state table, the `HealthBeforeDispatch` and `HealthBeforeSettle`
  barriers, `sweep`, `view`, `hold_refusal`, `clear`, `mute`,
  `doctor_line`, and `observe_ended` with its early returns. Unit tests
  first (no connector: no target configured):
  `a_trip_is_recorded_without_a_target`,
  `hold_is_set_on_trip_with_on_trip_hold`,
  `clear_drops_the_hold_and_starts_over`,
  `clear_and_mute_raise_the_generation_and_retire_open_notices`,
  `mute_suppresses_the_trip_notice`, `an_unverified_file_reads_held`,
  `view_writes_nothing`, `the_first_evaluation_counts_every_fact`. Run
  `cargo test -p apb-engine --lib health`, then `cargo test -p
  apb-engine --test main health_test` (all pass except 7, 20 and 23,
  which need 5.12 and 5.13).
- [x] 5.12 The hold: in `gate/mod.rs` `check_run` after line 729; in
  `prepare_placed` step 1 of design D1 "In preparation" (the hold check
  next to `key_for_start`, design D2 "Hold" item 2); the
  `playbook_on_hold` arm in `next_action/start.rs` `policy_actions`
  (line 220). Test first in `next_action/start.rs` tests:
  `a_hold_refusal_offers_the_clear_command`. Run `cargo test -p
  apb-engine --lib next_action`, `cargo test -p apb-engine --test main
  health_test` (7 and the engine test of 23 pass), `cargo test -p
  apb-mcp --test main run_tools_test a_held_playbook` and `cargo test -p
  apb-server --test main launch_api_test a_held_playbook` (pass).
- [x] 5.13 The evaluation point of design D2 "Evaluation points" item 1:
  in `EventLog::append_inner` (`event.rs` line 2808), after the line is
  written and the journal lock is released, call
  `crate::health::observe_ended(&dir)` for a `RunFinished` or
  `RunAborted` payload when `self.run_dir()` is `Some`. Run `cargo test
  -p apb-engine --test main health_test` (all pass) and `cargo test -p
  apb-engine --test main notify_send_test`.

### 5D. Surfaces

- [x] 5.14 CLI: add to `crates/apb-cli/tests/suite/health_cli_test.rs`
  first: `check_exits_3_on_hold`, `check_exits_0_when_tripped_but_not_held`,
  `clear_in_a_run_step_is_refused` (`APB_RUN_ID=x`, exit 2, the hold
  stays), `mute_until_silences_notices`, `doctor_names_tripped_playbooks`,
  `health_without_a_block_says_unwatched`, `stats_shows_health_without_runs`
  (an overdue watched playbook with no run: `apb stats` prints its
  `health:` line). Then create `crates/apb-cli/src/health.rs` with
  `HealthArgs` and `health_cmd` (design D2 "Commands"); `Command::Health`
  in `crates/apb-cli/src/main.rs`; the sweep lines in `resume_all_cmd`
  (`crates/apb-cli/src/run.rs` line 2733); the `health` and
  `concurrency` lines in `doctor_env` after line 158;
  `StatsReport.health`, filled by the stats callers, and its lines in
  `render_text` before the note. Run `cargo test -p apb --test main
  health_cli_test` and `cargo test -p apb-engine --lib run_stats`.
- [x] 5.15 Dashboard sweep: make `apb_engine::env_duration::test_override`
  `pub`; tests first in a new `crates/apb-server/src/health_sweep.rs`:
  `sweep_once_evaluates_a_watched_playbook`,
  `the_period_defaults_to_ten_minutes`; then `HEALTH_SWEEP_ENV =
  "APB_HEALTH_SWEEP_MS"`, `period()`, `sweep_once(roots: &[PathBuf],
  now_ms: u64) -> usize` (the number of playbooks evaluated) and
  `spawn()`; call `health_sweep::spawn()` in `run_server` after line
  404. Run `cargo test -p apb-server --lib health_sweep`.
- [x] 5.16 Tests first: `the_playbook_list_carries_health` in
  `crates/apb-server/tests/suite/api_test.rs`; `the_catalog_carries_health`
  and `an_unchanged_catalog_names_held_playbooks` in
  `crates/apb-mcp/tests/suite/catalog_tools_test.rs`;
  `stats_list_playbooks_not_ok` in `crates/apb-engine/src/run_stats/tests.rs`;
  `playbook_stats_carries_health` in
  `crates/apb-mcp/tests/suite/stats_tool_test.rs`. Then `GET
  /api/playbooks` `health` (`routes/playbooks.rs` `list_playbooks`, line
  770); MCP `add_health` and `held` (`crates/apb-mcp/src/catalog.rs`,
  lines 293 and 326). Run `cargo test -p apb-server --test main
  api_test`, `cargo test -p apb-mcp --test main catalog_tools_test`,
  `cargo test -p apb-engine --lib run_stats` and `cargo test -p apb-mcp
  --test main stats_tool_test`.
- [x] 5.17 Register `HealthView`, `HealthRuleView`, `HoldView` in
  `ts_contract.rs` and regenerate TypeScript.
- [x] 5.18 Web: tests first in `web/src/lib/playbooklist.test.ts`
  `healthBadge names a hold before a trip`, and in
  `web/src/lib/notify.test.ts` `the run dialog never offers health` and
  `onWords names health`. Then `PlaybookSummary.health?` in
  `web/src/lib/types.ts` (line 15); `healthBadge(entry)` in
  `web/src/lib/playbooklist.ts` returning `'on hold' | 'tripped' |
  null`; the badge in `web/src/pages/PlaybookList.svelte`; `onWords` and
  `RUN_TRANSITIONS` in `web/src/lib/notify.ts` (design D2 "Surfaces"
  item 4). Run `cd web && bun run test && bun run check`. Plain CSS
  only.
- [x] 5.19 Docs: `docs/HOWTO-authoring.md` `## health (a routine across
  runs)` after the deadline section with the V212 row (including the
  first evaluation, the notice states in words, cooldown across a
  recovery, and a preparation failure before provenance);
  `docs/NOTIFY.md` the transition, its target rule and the run-level
  note; `docs/CI.md` "Before a start: apb health check"; `docs/STATS.md`
  the `health` field.
- [x] 5.20 Section check: `cargo test -p apb-core --lib health`,
  `cargo test -p apb-engine --lib health`,
  `cargo test -p apb-engine --test main health_test`,
  `cargo test -p apb-engine --test main retention_test`,
  `cargo test -p apb-engine --test main notify_send_test`,
  `cargo test -p apb --test main health_cli_test`,
  `cargo test -p apb-mcp --test main catalog_tools_test`,
  `cargo test -p apb-mcp --test main run_tools_test`,
  `cargo test -p apb-server --test main launch_api_test` pass.

## 6. Records

- [x] 6.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both
  (only CLAUDE.md's "Claude-specific" section has no twin): in the
  `apb-core` list `concurrency.rs`, `health.rs`, `validate/concurrency`
  and `validate/health` (V210 to V212), the `policy_file` keys
  `runs.keep_*` and section `health`; in the `apb-engine` list
  `concurrency.rs`, `health/`, `retention/`, `notify/health.rs` and the
  review channel's torn-tail rule in `review.rs`; in the CLI list
  `health.rs` and `prune.rs`; in the server list `health_sweep.rs`; the
  "Routine operations" paragraph extended (the key after the start key
  and before the tree for starts and resumes, the provisional claim,
  `concurrency_own_run`, the signed slot and health files, the health
  ledger's claim, what prune never removes and its commit order).
- [x] 6.2 Write `docs/superpowers/specs/<implementation date>-routine-health-design.md`:
  the content of `proposal.md` and `design.md` as one public record,
  with no mention of `openspec/` and no reference to any outside
  article, author or project.
- [x] 6.3 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![`
  and Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>`
  finds nothing).

## 7. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 7.1 `cargo fmt --all -- --check`
- [x] 7.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 7.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 7.4 `cargo test --workspace --doc`
- [x] 7.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; the generated files are current)
- [x] 7.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 7.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 7.8 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 7.9 `openspec validate routine-health --strict`

## 8. Close

- [x] 8.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f __pause-waker` and `pgrep -f __drive`).
- [x] 8.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to
  0.1 and 0.2, each gate of section 7 with its numbers, the
  user-visible changes for the release notes (design "Release notes"),
  the changed file count, and anything left open.
