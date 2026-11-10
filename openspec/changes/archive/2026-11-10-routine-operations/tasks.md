# Tasks: routine operations

Work in order, one section at a time. Before a section, read its part of
`design.md` and the spec delta it names. Tick each box (`- [x]`) as soon as
it is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

For every behaviour: write the failing test first, run it and see it fail,
then write the code. For a test that names a field or a module that does
not exist yet, a compile error is the expected first failure, but it is
never the proof: the crash, race and parked-state tests below must fail
on a real assertion before the code that makes them pass exists.

This change is built on main 7e14c9e3 (after the dsh agent, NEAR AI Cloud
models with external data runs, and Google connectors). Line numbers in
`design.md` are from that commit: find every site by its name if it
moved. The follow-up change `routine-health` is not part of this one.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-routine-operations-target TMPDIR=$HOME/.cache/apb-routine-operations-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-routine-operations-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-routine-operations-target`
and `TMPDIR=/tmp/apb-routine-operations-tmp` instead, and before each build
check that `df -h /tmp` shows more than 4G free; if not, stop and report
it. `/tmp` is a RAM disk shared with other sessions. Never use `/dev/shm`,
and never put a target dir, cache or temp data inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR`. Never stop a process you did not start. Do not commit,
push or open a pull request: the git directory is read-only for you. Do not
start subagents.

During a section run only the tests of the crates it touches
(`cargo test -p <crate> --lib <module>`,
`cargo test -p <crate> --test main <module>`; the CLI package is named
`apb`, so its tests are `cargo test -p apb ...`), one filter per invocation:
`cargo test` takes one test name filter, so two modules are two commands.
New integration test files go in `crates/<crate>/tests/suite/` and are
declared in that suite's `main.rs`.

## 0. Preconditions

- [x] 0.1 `git log --oneline -1` shows 7e14c9e3 or a later main; note it.
- [x] 0.2 Confirm V200 is free: `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u`
  prints only `"V220"`, `"V221"` and `"V222"` (the cloud model rules; V223
  to V229 are reserved for them) and `git grep -ohE '"V1[89][0-9]"' crates |
  sort -u` ends at V187. If V200 is taken, use the next free code in V201 to V209, update
  `design.md` and the specs, and say so in the final report.
- [x] 0.3 Confirm no module named `start_key`, `run_deadline` or
  `step_start` exists in `apb-core` or `apb-engine`.
- [x] 0.4 Read `normalized` in
  `crates/apb-engine/tests/suite/external_data_test.rs` (line 329): 2.11
  compares a scenario run of a playbook with `deadline:` against one of the
  same playbook without it through that normalization (or a shared copy of
  it), so no digest needs to be pinned before the change. No bundled test
  run is needed here.

## 1. Shared seams and the graceful stop (D0)

Every seam here is used by a later section; build it first.

- [x] 1.1 Signed-store transaction, tests first in
  `crates/apb-core/src/signed_store.rs`: `Unsigned::Refuse` on garbage and
  on a valid shape without a MAC returns `StoreError::Unverified` and
  leaves the bytes unchanged; `Unsigned::Replace` behaves as `update` did;
  `transact_serializes_writers` (two threads, 100 entries each, while the
  other holds the lock: 200 entries at the end and the MAC verifies).
  Then `signed_store::transact`, `Unsigned`, `StoreError::Unverified`;
  `update` delegates with `Unsigned::Replace`.
  `cargo test -p apb-core --lib signed_store` passes, and so do the known
  problems and corrections tests (`cargo test -p apb-core --lib
  known_problems`, `cargo test -p apb-core --lib corrections`).
- [x] 1.2 `fsutil::try_hold_lock` (one attempt on a `HeldLock`, `WouldBlock`
  at once when held), with a unit test that a second open of the same file
  in the same process is refused while the first is held and accepted
  after it is dropped.
- [x] 1.3 Graceful kinds, tests first: `stop_context.rs` unit tests (a
  node flag marked `Graceful::Abort` and latched reads `RunAborted`;
  marked `Graceful::Pause` reads `RunPaused`); then
  `pause::interrupt::Graceful`, `mark_graceful(flag, kind)`,
  `is_graceful` (kill sites) and `is_pause` (stop causes:
  `StopContext::reason`, `scheduler/host.rs` line 554),
  `CancelFanout.graceful: Option<Graceful>`. Every existing pause test
  still passes (`cargo test -p apb-engine --lib pause`).
- [x] 1.4 Tests first in `crates/apb-engine/tests/suite/stop_run_test.rs`:
  `graceful_abort_sends_sigterm_first` (an agent stand-in that traps
  SIGTERM writes a marker; `APB_PAUSE_GRACE_MS=300`; the run ends
  `run_aborted` with the given reason and the marker exists);
  `a_graceful_abort_closes_the_attempt_as_aborted` (close reason
  `run_aborted`, never `run_paused`; no `attempt_finished { interrupted }`
  without the abort); `a_graceful_abort_cancels_the_host_task` (host
  mode: the task is closed `cancelled`, a later `run_task_submit` for it
  is refused as not pending); `apb_stop_stays_immediate`
  (`control.jsonl` holds no `"graceful"`); `a_stop_that_is_not_due_writes_nothing`
  (`StopRequest.due` returning false: `StopOutcome::NotDue`, no control
  line, no event). In `control.rs` tests: an old `abort` line without
  `graceful` decodes.
- [x] 1.5 `Control::Abort.graceful`, `stop::StopRequest` with `due`,
  `StopOutcome::NotDue`, `stop::stop_run_with` (`stop_run_by` delegates to
  it; with `due` set the event lock is required and `due` is evaluated on
  the journal read under it), the `StopWatcher` abort fanout
  (`Graceful::Abort`, fired before the stop fanout; never the pause
  fanout), `abort_children` passing `graceful`. Fix every constructor the
  compiler lists.
- [x] 1.6 `workspace::LOCAL_ENTRIES` gets `start-keys/`.
- [x] 1.7 `cargo test -p apb-engine --lib control`,
  `cargo test -p apb-engine --lib stop_context` and
  `cargo test -p apb-engine --test main stop_run_test` pass.

## 2. Run deadline (D2, spec `run-deadline`)

- [x] 2.1 Tests first: `pause.rs` tests for `deadline_elapsed_ms` (an
  engine pause counts, an operator pause does not, an ended span before a
  resume does not, an open ended span does not age, overlaps subtracted
  once, a person's pause replaced by an engine pause without a resume
  counts the engine part only, the reverse counts the engine part only,
  and `paused_intervals` of both journals is unchanged); `pause::parked`
  tests (a pause, a release and a replaced pause each give the `WakeHold`
  in force; a resumed or ended run gives none); `run_deadline.rs` tests for `in_force`, `remaining_ms`,
  `expired`, `reason` and `patch_refusal`; V200 tests in
  `crates/apb-core/src/validate/deadline.rs` (`45m` ok; `0`, `30s`, `31d`,
  `soon` errors); `event.rs` decode tests for `run_provenance` and
  `run_resumed` without `deadline`; `guarded.rs` tests that `deadline:`
  names `deadline` and `ALL` has 8 entries.
- [x] 2.2 Core: `Playbook.deadline`, `validate/deadline.rs` (V200) called
  with `// V200`, `GuardedFeature::Deadline`.
- [x] 2.3 Engine: `pause::deadline_elapsed_ms` over one private fold of
  owned pause segments shared with `paused_intervals` (design D2 "The
  clock"), `pause::parked`, `run_deadline.rs` (with `DeadlineLatch` and
  `Expect`), `RunProvenance.deadline`,
  `RunResumed.deadline` (every literal and `EventPayload::resumed` get
  `deadline: None`), `RunOptions.deadline_ms` and the start record (a
  top-level production run only: not a scenario, an eval or a child),
  `run_guard(playbook, events)` at the start and in `scheduler/patch.rs`,
  the patch refusal in both patch paths.
- [x] 2.4 Enforcement, tests first in
  `crates/apb-engine/tests/suite/run_deadline_test.rs`, each failing
  before its code: `a_run_ends_at_its_deadline` (graceful, reason prefix,
  by engine); `a_parent_stops_its_child_at_its_deadline`;
  `an_operator_pause_does_not_count`;
  `no_list_item_starts_after_expiry`; `a_tree_queued_run_ends_at_its_deadline`;
  `a_parked_run_ends_at_its_deadline` (an engine pause without `until`);
  `a_waker_does_not_probe_a_run_past_its_deadline` (probe stand-in counts
  no call); `a_plain_resume_past_the_deadline_is_refused` (synchronous,
  nothing journaled, no driver spawned); `a_resume_with_deadline_grants_time`
  (total = elapsed + grant); `a_detached_resume_carries_the_grant`;
  `a_resume_grant_adds_the_guard`; `a_patch_keeps_the_guard_and_refuses_a_deadline_change`;
  `no_work_starts_after_expiry` (the watch held back with
  `APB_TEST_DEADLINE_WATCH_DELAY_MS`; a batch with queued members and a
  list node; no start at or after `run_started.ts + total_ms`);
  `a_child_prepared_during_expiry_never_runs`;
  `a_released_run_ends_at_its_deadline`;
  `a_quota_repause_keeps_the_deadline_watch`;
  `a_grant_committed_first_is_not_aborted`;
  `a_resume_that_expires_before_its_commit_is_refused`;
  `a_person_pause_before_the_stop_wins`
  (design D2 wrong implementations 8, 16 to 21). The barrier tests use
  `testing`-feature hooks (`run_deadline::test_hooks`): one between the
  stop's decision and its commit, one before a resume's commit, one in the
  run watch before its stop, one in the child's preparation. A hook is a
  compile error without debug assertions, like every `testing` seam.
  Give the tests deadlines of a few seconds through `RunOptions.deadline_ms`
  and `ResumeRequest.deadline_ms` (the validator's 1 minute floor is for
  authors only).
- [x] 2.4a Tests first (`run_deadline_test.rs`):
  `a_baseline_read_is_bounded_by_the_deadline`,
  `a_slow_credential_helper_is_bounded_by_the_deadline`,
  `a_cut_before_read_journals_nothing`, `a_cut_after_read_journals_nothing`,
  `a_resume_with_more_time_reads_afresh` (design D2 "Engine reads on the
  drive thread", wrong implementations 13 to 15).
- [x] 2.5 Implement, in this order: `stop_if_expired(root, run_id,
  expect)` over `StopRequest.due` (1.5); the drive's `DeadlineLatch` and
  the run `DeadlineWatch` (`Expect::Driving`); the admission check in
  `node::execute_node_kind` and before every attempt spawn, plus the drive
  loop's top before the baseline's before snapshot (no check where a
  batch is only formed); the child start's publish-then-check after
  `child_run_started`; the tree-queue checks in `claim_queued_workdir` and
  `drive_run_from_dir`; the per-run `WakerFor::Deadline` (lease
  `deadline`, `--deadline` of `__pause-waker`) over `pause::parked`,
  armed at every park site; `resume_all`'s deadline pass before its
  candidate filtering (`ResumeAllAction::DeadlineStopped`, re-arm);
  the resume preflight in `resume_inner_checked` and
  `resume_detached_inner` and the final check in `try_journal_resumed`
  under the event lock; the checks before both quota probes;
  `DriverArgs.deadline_ms` and `--deadline-ms` of `__drive-run`;
  `ReadCtx.cap_secs` from the latch, applied in `call_connector_json`
  (`None` in the follow-up reader); `resolve_secrets` taking the call's
  deadline; `ensure_before` and `after_and_compare` treating an expired
  latch like `cancel`.
- [x] 2.6 Surfaces: CLI `apb run --deadline` and `apb resume --deadline`
  (`--supervise` through the handoff), MCP `playbook_run.deadline` and
  `run_resume.deadline`, HTTP start and resume bodies, each refusing an
  unparsable value `deadline_invalid`. E2e tests:
  `run_cli_test.rs` `deadline_flag_is_recorded` and
  `resume_deadline_flag_reaches_the_driver`; `run_tools_test.rs`
  `run_resume_deadline_grants_time`; `runs_api_test.rs`
  `resume_body_deadline_grants_time`; `pause_waker_cli_test.rs`
  `deadline_waker_ends_a_parked_run` and
  `resume_all_ends_an_expired_indefinite_pause` (the waker killed; `apb
  resume --all` ends the expired run and re-arms the waker of a run with
  time left).
- [x] 2.7 Views: `run_view::deadline` with unit tests of the three line
  forms; `apb runs <id>`, MCP `run_status`, HTTP run detail, HTML report
  line; `AutoOutcome::RunDeadline`, `RunActions.run_deadline`,
  `deadlines::run_deadline` with unit tests (`At` while running, `Frozen`
  while a person paused or the run ended); the CLI next-action line.
- [x] 2.8 Regenerate TypeScript now:
  `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then the
  same without the variable (register `RunDeadline`, `DeadlineSource`,
  `RunDeadlineView` in `ts_contract.rs` first).
- [x] 2.9 Web: the run detail type gets `deadline?`; `RunView.svelte`
  passes it to `RunDriveNotes`; `web/src/lib/rundeadline.ts` with a
  vitest; `journal.ts` reads a `deadline_exceeded` abort with a vitest; a
  rendered-page test `web/src/pages/RunView.svelte.test.ts` (mock the
  detail fetch the way `RunList.svelte.test.ts` mocks its list) that a
  detail payload with `deadline` shows the line. Plain CSS only; Tailwind
  is banned. `cd web && bun run check && bun run test`.
- [x] 2.10 Docs: `docs/HOWTO-authoring.md` `## deadline (a run's
  wall-clock limit)` right after `## budget`, with the V200 row in the
  format of the V177 table; `docs/CI.md` the paragraph under "Unattended
  runs"; `docs/MCP.md` the arguments.
- [x] 2.11 Scenarios and evals: `a_scenario_ignores_the_deadline`
  (`crates/apb-engine/tests/suite/scenario_parity_test.rs`): one scenario against two copies of a fixture
  playbook that differ only in `deadline: 1m`; equal report checks, equal
  normalized journals (0.4), no `deadline` field on either
  `run_provenance`. `cargo test -p apb --test main eval_cli_test` passes
  (the eval reason is unchanged).
- [x] 2.12 `cargo test -p apb-core --lib validate`,
  `cargo test -p apb-engine --lib`,
  `cargo test -p apb-engine --test main run_deadline_test`,
  `cargo test -p apb-engine --test main run_guard_test`,
  `cargo test -p apb --test main pause_waker_cli_test` pass.

## 3. Step-started runs (D3, spec `step-started-runs`)

- [x] 3.1 Tests first: `launch.rs` tests (a record without `started_by`
  decodes; one with it round-trips); `step_start.rs` unit tests over temp
  projects: verified with a matching token (attempt number), not verified
  with a wrong token, a finished starter, an `APB_RUN_DIR` that is a
  link, a script node without a token verified while open, a host task's
  env (token and task id) verified, a host task adopted by a resumed drive
  (its first attempt's token, a later attempt open) verified, `chain_of` through a child's parent,
  a starter in another registered project keeping its chain, refusal at
  chain 6. `external_data.rs` tests: `step_run_dir` finds the run the way
  `refuse_step_if_flagged` did; `for_start` with a flagged step run
  returns true and refuses an explicit false `external_data_immutable`;
  with a step run not found it inherits nothing.
- [x] 3.2 `LaunchRecord.started_by`, `StartedBy`, `MAX_START_CHAIN`; every
  literal gets `started_by: None`.
- [x] 3.3 `crates/apb-engine/src/step_start.rs` (`CallerSnapshot` with
  the connector layer's `AttemptClaim`, `resolve` binding the attempt
  through `connector::call::attempt::bind`, made `pub(crate)`, never a
  token comparison of its own, `chain_of`); the call in `gate::launch::decide` for
  `Initiator::RunStep` on new starts only; `is_launch_refusal` lists
  `start_chain_too_deep`; the backstop in `check_launch`;
  `external_data::step_run_dir` (shared with `refuse_step_if_flagged`),
  `for_start`'s `step_run` parameter passed by the CLI and MCP starts
  (`None` over HTTP); `playbook_trial` and `playbook_execute_plan` keep
  the requested tri-state flag and call `for_start` before their gate
  (no `== Some(true)` collapse before it, `server/run.rs` lines 58 and
  189); and the `started_by` case of the preparation backstop
  `external_data_start`.
- [x] 3.4 E2e in `crates/apb-engine/tests/suite/step_start_test.rs`:
  `a_step_start_links_both_runs` (the starter's journal holds no event
  about the new run); `a_child_of_a_step_started_run_is_not_marked`;
  `the_token_is_not_stored`; `a_forged_run_id_is_not_verified`;
  `the_sixth_chained_start_is_refused_across_projects`;
  `a_resume_at_chain_five_is_not_refused`;
  `a_step_of_a_flagged_run_starts_a_flagged_run` (with the explicit
  `false` refused and an unflagged starter's run left unflagged);
  `an_adopted_host_task_keeps_its_chain`;
  `a_starter_in_another_workspace_is_named_there`. In
  `crates/apb-mcp/tests/suite/run_tools_test.rs`:
  `a_trial_from_a_flagged_step_is_flagged` and
  `an_executed_plan_from_a_flagged_step_is_flagged` (each with the flag
  omitted, and with an explicit `false` refused).
- [x] 3.5 Views: `run_view::started_by`, `run_view::started_runs`
  returning `StartedRuns { runs, truncated }`, with unit tests (newest
  first, at most 50, unverified records left out, `truncated` true with
  520 newer directories and false when the scan reached an older run);
  `apb runs <id>` (`started runs in this project`, the truncation note,
  `in workspace <ws> (<root>)`), MCP `run_status` and the HTTP run detail
  (`started_runs_truncated`), HTML report.
- [x] 3.6 Register `StartedBy`, `StartedRuns` and `StartedRunView` in `ts_contract.rs`
  and regenerate as in 2.8.
- [x] 3.7 Web: the detail type gets `started_by?` and `started_runs?`;
  `RunView.svelte` passes both to `RunDriveNotes` (links to run pages of
  this project only; a starter in another workspace as text; the
  truncation note); extend `RunView.svelte.test.ts` with a payload
  carrying both, one with a foreign `workspace_id` (no link) and one with
  `truncated`.
- [x] 3.8 Docs: `docs/GUARDRAILS.md` the paragraph;
  `docs/HOWTO-authoring.md` "Starting a run from a run step".
- [x] 3.9 `cargo test -p apb-core --lib launch`,
  `cargo test -p apb-engine --test main step_start_test`,
  `cargo test -p apb-engine --test main launch_policy_test` pass.

## 4. Start key (D1, spec `start-key`)

- [x] 4.1 Tests first in `crates/apb-core/src/start_key.rs`: `StartKey::parse`
  (empty, 201 characters, a control character, leading space, `ghp_` plus
  36 characters refused; `digest-2026-11-09` accepted);
  `StartRequest::capture` gives the same tag for params maps built in two
  orders; `every_field_changes_the_request` (from one base
  `StartRequestFields`, changing any single field changes the tag; the
  test destructures the struct without `..`); `find`
  ignores an entry past `days`; `expire` drops it. `policy_file.rs` tests
  for `runs.start_key_days` (out of range refused naming the key; project
  lengthens only).
- [x] 4.2 Core: `start_key.rs` with `StartRequestFields`, the frozen
  `StartRequest`, `StartKeyRequest`, `StartKeysFile` (entries with
  `state`, `reservation`, optional `run_id`) as a `signed_store::SignedDoc`
  written only through `transact(.., Unsigned::Refuse, ..)`; `policy_file`
  `RunsSection { start_key_days }`.
- [x] 4.3 Engine: `crates/apb-engine/src/start_key.rs` (`lookup`, `claim`
  writing a pending entry and holding its `HeldLock`, `Claim::attach`,
  `Claim::bind`, `Drop for Claim`, the owner probe with `try_hold_lock`
  under the store lock, the 2 s retry and `start_key_busy`, the run
  check; no age rule), `EngineError::AlreadyStarted`,
  `RunOptions.start_key` carrying the surface's `StartRequest` (the engine
  never builds a request), in `prepare_placed` the claim before the tree
  lock, `attach` after `allocate_run` and `bind` after `run_provenance`
  and the run guard, `RunProvenance.start_key`, the `testing` hook after
  `attach` the race tests use, the `apb doctor` line for an unreadable
  file.
- [x] 4.4 E2e in `crates/apb-engine/tests/suite/start_key_test.rs`:
  `a_repeat_answers_the_existing_run`;
  `a_repeat_while_the_tree_is_held_answers_already_started`;
  `eight_concurrent_starts_make_one_run`; `another_request_is_a_conflict`;
  `another_explicit_version_is_a_conflict`;
  `another_external_data_flag_is_a_conflict`;
  `the_request_is_what_the_caller_passed` (remembered input changed in
  between); `a_failed_run_holds_its_key_until_reuse`;
  `reuse_of_a_succeeded_run_is_refused`; `reuse_from_a_run_step_is_refused`;
  `a_deleted_run_frees_its_key`; `the_store_holds_no_key`;
  `a_tampered_store_refuses_keyed_starts` (also: the file's bytes are
  unchanged); `a_forged_entry_is_void`;
  `a_lookup_during_preparation_answers_the_preparing_run`;
  `a_slow_preparation_keeps_its_key` (pending entry two minutes old, owner
  alive); `a_dead_owner_without_run_started_frees_its_key`;
  `a_failure_before_provenance_holds_the_key`;
  `a_refusal_before_allocation_leaves_the_key_free` (design D1 wrong
  implementations 12 to 14 and 16).
- [x] 4.5 CLI: `--start-key`, `--start-key-reuse`,
  `apb_cli::run::start_request` capturing `RunArgs` before the gate and
  the budget question, the early `lookup`, the foreground `wait_cmd`
  exit, the `Supervise` handoff field carrying key, reuse and the request
  tag, and the `ALREADY:` handshake. Tests in `crates/apb-cli/tests/suite/run_cli_test.rs`
  (`cargo test -p apb --test main run_cli_test`):
  `start_key_repeat_detach_prints_the_run`;
  `start_key_repeat_of_a_failed_run_exits_1`;
  `start_key_conflict_exits_2`; `start_key_is_not_on_the_child_argv`;
  `a_global_start_repeats`; `a_budget_answer_does_not_change_the_request`;
  `start_key_conflict_on_no_cache` and `start_key_conflict_on_supervise`.
- [x] 4.6 MCP `playbook_run.start_key` and `start_key_reuse` (tool
  description sentence), the `already_started` answer and the conflict
  refusal, the request captured before `check_run` and
  `permit.run_version`; tests in `crates/apb-mcp/tests/suite/run_tools_test.rs`
  including `a_candidate_does_not_change_the_request`. HTTP
  `RunBody` fields, 200, 409 and 400; tests in
  `crates/apb-server/tests/suite/api_test.rs`.
- [x] 4.7 Views: `run_view::start_key`, the four run surface lines; the
  detail type gets `start_key?`, `RunView.svelte` passes it, the
  `RunDriveNotes` sentence; regenerate TypeScript as in 2.8; extend
  `RunView.svelte.test.ts`.
- [x] 4.8 Docs: `docs/CI.md` "Starting once: --start-key"; `docs/MCP.md`.
- [x] 4.9 `cargo test -p apb-core --lib start_key`,
  `cargo test -p apb-engine --test main start_key_test`,
  `cargo test -p apb --test main run_cli_test`,
  `cargo test -p apb-mcp --test main run_tools_test` pass.

## 5. Records and gates

- [x] 5.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both: in
  the `apb-core` list `start_key.rs`, `validate/deadline` (V200), the
  `policy_file` section `runs`, `LaunchRecord.started_by`; in the
  `apb-engine` list `start_key.rs`, `run_deadline.rs`, `step_start.rs`,
  `stop::stop_run_with` with `StopRequest.due`, `pause::interrupt`
  kinds, `pause::parked`, `WakerFor::Deadline`; in the `apb-core` list
  also `signed_store::transact` and `fsutil::try_hold_lock`; a "Concepts that span
  files" paragraph "Routine operations" (the start key before the tree
  lock and its signed store, the deadline clock and its four states, the
  step record and its reverse links).
- [x] 5.2 Write `docs/superpowers/specs/2026-11-09-routine-operations-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] 5.3 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing) and no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`
  and `![` do not count). No CJK.
- [x] 5.4 The full gates once, each with its numbers in the report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo nextest run --workspace` (or `cargo test --workspace` when nextest
  is missing; say which);
  `cargo test --workspace --doc`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix every failure and every code-ranker violation (read
  `code-ranker docs base <ID>` first).
- [x] 5.5 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f __pause-waker` and `pgrep -f __drive` and stop only the ones
  your tests started).
- [x] 5.6 Final report: per section what was built, decisions you took
  where `design.md` left room, the answers to 0.1, 0.2 and 0.4, each gate
  with numbers, the user-visible changes for the release notes (design
  "Release notes"), the changed file count, and anything left open.
