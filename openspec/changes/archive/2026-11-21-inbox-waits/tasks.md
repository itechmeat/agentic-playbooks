# Tasks: inbox waits

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by
the name `design.md` gives, keep the decision, and note the difference
in the final report. Never invent a different behaviour, and never add
what design "Cut" and "Cut in the third pass" removed (a consumer, an
ack, a waiter file, a listener post, a retirement, a second park reason,
a batch window, an event body in the journal).

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet; the crash tests must fail on a real assertion before the code
that makes them pass exists. Each task lists its tests first; write
them before the code of that task.

Line numbers are those of main efae5822, re-checked on main 3bb424be
(where `eval-doubles` is merged) for the lines the fourth review pass
touched. If `start-rules-and-schedules` merged first, lines moved:
find every site by name.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=8 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export TMPDIR=$PWD/.tmp-luna
mkdir -p "$TMPDIR"
```

Cargo's target directory stays at `./target`; do not set `CARGO_TARGET_DIR`.

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

Inside a task run only the tests it names:

1. A unit test module: `cargo test -p <crate> --lib <module path>`.
2. An integration test: `cargo test -p <crate> --test main <file
   module>::<test>`.
3. Web: `cd web && bun run test -- <file>`.

## 0. Preconditions (design D0)

- [x] 0.1 The current main includes eval-doubles (#291) and this branch is
  rebased on it; rebase again only if main moved. Then confirm
  each symbol exists in the named file, and stop with a report if one
  does not:
  1. `RunRedactor::for_storing` and `RunRedactor::secret_json` in
     `crates/apb-engine/src/run_redact.rs`;
  2. `pub fn refusal` with its node loop in `crates/apb-core/src/eval.rs`;
  3. the `<node>#inbox` doc examples in
     `crates/apb-core/src/eval_doubles.rs` (`site` field) and
     `crates/apb-engine/src/eval/doubles.rs` (left as they are);
  4. `Inbox::read_events`, `Inbox::open`, `fn lost_prefix` (private) and
     `InboxEvent.event_type` in `crates/apb-core/src/connector/inbox.rs`;
  5. `WebhookSpec::names_event_type` in
     `crates/apb-core/src/connector/def.rs`; `ConnectorFacts` deriving
     `Default` in `crates/apb-core/src/connector/resolve.rs`;
  6. `Playbook::connector_sites` and `site_label` in
     `crates/apb-core/src/connector/sites.rs`; `check_site` in
     `crates/apb-core/src/list/mod.rs`; `resolve_playbook` and
     `expand_grant` in `crates/apb-core/src/connector/resolve.rs`;
     `config::load_merged` and `config::account_digest` in
     `crates/apb-core/src/connector/config.rs`; `store::load` in
     `crates/apb-core/src/connector/store.rs`; `TrustStore::is_approved`
     in `crates/apb-core/src/trust.rs`;
  7. `parse_path` and `extract` in `crates/apb-core/src/baseline/extract.rs`;
     `Observe`, `ObservationType` and `MAX_ITEM_CHARS` in
     `crates/apb-core/src/baseline/mod.rs`; `declared_fields` in
     `crates/apb-core/src/validate/node_io.rs`;
  8. `snapshot_connectors` in `crates/apb-engine/src/connector/run.rs`;
     `Manifest::grants_for` and `Manifest::connector` in
     `crates/apb-engine/src/manifest.rs`;
  9. `pub(crate) fn journal_gap` in
     `crates/apb-engine/src/connector/inbox.rs`;
  10. `EventLog::append_pair` and `EventPayload::is_checkpoint` in
      `crates/apb-engine/src/event.rs`;
  11. `wait_started_count`, `wait_ended_count`, `wait_active_ms` in
      `crates/apb-engine/src/scheduler/journal.rs`;
  12. `ParkCheck { timer, .. }`, `park_request`, `due_at_ms`, `ParkRetry`
      in `crates/apb-engine/src/scheduler/wait_park.rs`;
      `fn wait_timer_of` in `crates/apb-engine/src/scheduler/control_apply.rs`;
  13. `run_waker` and `pause_at_is_wait` in
      `crates/apb-engine/src/pause/waker.rs`; `wait_reason`,
      `parse_wait_reason` in `crates/apb-engine/src/pause/cause.rs`;
      `load_run_playbook` in `crates/apb-engine/src/legacy_snapshot.rs`;
  14. `enum Hook` in `crates/apb-engine/src/deadline_hooks.rs`;
  15. `crate::context::render` in `crates/apb-engine/src/context.rs`.
- [x] 0.2 Confirm the validator codes are free on main: `git grep -E
  '"V28[4-7]"' origin/main -- crates` prints nothing. Confirm
  `GuardedFeature::WaitPark` is named for a timer or inbox wait at `guarded_features` in
  `crates/apb-core/src/validate/guarded.rs`.
- [x] 0.3 Confirm the corpus rows W1 and W2 pass before any change:
  `cargo test -p apb --test main journal_corpus` (W1 and W2 pass individually;
  the corpus command also selects unrelated rows that fail in this sandbox).

## 1. apb-core (design D4 "Schema", "Authorization", "Validator", "Untrusted text", "Evals and scenarios"; spec "A wait for an inbox event", "The wait is a trusted binding site", "Only declared values are stored", "Validator codes")

- [x] 1.1 Tests: `schema.rs` `an_inbox_wait_parses`,
  `an_inbox_wait_with_an_unknown_key_is_refused`,
  `an_inbox_wait_without_a_timeout_is_a_parse_error`. Code: new
  `crates/apb-core/src/inbox_wait.rs` with the types and constants of
  design D4 "Schema" (function bodies left as the `todo` macro for now),
  `pub mod inbox_wait;` in `lib.rs`, `WaitFor::Inbox(Box<InboxWait>)`.
  Fix every exhaustive `match` over `WaitFor` the compiler names
  (`next_action/deadlines.rs`, `scheduler.rs`) with the arms of design
  D4 "Surfaces" item 3 and "The wait arm" item 1 (the inbox arm of
  `scheduler.rs` may stay the `todo` macro until 2.6).
- [x] 1.2 Tests in `inbox_wait.rs`: `a_plain_wait_matches_every_event`,
  `conditions_compare_rendered_text`,
  `type_passes_needs_a_type_when_listed`,
  `output_keeps_only_declared_values`, `an_unreadable_value_is_null`,
  `inbox_site_round_trips`. Code: `inbox_site`, `inbox_site_node`,
  `binding`, `matches`, `type_passes`, `value_text`, `output`.
- [x] 1.3 Test in `connector/sites.rs`: `the_inbox_site_is_its_own_site`.
  Code: the `<node>#inbox` sites in `connector_sites` and their label in
  `site_label` (design D4 "Authorization").
- [x] 1.4 Tests in `connector/inbox.rs`: `log_stamp_changes_on_append`,
  `log_stamp_is_none_without_a_log`,
  `concurrent_deliveries_are_stamped_in_seq_order`,
  `a_delivery_waiting_on_the_lock_is_stamped_when_it_gets_it`,
  `received_at_never_goes_back`; the existing `inbox_store_test` and
  `connector_inbox` tests must stay green unchanged. Code:
  `Inbox::log_stamp`; `append_at` with the clock read under the lock
  and `append_with`, `append_typed` and `append_bounded` over it
  (design D4 "The deadline", receipt);
  `lost_prefix` made `pub`; the module-doc sentence of design D4 "The
  visit".
- [x] 1.5 Tests in `validate/inbox_wait.rs`: one per V284 rule,
  `the_site_accepts_hyphenated_connector_and_account`,
  `v285_refuses_a_connector_that_is_not_installed`,
  `v285_needs_a_webhook_connector`,
  `v285_is_silent_without_a_connector_store`,
  `v286_warns_on_types_without_a_type_path`,
  `v287_refuses_a_node_named_like_an_inbox_site`,
  `v287_refuses_a_node_named_like_a_check_site`,
  `v287_is_silent_for_distinct_sites`. Code: the module with
  `check_inbox_waits` and `check_site_ids` (design D4 "Validator"
  item 4), its
  declaration and call in `validate/mod.rs`,
  `ConnectorFacts.names_event_type` in `connector/resolve.rs`, the
  conditions in `schema.rs` `template_texts`.
- [x] 1.6 Test in `validate/node_io.rs`:
  `an_inbox_wait_declares_its_taken_fields`. Code: the inbox arm of
  `declared_fields`.
- [x] 1.7 Test in `validate/guarded.rs`: `an_inbox_wait_names_wait_park`.
  Code: the `Timer | Inbox` condition.
- [x] 1.8 Tests in `untrusted.rs`: `an_inbox_wait_output_taints_its_reader`,
  `an_inbox_wait_output_taints_a_reader_declared_before_it`. Code: the
  wait's source inserted inside `analyze`'s loop beside rule 2 (never
  seeded before it), `carries_text`.
- [x] 1.9 Test in `eval.rs`: `an_eval_case_refuses_an_inbox_wait`.
  Code: the rule in `refusal`.
- [x] 1.10 `cargo test -p apb-core --lib` once for the touched modules.

## 2. apb-engine (design D4 "Authorization", "The position", "The visit", "The wait arm", "An ended visit", "Parking"; spec "The wait is a trusted binding site" to "Retention gaps are journaled before the position moves")

- [x] 2.1 Code: `EventPayload::InboxWaitRead` in `event.rs` with the
  fields of design D4 "The visit"; the no-op arm in `state.rs`; one line
  in `tests/fixtures/journal/known_events.jsonl`. Test:
  `cargo test -p apb-engine --test main journal_compat_test`.
- [x] 2.2 Code: `Hook::WaitBeforeNodeFinished` and
  `Hook::InboxWaitBeforePark` in `deadline_hooks.rs`, and their call
  sites (the shared tail right before a wait's `node_finished`; the park
  block right after the progress line). Skeleton
  `crates/apb-engine/src/inbox_wait/mod.rs` and `pending.rs` (functions
  return `Poll::Open`, `Ok(())`, `false`, `None`), `pub mod inbox_wait;`
  in `lib.rs`, the test module in `tests/main.rs`.
- [x] 2.3 Tests in `scheduler/journal.rs`: `wait_visit_reads_positions`.
  Tests in `wait_test.rs` (they pass on main already and must stay
  green):
  `a_timer_wait_ended_before_node_finished_waits_again_as_on_main`,
  `a_webhook_wait_ended_before_node_finished_waits_again_as_on_main`,
  `a_timer_wait_writes_the_journal_main_writes`. Code: `WaitVisit`,
  `wait_visit`, and the wait arm's guard over it for inbox waits only;
  timer and webhook waits keep main's count guard; for inbox waits,
  `[wait_started, node_finished, wait_started]` is `Open` (design D4
  "An ended visit").
- [x] 2.4 Tests in `inbox_wait/mod.rs`: `position_is_the_latest_read_line`,
  `position_ignores_a_torn_start_line`,
  `in_time_reads_the_receipt_on_the_paused_interval_clock`. Code:
  `position` with its counting rule (design D4 "The position") and
  `in_time` over `wait_park::due_at_ms` (design D4 "The deadline").
- [x] 2.5 Code: `journal_gap_in` with `GapScope` split out of
  `journal_gap` in `connector/inbox.rs` (`journal_gap` passes
  `GapScope::Account`: no behaviour change; the existing
  `connector_inbox` tests must pass:
  `cargo test -p apb-engine --test main connector_inbox`).
- [x] 2.6 Tests in `inbox_wait_test.rs` (all red first):
  `an_unapproved_account_refuses_the_start`,
  `a_connector_that_is_not_installed_refuses_the_start`,
  `the_manifest_records_the_inbox_site`,
  `a_changed_account_fails_the_wait_on_resume`. Code: `authorize` and
  its calls in `start` and `poll` step 2 (design D4 "Authorization").
- [x] 2.7 Tests in `inbox_wait_test.rs` (all red first):
  `a_wait_ends_at_its_first_matching_event`,
  `the_output_is_the_declared_values`, `the_output_keeps_the_event_type`,
  `a_timeout_without_a_match_times_out`,
  `an_event_received_before_the_deadline_matches_when_polled_late`,
  `an_event_received_after_the_deadline_is_left_for_the_next_visit`,
  `a_delivery_that_waited_on_the_lock_across_the_deadline_is_late`,
  `a_failed_read_is_retried_without_a_new_write`,
  `a_transient_head_read_error_is_retried`,
  `a_pause_moves_the_deadline_by_its_length`,
  `events_before_the_wait_are_below_its_start_line`,
  `events_after_the_match_stay_for_the_next_visit`,
  `a_second_visit_counts_only_newer_events`,
  `two_runs_wait_on_one_account_independently`,
  `a_wait_leaves_the_inbox_directory_unchanged`,
  `an_idle_wait_reads_only_when_the_log_changes`,
  `a_scripted_inbox_wait_ends_with_an_empty_object`. Code: `Scan`,
  `start`, `poll` steps 3, 5, 6 and 7, and the wait arm items 1 to 3 of
  design D4.
- [x] 2.8 Tests in `inbox_wait_test.rs`:
  `a_retention_gap_is_journaled_once`,
  `a_gap_after_an_empty_start_is_journaled`,
  `a_failed_gap_append_does_not_advance_the_position`,
  `a_gap_is_journaled_before_the_end_pair`,
  `a_gap_another_node_recorded_is_journaled_for_the_wait`,
  `a_connector_read_gap_rule_is_unchanged`. Code: `poll` step 4, with
  the wait node's id as the gap's `node_id` and `GapScope::Node`.
- [x] 2.9 Tests in `inbox_wait_test.rs`:
  `an_undeclared_field_never_reaches_the_journal`,
  `a_short_known_secret_in_a_declared_field_is_the_stated_limit`,
  `an_unresolved_secret_stores_nothing_and_fails`,
  `a_command_sourced_secret_stores_no_value` (the last three use
  `RunRedactor::for_storing`, on main since eval-doubles). Code: the redactor
  step of `poll` step 5.
- [x] 2.10 Tests in `inbox_wait_test.rs`: `the_end_is_one_write`,
  `an_inbox_wait_ended_before_node_finished_keeps_its_output`,
  `a_crash_before_the_end_reads_the_same_events_again`,
  `a_start_line_without_wait_started_is_superseded`,
  `a_journaled_end_is_finished_without_authorization`,
  `a_failed_inbox_wait_lets_the_next_visit_start`. Code: `poll` step 1
  and the inbox output of `WaitVisit::Ended`, both without `authorize`
  (design D4 "Authorization"), and the arm's `start` after a torn start
  line, which `position` from 2.4 already reads as `None` (design D4
  "The position", crash table row 2).
- [x] 2.11 Tests in `scheduler/wait_park.rs`: `park_seconds_by_kind`,
  `an_inbox_wait_parks_only_caught_up`; in
  `scheduler/control_apply.rs`:
  `an_inbox_park_is_re_decided_with_its_timeout`. Code:
  `ParkCheck.parks`, `park_seconds`, `park_wait_of`, the progress line
  and the `ParkRetry` length of design D4 "Parking" items 1 to 3.
- [x] 2.12 Tests in `inbox_wait/pending.rs`:
  `delivery_pending_compares_with_the_position`,
  `delivery_pending_reads_only_when_the_log_changed`,
  `delivery_pending_applies_the_types`,
  `delivery_pending_wakes_for_a_gap`,
  `delivery_pending_ignores_a_timer_park`,
  `delivery_pending_ignores_a_late_event`,
  `delivery_pending_retries_a_failed_read`; in `pause/waker.rs`:
  `an_inbox_park_waker_resumes_on_a_new_event`. Code:
  `delivery_pending` and its call in `run_waker` (design D4 "Parking"
  item 6).
- [x] 2.13 Tests in `inbox_wait_test.rs`:
  `a_long_inbox_wait_parks_with_the_timer_reason`,
  `a_delivery_resumes_a_parked_wait`,
  `an_event_stored_before_the_waker_started_resumes_the_run`,
  `a_parked_wait_over_unmatched_events_stays_parked`,
  `the_waker_does_not_resume_for_an_unauthorized_account`,
  `an_inbox_park_is_no_intervention_and_not_paused_time`. Code: whatever
  2.11 and 2.12 left; no new mechanism.
- [x] 2.14 Tests: update the park wording assertions in
  `pause/display.rs`, `next_action/tests.rs` (lines 637 and 672) and
  `tests/suite/report_html_test.rs` (line 461) to design D4 "Surfaces"
  item 2. Code: the neutral lines in `pause/display.rs` and
  `next_action/verdict.rs`, the inbox action text in `verdict.rs`.
- [x] 2.15 `cargo test -p apb-engine --lib` for the touched modules,
  then two commands, one test file each:
  1. `cargo test -p apb-engine --test main inbox_wait_test`
  2. `cargo test -p apb-engine --test main wait_test`

## 3. Surfaces, docs and records (design D4 "Surfaces", "Docs"; D0 "An older apb")

- [x] 3.1 Test: the `run_wait` park test in `apb-mcp/src/tools/run.rs`
  (line 1479) with the new text. Code: the text at line 829.
- [x] 3.2 Test: `apb-cli/src/pause.rs` (line 134) with the new note.
  Code: the note at line 85.
- [x] 3.3 Test: rows I1 to I3 in
  `crates/apb-cli/tests/suite/journal_corpus_test.rs` (design D4
  "Tests"), with their lines in the module doc beside W1 and W2.
- [x] 3.4 Test: `web/src/lib/pause.test.ts` (the park line, the cause
  line). Code: `web/src/lib/pause.ts`.
- [x] 3.5 Docs of design D4 "Docs" items 1 to 5 (item 1 says that
  collecting several events in one wait is future work and that a loop
  around a wait already expresses it).
- [x] 3.6 Records: `docs/superpowers/specs/2026-11-20-inbox-waits-design.md`
  (the design in public form, no review record); the module map lines
  in `CLAUDE.md` and the same lines in `AGENTS.md` (mirror rule):
  `apb_core::inbox_wait` (the site `<node>#inbox`, the matching and the
  declared values), `connector/sites` the inbox wait site,
  `validate/inbox_wait` V284 to V287, `apb_engine::inbox_wait` (`mod`
  the authorization, the visit, `position` and `in_time`, `pending` the park
  waker's check), `scheduler/journal` `wait_visit`.

## 4. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 4.1 `cargo fmt --all -- --check`
- [x] 4.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 4.3 `(cd web && bun install --frozen-lockfile && bun run build)`
  first (apb-server embeds `web/dist`), then `cargo nextest run
  --workspace`
- [x] 4.4 The journal corpus the CI way: `APB_CORPUS_REQUIRED=1
  APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries/bin cargo nextest run -p
  apb --test main journal_corpus` (when the directory lacks a tag's
  binary, build it with `scripts/old-apb.sh <tag> <dir>` for the tags
  `.github/workflows/ci.yml` names); W1 and W2 unchanged, I1 green
- [x] 4.5 `cargo test --workspace --doc`
- [x] 4.6 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; this change mirrors no type, so it passes unchanged)
- [x] 4.7 `cd web && bun run check && bun run test && bun run build`
- [x] 4.8 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 4.9 `scripts/third-party-notices.sh --check` (no dependency
  change; it must pass unchanged)
- [x] 4.10 `openspec validate inbox-waits --strict`
- [x] 4.11 `cargo clippy --release`: no error, and no warning in a file
  this change touches that the same command on the base main does not
  also print (run it on that main first in a clean worktree and keep the
  warning list; compare by file and lint)

## 5. Close

- [x] 5.1 Remove the `$TMPDIR` contents you created; run `cargo sweep
  --time 7` on `$CARGO_TARGET_DIR`. Stop only the processes you started
  (check `pgrep -f __drive` and `pgrep -f __pause-waker`).
- [x] 5.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to
  0.1 to 0.3, each gate of section 4 with its numbers, the user-visible
  changes (design "Release notes"), the changed file count (design "Code
  map" forecasts 49), and anything left open.

## 6. Implementation review, round 1

Design "Review record", "Implementation review" (round 1); its rules are folded into the design body. Every code change
starts from a test that fails for the reason the rule names.

- [x] 6.1 R1: a gap that cannot be journaled stops the read; the seam
  and its test; the vacuous lock test replaced
- [x] 6.2 R2: the final read before a timeout end; the late-read
  fixture; the two-event case; the two existing tests rewritten on it
- [x] 6.3 R3: conditions through `JudgeInputs`; `render_equals` removed
- [x] 6.4 R4: the three read states in the scan and in `pending`; the
  read count test; the drive's failed read test without a write
- [x] 6.5 R5: a patch drops the scans; the two tests
- [x] 6.6 R6: the site kind; `site_label(&str)` and `inbox_site_node`
  removed; the two tests
- [x] 6.7 R7: V284 over the filter's shape rules; one type predicate
- [x] 6.8 R8: the scripted wait call order; the timer test
- [x] 6.9 R9 items 1 to 5
- [x] 6.10 Section 4 gates again, all of them, with numbers

## 7. Implementation review, round 2

Design "Review record", "Implementation review" (round 2); its rules are folded into the design body. Test first, as in
section 6.

- [x] 7.1 U1: one "definition replaced" signal; the batch test
- [x] 7.2 U2: labels from the `Site` in the gate and the resolver; the
  `x#check` half of the V302 test removed
- [x] 7.3 U3: the read interval after a failed read; the test
- [x] 7.4 U4: the gap seam keyed by run directory with a guard
- [x] 7.5 U5 items 1 to 7
- [x] 7.6 U6: the module map, the same edit in both files
- [x] 7.7 U7: every gate with numbers; the detached driver test

## 8. Implementation review, round 3

Design "Review record", "Implementation review" (round 3); its rules are folded into the design body. Tests only.

- [x] 8.1 Y1: the lower bound of the read interval test
- [x] 8.2 Y2: the final read tests fail the final read only and count
  the final reads
- [x] 8.3 The changed tests twenty times each; `cargo fmt --all --
  --check` and `cargo clippy --workspace --all-targets -- -D warnings`

## 9. Close the change

- [x] 9.1 The three implementation review rounds folded into the design
  body (no appended section overrides another; a short record kept under
  "Review record")
- [x] 9.2 The public design record under `docs/superpowers/specs`
  matches the final design
- [x] 9.3 `probation/` in the committed `.apb/.gitignore`
- [x] 9.4 The change archived; a Purpose for the new spec;
  `openspec validate --all --strict` passes
- [x] 9.5 The branch squashed into one commit
