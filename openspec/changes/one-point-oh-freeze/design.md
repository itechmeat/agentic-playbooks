# Design: one point oh freeze

> Status (2026-11-22): ready for an implementer. Every site below is
> named by its function, constant or file and was verified on main
> 121e5c06 (1.0.0-beta.7), where all thirteen earlier changes of the 1.0
> plan are merged and archived. No line number is left. The design
> review of 2026-11-21 (findings F1 to F13) and the second review of
> 2026-11-22 (eleven findings, with the owner's decisions on the playbook
> store and V15) are folded into the sections below; nothing of them is
> kept apart.

Read `proposal.md` first. The thirteen earlier changes of the 1.0 plan
(people-in-the-loop, before-running-alone, typed-inbox-reads, wait-park,
eval-doubles, secret-params-in-run-files, mcp-tool-connectors,
one-point-oh-fixes, mcp-http-and-surfaces,
incremental-lists-and-session-fork, start-rules, inbox-waits,
templates-and-global-playbooks) are merged before this change starts and
touch many of the same files: find every site by the function or the
constant named here and note every moved or renamed site in the final
report. A site whose function no longer exists is a stop: report it, do
not invent one.

The freeze fixtures of D1 are never written from this document. They are
generated from the tree this change ends with (main as it was when the
change started, plus this change's own additions), by the writers of D0,
as the last implementation step before the records. D1.6 says which
names of the last three changes the fixtures must hold and which they
leave out, so a reviewer can check the generated files against it.

Four decision groups, built in the order D0, D3, D1, D5 (tasks.md). The
bump (D5) is the last step. Global playbooks and the template gallery,
drafted here at first, are the change templates-and-global-playbooks,
merged before this one.

Nothing in this document is left to the implementer. Where a choice was
open, it is decided here with its reason; tasks.md only orders the work.

## Facts on 121e5c06

These numbers are what the freeze starts from; the final report states
the numbers the generated fixtures hold.

1. `EventPayload` (`crates/apb-engine/src/event.rs`) has 100 variants,
   no `#[serde(alias)]` on a variant; `EventPayload::is_checkpoint` is
   true for `node_finished`, `run_paused`, `run_finished`, `run_aborted`
   and `follow_up_observed`.
2. The MCP server serves 91 tools: 34 in `READ_ONLY` (3 of them
   `Restricted`) and 57 in `REMOVED_IN_READ_ONLY`
   (`crates/apb-mcp/src/server/read_only.rs`).
3. `git grep -ohE '"V[0-9]+"' crates/apb-core/src | sort -u` prints 226
   strings. One of them, `"V7"`, is not a code: it is a prefix match in
   the test module of `validate/decisions.rs`. So 225 codes are in use.
   V15 has never been emitted in the history of the repository (the
   comment `// V14, V15` in `validate::validate` is stale), so it is
   listed retired with the reason "never emitted" and its number is never
   reused (D1.4); V16 is retired; V248 and V282 were reserved by earlier
   changes and never used.
4. `GuardedFeature::ALL` (`crates/apb-core/src/validate/guarded.rs`) has
   15 names.
5. `examples/playbooks/` holds 15 top-level `*.yaml` files and
   `bundled/playbooks/` holds 6 playbooks: 21 corpus documents.
   `examples/playbooks/quality-process/` is untracked local research; the
   top-level glob never reads it.
6. The newest published release is `v1.0.0-beta.7`; the released binary
   corpus reads with `v0.25.0` and `v1.0.0-beta.1`.

## D0. Shared pieces

No validator code is new in this change (V320 came with
one-point-oh-fixes, V340 with secret-params-in-run-files; V321 to V339
stay reserved for this family).

### The writers

Every frozen fixture of D1 is checked by assertion tests that fail on
drift and never write. Each fixture group has exactly one writer, a test
named `write_freeze` in the group's test file, and no other test writes.
The name `write_freeze` belongs to that test alone: no other item of the
module (function, constant, helper) may take it, because a Rust module
cannot hold two functions of one name. The groups:

1. schema (`crates/apb-core/tests/suite/schema_freeze_test.rs`):
   `corpus.yaml`, `expected.json`, `validator-codes.txt` under
   `crates/apb-core/tests/fixtures/schema-1.0/` (`fields.yaml`,
   `coverage.txt` and `witnesses.yaml` are written by hand and only
   read);
2. person files (`crates/apb-core/tests/suite/files_freeze_test.rs`):
   `connectors.yaml` under `crates/apb-core/tests/fixtures/files-1.0/`
   (the official connectors' manifests copied verbatim; the other files
   of that directory are written by hand);
3. run persistence and signed stores
   (`crates/apb-engine/tests/suite/persistence_freeze_test.rs`):
   `manifests.yaml`, `run-configs.yaml` under
   `crates/apb-engine/tests/fixtures/run-files-1.0/` and
   `crates/apb-engine/tests/fixtures/signed-1.0/stores.json`;
4. MCP and machine codes (`crates/apb-mcp/src/server/freeze_tests.rs`):
   `mcp-tools-1.0.json`, `mcp-results-1.0.json` and
   `machine-codes-1.0.txt` under `crates/apb-mcp/tests/fixtures/`;
5. playbook store (`crates/apb-core/tests/suite/store_freeze_test.rs`):
   `store.json` under `crates/apb-core/tests/fixtures/store-1.0/` (D1.5
   item 6).

The switch, one constant in each test file that has a writer:

```rust
/// Set to `1` to let the test `write_freeze` add what this build has and
/// the freeze lacks. The writer only ever adds: it never removes an entry,
/// never changes a frozen error code, line or argument, and writes
/// nothing when any freeze rule fails.
const WRITE_FREEZE_ENV: &str = "APB_WRITE_FREEZE";

/// Whether the switch is set. Named `write_freeze_enabled`, never
/// `write_freeze`: that name is the writer test's.
fn write_freeze_enabled() -> bool {
    std::env::var(WRITE_FREEZE_ENV).as_deref() == Ok("1")
}
```

The test `write_freeze` returns at once, passing, when
`write_freeze_enabled()` is false. With the switch set, it works in this
order:

1. Read what is frozen. Compute the proposed files in memory: frozen
   entries kept byte for byte (except where a decision below names the
   one field the writer may rewrite), new documents and entries added,
   each file in its sort order. A document without an expectation is an
   addition, not a failure.
2. Run every check of the group over the proposed snapshot, the checks
   that guard removal, retirement, parsing and key retention included
   (for the schema group also every D1.4 check). Any failure panics and
   nothing is written: the switch cannot paper over a break.
3. Write each file to a temporary file in the same directory and rename
   it over the fixture. The files are written in a fixed order with
   `expected.json` (or the group's last file) last; a run cut between two
   renames leaves a state the next writing run completes, because every
   step only adds.
4. Panic with `"freeze written: <paths>; review the diff and run the
   test again without APB_WRITE_FREEZE"`, so a writing run never passes
   by accident.

The commands (also in `docs/COMPATIBILITY.md` "Extending the freeze"):

```sh
APB_WRITE_FREEZE=1 cargo test -p apb-core --test main schema_freeze_test::write_freeze
APB_WRITE_FREEZE=1 cargo test -p apb-core --test main files_freeze_test::write_freeze
APB_WRITE_FREEZE=1 cargo test -p apb-engine --test main persistence_freeze_test::write_freeze
APB_WRITE_FREEZE=1 cargo test -p apb-mcp --lib server::freeze_tests::write_freeze
APB_WRITE_FREEZE=1 cargo test -p apb-core --test main store_freeze_test::write_freeze
```

The journal fixture (D1.2) has no writer: its lines are written by hand
(D1.2 "Writing the lines") because each must be a value a person checked.

Each writer's merge function has a unit test that proves it refuses a
break (named in each part below); the test builds the frozen and the
current state in memory and asserts the panic and an unchanged file.

## D3. Platform position

The position is unchanged code-wise (`docs/ISOLATION.md` "## macOS and
other systems"): on a system other than Linux, `full` refuses
`os_unsupported` and `best_effort` runs without a sandbox, recording
`no_sandbox`; apb never uses `sandbox-exec`. This change states it as
the 1.x position and routes the one sentence to both surfaces that show
it.

1. `crates/apb-core/src/isolation/contract.rs`, `ReasonCode::text` for
   `ReasonCode::OsUnsupported` (today "The sandbox needs Linux with
   bubblewrap; this system is not supported.") becomes:
   `"The sandbox needs Linux with bubblewrap; apb 1.x has no sandbox on
   this system, so full refuses and best_effort runs without one."`
   The machine spelling stays `os_unsupported` (`ReasonCode::id` and the
   serde form; the type has no `Display`).
2. The fit report: `crates/apb-engine/src/fit/setup.rs`
   `isolation_items` renders its reason lists through the `Display` of
   `Reason` (code and detail). The rendering moves into a private
   helper `reasons_text(&[Reason]) -> String`, which renders a reason
   whose code is `os_unsupported` as `os_unsupported: <the sentence of
   item 1>` (the code kept in front for a reader that matches it) and
   every other reason as today.
3. The doctor: the non-Linux branch of
   `crates/apb-engine/src/sandbox/probe.rs` `doctor_line` uses
   `ReasonCode::OsUnsupported.text()` in place of its own literal.
4. `web/src/lib/consts.gen.ts` carries the sentence in
   `ISOLATION_REASON_TEXTS`: regenerate it with
   `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract` and
   commit it; never edit it by hand.
5. `docs/COMPATIBILITY.md` "## Platforms" (D1.5) and `docs/ISOLATION.md`
   "## macOS and other systems" say "for the whole 1.x line".

Tests, each seen failing first:

1. `crates/apb-core/tests/suite/isolation_contract_test.rs`:
   `os_unsupported_states_the_one_point_x_position` asserts the exact
   sentence through `ReasonCode::OsUnsupported.text()` and through
   `contract::text("os_unsupported")`, and the machine spelling through
   `ReasonCode::OsUnsupported.id()` and `serde_json::to_string`.
2. `crates/apb-engine/src/fit/setup.rs`, a new `#[cfg(test)] mod tests`:
   `the_fit_item_carries_the_one_point_x_sentence` (a reason list with
   one `os_unsupported` refusal renders the code and the sentence).
3. `crates/apb-engine/src/sandbox/probe.rs` `mod tests`:
   `the_doctor_line_off_linux_is_the_one_point_x_sentence` (a
   `HostFacts` with a non-Linux `os`: the line is the sentence and
   `full` is not available).

Any test that asserts the old sentence is updated to the new one; the
search `rg -n "this system is not supported" crates web docs` finds
only the contract and the generated file on 121e5c06. Say in the report
which tests were updated.

Wrong implementation: changing the reason code instead of the sentence:
`os_unsupported` is a machine code frozen in D1.6 (a host reads it).
Caught by test 1.

## D1. Compatibility freeze

Six parts: the schema corpus, the journal fixture, the MCP tools, the
validator codes, the machine names and files, and the document. Each
fixture is generated (or, for the journal and the hand-written files,
written) as the last implementation step, from the tree this change
ends with.

### D1.1 Schema corpus

Files under `crates/apb-core/tests/fixtures/schema-1.0/`:

1. `corpus.yaml` (written by the writer): verbatim copies of every
   top-level `examples/playbooks/*.yaml` and every
   `bundled/playbooks/<id>/playbook.yaml`, as they are when the freeze is
   written. Documents are separated by a marker line, never by `---` (a
   prompt may hold a `---` line):

```yaml
# apb-freeze-document: examples/dependency-security-fix.yaml@1.0.0
schema: 2
id: dependency-security-fix
...
# apb-freeze-document: bundled/apb-code-review@1.4.0
...
```

   The name is `examples/<file>@<version>` or `bundled/<id>@<version>`
   (the `version:` of the document), so a later release that changes an
   example adds a new document beside the frozen one.
2. `fields.yaml` (by hand), same markers, the field fixtures ("Field
   fixtures" below).
3. `coverage.txt` (by hand), the coverage inventory ("Coverage" below).
4. `expected.json` (written by the writer): `{"<document name>":
   {"errors": ["V41"], "warnings": ["V112"]}, ...}`, keys sorted, each
   list sorted by code number and without repeats. Every document of
   `corpus.yaml` and `fields.yaml` has an entry.
5. `validator-codes.txt` and `witnesses.yaml` (D1.4).

The test file `crates/apb-core/tests/suite/schema_freeze_test.rs` (new,
declared in `crates/apb-core/tests/main.rs`) reads them with
`std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema-1.0/<file>"))`
and splits documents with:

```rust
const MARKER: &str = "# apb-freeze-document: ";
/// `(name, text)` of every document, in file order; text without its
/// marker line. Panics on a repeated name.
fn documents(file: &str) -> Vec<(String, String)>;
```

The policy for codes, one in the design, the spec and
`docs/COMPATIBILITY.md`: a frozen document's error codes are compared
exactly and never rewritten; its warning codes are recorded by the
writer and not asserted (a warning may come and go in 1.x).

Rules, one test each:

1. `every_frozen_document_parses`: `Playbook::from_yaml` succeeds for
   every document of `corpus.yaml` and `fields.yaml`.
2. `every_frozen_document_keeps_its_frozen_errors`:
   `apb_core::validate::validate(&pb, &ValidationContext::default())`;
   the sorted unique error codes equal `errors`. Warnings are not
   compared.
3. `no_frozen_key_is_dropped_on_parse`: for each document, every typed
   path of the document (`yaml_serde::Value`; one segment per mapping key
   or sequence index, such as `nodes[2].list.batch.size`) is a typed path
   of `yaml_serde::to_value(&parsed)`. Map keys the user names (`params`
   names, node ids used as map keys, judge value maps and the like) are
   data: such a key is compared as present or absent at its exact place,
   never matched by a rule. Exceptions only from

```rust
/// `(document, exact path, written form, serialized form, reason)`.
const NORMALIZED: &[(&str, &str, &str, &str, &str)] = &[];
```

   each entry asserted in both directions by the same test: the written
   form parses, and the serialized form parses to the same value. An
   entry is allowed only for a value the serializer writes under another
   spelling, never for a field the schema no longer has, and no entry
   matches by suffix. The entries the writer of `fields.yaml` is known
   to meet: `continue_session` written as a mapping serializes as its
   short form; a connector binding written `{ name: <c> }` serializes as
   the bare name. Every entry is named in the final report.
4. `every_coverage_line_is_exercised`,
   `every_derived_field_is_in_the_coverage`,
   `every_variant_field_is_in_the_coverage` and
   `every_flattened_field_is_in_the_coverage` ("Coverage" below).
5. `every_node_type_has_a_fixture`: every entry of `NODE_TYPES`
   (`schema.rs`) is the `type` of a node in some `fields.yaml` document
   named `node/<type>`.
6. `the_corpus_holds_every_example_and_bundled_playbook`: for every
   top-level `examples/playbooks/*.yaml` and every
   `bundled/playbooks/<id>/playbook.yaml` on disk, a document named
   `examples/<file>@<its version>` or `bundled/<id>@<its version>`
   exists and holds exactly the file's bytes. A missing one fails with
   "run APB_WRITE_FREEZE=1 ... to freeze <name>"; changed bytes under a
   frozen name fail with "bump the version of <file>". Duplicate names
   are refused by `documents`.

The writer (D0): appends to `corpus.yaml` every document rule 6 finds
missing (the file's bytes after its marker); adds an `expected.json`
entry for every document that has none; rewrites the `warnings` of an
existing entry to what `validate` reports now; never changes an
existing `errors` list, never replaces a corpus entry, never removes an
entry or a document.

#### Field fixtures

`fields.yaml` is written by hand once, from `coverage.txt` and
`NODE_TYPES`. Each document is the smallest valid playbook that carries
its places with a value that is **not** the field's default (a default
value may be dropped when the playbook is serialized again, which rule 3
would see as a removal):

```yaml
# apb-freeze-document: field/deadline
schema: 2
id: f-deadline
name: Field deadline
version: 1.0.0
deadline: 45m
nodes:
  - { id: start, type: start }
  - { id: done, type: finish, outcome: success }
edges:
  - { from: start, to: done }
```

1. Documents are named `field/<name>` (a top-level optional field),
   `defaults/<name>` (a `Defaults` field), `node/<type>` (one per node
   type, each node with as many of its kind's optional fields as one
   valid node can hold) and `node-common/<kind>` (the optional fields of
   the `Node` struct itself, split across node kinds that can validly
   hold them: gate-only fields such as `person_only`, `approves` and the
   form fields on a `human_review`, `requires_approval` on an
   `agent_task` or a `publish`, and the rest on an `agent_task`).
2. A field needing context (a `profile`, a connector) names it. The
   validator runs with `ValidationContext::default()`, which has no
   connector facts and no project profile names: an auto or global
   profile reference is not checked for existence, and an explicit
   `scope: project` reference gets V14. Field fixtures use auto
   references; one document, `field/profile-scope-project`, freezes V14
   on purpose.
3. A document may get errors: they are frozen too (rule 2). A document
   with errors is allowed only when its `coverage.txt` line says which
   rule it protects; prefer a document with none.
4. `project_checks.junit` (one-point-oh-fixes) goes into
   `field/baseline_check`
   (`checks: [{ id: tests, project_checks: { only: [test], junit:
   "reports/*.xml" } }]`, plus what the block needs to be valid).
5. The fields of the last three changes are named in D1.6 item 1.

#### Coverage

`coverage.txt` has one line per frozen schema place:

```text
Playbook.deadline field/deadline deadline
NodeKind::wait.wait_for node/wait nodes[1].wait_for
WaitFor~inbox node/wait nodes[1].wait_for
InboxWait.take node/wait nodes[1].wait_for.take
Node.requires_approval node-common/agent_task nodes[1].requires_approval
CacheSpec~mode field/cache-mode defaults.cache
```

Four kinds of line, each followed by the document name and the typed
path where that document exercises it:

1. `<Type>.<field>`: every serialized field of every schema struct
   reachable from `Playbook`, nested structs reached only through an enum
   variant included (`InboxWait`, the `list:` block and the like);
2. `<Enum>::<variant>.<field>`: every field of every struct variant of
   an enum the schema reaches (`NodeKind`, `WaitFor` and every other enum
   the inventory finds), keyed by the variant's serde name;
3. `<Enum>~<variant>` and `<Type>~<form>`: every variant of an enum and
   every accepted alternative of an untagged or hand-deserialized value
   (`CacheSpec~mode`, `CacheSpec~object`, a profile reference as name and
   as mapping, `ContinueSession~short`, `ContinueSession~full`, every
   `WaitFor` and `NodeKind` variant);
4. the own fields of a struct that flattens another (`Node`, whose
   `kind` is flattened), as `<Type>.<field>` lines.

The inventory is checked from the types, never from the fixtures alone,
so a new field or variant fails until it has a line and a fixture. Test
helpers only:

```rust
/// The field names serde knows for a derived struct: a deserializer that
/// records what `deserialize_struct` is called with and fails.
fn fields_of<T: serde::de::DeserializeOwned>() -> &'static [&'static str];

/// Every variant of an enum by its serde name, from the error serde gives
/// for an unknown tag (the method of `event_types` in D1.2).
fn variants_of<T: serde::de::DeserializeOwned>(tag: &str) -> Vec<String>;

/// Every field name of the struct variant `value` holds, serialized and
/// skipped alike: a serializer that records each `serialize_field` and
/// each `skip_field` call of `SerializeStruct` and writes nothing.
fn variant_fields_of<T: serde::Serialize>(value: &T) -> Vec<&'static str>;
```

Tests:

1. `every_coverage_line_is_exercised`: the named document exists and has
   a value at the named path, both in the document as written and in
   `yaml_serde::to_value` of its parse (a default value the serializer
   drops fails here).
2. `every_derived_field_is_in_the_coverage`: for every schema struct in
   the test's `DERIVED_STRUCTS` list (each struct that derives its
   deserializer without `#[serde(flatten)]`, the ones reached only through
   an enum variant included), every field `fields_of` reports is a
   `<Type>.<field>` line. `fields_of` is implemented with a struct
   `Capture<'a>(&'a mut Option<&'static [&'static str]>)` implementing
   `serde::Deserializer<'de>` with `type Error =
   serde::de::value::Error`, `deserialize_struct` storing `fields` and
   returning `Err(Error::custom("captured"))`, `deserialize_any`
   returning `Err(Error::custom("not a struct"))`, and
   `serde::forward_to_deserialize_any!` for every other method. A struct
   that stops calling `deserialize_struct` makes `fields_of` panic with
   "no struct fields recorded"; the struct then moves to the flattened
   list of test 4.
3. `every_variant_field_is_in_the_coverage`: for every enum in the
   test's `SCHEMA_ENUMS` list (with its tag name), `variants_of` lists its
   variants; each variant is an `<Enum>~<variant>` line, and each must be
   held by at least one fixture value (the failure names a variant no
   fixture holds). For each fixture value of a struct variant, read from
   the parsed fixtures (`node.kind` for a node, never the `Node` itself,
   whose flattening hides the skipped fields), `variant_fields_of`
   returns the variant's whole field list, because the derived
   serializer of an internally tagged struct variant calls `skip_field`
   for every field `skip_serializing_if` leaves out; each name is an
   `<Enum>::<variant>.<field>` line. A newtype variant over a struct
   (`WaitFor::Inbox(Box<InboxWait>)`) is covered through that struct in
   `DERIVED_STRUCTS`.
4. `every_flattened_field_is_in_the_coverage`: for every struct that
   flattens (on 121e5c06 `Node` only), the test reads its declaration
   from `crates/apb-core/src/schema.rs` (the block from `pub struct
   <Name> {` to its closing brace, comments stripped), takes every
   field that is not `#[serde(flatten)]` and not `#[serde(skip)]`, maps
   it through its `#[serde(rename = "..")]` if any, and requires a
   `<Type>.<field>` line for each. A new `#[serde(flatten)]` anywhere in
   `schema.rs` fails this test until its struct is added to the list.
   A field marked `skip_serializing` (read but never written) is listed
   by hand in `HAND_FIELDS`, `(line, source symbol)`, and checked to be a
   coverage line.

Wrong implementations:

1. A writer that rewrites `errors` "to match": a new error on an old
   definition is the break the freeze exists to catch. Caught by the
   merge function's unit test `the_writer_never_changes_frozen_errors`
   (a frozen entry with `errors: ["V01"]` and a current report without
   it: the merge panics, nothing is written).
2. Reading the examples live instead of the frozen copies: an example's
   edit would change the expectation with it. Caught by rule 6 (the
   corpus holds bytes, and the test reads `examples/` only to find what
   is missing or changed) and by review.
3. Default values in field fixtures (`publish_only: false`): rule 3 then
   fails and the tempting fix is a `NORMALIZED` entry. Use a non-default
   value instead.

### D1.2 Journal fixture

File (new): `crates/apb-engine/tests/fixtures/journal/every_event.jsonl`,
one line per `EventPayload` variant, each exactly what
`serde_json::to_string(&Event { seq, ts, payload })` writes (as
`known_events.jsonl` is), `seq` from 0 upward in file order, `ts`
`1790000000000 + seq`. It is not a coherent run and is never read by
`read_all` (whose checkpoint and deferral rules would refuse it); it is
read line by line.

Why a second file and not `known_events.jsonl`: `known_events.jsonl`
(34 lines, 26 types) is one coherent run that `RunView` and resume tests
fold; a hundred unrelated events in it would break those folds.

The journal promise, stated as the tests enforce it: every frozen line
still decodes, and its decoded event serializes again to the same JSON
value (compared as parsed `serde_json::Value`s, so the order of members
is free); a field added to a frozen event has `#[serde(default)]` and a
`skip_serializing_if` for its default, in nested frozen types too, so it
adds no member to a frozen line. Fields that exist when the freeze is
written are frozen as they serialize then. `docs/adr/0001-new-events-are-safe-to-skip.md`,
the conventions of `CLAUDE.md` and `AGENTS.md` and `docs/COMPATIBILITY.md`
say so.

Tests, added to `crates/apb-engine/tests/suite/journal_compat_test.rs`
(after `a_journal_without_unknown_events_round_trips_byte_for_byte`),
with `const EVERY: &str = include_str!("../fixtures/journal/every_event.jsonl");`:

1. `every_event_type_has_a_frozen_line`: the event types this binary
   knows, from serde (the same error `event::is_known_kind` reads):

```rust
/// Every `type` tag of `EventPayload`, as serde lists it in the error for
/// a tag no variant has ("unknown variant `x`, expected one of `a`, `b`").
fn event_types() -> Vec<String> {
    let e = serde_json::from_value::<EventPayload>(serde_json::json!({ "type": "__apb_freeze_no_such_event__" }))
        .unwrap_err()
        .to_string();
    let list = e.split_once("expected one of ").expect("serde lists the variants").1;
    list.split(", ").map(|s| s.trim().trim_matches('`').to_string()).collect()
}
```

   each must be the `type` of a line of `EVERY`; the failure names every
   missing type. If a `#[serde(alias)]` ever appears on a variant, the
   alias is listed in a `const ALIASES` the test skips, with a reason.
2. `every_frozen_line_is_read`: each line decodes as `Event`
   (`serde_json::from_str`) without error; the failure names the line's
   `seq` and `type`.
3. `every_frozen_line_keeps_its_value`: for each line,
   `serde_json::to_value` of the decoded `Event` equals
   `serde_json::from_str::<serde_json::Value>` of the line. Equal values
   mean no member was renamed, dropped or added (a new field written at
   its default adds a member and fails here); the order of members is not
   compared, the written promise names names and shapes, not order. The
   failure prints the members that differ.
4. `every_frozen_line_is_a_known_event`: each line's `type` is in
   `event_types()` (a removed variant fails here, not as "unknown event
   skipped").
5. `frozen_checkpoints_stay_checkpoints`: `const CHECKPOINTS: &[&str]`
   lists the types whose frozen line `EventPayload::is_checkpoint()` on
   the tree the freeze is written from (on 121e5c06: `node_finished`,
   `run_paused`, `run_finished`, `run_aborted`, `follow_up_observed`);
   for every frozen line whose type is in `CHECKPOINTS`, `is_checkpoint()`
   is true. A type outside `CHECKPOINTS` may become a checkpoint (that is
   safe under the skip rule: it only shortens what an older binary must
   skip), so the test never asserts `false`; a new checkpoint is added to
   `CHECKPOINTS` by hand in the change that makes it one.

#### Writing the lines

No writer: each line is checked by a person. For every type rule 1
reports missing:

1. find a construction of the variant (`rg -n "EventPayload::<Variant> \{"
   crates/apb-engine`), build the smallest value with every `Option`
   field `Some` and every `Vec` field non-empty where the type allows (so
   the line freezes the field names), in a temporary unit test that
   prints `serde_json::to_string(&Event { seq, ts, payload })`;
2. paste the printed line, delete the temporary test;
3. keep lines in variant declaration order (the order of
   `event_types()`).

Wrong implementations:

1. Generating the file from `event_types()` with `{"type": t}` lines:
   most variants have required fields, so those lines do not decode, and
   the ones that do freeze no field. Caught by rules 2 and 3 (a bare line
   does not decode, or serializes with members it lacks) and by review of
   the field coverage.
2. Folding `EVERY` with `read_all` "to be realistic": refused by the
   checkpoint rules for an unrelated sequence. Rules 2 and 3 read per
   line.
3. Comparing the line strings byte for byte: a harmless change of member
   order fails while every name and value is kept. Rule 3 compares parsed
   values.

#### The released binary corpus

`crates/apb-cli/tests/suite/journal_corpus_test.rs`:

1. `const FREEZE_READER: &str = "v1.0.0-beta.7";` next to `BETA1` and
   `V025`: the newest published release when this change starts (no
   earlier change of the plan bumps the version, so it is beta.7 unless
   a patch release was tagged in between: then that tag).
2. Two rows after `row2_beta1_writer_new_reader`, in its style:
   `rowf1_freeze_reader_writer_new_reader` (the freeze reader writes a
   run waiting at review, `case.write_waiting_at_review(&old)`; the new
   binary `case.read_resume_stop(&new_binary(), &run)`) and
   `rowf2_new_writer_freeze_reader` (the new binary writes, the freeze
   reader reads, resumes to the end and stops). Each starts with `let
   Some(old) = old_binary(FREEZE_READER, &case) else { return; };`.
3. The module doc names the third reader and rows F1, F2.
4. `.github/workflows/ci.yml` job `journal-corpus`:
   `OLD_APB_TAGS: v0.25.0 v1.0.0-beta.1 v1.0.0-beta.7`; the comment above
   the job names the new tag.

The rc.1 and 1.0.0 readers come with release-1.0.0, after their tags
exist (proposal "Out of scope" item 3).

### D1.3 MCP tools

File (new): `crates/apb-mcp/tests/fixtures/mcp-tools-1.0.json`:

```json
{
  "format": 1,
  "tools": {
    "playbook_run": {
      "read_only": "removed",
      "input": {
        "types": ["object"],
        "required": ["id"],
        "properties": {
          "id": { "types": ["string"] },
          "params": { "types": ["null", "object"], "properties": {}, "required": [] },
          "scope": { "types": ["null", "string"] },
          "budget": { "types": ["null", "object"], "required": [], "properties": {
            "tokens": { "types": ["integer", "null"], "format": "uint64", "minimum": 0 }
          } }
        }
      },
      "base": { "id": "x" },
      "examples": {},
      "boundaries": { "budget.tokens": [0, 18446744073709551615] }
    }
  }
}
```

`tools` keyed by tool name, sorted. `read_only` is `full`, `restricted`
(`READ_ONLY` with `ReadOnlyUse::Full` or `ReadOnlyUse::Restricted`,
`crates/apb-mcp/src/server/read_only.rs`) or `removed`
(`REMOVED_IN_READ_ONLY`). `input` is the tool's normalized input
schema:

```rust
/// The accepted input of a JSON schema, normalized: local `$ref`
/// resolved against the tool's root schema; the set of accepted types,
/// `null` included; for a union (`anyOf`, `oneOf`, a type array) every
/// alternative, each normalized; for an object its properties
/// (recursively) and its required members; for an array its item
/// schema; and every constraint that narrows the accepted input (the
/// list below). A construct the normalizer does not know fails the
/// extraction; `{"types": ["any"]}` is recorded only for a schema with
/// no constraint at all.
fn normalize(schema: &serde_json::Value, root: &serde_json::Value) -> serde_json::Value;
```

The narrowing constraints recorded, each where the schema has it:
`minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`,
`multipleOf`, `format` on an `integer` or `number` schema (schemars
writes the integer width there: `uint64`, `uint32`, `int64` and the
like), `enum`, `const`, `minLength`, `maxLength`, `pattern`,
`minItems`, `maxItems`, `uniqueItems`, `minProperties`,
`maxProperties` and `additionalProperties` (its value, normalized when
it is a schema). Descriptions, titles, defaults, examples and the
`format` of a string are never recorded: they do not narrow what a
tool accepts, and every doc edit would break the freeze.

`base` is one minimal argument object the tool accepts: its required
members only, each the simplest accepted value of its frozen schema (a
string `"x"`, a number at its minimum or 0, `false`, an enum's first
value, an object of its own required members). The writer synthesizes
it from the frozen schema; the tests below deserialize it.

`boundaries` holds, for every argument path (dotted, `[]` for array
items) whose frozen schema is a number or an enumeration, the values at
its edges: the frozen `minimum` (or 0 for an unsigned format) and the
frozen `maximum` (or the largest value of the frozen format: 255,
65535, 4294967295 or 18446744073709551615 for `uint8` to `uint64`, the
signed ranges for `int8` to `int64`), and every value of an `enum` or
the `const`. The writer computes them from the frozen schema when the
tool or argument is added and never changes them.

`examples` holds, for every argument whose normalized schema is a union
(`confirm_irreversible`, `notify`, `budget` and the rest the extraction
finds), one accepted JSON value per alternative, keyed by argument name.

Tests, a new `crates/apb-mcp/src/server/freeze_tests.rs` (`#[cfg(test)]
mod freeze_tests;` in `server/mod.rs` next to `mod read_only_tests;`),
over `WfMcp::new(PathBuf::from(".")).tool_router.list_all()` (what
`tools/list` answers; the same call `read_only_tests.rs` makes):

1. `every_frozen_tool_is_still_served`.
2. `every_frozen_input_is_still_accepted`: the current normalized schema
   accepts a superset of the frozen one, compared recursively: every
   frozen accepted type, union alternative, property and array item stays
   accepted, and no constraint narrows. A bound may only widen (a
   `minimum` may fall or vanish, a `maximum` may rise or vanish, the same
   for the exclusive bounds and the length, item and property counts);
   an `enum` may only gain values, and a `const` may only become an
   `enum` holding it or vanish; `pattern`, `multipleOf` and
   `uniqueItems: true` may only stay as frozen or vanish;
   `additionalProperties: false` may become a schema or vanish, never the
   reverse; an integer `format` may only stay or widen along the
   test's `WIDENINGS` table (`uint8` to `uint16` to `uint32` to `uint64`,
   `int8` to `int16` to `int32` to `int64`, and `uintN` to `intM` for
   M greater than N). The failure names the tool, the path and the
   constraint.
3. `no_frozen_object_gains_a_required_member`: at every object of the
   frozen schema, the tool's top level included, the current `required`
   is a subset of the frozen one.
4. `every_union_example_still_deserializes`: each frozen example, placed
   at its argument in a copy of `base`, is accepted by the current
   normalized schema and deserializes through the tool's own argument
   type (the `Parameters<..>` type its handler takes, called directly).
   The test maps each tool to that type in `ARG_TYPES: &[(&str, fn(serde_json::Value) -> Result<(), String>)]`,
   one hand-written line per tool; a served tool missing from the table
   fails, naming it.
5. `every_boundary_value_is_still_accepted`: `base` itself, and each
   frozen boundary value placed at its path in a copy of `base` (the
   objects on the way built from their own frozen required members),
   deserializes through the tool's argument type from `ARG_TYPES`. This
   is the check that a narrower Rust type (`u64` to `u32`) refuses a value
   1.0 accepted, even where the schema text would hide it.
6. `a_frozen_read_only_tool_stays_read_only`: frozen `full` stays
   `full`; frozen `restricted` is `restricted` or `full`; frozen
   `removed` may become anything.
7. `every_served_tool_and_argument_is_frozen`: a tool, or an argument
   of a frozen tool, the server has and the file lacks fails with "new
   tool `<name>`" or "new argument `<tool>.<arg>`" and the command
   `APB_WRITE_FREEZE=1 cargo test -p apb-mcp --lib
   server::freeze_tests::write_freeze`. This makes every new 1.x tool
   and argument join the freeze in the change that adds it.

The writer (D0): for a frozen tool, adds its new properties and union
alternatives, new examples and the boundaries of new arguments, sets each
`required` to the current list (a subset, by rule 3), each constraint to
the current one (a widening, by rule 2) and `read_only` to the current
class (allowed by rule 6); never changes `base` or a frozen boundary;
adds every new tool whole with its `base`, examples and boundaries;
sorts.

Wrong implementations:

1. A writer that copies the current `tools/list` over the file: a
   removed argument disappears from the freeze with it. Caught by the
   merge function's unit test `the_writer_refuses_a_removed_argument` (a
   frozen tool with an argument the current one lacks: the merge panics,
   nothing is written).
2. Recording one type word per argument: a union or an object argument
   could change its accepted input and still pass. Rule 2 compares the
   normalized schema.
3. Recording types and members without bounds: `tokens` narrowed from
   `u64` to `u32` keeps its type word `integer` and refuses a value 1.0
   accepted. Rule 2 compares `format` and the bounds, rule 5 sends the
   frozen edge values through the argument type.

### D1.4 Validator codes

1. `crates/apb-core/src/validate/codes.rs` (new, `pub mod codes;` in
   `validate/mod.rs`):

```rust
/// Every active validator code: the code, the severities it may be
/// reported at, and a one-line meaning. The validator emits no code that
/// is not here (a test proves it).
pub const CODES: &[(&str, &[Severity], &str)] = &[
    ("V01", &[Severity::Error], "the playbook has no start node"),
    // ...
];

/// Validator codes that no longer fire. A retired code keeps its number:
/// no new rule may take it (docs/COMPATIBILITY.md).
pub const RETIRED_CODES: &[(&str, &str)] = &[
    ("V15", "never emitted"),
    ("V16", "declared but not enforced isolation; isolation is enforced since 1.0.0-beta.1"),
];
```

   `CODES` covers every code family of `apb-core`: the playbook
   validator (`validate/`, `preflight.rs`, `untrusted.rs`,
   `connector/resolve.rs`), the eval suite rules (`eval.rs`), the
   scenario rules (`scenario/validate.rs`) and the start rule rules
   (`trigger/validate.rs`, V260 to V267). A code reported at more than
   one severity lists each. No earlier change of the 1.0 plan retired a
   code (their archived designs name none), so V16 is the only code that
   fired and was retired. V15 is listed retired too, with the reason
   "never emitted": a stale comment names it beside V14 and V16, so its
   number is kept from reuse like V16's. V248 and V282, reserved by
   earlier changes and never used, are in neither table. The comment in
   `validate::validate` that says V16 is retired points to
   `RETIRED_CODES` and says V15 is listed there as never emitted; its
   stale `// V14, V15` becomes `// V14`. The module doc of
   `validate/isolation.rs` names `RETIRED_CODES` too.
2. A unit test in `codes.rs`,
   `the_registry_is_what_the_validator_can_emit`, scans the production
   sources of `crates/apb-core/src` from disk (`codes.rs` itself left
   out; each file cut at its first `#[cfg(test)]`; comments stripped):
   1. every `"V<n>"` string literal is a code of `CODES` or
      `RETIRED_CODES`, and every code of `CODES` appears as such a
      literal at least once;
   2. the first argument of every `.error(` and `.warn(` call and the
      `code` of every `Issue {` literal is a `"V<n>"` literal or a
      constant whose value is one (`pub const V260: &str = "V260";` in
      `trigger/validate.rs`); any other expression fails the test;
   3. `CODES` and `RETIRED_CODES` are disjoint and each is sorted by
      number without repeats.
3. `crates/apb-core/tests/fixtures/schema-1.0/validator-codes.txt`:
   one line per code, `<code> <severities> active` or `<code> retired`,
   sorted by number, written from `CODES` and `RETIRED_CODES` by the
   schema writer. A line is never removed.
4. `crates/apb-core/tests/fixtures/schema-1.0/witnesses.yaml` (by
   hand): one witness for every active code that no document of the
   corpus or the fields fires (a rule that needs a validation context, a
   playbook directory, an eval suite, a scenario or a start rule
   document). A witness is executed, never looked up by name. Each
   document carries the D1.1 marker with the name `witness/<code>` and
   two header comments, `# apb-witness-kind: <kind>` and
   `# apb-witness-expects: <code> <severity>`, then its input. The kinds,
   each run through the checker that owns the code:
   1. `playbook`: the document is a playbook, validated with
      `validate::validate` and the `ValidationContext` the header
      `# apb-witness-context: <name>` names from the test's
      `WITNESS_CONTEXTS` table (connector facts, project profile names
      and the like, built in Rust);
   2. `playbook-dir`: the document is a playbook, written with the extra
      files listed after it (each under a `# apb-witness-file: <path>`
      marker) into a temporary version directory, and checked by the
      directory checks that own the code (`preflight::missing_scripts`,
      `eval::validate_suite`, `scenario::validate::validate_all`);
   3. `scenario`: the document is a scenario file checked by
      `scenario::validate::validate_scenario` against the playbook the header
      `# apb-witness-playbook: <document name>` names;
   4. `trigger-rule`: the document is a start rule document read by
      `trigger::parse_rule_document` and checked by `trigger::check_rule`
      with a `RuleContext` from `WITNESS_CONTEXTS`.
   A code whose input cannot be written as data gets a Rust witness in
   the test, `fn witness_v<n>() -> Vec<Issue>`, registered in
   `WITNESS_FNS: &[(&str, Severity, fn() -> Vec<Issue>)]`; it builds its
   input and calls the owning checker, never a copy of its logic.
5. Tests in `schema_freeze_test.rs`:
   1. `every_frozen_code_is_still_known`: each line of
      `validator-codes.txt` is in `CODES` with the same severities, or
      in `RETIRED_CODES`. Severity is frozen here, so a code that
      changes severity fails without a document.
   2. `every_known_code_is_frozen`: every code of the two tables is a
      line (a new code fails with the writer's command).
   3. `a_retired_code_never_fires`: no document of the corpus or the
      fields gets a code of `RETIRED_CODES`.
   4. `every_active_code_has_a_witness`: each code of `CODES` is fired
      by some corpus or field document at a severity `CODES` lists, or
      has a witness in `witnesses.yaml` or `WITNESS_FNS`.
   5. `every_witness_fires_its_code`: each witness is executed as its
      kind says, and the issues it returns hold its code at its expected
      severity; a witness that returns no such issue fails, naming the
      witness and what it returned. A witness for a code not in `CODES`,
      or for a retired code, fails too. So V263 removed from
      `trigger::check_rule`, or reported as a warning, fails here although
      every test name still exists.

Wrong implementations:

1. Deleting a retired code's literal and its `validator-codes.txt` line
   together: test 5.1 passes. Caught by the writer never removing a line
   (D0) and by review; `docs/COMPATIBILITY.md` says a line is never
   removed.
2. A witness that names a test function and checks the name exists: the
   test may stop firing the code while its name stays. Test 5.5 runs the
   witness and asserts the code and the severity.

### D1.5 Machine names and files

What 1.x keeps beyond the schema, the journal, the tools and the
validator codes. Each part has a frozen fixture and a test in the crate
that owns the type, and joins the writer of its group (D0).

The meaning rule, one for every frozen file of this section (run files,
person files, signed stores, the playbook store): reading and keeping
keys is not enough, because a reader can parse a file while it reads a
field differently. So:

1. every fixture holds nondefault state: each optional member it can hold
   set, each flag at its non-default value, each list non-empty, chosen
   so that a reader that ignored the member would decide otherwise;
2. every fixture document has a hand-written expectation in its test, a
   function `expect_<document>(&<typed value>)` (or a table entry of
   `(document, field path, expected value)` where that reads better),
   asserting named typed values of the parsed value (never only that it
   parsed), and the decisions its reader makes from them, named per part
   below;
3. a document without an expectation fails (`every_document_has_an_expectation`),
   so a document the writer adds fails until a person writes its
   expectation; the writer never writes an expectation.

Parts:

1. Machine codes: `crates/apb-mcp/tests/fixtures/machine-codes-1.0.txt`,
   one line `<group> <code>`, sorted. A frozen group lists the values its
   production constants and functions export, including values that reach
   a result through a variable or a constant of another crate. The source
   of truth is the production enumeration below; the scan of literals is
   only the completeness check. The groups:
   1. `reason`: `isolation::contract::ALL_REASONS` through `id()`;
   2. `isolation`: `isolation::contract::ALL_CODES` through `id()`;
   3. `publish`: `publish::Refusal::ALL` through `code()`;
   4. `caller`: every variant of `decision_channel::CallerRefusal`
      through its serde spelling;
   5. `start_action`: every variant of
      `next_action::start_action::StartActionCode` through its serde
      spelling;
   6. `guard`: `GuardedFeature::ALL` through `as_str()`; each frozen
      name must also parse back through `GuardedFeature::parse`;
   7. `gate`: the codes a start, trial, eval or resume refusal carries,
      produced in `crates/apb-engine/src/gate/`;
   8. `mcp`: the codes and flags an MCP result or refusal carries,
      produced in `crates/apb-mcp/src` or reaching it from another crate;
   9. `patch`: the codes `run_origin::patch_refusal` returns
      (`patch_global_unsupported`, `patch_origin_unreadable`).

   Groups 1 to 6 are typed: the test maps each production value to its
   spelling and fails on a missing or a changed one. Groups 7 to 9 are
   listed in a checked table in `freeze_tests.rs`:

```rust
/// `(group, code, source symbol)`: every machine code of the scanned
/// groups. Where a constant exists, the code is that constant itself
/// (`apb_engine::start_key::CONFLICT`), so the table reads the value the
/// production code exports; a code with no constant (a JSON member name,
/// a literal inside a function) is a literal here and its source symbol
/// names the function that produces it.
const MACHINE_CODES: &[(&str, &str, &str)] = &[
    ("mcp", apb_engine::start_key::CONFLICT, "apb_engine::start_key::CONFLICT"),
    ("mcp", "already_started", "apb_mcp::tools::run::already_started"),
    // ...
];
```

   Tests:
   1. `every_frozen_machine_code_is_still_produced`: each line of the
      fixture is a value of its typed source (groups 1 to 6) or an entry
      of `MACHINE_CODES` (groups 7 to 9); the failure names the group and
      the code.
   2. `every_scanned_code_is_in_the_table`: the completeness check. The
      test scans the production sources (test modules cut, comments
      stripped) of `crates/apb-engine/src/gate/` for group `gate`,
      `crates/apb-mcp/src` for group `mcp` and
      `crates/apb-engine/src/run_origin.rs` for group `patch`, and finds:
      the value of every `pub const <NAME>: &str = "<code>"`; every
      `"policy"`, `"error"` and `"code"` member of a `json!` literal,
      whose value is a literal or a path to a constant (the constant
      resolved by its name in any crate of the workspace, wherever it is
      defined); and the leading `<code>` of every refusal message built as
      `format!("{CONST}: ...")` or `"<code>: ..."`. Every code found must
      be in `MACHINE_CODES` under its group; a member whose value is
      another expression (a variable, a call) fails with "list the codes
      `<file>` `<function>` can return in MACHINE_CODES", so a value that
      reaches a result through a variable is listed by hand, with its
      source symbol, once.
   3. `every_table_code_is_frozen`: every entry of `MACHINE_CODES` and
      every typed value is a fixture line; a new one fails with the
      writer's command.
   4. `every_table_source_exists`: each source symbol of a literal entry
      names a function or constant found in the production sources (a
      name check is enough here: the value itself is asserted by the
      result tests of part 5 where a host reads it).

   `start_key_conflict` (through the constant) and `already_started` (the
   flag member of `apb_mcp::tools::run::already_started`'s answer) are in
   the frozen list. A code that only the CLI or the dashboard returns
   through shared code is frozen anyway when the scan finds it (a frozen
   spelling costs nothing).
2. Run persistence: `crates/apb-engine/tests/fixtures/run-files-1.0/`
   `manifests.yaml` and `run-configs.yaml`, multi-document files with
   the D1.1 marker, holding the `manifest.yaml` and `run.yaml` of six
   runs the writer starts with the engine test suite's stand-in agent,
   each with nondefault state:
   1. a plain run with a budget, a notify request, remembered input off
      and `external_data: true`;
   2. a guarded run (a `deadline` and a `concurrency:` block);
   3. a run with connector grants and an inbox wait;
   4. an unattended run;
   5. a run started by a start rule (`launch.triggered_by` set, with its
      connector and account);
   6. a child run (its parent and its inherited consent set).
   Tests in `persistence_freeze_test.rs`: each document, written to a
   temporary run directory, is read by `manifest::read` or
   `run_config::read_run_config`, keeps every key under the D1.1 rule 3
   comparison (with its own empty `NORMALIZED` table), and meets its
   expectation: named fields of `RunExecutionManifest` and `RunConfig`
   (the launch record, `triggered_by.rule`, the grants, the execution
   mode, the params, the budget, the deadline, the parent) and the
   decisions read from them: `RunOrigin::of_read`,
   `external_data::is_flagged`, the unattended mode the drive reads from
   the manifest, the deadline in force (`run_deadline`) and the run limits
   (`run_view::run_limits`).
3. Files a person or an installer keeps, under
   `crates/apb-core/tests/fixtures/files-1.0/`, multi-document files
   with the marker:
   1. `profiles.yaml`: one `profile.yaml` per executor kind (by hand),
      read by `ProfileDoc::from_yaml` and checked by
      `ProfileDoc::problems`;
   2. `connectors.yaml`: every official connector's `connector.yaml`
      under `connectors/` as it is (copied by the files writer, the
      corpus rules of D1.1 rule 6 applied: name `connectors/<name>@<its
      version>`, bytes compared), plus one hand-written manifest per auth
      kind and function form the official ones lack (an `mcp:` block
      over stdio and over http among them), read by
      `ConnectorDoc::from_yaml`;
   3. `connector-accounts.yaml`: connector account files of both scopes,
      each document named `global/<connector>` (the
      `<config dir>/connector-config/<c>.yaml` shape,
      `connector::config::global_config_path`) or `project/<connector>`
      (`.apb/connector-config/<c>.yaml`,
      `connector::config::project_config_path`), one pair or single per
      case. The test places each document at its path under a temporary
      `APB_CONFIG_DIR` and a temporary project, then reads it through
      `connector::config::load_merged`. The cases: one account per auth
      kind in each scope; a global-only account (`{{cmd:...}}` in a field,
      which only the global file may use); a project account replacing a
      global one of the same name; a project `default: true` that clears
      a global default; and a project account with a `{{cmd:...}}` field,
      which `load_merged` refuses. The expectations assert the merged
      list (names in order, the `default` flags, the fields of each
      account) and the refusal;
   4. `settings.yaml`: a `policy.yaml` with every section
      (`policy_file::parse`), a `config.yaml` with every section of
      `GlobalConfig` (`GlobalConfig::load` under a temporary
      `APB_CONFIG_DIR`) and a `decisions.yaml` with every provider kind
      (`decisions::load_file`);
   5. `trigger-rules.yaml`: one start rule document per source form and
      filter shape (by hand), read by `trigger::parse_rule_document`
      (re-exported from the private `trigger::rule`); every rule document
      must pass `trigger::check_rule` without an error, so no expectation
      of codes is needed, and the V260 to V267 witnesses stay in
      `witnesses.yaml`.
   Tests in `files_freeze_test.rs`: each document parses, keeps its keys
   under the D1.1 rule 3 comparison, and meets its expectation (the
   meaning rule): for `policy.yaml` the billing, launch, runs, health,
   unattended and notify values and the effective settings they yield;
   for a profile agent, model, fallbacks and `process_env`; for a
   connector auth kind and function names; for an account file the merged
   list above; for a start rule its source, filter, key template and
   params.
4. Signed stores: `crates/apb-engine/tests/fixtures/signed-1.0/stores.json`,
   `{"<kind>": {"path": "<path under the project or config dir>",
   "bytes": "<file text>"}}`, one entry per `signed_store::SignedDoc`
   implementor: start keys (`start_key.rs`), concurrency slots
   (`concurrency/slots.rs`), routine health (`health/state.rs`),
   probation (`probation.rs`), known problems (`known_problems/store.rs`),
   corrections (`corrections/store.rs`), start rules
   (`trigger::RulesFile`) and start rule state
   (`trigger::TriggerStateFile`). The test seam for the
   installation key is the key file itself: every MAC of a signed store
   goes through `run_origin::tag` and `run_origin::verify_tag`, which
   read `<config dir>/run-origin.key`, and the config dir is
   `APB_CONFIG_DIR`. The test writes a fixed 64-character key there
   (`const TEST_KEY` in the test), and the writer creates each store
   through its own public write function under that key, with
   nondefault state (a start key bound to a run and one pending; a slot
   with an active and a waiting run; a health state on hold with a
   notice in its ledger; probation entries that count and one that does
   not; a known problem `closed_proposed` with a recheck record; a
   correction `proposed` with a draft; a rule enabled and approved; a
   rule state positioned past an event), and records the bytes. Test:
   each frozen file, placed at its path, still verifies, reads through
   the store's own reader, and meets its expectation: the named entries
   and the decision each reader makes (`start_key::lookup` answers the
   bound run; the slot names the active and the waiting run;
   `health::state::hold_refusal` refuses with `playbook_on_hold`;
   `probation::counted` returns the frozen count; the known problem's
   status and the correction's status read as frozen; the rule reads
   enabled and approved under its digest; the state's position is the
   frozen one). Files signed by other means (grade files, the discovery
   result store, remembered run input) are not covered.
5. MCP results: `crates/apb-mcp/tests/fixtures/mcp-results-1.0.json`,
   `{"format": 1, "tools": {"<tool>": {"<variant>": {"keys": [...]}}}}`,
   the sorted result keys (dotted paths into the result object, arrays
   by `[]`) of each documented result variant of each tool a host is told
   to consume. The tools are every tool `docs/HOST-INTEGRATION.md` or the
   server's own instructions (`crates/apb-mcp/src/instructions.rs`) tell
   a host to call or read: `playbook_catalog`, `playbook_run`,
   `playbook_capture`, `suggestion_dismiss`, `playbook_interview`,
   `playbook_howto`, `profile_list`, `profile_howto`, `projects_list`,
   `run_wait`, `run_status`, `supervisor_wait_event`, `review_decide`,
   `run_answer`, `run_resume`, `run_task_submit` and `decision_ask`. The
   variants, each produced from a fixture run in the shape the existing
   `server/tests` helpers use:
   1. `run_wait`: `finished`; `needs_input` with `needs` `question`
      (`pending_question`), `review` (`pending_review`), `host_task`
      (`pending_tasks`, each task's documented members) and `supervisor`
      (`pending_supervisor`); `stopped` paused (`pause`), parked at a
      wait, released (`released`), with a dead driver, and budget paused;
      `timeout`;
   2. `supervisor_wait_event`: a wake (`wake`, its `trigger`, `node`,
      `detail`), a timeout, with `pending_review`, with
      `pending_supervisor` and with `pending_tasks`;
   3. `run_status`: a running run, a run waiting at review, a run with
      host tasks, a finished run (`attempt_models` among its keys);
   4. `playbook_run`: started; refused for trust with `consent_nonce`;
      `already_started`; refused `start_key_conflict`;
   5. `run_task_submit`, `review_decide`, `run_answer`, `run_resume`:
      accepted, and each documented refusal;
   6. `decision_ask`: answered, and refused (`refused`, `reason`);
   7. `playbook_catalog`: with matches, and with `suppressed_suggestions`;
   8. `playbook_capture`, `suggestion_dismiss`, `playbook_interview`,
      `playbook_howto`, `profile_list`, `profile_howto`, `projects_list`:
      their one documented result.
   Tests: `every_frozen_result_key_is_still_returned` (each frozen key of
   each variant is in the current result of that variant; the failure
   names the tool, the variant and the key), `every_variant_is_produced`
   (each frozen variant has a producer in the test) and
   `every_returned_key_is_frozen` (a new key or variant fails with the
   writer's command). Values are not frozen except the machine codes of
   part 1. A result of a tool not listed here, and a variant no document
   names, are named under "Not covered".
6. The playbook store: 1.x reads a playbook store written by 1.0 (the
   owner's decision). `crates/apb-core/tests/fixtures/store-1.0/store.json`,
   `{"project": {"<path under .apb/playbooks>": "<file text>"},
   "config": {"<path under the config dir>": "<file text>"}}`, written by
   the store writer (`store_freeze_test::write_freeze`) through the
   public write functions under a temporary project and `APB_CONFIG_DIR`,
   with nondefault state:
   1. the versions of a playbook: three versions
      (`<id>/<version>/playbook.yaml`, through `versioning::create_version`
      and `versioning::save_definition_by`), a lifecycle file `retired`
      on a second playbook (`trust::write_lifecycle`);
   2. the `current` pointer with its history: `current` moved twice
      through `versioning::current::move_current` (once by a person with a
      reason, once superseding a candidate), so `current-history.jsonl`
      holds `HistoryEntry` lines chained by `prev`; and a `candidate`
      pointer (`candidate_pointer::CANDIDATE_FILE`);
   3. the provenance sidecar `meta/<version>.yaml` (`VersionProvenance`):
      `created_by` values `template`, `supervisor` and `import`, a forward
      patch with `scope`, `base_version`, `rationale`, `evidence` and a
      `trial` record, an `imported` provenance and a `shipped_version`;
   4. the trust store `<config dir>/trust.json` (`TrustStore`, written
      through `TrustStore::approve` and its kind variants): approvals of a
      playbook, a profile bundle, a connector and an account, of different
      `OriginKind`s;
   5. the launch floor store `<config dir>/launch-floors.json`
      (`launch::floor::FLOORS_FILE`, written through
      `launch::floor::set_by_person`): a `FloorKey::Project` and a
      `FloorKey::Global` record at two different policies.
   An interrupted move's intent file (`current.intent.json`) is transient
   and not frozen. Tests in `store_freeze_test.rs`: the files, placed at
   their paths, read through their readers, and meet their expectations
   (the meaning rule): `versioning::list_versions_with_provenance` lists
   the versions in order with `is_current` and `is_candidate` as frozen;
   `versioning::read_provenance` returns the named fields of each sidecar
   (`created_by` among them); `versioning::current::history` returns the
   frozen entries with `incomplete` and `pending` false, and
   `versioning::current::settled` the frozen `current` and `last_move_by`;
   `read_current_pointer` (`versioning/sidecar.rs`) and `candidate_pointer::read_candidate`
   the frozen versions; `trust::read_lifecycle` `Retired`;
   `TrustStore::load` with `status` (`Approved` for a frozen digest,
   `Changed` for another digest of a frozen id, `Unapproved` otherwise)
   and `origin`; `launch::floor::read_from` the frozen records and
   `launch::floor::floor_in` the frozen policy for each key. The same
   `every_document_has_an_expectation` rule holds per file. The writer
   only adds files (a new store file kind joins with its expectation),
   never rewrites a frozen file.

Not covered, said in `docs/COMPATIBILITY.md` "## Not covered": scenario
files, eval cases and suites; the dashboard's HTTP routes and payloads
(also where they share code with MCP); the CLI's text and JSON output;
`diagnostics.jsonl`; the generated TypeScript; the Rust API of the
crates; the results of MCP tools part 5 does not list, and result
variants no document names; the values (not the keys) of a frozen
result other than the machine codes of part 1; files signed by other
means than `SignedDoc` (part 4); the intent file of an interrupted
`current` move; and the files D1.6 items 5 and 6 name.

### D1.6 What the last three changes bring

The rules of D1.1 to D1.5 decide each name below. Where they did not,
the rule written here decides it; the report of this pass names those.

1. inbox-waits:
   1. schema: `WaitFor::Inbox` (`wait_for: { type: inbox, connector,
      account, match, types, take }`) and every field of `InboxWait`:
      `WaitFor~inbox` and the `InboxWait.<field>` lines in
      `coverage.txt`, exercised by `node/wait` or a
      `field/wait-inbox` document (D1.1, coverage rule);
   2. journal: the event `inbox_wait_read`, not a checkpoint, gets its
      frozen line; `wait_started`, `wait_signalled`, `wait_timeout` and
      `inbox_gap` exist already and get theirs (D1.2 rule 1);
   3. validator: V284 to V287 into `CODES`; V286 is a warning (D1.4);
   4. guard: no new name; `wait_park` is frozen with the other 14
      (D1.5 item 1.6);
   5. run persistence: the inbox wait's connector, account and site in
      the manifest's `connectors` and `connector_grants`, frozen through
      the run with an inbox wait (D1.5 item 2);
   6. MCP: no new tool or argument; `run_wait`'s park text is a value,
      not a key, so it is not frozen.
2. templates-and-global-playbooks:
   1. CLI: `apb templates list|show|install [--scope project|global]
      [--as <id>]` and `apb run --scope project|global` are CLI surface,
      outside the promise (proposal "Out of scope" item 5);
   2. HTTP: `GET /api/templates`, `GET /api/templates/{id}`, `POST
      /api/templates/{id}/install` and the `scope` of the playbook list
      and detail are dashboard routes, outside the promise;
   3. refusal codes: `patch_global_unsupported` and
      `patch_origin_unreadable` are returned by MCP
      `supervisor_patch_playbook` and frozen (group `patch`);
      `template_in_run_step` is found by the `mcp` scan and frozen;
      `supervise_global_unsupported` is a constant of `apb-cli` only,
      outside every scanned source, so it is not frozen (MCP's own
      refusal for the same case, `supervise_self_global_unsupported`,
      is in the `mcp` group);
   4. the corpus: the 15 examples are frozen as corpus documents
      whether or not they are templates; template eval scenarios are
      scenario files, not covered;
   5. `created_by: template` in a version's provenance sidecar: frozen
      through the playbook store fixture (D1.5 item 6), which holds a
      version written with it.
3. start-rules:
   1. MCP: the tools `trigger_list` and `trigger_test` (both
      `ReadOnlyUse::Full`) join the tool freeze with their arguments
      (D1.3 rule 7);
   2. validator: V260 to V267 into `CODES`, witnessed through
      `witnesses.yaml` (a start rule is not a playbook);
   3. machine codes: the gate's `launch_requires_rule_approval`,
      `binding_changed` and `billing_refused_for_trigger` (group `gate`);
      `trigger_rules_unreadable`, `trigger_rule_not_found` and
      `trigger_test_refused_in_run_step` (group `mcp`); the
      `already_started` flag and the `start_key_conflict` refusal (group
      `mcp`, listed in `MACHINE_CODES` with their source symbols
      `apb_mcp::tools::run::already_started` and
      `apb_engine::start_key::CONFLICT`, because the scan of `apb-mcp`
      sees neither as a literal: the refusal's `policy` is the engine's
      constant, and the flag is a member name);
   4. run persistence: `launch.triggered_by` (`launch::TriggeredBy`:
      rule, incarnation, source and the optional connector and account)
      in the manifest, frozen through the run started by a start rule;
      the initiator label `trigger:<rule>` is a value, not a key;
   5. files a person keeps: the start rule document a person passes to
      `apb trigger set --file` (D1.5 item 3.5);
   6. signed stores: the rules file under `.apb/triggers/` and the state
      file under `.apb/trigger-state/` (D1.5 item 4);
   7. CLI: `apb trigger set|list|show|approve|enable|disable|remove|log|test`
      is CLI surface; the dashboard's `/api/triggers` routes are HTTP;
      both are outside the promise;
   8. no event type, field or guard name was added (the archived
      proposal says so; D1.2 rule 1 confirms it on the tree).
4. Decided (the result-value rule), not open: the trigger journal codes
   (`own_send`, `rate_cap`, `start_key_busy`, `change_waiting` and the
   rest `trigger/evaluate.rs` and `engine::trigger` journal) and the dry
   test's `policy` and `code` values (`trigger_launch_denied`,
   `trigger_approval_changed`, `trigger_budget_required`,
   `trigger_key_unavailable`) stay outside the promise: a code that apb
   only writes into a value of a result whose keys are not frozen (D1.5
   item 5) is not frozen. The trigger journal decides nothing and
   `trigger_test` is not a tool a host is told to read. They join when
   such a result is frozen in 1.x. The machine code groups of D1.5 item
   1 that are scanned stay scanned, with the one correction of that item:
   the scan is the completeness check of the `MACHINE_CODES` table, not
   the source of the frozen list.
5. Rule written here, for the listener heartbeat
   `<config_dir>/trigger-listener.json` (`trigger::view::LISTENER_FILE`):
   a file apb rewrites from scratch on its own schedule, which no person
   writes and no decision reads, is not covered.
6. Not covered by the rules above and said in "## Not covered": the
   per-incarnation trigger journal files (the result-value rule of item
   4 applies to their codes). The playbook store is frozen (D1.5 item 6,
   the owner's decision).

### D1.7 docs/COMPATIBILITY.md

New document, plain prose, numbered lists, no dashes of U+2013 or
U+2014:

1. "## What 1.x keeps": (1) playbook schema 2: a definition that parses
   and validates on 1.0 parses on every 1.x with the same error codes;
   warnings may come and go; a field may be added, never removed or
   renamed; (2) the run journal: every 1.0 event type and field keeps
   its name and shape, a new field is optional (`#[serde(default)]`,
   [decision record 0001](adr/0001-new-events-are-safe-to-skip.md)) and
   adds no member at its default, a new event type is safe to skip up to
   the next checkpoint, the checkpoints stay checkpoints (an event may
   become a checkpoint); the order of members in a line is not part of
   the promise; (3) the MCP tools: a tool and an argument keep their
   names, every input a 1.0 tool accepts stays accepted (its bounds,
   integer widths and enumerated values only widen), no tool or argument
   object gains a required member, a tool served read-only stays served;
   new tools and optional arguments may come; (4) validator codes: a code
   keeps its meaning and its severity, a retired code keeps its number
   (`RETIRED_CODES`, V15 and V16 among them) and a line of the frozen list
   is never removed; (5) machine names and files: the reason, refusal and
   guard codes of D1.5 item 1 keep their spelling; a run's
   `manifest.yaml` and `run.yaml`, profile files, connector manifests and
   account files of both scopes, `policy.yaml`, `config.yaml`,
   `decisions.yaml`, start rule documents and the signed stores of D1.5
   item 4 written by 1.0 still read with the same meaning; the result
   keys of every documented result variant of the tools of D1.5 item 5
   stay; (6) the playbook store: the versions of a playbook, the
   `current` pointer with its history, the `candidate` pointer, the
   provenance sidecar (`created_by` included), the trust store and the
   launch floor store written by 1.0 still read with the same meaning.
2. "## Deprecation": a name being replaced keeps working through 1.x with
   a warning naming its replacement; removal waits for 2.0.
3. "## Platforms": Linux and macOS are supported; isolation `full` needs
   Linux with unprivileged bubblewrap; on macOS `full` refuses
   `os_unsupported` and `best_effort` runs without a sandbox, for the
   whole 1.x line (`docs/ISOLATION.md`). Windows is not supported.
4. "## Not covered": the list at the end of D1.5, with D1.6 items 5 and
   6.
5. "## How it is guarded": the fixtures and their tests (paths), the
   released binary corpus and its readers.
6. "## Extending the freeze": the five commands of D0, "the switch only
   adds; a failing freeze test is a compatibility break to fix, never a
   fixture to regenerate", how a journal line is written (D1.2), and
   that a new tool, argument, event type, code or guard name joins the
   freeze in the change that adds it.

`CLAUDE.md` and `AGENTS.md` (identical shared text): a new bullet in
"Concepts that span files" pointing to `docs/COMPATIBILITY.md` and the
freeze fixtures; the module map gains `validate/codes.rs` (`CODES`,
`RETIRED_CODES`); the convention on new `EventPayload` fields adds "and
`skip_serializing_if` for its default, so it adds no member to a frozen
line". `docs/adr/0001-new-events-are-safe-to-skip.md`
gains the same sentence. `llms.txt` links `docs/COMPATIBILITY.md`.
`docs/HOST-INTEGRATION.md` has no list of tools a host relies on: it
gains a short section "## What a host can rely on" after its opening
paragraph, one paragraph naming the tools a host consumes (the list of
D1.5 item 5), saying that their arguments and the keys of each
documented result variant are frozen, and pointing to
`docs/COMPATIBILITY.md`.

## D5. Release candidate

1. Version `1.0.0-rc.1`: the workspace `version` of `Cargo.toml`, the six
   `version = "1.0.0-beta.7"` path dependencies on `apb-engine` and
   `apb-mcp` in `crates/apb-cli/Cargo.toml` (two) and
   `crates/apb-server/Cargo.toml` (four), and `Cargo.lock` (by `cargo
   build`), exactly as the release commit of beta.7 changed them (the
   same five files plus `llms.txt` and the notes).
2. `docs/release-notes/v1.0.0-rc.1.md` in the style of
   `v1.0.0-beta.7.md`: an opening paragraph, "## Installing the release
   candidate" (a pre-release; `latest` and Homebrew stay on 0.25.0; a
   beta install reaches it with `apb self-update`), one section per
   decision of this change from "Release notes" below, the changes of
   the thirteen earlier changes of the 1.0 plan, none of which was
   released (read their archived proposals; one-point-oh-fixes lists its
   lines under "Release notes", start-rules under "Behaviour changes for
   release notes", templates-and-global-playbooks in its design's
   "Release notes"), and "## The soak" with the checklist:
   1. at least 14 days on rc.1;
   2. three code routines and three non-code routines, one of each
      started by a start rule, one with a human gate, one with an MCP
      tool connector;
   3. no open defect of severity "wrong result, lost or repeated send,
      or security";
   4. ten consecutive green CI runs on main, every leg, no rerun;
   5. on a macOS desktop session, one publish step whose remote needs
      the keychain credential helper: no dialog appears, or the push
      times out within its bound (`docs/PUBLISH.md`, the limit that says
      `GCM_INTERACTIVE=never` is not verified on macOS; the result is
      written into that limit);
   6. the real update, a step a person performs and records (D5 item 5).
3. `crates/apb-cli/src/selfupdate.rs` `mod tests`: what `apb self-update
   --check` reports, and only that. `self_update_as(true, ..)` is the
   check path (`run_updater` with `check` set calls
   `is_update_needed_sync` and always answers `updated: false`); the
   install path (`run_updater` without `check`) runs `run_sync`, which
   downloads the release's installer and runs it against the receipt's
   install prefix, and the stub release API serves no installer (its
   asset has an empty `browser_download_url`) and the receipt's prefix is
   the test binary's own directory, so the install cannot be driven in a
   test without a real installer. Both cases run through the existing stub
   release API (`serve_releases`), an install receipt (`write_receipt`)
   and `self_update_as(true, ..)`, with stable `0.25.0` and pre-release
   `1.0.0-rc.1` served: `a_beta_install_sees_the_release_candidate`
   (running `1.0.0-beta.7`: code 10, "update available") and
   `a_stable_install_does_not_see_the_release_candidate` (running
   `0.25.0`: code 0, "apb is up to date"). The `takes_prerelease`
   helper's own cases are not repeated.
4. `llms.txt`: its version mentions, if any, and the new document line.
5. The release candidate procedure, written into the release notes'
   "## The soak" as item 6 with the steps below: after the tag
   `v1.0.0-rc.1` is published, a person on a machine with an install of
   `1.0.0-beta.7` made by the apb installer runs `apb self-update
   --check` (it reports the update), then `apb self-update`, then `apb
   --version` (it prints `1.0.0-rc.1`) and `apb doctor` (it runs, and its
   defaults line names no reconcile failure). The person records the
   date, the operating system, the version before and after and the
   output of the last two commands in the soak results that
   release-1.0.0 reviews; until that record exists the soak is not
   complete. A failure is a defect of severity "wrong result" for item
   3.

Wrong implementation: bumping to `1.0.0`: the soak has not run. Caught by
`release_workflow_test` only partly (it checks a notes file exists for the
tag); the task names the exact version.

## Decisions pending the owner

Items 1 and 2 follow the plan's recommendation; the owner may overrule
them. Item 3 is the owner's decision. (The
process env default and the secret param values in `run.yaml` were
decided in one-point-oh-fixes.)

1. macOS isolation (plan question 5). Decision here: no macOS sandbox in
   1.x (D3). A `sandbox-exec` backend is deprecated by the vendor, needs
   its own policy language and two CI legs, and would delay 1.0 by about
   one change.
2. Soak length and set (plan question 6). Decision here: 14 days, three
   code and three non-code routines (D5 item 2). Seven days would miss a
   weekly routine and the reminder cadence.
3. The playbook store. Decided by the owner: 1.x reads a playbook store
   written by 1.0, frozen in this change (D1.5 item 6).

## Carried fixes

Two test defects carried into this change (the owner's rule: no micro
pull requests). Neither changes production behaviour.

1. `crates/apb-engine/tests/suite/inbox_wait_test.rs`
   `a_batch_supervisor_patch_drops_the_wait_scan` times out on a loaded
   CI runner. Its steps each poll with `POLL_DEADLINE` (20 s), its final
   loop has its own 10 s bound, and the wait itself has
   `timeout_seconds: 30` (`batch_wait_yaml(30)`) while two agent retries
   with `retry_delay: 1s`, two supervisor wakes and a patch must happen
   first. Fix direction: give the wait a timeout no loaded run reaches
   (`batch_wait_yaml(600)`, the run is aborted by the test anyway), run
   the final loop through `poll_until` instead of its own 10 s bound,
   and give this test (and only where the same pattern repeats in the
   file) a named longer deadline for the steps that wait on agent
   processes; the assertions stay as they are. Seen failing first is not
   required (a timing flake); the report says how the test behaves under
   `stress`-like load (for example two copies run at once).
2. `crates/apb-engine/src/connector/oauth/consent.rs` `mod tests`
   `finish_exchanges_the_code_and_returns_the_refresh_token` is flaky on
   macOS. The stub token server (`stub_server`) puts its listener in
   non-blocking mode to poll `accept`; on macOS an accepted socket
   inherits non-blocking mode from the listener (on Linux it does not),
   so `read_line` or `read_exact` can stop at `WouldBlock` before the
   request body is read and recorded, while the stub still answers and
   the test then reads a body that is not there. Fix direction: after
   `accept`, set the accepted stream back to blocking
   (`set_nonblocking(false)`) with a read timeout, so the stub records
   the whole request, headers and body, before it writes its answer;
   the other tests of the module that use `stub_server` get the same
   fix through the helper.

## Files

Sixty changed files (tests, fixtures, docs, records and the openspec
documents included). `scripts/old-apb.sh` is used, not changed.

Platform (D3), 6:

1. `crates/apb-core/src/isolation/contract.rs`
2. `crates/apb-core/tests/suite/isolation_contract_test.rs`
3. `crates/apb-engine/src/fit/setup.rs`
4. `crates/apb-engine/src/sandbox/probe.rs`
5. `web/src/lib/consts.gen.ts` (regenerated)
6. `docs/ISOLATION.md`

Schema and validator codes (D1.1, D1.4), 11:

7. `crates/apb-core/src/validate/mod.rs`
8. `crates/apb-core/src/validate/codes.rs` (new)
9. `crates/apb-core/src/validate/isolation.rs` (module doc)
10. `crates/apb-core/tests/main.rs`
11. `crates/apb-core/tests/suite/schema_freeze_test.rs` (new)
12. `crates/apb-core/tests/fixtures/schema-1.0/corpus.yaml` (new)
13. `crates/apb-core/tests/fixtures/schema-1.0/fields.yaml` (new)
14. `crates/apb-core/tests/fixtures/schema-1.0/coverage.txt` (new)
15. `crates/apb-core/tests/fixtures/schema-1.0/expected.json` (new)
16. `crates/apb-core/tests/fixtures/schema-1.0/validator-codes.txt` (new)
17. `crates/apb-core/tests/fixtures/schema-1.0/witnesses.yaml` (new)

Person files (D1.5 item 3), 6:

18. `crates/apb-core/tests/suite/files_freeze_test.rs` (new)
19. `crates/apb-core/tests/fixtures/files-1.0/profiles.yaml` (new)
20. `crates/apb-core/tests/fixtures/files-1.0/connectors.yaml` (new)
21. `crates/apb-core/tests/fixtures/files-1.0/connector-accounts.yaml` (new)
22. `crates/apb-core/tests/fixtures/files-1.0/settings.yaml` (new)
23. `crates/apb-core/tests/fixtures/files-1.0/trigger-rules.yaml` (new)

Playbook store (D1.5 item 6), 2:

24. `crates/apb-core/tests/suite/store_freeze_test.rs` (new)
25. `crates/apb-core/tests/fixtures/store-1.0/store.json` (new)

Journal, persistence and the released corpus (D1.2, D1.5 items 2 and 4), 9:

26. `crates/apb-engine/tests/fixtures/journal/every_event.jsonl` (new)
27. `crates/apb-engine/tests/suite/journal_compat_test.rs`
28. `crates/apb-engine/tests/suite/persistence_freeze_test.rs` (new)
29. `crates/apb-engine/tests/main.rs`
30. `crates/apb-engine/tests/fixtures/run-files-1.0/manifests.yaml` (new)
31. `crates/apb-engine/tests/fixtures/run-files-1.0/run-configs.yaml` (new)
32. `crates/apb-engine/tests/fixtures/signed-1.0/stores.json` (new)
33. `crates/apb-cli/tests/suite/journal_corpus_test.rs`
34. `.github/workflows/ci.yml`

MCP and machine codes (D1.3, D1.5 items 1 and 5), 5:

35. `crates/apb-mcp/src/server/freeze_tests.rs` (new)
36. `crates/apb-mcp/src/server/mod.rs`
37. `crates/apb-mcp/tests/fixtures/mcp-tools-1.0.json` (new)
38. `crates/apb-mcp/tests/fixtures/mcp-results-1.0.json` (new)
39. `crates/apb-mcp/tests/fixtures/machine-codes-1.0.txt` (new)

Records (D1.7), 13:

40. `docs/COMPATIBILITY.md` (new)
41. `docs/adr/0001-new-events-are-safe-to-skip.md`
42. `docs/HOST-INTEGRATION.md`
43. `llms.txt` (also D5)
44. `CLAUDE.md`
45. `AGENTS.md`
46. `docs/superpowers/specs/<date>-one-point-oh-freeze-design.md` (new)
47. `openspec/changes/one-point-oh-freeze/proposal.md`
48. `openspec/changes/one-point-oh-freeze/design.md`
49. `openspec/changes/one-point-oh-freeze/tasks.md`
50. `openspec/changes/one-point-oh-freeze/specs/compatibility-freeze/spec.md`
51. `openspec/changes/one-point-oh-freeze/specs/platform-position/spec.md`
52. `openspec/changes/one-point-oh-freeze/specs/release-candidate/spec.md`

Release candidate (D5), 6:

53. `Cargo.toml`
54. `crates/apb-cli/Cargo.toml`
55. `crates/apb-server/Cargo.toml`
56. `Cargo.lock`
57. `crates/apb-cli/src/selfupdate.rs`
58. `docs/release-notes/v1.0.0-rc.1.md` (new)

Carried fixes, 2:

59. `crates/apb-engine/tests/suite/inbox_wait_test.rs`
60. `crates/apb-engine/src/connector/oauth/consent.rs`

The count is under the owner's limit of about 80 files per pull request.
It is the last change before the release candidate: it must see every
other change merged, and nothing else rides with a version bump.

## Code map

| Area | File | What |
|---|---|---|
| core | `src/validate/codes.rs` (new) | `CODES`, `RETIRED_CODES`, the registry test |
| core | `src/isolation/contract.rs` | the 1.x sentence |
| engine | `src/fit/setup.rs`, `src/sandbox/probe.rs` | the sentence on the fit item and the doctor line |
| mcp | `src/server/freeze_tests.rs` (new) | the tool, result key and machine code freeze |
| cli | `src/selfupdate.rs` | the release candidate tests |
| fixtures | `crates/apb-core/tests/fixtures/{schema-1.0,files-1.0,store-1.0}/*`, `crates/apb-engine/tests/fixtures/{journal/every_event.jsonl,run-files-1.0/*,signed-1.0/stores.json}`, `crates/apb-mcp/tests/fixtures/*` | freeze |

## Release notes

1. Isolation: the macOS sentence states the 1.x position, on the fit
   report and in `apb doctor` too.
2. Compatibility: `docs/COMPATIBILITY.md` states the 1.x promise, guarded
   by freeze tests of the schema, the journal, the MCP tools and the
   result keys of every documented result variant, the validator codes,
   the machine codes and guard names, the run and settings files, the
   signed stores and the playbook store (versions, `current` and its
   history, the provenance sidecar, the trust store, the launch floors).
3. Validator codes: V15, never emitted, is listed retired beside V16, so
   its number is never reused.

The notes of `v1.0.0-rc.1` also carry the lines of the thirteen earlier
changes (D5 item 2).

## Implementation notes

1. The thirteen earlier changes add events, fields, node kinds, MCP tools
   and validator codes. The freeze is generated last, so it holds them;
   D1.6 lists what the last three brought so a reviewer can check the
   generated files.
2. `fields_of`, `variants_of`, `variant_fields_of` and `normalize` are
   test helpers only; they never ship.
3. Order of the work (tasks.md): preconditions, the platform position,
   the freeze checks and writers, the fixtures generated from the final
   production code, the records, the bump, then the carried fixes,
   style, gates and close. A test that pins existing behaviour may pass
   at once; a test for changed behaviour is seen failing first.
