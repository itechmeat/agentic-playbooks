# Tasks: typed inbox reads

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by the
name `design.md` gives, keep the decision, and note the difference in the
final report. Never invent a different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet. So every section builds in this order: types
and changed signatures first (each step compiles), then a skeleton whose
functions do nothing where a test file needs them to exist, then the red
tests, then the behaviour one piece at a time.

Line numbers are those of main 316c8180. This change has no hard
predecessor; if people-in-the-loop or before-running-alone merged first,
a line may have moved: find the site by its name.

This change adds no start rule, no `TriggeredBy`, no `Initiator::Trigger`,
no validator code and no `wait_for` type: those belong to
`start-rules-and-schedules` and `evals-and-inbox-waits`.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-typed-inbox-reads-target TMPDIR=$HOME/.cache/apb-typed-inbox-reads-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-typed-inbox-reads-target`
fails with a read-only file system, use
`CARGO_TARGET_DIR=/tmp/apb-typed-inbox-reads-target` and
`TMPDIR=/tmp/apb-typed-inbox-reads-tmp` instead, and before each
build check that `df -h /tmp` shows more than 4G free; if not, stop and
report it. `/tmp` is a RAM disk shared with other sessions. Never use
`/dev/shm`, and never put a target dir, cache or temp data inside the
worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Do not commit, push or open a pull request. Do not
start subagents.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-core --lib connector::event_filter`).
2. An integration test file: `cargo test -p <crate> --test main <file module>`
   (for example `cargo test -p apb-engine --test main inbox_filter_test`).
   The CLI package is named `apb`: `cargo test -p apb --test main official_connectors_gate`.
3. `cargo test` takes one name filter per invocation: two modules are
   two commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` (see
`crates/apb-engine/tests/suite/connector_contract_inbox.rs` for an inbox
fixture and `crates/apb-server/tests/suite/ingest_test.rs` for a signed
delivery).

After any change to a type listed in `crates/apb-server/src/ts_contract.rs`
regenerate TypeScript: `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`,
then `cargo test -p apb-server --lib ts_contract` (must pass without the
variable).

Web work uses plain CSS over the existing CSS variables only.

## 0. Preconditions

- [x] 0.1 Check these exist (one `rg -n` each; note the line):
  `pub struct InboxEvent` and `pub fn min_cursor` in
  `crates/apb-core/src/connector/inbox.rs`; `pub struct WebhookSpec` in
  `crates/apb-core/src/connector/def.rs`; `pub struct InboxCall` in
  `crates/apb-engine/src/connector/inbox.rs`. If any is missing, stop and
  report it.
- [x] 0.2 None of these exists: `crates/apb-core/src/connector/event_filter.rs`,
  `crates/apb-engine/tests/suite/inbox_filter_test.rs`; `InboxEvent` has
  no `event_type`. If one exists, it is a
  partial earlier attempt: review it against `design.md`.
- [x] 0.3 This change needs no earlier change of the 1.0 sequence. Note in
  the report which of `people-in-the-loop` and `before-running-alone` are
  already archived (`ls openspec/changes/archive`); nothing here depends
  on them.

## 1. Typed inbox reads (design D2, spec typed-inbox-reads)

- [x] 1.1 Tests first, `def.rs`: `a_webhook_with_both_event_type_fields_is_refused`,
  `event_type_fields_refuse_placeholders`. Then the two `WebhookSpec`
  fields, `names_event_type` and the checks in `validate_webhook_shape`
  (line 1168) and `validate_webhook_templates` (line 1592). Run
  `cargo test -p apb-core --lib connector::def`.
- [x] 1.2 Tests first, `webhook.rs`: `event_type_prefers_the_header`,
  `clean_event_type_refuses_spaces_and_long_values`. Then
  `EVENT_TYPE_MAX`, `event_type`, `clean_event_type`. Run
  `cargo test -p apb-core --lib connector::webhook`.
- [x] 1.3 `InboxEvent.event_type`, the `event_type` parameter of
  `append_bounded` (callers pass `None`), `append_typed`. Test
  `an_old_event_reads_untyped`. Run
  `cargo test -p apb-core --lib connector::inbox`.
- [x] 1.4 Tests first, `crates/apb-core/src/connector/event_filter.rs`
  (skeleton with the types of design D2 "The filter"):
  `an_untyped_event_never_passes_a_type_condition`,
  `a_condition_reads_a_dot_path`, `equals_null_needs_a_present_null`,
  `an_object_never_equals`, `check_refuses_an_object_equals`,
  `explain_never_carries_body_text`. Then `field_equals` (`items.rs`
  line 240) becomes `pub(crate)` and the filter. Run
  `cargo test -p apb-core --lib connector::event_filter`.
- [x] 1.5 Tests first, `inbox.rs`: `a_filtered_read_acks_only_the_leading_skips`,
  `a_filtered_read_leaves_other_consumers_alone`,
  `a_filter_never_hides_a_gap`. Then `FilteredRead` and `read_filtered`.
  Run `cargo test -p apb-core --lib connector::inbox`.
- [x] 1.6 Tests first, `crates/apb-engine/tests/suite/inbox_filter_test.rs`:
  `inbox_read_with_types_skips_and_acks_for_its_consumer`,
  `inbox_read_with_where_returns_skipped`,
  `a_filter_on_ack_is_invalid_args`, `a_read_only_process_moves_no_cursor`.
  Then `crates/apb-engine/src/connector/inbox.rs` design D2 items 1 to 5
  (`filter`, `build`, `send`, `filtered_envelope`, the row's `type`). Run
  `cargo test -p apb-engine --test main inbox_filter_test` and
  `cargo test -p apb-engine --test main connector_contract_inbox`.
- [x] 1.7 Tests first, `crates/apb-server/tests/suite/ingest_test.rs`:
  `a_delivery_stores_its_header_type`, `a_delivery_stores_its_body_type`.
  Then the ingest change of design D2 "The stored event carries its
  type" (the header type, `webhook::event_type`, `append_typed`; nothing
  else in the handler changes). Run `cargo test -p apb-server --test main ingest_test`.
- [x] 1.8 Bundled connectors: github 0.4.0 (account field, webhook
  block, three inbox functions, three `tests.yaml` cases, README
  paragraph) and whatsapp 0.2.0, as design D2 "Bundled connectors" says.
  Run `cargo test -p apb --test main official_connectors_gate` and
  `cargo test -p apb-core --test main bundled_content_lint_test`.
- [x] 1.9 `apb connector doctor`'s webhook line (`crates/apb-cli/src/connector.rs`
  line 795). Docs: `docs/CONNECTORS.md` as design D2 "Surfaces and docs".

## 2. Records

- [x] 2.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both
  (only CLAUDE.md's "Claude-specific" section has no twin): in the
  `apb-core` list `connector/event_filter.rs` (the `types` and `where`
  filter of an inbox read, `explain` never carries body text) next to the
  connector entries. Nothing else: start rules are the next change's.
- [x] 2.2 Write `docs/superpowers/specs/<implementation date>-typed-inbox-reads-design.md`
  (the name of design.md's code map):
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] 2.3 Style over every changed doc, spec and web file: no U+2013 or
  U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no
  exclamation mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`,
  `<!--`, `![` and Rust macros such as `matches!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>`
  finds nothing).

## 3. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) in the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 3.1 `cargo fmt --all -- --check`
- [x] 3.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 3.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 3.4 `cargo test --workspace --doc`
- [x] 3.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; the generated files are current)
- [x] 3.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 3.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 3.8 `scripts/third-party-notices.sh --check` (regenerated with the
  dependency updates; it must pass)
- [x] 3.9 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 3.10 `openspec validate typed-inbox-reads --strict`

## 4. Close

- [x] 4.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f __pause-waker` and `pgrep -f __drive`).
- [x] 4.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to
  0.1 to 0.3, each gate of section 3 with its numbers, the user-visible changes
  for the release notes (design "Release notes"), the changed file
  count, and anything left open.
