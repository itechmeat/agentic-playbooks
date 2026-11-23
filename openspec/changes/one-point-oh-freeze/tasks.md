# Tasks: one point oh freeze

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

The order is: preconditions, the platform position, the freeze checks
and writers, the fixtures generated from the final production code, the
records, the bump, then the carried fixes (section 10, written last
because it is not part of the freeze, done right after section 6), and
then style, gates and close (sections 7 to 9).

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a file moved), find the site by
the name `design.md` gives, keep the decision, and note the difference
in the final report. Never invent a different behaviour.

For a changed behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet. A test that pins existing behaviour (the freeze checks, the
registry test, the self-update cases) may pass on its first run; say so
in the report.

Sites are named by function, constant or file, never by line: the
thirteen earlier changes of the 1.0 plan (people-in-the-loop,
before-running-alone, typed-inbox-reads, wait-park, eval-doubles,
secret-params-in-run-files, mcp-tool-connectors, one-point-oh-fixes,
mcp-http-and-surfaces, incremental-lists-and-session-fork, start-rules,
inbox-waits, templates-and-global-playbooks) are merged before this one.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-one-point-oh-freeze-target TMPDIR=$HOME/.cache/apb-one-point-oh-freeze-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-one-point-oh-freeze-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-one-point-oh-freeze-target`
and `TMPDIR=/tmp/apb-one-point-oh-freeze-tmp` instead, and before each build
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
   (for example `cargo test -p apb-engine --lib concurrency`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   concurrency_test`). The CLI package is named `apb`: `cargo test -p apb
   --test main prune_cli_test`.
3. `cargo test` takes one name filter per invocation: two modules are
   two commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` (see
`crates/apb-engine/tests/suite/start_key_test.rs` for a project fixture).

After any change to a type or constant listed in
`crates/apb-server/src/ts_contract.rs` regenerate TypeScript:
`APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then
`cargo test -p apb-server --lib ts_contract` (must pass without the
variable).

## 1. Preconditions (design "Facts on 121e5c06")

- [x] 1.1 `git log --oneline -1` and `grep -n '^version' Cargo.toml`:
  record both (the version is still `1.0.0-beta.7`: no earlier change
  bumps it). Every one of the thirteen earlier changes is archived under
  `openspec/changes/archive/` (a missing one is a stop; report it).
  Read their archived proposals for the release notes lines (design D5
  item 2). No earlier change retired a validator code (design D1.4 item
  1); if one now says otherwise, it is one more `RETIRED_CODES` entry,
  reported.
- [x] 1.2 Record the numbers of design "Facts on 121e5c06" on the tree:
  the `EventPayload` variant count, the tool count of `READ_ONLY` plus
  `REMOVED_IN_READ_ONLY`, `git grep -ohE '"V[0-9]+"' crates/apb-core/src
  | sort -u | wc -l` (one of them, `"V7"`, is a test prefix, not a code),
  the length of `GuardedFeature::ALL`, and the corpus document count. A
  number that differs is noted, not a stop.
- [x] 1.3 Each of these exists (one `rg -n` each): `pub const READ_ONLY`
  and `pub const REMOVED_IN_READ_ONLY` in
  `crates/apb-mcp/src/server/read_only.rs`; `pub fn is_checkpoint` in
  `crates/apb-engine/src/event.rs`; `pub const NODE_TYPES`, `pub struct
  Playbook` and `pub struct Defaults` in `crates/apb-core/src/schema.rs`;
  `fn takes_prerelease`, `fn self_update_as`, `fn serve_releases` and
  `fn write_receipt` in `crates/apb-cli/src/selfupdate.rs`;
  `ReasonCode::OsUnsupported` and `pub const ALL_CODES` in
  `crates/apb-core/src/isolation/contract.rs`; `fn isolation_items` in
  `crates/apb-engine/src/fit/setup.rs`; `pub fn doctor_line` in
  `crates/apb-engine/src/sandbox/probe.rs`; `pub const ALL` in
  `crates/apb-core/src/validate/guarded.rs`; `pub fn patch_refusal` in
  `crates/apb-engine/src/run_origin.rs`; `pub fn parse_rule_document`
  and `pub struct TriggerStateFile` re-exported by
  `crates/apb-core/src/trigger/mod.rs`; `pub fn tag` and `pub fn
  verify_tag` in `crates/apb-core/src/run_origin.rs`; `pub fn
  global_config_path`, `pub fn project_config_path` and `pub fn
  load_merged` in `crates/apb-core/src/connector/config.rs`; `pub const
  CONFLICT` in `crates/apb-engine/src/start_key.rs` and `pub fn
  already_started` and `pub fn run_wait_result_as` in
  `crates/apb-mcp/src/tools/run.rs`; `pub fn supervisor_wait_event` in
  `crates/apb-mcp/src/tools/supervisor.rs`; `pub fn
  list_versions_with_provenance` and `pub fn read_provenance` in
  `crates/apb-core/src/versioning/mod.rs`; `pub fn move_current`, `pub
  fn history` and `pub fn settled` in
  `crates/apb-core/src/versioning/current.rs`; `pub struct
  VersionProvenance` and `pub fn read_current_pointer` in
  `crates/apb-core/src/versioning/sidecar.rs`; `pub fn read_candidate` in
  `crates/apb-core/src/candidate_pointer.rs`; `pub struct TrustStore` and
  `pub fn read_lifecycle` in `crates/apb-core/src/trust.rs`; `pub fn
  read_from`, `pub fn floor_in` and `pub fn set_by_person` in
  `crates/apb-core/src/launch/floor.rs`. If one is missing, stop and
  report it.
- [x] 1.4 None of these exists: `docs/COMPATIBILITY.md`,
  `crates/apb-core/src/validate/codes.rs`,
  `crates/apb-core/tests/fixtures/schema-1.0/`,
  `crates/apb-core/tests/fixtures/files-1.0/`,
  `crates/apb-core/tests/fixtures/store-1.0/`,
  `crates/apb-engine/tests/fixtures/journal/every_event.jsonl`,
  `crates/apb-engine/tests/fixtures/run-files-1.0/`,
  `crates/apb-engine/tests/fixtures/signed-1.0/`,
  `crates/apb-mcp/tests/fixtures/`. If one exists, it is a partial
  earlier attempt: review it against `design.md`.

## 2. Platform position (design D3, spec `platform-position`)

- [x] 2.1 Test first: `os_unsupported_states_the_one_point_x_position`
  in `crates/apb-core/tests/suite/isolation_contract_test.rs`; run
  `cargo test -p apb-core --test main isolation_contract_test` and see
  it fail; then the sentence in `ReasonCode::text`; run again.
- [x] 2.2 Test first: `the_fit_item_carries_the_one_point_x_sentence`
  in a new `mod tests` of `crates/apb-engine/src/fit/setup.rs` and
  `the_doctor_line_off_linux_is_the_one_point_x_sentence` in
  `crates/apb-engine/src/sandbox/probe.rs` `mod tests`; run `cargo test
  -p apb-engine --lib fit::setup` and `cargo test -p apb-engine --lib
  sandbox::probe` and see them fail; then `reasons_text` in
  `isolation_items` and the `doctor_line` change; run both again.
- [x] 2.3 `rg -n "this system is not supported" crates web docs`: update
  every test that asserted the old sentence (name each in the report);
  regenerate `web/src/lib/consts.gen.ts` with the ts_contract command of
  "Environment".
- [x] 2.4 `docs/ISOLATION.md` "## macOS and other systems": "for the
  whole 1.x line".

## 3. Freeze checks and writers (design D0, D1, spec `compatibility-freeze`)

Precondition: sections 1 and 2 are complete. This section writes the
checks and the writers with empty fixtures; section 4 fills them.

- [x] 3.1 `crates/apb-core/src/validate/codes.rs` with `CODES` (every
  code the scan of design D1.4 item 2 finds, its severities and a
  one-line meaning), `RETIRED_CODES` (V15 with the reason "never
  emitted" and V16 with the reason of design D1.4 item 1) and
  `the_registry_is_what_the_validator_can_emit`; `pub mod codes;` in
  `validate/mod.rs`; the comment in `validate::validate` (V16 points to
  `RETIRED_CODES`, V15 is said to be listed there as never emitted, `//
  V14, V15` becomes `// V14`) and the module doc of
  `validate/isolation.rs`. Run `cargo test -p apb-core
  --lib validate::codes`.
- [x] 3.2 `crates/apb-core/tests/suite/schema_freeze_test.rs` (declared
  in `crates/apb-core/tests/main.rs`) with `MARKER`, `documents`,
  `fields_of`, `variants_of`, `variant_fields_of`, `DERIVED_STRUCTS`,
  `SCHEMA_ENUMS`, `HAND_FIELDS`, `NORMALIZED`, rules 1 to 6 of design
  D1.1, the four coverage tests of design D1.1 "Coverage", the five tests
  of design D1.4 item 5 with the witness runner (`WITNESS_CONTEXTS`,
  `WITNESS_FNS` and the four witness kinds),
  `the_writer_never_changes_frozen_errors`, the switch helper
  `write_freeze_enabled` and the writer test `write_freeze` (design D0;
  no other item of the module is named `write_freeze`). Create
  `crates/apb-core/tests/fixtures/schema-1.0/` with an empty
  `corpus.yaml`, `fields.yaml`, `coverage.txt`, `witnesses.yaml` and
  `validator-codes.txt` and `{}` as `expected.json`. Run `cargo test -p
  apb-core --test main schema_freeze_test` and see rules 5 and 6, the
  coverage tests and the code tests fail.
- [x] 3.3 `crates/apb-core/tests/suite/files_freeze_test.rs` (declared)
  with the tests of design D1.5 item 3 (the account files of both scopes
  placed under a temporary `APB_CONFIG_DIR` and project and read through
  `load_merged`, the cases of item 3.3), the meaning rule of design D1.5
  (`expect_<document>` functions and
  `every_document_has_an_expectation`), `write_freeze_enabled` and the
  writer test `write_freeze`; create
  `crates/apb-core/tests/fixtures/files-1.0/` with the five empty files.
  Run `cargo test -p apb-core --test main files_freeze_test` and see the
  connector corpus rule fail.
- [x] 3.4 Journal: an empty `every_event.jsonl` and the five tests of
  design D1.2 in `journal_compat_test.rs` (lines read, values compared as
  parsed `serde_json::Value`s, never as strings; `CHECKPOINTS` asserted
  to stay checkpoints, never asserted `false` for other types); run
  `cargo test -p apb-engine --test main journal_compat_test` and see
  `every_event_type_has_a_frozen_line` name the missing types.
- [x] 3.5 `crates/apb-engine/tests/suite/persistence_freeze_test.rs`
  (declared in `crates/apb-engine/tests/main.rs`) with the run file
  tests of design D1.5 item 2, the signed store tests of item 4
  (`TEST_KEY` written as `run-origin.key` under a temporary
  `APB_CONFIG_DIR`), the meaning rule of design D1.5 (an expectation per
  document asserting named typed values and the reader's decisions, and
  `every_document_has_an_expectation`), `write_freeze_enabled` and the
  writer test `write_freeze`; create the empty
  `run-files-1.0/manifests.yaml`, `run-files-1.0/run-configs.yaml` and
  `{}` as `signed-1.0/stores.json`. Run `cargo test -p apb-engine --test
  main persistence_freeze_test` and see the "every kind is frozen" checks
  fail.
- [x] 3.6 MCP: `crates/apb-mcp/src/server/freeze_tests.rs` with
  `normalize` (the narrowing constraints of design D1.3 recorded),
  `WIDENINGS`, `ARG_TYPES`, the seven rules of design D1.3 (boundary
  values and `base` sent through each tool's argument type among them),
  the `MACHINE_CODES` table and the four machine code tests of design
  D1.5 item 1, the result tests of item 5 with one producer per
  documented variant of every listed tool,
  `the_writer_refuses_a_removed_argument`, `write_freeze_enabled` and
  the writer test `write_freeze`; declare it in `server/mod.rs`. Create `crates/apb-mcp/tests/fixtures/` with
  `mcp-tools-1.0.json` as `{"format": 1, "tools": {}}`,
  `mcp-results-1.0.json` as `{"format": 1, "tools": {}}` and an empty
  `machine-codes-1.0.txt`. Run `cargo test -p apb-mcp --lib
  server::freeze_tests` and see rule 7 and the "found but not frozen"
  checks fail.
- [x] 3.7 Corpus: `FREEZE_READER`, rows `rowf1_freeze_reader_writer_new_reader`
  and `rowf2_new_writer_freeze_reader`, the module doc (design D1.2 "The
  released binary corpus"). With the binaries built
  (`scripts/old-apb.sh <tag> $HOME/.cache/apb-old-binaries` for the
  freeze reader; it builds a release in a detached worktree, so allow
  time), run `APB_OLD_BINARIES=$HOME/.cache/apb-old-binaries cargo test
  -p apb --test main journal_corpus_test::rowf`. Without network or time
  to build it, the rows print their skip line; say so.
- [x] 3.8 `.github/workflows/ci.yml` `OLD_APB_TAGS` and its comment
  (design D1.2 item 4).
- [x] 3.9 `crates/apb-core/tests/suite/store_freeze_test.rs` (declared
  in `crates/apb-core/tests/main.rs`) with the playbook store tests of
  design D1.5 item 6 (the files placed under a temporary project and
  `APB_CONFIG_DIR`, read through the readers that item names, the
  expectations of the meaning rule and
  `every_document_has_an_expectation`), `write_freeze_enabled` and the
  writer test `write_freeze`; create
  `crates/apb-core/tests/fixtures/store-1.0/store.json` as
  `{"project": {}, "config": {}}`. Run `cargo test -p apb-core --test
  main store_freeze_test` and see the "every store file is frozen" check
  fail.

## 4. Fixtures from the final production code (design D1, D1.6)

Precondition: sections 1 to 3 are complete and no production code
changes after this section except the records and the version.

- [x] 4.1 Write `fields.yaml` and `coverage.txt` by hand (design D1.1
  "Field fixtures" and "Coverage", the inbox wait places of design D1.6
  item 1.1 included) until the coverage tests and
  `every_node_type_has_a_fixture` pass; write `witnesses.yaml` and the
  `WITNESS_FNS` it needs (design D1.4 item 4; V260 to V267 among them)
  until `every_active_code_has_a_witness` and
  `every_witness_fires_its_code` pass.
- [x] 4.2 Run `APB_WRITE_FREEZE=1 cargo test -p apb-core --test main
  schema_freeze_test::write_freeze` (it fails with "freeze written"),
  review the diff of `corpus.yaml`, `expected.json` and
  `validator-codes.txt`, then run `cargo test -p apb-core --test main
  schema_freeze_test` without the switch: every test passes. A failing
  `no_frozen_key_is_dropped_on_parse` on a field fixture: change the
  fixture's value first; only a real spelling change goes into
  `NORMALIZED` (record each).
- [x] 4.3 Write `profiles.yaml`, the hand-written part of
  `connectors.yaml`, `connector-accounts.yaml`, `settings.yaml` and
  `trigger-rules.yaml` (design D1.5 item 3); run the files writer, review
  the copied official manifests, run `files_freeze_test` without the
  switch: every test passes.
- [x] 4.4 Write the journal lines (design D1.2 "Writing the lines") until
  the five journal tests pass, and fill `CHECKPOINTS`; the line of
  `inbox_wait_read` is among them.
- [x] 4.5 Run the persistence writer, review the six runs' files and the
  eight signed store entries (each with the nondefault state design D1.5
  items 2 and 4 name), write the expectation of every document, run
  `persistence_freeze_test` without the switch: every test passes.
- [x] 4.6 Fill `MACHINE_CODES` (design D1.5 item 1: a constant where one
  exists, else a literal with its source symbol; `start_key_conflict`
  through `apb_engine::start_key::CONFLICT` and `already_started`
  among them) until `every_scanned_code_is_in_the_table` passes; run the
  MCP writer, review the three files (`trigger_list` and `trigger_test`
  among the tools, each with `base`, examples and boundaries; every
  variant of design D1.5 item 5 in `mcp-results-1.0.json`; the codes of
  design D1.6 items 2.3 and 3.3 in `machine-codes-1.0.txt`; none of the
  codes design D1.6 item 4 excludes), run `cargo test -p apb-mcp --lib
  server::freeze_tests` without the switch: every test passes.
- [x] 4.7 Run the store writer (design D1.5 item 6), review
  `store.json` (three versions, two moves of `current`, a candidate, the
  sidecars with `created_by` `template`, `supervisor` and `import`, the
  trust approvals of four kinds, two floor records, a retired
  lifecycle), write the expectations, run `cargo test -p apb-core --test
  main store_freeze_test` without the switch: every test passes.
- [x] 4.8 Check the generated fixtures against design D1.6 item by item
  and record the frozen counts (documents, coverage lines, witnesses,
  event types, tools, arguments, boundary values, result variants,
  codes, machine codes, guard names, store kinds, playbook store
  files).

## 5. Records (design D1.7)

- [x] 5.1 `docs/COMPATIBILITY.md` (design D1.7 items 1 to 6).
- [x] 5.2 `CLAUDE.md` and `AGENTS.md`, the same shared text in both
  (only CLAUDE.md's "Claude-specific" section has no twin): the
  compatibility bullet, the module map entry for `validate/codes.rs` and
  the `skip_serializing_if` sentence of the event field convention (a
  new field adds no member to a frozen line); the same sentence in
  `docs/adr/0001-new-events-are-safe-to-skip.md`.
- [x] 5.3 `llms.txt` (the compatibility document line) and
  `docs/HOST-INTEGRATION.md` (the section "## What a host can rely on",
  naming the tools of design D1.5 item 5).
- [x] 5.4 Write `docs/superpowers/specs/<implementation date>-one-point-oh-freeze-design.md`
  (the file name `2026-11-XX-one-point-oh-freeze-design.md` with the
  real date): the content of `proposal.md` and `design.md` as one
  public record, with no mention of `openspec/` and no reference to any
  outside article, author or project.

## 6. Release candidate (design D5, spec `release-candidate`)

- [x] 6.1 Tests in `crates/apb-cli/src/selfupdate.rs` `mod tests`:
  `a_beta_install_sees_the_release_candidate` (code 10, "update
  available") and `a_stable_install_does_not_see_the_release_candidate`
  (code 0, "apb is up to date"), through `serve_releases`,
  `write_receipt` and `self_update_as(true, ..)`: the check path only
  (design D5 item 3); do not drive the install path. Run `cargo test -p apb --bin apb selfupdate` (they
  may pass at once: they pin existing behaviour).
- [x] 6.2 Version `1.0.0-rc.1` in the files of design D5 item 1;
  `cargo build` updates `Cargo.lock`.
- [x] 6.3 `docs/release-notes/v1.0.0-rc.1.md` (design D5 item 2 and
  "Release notes"; the soak checklist holds item 6, the real update a
  person performs and records, with the steps of design D5 item 5), and
  `llms.txt` version mentions if any.
- [x] 6.4 Run `cargo test -p apb --test main release_workflow_test`.

## 7. Style

Precondition: section 10 (Carried fixes) is complete.

- [x] 7.1 Over every changed doc, YAML and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![`
  and Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>`
  finds nothing).
- [x] 7.2 `rg -n "tailwind|@apply|class=\"[a-z]+-[0-9]" web/src` finds
  nothing new in the files this change touched.

## 8. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean. A failing freeze test is a
compatibility break to fix in the code, never a fixture to regenerate.

- [x] 8.1 `cargo fmt --all -- --check`
- [x] 8.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 8.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 8.4 `cargo test --workspace --doc`
- [x] 8.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; the generated files are current)
- [x] 8.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 8.7 `scripts/third-party-notices.sh --check` (no dependency is
  added; it must pass unchanged)
- [x] 8.8 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 8.9 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 8.10 `openspec validate one-point-oh-freeze --strict`

## 9. Close

- [x] 9.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Keep `$HOME/.cache/apb-old-binaries` (CI-shaped
  cache, reused). Stop only the processes you started.
- [x] 9.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did (every moved
  site), the answers to 1.1 to 1.4, every `NORMALIZED` entry with its
  reason, every `RETIRED_CODES` entry, the frozen counts of 4.8, each
  gate of section 8 with its numbers, the user-visible changes for the
  release notes (design "Release notes" and D5 item 2), the changed
  file count, and anything left open.

## 10. Carried fixes (design "Carried fixes")

Done right after section 6, before section 7. Neither task changes
production behaviour.

- [x] 10.1 `crates/apb-engine/tests/suite/inbox_wait_test.rs`
  `a_batch_supervisor_patch_drops_the_wait_scan`: the wait gets a
  timeout no loaded run reaches (`batch_wait_yaml(600)`), the final loop
  runs through `poll_until` instead of its own 10 second bound, and the
  steps that wait on agent processes use a named longer deadline; the
  assertions stay. Run `cargo test -p apb-engine --test main
  inbox_wait_test::a_batch_supervisor_patch_drops_the_wait_scan` twice in
  parallel shells and record both results.
- [x] 10.2 `crates/apb-engine/src/connector/oauth/consent.rs` `mod tests`
  `stub_server`: after `accept`, set the stream back to blocking with a
  read timeout, so the stub records the whole request, headers and body,
  before it answers (on macOS an accepted socket inherits the listener's
  non-blocking mode). Run `cargo test -p apb-engine --lib
  connector::oauth::consent`.

## File budget

60 changed files, listed in `design.md` "Files": platform 6; schema and
validator codes 11; person files 6; playbook store 2; journal,
persistence and the released corpus 9; MCP and machine codes 5; records
13; release 6; carried fixes 2.
