# Tasks: recurring routines

Work in order, one section at a time. Before a section, read its part of
`design.md` and the spec delta it names. Tick each box (`- [x]`) as soon as
it is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

For every behaviour: write the failing test first, run it and see it fail,
then write the code. For a test that names a field or a module that does
not exist yet, a compile error is the expected first failure.

The change has three features, built one after another: A (previous
success, sections 1 to 4), B (param patterns, sections 5 to 7), C
(untrusted input, sections 8 to 11). Each feature is finished, documented,
tested and gated before the next starts, so a tree that stops after any
feature's gate section is coherent and green. Do not start a section of a
later feature while a box of an earlier one is open.

This change starts from main e3ae94cf, which already holds
`node-contracts`; no rebase is needed. Where this change adds lines to a
list `node-contracts` also extended (`validate/mod.rs`, `schema.rs`,
`validate/guarded.rs`, CLAUDE.md, AGENTS.md), keep every existing line and
add after it. `design.md` D12 lists where the two changes meet.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-recurring-target TMPDIR=$HOME/.cache/apb-recurring-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-recurring-target` fails with a read-only
file system, use `CARGO_TARGET_DIR=/tmp/apb-recurring-target` and
`TMPDIR=/tmp/apb-recurring-tmp` instead, and before each build check that
`df -h /tmp` shows more than 4G free; if not, stop and report it. `/tmp` is a
RAM disk shared with other sessions. Never use `/dev/shm`, and never put a
target dir, cache or temp data inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR`. Never stop a process you did not start. Do not commit,
push or open a pull request: the git directory is read-only for you. Do not
start subagents.

During a section run only the tests of the crates it touches
(`cargo test -p <crate> --lib <module>`,
`cargo test -p <crate> --test main <module>`). Each feature's last section
runs that feature's gates; section 12 runs the whole workspace once.

## 1. A: previous success, core (D1, spec `previous-success`)

- [x] 1.0 Confirm V176 to V182 are free:
  `git grep -ohE '"V1[0-9][0-9]"' crates | sort -u | tail -5`. If taken, use
  the next free codes and update `design.md` and the spec deltas.
  Checked: highest taken is V175, V176 to V182 free.
- [x] 1.1 Create `crates/apb-core/src/previous_success.rs` with `PREFIX`,
  `PrevRef`, `parse`, `key`, `refs`, `reads`, `PreviousSuccessBlock` and
  `WhenNone` returning defaults; declare it in `lib.rs`.
- [x] 1.2 Unit tests in that file: `parse` of `id`, `started_at`,
  `finished_at`, `nodes.a.output`, `nodes.a.report.f` (key
  `nodes.a.output.f`), `nodes.a.output.f.g` (error), `nodes.a.output.`
  (error), `foo` (error), the bare prefix (error), `run.id` (`None`);
  `refs` over a playbook reading two of them from a prompt and a cache key,
  sorted and without repeats; `reads` true for a malformed reference.
- [x] 1.3 Implement them. Add `Playbook.previous_success` in `schema.rs`
  with a YAML round-trip test (with and without `when_none`); an unknown
  key inside the block is a parse error.
- [x] 1.4 Make `apb_core::duration::parse_rfc3339_ms` `pub`.
- [x] 1.5 New `crates/apb-core/src/validate/previous_success.rs`: one
  failing test per V176 rule first (unknown key, deep path, unknown node,
  a read in the worktree template, a `when_none` literal that is not RFC
  3339, a `when_none` template reading `nodes.a.output`, a `when_none`
  template reading a secret param), and that the unknown key is reported
  as V176 and not V13; then the rules, called from `validate()` next to
  `check_templates` with `// V176`. Grammar in `validate/templates.rs`
  `check_text` and `V13_KNOWN_NAMESPACES`.
- [x] 1.6 V46 in `validate/node_io.rs` line 135 for
  `run.previous_success.nodes.<id>.output.<field>`; test: a field the node
  does not declare warns V46.
- [x] 1.7 `GuardedFeature::PreviousSuccess` in `validate/guarded.rs`,
  appended after `WriteScope` (`ALL` becomes `[GuardedFeature; 6]`); test:
  a playbook reading `{{run.previous_success.id}}` lists it, one that does
  not lists nothing new, and one that also declares `writes` lists
  `[WriteScope, PreviousSuccess]` in that order.
- [x] 1.8 `cargo test -p apb-core` passes.

## 2. A: previous success, engine (D2 to D4)

- [x] 2.1 Decode test in `event.rs` tests: an old `run_provenance` line
  without `previous_success` reads as `None`.
- [x] 2.2 `PreviousSuccess` and the `RunProvenance.previous_success` field
  (`lenient_option`); every literal the compiler lists gets `None`.
- [x] 2.3 `RunHead.origin` and `started_checked` in `run_history.rs`; tests
  in its `tests` module: a head with a `run_provenance` origin `global`
  reads `Some("global")`, one without reads `None`; a head whose second
  line is not JSON is `Err("journal")`; a journal with only `run_queued`
  lines and no `run_started` is `Ok(None)`.
- [x] 2.4 New `crates/apb-engine/src/previous_success.rs` with `find`,
  `HistoryUnreadable`, `snapshot`, `template_values`, `patch_refusal` and
  `OUTPUT_MAX_BYTES`. Unit tests of `find` (seed run directories as the
  `run_history.rs` tests do, with `in_plain_child` and a stamped origin):
  a failed newer run is skipped; another scope is skipped; a child run is
  skipped; a scenario and an eval run are skipped; an unstamped run is
  skipped even when it does not read; a run revived after `run_finished`
  and not finished again is skipped; the newest success wins by
  `run_started` time, not file time; a stamped run newer than the latest
  success whose journal, manifest or `run.yaml` does not read is `Err`
  naming it and what did not read; the same unreadable run older than the
  latest success is not reached and gives `Ok`.
- [x] 2.5 Unit tests of `snapshot`: times through `iso_utc` (milliseconds
  truncated); an array field as compact JSON; an absent node adds no key;
  a value over 16 KiB is cut on a char boundary with `(clipped)`; the
  earlier run's secret param value is replaced; an earlier run whose
  `run.yaml` does not read puts the key in `withheld`; no earlier run with
  `when_none` over params gives `fallback: true` and the rendered times; a
  `when_none` that renders `yesterday` is `Err` and the error text does
  not hold `yesterday`.
- [x] 2.6 `prepare.rs` computes it after the defaults fill per D3 "Where
  the start records it" (scenario, eval and child: no lookup;
  `continued_from`: copy; else `find`, whose `Err` refuses), keeps it for
  the `RunProvenance` append, only when
  `apb_core::previous_success::reads`. `RunState.previous_success` folded
  in `state.rs` (the `RunProvenance` arm of `fold`, line 565).
- [x] 2.7 `patch_refusal` called in `scheduler/patch.rs`; tests in its
  `tests` module: a patch adding a read of an output key the start did not
  read is `patch_rejected previous_success_unrecorded`; a patch adding the
  namespace to a run that recorded none is refused; a patch keeping the
  start's reads passes.
- [x] 2.8 Rendering per D4: `JudgeInputs.previous_success`, the `previous`
  parameter on `context::render` and `resolve` with every caller,
  `unfilled`, `judge_eval/example.rs` `run_values`, the `render_state`
  output class. Tests: a serialized `JudgeInputs` with an empty map has
  no `previous_success` key; a judge state field reading
  `{{run.previous_success.nodes.a.output}}` is a `RenderedField` with
  class `output`.
- [x] 2.9 E2e in a new `crates/apb-engine/tests/suite/previous_success_test.rs`
  (declared in the suite's `main.rs`; reuse the stand-in agent of
  `loop_edges_test.rs` that writes its prompt to a file): run 1 of a
  playbook whose prompt reads `started_at`, `id` and
  `nodes.collect.output.items` renders the `when_none` time and empty id;
  after it succeeds, run 2 renders run 1's id, start time and items;
  a run 3 that fails does not move run 4's window off run 2; a crash of
  run 2 after its first node and a resume after another run succeeded
  renders run 1's values again; a run continued from a failed run keeps
  the predecessor's record; a child run of the playbook started by a
  parent playbook renders the `when_none` time and empty id although a
  top-level run succeeded; a start after a stamped newer run's journal
  was corrupted is refused with `previous_success_unreadable` and leaves
  no run directory; the journal of a run whose definition does not read
  the namespace has no `previous_success` key and no new guard; with
  `cache: auto`, run 2's `NodeCacheMiss.key` differs from run 1's.
- [x] 2.10 `cargo test -p apb-engine --lib previous_success` and
  `cargo test -p apb-engine --test main previous_success_test` pass.

## 3. A: previous success, surfaces and docs (D5)

- [x] 3.1 `run_view::PreviousSuccessView` and `run_view::previous_success`;
  CLI line in `apb-cli/src/run.rs` with a test in
  `crates/apb-cli/tests/suite/runs_detail_cli_test.rs`
  (`runs_with_an_id_shows_the_previous_success`, the three line forms).
- [x] 3.2 MCP `run_status` block with a unit test next to
  `run_status_lists_the_run_limits` (`tools/run.rs` line 1530): present
  with `run_id`, no `outputs` key and no snapshotted value; absent for a
  run whose definition does not read the namespace.
- [x] 3.3 `RunDetail.previous_success` (the view), the ts export, a test of
  the run detail route that the JSON holds no `outputs` key and no
  snapshotted value, `previoussuccess.ts` `previousSuccessLine` with
  vitests, `RunDriveNotes.svelte` and `RunView.svelte`. Plain CSS only;
  Tailwind is banned.
- [x] 3.4 HTML report header line, with a test in the report's tests that
  the line is present when recorded and absent otherwise.
- [x] 3.5 Docs: `docs/HOWTO-authoring.md` "Template variables" and
  `docs/CI.md` "Any other runner" per D5.

## 4. A: records and gates

- [x] 4.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both: in the
  `apb-core` list add `previous_success.rs` (the `run.previous_success`
  grammar and the `when_none` block) and the validator module
  `previous_success` for V176; in the `apb-engine` list add
  `previous_success.rs` (the earlier successful top-level run, the checked
  lookup, its snapshot on `run_provenance` and the patch refusal).
- [x] 4.2 Style over every doc and bundled file this feature changed: no
  U+2013 or U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing)
  and no exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--` and `![` do not count).
- [x] 4.3 Gates, each with its numbers noted for the final report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test -p apb-core`; `cargo test -p apb-engine`; `cargo test -p apb-cli --test main runs_detail_cli_test`; `cargo test -p apb-mcp --lib tools::run`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix every failure and every code-ranker violation (read
  `code-ranker docs base <ID>` first) before the next feature.

## 5. B: param patterns, core (D6, spec `param-pattern`)

- [x] 5.1 `Param.pattern` and `Param::default_text` (moved body of
  `stringify_param_default`; the three engine callers use it). YAML
  round-trip test of `pattern`.
- [x] 5.2 New `crates/apb-core/src/param_pattern.rs` with unit tests first:
  `[a-z]+` rejects `abc; rm` and `ABC`, accepts `abc`; `a|b` rejects `ab`
  (the group); the empty value is checked; a non-text param and a param
  without pattern pass; a pattern over 512 characters and one that does
  not compile give `Err` from `compile`; the `Mismatch` holds the name and
  pattern only. Then the code.
- [x] 5.3 V177 and V178 in `validate/params.rs` with one failing test per
  rule (non-text, empty, too long, does not compile, a default that does
  not match, a number default on a text param checked through
  `default_text`), then the rules; a secret param with a pattern is valid.
- [x] 5.4 `Incompat::PatternMismatch` and `check_value` in
  `run_memory/schema.rs`; tests in
  `crates/apb-core/tests/suite/run_memory_test.rs`: a remembered value
  that does not match is incompatible with `pattern_mismatch` and the
  param is in `missing`; `remember` refuses it with that reason.
- [x] 5.5 Scenario V178 in `scenario/validate.rs` `check_params`, with a
  test; a secret param's placeholder is not checked.
- [x] 5.6 `cargo test -p apb-core` passes.

## 6. B: param patterns, launch surfaces and bundled (D6, D11 item 1.1)

- [x] 6.1 Tests in `gate/params.rs` tests: a given value that does not
  match is refused with `param_pattern_mismatch`, `invalid` and
  `patterns`, and the refusal text holds no part of the value (use a
  secret param with value `s3cr3t-value`); a missing value is answered
  first; the start actions give `ProvideParam` per invalid name.
- [x] 6.2 `PARAM_PATTERN_MISMATCH`, the check in `check_params`, the
  export in `gate/mod.rs`, the `from_refusal` arm in
  `next_action/start.rs`, the CLI message in `apb-cli/src/run.rs`.
- [x] 6.3 One test per surface that it refuses: CLI in
  `crates/apb-cli/tests/suite/next_actions_cli_test.rs`
  (`run_refuses_a_param_that_does_not_match_its_pattern`, no run
  directory created), MCP in `crates/apb-mcp/src/server/tests.rs` next to
  line 2732, server in `crates/apb-server/tests/suite/fit_api_test.rs`
  next to line 113.
- [x] 6.4 The preparation backstop in `prepare.rs`; e2e in a new
  `crates/apb-engine/tests/suite/param_pattern_test.rs`: a start through
  `run_background_resolved` with a mismatching value fails before a run
  directory exists; a scenario run of a playbook with a secret param that
  has a pattern starts.
- [x] 6.5 `incompatText` case, `PlaybookParam.pattern`, the pattern note in
  `paramFields`; vitests in `runinput.test.ts`.
- [x] 6.6 `apb-issue-triage`: patterns on `owner` and `repo` and the
  version 1.0.1 (D11 item 1.1 and 1.4; the markers follow in section 10
  without a second bump).
- [x] 6.7 Docs: `docs/HOWTO-authoring.md` "Param patterns" per D6.

## 7. B: records and gates

- [x] 7.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both: in the
  `apb-core` list add `param_pattern.rs` (a text param's anchored pattern;
  V177 and V178 live in `validate/params`).
- [x] 7.2 Style over every doc and bundled file this feature changed: no
  U+2013 or U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing)
  and no exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--` and `![` do not count).
- [x] 7.3 Gates, each with its numbers noted for the final report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test -p apb-core`; `cargo test -p apb-engine`; `cargo test -p apb-cli --test main`; `cargo test -p apb-mcp`; `cargo test -p apb-server`; `cargo test -p apb-cli --test main bundled_scenarios_test`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun run check && bun run test && bun run build`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix every failure and every code-ranker violation (read
  `code-ranker docs base <ID>` first) before the next feature.

## 8. C: untrusted input, core (D7, D8, spec `untrusted-input`)

- [x] 8.1 The markers: `Param.untrusted`, `ConnectorBinding.untrusted`
  (both serde impls; tests: the bare string still round-trips, an
  `untrusted: true` binding round-trips as an object), `Node.untrusted`,
  `Node.accept_untrusted`. Fix every literal the compiler lists.
- [x] 8.2 Move `reads_recorded_context` into `apb_core::template`; the
  engine function delegates; its two tests in `context.rs` still pass.
- [x] 8.3 New `crates/apb-core/src/untrusted.rs`; unit tests first: each
  of rules 1 to 7 alone taints; `a -> b -> c` through outputs taints `c`
  with nodes listed `c, b, a`; `run.context` is tainted by an untrusted
  param alone; any `run.previous_success.nodes.x.output` read is tainted
  with source `EarlierRun` although `x` carries no marker in the
  definition (the case of a version that removed the marker); a judge's
  answer read by a later prompt is not tainted; a finish prompt without a
  context read is tainted while anything is; an `agent_task` whose
  template-form `writes` reads an untrusted param, or a field of a tainted
  node, is tainted (rule 8) and one whose glob-form `writes` reads nothing
  is not; the `outward` reasons
  (unisolated, `best_effort`, full with network open, full with network
  none and no connector gives none, a connector, a declared effect, a
  finish, a playbook node); the `secrets` entries; `Source::label` texts.
  Then the code.
- [x] 8.4 New `crates/apb-core/src/validate/untrusted.rs`: one failing
  test per V179 to V182 rule (including: an accepted node gets no V179
  and still V180; `accept_untrusted: " "` is V181; an accepted node that
  is isolated with network none is V182), then the rules, called from
  `validate()` after `isolation::check_isolation` with `// V179-V182`.
- [x] 8.5 `GuardedFeature::UntrustedInput` in `validate/guarded.rs`,
  appended after `PreviousSuccess` (`ALL` becomes `[GuardedFeature; 7]`);
  tests:
  a playbook with a follower that renders untrusted text lists it, one
  with markers that reach no follower does not, and
  `GuardedFeature::parse("untrusted_input")` round-trips.
- [x] 8.6 `cargo test -p apb-core` passes.

## 9. C: untrusted input, engine, surfaces, editor (D9, D10)

- [x] 9.1 Decode test in `event.rs` tests: an old `run_provenance` line
  without `untrusted` reads as empty. `UntrustedNode`,
  `RunProvenance.untrusted`, filled in `prepare.rs`;
  `run_view::untrusted_nodes`.
- [x] 9.2 `context::untrusted_section` with unit tests (empty for no
  sources; the fixed text with two labels). Appended in `scheduler/node.rs`
  to `attempt_prompt` after the evidence section, before `AgentTask`, and
  to the finish composer's final text.
- [x] 9.3 E2e in a new `crates/apb-engine/tests/suite/untrusted_prompt_test.rs`
  (stand-in agent writing its prompt to a file): a node rendering an
  untrusted param gets the section, one that does not gets none; a list
  node that binds a connector (mock connector of the connector tests),
  has `evidence` and takes items from a tainted node has a prompt whose
  last section, on its first attempt and on a retry, is
  `## untrusted input`; a node with `writes` (its write scope note) and a
  rejected first attempt (the retry briefing with its `REPAIR_SCOPE`
  paragraph) still ends both prompts with `## untrusted input`; the text `render_node_prompt` returns for that
  node (what its cache key is derived from) holds no
  `## untrusted input`; the run's `run_provenance.untrusted` names the
  node, its sources and its accepted reason; the journal has a
  `run_guard` whose `requires` holds `untrusted_input`, and a resume with
  that name removed from the known set is refused the way an unknown
  guard name is (reuse the existing unknown-guard test helper in
  `event.rs` tests or `scheduler/patch.rs` tests).
- [x] 9.4 Surfaces per D9: CLI line (test in `runs_detail_cli_test.rs`),
  MCP `run_status` block (unit test), `RunDetail.untrusted` and the ts
  export, `untrustedLine` and `RunDriveNotes.svelte` (vitests), the HTML
  report section (test).
- [x] 9.5 `web/src/lib/connectorbinding.ts` per D10 with vitests in
  `connectorbinding.test.ts`; `PlaybookParam.untrusted` in `types.ts`.
- [x] 9.6 `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`,
  then the same without the variable to confirm no drift.
- [x] 9.7 Docs: `docs/HOWTO-authoring.md` "Untrusted input",
  `docs/CONNECTORS.md` (line 326), `docs/GUARDRAILS.md` (one row) per D8.
- [x] 9.8 `cargo test -p apb-engine --lib` and
  `cargo test -p apb-engine --test main untrusted_prompt_test` pass.

## 10. C: bundled content (D11)

- [x] 10.1 `apb-issue-triage`: the binding marker and `accept_untrusted`
  on its eight agent nodes (version already 1.0.1 from 6.6).
- [x] 10.2 `apb-inbox-digest`: the binding marker, `accept_untrusted` on
  its three agent nodes, version 1.0.1.
- [x] 10.3 `bundled_agents_that_read_untrusted_text_say_why` in
  `crates/apb-core/tests/suite/bundled_content_lint_test.rs`.
- [x] 10.4 `cargo test -p apb-cli --test main bundled_scenarios_test` and
  `cargo test -p apb-core --test main bundled_content_lint_test` pass.

## 11. C: records and gates

- [x] 11.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both: in
  the `apb-core` list add `untrusted.rs` (untrusted markers and how they
  reach agents) and the validator module `untrusted` for V179 to V182.
- [x] 11.2 Style over every doc and bundled file this feature changed: no
  U+2013 or U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing)
  and no exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--` and `![` do not count).
- [x] 11.3 Gates, each with its numbers noted for the final report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test -p apb-core`; `cargo test -p apb-engine`; `cargo test -p apb-cli --test main`; `cargo test -p apb-mcp`; `cargo test -p apb-server`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun run check && bun run test && bun run build`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix every failure and every code-ranker violation (read
  `code-ranker docs base <ID>` first) before the next feature.

## 12. Public record, workspace gate, report

- [x] 12.1 Write `docs/superpowers/specs/2026-11-07-recurring-routines-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project. Style check it as in 4.2.
- [x] 12.2 `cargo nextest run --workspace` (or `cargo test --workspace`
  when nextest is missing) and `cargo test --workspace --doc`, with their
  numbers.
- [x] 12.3 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started.
- [x] 12.4 Final report: per section what was built, decisions you took
  where `design.md` left room, each gate with its numbers, the user-visible
  changes for the release notes, the changed file count, and anything left
  open.
