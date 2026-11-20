# Tasks: wait park

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec requirement it
names (`specs/wait-park/spec.md`). Tick each box (`- [x]`) as soon as the
task is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by the
name `design.md` gives, keep the decision, and note the difference in the
final report. Never invent a different behaviour. If the code shows that
the park cannot be the two lines of design D1 (post the engine pause,
`continue`) plus the scan rule of D6, the waker check of D7 and the
numbering lock of D8, without other new hand-over, lock or control code,
stop and report why with file:line.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first failure
only for a test that names a type or function that does not exist yet.

Line numbers are those of main e76aa42e (after "People in the loop",
#288). If a change merged since moved a line, find the site by its name.

This change adds no journal event type, no validation rule, no state
file, no lock function and no waker body. Its one event field is
`notify_set.remind_ms` of the reminders (design D9, section 4a). Its one new
lock is the file name `control.jsonl.lock` taken through the existing
`apb_core::fsutil::hold_lock` (design D8). Its one journal addition is
the run guard feature `wait_park` (design D4), written through the
existing run guard path. Its only control handling is design D6, D7 and
D8, in four files: the park arm of the scan and the discard at every
exit of a pass (`scheduler/control_apply.rs`), the stop watcher's Pause
check (`stop.rs`, one guard and one test), the check on each poll of the
pause waker (`pause/waker.rs`), and in `control.rs` the numbering lock
of `post_control` and `pending_pause` skipping a park. Sections 3a and
3b are the only sections that touch them, and only as D6, D7 and D8
say. It must not change `workdir.rs`, `driver.rs`,
`scheduler/deferred.rs`, `scheduler/resume.rs`, `pause/request.rs`,
`pause/resume_all.rs`, `run_view.rs` or `deadline_hooks.rs` (design
"Code map"). If a task seems to need one of them, stop and report it.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-wait-park-target TMPDIR=$HOME/.cache/apb-wait-park-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-wait-park-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-wait-park-target`
and `TMPDIR=/tmp/apb-wait-park-tmp` instead, and before each build check
that `df -h /tmp` shows more than 4G free; if not, stop and report it.
`/tmp` is a RAM disk shared with other sessions. Never use `/dev/shm`,
and never put a target dir, cache or temp data inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Never export `APB_TEST_BILLING_DEFAULT` in a shell; the
test helpers set what they need. Do not commit, push or open a pull
request. Do not start subagents. Search code with `zg --rg -F <symbol>
<path>` before opening a file.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-engine --lib pause::clock`).
2. An integration test file: `cargo test -p <crate> --test main <file module>`
   (for example `cargo test -p apb-engine --test main wait_test`).
3. `cargo test` takes one name filter per invocation: two modules are
   two commands.

The engine integration tests go into the existing
`crates/apb-engine/tests/suite/wait_test.rs` (no new test file, no change
to `tests/main.rs`) and use the shared helpers through `use
crate::common;` (see
`crates/apb-engine/tests/suite/deferred_wake_test.rs` for
`common::EnvVar`, `APB_RELEASE_AFTER_MS` and a drive in a thread, and
`crates/apb-engine/tests/suite/operator_pause_test.rs` for a write run,
`request_pause` and `resume_all`).

After the change to `PauseCause` regenerate TypeScript:
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then
`cargo test -p apb-server --lib ts_contract` (must pass without the
variable).

Web work uses plain CSS over the existing CSS variables only; this
change needs no CSS.

## 0. Preconditions

- [x] 0.1 Check these exist (one `zg --rg -F` each; note the line):
  `pub(crate) fn may_release` in `crates/apb-engine/src/scheduler/deferred.rs`;
  `pub(crate) fn post_engine_pause` in `crates/apb-engine/src/scheduler/journal.rs`;
  `pub fn read_control_after`, `pub fn post_control` and
  `pub fn pending_pause` in `crates/apb-engine/src/control.rs`;
  `pub fn hold_lock` in `crates/apb-core/src/fsutil.rs`;
  `pub(crate) fn scan_control` in `crates/apb-engine/src/scheduler/control_apply.rs`;
  `pub(crate) fn after_pause` in `crates/apb-engine/src/pause/request.rs`;
  `pub keep_lock: bool` in `crates/apb-engine/src/pause.rs`;
  `fn paused_fold` in `crates/apb-engine/src/pause/clock.rs`;
  `pub fn cause_of` in `crates/apb-engine/src/pause/cause.rs`;
  `pub fn run_waker` and `fn resume_while` in `crates/apb-engine/src/pause/waker.rs`;
  `pub(crate) fn try_release` in `crates/apb-engine/src/scheduler/deferred.rs`
  (the event lock taken in the drive loop: the precedent of D6 rule 3);
  `if entries.iter().any(|e| matches!(e.cmd, Control::Pause(_)))` in
  `crates/apb-engine/src/stop.rs` (the watcher's Pause check);
  `pub const EVENT_LOCK` in `crates/apb-engine/src/run_lock.rs`;
  the wait branch `NodeKind::Wait {` in `crates/apb-engine/src/scheduler.rs`;
  `fn latest_run_guard` in `crates/apb-engine/src/event.rs` (it was
  `patch::latest_guard` before #289 moved it);
  `fn run_guard_of` in `crates/apb-engine/src/scheduler/prepare.rs`;
  `pub enum GuardedFeature` and `pub fn guarded_features` in
  `crates/apb-core/src/validate/guarded.rs`;
  for the reminders (design D9): `pub struct NotifyPolicy` and
  `fn validate` in `crates/apb-core/src/policy_file.rs`;
  `pub struct Effective` in `crates/apb-core/src/notify/precedence.rs`;
  `pub fn due`, `fn open_transitions` and `pub struct TransitionId` in
  `crates/apb-engine/src/notify/transitions.rs`; `fn record_unsent` and
  `fn sweep_within` in `crates/apb-engine/src/notify/send.rs`;
  `pub(crate) fn append_decided_synced` in
  `crates/apb-engine/src/event.rs`; `fn decide` in
  `crates/apb-engine/src/connector/call/send_once.rs`; `pub fn notify`
  of `CallContext` in `crates/apb-engine/src/connector/call/request.rs`;
  `NotifySet {` in `crates/apb-engine/src/event.rs`.
  If one is missing, stop and report it. If before-running-alone landed
  first, `GuardedFeature::ALL` ends with `ListPilot` (12 entries): this
  change makes it 13 with `WaitPark` last and names the count test
  `all_has_thirteen_entries_in_order`, and `policy_file.rs` has an
  `unattended` section: add `notify.remind` beside it without touching
  it.
- [x] 0.2 None of these exists: `crates/apb-engine/src/scheduler/wait_park.rs`,
  `PauseCause::Wait`, `WAIT_PREFIX`, `GuardedFeature::WaitPark`,
  `remind_ms`, `still_due`, `crates/apb-engine/tests/suite/notify_remind_test.rs`. If one
  exists, it is a partial earlier attempt: review it against `design.md`.
- [x] 0.3 None of `scheduler/wait_release.rs`, `DriveReleased.waits`,
  `pause/recover.rs`, `fn try_park`, `fn park_in_place`, `fn take_back`
  or `fn commit_terminal` exists on this tree (earlier attempts are not
  merged). If one exists, stop and report it.
- [x] 0.4 A run that never parks writes the same journal as main (design
  "Tests" 19 and 20; spec "A run that can park is guarded"). Before any
  other code change, on the unchanged tree, add rows W1 and W2 to
  `crates/apb-cli/tests/suite/journal_corpus_test.rs`, with their
  playbooks inline as rows D11 to D13 do (no new fixture file), and a
  `//! Rows W1 and W2` paragraph in the module doc:
  1. `w1_a_run_without_a_timer_wait_journals_as_main`: the new binary
     runs a script node, then a webhook wait with `timeout_seconds: 1`
     and no signal, routed on by `fallback: true` to a finish (the
     shape of `WF_TIMEOUT` in `crates/apb-engine/tests/suite/wait_test.rs`),
     to its end. The row asserts: the journal holds no `run_guard`
     line, no `run_paused` and no `run_resumed`; the run has no
     `control.jsonl` entry (the file is absent or empty); and the journal's sequence of event `type`s (seq, ts,
     ids, paths and digests left out) equals a literal list in the row.
     Write the literal from what this unchanged tree journals, so the
     row is main's journal by construction. When the old binaries are
     present, both read the run (`apb runs <id>` exits 0 and names its
     outcome).
  2. `w2_a_short_timer_wait_adds_only_its_guard`: the same with a timer
     wait of `seconds: 1` (under `release_after`, so it never parks).
     The row asserts: no `run_paused`, no `run_resumed`, no
     `control.jsonl` entry, and the journal with every `run_guard` line removed
     has the literal sequence of event types this unchanged tree writes.
     Task 1.3 then adds the assertion that the journal holds `run_guard`
     lines, each with `requires` exactly `["wait_park"]`, one right
     after `run_provenance` and one right after every checkpoint, and
     nowhere else (design D4: the accepted cost of a timer wait that
     cannot park); that assertion fails until 1.3 is done.
  Run both rows the CI way (gate 6.12) on the unchanged tree: they must
  pass before any other change. The literal lists never change after
  this task; if a later task makes W1 or W2 fail on them, the change
  altered a journal it must not alter: fix the code, not the list.

## 1. The cause and the guard (design D2, D4; spec "Surfaces show a park as a wait", "A run that can park is guarded", "An older apb does not resume a parked run")

- [x] 1.1 Tests first, a new `#[cfg(test)] mod tests` in `crates/apb-engine/src/pause/cause.rs`:
  `a_wait_reason_is_a_wait_only_from_the_engine` (an engine `wait (pause)`
  and an engine `wait (a, b (c))` are `Wait`; the same reasons by
  `Operator` or `Supervisor` are `None`; an engine `wait (a/b)` is
  `None`; `PauseRequest::is_park` in `pause.rs` agrees for the same
  requests, its test `a_park_request_is_the_engines_wait_reason`),
  `parse_wait_reason_refuses_an_unsafe_node`,
  `parse_wait_reason_round_trips_any_valid_node` (`parse_wait_reason(&wait_reason(n))`
  is `Some(n)` for `w`, `a, inbox`, `x (y)`, `a)` and `a, b (c)`;
  `wait ()`, `wait (w` and `wait (w) x` are `None`). Then
  `PauseCause::Wait` with `as_str` "wait" in `pause/kind.rs` (after
  `Billing`), and in `cause.rs` `WAIT_PREFIX`, `wait_reason`,
  `parse_wait_reason` (design D2 3: the fixed prefix, the final `)`, the
  node verbatim between, a safe segment; one form only), and
  `PauseRequest::is_park` in `pause.rs` (design D2 4), the arm in `cause_of` after billing; the module doc
  names the wait. Run `cargo test -p apb-engine --lib pause::cause`.
- [x] 1.2 Regenerate TypeScript (see above); `web/src/lib/api.gen.ts`
  gains `"wait"` in `PauseCause`.
- [x] 1.3 The run guard (design D4). Tests first,
  `crates/apb-core/src/validate/guarded.rs` `mod tests`:
  `a_timer_wait_is_a_guarded_feature` (a definition with a `wait` whose
  `wait_for` is a timer gives `[WaitPark]`; one with only a webhook wait
  gives `[]`), the count in `a_deadline_is_a_guarded_feature` becomes
  13, and `all_has_twelve_entries_in_order` becomes
  `all_has_thirteen_entries_in_order` asserting `ListPilot` twelfth and
  `WaitPark` last. Then
  `GuardedFeature::WaitPark` (`as_str` "wait_park", last in `ALL`, after
  `ListPilot`, which becomes `[GuardedFeature; 13]`), the arm in
  `guarded_features`, and one sentence in the module doc. Do not change
  `scheduler/prepare.rs` or `scheduler/patch.rs`: `run_guard_of` already
  passes the feature through (check that it filters only `Deadline` and
  `Concurrency`). Run `cargo test -p apb-core --lib validate::guarded`
  and `cargo test -p apb-engine --lib scheduler::prepare`. Then add the
  guard assertion of row W2 (task 0.4 2) and run the corpus the CI way
  (gate 6.12): W1 passes unchanged, W2 passes with its guard lines.

## 2. The clock (design D3; spec "A park is not paused time", "A park is not an intervention")

- [x] 2.1 Tests first, a new `#[cfg(test)] mod tests` in
  `crates/apb-engine/src/pause/clock.rs`: `a_park_is_not_paused_time` (an
  engine pause with reason `wait (a, b (c))` from 100 to 600: `paused_intervals`
  is empty, `active_elapsed_ms(0, 1000)` is 1000; the same with reason
  `quota (a/m)` still gives `[(100, 600)]`),
  `a_person_pause_over_a_park_counts_from_its_ts` (park at 100, operator
  `run_paused` at 300, resume at 700: `[(300, 700)]`),
  `a_park_counts_toward_the_deadline` (`deadline_elapsed_ms` includes the
  park). Then `paused_fold` with a segment struct `{ start, end, by, park }`
  and `paused_intervals` returning the non-park spans merged where
  adjacent. `deadline_elapsed_ms` keeps subtracting only `operator`
  segments. The doc comment of `pause/clock.rs` lines 1 to 5 gains the
  sentence of design D3. Run `cargo test -p apb-engine --lib pause` and
  `cargo test -p apb-engine --lib next_action::deadlines`.
- [x] 2.2 The intervention counts (design D3 "The intervention counts").
  Tests first, `crates/apb-engine/src/run_stats/interventions.rs`
  `mod tests`: `a_park_and_its_automatic_resume_are_no_intervention`.
  Then `interventions` (line 296) keeps the open pause's cause while it
  folds, skips a `run_paused` by `Engine` with cause `Wait`
  (`pause::cause::cause_of`) and a `run_resumed` by `Engine` whose open
  pause is such a park; a resume by `Operator` or `Supervisor` is
  counted as today. Do not change `run_retro.rs` or `run_retro/text.rs`:
  they read the same fold. Run
  `cargo test -p apb-engine --lib run_stats::interventions` and
  `cargo test -p apb-engine --lib run_retro`.

## 3. The park (design "The quota pause on main, traced", D1, D5; spec "A long timer wait posts an engine pause", "A park holds no driver", "A parked run wakes when its wait is due", "Resuming a parked run")

- [x] 3.1 Trace the quota pause on this tree with the table of design
  "The quota pause on main, traced" (steps 1 to 11) and the budget
  gate's two lines (`scheduler.rs` 1204 to 1206). Confirm that
  `PauseRequest.keep_lock` is journaled by the scan's Pause arm and
  honoured by `after_pause`. Note any line that moved in the report.
- [x] 3.2 Tests first, `crates/apb-engine/src/scheduler/wait_park.rs`
  (new, declared next to `mod deferred;` in `scheduler.rs`) with a
  `#[cfg(test)] mod tests`: `left_ms_takes_the_earlier_of_timer_and_timeout`,
  `left_ms_saturates`, `the_due_time_moves_only_with_paused_time`
  (design "Tests", unit 1), `a_long_timer_wait_asks_for_a_park` (reason `wait
  (<node>)`, `by: Engine`, `Drain`, `until` the given `wake_at_ms`,
  `keep_lock` as given: the caller passes `lock.is_some()`, design D1), `a_webhook_wait_never_parks`,
  `a_scenario_wait_never_parks`, `a_wait_beside_other_work_never_parks`,
  `a_pending_control_blocks_the_park`,
  `a_run_that_may_not_release_never_parks`,
  `an_unguarded_run_never_parks`. Then `left_ms`, `due_at_ms` (over
  `scheduler::journal::wait_active_ms` with one `now`), `ParkCheck` and
  `park_request` (design D1). Run
  `cargo test -p apb-engine --lib scheduler::wait_park`.
- [x] 3.3 Red integration tests in `crates/apb-engine/tests/suite/wait_test.rs`
  (design "Tests" 1 to 8, ten tests): `a_long_timer_wait_parks_and_wakes_when_due`,
  `a_short_wait_keeps_its_driver`, `a_webhook_wait_keeps_its_driver`,
  `an_unattended_run_keeps_its_driver`,
  `a_run_without_the_guard_keeps_its_driver`,
  `an_early_resume_parks_again`, `a_pause_during_the_park_freezes_the_wait`,
  `a_pending_park_is_applied_by_the_next_drive`,
  `a_read_only_wait_parks_without_the_lock`,
  `a_shared_workdir_wait_parks_without_the_lock`. Use
  `APB_RELEASE_AFTER_MS=300`, `APB_WAKER_POLL_MS=50` and a timer of 3
  seconds; a write run (a script node before the wait). Run
  `cargo test -p apb-engine --test main wait_test` and see the park
  tests fail on an assertion.
- [x] 3.4 The call in the wait branch (design D1 code): compute
  `wake_at_ms` with `wait_park::due_at_ms` and one `now`, call
  `park_request`, and on `Some` call
  `journal::post_engine_pause` and `continue`; otherwise sleep as
  today. Nothing else in the drive changes. Run
  `cargo test -p apb-engine --test main wait_test`, then
  `cargo test -p apb-engine --test main operator_pause_test`,
  `cargo test -p apb-engine --test main quota_pause_test` and
  `cargo test -p apb-engine --test main deferred_wake_test`.
- [x] 3.5 The end-to-end test with real processes (design "Tests" 18), in
  `crates/apb-cli/tests/suite/pause_waker_cli_test.rs`:
  `a_parked_wait_holds_no_driver_and_its_waker_resumes_it`. After the
  park's `run_paused` appears, poll every 50 ms for at most 10 s until
  the settled state holds: `driver.pid` gone, no process whose command
  line holds `__drive-run` and the run id (`pgrep -f`), and the waker's
  lease held (`apb_engine::waker::Lease::held(run_dir,
  WakerFor::Pause(<park seq>))`); on a timeout fail with the state seen
  last. Only then assert exactly one `__pause-waker` process for the run
  (the expected waker, named as such in the assertion message); then the
  run is resumed by `engine` no earlier than `until` and ends
  `succeeded`. Run `cargo test -p apb --test main pause_waker_cli_test`.

## 3a. One numbering of control entries (design D8, D6 "`pending_pause`"; spec "Control entries are numbered in append order", "An older apb's control writes are not ordered")

- [x] 3a.1 Before any edit, list every caller of `post_control`
  (`zg --rg -F "post_control(" crates`) and, for each, whether it holds
  the run's event lock when it posts (design D8 "Lock order" names
  `pause/request.rs` 73 and 88, `stop.rs` 194 to 214 and 335). Confirm
  that no code takes any lock while holding `CONTROL_LOCK` and that
  nothing but `post_control` writes `control.jsonl` in `crates/*/src`
  (tests that write a raw line do not count). Put the list in the
  report. If a caller takes a lock after posting while still inside a
  section that another path enters in the other order, stop and report
  it.
- [x] 3a.2 Tests first, `crates/apb-engine/src/control.rs` `mod tests`:
  `two_concurrent_posts_get_distinct_increasing_numbers` (design "Tests",
  unit 13). Run `cargo test -p apb-engine --lib control` a few times on
  main's `post_control` and note in the report whether it failed (it is
  a race; a pass on main is no proof). Then `pub const CONTROL_LOCK:
  &str = "control.jsonl.lock";` and `post_control` holding
  `apb_core::fsutil::hold_lock(run_dir, CONTROL_LOCK)?` across the count
  and the append, the line and its newline written with one
  `write_all`. Nothing else in `post_control` changes. Run
  `cargo test -p apb-engine --lib control`.
- [x] 3a.3 Tests first, `control.rs` `mod tests`:
  `a_pending_park_is_not_a_pending_pause`. Then the `find_map` of
  `pending_pause` matches only `!crate::pause::cause::is_park(&req)`.
  `pending_stop` does not change. Run
  `cargo test -p apb-engine --lib control`, then
  `cargo test -p apb-engine --lib pause` (the `pausing` overlay test,
  `pause.rs` 646).
- [x] 3a.4 Run `cargo test -p apb-engine --test main operator_pause_test`,
  `cargo test -p apb-engine --test main control_liveness_test` and
  `cargo test -p apb-engine --test main approval_test` (callers of
  `post_control` that hold the event lock or post many entries).

## 3b. The park yields (design D6, D7; spec "A park yields to every other control", "A stale park is discarded", "No park is left behind the end of a pass", "The waker of a park wakes the run for a pending control", "A failed read does not end the waker of a park")

- [x] 3b.1 Red integration tests in `crates/apb-engine/tests/suite/wait_test.rs`
  (design "Tests" 9 to 17 and 21): `a_pause_after_the_park_is_applied_instead`,
  `a_stop_after_the_park_is_applied_instead`,
  `a_patch_before_the_park_discards_it`,
  `a_control_posted_in_the_exit_window_wakes_the_run_at_once`,
  `a_half_written_control_line_does_not_end_the_waker`,
  `a_note_posted_after_the_waker_settled_reaches_the_run`,
  `a_resume_after_the_patch_limit_pause_applies_no_stale_park`,
  `a_pause_queued_before_the_park_leaves_no_stale_park`,
  `a_park_for_another_node_is_discarded`,
  `a_park_pending_behind_a_direct_pause_is_discarded` (design "Tests"
  21: the crash and direct-pause sequence of design review 6, finding
  5: a park posted, the driver dead before its scan, a person's
  `request_pause` journaling `run_paused` directly because no driver
  lives, a person's resume 2 s later; the park is discarded and the
  wait parks again with an `until` that leaves out the paused 2 s).
  Tests 9 to 11, 15 to 17 and 21 write their state directly (the wait open, a dead `driver.pid`, the
  control entries in the order the design names, with `post_control`
  calls made one after the other; tests 9 and 16 assert the two entries
  got `N` and `N + 1` in that order);
  for test 11 publish the 3600 s version first the way the existing
  patch tests do (`zg --rg -F "Control::Patch" crates/apb-engine/tests`);
  for test 15 find how a run's patch limit is set
  (`zg --rg -F "max_patches_per_run" crates/apb-engine`). Run
  `cargo test -p apb-engine --test main wait_test` and see them fail
  on an assertion.
- [x] 3b.2 The scan (design D6 "The rule" 1 to 6), in
  `crates/apb-engine/src/scheduler/control_apply.rs` only: the local
  `consumed`, set by every arm that writes the cursor; in the Pause arm,
  a park (`is_park`) that is the last entry of `pending_control` with
  `consumed` false is applied under
  `apb_core::fsutil::lock_dir(run_dir, crate::stop::EVENT_LOCK)` after an
  empty re-read `read_control_after(run_dir, Some(entry.seq))`, with the
  same append and cursor write as the Pause arm, and only when its
  reason names `*current` (`parse_wait_reason`, design D6 rule 2.3);
  before it is applied, still under the lock, the wait's due time is
  computed again on the current clock with `wait_park::due_at_ms` over
  `read_all(run_dir)` for the node `*current` of `playbook`, and the
  park is applied only when that is `Some(req.until)` (design D6 rule
  3.1); every other park is discarded (in-memory and persisted cursor to its
  seq, nothing journaled, the loop goes on); every exit of the pass (the
  Abort arm, the Pause arm for a non-park entry, `PatchResult::Applied`,
  `PatchResult::Paused`, the salvage) writes, in its one existing cursor
  write, the `seq` of the park right after its own entry in
  `pending_control` when there is one, else its own (design D6 rule 5);
  the salvage's Pause arm matches only `!is_park(req)`. Every non-park
  entry keeps its effect unchanged. Check that no caller of
  `scan_control` holds the event lock (`scheduler.rs` 1047) and report
  it. Run `cargo test -p apb-engine --test main wait_test`, then
  `operator_pause_test`, `quota_pause_test` and `deferred_wake_test`.
- [x] 3b.3 The stop watcher (design D6 "The stop watcher"). Test first,
  `crates/apb-engine/src/stop.rs` `mod tests`:
  `a_park_does_not_halt_the_drive`. Then the watcher's Pause check
  (line 567) becomes `matches!(&e.cmd, Control::Pause(r) if
  !crate::pause::cause::is_park(r))`; nothing else in `stop.rs`
  changes. Run `cargo test -p apb-engine --lib stop`.
- [x] 3b.4 The waker (design D7), in `crates/apb-engine/src/pause/waker.rs`
  only: inside the loop of `run_waker`, right after `same_pause_until`
  confirmed the pause at `seq` and before the due check, when the
  `run_paused` at `seq` has cause `Wait` (read again whenever `seq`
  changes) and the pending read (`read_control_cursor(&run_dir)`, then
  `read_control_after(&run_dir, cursor)`) gives at least one entry, call
  `resume_while(root, run_id, WakeHold::Pause(seq))` once, drop the
  lease and return its outcome. No `?` on that read: an error from
  either call counts as nothing pending for this poll, the loop goes on
  to the due check, and the waker writes one stderr line for the first
  failed read and none after (design D7 "The pending read"). No read or
  write of a control entry beyond that emptiness test, and nothing else
  in the loop changes. Run
  `cargo test -p apb-engine --test main wait_test` (tests 12 to 14 go
  green) and `cargo test -p apb-engine --lib pause::waker`.

## 4. Surfaces (design "Surfaces", D5; spec "Surfaces show a park as a wait", "A park sends no paused notice", "Next actions follow a park")

- [x] 4.1 Tests first, a new `#[cfg(test)] mod tests` in
  `crates/apb-engine/src/pause/display.rs`: `a_park_reads_waiting_on_timer`
  (`line()` of a `PauseInfo` with cause `Wait`, reason `wait (pause)`,
  `until`, `keep_lock`). Then `line()` per Surfaces 1. Run
  `cargo test -p apb-engine --lib pause::display`.
- [x] 4.2 Tests first, `crates/apb-engine/src/notify/transitions.rs`:
  `the_notify_sweep_sends_no_paused_notice_for_a_park`. Then the
  `RunPaused` arm (lines 129 to 133) reads the reason too: an engine
  pause with cause `Wait` sets `paused = None`. Run
  `cargo test -p apb-engine --lib notify::transitions`.
- [x] 4.3 Tests first, `crates/apb-engine/src/next_action/tests.rs`:
  `a_park_offers_the_resume_with_its_until` (the resume action's reason
  is the wait line, `auto` is `pause_until(until)`, and the run's clock
  is not paused), `an_overdue_park_says_so` (`until` 2 minutes past, no
  lease), `an_unattended_park_resumes_its_attempt`. Then
  `next_action/verdict.rs`: `Clock.paused` (line 76) is `status ==
  Paused` and the pause's cause is not `Wait`; `resume` (625) gets the
  wait line and the overdue line (overdue: `now_ms > until +
  pause::RELEASE_GRACE_MS` and `waker::Lease::held(run_dir,
  WakerFor::Pause(pause_seq))` false; make `RELEASE_GRACE_MS` reachable
  as it is, `pub(crate)`); `next_action/unattended.rs` line 120 treats
  `Wait` like `Quota`. Run `cargo test -p apb-engine --lib next_action`.
- [x] 4.4 Tests first, `crates/apb-engine/tests/suite/report_html_test.rs`:
  `the_report_shows_a_pause_line` (a paused run's report holds `pause: `
  and the pause line; a parked run's holds the wait line). Then
  `model.rs` `pause: Option<String>` from `view.pause().map(|p| p.line())`
  and `sections.rs` `meta("pause", ..)` after `deadline` (line 163). Run
  `cargo test -p apb-engine --test main report_html_test`.
- [x] 4.5 Tests first, a unit test `run_wait_explains_a_park` in the
  `#[cfg(test)] mod tests` of `crates/apb-mcp/src/tools/run.rs` (add the
  module if it has none): the instruction for a run view whose pause has
  cause `wait` says the run waits on a timer and its waker resumes it at
  `pause.until`. Then the `run_wait` instruction branch for cause `Wait`
  before line 830 (design Surfaces 3). Run
  `cargo test -p apb-mcp --lib tools::run`.
- [x] 4.6 Tests first, `crates/apb-cli/src/pause.rs`: a new
  `#[cfg(test)] mod tests` with `a_park_resumed_early_says_it_waits_again`
  (a run directory written under `$TMPDIR`). Then `early_resume_note`
  (line 70) gives the wait wording of design D5 5 for cause `Wait`. The
  CLI package has no library target: run `cargo test -p apb --bin apb pause`.
- [x] 4.7 Tests first, `web/src/lib/pause.test.ts`:
  `pauseLine names a timer wait`, `pauseCauseLabel names a timer wait`.
  Then `web/src/lib/pause.ts`: `pauseLine` (the node read from the
  reason with the one form of design D2 3: the prefix `wait (`, the
  final `)`, the node verbatim between) and `pauseCauseLabel` for
  `cause === 'wait'`. `canPause` and `canResume` stay as they are. Run
  `cd web && bun run test && bun run check`.

## 4a. Reminders for open waits (design D9; spec wait-reminders)

Independent of sections 1 to 4 except task 4.2 (the park's notify arm),
which comes first.

- [x] 4a.1 Tests first in `crates/apb-core/tests/suite/policy_file_test.rs`:
  `remind_entries_are_checked` (each message of design D9 "Where the
  cadence lives" item 2), `a_project_may_only_turn_reminders_off`,
  `remind_ms_reads_the_machine_list`. Then `NotifyPolicy.remind`,
  `validate(source)`, `Policies::remind_ms`, `Effective.remind_ms` and
  the module doc. Run `cargo test -p apb-core --test main
  policy_file_test` and `cargo test -p apb-core --lib notify`.
- [x] 4a.2 `NotifySet.remind_ms` (every literal construction the
  compiler names), `NotifyPlan`, `set_event`, `Active.remind_ms` with the
  fold and `follow_parent` (design D9 "The subscription carries it"),
  and `Active.target_set_seq` with the fold keeping it across a set
  whose `target` is equal, and the notice intent's `target_digest`
  compared at reminder admission (design D9 "The notice's target").
  `cargo build -p apb-engine`; `cargo test -p apb-engine --lib notify`
  and `cargo test -p apb-engine --test main journal_compat_test` (both
  must still pass).
- [x] 4a.3 Tests first in the `mod tests` of `notify/transitions.rs`:
  `a_reminder_has_its_own_label`,
  `no_reminder_before_the_notice_is_confirmed`,
  `only_the_latest_due_reminder_is_sent`,
  `a_failed_reminder_is_tried_again` (design D9 "Wrong implementations
  of the reminders" 13: a failed first try of `.r1` is due again after
  the retry gap, not final), `no_reminder_to_a_target_the_notice_missed`
  (item 14),
  `paused_time_does_not_count_toward_a_reminder`,
  `no_reminder_for_a_closed_wait`,
  `only_reviews_questions_and_host_tasks_are_reminded`,
  `a_park_is_never_reminded` (the last may pass at once, since a park is
  not a reminded kind; say so). Then `TransitionId.reminder`, `id`,
  `reminder_may_send` (the notice confirmed to the target in force, and
  `may_try` over the latest due threshold's own label; no skip for a
  label that already has a try) and the reminder part of `due`. Run `cargo test -p apb-engine --lib
  notify::transitions`.
- [x] 4a.4 Engine tests first in a new
  `crates/apb-engine/tests/suite/notify_remind_test.rs` (declared in
  `tests/main.rs`; the notify connector stand-in of `notify_send_test.rs`
  lines 25 to 120, with `RETRY_GAP_ENV` short; durations of `10m` are
  written into the journal's times by test events, never waited for):
  `a_reminder_is_sent_through_the_ledger_once`,
  `a_pause_in_the_middle_moves_the_reminder`,
  `a_decided_gate_sends_no_reminder`, `a_reminder_reads_still_waiting`,
  `a_reask_restarts_the_reminders`,
  `a_reminder_admitted_before_the_decision_completes`,
  `two_sweeps_send_one_reminder` (two variants: a resolving target, and
  a live target that does not resolve, where both sweeps reach
  `record_unsent` and exactly one `not_sent` intent and one settlement
  are recorded for the label; design review 6, finding 2),
  `a_crash_after_the_intent_does_not_resend`,
  `a_target_change_moves_reminders_to_the_next_request` (the sequence of
  design review 6, finding 3: review 1 noticed at target A, the target
  changed to B, 1 h of active time with no reminder at A or B; review 1
  decided, review 2 noticed and, 1 h later, reminded at B; A's stand-in
  saw exactly one request) and
  `a_question_and_a_host_task_are_reminded` (design D9 "Wrong
  implementations of the reminders"; the resolving variant of
  `two_sweeps_send_one_reminder` and
  `a_crash_after_the_intent_does_not_resend` may pass at once through the
  send ledger, say so; the unresolved-target variant must fail before
  task 4a.5). Then `Notice::compose` for reminders and the
  `notify::view` rows. Run `cargo test -p apb-engine --test main
  notify_remind_test` and `cargo test -p apb-engine --test main
  notify_send_test` (must still pass).
- [x] 4a.5 The ordering boundary (design D9 "The ordering boundary"),
  tests first in a `#[cfg(test)] mod tests` of
  `crates/apb-engine/src/notify/send.rs` (add it if the file has none):
  `a_reminder_due_on_a_stale_journal_is_not_admitted` and
  `a_closed_wait_gets_no_unsent_reminder` (both its variants), each seen
  failing. Then `CallContext.still_due` and its argument to
  `CallContext::notify` (`connector/call/request.rs`), the recheck in
  `send_once::decide` for `Admission::New` only, refused with
  `CallErrorCode::AttemptClosed` and the design's message, the
  `still_due` parameter of `record_unsent` appending through
  `append_decided_synced`, and the one predicate `Notifier::send` builds
  for a reminder and passes to the call and to every `record_unsent`
  call: the unresolved target, the args the function no longer names
  and the refused call. The predicate, over the locked events, is the
  wait still open, the target in force still the notice's, and
  `reminder_may_send` for the reminder's label (design D9 "The ordering
  boundary" item 4). Run `cargo test -p apb-engine --lib notify`, `cargo test -p
  apb-engine --lib connector` and `cargo test -p apb-engine --test main
  notify_remind_test`, then `cargo test -p apb-engine --test main
  send_intent_test` and `cargo test -p apb-engine --test main
  notify_send_test` (both must still pass).
- [x] 4a.6 `apb notify` prints the reminders line (a test first in
  `crates/apb-cli/tests/suite/notify_cli_test.rs`:
  `notify_status_names_the_reminders`); `docs/NOTIFY.md` (design D9
  "Surfaces and docs" item 3). Run `cargo test -p apb --test main
  notify_cli_test`.

## 5. Records and docs

- [x] 5.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both (only
  CLAUDE.md's "Claude-specific" section has no twin): in the `apb-engine`
  scheduler list add `wait_park` (the decision whether a long timer wait
  parks: the engine pause it posts) and, at `control_apply`, "a park
  applied only as the last entry of a pass at its own node, else
  discarded, also right behind the entry any exit of a pass ends at"; at
  the control channel (where `control.rs` is described, or next to the
  `control_apply` entry if none), "`post_control` numbers and appends
  under `control.jsonl.lock`, a leaf below the event lock"; in the pause
  list, at `waker`, "the waker of a park resumes the run once when a
  control is pending at any poll"; in the pause list extend `cause` to
  "a quota, budget, billing or wait pause's reason"; in the `apb-core`
  `guarded` entry add "`wait_park` names a definition with a timer
  wait"; in "Concepts that span files" add after the quota pause bullet:
  "A **wait park**: a timer wait with more than `release_after` left, in
  a run that may release and carries the `wait_park` run guard, with
  nothing else to run and no control pending, posts the engine's own
  pause (`wait (<node>)`, `until`, `keep_lock` for a write run) like a
  quota pause; the control scan applies it only as the last entry of its
  pass with nothing else applied and at the node it names, under the
  run's event lock, and discards it otherwise (also right behind the
  entry any exit of a pass ends at), so a person's pause or stop or a
  patch is never held behind it; the waker of a park that finds a
  control pending at any poll resumes the run once. From there it is an
  ordinary engine
  pause with `until` (the pause waker, `apb resume --all`, a person's
  pause and stop). The paused-interval clock does not count it, the
  deadline does; no notify transition and no reminder." In the
  `apb-core` list, at `policy_file`, add `notify.remind`; in the
  `apb-engine` notify list add "reminders for an open review, question
  or host task on the active run clock, one label each
  (`notify:<kind>.<seq>.r<n>`), a re-asked request starting its own,
  every reminder intent admitted under the journal lock only while its
  wait is open, its notice's target is in force and its label is
  admitted (`CallContext.still_due`, `record_unsent`), only to the
  target the notice reached, best effort across binary versions"; and in the
  **Notifications** paragraph of "Concepts that span files" one sentence
  with the same content. Check the mirror by
  reading both diffs (`git diff CLAUDE.md AGENTS.md`): the added lines
  must be the same text in both files.
- [x] 5.2 `docs/MCP.md` "Pause and resume": after the `until` bullet, a
  bullet on the wait park (a long timer wait reads paused with cause
  `wait`; no driver runs, its waker resumes it, `apb resume --all` after
  a reboot; a resume before `until` parks again; a pause or stop works as
  on any paused run, also one sent while the run is about to park); in "Clocks freeze" the sentence "A wait park is not
  paused time: the wait's timer keeps running."
- [x] 5.3 `docs/HOWTO-authoring.md`: add `## wait (a timer or a signal)`
  after the last node-kind section (find it with `rg -n '^## ' docs/HOWTO-authoring.md`):
  the two forms, `timeout_seconds`, and "a timer wait with more than a
  minute left pauses the run until it is due: no driver and no agent runs
  (only the small pause waker waits), the busy lock of a write run stays
  parked, the pause waker resumes the run when it is due and `apb resume
  --all` after a restart; a webhook wait and every wait of an
  unattended, supervised, eval or child run keep their driver; a
  playbook with a timer wait carries the run guard `wait_park`, so an
  older apb refuses to resume or pause its runs while they wait on the
  timer".
- [x] 5.4 `docs/GUARDRAILS.md` "Older apb binaries" (line 237): one
  paragraph that a run whose playbook has a timer wait carries a
  `run_guard` naming `wait_park`, so an apb that does not know the park
  refuses to resume or pause it while the park is its open state and
  still reads and stops it; and the limit, in one sentence: once such an
  older apb stopped the run, a person's resume with that older apb
  counts the park as paused time, so the timer waits again and a run
  deadline may end the run (design "An older apb"). Add one sentence
  that sending controls to one live run from two apb versions at once is
  not supported (design D8 "Writers of an older apb"). Add the sentence
  sentences of design D9 "Surfaces and docs" 4 (`notify.remind` in an
  older apb; reminders best effort across binary versions).
  Check line 167 too: if it lists pause causes, add the wait.
- [x] 5.5 Write `docs/superpowers/specs/2026-11-19-wait-park-design.md`
  (use the implementation date in the name if it differs): the content of
  `proposal.md` and `design.md` as one public record, with no mention of
  `openspec/` and no reference to any outside article, author or project.
- [x] 5.6 Style over every changed doc, spec and web file: no U+2013 or
  U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--`, `![` and Rust macros such as `matches!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>`
  finds nothing).

## 6. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 6.1 `cargo fmt --all -- --check`
- [x] 6.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 6.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 6.4 `cargo test --workspace --doc`
- [x] 6.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; the generated files are current)
- [x] 6.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 6.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean; one violation remains, the module cycle
  `pause.rs -> cause.rs -> clock.rs`, which main already holds: verified
  by running the same check on the stashed pristine tree, and no file
  this change touches adds an edge. The test-only edges this change had
  added into that chain were removed.)
- [x] 6.8 `scripts/third-party-notices.sh --check` (no dependency changes;
  it must pass unchanged)
- [x] 6.9 `cargo clippy --release`: passes with no error, and no warning
  in a file this change touches that `cargo clippy --release` on main
  e76aa42e does not also print (compare the two outputs filtered to the
  changed files with `git diff --name-only e76aa42e`)
- [x] 6.10 `openspec validate wait-park --strict`
  (the openspec CLI is not on this machine and the npm package does not
  resolve offline; a structural check stood in: every requirement of
  both specs carries at least one scenario, and the change folder shape
  is the one the repo's merged changes use)
- [x] 6.11 `git diff --name-only e76aa42e` lists none of the files the
  header says must not change, in `scheduler/control_apply.rs`,
  `stop.rs` and `pause/waker.rs` only the changes of section 3b, and in
  `control.rs` only those of section 3a (read `git diff e76aa42e --
  <file>` for each; after a rebase onto a main that carries
  before-running-alone, diff against that main instead).
- [x] 6.12 The journal corpus the CI way (`.github/workflows/ci.yml`
  job `journal-corpus`, with this machine's binary directory):

  ```sh
  (cd web && bun run build)   # apb-server embeds web/dist
  for tag in v0.25.0 v1.0.0-beta.1; do
    [ -x "$HOME/.cache/apb-old-binaries/bin/$tag/apb" ] ||
      scripts/old-apb.sh "$tag" "$HOME/.cache/apb-old-binaries/bin"
  done
  APB_CORPUS_REQUIRED=1 APB_OLD_BINARIES="$HOME/.cache/apb-old-binaries/bin" \
    cargo nextest run --profile ci -p apb --test main journal_corpus
  ```

  Every row passes, none is skipped (`APB_CORPUS_REQUIRED=1` turns a
  missing binary into a failure). Rows W1 and W2 are the proof that a
  run that never parks writes the same journal as main: W1 with no
  guard and no new line, W2 with only its `wait_park` guard lines.
  Record the row count and time in the report.

## 7. Close

- [x] 7.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f __pause-waker` and `pgrep -f __drive`).
- [x] 7.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to
  0.1 to 0.3, each gate of section 6 with its numbers, the user-visible
  changes for the release notes (`proposal.md` "Impact" 5), the changed
  file count (design "Code map" says 51: 34 for the park, 17 for the
  reminders), the caller list of 3a.1, and anything left open.

## 8. Review round 1

Findings of the spec conformance review and the security and test audit,
each verified against the code first; the decisions are the owner's.

- [x] 8.1 `park_request` refuses a node whose reason does not round-trip
  (design D1 condition 1); unit test `a_wait_on_an_unsafe_node_never_parks`.
- [x] 8.2 A failed event-lock take discards the park (D6 rule 4); unit
  test `control_apply::tests::a_park_whose_event_lock_is_busy_is_discarded`.
- [x] 8.3 `post_control` writes the line and its newline with one
  `write_all`, numbers one above the highest number, and a stop that
  cannot take `control.jsonl.lock` appends without it (D8 items 2 and 4);
  unit test `a_stop_is_posted_while_the_numbering_lock_is_held`.
- [x] 8.4 No park with an unbounded `until`; a discarded park is not
  posted again before a change or `PARK_RETRY` (D1, `ParkRetry`); the wait
  branch reads the outer `state`; unit test
  `a_discarded_park_is_posted_again_only_after_a_change_or_the_backoff`.
- [x] 8.5 `a_pending_park_is_applied_by_the_next_drive`, and every discard
  test posts the wait's real due time (Tests 6).
- [x] 8.6 Reminders: `reminder_open` with the filters `due` uses,
  `reminder_may_send` without its journal, the cadence in force passed to
  the notice, an early return on `AttemptClosed`; the stale-journal test
  with a resolving target, the args variant, both race variants and the
  narrowed-subscription test in `notify_remind_test.rs`;
  `notify::send` crate-private with a `testing`-only re-export.
- [x] 8.7 The HTML report's meta lines go through the run redactor; test
  `the_pause_line_is_redacted`.
- [x] 8.8 One `pause::cause::is_park_event`; the overdue park line with
  an unknown pause seq; the kind rule's test covers `stalled` and
  `paused`.
- [x] 8.9 Test defects: journal-based timing in `wait_test.rs`, a bounded
  poll in the stop watcher test, `pgrep` checks that can fail and an
  exact lease name in `pause_waker_cli_test.rs`, the machine policy put
  back in `notify_cli_test.rs`, positive controls, no debug block, no
  fixed sleeps after a synchronous sweep in `notify_remind_test.rs`.
- [x] 8.10 Design, specs, the public design record, `CLAUDE.md` and
  `AGENTS.md` updated.
- [x] 8.11 Gates of section 6, the suite run twice.

## 9. Review round 2

Findings of the second review, each verified against the code first and
proved by a test that failed before its fix.

- [x] 9.1 A child's early due check (`Notifier::may_be_due`) reads the
  subscription in force, so a child follows the cadence its parent
  turned on (design D9 item 4); test
  `notify_remind_test::a_child_follows_the_cadence_its_parent_turned_on`.
- [x] 9.2 The admission computes the due reminder again from the locked
  journal (`transitions::due_reminder`) on a clock read there, and admits
  only the selected one under the same target and cadence (D9 "The
  ordering boundary" item 4); tests
  `a_cadence_changed_before_admission_records_no_intent` and
  `a_target_changed_before_admission_records_no_intent` (the second
  already held through the notice's target check and stays as a guard).
- [x] 9.3 `two_sweeps_send_one_reminder` races over a slice read before
  reminders were turned off and asserts no intent (fails with the locked
  recheck removed); the plain race is
  `two_sweeps_of_an_unchanged_journal_send_one_reminder` (Tests 9).
- [x] 9.4 The process hand-over test uses a 120 second timer, longer than
  its 10 second observation bound, and ends its waker;
  `a_parked_wait_is_resumed_by_its_waker_at_until` is the timed half
  (Tests 18).
- [x] 9.5 Own sweep: `NotifyState::in_force` follows the parent chain,
  bounded by `MAX_SUBPLAYBOOK_DEPTH`; test
  `a_grandchild_follows_the_cadence_at_the_top_of_its_chain`.
- [x] 9.6 Design, specs, the public design record and `docs/NOTIFY.md`
  updated; gates of section 6.

## 10. Review round 3 and closing

- [x] 10.1 A reminder goes only to the exact target its notice was
  confirmed to: the notice's send intent records
  `target_digest` (`NotifyTargetRecord::digest`, recipient and pins
  included), compared at reminder admission with no time compared across
  journals; a notice recorded without it gets no reminder after a target
  change (design D9 "The notice's target"); tests
  `no_reminder_to_a_recipient_the_notice_did_not_reach`,
  `a_notice_without_a_target_digest_is_reminded_only_without_a_target_change`
  and `a_child_sends_no_reminder_after_its_parent_changed_the_recipient`,
  the first and the third failing before the fix.
- [x] 10.2 The waker's poll tests and the stop watcher's park test wait
  on a completed poll (a test-only poll count), not a fixed sleep; each
  fails with its per-poll check removed.
- [x] 10.3 Design, specs, the public design record and `docs/NOTIFY.md`
  updated; the change archived with a Purpose for each new spec;
  `openspec validate --all --strict` passes.

## 11. Pull request review

- [x] 11.1 A stop posted without the numbering lock is numbered
  `UNLOCKED_STOP_GAP` above the next free number, so no locked writer
  publishes its number first (design D8 item 4); an exit of the scan
  writes the higher of a discarded park's number and its own entry's
  (D6 rule 5); tests
  `a_stop_is_posted_while_the_numbering_lock_is_held` and
  `a_stop_numbered_above_a_later_park_is_not_left_pending`.
- [x] 11.2 The drive's park is skipped, not an error, while the numbering
  lock is busy past its bound (`post_control_unless_busy`, D8 item 5).
- [x] 11.3 Tests: the CLI waker checks count pids with a `pgrep` macOS
  has and match a process by its own root as well as its run id; the
  reminder tests set the retry gap under the env lock, hold the reminder's
  answer until the decision is journaled, age the cut intent past the
  retry gap with a retryable control, and make the target fail to load;
  the corpus rows fail on a control file that does not read and fix W2's
  release threshold; the park tests bound their wait for `until` and
  derive the second park's due time from the journal.
- [x] 11.4 `docs/HOWTO-authoring.md` names when a timer wait parks.
