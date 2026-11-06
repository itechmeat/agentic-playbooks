# Tasks: node contracts

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
export CARGO_TARGET_DIR=$HOME/.cache/apb-node-contracts-target TMPDIR=$HOME/.cache/apb-node-contracts-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-node-contracts-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-node-contracts-target`
and `TMPDIR=/tmp/apb-node-contracts-tmp` instead, and before each build
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

The change is built as four feature parts, in this order: A (typed
outputs, numeric edges and the bundled updates), B (stall exit), C
(proven checks), D (write scope). Each part is fully built, documented and
gated before the next one starts; the last section runs the whole
workspace suite once and writes the records. Inside a part run only the
tests of the crates it touches (`cargo test -p <crate> --lib <module>`,
`cargo test -p <crate> --test main <module>`) until its gate.

The part gate, run at the end of every part, each with its numbers in the
report: `cargo fmt --all -- --check`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`cargo test -p apb-core`; `cargo test -p apb-engine`;
`cargo test -p apb-cli --test main bundled_scenarios_test`;
`cargo test -p apb-server --lib ts_contract`;
`cd web && bun run check && bun run test` when the part touched `web/`;
`cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
Fix every failure and every code-ranker violation (read
`code-ranker docs base <ID>` first). `scheduler/node.rs` is already large:
keep new logic in the new modules and leave only calls there.

## A1. Typed fields: core (D1, spec `typed-outputs`)

From A1.4 until A2.1 `apb-engine` may not build (its name-list sites change
type). Run only `cargo test -p apb-core` in this section.

- [x] A1.1 Confirm V167 to V175 are free:
  `git grep -ohE '"V1[67][0-9]"' crates | sort -u` shows none of them.
  Create `crates/apb-core/src/output_contract.rs` with the types and
  functions of D1 returning defaults, declared `pub mod output_contract;`
  in `crates/apb-core/src/lib.rs`.
- [x] A1.2 Unit tests in `output_contract.rs`: `numeric` reads `80`,
  `-1.5`, `"80"`, `"1e3"` and refuses `" 80"`, `"+80"`, `"inf"`, `"NaN"`,
  `"0x10"`, `""`, `true`, `null`; `json_in` reads an array, an object, a
  string holding either, and refuses a string holding a number; `check`
  per type (string, number, bool, array, object, enum) with one passing
  and one failing value each; `required` absent and `null`; an optional
  absent field; output not a JSON object with and without a required
  entry; a plain-only list never yields a mismatch, even for output
  `"not json"`; `reason` texts exactly as in D1, declaration order of
  several mismatches, and no reason holding any part of the offending
  value (a 200-character value).
- [x] A1.3 Implement `output_contract` (D1 rules 1 to 6).
- [x] A1.4 `FieldDecl`, `TypedFieldDecl`, `FieldType`, `field_names` in
  `schema.rs`; `NodeFiles.fields: Vec<FieldDecl>`,
  `NodeFiles::field_names`; `ListSpec.item_outputs: Vec<FieldDecl>`,
  `ListSpec::item_output_names`. Tests in `schema.rs`: a plain list
  round-trips to the same YAML and the same `serde_json::to_string` of the
  node as a `Vec<String>` gave (write the expected JSON literally); the
  typed form round-trips; an unknown key in a typed entry is a parse
  error.
- [x] A1.5 Fix every `apb-core` site the compiler lists (D1 "Call sites"
  1 to 3).
- [x] A1.6 New `crates/apb-core/src/validate/output_types.rs`: one failing
  test per V167 rule and per V168 rule for the readers that exist now
  (`equals` on `bool`, `number`, `enum`, `array` and `object` fields,
  `items.from`, `no_new_items`) first, then the rules; call it from
  `validate()` with `// V167, V168`.
- [x] A1.7 `cargo test -p apb-core` passes.

## A2. Typed fields: engine (D1)

- [x] A2.1 Fix every `apb-engine` site the compiler lists (D1 "Call
  sites" 4 to 8); `apb-engine` builds.
- [x] A2.2 Unit tests in `scheduler/status_file.rs`: the note of a
  plain-only list is byte-for-byte today's (copy today's expected text
  into the test before changing the function); the typed note of D1.
  Then change `status_file_note` to take `&[FieldDecl]`.
- [x] A2.3 E2e in a new `crates/apb-engine/tests/suite/output_contract_test.rs`
  (declare it in the suite's `main.rs`; reuse the stand-in agent and
  status-file fixtures of `named_outputs_test.rs` and
  `success_check_test.rs`):
  A2.3.1 `blocking: "two"` on attempt 1, `blocking: 0` on attempt 2 with
  `max_retries: 1`: attempt 1 `failed` with `rejected_for` starting
  `output_contract:`, attempt 2's prompt holds the reason, the node
  succeeds;
  A2.3.2 the same reason on both attempts: the node fails and its output
  starts `output_contract:`;
  A2.3.3 the agent dies after writing a success verdict with a bad field
  (the recovery path): the attempt is rejected the same way;
  A2.3.4 a script node printing `{"count": "many"}` against
  `{ name: count, type: number, required: true }` fails with that reason;
  A2.3.5 a scenario run whose script node is scripted with that stdout
  fails the same way (D1 wrong implementation 10);
  A2.3.6 a list item step checked against `item_outputs`, not against the
  node's `outputs.fields`;
  A2.3.7 an optional typed field left out: success and
  `output_fields_missing`;
  A2.3.8 a configured connector secret inside a 200-character value of a
  `number` field: no 8-character run of the secret in the journal or in
  attempt 2's prompt (D1 wrong implementation 9).
- [x] A2.4 Implement: the `contract` parameter of
  `success_check_rejection` and both call sites; `script_contract` at
  both succeeded returns of the script arm.
- [x] A2.5 `journal.ts` `rejected:` note and the `nodeio.ts` and
  `nodefields.ts` changes of D1 "Surfaces", vitests first
  (`journal.test.ts`, `nodeio.test.ts`). Plain CSS only; Tailwind is
  banned.
- [x] A2.6 `cargo test -p apb-engine --lib status_file`,
  `cargo test -p apb-engine --test main output_contract_test`,
  `named_outputs_test` and `success_check_test` pass.

## A3. Numeric thresholds (D2, spec `numeric-edges`)

From A3.2 until A3.3 `apb-engine` does not build.

- [x] A3.1 Unit tests in `schema.rs`: `field_compare` for each comparator,
  for none and for two; `output_field_matches` for `lt`, `lte`, `gt`,
  `gte` at, below and above the threshold, with a number and a numeric
  string, `9` against `lt: 10`, and non-matches for `true`, `null`,
  `[1]`, `{}`, `"large"`, an absent field and output `"x"`; `equals`
  exactly as today; a YAML round trip of `lt: 80`; `Threshold` equal and
  hashed by bits.
- [x] A3.2 Implement the variant change, `Threshold`, `FieldCompare`,
  `field_compare`, `output_field_matches`.
- [x] A3.3 Every site of D2 "Every site of the variant" 1 to 9; V169 in
  `validate/edges.rs` with its tests (none, two, `.inf`) first; the V168
  threshold case (a threshold on a field typed other than `number`) with
  its failing test first.
- [x] A3.4 E2e in `crates/apb-engine/tests/suite/output_field_test.rs`: a
  script node printing `{"coverage": 72.5}` routes through an `lt: 80`
  edge, `{"coverage": "91"}` through the `gte: 80` edge, and
  `{"coverage": "n/a"}` through the fallback; `diagnostics.jsonl` records
  `observed` `{"value": "72.5", "number": 72.5}` for the threshold edge.
- [x] A3.5 `report_html/graph.rs` `edge_label` with a unit test per
  comparator.
- [x] A3.6 Web: vitests first in `graph.test.ts` (labels and titles of the
  four thresholds) and a load, save, reload round trip that keeps `lt`
  (in the style of `loopexit.test.ts`); then `graph.ts`.
- [x] A3.7 `cargo test -p apb-core` and `cargo test -p apb-engine --test
  main output_field_test` pass.

## A4. Bundled content (D6)

- [x] A4.1 `apb-feature-delivery`: the typed reviewers and collector, the
  `collect` scenario strings as real JSON arrays, version `1.2.4`.
- [x] A4.2 New scenario `reviewer-malformed-output-retries.yaml`.
- [x] A4.3 `apb-code-review`: the typed reviewers, version `1.1.3`, new
  scenario `reviewer-prose-findings-fails.yaml` (confirm the terminal
  output form, D6 item 3).
- [x] A4.4 `cargo test -p apb-cli --test main bundled_scenarios_test` and
  `cargo test -p apb-core --test main bundled_content_lint_test` pass.

## A5. Docs and gate of part A

- [x] A5.1 `docs/HOWTO-authoring.md`: "Named outputs" (D1) and the
  `output_field` bullet (D2), with the V167 to V169 rows in the format of
  the V160 to V162 table (line 211).
- [x] A5.2 Style over every changed doc and bundled file: no U+2013 or
  U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing) and no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--` and `![` do not count).
- [x] A5.3 The part gate (Environment), all green.

## B1. Stall exit: core and journal (D5, spec `stall-exit`)

- [x] B1.1 New `crates/apb-core/src/rejection.rs` with unit tests first:
  each normalization step alone; the spec scenario (two counts, two
  timestamps, one fingerprint); a run dir and a run id replaced; a word
  of 7 hex letters without a digit (`deadbee`) kept; an 8-character hex id
  with a digit replaced; the fingerprint is 16 lowercase hex characters
  and stable across calls.
- [x] B1.2 Decode tests in `event.rs`: an old `attempt_finished` line has
  `rejection_fingerprint: None` and `stalled: false`; a new one with both
  round-trips and writes neither key when unset. Add both fields; fix
  every constructor with `rejection_fingerprint: None, stalled: false`.
- [x] B1.3 Schema: `stall_after` on `Node` and `Defaults`,
  `effective_stall_after`, `StatusEq::Stalled`; V174 and V175 in
  `validate/retry.rs` with one failing test per rule first; the V09/V39
  coverage fix with a test that `success` plus `stalled` without a
  fallback still reports V09 on a condition node.

## B2. Stall exit: engine, notify, web

- [x] B2.1 `scheduler/stall.rs` with unit tests of `streak`: trailing
  equal fingerprints counted; a transient and an interrupted attempt
  skipped; another fingerprint or a success stops it; attempts before the
  effect scope floor ignored; an item key counts only its item. `assess`
  reads `read_all` and returns its error.
- [x] B2.2 The four rejection sites per D5, through one helper in
  `stall.rs`; `RunState.stalled` and its fold with state tests (a stalled
  execution, then a successful one clears it); `parallel::status_matches`
  over the state; `scheduler.rs` `dead_end_reason`.
- [x] B2.3 E2e in a new `crates/apb-engine/tests/suite/stall_test.rs`
  (declare it in `main.rs`; reuse the check fixtures of
  `success_check_test.rs`):
  B2.3.1 `stall_after: 3`, `max_retries: 5`, a check that always fails the
  same way: exactly 3 attempts, the last `stalled: true`, the node
  `failed` with output starting `stalled:`, a `stalled` edge taken before
  a `failure` edge;
  B2.3.2 without `stall_after`: 6 attempts, every one with a fingerprint;
  B2.3.3 a check whose message alternates between two texts: no stall;
  B2.3.4 a check whose message carries a changing timestamp and counter:
  stalls;
  B2.3.5 with `retry_delay` (the fixtures of `deferred_retry_test.rs`): the
  stall happens at the same attempt;
  B2.3.6 a profile with a fallback step: the stall ends the node without
  `fallback_triggered`;
  B2.3.7 a transient failure between two identical rejections does not
  break the streak.
- [x] B2.4 Notify: `Transition::Stalled` (core), `open_transitions`, the
  notice and `command_for`; tests in
  `crates/apb-engine/tests/suite/notify_transitions_test.rs`: a stalled
  attempt opens `stalled.<seq>`, the run end does not close it, it is not
  among the default transitions; a notice test of the subject and lines.
- [x] B2.5 `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`
  (`consts.gen.ts` gains `stalled` in `NOTIFY_TRANSITIONS`), then the same
  without the variable to confirm no drift.
- [x] B2.6 Web, vitests first: `graph.test.ts` the `stalled` tones and
  title, a `stalled` edge kept on save; `journal.test.ts` the `stalled`
  note; then `graph.ts` and `journal.ts`.
- [x] B2.7 `cargo test -p apb-core` and `cargo test -p apb-engine --test
  main stall_test notify_transitions_test` pass.

## B3. Docs and gate of part B

- [x] B3.1 `docs/HOWTO-authoring.md`: "Stopping a retry loop that repeats
  itself (stall_after)", the `node_status` `stalled` value, "The retry
  briefing" fingerprint field, the V174 and V175 rows; `docs/NOTIFY.md`
  the `stalled` row and its opt-in.
- [x] B3.2 The style check of A5.2 over the files of this part.
- [x] B3.3 The part gate, all green.

## C1. A check that must fail first (D4, spec `proven-checks`)

- [x] C1.1 Schema tests: `{ script, scope }` parses and serializes as
  today; `{ script, prove_fails_first: true }` and with `on_green_before:
  skip` parse; `{ marker, prove_fails_first: true }` is a parse error.
  Then the `Scoped` change, `OnGreenBefore`, `prove()`, and the sites of
  D4 "Schema".
- [x] C1.2 V173 in `validate/nodes.rs` with one failing test per rule
  first (the `{ script }` map, `on_green_before` alone, `cache` with a
  proof, and `skip` with `outputs.fields`, with `results` and with an
  edge condition reading the node); V163 tests still pass.
- [x] C1.3 Decode test of `check_proved` in `event.rs` (with and without
  `changed_tree`); a test that an older reader skips it (follow the
  existing skip-safety tests); then the variant.
- [x] C1.4 `scheduler/prove.rs` with unit tests of `state`: no attempt in
  scope is `Due`; an attempt since the floor is `NotDue`; a `check_proved`
  with no `node_finished` after it is `Open` with its result; a loop visit
  after a success is `Due` again; a green `fail` proof followed by its
  `node_finished`, then a supervisor retry, is `Due`.
- [x] C1.5 `cache::prepare` returns no context for a proving node, with a
  unit test.
- [x] C1.6 E2e in a new `crates/apb-engine/tests/suite/prove_test.rs`
  (declare it in `main.rs`), with a check script that reads a marker file
  the stand-in agent creates:
  C1.6.1 red first: `check_proved { failed_first: true }`, then the
  attempt, then success;
  C1.6.2 green first, default: the node fails with output starting
  `check_green_before:`, no `attempt_started`, and
  `OutcomeSummary.needs_look` lists `1 check green before work`;
  C1.6.3 green first, `skip`: the node succeeds with output starting
  `already done:`, no `attempt_started`;
  C1.6.4 a rejected attempt 1 and its retry: exactly one `check_proved`;
  C1.6.5 the journal cut after a red `check_proved`, then a resume: one
  `check_proved`, the attempt runs; the same after a green `fail` proof:
  one `check_proved`, the node fails;
  C1.6.6 a check that modifies a tracked file and exits 1: the node fails
  with `check_changed_tree:`, no `attempt_started`;
  C1.6.7 a scenario run of a proving node journals no `check_proved`;
  C1.6.8 with validation bypassed, a proving node with `cache: auto` run
  twice as a batch member beside another script journals no
  `node_cache_hit`.
- [x] C1.7 `NeedsLook` reason and count, `line()`; unit tests in
  `run_outcome/summary.rs`; `APB_WRITE_TS=1 cargo test -p apb-server --lib
  ts_contract` then without the variable; `runverdict.ts` and the
  `check_proved` line of `journal.ts`, vitests first.
- [x] C1.8 `cargo test -p apb-core`, `cargo test -p apb-engine --lib
  run_outcome prove` and `cargo test -p apb-engine --test main
  prove_test` pass.

## C2. Docs and gate of part C

- [x] C2.1 `docs/HOWTO-authoring.md` "A check that must fail first", with
  the observation rule and what it cannot observe, the V173 row.
- [x] C2.2 The style check of A5.2 over the files of this part.
- [x] C2.3 The part gate, all green.

## D1. Write scope: core (D3, spec `write-scope`)

- [x] D1.1 New `crates/apb-core/src/write_scope.rs` with unit tests first:
  `entry_problem` (empty, `/a`, `C:/a`, `a/../b`, `./a`, `{{x}}`, `[`);
  `parse_rendered` (a JSON array, a newline list with blank lines, an
  empty text, an array with a number, 201 entries); `template_ref`
  (exactly one reference, text around it, two references).
- [x] D1.2 Schema: `WriteScope`, `writes`, `max_changed_files` on `Node`;
  round-trip tests of both forms.
- [x] D1.3 `validate/write_scope.rs`: one failing test per V170 (`cache`
  included), V171 and V172 rule first, then the rules; the V168 `writes`
  template case with its failing test first; `writes_texts` chained into
  V38 and V46 with a test each; `GuardedFeature::WriteScope` with its test
  in `guarded.rs`.

## D2. Write scope: engine

- [x] D2.1 `read_only.rs`: `Before::compare` split out, `evidence_stem`
  and `next_ordinal` `pub(super)`. All `read_only_test` and
  `read_only_batch_test` tests still pass unchanged.
- [x] D2.2 `scheduler/write_scope.rs`: `resolve`, `note`, `finish`, with
  unit tests of the classification (own, outside, git state kinds
  ignored), a partial before state and a partial after state failing, the
  message forms and the evidence file (0600, no content).
- [x] D2.3 `batch_class.rs` `is_scoped` and `batch_admits` with unit tests
  beside the existing ones (a scoped candidate refused beside a writer, a
  writer refused beside a scoped member, a scoped node beside an enforced
  read-only one admitted); `cache::prepare` returns no context for a
  scoped node, with a unit test.
- [x] D2.4 E2e in a new `crates/apb-engine/tests/suite/write_scope_test.rs`
  (declare it in `main.rs`; git work tree fixtures of `read_only_test.rs`):
  D2.4.1 a script with `writes: ["src/**"]` writing `src/a.rs` succeeds;
  writing `docs/a.md` fails with `write_scope:`, the file keeps the
  change;
  D2.4.2 an `agent_task` stand-in with `max_retries: 2` that writes outside
  its scope: one attempt only, `failure_kind` `write_scope`, no
  `retry_started`;
  D2.4.3 `max_changed_files: 1` and two changed files: fails; a commit of
  one in-scope file passes;
  D2.4.4 the template form from an earlier node's array field; an empty
  render refuses the node before any attempt; a `../x` entry refuses it;
  D2.4.5 two scoped scripts with disjoint globs and `max_parallel: 2`
  never overlap in time; a scoped script and an unscoped one never
  overlap; two unscoped scripts still do (today's behaviour);
  D2.4.6 with `APB_READ_ONLY_MAX_ENTRIES` lowered (debug build): a tree
  over the limit before the attempt fails the node with the `too large to
  compare whole` reason, and so does a tree the attempt grows past it;
  D2.4.7 a write to a git-ignored path outside the scope passes;
  D2.4.8 with validation bypassed, a scoped node with `cache: auto` run
  twice sequentially journals no `node_cache_hit`;
  D2.4.9 the agent's prompt holds the write scope note on attempt 1.
- [x] D2.5 Wire the agent arm and the script arm per D3 "Where it runs".
- [x] D2.6 `cargo test -p apb-core`, `cargo test -p apb-engine --lib
  batch_class read_only write_scope` and `cargo test -p apb-engine --test
  main write_scope_test read_only_test read_only_batch_test` pass.

## D3. Docs and gate of part D

- [x] D3.1 `docs/HOWTO-authoring.md` "Write scope (writes,
  max_changed_files)" with the observed boundary, the partial rule, the
  batch rule, the no-cache rule, the downgrade limit and the V170 to V172
  rows; `docs/GUARDRAILS.md` (`writes` row); `docs/ISOLATION.md` (one
  sentence).
- [x] D3.2 The style check of A5.2 over the files of this part.
- [x] D3.3 The part gate, all green; `apb validate` reports no new warning
  on any bundled playbook or example (V171 and V172 must not fire there).

## E. Whole suite, records and report

- [x] E.1 `CLAUDE.md` and `AGENTS.md`, identical shared text in both: in
  the `apb-core` list `output_contract.rs` (the typed field contract and
  the numeric reading), `write_scope.rs` (scope entries and rendering),
  `rejection.rs` (the stall fingerprint), the validator modules
  `output_types` (V167, V168) and `write_scope` (V170 to V172), V169 under
  `edges`, V173 under `nodes`, V174 and V175 under `retry`; in the
  `apb-engine` list `scheduler/write_scope`, `scheduler/prove`,
  `scheduler/stall`, and the `check_proved` event.
- [x] E.2 Write `docs/superpowers/specs/2026-11-07-node-contracts-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] E.3 The whole workspace, each with its numbers in the report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo nextest run --workspace` (or `cargo test --workspace` when
  nextest is missing); `cargo test --workspace --doc`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
- [x] E.4 Two items ride in this PR and need no action from you: the
  `openspec/` directory is tracked in the repository and is committed with
  the change (the orchestrator runs `openspec archive node-contracts
  --yes` before the push, so `openspec/specs/` describes main plus this
  change), and a new `.coderabbit.yaml` at the repository root, whose
  content the orchestrator provides. Do not edit or create either.
- [x] E.5 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started.
- [x] E.6 Final report: per part what was built, decisions you took where
  `design.md` left room, each gate with numbers, the answer to D6 item 3,
  the user-visible changes for the release notes (with the downgrade
  limit of `writes`), the changed file count, and anything left open.
