# Tasks: people in the loop

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
exist yet. So every section builds in this order: types and changed
signatures first (each step compiles), then a skeleton whose functions do
nothing where a test file needs them to exist, then the red tests, then
the behaviour one piece at a time. A task names the tests that may pass
before its code exists; every other test it writes must be seen failing
first.

Line numbers are those of main 316c8180.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-people-in-the-loop-target TMPDIR=$HOME/.cache/apb-people-in-the-loop-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-people-in-the-loop-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-people-in-the-loop-target`
and `TMPDIR=/tmp/apb-people-in-the-loop-tmp` instead, and before each build
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
   (for example `cargo test -p apb-engine --lib review::scope`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   review_scope_test`). The CLI package is named `apb`: `cargo test -p apb
   --test main diagram_cli_test`.
3. `cargo test` takes one name filter per invocation: two modules are
   two commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` (design D0
"Test helpers").

After any change to a type listed in `crates/apb-server/src/ts_contract.rs`
or registered there by this change, regenerate TypeScript:
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then
`cargo test -p apb-server --lib ts_contract` (must pass without the
variable).

UI work uses plain CSS over the existing CSS variables only, in the
component's `<style>` block.

## 0. Preconditions

- [x] 0.1 This change is the first of the 1.0 sequence and depends on no
  other change. The open pull requests 284, 285 and 286 belong to the
  orchestrator, who clears them before this change's pull request is
  opened: do not touch them and do not wait for them. Note their state in
  the final report if `gh pr view 284 --json state` (and 285, 286)
  answers; a missing answer is not a reason to stop.
- [x] 0.2 Check these exist (one `rg -n` each; note the line):
  `pub(crate) fn await_decision` in `crates/apb-engine/src/scheduler/review_gate.rs`;
  `pub struct PendingReview` in `crates/apb-engine/src/progress.rs`;
  `fn check_review_target` in `crates/apb-engine/src/review.rs`;
  `fn stage_version` in `crates/apb-core/src/versioning/mod.rs`;
  `Concurrency,` in `crates/apb-core/src/validate/guarded.rs` (and
  `pub const ALL: [GuardedFeature; 9]` there). If any is missing, stop
  and report it.
- [x] 0.3 `git grep -ohE '"V2[0-9][0-9]"' crates | sort -u` prints
  exactly `"V200"`, `"V210"`, `"V211"`, `"V212"`, `"V220"`,
  `"V221"`, `"V222"`. If any of V240 to V259 appears, stop and report
  it.
- [x] 0.4 None of these exists: `crates/apb-core/src/review_form.rs`,
  `crates/apb-core/src/diagram.rs`,
  `crates/apb-engine/src/review/scope.rs`,
  `crates/apb-engine/src/review/form.rs`. If one exists, it is a partial
  earlier attempt: review it against `design.md`.

## 1. Shared pieces (design D0)

- [x] 1.1 Core types first: create `crates/apb-core/src/review_form.rs`
  with the constants and types of design D2 "max_scope (schema)" and D3
  "Schema" (`MaxScope`, `ReviewField`, `FieldKind` with `#[cfg_attr(feature
  = "ts", derive(ts_rs::TS))]`, `Required`, `ShowEntry`) and stubs
  `check_values`, `coerce`, `gate_output`, `decision_material` that
  return `Err("todo".into())` or an empty value. Declare `pub mod
  review_form;` in `crates/apb-core/src/lib.rs`. Add `fields`, `show`
  and `max_scope` to `NodeKind::HumanReview` (`schema.rs` line 2118) and
  `..` to the four patterns (design D2 "max_scope (schema)"). `cargo
  build -p apb-core -p apb-engine`.
- [x] 1.2 The two events of design D0 in `crates/apb-engine/src/event.rs`
  and their arm in `RunState::fold` (`state.rs` line 891). For the view
  types the events name, create `crates/apb-engine/src/review/scope.rs`
  (`ReviewScope` only) and `crates/apb-engine/src/review/form.rs`
  (`ShownEntry`, `FieldView`, `GateDetail` without a `pilot` field),
  declared in `review.rs` (line 12). `cargo build -p apb-engine`; then
  `cargo test -p apb-engine --test main journal_compat_test` (passes:
  nothing writes the new events yet).
- [x] 1.3 Tests first in `crates/apb-core/src/validate/guarded.rs`:
  `review_fields_and_max_scope_are_guarded`, and update
  `a_deadline_is_a_guarded_feature` (11) and rename
  `all_has_nine_entries_in_order` to `all_has_eleven_entries_in_order`
  (design D0 "Two new guarded features"). Then the two variants,
  `as_str`, `ALL`, `guarded_features` and the module doc sentence. Run
  `cargo test -p apb-core --lib validate::guarded` and `cargo test -p
  apb-engine --lib scheduler::prepare` (must still pass;
  `scheduler/prepare.rs` is not edited).
- [x] 1.4 Create `crates/apb-core/src/validate/review_form.rs` with a
  module doc line and an empty `pub(super) fn check_review_form(playbook:
  &Playbook, r: &mut ValidationReport) {}`; declare it and add the call
  in `validate/mod.rs` exactly as design D0 "Validator codes" shows.
  `cargo build -p apb-core`.

## 2. Scope at every review gate (design D2, spec review-scope)

- [x] 2.1 Unit tests first in `review/scope.rs` (`#[cfg(test)] mod
  tests`): `the_window_starts_after_the_last_decision`,
  `commits_and_counts_come_from_the_window`,
  `an_unreadable_range_is_not_read`, `notify_sends_are_left_out`,
  `the_fast_floor_grows_with_the_scope` (clamped at 5 s and 300 s),
  `the_scope_line_reads_like_the_docs` (the example line of design D2,
  the singular forms, `files not read`, `lines not read`, `nothing changed since the last
  decision`), `an_unknown_count_passes_a_limit`,
  `withheld_names_the_counts_and_the_limits`. Use a temporary git
  repository with real commits for the numstat cases (`git init`, two
  commits; the helper `git_tree` of `list_run_test.rs` line 76 shows the
  commands). Then `window_start`, `scope_since`, `fast_floor_ms`,
  `scope_line`, `withheld`. Run `cargo test -p apb-engine --lib
  review::scope`.
- [x] 2.2 `GateDrive.workdir` and `GateRequest.detail` (design D2 "When
  the scope is taken"); fill them at the constructions
  (`scheduler.rs` lines 2411 and 2520, `review_gate.rs` lines 307 and
  353, `publish.rs` line 379) with `GateDetail::default()` for now;
  append `ReviewDetail` in `await_decision` after the request and before
  the automatic decision. `cargo build -p apb-engine`.
- [x] 2.3 V243 tests first in
  `crates/apb-core/tests/suite/validate_review_test.rs`:
  `v243_max_scope_needs_a_limit`, `v243_a_zero_limit`,
  `v243_withhold_names_an_offered_option`,
  `v243_not_every_option_withheld`. Then the V243 part of
  `check_review_form`. Run `cargo test -p apb-core --test main
  validate_review_test`.
- [x] 2.4 Engine tests first in a new
  `crates/apb-engine/tests/suite/review_scope_test.rs`:
  `a_gate_shows_the_scope_of_its_commits` (a script node commits one file
  of 3 lines, the gate's `pending_review.scope_line` is `1 commit, 1
  file, +3 -0 lines`), `an_uncommitted_change_is_not_in_the_scope`,
  `the_detail_holds_counts_only`,
  `a_second_visit_counts_since_the_first_decision`,
  `a_withheld_option_is_refused_through_the_channel`,
  `an_unattended_withheld_decision_fails_the_run`,
  `a_publish_gate_shows_its_scope`. Then the withheld refusal in
  `check_review_target` (design D2 "Withheld options are refused, not
  hidden" item 1) and the `PendingReview` fields with their fill in
  `pending_gate` and `pending_publish` (design D2 "Surfaces" item 1). Run
  `cargo test -p apb-engine --test main review_scope_test`, then
  `cargo test -p apb-engine --test main review_test` and `cargo test -p
  apb-engine --test main publish_test` (must still pass).
- [x] 2.5 Tests first in `run_stats/interventions.rs` tests (line 865):
  `a_quick_channel_decision_is_fast`,
  `a_pause_inside_a_visit_does_not_count_as_reading`,
  `an_engine_decision_is_never_fast`, `a_visit_without_detail_is_never_fast`.
  Then `ReviewCounts.fast` and the fold (design D2 "Decided without
  review" item 1), the `apb stats` text (item 2), and
  `OutcomeSummary.fast_decisions` with its line (item 3). Run `cargo
  test -p apb-engine --lib run_stats` and `cargo test -p apb-engine --lib
  run_outcome`.
- [x] 2.6 Surfaces: the CLI pending print (`crates/apb-cli/src/run.rs` line
  3036), the HTML report's decided gates (design D2 "Surfaces" item 4),
  register `ReviewScope` in `ts_contract.rs` and regenerate TypeScript.
  Web: tests first in `web/src/lib/ReviewPanel.test.ts`: `shows the scope
  line`, `a withheld option is disabled with its reason`; then the panel.
  Run `cd web && bun run test && bun run check`.
- [x] 2.7 Docs of design D2 "Docs". Section check: `cargo test -p
  apb-engine --lib review::scope`, `cargo test -p apb-engine --test main
  review_scope_test`, `cargo test -p apb-core --test main
  validate_review_test`.

## 3. Typed fields on a review gate (design D3, spec review-fields)

- [x] 3.1 Core tests first in `review_form.rs` (`#[cfg(test)] mod tests`):
  `coerce_reads_each_type` (text length, number from a string, bool
  words, enum options), `required_applies_to_the_named_options_only`,
  `a_default_fills_and_a_given_value_wins`, `an_unknown_field_is_refused`,
  `the_output_has_the_decision_and_every_field`,
  `the_material_changes_with_the_fields` (empty fields give the bare
  decision). Then `coerce`, `check_values`, `gate_output`,
  `decision_material`. Run `cargo test -p apb-core --lib review_form`.
- [x] 3.2 V244 to V247 tests first in
  `crates/apb-core/tests/suite/validate_review_test.rs`:
  `v244_field_names_types_and_defaults` (one case per problem of design
  D3 item 1), `v245_show_entries`,
  `v246_a_required_field_without_default_on_an_engine_decided_gate`,
  `v247_an_untrusted_default`. Then `template_texts` with field
  defaults, `review_show_texts` and its use in `check_templates`,
  `declared_fields` for gates, the untrusted changes (design D3
  "Untrusted text"), and the V244 to V247 checks. Run `cargo test -p
  apb-core --test main validate_review_test`, `cargo test -p apb-core
  --lib untrusted` and `cargo test -p apb-core --lib validate`.
- [x] 3.3 `ReviewEntry.fields`, `DecisionPost` and `post`, with
  `post_decision_via` and `post_decision_passkey` as wrappers (design D3
  "The values a decision carries" items 1 and 2). `cargo build
  --workspace`.
- [x] 3.4 Engine tests first in a new
  `crates/apb-engine/tests/suite/review_form_test.rs`:
  `fields_reach_the_next_prompt_and_an_output_field_edge`,
  `a_value_of_the_wrong_type_is_refused_by_the_channel`,
  `a_hand_written_entry_with_bad_fields_ends_the_run`,
  `a_secret_value_in_a_field_is_refused_before_the_channel_holds_it`,
  `an_untrusted_default_is_marked_and_taints_readers`,
  `the_show_block_renders_earlier_outputs`,
  `a_passkey_minted_without_the_fields_is_not_consumed` (follow the
  enrolled-passkey setup of `passkey_gate_test.rs` line 143),
  `a_gate_without_fields_keeps_the_decision_word`. Then the checks when
  posted, `render_form` and the `FormCtx` plumbing, `form::settle`, the
  `ReviewFields` event and the JSON output in the drive, `dead_end_reason`,
  and `passkey_stamp_ok` with `decision_material` (design D3 sections
  "Checked when posted" to "Passkeys cover the values"). Run `cargo test
  -p apb-engine --test main review_form_test`, then `review_test`,
  `passkey_gate_test` and `approval_test` (must still pass).
- [x] 3.5 Surfaces, tests first: `crates/apb-mcp/tests/suite/review_tool_test.rs`
  `review_decide_passes_fields`; `crates/apb-cli/tests/suite/people_cli_test.rs`
  (new) `review_field_flags_reach_the_gate` and
  `a_field_without_equals_is_refused`;
  `crates/apb-server/tests/suite/runs_api_test.rs`
  `a_review_post_carries_fields`. Then `ReviewDecideArgs.fields`,
  `review_decide_via`, the CLI `--field`, `ReviewBody.fields` and the
  handler, the passkey purpose in `routes/passkeys.rs` (design D3
  "Surfaces" and "Passkeys cover the values"). Run each test file.
- [x] 3.6 `PendingReview.show` and `fields`, the CLI print lines, the
  report's field values; register `ShownEntry`, `FieldView`, `FieldKind`
  in `ts_contract.rs` and regenerate TypeScript. Web: tests first in
  `web/src/lib/reviewform.test.ts`: `initial values take the defaults`,
  `a required field blocks only its options`, `numbers and bools are
  sent typed`; and in `ReviewPanel.test.ts`: `renders one input per
  field`, `marks an untrusted shown entry`. Then `reviewform.ts`,
  `ReviewPanel.svelte`, `postReview` and `runposts.ts`. Run `cd web &&
  bun run test && bun run check`.
- [x] 3.7 Docs of design D3 "Docs". Section check: `cargo test -p apb-core
  --lib review_form`, `cargo test -p apb-engine --test main
  review_form_test`, `cargo test -p apb-mcp --test main
  review_tool_test`, `cargo test -p apb --test main people_cli_test`,
  `cargo test -p apb-server --test main runs_api_test`.

## 4. The derived diagram (design D6, spec playbook-diagram)

- [x] 4.1 Core tests first in `diagram.rs` (`#[cfg(test)] mod tests`):
  `the_text_is_stable`, `shapes_and_edge_labels` (one node of each shape
  group, each edge label kind of design D6 item 4),
  `author_text_never_leaves_its_label`, `the_diagram_changes_no_digest`,
  `state_of_tells_each_case` (current, missing, stale, edited,
  unreadable), `scan_counts_every_version`. Then `mermaid`,
  `write_beside`, `state_of`, `scan`. Run `cargo test -p apb-core --lib
  diagram`.
- [x] 4.2 Tests first: `versioning_test::every_version_write_leaves_a_diagram`,
  `versioning::tests::shipped_content_leaves_a_diagram`,
  `bundle_test::an_export_carries_no_diagram`, and in
  `crates/apb-core/tests/suite/` the migrate case
  `a_migrated_version_has_a_diagram` in
  `crates/apb-core/tests/suite/schema_migrate_test.rs`.
  Then the three write sites (design D6 "Written at every version write").
  Run `cargo test -p apb-core --test main versioning_test`, `cargo test
  -p apb-core --lib versioning`, `cargo test -p apb-core --test main
  bundle_test`.
- [x] 4.3 CLI tests first in a new `crates/apb-cli/tests/suite/diagram_cli_test.rs`:
  `diagram_prints_the_text`, `diagram_out_writes_a_file`,
  `rebuild_all_writes_missing_diagrams`, `doctor_names_drifted_diagrams`,
  `an_edited_diagram_is_never_used` (the CLI and doctor part). Then
  `diagram.rs`, `Command::Diagram` and the doctor line. Run `cargo test
  -p apb --test main diagram_cli_test`.
- [x] 4.4 MCP and HTTP: tests first in
  `crates/apb-mcp/tests/suite/read_tools_test.rs`:
  `playbook_get_carries_the_diagram_of_the_yaml`, and in
  `crates/apb-server/tests/suite/api_test.rs`:
  `the_playbook_detail_carries_the_diagram` (both with an edited
  `playbook.mmd` on disk). Then the two fields. Run both files.
- [x] 4.5 The HTML report block (a test first in `report_html/graph.rs`
  tests: `the_graph_section_carries_the_diagram_text`, escaped and
  redacted like the node labels, see the test at line 808) and the
  dashboard block in `PlaybookView.svelte` (plain CSS over the existing CSS
  variables, no diagram library). Run `cargo test -p apb-engine --lib report_html`
  and `cd web && bun run check`.
- [x] 4.6 Docs of design D6 "Docs".

## 5. Records

- [x] 5.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both (only
  CLAUDE.md's "Claude-specific" section has no twin), limited to what this
  change adds: in the `apb-core` list `review_form.rs` (the review form:
  fields, show, max_scope and their checks), `diagram.rs` (the derived
  `playbook.mmd`) and the validator module `review_form` (V243 to V247);
  in the `apb-engine` list `review/scope.rs` (the scope of a review
  request and the fast floor), `review/form.rs` (the gate's form views
  and the consume-time check) and the two new events; in the CLI list
  `diagram.rs`. Add one paragraph to "Concepts that span files": **People
  in the loop** (the scope journaled with every review request and the
  fast-decision report, `max_scope` refused on every channel, gate fields
  as a JSON output covered by the passkey, the derived diagram never read
  back).
- [x] 5.2 Write `docs/superpowers/specs/2026-11-XX-people-in-the-loop-design.md`
  (`XX` filled with the day you write it): the content of `proposal.md`
  and `design.md` as one public record, with no mention of `openspec/`
  and no reference to any outside article, author or project.
- [x] 5.3 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![`
  and Rust macros such as `writeln!(` do not count), no CJK
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
  until clean)
- [x] 6.8 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 6.9 `openspec validate people-in-the-loop --strict`

## 7. Close

- [x] 7.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f __pause-waker` and `pgrep -f __drive`).
- [x] 7.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to
  0.1 to 0.3, each gate of section 6 with its numbers, the user-visible
  changes for the release notes (design "Release notes"), the changed
  file count against the 72 of design "Files", and anything left open.
