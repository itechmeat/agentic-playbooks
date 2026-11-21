# Tasks: one point oh fixes

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta
`specs/one-point-oh-leftovers/spec.md`. Tick each box (`- [x]`) as soon as
the task is done, so an interrupted run can resume from the first open
box. Partial edits from an earlier attempt may already be in the tree:
review them against `design.md`, keep what is right, finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by the
name `design.md` gives, keep the decision, and note the difference in the
final report. Never invent a different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first failure
only for a test that names a type or function that does not exist yet.
The pinning tests 6.1 and 6.2 may pass on their first run; every other
test must be seen failing first.

Line numbers are those of main efae5822 and are to be re-verified on the
main the implementer starts from (eval-doubles, start-rules,
incremental-lists-and-session-fork, inbox-waits and the mcp changes may
have merged and moved some of them).

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-one-point-oh-fixes-target TMPDIR=$HOME/.cache/apb-one-point-oh-fixes-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache` fails with a read-only file system, use
`CARGO_TARGET_DIR=/tmp/apb-one-point-oh-fixes-target` and
`TMPDIR=/tmp/apb-one-point-oh-fixes-tmp` instead, and before each build
check that `df -h /tmp` shows more than 4G free; if not, stop and report
it. Never use `/dev/shm`, and never put a target dir, cache or temp data
inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Do not commit, push or open a pull request. Do not
start subagents. Search the code with `zg --rg -F <symbol> <path>` first.

Inside a task run only the tests it names:

1. A unit test module: `cargo test -p <crate> --lib <module path>`.
2. An integration test file: `cargo test -p <crate> --test main <file
   module>`. The CLI package is named `apb`: `cargo test -p apb --test
   main diagram_cli_test`.
3. `cargo test` takes one name filter per invocation: two modules are two
   commands.

## 0. Preconditions

- [x] 0.1 Record `git log --oneline -1` and `grep -n '^version' Cargo.toml`.
  List `openspec/changes/archive/` and note which of eval-doubles,
  start-rules, incremental-lists-and-session-fork, inbox-waits,
  mcp-tool-connectors and mcp-http-and-surfaces are archived: none is
  required, but each one there may have moved a line this change names
  (`design.md` "Shared files with the changes in flight"). Note also
  whether secret-params-in-run-files is archived (it shares no code
  file with this change, only neighbour sections of
  `docs/HOWTO-authoring.md`, `CLAUDE.md` and `AGENTS.md`).
- [x] 0.2 `git grep -nE '"V3[2-3][0-9]"' crates` prints nothing. If any
  code from V320 to V339 appears, stop and report it (V320 is this
  change's; V321 to V339 are its family's reserve).
- [x] 0.3 Each of these exists where named (one `zg --rg -F` each; record
  the line). If one is missing, stop and report it.
  1. `pub struct ProjectChecksSpec` and `mod cargo_manifest;` in
     `crates/apb-core/src/project_checks.rs`;
  2. `pub(super) fn check_structure` in `crates/apb-core/src/validate/baseline.rs`;
  3. `fn input_digest`, `fn numbered_repeats`, `pub(crate) fn read`,
     `fn run(` and `is_cargo_test()` in `crates/apb-engine/src/baseline/project.rs`;
  4. `pub fn read_regular_capped` and `pub fn list_dir` in
     `crates/apb-core/src/fsutil/beneath.rs`;
  5. `name: create_release`, `name: get_release_by_tag` and
     `healthcheck: get_user` in `connectors/gitlab/connector.yaml`;
     `fn check_marker` in `crates/apb-core/src/connector/idempotency.rs`;
  6. `fn a_github_comment_with_a_lost_answer_is_found_by_its_marker_and_not_posted_again`
     in `crates/apb-engine/tests/suite/send_intent_test.rs`;
  7. `export function parseProfileDoc` and `export interface ParsedProfileDoc`
     in `web/src/lib/profileedit.ts`; `async function loadProfile` and
     `async function save` in `web/src/pages/ProfileEdit.svelte`;
  8. `pub fn copy_playbook_to` in `crates/apb-core/src/scope_move.rs`;
     `fn doctor_env` and `global_playbooks_parent` in
     `crates/apb-cli/src/doctor.rs`; `fn doctor(` in
     `crates/apb-cli/tests/suite/diagram_cli_test.rs`;
  9. `resume: Some(&["-z", "--resume"` in `crates/apb-core/src/agents/hermes.rs`;
  10. `pub struct ClaudeAdapter` in `crates/apb-engine/src/adapter.rs`.
      Record the output of `rg -l ClaudeAdapter crates` (18 files on
      efae5822);
  11. `fn an_unstartable_item_in_a_host_session_run_fails_without_a_host_fallback`
      in `crates/apb-engine/tests/suite/list_run_test.rs`;
      `track_provenance: run.workers == 1` in
      `crates/apb-engine/src/scheduler/list/batch.rs`; `const PANEL` and
      `fn positions` in `crates/apb-engine/tests/suite/join_loop_test.rs`;
  12. `pub fn lock_dir` and `fn lock_is_stale` in
      `crates/apb-core/src/fsutil.rs`; `fn interrupted_draft`, `fn
      temp_dir_name` and `const DRAFT_VERSION` in
      `crates/apb-core/src/versioning/mod.rs`; `pub fn file_size` (twice) in
      `crates/apb-core/src/fsutil/beneath.rs`; `pub(crate) struct ReadCtx`
      in `crates/apb-engine/src/baseline/ctx.rs`; `pub shared_workdir` in
      `crates/apb-engine/src/run_config.rs`; `pub fn shell_quote` in
      `crates/apb-engine/src/next_action/command.rs`; `fn
      sanitize_for_terminal` in `crates/apb-cli/src/util.rs`;
  13. `fn approve_cmd` in `crates/apb-cli/src/connector.rs`;
      `approve_connector_handler` in
      `crates/apb-server/src/routes/connectors/mod.rs`; `pub fn refusal`
      in `crates/apb-core/src/step_env.rs`; `enum RequestChannel` in
      `crates/apb-server/src/auth.rs`; `pub fn person_only_refusal` in
      `crates/apb-engine/src/decision_channel.rs`; `pub(crate) async fn
      post_review_handler` in `crates/apb-server/src/routes/runs.rs`;
      `fn authed_with` and `fn login_request` in
      `crates/apb-server/tests/suite/auth_endpoints_test.rs`; `inbox_covered` and
      `fn an_inbox_grant_keeps_only_the_inbox_paragraph` in
      `crates/apb-engine/src/connector/prompt.rs`;
      `fn approve_marks_connector_then_account_as_trusted` in
      `crates/apb-cli/tests/suite/connector_cli.rs`;
      `fn approve_endpoint_flips_connector_trust` in
      `crates/apb-server/tests/suite/connectors_api_test.rs`.
- [x] 0.4 `crates/apb-core/src/project_checks/junit.rs` does not exist,
  and `rg -n "process_env|processEnv" web/src/pages/ProfileEdit.svelte
  web/src/lib/profileedit.ts` prints nothing (other web files, such as
  `web/src/lib/journal.ts` and `web/src/lib/api/core.ts`, name it on
  main and are not part of this check). If either differs, it is a
  partial earlier attempt: review it against `design.md`.
- [x] 0.5 `git status --short examples/playbooks`: note whether
  `examples/playbooks/quality-process/` is there. Never commit, move or
  delete it.

## 1. GitLab release sent once (design D4.1)

- [x] 1.1 Tests first in `crates/apb-engine/tests/suite/send_intent_test.rs`:
  `a_gitlab_release_with_a_lost_answer_is_found_by_its_marker_and_not_created_again`
  and `a_gitlab_release_by_another_account_is_not_taken_for_the_send`,
  through the file's helpers. Run `cargo test -p apb-engine --test main
  send_intent_test` and see both fail.
- [x] 1.2 `connectors/gitlab/connector.yaml`: version 0.3.0, `identity`,
  the `create_release` `idempotency` block and `list_releases` (design
  D4.1 items 1 to 4). Run the tests of 1.1 (pass).
- [x] 1.3 `connectors/gitlab/tests.yaml`: `description: "/close later"` in
  the `create_release` case and the two `list_releases` cases, the
  second with `per_page=20` (design D4.1 item 5). Run `cargo test -p apb --test main
  official_connectors_gate` and `cargo test -p apb-engine --test main
  connector_gitlab`.
- [x] 1.4 Docs: `connectors/gitlab/PUBLIC.md` and `docs/CONNECTORS.md`
  "### gitlab" (design D4.1 item 6).

## 2. JUnit reports (design D4.2)

- [x] 2.1 Tests first in `crates/apb-core/src/project_checks.rs` `mod
  tests`: `junit_round_trips`, `an_absent_junit_is_not_serialized`; then
  `ProjectChecksSpec.junit`. Run `cargo test -p apb-core --lib
  project_checks`.
- [x] 2.2 Tests first in `crates/apb-core/tests/suite/validate_baseline_test.rs`:
  `v320_empty_glob`, `v320_absolute_glob`, `v320_backslash`,
  `v320_parent_component`, `v320_invalid_glob`, `v320_only_without_test`,
  `v320_wildcard_in_the_first_component`, `v320_empty_or_dot_component`,
  `a_relative_glob_with_test_is_valid`; then V320 in `check_structure`.
  Run `cargo test -p apb-core --test main validate_baseline_test`.
- [x] 2.3 Tests first in the new `crates/apb-core/src/project_checks/junit.rs`
  `mod tests`: the tests of design D4.2 "Tests" item 1 (the legal-XML and
  fail-closed tables included); then `failed_tests` over bytes, the
  complete-report rule, `failing_item`, `is_wildcard`, `matcher`,
  `MAX_REPORT_BYTES`, `MAX_REPORT_FILES`, `CHECK_EVERY` and the `pub use`.
  Run `cargo test -p apb-core --lib project_checks::junit`.
- [x] 2.4 Tests first: `junit_reports_list_the_failing_tests_of_a_make_test`,
  `a_report_the_command_did_not_write_reads_unknown`,
  `a_report_in_a_tracked_place_reads_unknown`,
  `a_new_failing_case_is_a_regression` and
  `a_shared_workdir_run_reads_junit_unknown` in
  `crates/apb-engine/tests/suite/baseline_test.rs`, and
  `a_linked_report_is_not_read`, `a_walk_cut_by_its_depth_limit_is_unknown`,
  `a_name_that_is_not_utf8_where_a_report_could_lie_is_unknown`,
  `a_rewritten_report_counts_and_an_untouched_one_does_not`,
  `an_equal_length_rewrite_within_one_timestamp_tick_counts`,
  `the_walk_root_is_the_literal_directory_prefix`,
  `a_literal_report_path_is_read`, `a_star_stays_inside_one_component`,
  `a_scan_that_uses_up_the_allowance_does_not_start_the_command`,
  `a_cancelled_scan_stops`,
  `an_unreadable_before_scan_still_runs_the_command`,
  `an_exhausted_before_scan_still_runs_the_command`,
  `a_cancel_during_the_last_scan_operation_starts_no_command`,
  `a_shared_tree_reads_junit_unknown`,
  `a_hidden_character_in_a_test_name_reads_unknown`,
  `a_report_path_and_a_long_item_hold_to_the_bound` and
  `without_junit_the_digest_is_unchanged` in
  `crates/apb-engine/src/baseline/project.rs` `mod tests`, and
  `file_stamp_reads_a_regular_file_without_following_links` in
  `crates/apb-core/src/fsutil/beneath.rs` `mod tests`. See them fail.
- [x] 2.5 `Beneath::file_stamp` (both variants, beside `file_size`);
  `ReadCtx.holds_tree` set at its three builders (`scheduler/baseline.rs`,
  `scheduler/list/check.rs`, `follow_up/read.rs`, design D4.2 "Who may
  read a report"); then the engine: `walk_root`, `ReportStamp` stamped
  through `file_stamp`, `ScanLimits`, `junit_reports`,
  `junit_failed_tests`, the `run` changes (no scan without
  `holds_tree`, the before scan, the allowance computed after it, the
  cancel check immediately before `run_capture`, a failed before scan
  that still runs the command), `read`, `input_digest` and the module
  doc (design D4.2 "The engine" items 1 to 7). Run `cargo test -p
  apb-core --lib fsutil::beneath`, `cargo test -p apb-engine --lib
  baseline::project` and `cargo test -p apb-engine --test main
  baseline_test`.
- [x] 2.6 `docs/HOWTO-authoring.md`: `junit` in the `project_checks`
  bullet (with the shared-tree rule) and V320 in the validator paragraph
  (design D4.2 "Tests" item 6).

## 3. process_env in the profile editor (design D4.3)

- [x] 3.1 Tests first in `web/src/lib/profileedit.test.ts`: the six of
  design D4.3 "Tests". Run `cd web && bun run test -- profileedit` and see
  them fail.
- [x] 3.2 `web/src/lib/profileedit.ts`: `ProcessEnvPolicy`, `ProcessEnv`,
  `ParsedProfileDoc.processEnv`, `parseProfileDoc`, `parseAllow`,
  `allowText`, `processEnvChange`. Run `cd web && bun run test --
  profileedit` (pass).
- [x] 3.3 `web/src/pages/ProfileEdit.svelte`: the state, the reset on a
  route change, the fieldset and the `process_env` of the save body
  (design D4.3 item 2). Plain CSS over the page's existing CSS variables
  only; no utility class strings. Run `cd web && bun run check`. The
  page test `web/src/pages/ProfileEdit.svelte.test.ts` pins the save
  body (an untouched save sends no `process_env`).

## 4. A stale claimed directory in doctor (design D4.6)

- [x] 4.1 Tests first: `lock_held_follows_the_staleness_of_lock_dir` in
  `crates/apb-core/src/fsutil.rs` `mod tests`;
  `a_staging_name_is_draft_debris` in
  `crates/apb-core/src/versioning/mod.rs` `mod tests`; and in
  `crates/apb-core/tests/suite/scope_move_test.rs`
  `an_old_empty_claim_is_named`, `a_young_empty_claim_is_not_named`,
  `a_directory_with_current_is_not_named`, `a_link_is_not_named`,
  `an_old_empty_claim_whose_id_lock_is_held_is_not_named`,
  `an_old_empty_claim_with_a_stale_id_lock_is_named`,
  `only_the_exact_draft_names_are_debris`,
  `a_version_without_current_is_other`,
  `a_claim_whose_pointer_lock_is_held_is_not_named`,
  `an_empty_claim_with_a_staged_copy_is_staging`; then `lock_held`,
  `is_draft_debris` (beside `temp_dir_name`; `interrupted_draft` is
  not changed), `STALE_CLAIM_SECS`, `ClaimLeft`, `StaleClaim` and
  `stale_claims` (design D4.6 item 1). Run `cargo test -p apb-core --lib
  fsutil`, `cargo test -p apb-core --lib versioning` and `cargo test -p
  apb-core --test main scope_move_test`.
- [x] 4.2 Tests first: in `crates/apb-cli/src/doctor.rs` a new
  `#[cfg(test)] mod tests` with
  `an_empty_claim_command_keeps_the_path_whole`,
  `a_path_that_cannot_be_quoted_gets_no_command`,
  `only_an_empty_claim_gets_a_command` and
  `a_claim_joins_the_failed_load_of_its_id`; in
  `crates/apb-cli/tests/suite/diagram_cli_test.rs`
  `doctor_names_an_empty_claimed_playbook_directory` and the module doc
  line. Then `stale_claim_line` (the four lines of design D4.6 item 2,
  the path in the one command through
  `apb_engine::next_action::command::shell_quote`, never through
  `sanitize_for_terminal`), `fold_claims` joining a project claim to its
  failed load line, and the call in `doctor_env`'s store loop.
  Run `cargo test -p apb --lib doctor` and `cargo test -p apb --test main
  diagram_cli_test`.

## 5. hermes (design D4.9)

- [x] 5.1 Test first: a `#[cfg(test)] mod tests` in
  `crates/apb-core/src/agents/hermes.rs` with
  `every_value_option_is_followed_by_its_placeholder`. Run `cargo test -p
  apb-core --lib agents::hermes` and see it fail; then the resume form
  and its comment. Run it again (pass). Then
  `a_hermes_session_id_of_another_shape_is_no_session` in
  `crates/apb-engine/tests/suite/adapter_test.rs` and the empty id
  refused by `is_plausible_session_id` (design D4.9).

## 6. Pinning tests (design D4.11)

- [x] 6.1 `an_unstartable_item_in_a_cli_run_fails_without_a_host_fallback`,
  `items_of_one_worker_each_record_their_own_commits` and
  `items_of_several_workers_record_no_commits` in
  `crates/apb-engine/tests/suite/list_run_test.rs`. The commits test
  asserts on the journal as design D4.11 item 2 states: two
  `artifacts_committed` events for the node, the first within item `a`'s
  execution and the second within item `b`'s, each with one commit whose
  `apb_run` names the run and whose subject names its item. Run `cargo test -p
  apb-engine --test main list_run_test`.
- [x] 6.2 `a_join_waits_when_one_branch_of_the_new_round_has_not_started`
  in `crates/apb-engine/tests/suite/join_loop_test.rs`. Run `cargo test
  -p apb-engine --test main join_loop_test`.
- [x] 6.3 A test of 6.1 or 6.2 that fails is a defect: fix it as design
  D4.11 says, rerun the file, and record the site. No pinning test failed
  for a fix site; the script attempt linkage was cut (design D4.11 item
  2, proposal "Out of scope" item 11).

## 7. CliAdapter (design D4.10)

- [x] 7.1 Rename `ClaudeAdapter` to `CliAdapter` in every file `rg -l
  ClaudeAdapter crates` lists and drop the "historical" sentence. `cargo
  build --workspace --all-targets`; `rg -n ClaudeAdapter crates CLAUDE.md
  AGENTS.md` finds nothing. Leave `docs/adr/` and `docs/superpowers/` as
  they are. Run `cargo test -p apb-engine --test main adapter_test`.

## 8. Connector fixes (design D4.13)

- [x] 8.1 Tests first: `approve_is_refused_inside_a_run_step` in
  `crates/apb-cli/tests/suite/connector_cli.rs`;
  `approve_endpoint_refuses_a_keyless_request_from_a_local_process`,
  `approve_endpoint_refuses_a_bearer_key` and
  `a_signed_in_session_approves_while_the_server_has_a_run_id` in
  `crates/apb-server/tests/suite/connectors_api_test.rs`; in that file
  the helpers `keyed` (a keyed `AppState` and its key, built as
  `auth_endpoints_test.rs` `authed_with` builds it) and `sign_in` (the
  login through `build_router(state.clone())` with the CSRF marker,
  returning the session cookie pair), a `cookie` argument on
  `post_json` and `get_json`, and the two existing approval tests moved
  to a keyed state whose every request carries the cookie and
  `CSRF_HEADER` (design D4.13 item 1, "Tests" items 1 to 3). Run `cargo test -p
  apb --test main connector_cli` and `cargo test -p apb-server --test
  main connectors_api_test` and see the refusal tests fail; then the
  `StepCaller` refusal as the first statement of `approve_cmd`, and the
  `RequestChannel` check as the first statement of
  `approve_connector_handler` (no `StepCaller` there). Run both again
  (pass). Then the boundary sentence in `docs/CONNECTORS.md` beside
  `apb connector approve`.
- [x] 8.2 Test first in `crates/apb-engine/src/connector/prompt.rs` `mod
  tests`: `an_inbox_grant_beside_an_untrusted_function_keeps_both_paragraphs`
  (design D4.13 item 2); then the condition. Run `cargo test -p
  apb-engine --lib connector::prompt`.

## 9. Records

- [x] 9.1 `CLAUDE.md` and `AGENTS.md`, the same text in both, module map
  only: the `apb-core` entry "the project's own test and lint commands
  (`project_checks.rs`)" gains "with `project_checks/junit` the bounded
  JUnit XML reader of failing tests"; the `apb-engine` entry "invocation resolution
  (`invocation.rs`), agent adapters (`adapter.rs`" gains "`CliAdapter`,
  the one adapter of every command-line agent". Nothing else.
- [x] 9.2 Write `docs/superpowers/specs/<implementation date>-one-point-oh-fixes-design.md`
  (`2026-11-XX` with the real date): the content of `proposal.md` and
  `design.md` as one public record, with no mention of `openspec/` and no
  reference to any outside article, author or project.

## 10. Style

- [x] 10.1 Over every changed doc, YAML and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![` and
  Rust macros such as `writeln!(` do not count), no CJK (`rg -nP
  '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds nothing).
- [x] 10.2 In `web/src/pages/ProfileEdit.svelte` and
  `web/src/lib/profileedit.ts`: no utility class strings and no
  `@apply`.

## 11. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 11.1 `cargo fmt --all -- --check`
- [x] 11.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 11.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 11.4 `cargo test --workspace --doc`
- [x] 11.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`: no mirrored type changed, the generated files must be
  current as they are)
- [x] 11.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 11.7 `scripts/third-party-notices.sh --check` (no dependency is
  added; it must pass unchanged)
- [x] 11.8 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 11.9 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 11.10 `openspec validate one-point-oh-fixes --strict`

## 12. Close

- [x] 12.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started.
- [x] 12.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did (every moved line),
  the answers to 0.1 to 0.5, every pinning
  test of section 6 that failed first and its fix, the files the rename of
  section 7 touched (count), each gate of section 11 with its numbers, the
  release notes lines (design "Release notes"), the changed file count,
  and anything left open.

## File budget

59 changed files, listed one by one in `design.md` "Files": GitLab 5;
JUnit 12 (`fsutil/beneath.rs` and the four `holds_tree` files
included); profile editor 3; doctor 6 (`fsutil.rs` and
`versioning/mod.rs` included); hermes 1; rename 18; pinning tests 2;
connector fixes 5; records 7. A user of `ClaudeAdapter` that a
change merged earlier added raises the rename by one file each, and a
fix a pinning test forces adds its file; say so in the report. The
change stays under 65 files in every case.
