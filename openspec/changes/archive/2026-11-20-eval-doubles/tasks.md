# Tasks: eval doubles

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
exist yet; the crash and race tests must fail on a real assertion before
the code that makes them pass exists. So every section builds in this
order: types and changed signatures first (each step compiles), then a
skeleton whose functions do nothing where a test file needs them to
exist, then the red tests, then the behaviour one piece at a time.

Line numbers are those of main efae5822 (after #287 to #290). If
`start-rules-and-schedules` merged first, it only moved lines: find every
site by the name `design.md` gives.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-eval-doubles-target TMPDIR=$HOME/.cache/apb-eval-doubles-tmp
mkdir -p "$TMPDIR"
```

Never use `/tmp` or `/dev/shm` for a target dir, a cache or temp data,
and never put them inside the worktree. Never export
`APB_TEST_BILLING_DEFAULT`; the test helpers set it where they need it.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If
it fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get
one under `$TMPDIR` (the existing test helpers do this). Never start a
real agent or reach a real service. Never stop a process you did not
start. Do not commit, push or open a pull request unless the person who
assigned the task asked for it. Do not start subagents.

Search code with `zg --rg -F <symbol> <path>` (or `zg --fts <name>`)
before opening a file; verify a rename with plain `rg`.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>` (for
   example `cargo test -p apb-core --lib eval_doubles`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   eval_doubles_test`). The CLI package is named `apb`: `cargo test -p apb
   --test main eval_doubles_cli_test`.
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
framework, no new styles.

## 0. Preconditions

- [x] 0.1 Check these exist (one `zg --rg -F` each; note the line): `pub
  fn refusal` and `const LATER_FIELDS` in `crates/apb-core/src/eval.rs`;
  `pub struct MockSpec` in `crates/apb-core/src/connector/def.rs`; `pub
  fn versioned_use` in `crates/apb-core/src/decision_thresholds.rs`; `pub
  struct EvalRunSettings` in `crates/apb-engine/src/run_config.rs`;
  `pub(crate) fn append_pair` in `crates/apb-engine/src/event.rs`; `pub
  fn snapshot_connectors` in `crates/apb-engine/src/connector/run.rs`;
  `fn snapshot` in `crates/apb-engine/src/connector/call/target.rs`;
  `pub struct ManifestConnector` in `crates/apb-engine/src/manifest.rs`;
  `pub fn secret_json` and `fn storing_refused` in
  `crates/apb-engine/src/run_redact.rs`; `pub fn tag` in
  `crates/apb-core/src/run_origin.rs`; `pub fn try_hold_lock` in
  `crates/apb-core/src/fsutil.rs`; `pub fn approving_options` in
  `crates/apb-core/src/schema.rs`; `pub fn read_stored` and `pub fn
  boundaries` in `crates/apb-engine/src/run_definitions.rs`; `fn
  withdraw_if_rescoped` in `crates/apb-engine/src/scheduler/review_gate.rs`
  and `pub fn is_scope_changed` in `crates/apb-engine/src/event.rs`; `fn
  gate_visit_decision` in
  `crates/apb-engine/src/decision/report/labels.rs`. If any is missing,
  stop and report it.
- [x] 0.2 `rg -ohE '"V2[0-9][0-9]"' crates | sort -u` prints neither
  V280 nor V281, and `rg -n 'double_unmatched|eval_doubles|EvalDoubles|eval_call_key_unavailable|eval_call_delivery_unrecorded'
  crates web/src` finds nothing. If one appears, stop and report it.
  (V282 stays unused; V283 belongs to
  `incremental-lists-and-session-fork`, V284 to V286 to `inbox-waits`.)
- [x] 0.3 `GuardedFeature::ALL` in `crates/apb-core/src/validate/guarded.rs`
  has 13 entries ending with `ListPilot, WaitPark`. If it has more, a
  later change landed: `EvalDoubles` still goes last, and the count in
  design D0 and in the tests below moves with it; note it in the report.
- [x] 0.4 None of these exists: `crates/apb-core/src/eval_doubles.rs`,
  `crates/apb-engine/src/eval/doubles.rs`. If one exists, it is a partial
  earlier attempt: review it against `design.md`.

## 1. Honest completion labels (design D5; spec honest-decision-labels)

- [x] 1.1 `crates/apb-core/src/decision_thresholds.rs` (`mod tests`, line
  365): tests first `completion_check_is_a_versioned_use` and
  `a_threshold_stored_under_the_old_rule_no_longer_enforces`; then
  `COMPLETION_RULE` and the `versioned_use` arm. Run `cargo test -p
  apb-core --lib decision_thresholds`.
- [x] 1.2 `crates/apb-engine/src/decision/report/labels.rs` tests first
  (design D5 "Tests" item 1, the five re-ask tests included; delete the
  two tests it replaces); then `CompletionLabeller` under `reply/1` with
  `label_with_rule`, the trait method `label_in` over the events and the run directory
  (default: `label_with_rule`), `RunJournal.run_dir` in `report/mod.rs` and the
  calls of `label_in` in its labelling loop and in `report/replay.rs`,
  the keep rule (a gate whose stored definition `approves` the node, a
  binding of this execution's snapshot, one of the gate's approving
  options read from the stored definition of the chain's final request,
  over the new `chain_decision` that follows scope re-asks keeping that
  binding and ends at a `patch_applied`; `gate_visit_decision`
  unchanged) and the new
  `label_source`; delete `moved_back_to` if nothing else uses it. Run
  `cargo test -p apb-engine --lib decision::report`.
- [x] 1.3 `crates/apb-engine/tests/suite/completion_labels_test.rs`:
  `a_report_on_a_finished_reply_and_a_failing_next_node`. Run `cargo test
  -p apb-engine --test main completion_labels_test` and `cargo test -p apb
  --test main decisions_cli_test` (the CLI report tests; fix expectations
  that encoded the old labels and note each in the report).
- [x] 1.4 `crates/apb-cli/src/decisions.rs` line 80: the `--rule` help
  names `completion_check: reply/1` beside `retry_advice: exec/1`.
  `docs/DECISIONS.md` as design D5 "Docs" says.

## 2. Connector doubles: the case and the validator (design D1 "The case format", "Validator: V280 and V281" and "What stays refused"; spec eval-connector-doubles)

- [x] 2.1 `crates/apb-core/src/connector/resolve.rs` `ConnectorFacts`:
  `reconcile_functions` (function to its `reconcile.function`, filled
  beside `idempotency`) and `identity_function` (the connector's
  `identity.function`, set for a connector with a `marker` send). The tests that build facts use
  `..Default::default()`. `cargo build -p apb-core`.
- [x] 2.2 Create `crates/apb-core/src/eval_doubles.rs` (declared in
  `lib.rs` next to `pub mod eval;`) with the types (deriving `Eq`) and
  empty functions; tests first (design D1 "Tests" item 1); then the
  matching, `pick`, `bound_functions` (over `connector_sites`, with
  reconcile functions) and `coverage_gaps`. Run `cargo test -p apb-core
  --lib eval_doubles`.
- [x] 2.3 `crates/apb-core/src/eval.rs`: the case fields, `LATER_*` and
  the new message at line 351, `refusal` without its connector leg
  (every other leg, the `irreversible` one included, kept as on main),
  `case_refusal`, `validate_suite_with(.., facts)`, V280 (only for a case
  with doubles), V281, V81 per case over `case_refusal`; tests first
  (`a_doubled_case_parses`, `answers_and_stop_before_stay_refused`,
  `v280_names_an_uncovered_function`, `v281_refuses_count_with_min`,
  `a_case_without_doubles_keeps_every_reason`,
  `full_doubles_keep_the_irreversible_refusal`); update the existing test
  that expects "node `w` binds a connector" (around line 1182) to the
  per-case V81 and the new connector reason text. Update the engine
  wrapper `apb_engine::eval::checks::validate_suite` to pass
  `validation_facts()`. Run `cargo test -p apb-core --lib eval` and
  `cargo test -p apb-engine --lib eval`.

## 3. Connector doubles: the run (design D1 "Running a doubled case", "The run guard of a doubled run" and "Answering a call"; spec eval-connector-doubles)

- [x] 3.1 `crates/apb-engine/src/connector/result.rs`
  `CallErrorCode::DoubleUnmatched`; `EvalRunSettings.doubles` and
  `RunConfig::doubles`; `RunProvenance.eval_doubles` in `event.rs`
  (`serde(default)`, skipped when false). `cargo build -p apb-engine`.
- [x] 3.2 The guard: `GuardedFeature::EvalDoubles` last in `ALL` (14
  entries) with its `as_str`, `parse` and module doc sentence; tests first
  in `guarded.rs` (rename the count test to the new count, as the earlier
  changes did), `a_doubled_run_carries_the_eval_doubles_guard`,
  `the_guard_is_written_with_the_provenance` and
  `an_undoubled_guarded_start_writes_as_main` in `scheduler/prepare.rs`
  `mod run_guard_tests`; then `run_guard_of(.., eval_doubles)`,
  `run_guard` reading `run_provenance.eval_doubles`, the provenance
  written at start when `opts.eval` carries doubles, and, for a doubled
  start only, the provenance and guard in one `append_pair` (design D1
  "The run guard of a doubled run" item 3; every other start keeps its
  separate appends). Run `cargo test -p apb-core --lib
  validate::guarded`, `cargo test -p apb-engine --lib scheduler::prepare`
  and `cargo test -p apb-engine --lib scheduler::patch`.
- [x] 3.3 The doubled snapshot (design D1 "The doubled snapshot"):
  `ManifestConnector.doubled_accounts` and `DoubledAccount` in
  `manifest.rs`; `snapshot_connectors(.., doubled)` in `connector/run.rs`
  (no account in `accounts` for a doubled run), called with it from
  `scheduler/prepare.rs` line 350. Test first:
  `a_doubled_snapshot_records_no_account` in `connector/run.rs`. Run
  `cargo test -p apb-engine --lib connector::run`.
- [x] 3.4 Create `crates/apb-engine/src/eval/doubles.rs` (`pub mod
  doubles;` in `eval/mod.rs`; `CaptureLine`, `CapturedCall` with
  `ArgsKept`, `Reservation`, `CaptureError`, empty `of_run`, `reserve`,
  `deliver` returning a `Result`, `read_calls` returning a `Result`); add
  `RunRedactor::for_storing` to `run_redact.rs` with its test
  `for_storing_refuses_an_unresolved_secret_in_a_driving_process` (test
  first), and the hook variants `DoubleBeforeDispatch` and
  `DoubleBeforeDeliver` to
  `deadline_hooks.rs`; write `crates/apb-engine/tests/suite/eval_doubles_test.rs`
  and the unit tests `a_doubled_run_never_notifies` and
  `snapshot_and_settings_that_disagree_fail_closed` in
  `connector/call/tests.rs` (design D1 "Tests" items 4 and 7, including
  `an_apb_blind_to_doubles_cannot_call_from_a_doubled_snapshot`,
  `a_function_mock_does_not_bypass_the_double`,
  `a_doubled_call_without_a_key_is_refused`,
  `an_unrecorded_delivery_returns_no_answer`,
  `an_undelivered_send_fails_the_repetition`,
  `a_crash_after_dispatch_leaves_a_reservation_the_checks_fail_on`,
  `the_capture_directory_is_synced_before_the_first_dispatch`,
  `the_capture_and_the_journal_are_read_by_their_own_readers` and
  `connector_calls_args_are_the_input_args`); see them fail.
- [x] 3.5 `of_run` (fail closed), computed once in `prepare`; `reserve`
  with the lock, the strict capture read, the call key (refused
  `eval_call_key_unavailable` when `run_origin::tag` errors, before any
  line), the replay over a free owner lock, `pick` over every
  reservation, the storage redactor's `secret_json` over the input
  arguments, `args_kept` and the reservation line synced before
  `reserve` returns (so before dispatch); the capture removal (later `remove_external_repetition` over `fsutil::beneath`); `deliver` (the line written and synced, then the
  owner lock released) as the last step of `execute`, whose error
  (`eval_call_delivery_unrecorded`) replaces the result; `read_calls`
  over both line kinds; `target::snapshot` over `doubled_accounts` for a
  doubled run, failing closed when snapshot and settings disagree;
  `PreparedCall::Double`, the notification refusal in `prepare` and the
  double in `build_prepared` after the args checks and before the
  function's `mock` (ordinary and inbox functions), with a dry run left
  as it is. Run `cargo test -p apb-engine --test main eval_doubles_test`,
  `cargo test -p apb-engine --lib connector::call`, `cargo test -p
  apb-engine --test main connector_e2e`, `cargo test -p apb-engine
  --test main connector_run` and `cargo test -p apb-engine --test main
  notify_send_test`.

## 4. Connector doubles: checks, CLI and docs (design D1 "The checks", "Running a doubled case" and "Docs"; spec eval-connector-doubles)

- [x] 4.1 Checks and the result: `CheckInput.calls` (the `Result` of
  `read_calls`), the `connector_doubles` and `connector_calls[...]`
  checks over dispatched calls (delivered captures counted; an
  undelivered reservation fails `connector_doubles` and makes its
  function's checks indeterminate; captures not kept whole and a capture
  that does not read as the design says), `CallSummary` on `Repetition`;
  tests first (design D1 "Tests" item 5). Run `cargo test -p apb-engine
  --lib eval`.
- [x] 4.2 `crates/apb-cli/src/eval.rs`: the per-case recheck in
  `run_eval`, the doubles and the `PATH` in the run settings (this
  binary's directory, then the inherited `PATH`, nothing else), the
  captures to `CheckInput`, the removal of the capture of an
  `--external-data` repetition after the checks and before it is stored
  or kept, on both paths (design D1 "Running a doubled case" item 5),
  the dry-run lines, the text report line;
  tests first in `crates/apb-cli/tests/suite/eval_doubles_cli_test.rs`
  (design D1 "Tests" item 6: the `PATH` test asserts the exact value,
  and `an_external_data_repetition_stores_no_capture` and
  `a_kept_external_data_repetition_keeps_no_capture` cover both paths).
  Run `cargo test -p apb --test main
  eval_doubles_cli_test` and `cargo test -p apb --test main
  eval_cli_test` (fix expectations of the old connector refusal and note
  each).
- [x] 4.3 Docs: `docs/EVALS.md` items 1 to 4 of design D1 "Docs" (the
  worked `crm-cleanup` case; "Never with irreversible effects" keeps its
  rule; `connector_calls` `args` and `contains` assert the input
  arguments of the call, before text modes and markers, worded as the
  `CallCheck` field docs say), `docs/HOWTO-authoring.md` (the eval paragraph at line 5364),
  `docs/GUARDRAILS.md` (the guard name `eval_doubles`, lines 276 to 282).
- [x] 4.4 Row V1 in `crates/apb-cli/tests/suite/journal_corpus_test.rs`:
  a doubled run directory written by this binary (manifest with
  `doubled_accounts`, `run.yaml` doubles, `eval_doubles` provenance and
  guard), on which each older binary runs `apb connector call` against a
  `ScriptedHttpServer` base URL: the call fails, the server received
  nothing, the `{{cmd:...}}` marker file is absent; the module doc lists
  the row. Run it the CI way (section 6.4).

## 5. Records

- [x] 5.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both (only
  CLAUDE.md's "Claude-specific" section has no twin), limited to what this
  change adds: in the `apb-core` list `eval_doubles.rs` (the doubles of an
  eval case, matching and coverage with reconcile functions, V280 and
  V281 with the suite rules in `eval.rs`) and `GuardedFeature::EvalDoubles`
  in the guarded list; in the `apb-engine` list `eval/doubles` (answer,
  capture, read, fail closed, the call key, the reservation and the
  durable delivery with the replay of a crashed call),
  `RunRedactor::for_storing` (the storage redactor in the `run_redact.rs`
  entry), the doubled snapshot (`connector/run.rs`, `target.rs`,
  `ManifestConnector.doubled_accounts`), `label_in`, `chain_decision` and
  `RunJournal.run_dir` (the decisions report); and in "Concepts that span
  files" a short "Connector doubles" paragraph (every dispatched call of
  a doubled run checked then answered or `double_unmatched`, no secret,
  no notification, a manifest with account names only, the guard
  `eval_doubles` written with the provenance of a doubled start, a
  declared `irreversible` still refused).
- [x] 5.2 Write `docs/superpowers/specs/<date>-eval-doubles-design.md`
  (`<date>` the day you write it, `YYYY-MM-DD`): the content of
  `proposal.md` and `design.md` as one public record, with no mention of
  `openspec/` and no reference to any outside article, author, product or
  project.
- [x] 5.3 Release notes: if `docs/release-notes/` has a file for the next
  version, add the lines of design "Release notes"; otherwise put them in
  the final report. Do not bump any version.
- [x] 5.4 Style over every changed doc file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![` and
  Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds
  nothing).

## 6. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 6.1 `cargo fmt --all -- --check`
- [x] 6.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 6.3 `(cd web && bun install --frozen-lockfile && bun run build)`
  first (apb-server embeds `web/dist`), then `cargo nextest run
  --workspace`
- [x] 6.4 The journal corpus the CI way: `APB_CORPUS_REQUIRED=1
  APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries/bin cargo nextest run -p
  apb --test main journal_corpus` (when the directory lacks a tag's
  binary, build it with `scripts/old-apb.sh <tag> <dir>` for `v0.25.0`
  and `v1.0.0-beta.1`, the tags `.github/workflows/ci.yml` names)
- [x] 6.5 `cargo test --workspace --doc`
- [x] 6.6 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; no mirrored type changes in this change)
- [x] 6.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 6.8 `scripts/third-party-notices.sh --check` (no dependency changes
  are planned; it must pass unchanged)
- [x] 6.9 `openspec validate eval-doubles --strict`
- [x] 6.10 `cargo clippy --release`: passes when it prints no error, and
  no warning in a file this change touches that `cargo clippy --release`
  on main efae5822 does not also print (run it on main first in a clean
  worktree and keep the warning list; compare by file and lint)

## 7. Close

- [x] 7.1 Remove the `$TMPDIR` contents you created; run `cargo sweep
  --time 7` on `$CARGO_TARGET_DIR`. Stop only the processes you started
  (check `pgrep -f __drive`).
- [x] 7.2 Final report: per section what was built, every place where the
  code differed from `design.md` and what you did, the answers to 0.1 to
  0.3, each gate of section 6 with its numbers, the user-visible changes
  for the release notes (design "Release notes"), the changed file count
  (design "Code map" forecasts 44), and anything left open.

## 8. Fix round 1 (the implementation review)

- [x] 8.1 The identity function of a `marker` send in `ConnectorFacts` and
  `bound_functions`; an engine test of a doubled marker read-back.
- [x] 8.2 The run directory synced when the capture file is created, tested
  through a failing directory sync before dispatch.
- [x] 8.3 An external-data repetition stops its processes before its
  capture goes; the kept path tested with the regular-file recipe, a live
  driver and a discard.
- [x] 8.4 The capture read beneath the run directory; list item ids
  scrubbed.
- [x] 8.5 A keep needs a person's channel; a patch while the gate waits
  leaves the reply unlabelled.
- [x] 8.6 The report's figures over retry advice; a real run labelled keep
  through the report and the replay.
- [x] 8.7 The engine tests run the checks and read the run outcome.
- [x] 8.8 A notification of a run whose settings do not read names the
  settings.
- [x] 8.9 Records, docs and the minors of the review; every gate of
  section 6 again.
- [x] 8.10 Recheck: the stop on every unix with the discard fallback, the
  directory sync before a replayed dispatch, the no-follow mode reset,
  the bounded reap poll.
- [x] 8.11 Code review: the journal cross-check of the capture, the stop by
  the recorded driver group with the discard without `/proc`, item
  decisions unlabelled.
- [x] 8.12 Second code review: no group signalled by number without a
  driver handle, the capture created at preparation, a doubled call the
  journal cannot record refused.
- [x] 8.13 Third code review: the stop through pidfds only, an unproven
  stop discards the repetition.
- [x] 8.14 Fourth code review: the process stop is cut; an
  `--external-data` repetition with doubles is never kept (design "Cut").
  The stop work of 8.3, 8.10, 8.11 (its stop part) and 8.13 is superseded.
- [x] 8.15 Fifth code review: the removal through descriptors only, a moved
  run tree reported; the boundary of the never-kept rule written down.
- [x] 8.16 Own review c5: a replay recorded in the capture, the scratch
  guard never keeps an external-data repetition with doubles.
- [x] 8.17 Recheck of round 7: the success text and the rule say only what
  holds, messages name what was found, capture removal errors reported,
  `remove_capture` removed.

## 9. Close the change

- [x] 9.1 The public design record
  `docs/superpowers/specs/2026-11-20-eval-doubles-design.md` matches the
  final proposal and design (the never-kept rule of an `--external-data`
  repetition with doubles).
- [x] 9.2 The change archived with a Purpose for each new spec;
  `openspec validate --all --strict` passes.

## 10. Pull request review

- [x] 10.1 The macOS `ignore` back on the test with a name that is not
  UTF-8; the permission walk holds one descriptor per level and fails on
  `EMFILE` instead of skipping (`make_dirs_private_repairs_a_wide_tree`).
- [x] 10.2 A doubled dry run returns a preview naming the connector and
  account (`doubled: true`), never a rendered request.
- [x] 10.3 A connector entry with no function is a V280 gap; a binary
  directory that holds the `PATH` separator refuses the repetition.
- [x] 10.4 A capture append whose write or sync fails is cut back
  (`append_restoring`); an unreadable capture reports its calls as
  unknown (`calls_unreadable`); a started external-data eval says why
  its result is not stored.
- [x] 10.5 Docs: scrubbed captured arguments, secrets read only to scrub,
  the `reply/1` report figures, the v2 thresholds file and a person's own
  `completion_check` cut.
- [x] 10.6 Tests: the helper stopped on a panic and bounded, the
  permission probe before the eval, the fault tests skipped where a mode
  does not bind, the gate poll reporting an early run end.
- [x] 10.7 Review round 2: a listing that fails for want of a descriptor
  fails the permission walk; a failed line that cannot be cut back is
  followed by a line that never parses, so the capture fails closed; the
  fault tests also skip where a directory of mode 0300 still lists.
