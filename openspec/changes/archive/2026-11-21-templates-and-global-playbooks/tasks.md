# Tasks: templates and global playbooks

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed or moved), find the site by the name
`design.md` gives, keep the decision, and note the difference in the
final report. Never invent a different behaviour. A named function that
does not exist is a stop: report it.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first failure
only for a test that names a type or function that does not exist yet.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-templates-target TMPDIR=$HOME/.cache/apb-templates-tmp
mkdir -p "$TMPDIR"
```

Never use `/dev/shm`, and never put a target dir, cache or temp data
inside the worktree. Never export `APB_TEST_BILLING_DEFAULT`.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing. The server crate embeds
`web/dist`: run `(cd web && bun run build)` once before the first cargo
command that compiles `apb-server` or `apb`.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Work only in this worktree; other worktrees next to it
belong to other sessions. Do not push and do not open a pull request. Do
not run `scripts/daily-cleanup.sh`.

Search the code with `zg --rg -F <symbol> <path>` (or `rg`) before reading
a file. Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`.
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (the CLI package is named `apb`: `cargo test -p apb --test
   main global_run_cli_test`).
3. `cargo test` takes one name filter per invocation.
4. Web: `cd web && bun run test -- <file>`.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;`.

Texts: English only; no U+2013 or U+2014 dash and no exclamation mark in
docs, comments or user-facing strings; no CJK. Web styles are plain CSS
over the existing CSS variables in the component's `<style>` block; no
utility class strings.

## 0. Preconditions

- [x] 0.1 `git log --oneline -1` (note the head; this change was written
  against main 265d1410) and `git status --short` (only this change's
  openspec folder may be new).
- [x] 0.2 Each of these exists (one `rg -n` each; note the file and
  line): `fn gate_run`, `fn remember_input`, `fn key_lookup`, `fn
  run_host_foreground` and `struct RunArgs` in `crates/apb-cli/src/run.rs`;
  `fn resolve`, `fn prestart`, `fn estimate_cmd` in
  `crates/apb-cli/src/budget.rs`; `fn start_check` in
  `crates/apb-cli/src/notify.rs`; `run_resolved`,
  `run_background_resolved`, `start_detached_resolved` exported from
  `crates/apb-engine/src/lib.rs`; `fn tag_summary`, `fn list_playbooks`,
  `fn get_playbook`, `fn validation_issues` in
  `crates/apb-server/src/routes/playbooks.rs`; `is_trial_start` named in
  `crates/apb-engine/src/scheduler/prepare.rs`; `fn parent_run_origin`
  in `crates/apb-engine/src/run_origin.rs`; `pub fn after_stored` in
  `crates/apb-mcp/src/tools/grade.rs`; `pub fn playbook_patch` in
  `crates/apb-mcp/src/tools/supervisor.rs`; `pub fn create` in
  `crates/apb-engine/src/forward_patch.rs`; `fn append_patch_rejected`
  in `crates/apb-engine/src/scheduler/patch.rs`; `pub fn all_runs_of` in
  `crates/apb-engine/src/run_history.rs`;
  `filterProfilesByProject` in `web/src/lib/projectfilter.ts`; `fn
  tool_refusal`, `fn
  resolve_root_for_scope` in `crates/apb-server/src/state.rs`; `fn
  embedded`, `fn sub_tree` in `crates/apb-core/src/bundled/mod.rs`; `fn
  group_by_folder`, `fn install_tree` in
  `crates/apb-core/src/tree_install.rs`; `fn capture_draft`, `enum
  CaptureOrigin` in `crates/apb-mcp/src/tools/capture.rs`; `fn
  scope_parent`, `fn outside_run` in `crates/apb-mcp/src/tools/mod.rs`;
  `pub fn assess`, `pub fn problems` in `crates/apb-engine/src/fit/mod.rs`.
  A missing one is a stop: report it.
- [x] 0.3 None of these exists: `crates/apb-core/src/templates.rs`,
  `crates/apb-mcp/src/tools/templates.rs`, `crates/apb-cli/src/templates.rs`,
  `crates/apb-server/src/routes/templates.rs`,
  `web/src/pages/TemplateList.svelte`, and `rg -n "long.*scope"
  crates/apb-cli/src/run.rs` shows no `scope` flag in `RunArgs`.
- [x] 0.4 `ls examples/playbooks/*.yaml | wc -l` is 15 and `git status
  --short examples/playbooks` (note whether `quality-process/` is
  there; never add, change or delete it).

## 1. A global run never touches a project's candidate line (design G0), then apb run --scope global (design G1, spec `global-playbooks`)

- [x] 1.0 Design G0, tests first: the four of
  `global_scope_run_test.rs`, the unit test beside `project_runs_of`,
  the two consumer regressions (quality comparison, summary), the two of
  `grade_tool_test.rs`, the one of `patch_tool_test.rs`. Run each module: the tests design G0 marks "fails
  without" fail; `a_global_run_leaves_a_project_candidate_alone` may pass
  already. Then `run_origin::is_global`, the four guards and
  `project_runs_of`. Run `cargo test -p apb-engine --test main
  global_scope_run_test`, `cargo test -p apb-engine --lib candidate`,
  `cargo test -p apb-mcp --test main grade_tool_test`, `cargo test -p
  apb-mcp --test main patch_tool_test`, `cargo test -p apb-mcp --test
  main forward_patch_tool_test`: green, no assertion of theirs edited.
- [x] 1.1 Create `crates/apb-cli/tests/suite/global_run_cli_test.rs` with
  the fourteen tests of design G1 "Tests" and declare it. Run it: every test
  fails (an unknown flag).
- [x] 1.2 `RunArgs.scope`, `playbook_ref`, the two early refusals
  (`supervise_global_unsupported`, `--eval-settings`).
- [x] 1.3 The reference in every site of design G1 "Today" items 1 to 7:
  `gate_run`, `remember_input`, `key_lookup`, the unattended refusal, the
  readiness warning, `budget.rs` (`resolve`, `prestart`, `estimate_cmd`),
  `notify.rs` `start_check` and its other callers.
- [x] 1.4 `StartTarget`, `start_target` and the three starts.
- [x] 1.5 `cargo test -p apb --test main global_run_cli_test` green; then
  `cargo test -p apb --test main run_cli_test` and every other CLI suite
  module whose name contains `run`, `budget`, `notify`, `memory` or
  `start_key` (list them with `ls crates/apb-cli/tests/suite`), one
  command each: all green, no assertion of theirs edited.
- [x] 1.6 Docs of design G1 (`docs/HOWTO-authoring.md`, `llms.txt`,
  `README.md`).

## 2. Global playbooks in the dashboard (design G2, spec `global-playbooks`)

- [x] 2.1 Tests first in `crates/apb-server/tests/suite/api_test.rs` (the
  six of design G2 "Tests" item 1). Run: they fail.
- [x] 2.2 Server: `origin_of` (and `fit.rs` using it), `tag_summary` with
  its scope, the aggregated list, the detail with `scope` and
  `validation_issues` taking the origin (`routes/graph.rs` passes
  `Project`). `run_playbook_handler`, `after_start`, `routes/estimate.rs`
  and `routes/notify.rs` are not touched.
- [x] 2.3 `cargo test -p apb-server --test main api_test`, then
  `fit_api_test` and `graph_api_test`: green, no assertion of theirs
  edited.
- [x] 2.4 Web: tests first (`playbooklist.test.ts`, `projectfilter.test.ts`,
  `GlobalPlaybookView.test.ts`, `PlaybookList.test.ts`; design G2 "Tests"
  items 2 to 5), then
  `types.ts`, `api/core.ts`, `playbooklist.ts` (`groupKey`, `groupLabel`,
  `sortGroups`), the filter rename in `projectfilter.ts` and its callers,
  `PlaybookList.svelte`, `GlobalPlaybookView.svelte`, the route in
  `App.svelte`. `PlaybookView.svelte` and `RunStartDialog.svelte` are not
  touched. `cd web && bun run check && bun run test`.

## 3. Template gallery (design G3, spec `template-gallery`)

- [x] 3.1 Core: `crates/apb-core/tests/suite/templates_test.rs` with the
  six tests (failing) and the unit test of `select` in `templates.rs`, then `sub_tree` made `pub(crate)`,
  `templates.rs`, `pub mod templates;`. `cargo test -p apb-core --test
  main templates_test`, then `cargo test -p apb-core --lib bundled`
  (unchanged, green).
- [x] 3.2 Tool layer: `crates/apb-mcp/tests/suite/templates_tool_test.rs`
  with the thirteen tests (failing), then `write_draft` extracted from
  `capture_draft`, `CaptureOrigin::Template`, `tools/templates.rs`.
  `cargo test -p apb-mcp --test main templates_tool_test`, then
  `capture_tools_test` and `discover_tool_test` (green, no assertion of
  theirs edited: the capture answers are byte for byte what they were).
- [x] 3.3 CLI: `crates/apb-cli/tests/suite/templates_cli_test.rs`
  (failing), then `crates/apb-cli/src/templates.rs` and
  `Command::Templates`. `cargo test -p apb --test main templates_cli_test`.
- [x] 3.4 HTTP: `crates/apb-server/tests/suite/templates_api_test.rs`
  (failing), then `routes/templates.rs`, `routes/mod.rs`, the three
  routes in `lib.rs`. `cargo test -p apb-server --test main
  templates_api_test`.
- [x] 3.5 `TemplateSummary` in `ts_contract.rs`; regenerate with
  `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then
  `cargo test -p apb-server --lib ts_contract` passes without the
  variable. Never edit `api.gen.ts` by hand.
- [x] 3.6 Web: tests first in `web/src/lib/templates.test.ts`, then
  `templates.ts`, the three fetch functions in `api/core.ts`,
  `TemplateList.svelte`, the Topbar link, the route in `App.svelte`.
  `cd web && bun run check && bun run test`.
- [x] 3.7 Docs of design G3 "Surfaces" item 5.

## 4. Records (design "Records")

- [x] 4.1 `CLAUDE.md` and `AGENTS.md`: the same shared text in both;
  `diff <(sed -n '/^## What this is/,/^## Claude-specific/p' CLAUDE.md)
  <(sed -n '/^## What this is/,$p' AGENTS.md)` shows no difference in
  the lines this change added (compare the way the files differ on main
  first, so only your additions are judged).
- [x] 4.2 `docs/superpowers/specs/<today>-templates-and-global-playbooks-design.md`.

## 5. Style

- [x] 5.1 Over the lines this change adds (not the unchanged lines of a
  file it touches): no U+2013 or U+2014 dash, no exclamation mark in
  prose or user-facing strings, no CJK: `git diff origin/main | grep '^+'
  | grep -nP "[\x{2013}\x{2014}\x{4E00}-\x{9FFF}]"` prints nothing.
- [x] 5.2 The new and changed Svelte files carry their styles in their
  own `<style>` block with the existing CSS variables; no class attribute
  made of utility names.

## 6. Gates (on the final tree; fix and re-run until each is clean)

- [x] 6.1 `cargo fmt --all -- --check`
- [x] 6.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 6.3 `cargo nextest run --workspace --no-fail-fast` (a
  timing-sensitive test that fails once under load is re-run alone before
  it is judged; known: `signed_store::tests::transact_serializes_writers`)
- [x] 6.4 `APB_CORPUS_REQUIRED=1 APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries/bin cargo nextest run --profile ci -p apb --test main journal_corpus`
- [x] 6.5 `cargo test --workspace --doc`
- [x] 6.6 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`)
- [x] 6.7 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 6.8 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>`, fix, re-run)
- [x] 6.9 `scripts/third-party-notices.sh --check` (no dependency changes
  in this change; it must be up to date)
- [x] 6.10 `cargo clippy --release` (no error; clean warnings of this
  change)
- [x] 6.11 `openspec validate templates-and-global-playbooks --strict`
  (`bunx @fission-ai/openspec` when the binary is missing)
- [x] 6.12 `.ai-notes/improvement/check-forbidden.sh origin/main` of the
  main checkout (`/home/techmeat/projects/tools/agentic-playbooks`), run
  from this worktree: exit 0.

## 7. Close

- [x] 7.1 Commit locally with `git commit -s` (one or a few commits, the
  subject starting "Templates and global playbooks"); no co-author
  trailer, no authorship marker of any tool. Clean tree. No push.
- [x] 7.2 Remove the `$TMPDIR` contents you created; leave
  `$CARGO_TARGET_DIR`. Stop only the processes you started.
- [x] 7.3 Final report in `$HOME/.cache/apb-templates-report.md`: per
  section what was built; the answers to 0.1 to 0.4; every place where
  the code differed from `design.md` and what you did; every file changed
  outside design "Files" with its reason; each gate of section 6 with its
  command, exit code and numbers; the changed file count
  (`git diff --stat origin/main | tail -1`); anything left open.

## 8. Implementation review, round 1

Design "Review record", findings T1 to T8 (now in the body). Every code change
starts from a test that fails for the reason the rule names.

- [x] 8.1 T1: `origin_of_events` and the three answers at every caller
- [x] 8.2 T2: settlement and `trial_of` check the origin
- [x] 8.3 T3: `store_path_linked` in `write_draft`; the two tests
- [x] 8.4 T4: one tail per start form; the comparison test
- [x] 8.5 T5 items 1 to 4, the browser pass included
- [x] 8.6 T6: the warnings function and the install test
- [x] 8.7 T7 items 1 to 5
- [x] 8.8 T8 items 1 to 6
- [x] 8.9 Section 6 gates again, all of them, with numbers

## 9. Implementation review, round 2

Design "Review record", findings X1 to X5 (now in the body). Test first.

- [x] 9.1 X1: the store check beneath the resolved root; the tests
- [x] 9.2 X2: a missing journal is unreadable; the tests
- [x] 9.3 X3: one parser at the four readers; the test
- [x] 9.4 X4: the Templates page on a failed project list
- [x] 9.5 X5 items 1 and 2
- [x] 9.6 The public design record states the limit of X1; section 6
  gates again, all of them, with numbers

## 10. Close

- [x] 10.1 Rebase onto main after "Inbox waits"; the global scope logic
  checked where it meets the inbox wait (origin label, patch guards,
  start key)
- [x] 10.2 The review findings folded into the design body; a short
  Review record kept; the public design record matches
- [x] 10.3 Section 6 gates on the final tree
- [x] 10.4 Archive the change; a Purpose for each new spec

## File budget

70 changed files, listed in `design.md` "Files": G0 14, G1 8, G2 16,
G3 24, records 8. The number is what the design needs, not a budget.
