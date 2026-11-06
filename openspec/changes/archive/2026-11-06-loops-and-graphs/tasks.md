# Tasks: loops and graphs that check themselves

Work in order, one section at a time. Before a section, read its part of
`design.md` and the spec delta it names. Tick each box (`- [x]`) as soon as
it is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

For every behaviour: write the failing test first, run it and see it fail,
then write the code. For a test that names a field or a module that does
not exist yet, a compile error is the expected first failure.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-loops-target TMPDIR=$HOME/.cache/apb-loops-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-loops-target` fails with a read-only
file system, use `CARGO_TARGET_DIR=/tmp/apb-loops-target` and
`TMPDIR=/tmp/apb-loops-tmp` instead, and before each build check that
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
`cargo test -p <crate> --test main <module>`). The wider gates run in
section 8.

## 1. Array and object fields in templates (D0, spec `prompt-templates`)

- [x] 1.1 Unit tests for `apb_core::schema::output_field_text` (in
  `schema.rs` tests): array, object, string, bool, number, null, absent
  field, output that is not JSON, output that is a JSON array (not an
  object).
- [x] 1.2 Implement `output_field_text` next to `output_field_value`; leave
  `output_field_value` unchanged.
- [x] 1.3 Use it in `crates/apb-engine/src/context.rs` `resolve` for the
  `nodes.<id>.output|report.<field>` form only. Test in
  `crates/apb-engine/tests/suite/context_test.rs`: `context::render` with a
  node output `{"findings":[{"id":"a"}]}` renders
  `{{nodes.<id>.output.findings}}` as `[{"id":"a"}]`.
- [x] 1.4 Regression test in `parallel.rs` tests: an `output_field` edge on
  an array field still does not match.
- [x] 1.5 `docs/HOWTO-authoring.md` "Template variables": one paragraph.
- [x] 1.6 `cargo test -p apb-cli --test main bundled_scenarios_test` passes.

## 2. Rejection reason and repair scope (D3, spec `repair-briefing`)

- [x] 2.1 Decode test in `crates/apb-engine/src/event.rs` tests: an old
  `attempt_finished` line without `rejected_for` reads as `None`.
- [x] 2.2 Add `rejected_for` to `EventPayload::AttemptFinished`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`). Fix every
  constructor the compiler lists with `rejected_for: None`.
- [x] 2.3 Set it at the four sites in `scheduler/node.rs` named in D3
  (lines 2741, 2801, 3025, 3072), `(!fatal).then(...)` where the append
  precedes `if fatal`; scrub with `Scrubber::for_workspace(workdir,
  Vec::new())`, then `chars().take(500)`.
- [x] 2.4 Unit tests in `attempt_memory.rs`: a rejected attempt renders the
  whole quoted reason and ends with `REPAIR_SCOPE`; a transient failure
  after a rejection has no paragraph; a list item rejection newer than the
  last `attempt_finished` gets it; an older one does not; a first attempt
  gets no briefing.
- [x] 2.5 Implement `AttemptRecall.rejected_for`, the `ending` text, the seq
  kept by `rejections`, and `REPAIR_SCOPE` in `attempt_memory.rs`.
- [x] 2.6 E2e in `crates/apb-engine/tests/suite/attempt_briefing_test.rs`
  (reuse the check fixtures of `success_check_test.rs`): a stand-in agent
  whose first report a `success_check` rejects; attempt 2 (fresh session)
  has the reason and `Repair scope:` in the prompt it was given. Give the
  node `cache: auto` and assert the execution's `NodeCacheMiss.key` equals
  its `NodeCacheStored.key`.
- [x] 2.7 Bundled `fix` prompt sentence (D3), the Repair row of
  `bundled/skills/apb-delivery/references/stages.md`, a patch bump of
  `apb-delivery/SKILL.md` `metadata.version`, and the one patch bump of the
  `apb-feature-delivery` playbook `version` for this whole change.
- [x] 2.8 `docs/HOWTO-authoring.md` "The retry briefing".
- [x] 2.9 `cargo test -p apb-engine --lib attempt_memory` and
  `cargo test -p apb-engine --test main attempt_briefing_test` pass.

## 3. `no_new_items`: core (D2, spec `loop-exit`)

From 3.3 until 4.3 `apb-engine` does not build (its exhaustive matches lack
the new variant). Run only `cargo test -p apb-core` in this section.

- [x] 3.0 Create `crates/apb-core/src/loop_items.rs` with the `LoopItems`
  type and an `evaluate` that returns a default, and declare
  `pub mod loop_items;` in `crates/apb-core/src/lib.rs`.
- [x] 3.1 Unit tests in `loop_items.rs`: strings; integers; objects by
  `key`; an array held in a string; missing field, non-array, element
  without key, bad key character (all unreadable); `[]` dry;
  `[a,b]`, `[a]`, `[b,a]` with rounds 2 matches only after the third;
  `[a]`, `[b]`, `[a]`, `[a]`: streak 0, 0, 1, 2; an unreadable visit resets
  the streak and adds nothing to `seen`; `seen` keeps first-seen order.
- [x] 3.2 Implement `evaluate` (D2 rules 1 to 7); make
  `apb_core::list::items::id_of` `pub(crate)` and reuse it.
- [x] 3.3 Add `EdgeCondition::NoNewItems` to `schema.rs`; round-trip test of
  the YAML form with and without `key`.
- [x] 3.4 Handle the variant in every `apb-core` match the compiler lists,
  and in the two it does not: `validate/graph.rs` line 232 and
  `validate/node_io.rs` line 138 (D2 "Matching").
- [x] 3.5 Confirm V164 is free. New `crates/apb-core/src/validate/loop_items.rs`
  with one failing test per rule of V164, V165 and V166 first, then the
  rules; call the check from `validate()` in `validate/mod.rs` next to line
  383 with a `// V164-V166` comment. Add `nodes.<id>.seen.<field>` to the
  template grammar and to `V13_KNOWN_NAMESPACES`. Test that V34 is reported
  when the loop edge `hunt -> hunt` is unconditional, in either order
  relative to the exit edge.
- [x] 3.6 `cargo test -p apb-core` passes.

## 4. `no_new_items`: engine (D2)

- [x] 4.1 Confirm `node_finished.output` is the whole output; note the
  answer in the final report.
- [x] 4.2 `RunState.success_outputs`, folded in the `NodeFinished` arm
  (`state.rs` lines 588 to 601) when
  `NodeStatus::from_label(status) == NodeStatus::Succeeded`. State tests: a
  failed visit is not added; a `continue_from` keeps earlier visits.
- [x] 4.3 `edge_matches_at` and `condition_reads` in `parallel.rs`; every
  other `apb-engine` match the compiler lists; `diagnostics/route.rs`
  (`kind_name`, `observed()` per D2); `report_html/graph.rs` `edge_label`
  arm. `apb-engine` builds again.
- [x] 4.4 The seen text per D2 "The seen template" points 1 to 5:
  `loop_seen.rs` with `seen_texts`, `JudgeInputs.seen`, the `playbook`
  parameter on `JudgeInputs::from_state` and `render_node_prompt` with all
  call sites, the `seen` parameter on `context::render` and `resolve`.
  Unit tests of `seen_texts`: the 200-key bound, the 8 KiB bound, the
  `(N more not shown)` line, empty on a first visit, the first condition in
  edge order when two share node and field. `judge_eval/example.rs`
  `run_values` fills `seen`; a serialized `JudgeInputs` with an empty map has
  no `seen` key.
- [x] 4.5 E2e in `crates/apb-engine/tests/suite/loop_edges_test.rs` (reuse
  its `write_agent` and loop fixtures; the loop node is an `agent_task`
  stand-in that writes its prompt to a file, since a script node has no
  prompt): a loop with scripted findings stops at the dry round before its
  cap; a loop that keeps finding stops at the cap through the fallback;
  resume after a crash between rounds gives the same route; round 3's
  prompt lists the seen keys; a patch that adds the exit edge mid-run takes
  effect at the next routing. Cache keys: with `cache: auto` and a prompt
  that reads only `{{nodes.hunt.seen.findings}}` (no `nodes.hunt.output`),
  over visits `[a,b]`, `[c]`, `[a]`, `[b]`: visit 2's `NodeCacheMiss.key`
  differs from visit 1's, and visit 3's differs from visit 2's.
- [x] 4.6 `cargo test -p apb-engine --lib` and
  `cargo test -p apb-engine --test main loop_edges_test` pass.

## 5. `no_new_items`: web and docs (D2)

- [x] 5.1 `web/src/lib/graph.ts`: `edgeLabel` and `exitCaption` cases for
  `no_new_items`. `playbookedit.ts`, `nodefields.ts`, `NodePanel.svelte`
  keep the condition on save. Vitest: load, save, reload keeps
  `no_new_items` with all its fields; both labels. Plain CSS only;
  Tailwind is banned.
- [x] 5.2 `docs/HOWTO-authoring.md`: the section "Loops that stop when
  nothing new turns up" under "Bounded loops" with a three-row V164 to V166
  table in the format of the V160 to V162 table at line 211.

## 6. Read-only batches: engine (D1, spec `read-only-batches`)

- [x] 6.1 Tests first, in `crates/apb-engine/tests/suite/read_only_test.rs`
  (or a new `read_only_batch_test.rs` declared in the suite's `main.rs`):
  6.1.1 two `read_only` scripts that sleep and do not write,
  `max_parallel: 2`: both succeed, and both `read_only_checked` events
  precede both `node_finished` events (a script node journals no
  `attempt_started`);
  6.1.2 in a git work tree with a tracked file, the same with one script
  that sleeps, then writes the tracked file: both fail `read_only_dirty`,
  each `read_only_violated.concurrent_with` names the other, no
  `retry_started`. Leave `fork.on_branch_failure` at its default;
  6.1.3 a writer script and a `read_only` script ready together never
  overlap, whichever is `current`;
  6.1.4 a `read_only` member finishes first, its successor is a batchable
  writer, another `read_only` member still runs: the writer starts after it
  ends;
  6.1.5 a `protect` node and a `read_only` node never overlap;
  6.1.6 an `isolation: full` read-only node still batches with a writer
  (Linux with bubblewrap only; skip as the existing sandbox tests do);
  6.1.7 an old `read_only_violated` line without `concurrent_with` decodes
  (in `event.rs` tests).
- [x] 6.2 `scheduler/batch_class.rs`: `WriteClass`, `write_class`,
  `batch_admits`, the moved `detected_read_only`; `is_batchable` without
  `detected_read_only`; its doc comment updated.
- [x] 6.3 Batch formation in `scheduler.rs` and mid-batch admission in
  `scheduler/pipeline.rs` (one candidate at a time, pushing each admitted
  node before the next check) both use `batch_admits`.
- [x] 6.4 `BatchIntervals` and `ExecOptions.batch_peers` per D1 "Who ran
  beside whom"; `Before::finish` gets the overlapping set;
  `concurrent_with` on `ReadOnlyViolated`; the message forms of D1.
- [x] 6.5 Surfaces per D1 "Surfaces": `run_view.rs` `ReadOnlyViolation` and
  `ReadOnlyRow::line()` with unit tests of both line forms;
  `AttemptIsolationPanel.svelte` and `journal.ts` with vitests
  (`AttemptIsolationPanel.test.ts`). `readOnlyText` stays unchanged.
- [x] 6.6 `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`
  (regenerates `web/src/lib/api.gen.ts` for `JudgeInputs` from section 4
  and the read-only types from this section), then the same without the
  variable to confirm no drift.
- [x] 6.7 `cargo test -p apb-engine --lib` and the read-only suite tests
  pass.

## 7. Read-only batches: bundled playbooks and docs (D1)

- [x] 7.1 `apb-feature-delivery`: `review_correctness` and
  `review_security` get `read_only: true` and lose `protect`; `review_tests`
  and every other node keep theirs. No second version bump (2.7 did it).
- [x] 7.2 `apb-code-review`: `review_correctness`, `review_security`,
  `review_tests` get `read_only: true` and lose `protect`; `scope` and
  `native_checks` keep `protect`. One patch bump of its `version`.
- [x] 7.3 One new scenario per playbook: a reviewer scripted as
  `read_only_dirty` fails at once with no retry, asserted by its `failed`
  status; the run outcome follows each playbook's routing. Fix `clean-panel` and
  `single-change-clean-panel` only if they assert order.
- [x] 7.4 Docs: `docs/GUARDRAILS.md`, `docs/ISOLATION.md`,
  `docs/HOWTO-authoring.md` (read-only node, and the `protect` sentence
  about batches).
- [x] 7.5 `cargo test -p apb-cli --test main bundled_scenarios_test` and
  the bundled content lint
  (`cargo test -p apb-core --test main bundled_content_lint_test`) pass.

## 8. Records and gates

- [x] 8.1 `CLAUDE.md` and `AGENTS.md`: add `loop_items` to the `apb-core`
  module list and `loop_seen`, `scheduler/batch_class` to the `apb-engine`
  list, with the same shared text in both files.
- [x] 8.2 Write `docs/superpowers/specs/2026-11-06-loops-and-graphs-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article or
  project.
- [x] 8.3 Style over every changed doc and bundled file: no U+2013 or
  U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing) and no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`
  and `![` do not count).
- [x] 8.4 Gates, each with its numbers in the report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test -p apb-core`; `cargo test -p apb-engine`;
  `cargo test -p apb-cli --test main bundled_scenarios_test`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix every failure and every code-ranker violation (read
  `code-ranker docs base <ID>` first). The full workspace suite with the
  journal corpus runs after you, outside this task.
- [x] 8.5 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if it
  is under `/tmp`. Stop only the processes you started.
- [x] 8.6 Final report: per section what was built, decisions you took where
  `design.md` left room, each gate with numbers, the answer to 4.1, the
  user-visible changes for the release notes, the changed file count, and
  anything left open.
