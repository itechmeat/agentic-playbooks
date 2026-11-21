# Tasks: start rules

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved, an in-flight change
merged first), find the site by the name `design.md` gives, keep the
decision, and note the difference in the final report. Never invent a
different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet; the crash and race tests must fail on a real assertion before
the code that makes them pass exists. So every section builds in this
order: types and changed signatures first (each step compiles), then a
skeleton whose functions do nothing where a test file needs them to
exist, then the red tests, then the behaviour one piece at a time.

Line numbers are those of main efae5822. `eval-doubles` and `inbox-waits`
may have merged before this is built: design "The in-flight changes this
one must not conflict with" says which symbol each owns. This change no
longer shares `Inbox::retire`: it is inbox-waits' alone.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-start-rules-target TMPDIR=$HOME/.cache/apb-start-rules-tmp
mkdir -p "$TMPDIR"
```

Never use `/tmp` or `/dev/shm` for a target dir, cache or temp data, and
never put them inside the worktree. Before the first web command run
`(cd web && bun install --frozen-lockfile)` if `web/node_modules/.bin/vite`
is missing; it needs network access. If it fails, skip the web steps and
report it.

Never touch the real `~/.config/apb`; tests that need a config dir get
one under `$TMPDIR` (the existing test helpers do this). Never stop a
process you did not start. Do not commit, push or open a pull request.
Do not start subagents. Search code with `zg --rg -F <symbol> <path>`
before opening files.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-core --lib trigger::render`).
2. An integration test file: `cargo test -p <crate> --test main <file module>`
   (for example `cargo test -p apb-engine --test main trigger_test`). The
   CLI package is named `apb`: `cargo test -p apb --test main trigger_cli_test`.
3. `cargo test` takes one name filter per invocation: two modules are two
   commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` (see
`crates/apb-engine/tests/suite/start_key_test.rs` for a project fixture,
`crates/apb-engine/tests/suite/connector_contract_inbox.rs` for an inbox
fixture, `crates/apb-engine/tests/suite/wait_test.rs` for a timer wait and
`crates/apb-server/tests/suite/ingest_test.rs` for a signed delivery).

After any change to a type listed in `crates/apb-server/src/ts_contract.rs`
regenerate TypeScript: `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`,
then `cargo test -p apb-server --lib ts_contract` (must pass without the
variable).

Web work uses plain CSS over the existing CSS variables only.

## 0. Preconditions

- [x] 0.1 Find each symbol in its exact file and note the line (one
  `zg --rg -F` each); if one is missing, stop and report it:
  1. `pub enum Initiator`, `pub enum LaunchConfirmation`, `pub fn decide`,
     `fn launch_confirmation`, `pub(crate) fn billing_confirmation`,
     `fn refusal`, `pub const ASK_POLICY`, `pub fn run_authorization` in
     `crates/apb-engine/src/gate/launch.rs`;
  2. `pub const BILLING_ENGINE` in `crates/apb-engine/src/gate/billing.rs`;
  3. `pub struct LaunchRecord` and `pub struct StartedBy` in
     `crates/apb-core/src/launch.rs`;
  4. `pub struct InboxEvent`, `pub fn read_events`, `pub fn min_cursor`,
     `fn enforce_retention` (and its `let acked_through`), `max_age_ms`
     of `Retention::default`, `fn last_seq`, `pub fn read_filtered`,
     `pub fn append_typed` in `crates/apb-core/src/connector/inbox.rs`;
  5. `pub struct EventFilter`, `pub struct ConditionResult`,
     `pub fn explain` in `crates/apb-core/src/connector/event_filter.rs`;
  6. `const KEYWORD` and `pub fn render` in
     `crates/apb-core/src/connector/marker.rs`;
  7. `pub fn scalar_at_path` and `pub fn value_at_path` in
     `crates/apb-core/src/connector/common.rs`;
  8. `pub trait SignedDoc`, `pub fn transact`, `pub fn read`,
     `pub enum Unsigned` in `crates/apb-core/src/signed_store.rs`;
  9. `pub fn try_hold_lock`, `pub fn hold_lock_waiting`,
     `pub fn lock_dir` in `crates/apb-core/src/fsutil.rs`;
  10. `pub fn parse`, `pub fn key_tag`, `pub struct StartRequestFields`,
      `pub const DEFAULT_DAYS` in `crates/apb-core/src/start_key.rs`;
  11. `pub fn registered_id`, `pub fn bind_id`, `pub fn touch`,
      `fn registration_disabled` and `pub fn list_active` in
      `crates/apb-core/src/projects.rs`; `projects::touch` in `main` of
      `crates/apb-cli/src/main.rs`; `pub fn workdir_queue_wait` in
      `crates/apb-core/src/config.rs`;
  12. `pub const LOCAL_ENTRIES` and
      `fn the_enforcement_state_directories_are_local_entries` in
      `crates/apb-core/src/workspace.rs`;
  13. `pub fn checked_run` in `crates/apb-engine/src/concurrency.rs`;
  14. `opts.unattended = super::unattended::mode` in
      `crates/apb-engine/src/scheduler/node.rs` (a child inherits its
      parent's mode) and `pub fn counted` in `crates/apb-core/src/probation.rs`
      (it counts `!e.unattended` only); if either does not hold, stop and
      report it;
  15. `pub fn check_probation` and `pub fn check_start` in
      `crates/apb-engine/src/gate/unattended.rs`; `pub require_ready` and
      `pub waive_probation` in `crates/apb-engine/src/scheduler/entry.rs`;
      `pub fn start_detached` and `pub fn start_detached_resolved` there;
  16. `fn resume_while` in `crates/apb-engine/src/pause/waker.rs` and
      `pub fn check_engine_resume` in `crates/apb-engine/src/resume_launch.rs`;
  17. `pub fn started_by` and `pub fn started_by_line` in
      `crates/apb-engine/src/run_view/started.rs`;
  18. `pub struct StartArgs`, `pub fn start_options` (and its
      `workdir_queue_wait: None`), `pub fn keyed_lookup`
      in `crates/apb-mcp/src/tools/run.rs`; `workdir_queue_wait:` in
      `crates/apb-server/src/routes/playbooks.rs`; `pub fn preflight` in
      `crates/apb-mcp/src/tools/budget.rs`; `pub fn start_check` in
      `crates/apb-mcp/src/tools/notify.rs`; `fn playbook_run_for` in
      `crates/apb-mcp/src/server/run.rs`; `fn tool_router` in
      `crates/apb-mcp/src/server/mod.rs`; `pub const READ_ONLY` in
      `crates/apb-mcp/src/server/read_only.rs`;
  19. `async fn post_hook_handler` (and its `Appended::Stored` arm) and
      `pub async fn run_ingest_server` in `crates/apb-server/src/ingest.rs`;
      `pub(crate) fn spawn` in `crates/apb-server/src/health_sweep.rs`;
  20. `fn resume_all_cmd` in `crates/apb-cli/src/run.rs`;
      `Health(crate::health::HealthArgs)` in `crates/apb-cli/src/main.rs`;
      `fn spawn_ingest_if_enabled` in `crates/apb-cli/src/serve.rs`;
  21. `let guarded = crate::manifest::manifest_guard` in
      `crates/apb-engine/src/scheduler/prepare.rs`; `pub const GUARD_SUFFIX`,
      `pub const EXTERNAL_SUFFIX`, `fn base`, `pub fn for_run`,
      `pub fn manifest_guard` and `pub fn execution_mode` in
      `crates/apb-engine/src/manifest.rs`; `pub fn parse` of
      `ExecutionMode` in `crates/apb-core/src/execution.rs`;
      `manifest.is_host_mode()?` in `crates/apb-engine/src/scheduler/resume.rs`;
      `pub record` of `pub struct LaunchGrant` in
      `crates/apb-engine/src/gate/launch.rs`; `pub fn read_lifecycle` and
      `pub enum Lifecycle` in `crates/apb-core/src/trust.rs`;
      `pub fn from_env` of `StepCaller` in `crates/apb-core/src/step_env.rs`;
      the fault points of `crates/apb-core/src/versioning/current/fault.rs`;
  22. `fn entry_of` and `let from_step` in
      `crates/apb-engine/src/probation.rs`; `const NO_LINKS` and
      `fn belongs_to` of `ProbationFile` in `crates/apb-core/src/probation.rs`
      (the pattern the two signed files follow; that file does not change);
  23. `pub fn tag`, `pub fn tag_if_keyed` and `pub fn ensure_key` in
      `crates/apb-core/src/run_origin.rs`; `pub fn lookup` and `fn judge`
      in `crates/apb-engine/src/start_key.rs`;
  24. `pub(crate) fn ask_typed` in `crates/apb-cli/src/consent.rs`;
      `fn repair_tail` in `crates/apb-engine/src/review.rs`;
      `pub fn permit_digest`, `pub fn check_run` and `fn admit` in
      `crates/apb-engine/src/gate/mod.rs`; `pub fn child` of `LaunchGrant`
      in `crates/apb-engine/src/gate/launch.rs` (it keeps recording no
      rule); `pub parent_run` in
      `crates/apb-engine/src/run_config.rs`; `pub(crate) fn created_here`
      and `pub(crate) fn candidate_run_dirs` in
      `crates/apb-engine/src/decision/report/mod.rs`;
      `pub fn started_runs` and `STARTED_RUNS_SCAN` in
      `crates/apb-engine/src/run_view/started.rs`;
      `pub(crate) fn newest_run_dirs_where` in
      `crates/apb-engine/src/decision/report/mod.rs`;
      `pub enum RequestChannel`
      in `crates/apb-server/src/auth.rs`; `fn init_on_tty` in
      `crates/apb-cli/tests/init_interactive_test.rs`;
  25. `pub struct ConditionResult` (its `condition` and `got`) in
      `crates/apb-core/src/connector/event_filter.rs`.
- [x] 0.2 `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u` shows no code
  from V260 to V279, and `git grep -n rule_start crates` finds nothing. V280, V281 (eval-doubles) and V284 to V286
  (inbox-waits) are expected if those merged. If a code of V260 to V279
  appears, stop and report it.
- [x] 0.3 None of these exists: `crates/apb-core/src/trigger/`,
  `crates/apb-engine/src/trigger/`, `crates/apb-server/src/trigger_sweep.rs`,
  `crates/apb-server/src/routes/triggers.rs`; `Inbox` has no `head_seq`. `Inbox::retire` exists only if inbox-waits
  merged; this change neither adds nor calls it. If another one exists, it is a partial earlier
  attempt: review it against `design.md`.

## 1. Shared pieces (design D0)

- [x] 1.1 `crates/apb-core/src/workspace.rs` `LOCAL_ENTRIES`: add
  `"triggers/"` and `"trigger-state/"` after `"runs-archive/"` with
  one-line comments, and both to
  `the_enforcement_state_directories_are_local_entries` first. Run
  `cargo test -p apb-core --lib workspace`.
- [x] 1.2 `TriggeredBy` and `LaunchRecord.triggered_by` in
  `crates/apb-core/src/launch.rs` exactly as design D0 shows; add
  `triggered_by: None` to every `LaunchRecord {` literal
  (`rg -n 'LaunchRecord \{' crates`). `cargo build --workspace`.
- [x] 1.3 Tests first, unit in `crates/apb-engine/src/gate/launch.rs`
  `mod tests`: `a_child_grant_records_no_rule`,
  `a_rule_start_is_decided_like_a_cli_without_a_terminal`,
  `an_equal_approved_binding_confirms_an_ask_launch`,
  `a_deny_playbook_is_refused_even_with_an_approval`,
  `a_rule_billing_ask_writes_nothing`, `a_rule_grant_records_its_rule`;
  unit in `crates/apb-engine/src/gate/billing.rs`:
  `binding_lines_differ_for_allow_and_deny`;
  unit in `crates/apb-engine/src/gate/mod.rs`:
  `the_binding_is_compared_in_admission`. Then design D0 items 1 to 10
  (`LaunchContext.approved_binding`, `approval_binding` over
  `billing.binding_lines()`, the comparison in `Gate::admit`
  before `launch::decide`, `BINDING_CHANGED` and `binding_changed_of`, the
  `ask` arm confirmed by an equal approval, no launch nonce), with
  `triggered_by: None` in every other `LaunchRecord` literal,
  `LaunchGrant::child` included.
  Run
  `cargo test -p apb-engine --lib gate::` (the launch, billing and
  `gate/mod.rs` tests, `the_binding_is_compared_in_admission` included).
- [x] 1.4 Register `TriggeredBy` in `ts_contract.rs` and regenerate
  TypeScript.
- [x] 1.5 The nonmutating key lookup's core half (design D1 "`apb
  trigger test`"). Tests first: `key_tag_if_keyed_creates_no_key`
  (`crates/apb-core/src/start_key.rs` tests), `flock_held_creates_nothing`
  (`crates/apb-core/src/fsutil.rs`),
  `repair_line_tail_keeps_a_whole_tail_and_cuts_a_partial_one` (same file).
  Then `key_tag_if_keyed`, `StartRequest::capture_if_keyed`,
  `fsutil::flock_held`, and `fsutil::repair_line_tail`: the body of
  `repair_tail` in `crates/apb-engine/src/review.rs` moved there with the
  parse as a parameter, `review.rs` calling it with the parse of a
  `ReviewEntry` (its torn-tail tests must pass unchanged:
  `cargo test -p apb-engine --lib review`). Run
  `cargo test -p apb-core --lib start_key` and
  `cargo test -p apb-core --lib fsutil`.

## 2. Inbox additions (design D1 "Inbox additions for rules")

- [x] 2.1 Tests first, `crates/apb-core/src/connector/inbox.rs`:
  `head_seq_is_zero_for_an_empty_log`. Then `head_seq` (a new public
  function over the private `last_seq`, which stays private and
  unchanged). This change adds no `retire` and registers no consumer
  (design D1 "Inbox additions for rules"). Run
  `cargo test -p apb-core --lib connector::inbox`.
- [x] 2.2 Tests first, `crates/apb-core/src/connector/marker.rs`:
  `trailing_marker_reads_only_the_exact_trailing_marker`. Then
  `trailing_marker`, the inverse of `render`. Run
  `cargo test -p apb-core --lib connector::marker`.

## 3. Rules in core (design D1, spec inbox-start-rules)

- [x] 3.1 Skeleton of `crates/apb-core/src/trigger/` (`mod.rs`, `rule.rs`,
  `render.rs`, `validate.rs`, `state.rs`, `evaluate.rs`) with
  the types of design D1 and functions that return defaults;
  `pub mod trigger;` in `crates/apb-core/src/lib.rs`. `cargo build -p apb-core`.
- [x] 3.2 Tests first, `render.rs`: `a_template_renders_body_paths_and_the_event`,
  `an_unknown_placeholder_does_not_parse`, `an_object_is_not_scalar`,
  `a_missing_path_and_an_empty_string_differ`,
  `reads_event_text_sees_the_type`. Then `parse`, `render` (through
  `value_at_path`, never `scalar_at_path` alone),
  `reads_event_text`. Run `cargo test -p apb-core --lib trigger::render`.
- [x] 3.3 Tests first, `validate.rs`: `v260_a_connector_without_webhook`,
  `v260_types_without_an_event_type`, `v260_a_long_name`,
  `v261_an_unknown_version`, `v261_a_draft_target`,
  `v261_a_retired_target`, `v262_a_body_value_into_a_plain_param`
  (with its pass for `untrusted: true`),
  `v262_a_pattern_alone_does_not_pass`,
  `v262_a_secret_param`, `v262_an_unmapped_required_param`,
  `v263_no_budget`, `v263_on_no_estimate_ask`, `v264_limits_out_of_range`,
  `v265_a_write_without_a_marker`, `v266_external_data_off`,
  `v267_pause_with_external_data`. Then the
  codes, `RuleContext::load` (with `lifecycle` from
  `trust::read_lifecycle`) and `check_rule`. Run
  `cargo test -p apb-core --lib trigger::validate`.
- [x] 3.4 Tests first, `state.rs`:
  `position_at_head_replaces_another_incarnation`,
  `position_at_head_writes_the_line_first`,
  `read_shows_the_last_position_attempt`,
  `a_journal_line_holds_condition_marks_only`,
  `needs_position_for_a_missing_or_foreign_state`,
  `a_torn_tail_is_repaired_before_the_next_append`,
  `a_state_file_of_another_project_is_refused`,
  `the_journal_keeps_the_newest_thousand`, `a_torn_line_is_skipped`,
  `an_unknown_verdict_is_skipped`, `append_skips_ids_already_there`,
  `read_drops_a_repeated_id_and_folds_already_started`,
  `a_code_outside_the_closed_set_is_refused_other`,
  `the_first_line_is_durable_before_the_state` (the file, `log/` and its
  parent synced before the state transaction),
  `a_failed_sync_returns_an_error_before_anything_else` (a `cfg(test)`
  fault point, as in `versioning/current/fault.rs`). Then `state.rs`
  (`TriggerStateFile` with `project`, `RuleState` with `incarnation`,
  `source` and `seen_through` (no `start_seq`, no list of starts), its `SignedDoc`
  with `NO_LINKS = true` and `belongs_to` over `projects::registered_id`,
  `position_at_head` and `needs_position`, and the journal: `LogEntry` with its closed fields, `Verdict`,
  `journal_ids`, `append` with `sync_all` of the file and, on creation,
  the directory, and `read`, the deduping reader). Run
  `cargo test -p apb-core --lib trigger::state`. (`position_at_head` reads
  the `RuleEntry` type of the 3.1 skeleton.)
- [x] 3.5 Tests first, `rule.rs`: `a_rule_round_trips_through_yaml`,
  `an_unknown_field_is_refused`, `set_refuses_a_document_naming_enabled`,
  `each_creation_gets_its_own_incarnation`,
  `rule_digest_covers_every_firing_field`,
  `a_changed_rule_drops_its_approval`,
  `an_unchanged_set_keeps_its_approval`, `a_new_rule_is_not_approved`,
  `approve_and_enable_delete_the_state_before_the_rules_write`,
  `a_source_change_gives_a_new_incarnation`,
  `an_approval_carries_the_binding`, `revoke_clears_only_the_stale_approval`,
  `a_disable_then_set_keeps_the_rule_disabled`,
  `a_mutation_waits_for_the_rule_locks`,
  `an_unregistered_project_cannot_save_a_rule`,
  `a_rules_file_of_another_project_is_refused`,
  `a_linked_rules_file_is_refused`, `remove_deletes_entry_state_and_journal`
  (the entry, the state entry and the journal file each gone).
  Then `RulesFile` (with `project`) and its `SignedDoc` (`NO_LINKS =
  true`, `belongs_to` over `projects::registered_id`), `Rule` without
  `enabled` and without `attended`, `RuleEntry` with `incarnation` and
  `enabled` (no `start_seq`),
  `rule_digest`, `TriggerError` (with `Unregistered` and
  `EnabledNotSettable`), `set_rule` over the document as given,
  `set_enabled`, `store_approval`, `remove_rule`, `hold_rule_lock` and
  the `_locked` variants, every mutation in the order of design D1
  "Where rules live" item 6 (change lock, sweep lock; for `approve` and
  `enable` the state entry deleted before the rules write, no inbox
  read; `remove` then the state entry and the journal file). Fixtures
  register their project with `projects::bind_id` (`CI` turns
  `projects::touch` off). Run `cargo test -p apb-core --lib trigger::rule`.
- [x] 3.6 Tests first, `evaluate.rs`: `conditions_decide_in_order`,
  `own_send_needs_a_marker_of_this_installation_where_the_rule_reads`,
  `a_long_key_is_invalid`,
  `the_start_key_names_the_incarnation`,
  `a_skip_names_the_rules_path_never_the_value`. Then
  `evaluate_event`. Run `cargo test -p apb-core --lib trigger::evaluate`.

## 4. The sweep in the engine (design "Invariants", D1 "One pass over one rule", "Probation", "Views", "The run surfaces")

- [x] 4.1 `crates/apb-engine/src/trigger/` skeleton (`mod.rs` with
  `FireRequest`, `FireOutcome`, `Fire`, `RuleSweep`, an empty `sweep`
  and `starts_in_last_hour`; `view.rs` with `RuleView`, `list`,
  `doctor_line`, the heartbeat `beat` and `status`, `triggered_by` and
  `triggered_by_line`); `pub mod trigger;` in
  `crates/apb-engine/src/lib.rs`. `cargo build -p apb-engine`.
- [x] 4.2 Tests first, `crates/apb-engine/tests/suite/trigger_test.rs`
  (declared in `tests/main.rs`) with the two start stand-ins of design D1
  "Tests" item 4 and the fixture project registered with
  `projects::bind_id`: the 37 tests of design D1 "Tests" item 4 (1 to 34 with 21a, 21b and
  24a), among
  them `the_first_pass_after_approval_only_positions`,
  `a_removed_state_restarts_nothing_eight_days_later`,
  `a_crash_between_line_and_transaction_repeats_the_event_once`,
  `a_journal_write_failure_leaves_the_position`,
  `a_busy_concurrency_key_leaves_no_value_in_the_journal`,
  `a_rule_registers_no_consumer`, `an_unapproved_rule_starts_nothing`,
  `a_disable_during_a_pass_starts_nothing_after_it` (one start, made
  before the disable committed, none after it, the disable returned
  after the current start only), `a_billing_deny_revokes_the_approval`
  (allow to deny to allow, a start attempted at the deny),
  `a_restored_binding_with_no_start_keeps_the_approval`,
  `an_unreadable_runs_directory_holds_the_cap`,
  `a_changed_permit_revokes_the_approval`
  (allow and ask, A to B to A across a restart),
  `a_crash_before_the_revocation_is_completed_or_moot`,
  `an_unattributable_run_counts_toward_the_cap`,
  `a_lost_launch_record_counts_toward_every_cap`,
  `a_stamped_directory_without_manifest_does_not_count`,
  `a_failed_hand_over_counts_at_once`,
  `leftovers_of_a_crashed_remove_are_inert`, `a_rule_start_is_unattended`
  (a gate that needs a person fails it `hitl_required`; a long timer wait
  keeps the driver), `a_rule_run_earns_no_probation_credit` (the
  real start with `external_data: false` and a sub-playbook: both entries
  `unattended: true`, `counted` 0) and
  `a_default_rule_run_writes_no_probation_entry`. Then `sweep` exactly as
  design D1 "One pass over one rule" says (only enabled and approved
  rules; sweep lock, re-read; a missing or foreign state positioned at
  the head the pass reads, line first, starting nothing in that pass; the
  journal's ids; read; the gap; each event in order with the cap read
  before each matching start, the start limit, the change-lock probe and
  the re-read before each start, every line appended and synced before
  the transaction that moves the position, the binding mismatch's line
  synced before `revoke_approval_locked`; no consumer, no deletion by
  name) and `starts_in_last_hour` over the fallible `run_dirs_strict`. The
  scheduler, `deferred.rs`, `wait_park.rs`, `prepare.rs`, `manifest.rs`
  and the engine's `probation.rs` do not change. Run
  `cargo test -p apb-engine --test main trigger_test`.
- [x] 4.3 Tests first, `view.rs`: `a_beat_reads_live_and_an_old_one_stale`,
  `doctor_line_names_an_unverified_state`,
  `doctor_line_names_an_unregistered_project`,
  `doctor_line_names_leftovers_of_a_crashed_remove` (the orphan journal
  of an earlier incarnation listed with its path),
  `triggered_by_line_names_the_rule_and_event`,
  `verdict_words_never_repeat_a_stored_text`. Then `beat`, `status`,
  `list`, `doctor_line`, `triggered_by`, `triggered_by_line` and
  `verdict_words`. Run `cargo test -p apb-engine --lib trigger`.
- [x] 4.4 Tests first: `peek_writes_nothing` (unit,
  `crates/apb-engine/src/start_key.rs`). Then `start_key::peek` (design
  D1 "`apb trigger test`"). Run `cargo test -p apb-engine --lib start_key`.

## 5. The fire and the surfaces (design D1 "The fire" to "Doctor")

- [x] 5.1 Tests first, `crates/apb-mcp/tests/suite/trigger_tools_test.rs`:
  the tests of design D1 "Tests" item 5, among them
  `trigger_test_reports_every_check_in_order` (asserting the typed
  responses of design D1 "`apb trigger test`": twelve `CheckLine`s in
  `CheckId::ORDER`, `passed`, `refused` with its code and `not_run`, a
  param that does not render as `ParamCheck.failure`, `KeyUse` and
  `KeyReport`), `trigger_test_is_refused_in_a_run_step`,
  `a_changed_permit_is_caught_in_admission` (allow and ask; restored, the
  approval does not come back), `a_change_after_admission_fails_preparation`
  (held at the existing `Hook::KeyAdmitted`),
  `an_ask_rule_starts_without_a_launch_nonce`,
  `a_stricter_policy_revokes_the_approval` and
  `a_changed_billing_decision_revokes_the_approval`. Then
  `tools::run::keyed_peek` (`crates/apb-mcp/src/tools/run.rs`) and
  `crates/apb-mcp/src/tools/trigger.rs` (`fire` steps 1 to 13, always
  unattended, with the budget's canonical JSON of step 4, `keyed_peek` for
  a dry run that stops before preparation (`PreStartPassed`), refusals as
  codes with the param `name` of a pattern refusal, the
  `workdir_queue_wait` of step 13, `ApprovalNeed` with `rule_digest`,
  `binding` and `Deserialize`, `BindingChanged { binding }` read from the
  `binding_changed` refusal of `check_run` with `with_approved_binding`,
  `approval_need`, `approve` under the rule's locks, `fire_traced`
  collecting the `CheckLine`s, `fire` over it, `test_rule` with
  `key_tag_if_keyed`, the key's use, the trace completed with `not_run`
  and `checked_at_start`, no lock and its run-step refusal, the permit
  `check_run` returns handed on as the expected pins, `list`,
  `sweep_root`; `pub mod trigger;` in `tools/mod.rs`) and
  `crates/apb-mcp/src/server/trigger.rs` with the two tools, their args
  in `server/args.rs`, `trigger_router` in `server/mod.rs` and the two
  `READ_ONLY` rows. Run `cargo test -p apb-mcp --test main trigger_tools_test`
  and `cargo test -p apb-mcp --lib read_only`.
- [x] 5.2 Tests first, unit in `crates/apb-server/src/trigger_sweep.rs`:
  `the_period_defaults_to_five_seconds`, `spawn_runs_once_per_process`.
  Then `trigger_sweep.rs`, `mod trigger_sweep;` in `lib.rs`, `spawn()` in
  `run_ingest_server` and `nudge()` in the split `Appended::Stored` arm
  of `post_hook_handler`. Run `cargo test -p apb-server --lib trigger_sweep`
  and `cargo test -p apb-server --test main ingest_test`.
- [x] 5.3 Tests first, `crates/apb-cli/tests/suite/trigger_cli_test.rs`:
  the tests of design D1 "Tests" item 6. Then
  `crates/apb-cli/src/trigger.rs` (every mutation: the run-step refusal
  and `consent::ask_typed` with the rule's name, design D0 "A person's
  command" item 1; no passkey), `Command::Trigger` in `main.rs`, the
  doctor lines in `doctor.rs` and the pass in `resume_all_cmd`, skipped
  inside a run step before any read. Run
  `cargo test -p apb --test main trigger_cli_test`.
- [x] 5.4 The run surfaces (design D1 "The run surfaces"), over
  `trigger::view::triggered_by` of task 4.3: `apb runs <id>`
  (`crates/apb-cli/src/run.rs`), MCP `run_status` (`tools/run.rs`),
  `RunDetail.triggered_by` (`routes/runs.rs`) and the HTML report
  (`report_html/model.rs`, `sections.rs`). Run
  `cargo test -p apb-engine --lib report_html` and
  `cargo test -p apb-engine --lib trigger`.
- [x] 5.5 Tests first, `crates/apb-server/tests/suite/triggers_api_test.rs`:
  the tests of design D1 "Tests" item 7. Then
  `crates/apb-server/src/routes/triggers.rs` (`RequestChannel::Session`
  only, the body types with unknown fields denied, no passkey token,
  every blocking step inside `spawn_blocking`), `pub mod triggers;` in
  `routes/mod.rs`, the `merge` in `lib.rs`, and the TypeScript
  registrations (`RuleView`, `LogEntry`, `Verdict`, `ConditionMark`,
  `ConditionResult`, `TriggeredBy`). Regenerate TypeScript. Run
  `cargo test -p apb-server --test main triggers_api_test`.
- [x] 5.6 Web, tests first: `verdictWords names every code` and
  `triggeredByLine names the rule and the event`
  (`web/src/lib/triggers.test.ts`), `RunDriveNotes names the rule that
  started the run` (`web/src/lib/RunDriveNotes.test.ts`). Then
  `web/src/lib/triggers.ts`, `web/src/pages/TriggerList.svelte`, the route
  in `App.svelte`, the Topbar entry, and the `triggeredBy` prop of
  `RunDriveNotes.svelte` passed from `RunView.svelte`. Run
  `cd web && bun run test && bun run check`.

## 6. Docs and records

- [x] 6.1 Docs (design D1 "Docs"): new `docs/TRIGGERS.md`, and
  `docs/CONNECTORS.md`, `docs/CI.md` (the item at line 455),
  `docs/GUARDRAILS.md`, `docs/EXTERNAL-DATA.md`, `docs/MCP.md`
  (`docs/DEPLOYMENT.md` does not change). Test first: `docs_examples_validate` in
  `trigger_cli_test.rs` (every rule YAML block of `docs/TRIGGERS.md`
  passes `check_rule` against the bundled github connector). Run
  `cargo test -p apb --test main trigger_cli_test`.
- [x] 6.2 `CLAUDE.md` and `AGENTS.md`, the same shared text in both
  (only CLAUDE.md's "Claude-specific" section has no twin): in the
  `apb-core` list the `trigger/` module (`rule`, `render`, `validate`
  with V260 to V267, `state` with the journal, `evaluate`) and, at
  `connector/event_filter.rs`, that a start rule's filter is the same
  `EventFilter`; in the `apb-engine` list `trigger/` (the
  sweep in `mod`, `view` with the heartbeat and the run's rule); in the
  `apb-mcp` list `tools::trigger` (the
  fire every rule start goes through); in the CLI list `trigger.rs`; in
  the server list `trigger_sweep.rs` (in the ingest listener) and
  `routes/triggers.rs`; a "Start rules" paragraph under "Concepts that
  span files" (a person writes the rule, in a registered project; the
  signed per-project store; the incarnation that binds state, journal
  and start keys; no inbox consumer, a gap line instead; the order of conditions; each event starts
  now or is recorded, only `start_key_busy` and the hourly cap waiting
  for the next pass; the start key as the only dedupe for
  `runs.start_key_days`; runs at once are the playbook's `concurrency:`
  block; `Initiator::Trigger` and the rule's approval bound to its
  binding, needed by every rule, with I1's boundary stated once; a
  rule's run always unattended and keeping its driver; `enabled`
  changed only by `enable` and `disable`; the journal line appended and
  synced before the position moves, codes only; the invariants I1 to I5 in one sentence each; the change lock
  a pass yields to; the sweep in the listener and `apb resume --all`, never in
  a run step; `apb trigger test` naming what is checked at start; the trigger journal decides nothing;
  time-based starts after 1.0).
- [x] 6.3 Write `docs/superpowers/specs/<implementation date>-start-rules-design.md`:
  the content of `proposal.md` and `design.md` as one public record,
  with no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] 6.4 Style over every changed doc, spec and web file: no U+2013 or
  U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--`, `![` and Rust macros such as `matches!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds
  nothing).

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
  until clean; `apb-engine::trigger` must not name `apb_mcp`, the fire is
  a parameter)
- [x] 7.8 `scripts/third-party-notices.sh --check` (no dependency
  changed; it must pass unchanged)
- [x] 7.9 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 7.10 `openspec validate start-rules --strict`
- [x] 7.11 Close: remove the `$TMPDIR` contents you created; leave
  `$CARGO_TARGET_DIR`; stop only the processes you started (check
  `pgrep -f __pause-waker`, `pgrep -f __drive` and `pgrep -f 'apb ingest'`).
  Final report: per section what was built, every place where the code
  differed from `design.md` and what you did, which of the in-flight
  changes had merged and which shared symbol you found already there,
  the answers to 0.1 to 0.3, each gate with its numbers, the user-visible
  changes for the release notes (design "Release notes"), the changed
  file count, and anything left open.

## 8. Implementation review, round 2

Design "Review record", "Implementation review" (round 2); its rules are
folded into the design body. Every code change starts from a test that
fails for the reason the rule names.

- [x] 8.1 S1: directory durability of every journal write; the two tests
- [x] 8.2 S2: the size check inside the signed store transaction;
  `rules_store_full`; the test
- [x] 8.3 S3: journal removal, compaction, repair and append beneath
  the anchor; the three tests
- [x] 8.4 S4: a refused journal stops its rule; capped repair; the test
- [x] 8.5 S5 items 1 to 4
- [x] 8.6 Section 7 gates again, all of them, with numbers

## 9. Implementation review, round 3

Design "Review record", "Implementation review" (round 3); its rules are
folded into the design body. Test first.

- [x] 9.1 W1 items 1 to 4
- [x] 9.2 W2: the two sweep tests
- [x] 9.3 W3: `Beneath::sync_dir` and the seam on it
- [x] 9.4 W4: the sentence and `journal_error` on every list surface
- [x] 9.5 W5: `journal_left`
- [x] 9.6 W6 items 1 to 5
- [x] 9.7 Section 7 gates again, all of them, and the journal corpus
  (`APB_CORPUS_REQUIRED=1 APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries/bin
  cargo nextest run -p apb --test main journal_corpus`), with numbers

## 10. Implementation review, round 4

Design "Review record", "Implementation review" (round 4); its rules are
folded into the design body. Test first.

- [x] 10.1 Z1: the Linux gate and the empty link
- [x] 10.2 Z2 items 1 to 7
- [x] 10.3 Z3: `state_left` on every surface, the dashboard included
- [x] 10.4 Z4: the component remedy and the typed reason
- [x] 10.5 Z5: the parent descriptor sync; the non-unix twin
- [x] 10.6 Z6 items 1 to 4
- [x] 10.7 Section 7 gates again, all of them, and the journal corpus,
  with numbers

## 11. Implementation review, round 5

Design "Review record", "Implementation review" (round 5); its rules are
folded into the design body. Test first.

- [x] 11.1 Q1: the first newline ends the overlong line; the test
- [x] 11.2 Q2: every read byte counted before classification; the test
- [x] 11.3 Q3: the orphan asserted and the two tests on Linux only
- [x] 11.4 Q4: the non-unix twin
- [x] 11.5 Section 7 gates again, all of them, and the journal corpus,
  with numbers

## 12. Close the change

- [x] 12.1 Rebased onto main with inbox waits and global playbooks; the
  places where they meet start rules reviewed once (the global target's
  test added)
- [x] 12.2 The five implementation review rounds folded into the design
  body (no appended section overrides another; a short record kept under
  "Review record")
- [x] 12.3 The public design record under `docs/superpowers/specs`
  matches the final design
- [x] 12.4 The change archived; a Purpose for the new spec;
  `openspec validate --all --strict` passes
- [x] 12.5 The branch squashed into one commit
