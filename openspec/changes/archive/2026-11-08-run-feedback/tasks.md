# Tasks: run feedback that outlives the run

Work in order, one section at a time. Before a section, read its part of
`design.md` and the spec delta it names. Tick each box (`- [x]`) as soon as
it is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

For every behaviour: write the failing test first, run it and see it fail,
then write the code. For a test that names a field or a module that does
not exist yet, a compile error is the expected first failure.

This change is implemented on top of PR 1 `node-contracts` (merged) and
PR 2 `recurring-routines` (the branch this tree starts from, its one
commit on top of main). Line numbers in `design.md` are from before them:
find every site by its name.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-run-feedback-target TMPDIR=$HOME/.cache/apb-run-feedback-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-run-feedback-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-run-feedback-target`
and `TMPDIR=/tmp/apb-run-feedback-tmp` instead, and before each build check
that `df -h /tmp` shows more than 4G free; if not, stop and report it.
`/tmp` is a RAM disk shared with other sessions. Never use `/dev/shm`, and
never put a target dir, cache or temp data inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR`. Never stop a process you did not start. Do not commit,
push or open a pull request: the git directory is read-only for you. Do not
start subagents.

The change has three independent features, built in this order: Part A
(D1, scenario capture with the scripted timeout), Part B (D2, the
corrections ledger with the signed-store extraction), Part C (D3, the
follow-up check). Each part is built, documented and passes its own gate
before the next part starts; the workspace suite, the public record and
the report come once at the end.

During a section run only the tests of the crates it touches
(`cargo test -p <crate> --lib <module>`,
`cargo test -p <crate> --test main <module>`), one filter per invocation:
`cargo test` takes one test name filter, so two modules are two commands.

## 0. Preconditions (D0)

- [x] 0.1 Confirm PR 1 and PR 2 are in the tree: `git log --oneline -3`
  shows the recurring-routines commit, and
  `ls openspec/changes/archive` lists `2026-11-07-node-contracts` and
  `2026-11-07-recurring-routines`. Run
  `rg -n rejection_fingerprint crates/apb-engine/src/event.rs` and note the
  field's exact name and type for D2 (or that it is absent).
- [x] 0.2 Confirm V183 to V190 are free:
  `git grep -ohE '"V1[89][0-9]"' crates | sort -u`. If any is taken, use the
  next free codes, update `design.md` and the specs, and say so in the
  final report.

# Part A: scenario capture (D1, spec `scenario-capture`)

## 1. The scripted timeout (D1 "The scripted timeout")

- [x] 1.1 Tests first: in `crates/apb-core/src/scenario/format.rs` tests,
  `- { timed_out: true }` and `{ timed_out: true, output: x }` parse, a
  child answer with `status: timed_out` still fails to parse, and the
  digest of the worked example (without the field) equals a constant you
  take with the code before the change; in
  `crates/apb-core/src/scenario/validate.rs` tests, V146 for `timed_out`
  with `status`, `failure_kind`, `outputs`, `check`, `read_only` or
  `question`, and none for it as a question's `then`; in
  `crates/apb-engine/src/scenario/adapter.rs` tests, the answer is an
  `AgentFailure` of class `ErrorClass::Timeout`.
- [x] 1.2 The field, the V146 rule and the module docs, the adapter branch,
  and the `counted_attempts` decision (read the timeout continuation in
  `scheduler/node.rs` first; note the answer for the final report).
- [x] 1.3 E2e in `crates/apb-cli/tests/suite/scenario_run_test.rs`: a quick
  scenario answering a node (`max_retries: 0`, `timeout_seconds: 600`) with
  `{ timed_out: true }` passes with `expect.status: { <node>: timed_out }`
  and finishes within 30 s; with `max_retries: 1` and answers
  `[{ timed_out: true }, { status: succeeded }]` the node succeeds with
  `visits: 1`.

## 2. The capture (D1 "Who builds it" to "Saving")

- [x] 2.1 Unit tests for `apb_core::fsutil::write_new_no_clobber` (writes a
  new file; `AlreadyExists` for an existing one, whose bytes are
  unchanged; two threads on one path: exactly one succeeds) and for
  `apb_core::scenario::write_new` (`scenario_exists`, an invalid name, a
  link below the playbook directory). Implement both.
- [x] 2.2 E2e tests first in a new `crates/apb-engine/tests/suite/scenario_capture_test.rs`
  (declared in `tests/main.rs`; build real runs with the stand-in agents of
  the existing engine suites, then capture, and assert the captured
  `Scenario` value): a success check that rejected attempt 1 gives a
  `check: reject` attempt then a succeeded one in visit 1; declared output
  fields land in `outputs`; a transient failure then success; a timed-out
  attempt (stand-in agent sleeping past a 1 s node timeout) is
  `{ timed_out: true }`; a review `reject` with a note; a judge node and a
  judge edge (by address); a question answered by a human; a script
  failure; two attempts in one execution are one visit; `--node` narrows
  `expect.status` and `expect.visits`; a secret value and a
  credential-shaped token are scrubbed (`redacted >= 1`); an output holding
  a YAML injection string round-trips as a string; and each refusal of the
  "Refusals" table and of the answers list, among them `list_node`,
  `aborted` (a stopped run), `cache_hit`, `fatal_rejection`,
  `unknown_events` (an unknown event type before the last checkpoint) and
  `journal_unreadable` (a torn last line).
- [x] 2.3 Implement `scenario/capture.rs` per D1 with the strict
  `read_all`. Confirm the judge node `use_site` string and the signalled
  wait's output (D1 points 4 and 6); note both for the final report.

## 3. Capture surfaces (D1 "The tool layer")

- [x] 3.1 Tool layer `crates/apb-mcp/src/tools/scenario_capture.rs` with
  tests: preview writes nothing; save with the preview's digest writes;
  `stale_preview`; `save_by_person` for `SaveChannel::Agent`; refused from a
  run step; a global playbook's scenario lands in the global store.
- [x] 3.2 CLI `apb scenario capture` (`crates/apb-cli/src/scenario_capture.rs`,
  the `Scenario` subcommand) with a test in a new
  `crates/apb-cli/tests/suite/scenario_capture_cli_test.rs`: preview text
  and `--json`; `--save --yes` writes; `--save` without a terminal and
  without `--yes` is exit 2. Replay tests (the scenario plays in its own
  child process, so they live here; reuse `scenario_fixture.rs` and
  `scenario_run_test.rs` helpers): for a rejected-attempt run, a
  declared-fields run, a review run and a timed-out run built here,
  `apb eval <id> --level quick --scenario <name>` on the captured file
  exits 0 (same outcome, statuses and visits, and the later template
  renders the same field value).
- [x] 3.3 MCP `run_scenario_capture` (preview only, `ReadOnlyUse::Full`) in
  `crates/apb-mcp/src/server/scenario.rs`; HTTP `POST
  /api/runs/{id}/scenario-capture` in `routes/scenario.rs`; tests: MCP has
  no save argument and writes nothing; HTTP 409 from a run step, 422 for an
  unknown field, a save writes.
- [x] 3.4 Register `CapturePreview` and `CaptureArgs` in
  `crates/apb-server/src/ts_contract.rs`, then
  `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract` and the same
  without the variable (no drift).
- [x] 3.5 Dashboard `ScenarioCaptureBox.svelte` on a finished run page, with
  `ScenarioCaptureBox.test.ts` (name grammar, preview shown, save posts the
  digest, a refusal lists the gaps). Plain CSS only; Tailwind is banned.

## 4. Part A docs and gate

- [x] 4.1 `docs/EVALS.md`: the `timed_out` attempt field, and "Capturing a
  run as a scenario" (D1 docs); `run_scenario_capture` in `docs/MCP.md`.
- [x] 4.2 `CLAUDE.md` and `AGENTS.md`, the same shared text in both: the
  scripted timeout in the scenario format line, `scenario/capture.rs` in the
  `apb-engine` list, `scenario_capture` in the `apb-mcp` list,
  `scenario_capture.rs` in the `apb-cli` list.
- [x] 4.3 Gate A, each with its numbers in the report: `cargo fmt --all --
  --check`; `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo test -p apb-core`; `cargo test -p apb-engine`;
  `cargo test -p apb-cli --test main scenario_capture_cli_test`;
  `cargo test -p apb-cli --test main scenario_run_test`;
  `cargo test -p apb-cli --test main bundled_scenarios_test`;
  `cargo test -p apb-server --lib ts_contract`;
  `cd web && bun run check && bun run test`;
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix everything before Part B.

# Part B: the corrections ledger (D2, spec `corrections-ledger`)

## 5. One signed store (D2 "One signed store")

- [x] 5.1 Fixture test first, in `crates/apb-core/src/known_problems/store.rs`
  tests: write a problem file with the current code, keep its bytes as a
  test constant, and assert `read` verifies it (`signed: true`). This test
  must keep passing after the move.
- [x] 5.2 New `crates/apb-core/src/signed_store.rs` with `SignedDoc`,
  `Loaded<T>`, `StoreError`, `dir`, `read`, `update`, moved from
  `known_problems/store.rs`. Unit tests over a small test doc: unsigned
  file reads empty and counts `ENTRIES_KEY`; a file signed under another
  domain reads unsigned; a misplaced file reads unsigned; a newer schema
  refuses an update; an unsafe id is `InvalidId`.
- [x] 5.3 `known_problems/store.rs` on top of it, the public API kept.
  `cargo test -p apb-core known_problems` and
  `cargo test -p apb-engine --test main known_problems_test` pass unchanged.
- [x] 5.4 Extract `known_problems::keep_wording`; known-problems tests pass.

## 6. Corrections: core (D2 "Types", "The key", "Folding", "Draft", "After the fix")

- [x] 6.1 Unit tests in `crates/apb-core/src/corrections/normalize.rs`: the
  two parser sentences share a key; `fix line 12` and `fix line 40` share
  one; a one-word text keys on its whole text; kind and node are part of the
  key; non-ASCII words are tokens; `fingerprint_key` accepts a hex hash and
  refuses anything else. Implement.
- [x] 6.2 Unit tests in `crates/apb-core/src/corrections/mod.rs`: observing
  a run twice changes nothing; after `OBSERVED_KEPT + 1` later runs the
  first run is below the floor and still changes nothing; a run without
  sightings counts in `runs_by_version`; one key twice in one run counts
  once; `open` becomes `proposed` at `MIN_RUNS`; `dismissed` stays;
  wordings keep the leading one; `EVIDENCE_KEPT`; order by runs.
- [x] 6.3 Implement the types, `observe`, `settle_status`,
  `impl SignedDoc for CorrectionFile`; `"corrections/"` in
  `LOCAL_ENTRIES` and its test list.
- [x] 6.4 Unit tests and implementation of `draft` (the three line forms;
  none without node, prompt or text, none for a `node_note`; `LINE_MAX`;
  hidden chars replaced) and of `mark_fixed` and the reopen rule (snapshot
  taken at marking; base 2 of 2 with fix 1 of 3 stays `fixed`, 3 of 3 is
  `reopened`; under 3 fix runs no judgment; `unknown_version`;
  `not_proposed`).

## 7. Corrections: collecting (D2 "What a run contributes", "The one run-end hook")

- [x] 7.1 Unit tests in `crates/apb-engine/src/corrections/extract.rs` over
  hand-built journals: a person's `reject` with a note and its delivering
  node from `edge_traversed`; a gate fed by two branches gives `node: None`;
  a gate fed by one agent task names it although another branch finished
  later; `approve` with a note, a publish consent and `decided_by: model`
  add nothing; a `context_append` with a node (`via_supervisor`), one
  without a node adds nothing; a human answer with its question; a
  `timeout` answer adds nothing; an attempt with `rejected_for`; with
  `rejection_fingerprint` the key is `f:` (skip if 0.1 found no field); an
  applied `retry_advice` adds nothing; two notes differing only in a secret
  value share a key; a note that is only a credential-shaped token is
  dropped, and with a fingerprint kept with `text: None`.
- [x] 7.2 Implement `extract::sightings` and `corrections::observe` (strict
  `read_all`, production runs only, no file for a playbook without
  corrections).
- [x] 7.3 `crates/apb-engine/src/run_end.rs` with `record`; replace the four
  `known_problems::observe_ended` calls.
- [x] 7.4 E2e in a new `crates/apb-engine/tests/suite/corrections_test.rs`
  (declared in `tests/main.rs`): two runs with the same review note give
  one `proposed` entry with `runs: 2`; a stopped run is observed too; a
  scenario run adds nothing; a journal with an unknown event after its last
  checkpoint adds nothing; the file bytes of a token-only note hold neither
  the token nor its `sha256`; a later run's rendered prompt holds no ledger
  text; after marking fixed, the playbook store has no new version and no
  candidate pointer.

## 8. Corrections: surfaces (D2 "Surfaces")

- [x] 8.1 Tool layer `crates/apb-mcp/src/tools/corrections.rs` with tests:
  every entry point refused from a run step; dismiss, reopen and
  `MarkFixed` keep `Decided`.
- [x] 8.2 CLI `crates/apb-cli/src/corrections.rs` and the `Corrections`
  subcommand; test in a new `crates/apb-cli/tests/suite/corrections_cli_test.rs`:
  list text, `--json`, `show` prints the draft, `fixed --version`,
  `dismiss`, `reopen`.
- [x] 8.3 MCP `playbook_corrections` and `correction_decide` in
  `crates/apb-mcp/src/server/corrections.rs`, classified in
  `read_only.rs`; a test in `crates/apb-mcp/src/server/tests/` that MCP has
  no `fixed` decision and refuses a run step.
- [x] 8.4 HTTP `crates/apb-server/src/routes/corrections.rs` and the router
  lines; route tests for 200, 409 from a run step, 422 for an unknown action
  or field.
- [x] 8.5 Register the D2 types in `ts_contract.rs` (D2 "TypeScript"), then
  regenerate and check for no drift.
- [x] 8.6 Dashboard `CorrectionsCard.svelte` on the playbook page with
  `corrections.test.ts` (list, copy the draft, mark fixed with a version and
  a confirm box, dismiss, reopen, the two rates). Plain CSS only.

## 9. Part B docs and gate

- [x] 9.1 New `docs/CORRECTIONS.md` (D2 docs), a pointer in
  `docs/KNOWN-PROBLEMS.md`, the two tools in `docs/MCP.md`.
- [x] 9.2 `CLAUDE.md` and `AGENTS.md`, same shared text in both:
  `signed_store.rs` and `corrections/` (`mod`, `normalize`, `store`) in the
  `apb-core` list; `run_end.rs` and `corrections/` (`extract`) in the
  `apb-engine` list; `corrections` in the `apb-mcp` list; `corrections.rs`
  in the `apb-cli` list; `corrections` in the `apb-server` list; a
  "Concepts that span files" paragraph on the ledger (who may read it, the
  draft a person applies, marking a fix).
- [x] 9.3 Gate B, as 4.3 with `corrections_cli_test`,
  `corrections_test` and `known_problems_test` in place of the Part A test
  binaries' filters. Fix everything before Part C.

# Part C: the follow-up check (D3, spec `follow-up-check`)

## 10. Follow-up: core (D3 "Schema", "Rule check", "Validator")

- [x] 10.1 Unit tests in `crates/apb-core/src/follow_up.rs`: the YAML of D3
  round-trips; an unknown key is refused; `after_seconds` and
  `grace_seconds` (default 24 h). Implement it and `Playbook.follow_up`.
- [x] 10.2 Unit tests for `baseline::rule::check`, then implement it.
- [x] 10.3 Make the four baseline rule helpers take a label (messages of
  existing baseline tests unchanged).
- [x] 10.4 New `crates/apb-core/src/validate/follow_up.rs`: one failing test
  per rule V183, V184, V185, V186 and one that a V63 problem of the check
  names `follow-up check`; then the rules and the call from `validate()`.
- [x] 10.5 `FOLLOW_UP_SITE` in `connector/sites.rs` with tests
  (`connector_sites`, `site_label`, `is_engine_site`).

## 11. Follow-up: engine (D3 "The event" to "Arming")

- [x] 11.1 Tests first in `crates/apb-engine/src/event.rs` tests: a
  `follow_up_observed` line round-trips; `is_checkpoint` is true for it;
  `run_finished`, an unknown type, then `follow_up_observed` reads with
  `read_all`; an unknown type after `follow_up_observed` is refused. In
  `crates/apb-engine/tests/suite/journal_compat_test.rs`: a journal ending
  in `run_finished` then an unknown type (what an older apb sees) is
  refused by `read_all`, read by `read_journal` with the event counted, and
  allowed by the stop path.
- [x] 11.2 Add `EventPayload::FollowUpObserved` and `FollowUpResult`; every
  match the compiler lists.
- [x] 11.3 `ReadCtx.site`; the connector read uses it; the drive and the list
  check pass `BASELINE_SITE`. Existing baseline tests pass.
- [x] 11.4 Unit tests for `state` and `arm_plan(playbook, events,
  production) -> Option<WakerFor>`: no block; a failed end; a succeeded end
  gives `Pending` due at the `run_finished` ts plus `after`; a
  `run_resumed` after it gives `None`; an observed one; a scenario or eval
  origin gives `None`.
- [x] 11.5 `WakerFor::FollowUp` in `waker.rs` (lease name and args tests),
  the CLI flag `--follow-up` and the `pause_waker_cmd` arm.
- [x] 11.6 E2e in a new `crates/apb-engine/tests/suite/follow_up_test.rs`
  (declared in `tests/main.rs`), with `after: 1m` and the clock moved by
  passing `now_ms` to `observe_due` (no sleeping test): a matching script
  journals `observed` and `met`; a non matching one `not_met`; a failing
  one `unknown`; past grace `missed` with the script not run (a marker file
  it would write is absent); two calls journal one event; a held lease
  journals nothing; a connector stand-in source (reuse the connector
  fixtures of `baseline_test.rs`) reads under `@follow_up`; a removed
  worktree reads `unknown`; a run directory without an origin stamp is
  never read (marker absent); a configured secret in an observed value is
  absent from the bytes of `events.jsonl` while the verdict is computed
  from it; unresolved secrets give `unknown` with no observations.
- [x] 11.7 Implement `follow_up/` (`state`, `arm_plan`, `observe_due` with
  the trust check and the scrub before append, `read.rs`, `waker.rs` that
  drops its lease before `observe_due`) per D3.
- [x] 11.8 Arm in `drive` after a succeeded run. CLI test in
  `crates/apb-cli/tests/suite/pause_waker_cli_test.rs` (reuse its helpers
  and `common.rs` line 222 to kill the wakers it started): `apb run` of a
  stand-in playbook with `follow_up.after: 1m` that succeeds leaves
  `runs/<id>/wakers/follow-up-<seq>` held; with `APB_WAKER_POLL_MS` short
  and `APB_FOLLOW_UP_AFTER_MS` short (D3, read through
  `crate::env_duration::test_override` in a debug build only), the waker
  journals `follow_up_observed` and exits.
- [x] 11.9 `apb resume --all`: test in `follow_up_test.rs` that a due
  follow-up is read, a late one `missed`, one ahead rearmed, an imported
  run untouched; implement `follow_up::sweep_finished` and its call.

## 12. Follow-up: surfaces (D3 "Surfaces")

- [x] 12.1 `run_outcome/follow_up.rs` with unit tests (pending, observed,
  unknown, missed, not scheduled); `RunReadout.follow_up`;
  `OutcomeSummary.follow_up`, the `Follow-up` section in `sections()` and
  `NeedsLookReason::FollowUp` for `not_met` only.
- [x] 12.2 MCP `run_status` `follow_up` block (present for a pending
  follow-up, absent without one).
- [x] 12.3 `run_stats/follow_up.rs` with unit tests (met rate over observed
  only; unknown and missed apart; pending counted), the per-version field,
  the text line and `compare.follow_up_met_rate`.
- [x] 12.4 `Transition::FollowUp`: core tests (`ALL` has it, `DEFAULT` does
  not); `open_transitions` test (opened at the event, closed by a resume);
  `notice.rs` arm test; an e2e next to `notify_send_test.rs` that a
  subscription naming `follow_up` sends one notice through a connector
  stand-in after `observe_due`, and one without it sends none.
- [x] 12.5 Register the D3 types in `ts_contract.rs` (D3 "TypeScript"), then
  regenerate `api.gen.ts` and `consts.gen.ts` and check for no drift.
- [x] 12.6 Web: the `Follow-up` block in `RunVerdictPanel.svelte`, the
  `journal.ts` line, the notify control lists `follow_up` (confirm it reads
  `NOTIFY_TRANSITIONS`), with vitests. Plain CSS only.

## 13. Part C docs and gate

- [x] 13.1 `docs/HOWTO-authoring.md` "Follow-up check" with the V183 to V186
  table in the format of the V160 to V162 table and the older-apb and
  fresh-start notes; `docs/NOTIFY.md` the `follow_up` transition;
  `docs/STATS.md` the follow-up line and the compare field.
- [x] 13.2 `CLAUDE.md` and `AGENTS.md`, same shared text in both:
  `follow_up.rs` and `validate/follow_up` (V183 to V186) in the `apb-core`
  list; `follow_up/` (`read`, `waker`), `run_outcome/follow_up` and
  `run_stats/follow_up` in the `apb-engine` list; a "Concepts that span
  files" paragraph (the checkpoint after `run_finished`, the trust check,
  the older-apb cost).
- [x] 13.3 Gate C, as 4.3 with `follow_up_test`, `journal_compat_test`,
  `pause_waker_cli_test` and `notify_send_test` as the test binaries'
  filters (one per invocation).

# Finish

## 14. Workspace suite, record, report

- [x] 14.1 Write `docs/superpowers/specs/<implementation date>-run-feedback-design.md`
  (for example `2026-11-08-run-feedback-design.md`): the content of
  `proposal.md` and `design.md` as one public record, with no mention of
  `openspec/` and no reference to any outside article, author or project.
- [x] 14.2 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing) and no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`
  and `![` do not count). No CJK.
- [x] 14.3 The full gates once, each with its numbers in the report:
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
- [x] 14.4 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f __pause-waker` and stop only the wakers your tests started).
- [x] 14.5 Final report: per part what was built, decisions you took where
  `design.md` left room, the answers to 0.1, 1.2 and 2.3, each gate with
  numbers, the user-visible changes for the release notes (design "Release
  notes"), the changed file count, and anything left open.
