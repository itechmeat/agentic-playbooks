# Design: node contracts

Read `proposal.md` first. Every decision below names its files. Line
numbers are from origin/main a118ae4e and were checked; find the code by
the names if they moved. "Today" always means that commit.

Shared rules for every decision:

1. A playbook that uses none of the new keys behaves exactly as today:
   same parse, same canonical node JSON (the cache key's `node_def`,
   `crates/apb-core/src/cache.rs` `KeyParts`), same prompts, same events.
2. Every new event field is `#[serde(default)]` with
   `skip_serializing_if`; nothing new is mirrored to TypeScript except
   `NeedsLook` (D4) and the notify transition constant (D5).
3. Machine strings and messages are English, with no U+2013 or U+2014 and
   no exclamation mark.

## D1. Typed and required output fields

### Schema

`crates/apb-core/src/schema.rs`, next to `NodeFiles` (lines 332 to 353):

```rust
/// One declared output field: today's plain name, or a typed entry.
/// `untagged`, so `fields: [a, b]` parses and serializes exactly as before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldDecl {
    Name(String),
    Typed(TypedFieldDecl),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypedFieldDecl {
    pub name: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<FieldType>,
    /// The allowed values of an `enum` field, compared as text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType { String, Number, Bool, Array, Object, Enum }

impl FieldDecl {
    pub fn name(&self) -> &str;
    pub fn typed(&self) -> Option<&TypedFieldDecl>;
}
/// The names of `fields`, in declaration order.
pub fn field_names(fields: &[FieldDecl]) -> Vec<String>;
```

The name `FieldDecl` avoids a clash with `EdgeCondition::OutputField`.
`NodeFiles.fields` becomes `Vec<FieldDecl>` (keep its attributes);
`apb_core::list::ListSpec.item_outputs` (`crates/apb-core/src/list/mod.rs`
line 67) becomes `Vec<FieldDecl>` too. Add
`NodeFiles::field_names(&self) -> Vec<String>` and
`ListSpec::item_output_names(&self) -> Vec<String>`.

YAML:

```yaml
outputs:
  fields:
    - { name: role, type: enum, values: [correctness], required: true }
    - { name: blocking, type: number, required: true }
    - { name: findings, type: array, required: true }
    - notes                       # a plain name, as today
```

### Call sites of the name lists

The compiler lists them; every one reads names and gets `field_names`
(or the item variant) unless said otherwise:

1. `crates/apb-core/src/validate/node_io.rs` line 166 (`declared_fields`).
2. `crates/apb-core/src/validate/list.rs` lines 226, 287, 346.
3. `crates/apb-core/src/validate/outputs.rs` lines 96 to 105.
4. `crates/apb-engine/src/scheduler/cache.rs` line 699
   (`missing_output_fields`): iterate names.
5. `crates/apb-engine/src/scheduler/result.rs` line 507 (`fields_text`)
   and `ItemSubject.fields` (`scheduler/list/run.rs` line 127).
6. `crates/apb-engine/src/scheduler/list/run.rs` lines 185 to 190
   (`settled_output`) and `scheduler/list/check.rs` line 57.
7. `crates/apb-engine/src/scheduler/node.rs` line 483 (routing step
   outputs, names).
8. `crates/apb-engine/src/scheduler/list/batch.rs` line 262: the item's
   `outputs` keeps the declarations (`Vec<FieldDecl>`), because the
   contract and the status-file note below need the types.

### The contract: `crates/apb-core/src/output_contract.rs` (new)

```rust
pub const OUTPUT_CONTRACT: &str = "output_contract";
pub enum Problem {
    NotObject,                                   // the output is not a JSON object
    Missing,                                     // required, absent or null
    WrongType { want: FieldType, got: &'static str },
    NotInValues { got: &'static str },
}
pub struct Mismatch { pub field: String, pub problem: Problem }
pub fn check(fields: &[FieldDecl], output: &str) -> Vec<Mismatch>;
pub fn reason(fields: &[FieldDecl], mismatches: &[Mismatch]) -> Option<String>;
/// A JSON number, or a string that is exactly the text of a JSON number.
pub fn numeric(value: &serde_json::Value) -> Option<f64>;
/// An array or an object, or a string that parses as one.
pub fn json_in(value: &serde_json::Value) -> Option<serde_json::Value>;
```

Rules of `check`, over the typed entries in declaration order (a plain
`Name` is never checked here; its absence stays the
`output_fields_missing` warning):

1. When no entry is typed with a `type` or `required: true`, return empty
   without parsing.
2. The output is not a JSON object: one `NotObject` mismatch when any
   entry is `required`; otherwise empty (nothing can be type-checked, the
   warning path covers it).
3. A field absent or `null`: `Missing` when `required`, else no check.
4. A present value against `type`:
   - `string`: a JSON string. A number or a bool is `WrongType`.
   - `number`: `numeric(value)` is `Some` and finite.
   - `bool`: `true`, `false`, or the string `"true"` or `"false"` exactly.
   - `array`: `json_in(value)` is an array.
   - `object`: `json_in(value)` is an object.
   - `enum`: the value's text (the string itself, or the JSON text of a
     number or a bool, as `output_field_value` reads it) is one of
     `values`, exactly, with no case folding. An array, an object or a
     text outside `values` is `NotInValues`.
5. `numeric`: a `Value::Number` as `f64`; a `Value::String` only when
   `serde_json::from_str::<serde_json::Value>(s)` gives a
   `Value::Number` and `s` has no leading or trailing whitespace. So
   `"80"`, `"-1.5"` and `"1e3"` are numbers; `" 80"`, `"+80"`, `"inf"`,
   `"NaN"`, `"0x10"` and `""` are not. Never `str::parse::<f64>`.
6. `got` is the JSON kind: `string`, `number`, `bool`, `array`,
   `object`. A reason never holds the offending value, whole or in part:
   a cut value no longer matches the full secret the scrubber looks for,
   and the reason is journaled and sent in the retry prompt.

`reason` renders, or `None` for no mismatch:

```text
output_contract: field `blocking` must be a number, got a string; field `findings` is required and missing
output_contract: field `role` must be one of `correctness`, got a string outside them
output_contract: the output is not a JSON object, so the required fields `blocking`, `findings` are missing
```

The text depends only on the declaration and the output, never on time
or paths, so the stall fingerprint (D5) of a repeated mismatch is stable.

### Where the contract runs

`success_check_rejection` (`crates/apb-engine/src/scheduler/node.rs`
line 263) gets a parameter `contract: &[FieldDecl]` after
`protect_violation`. Order inside: protect, then the contract, then
evidence, then the check. A mismatch returns
`Some(Rejection::retry(reason))`: non-fatal, `failure_kind: None`, so the
existing code at both call sites journals `attempt_finished` `failed` with
`rejected_output` and `rejected_for`, and the retry briefing quotes it
(`scheduler/attempt_memory.rs`). Both callers pass the same slice: the
item's `outputs` for a list item, else the node's `outputs.fields`:

1. the report path, call at line 2774;
2. the status-file recovery path, call at line 3057.

The contract runs before the scenario's scripted check answer, so a
scenario run checks it too (the scripted outputs are real JSON).

A `script` node (`node.rs` lines 3546 to 3646, the script arm): one
helper `script_contract(node, status, stdout) -> Option<String>` runs
`output_contract::check` on the stdout with `node.outputs` fields when
the status is `Succeeded`; a reason returns
`AttemptOutcome::Finished { status: Failed, output: reason, events }`.
It is called at both returns of a succeeded script: after
`run_script_with_env` (when the read-only check found nothing), and on a
scenario's scripted answer (`Answer::Script`, lines 3573 to 3581), which
today returns before the end of the arm. A script node has no retry, so
the node fails with the reason as its output.

A cache hit is not checked: the declaration is part of the node's
canonical JSON, so a changed contract is a different key.

The completion check's `missing_fields` (`node.rs` line 413) and
`missing_output_fields` keep reading names only.

### The status-file note

`status_file_note` (`crates/apb-engine/src/scheduler/status_file.rs`
lines 117 to 137) takes `&[FieldDecl]`. When every entry is a plain name,
the note is byte-for-byte today's text (test it). Otherwise the sentence
after the status-file note reads:

```text
 This step publishes named outputs: write the status file with an outputs object that carries these keys: `role` (one of `correctness`; required), `blocking` (a number; required), `findings` (a JSON array; required), `notes`. A required key that is missing, or a value of the wrong type, rejects the attempt. Later steps read them by name.
```

Phrases: string "a string", number "a number", bool "true or false",
array "a JSON array", object "a JSON object", enum "one of `a`, `b`";
`; required` only when required; a plain entry is the name alone. The
note is appended outside `render_node_prompt`, so the cache key does not
move beyond the node JSON change.

### Validator: `crates/apb-core/src/validate/output_types.rs` (new)

Called from `validate()` in `validate/mod.rs` after line 396 with a
`// V167, V168` comment.

| Code | Severity | Rule |
|---|---|---|
| V167 | error | a typed entry of `outputs.fields` or `list.item_outputs` is malformed: `name` empty or holding `.`, `{`, `}` or whitespace; a name declared twice where one of the two is typed; `values` without `type: enum`; `type: enum` with no `values`, an empty value or a repeated value |
| V168 | warning | a reader cannot read a typed field's type: an `output_field` `equals` on a `bool` field other than `true`/`false`, on a `number` field that `numeric` does not read, on an `enum` field outside its `values`, or on an `array` or `object` field; a threshold (D2) on a field typed other than `number`; a list's `items.from` field typed other than `array`; a `no_new_items` field typed other than `array`; a `writes` template (D3) reading a field typed other than `array` or `string` |

A duplicate among plain names only stays allowed (existing playbooks).

V168 grows with the change: task A1.6 implements it for the readers that
exist then (`equals`, `items.from`, `no_new_items`); A3.3 adds the
threshold case (D2) and D1.3 the `writes` template case (D3), each with
its failing test first.

### Surfaces

The mismatch reaches people through what a rejection already reaches: the
retry briefing, `attempt_finished.rejected_for` (MCP `run_events`), and,
when retries run out, the node's output (`apb runs <id>`, MCP
`run_status`, the dashboard, the HTML report). Add to
`web/src/lib/journal.ts` (the `attempt_finished` case, line 152) a note
`rejected: <rejected_for, first 80 characters>` when the event carries
`rejected_for`; test in `journal.test.ts`.

Web editor: `web/src/lib/nodeio.ts` `fieldsToField` (line 5) shows each
entry's name (an object entry's `name`), never `[object Object]`;
`fieldToOutputs` (line 15) keeps a typed entry whose name is still listed,
drops one whose name was removed, and adds new names as plain strings,
in the order typed. Update the hint in `web/src/lib/nodefields.ts`
(line 68): a typed field rejects a wrong or missing value; edit types in
the YAML. Vitests in `nodeio.test.ts`.

Docs: `docs/HOWTO-authoring.md` "Named outputs (outputs.fields)"
(line 814): the typed form, the type table, the tolerance rules, the
script and list item behaviour, the note, and a V167/V168 table.

Wrong implementations to avoid:

1. `Name` serialized as `{ name: x }`: the canonical node JSON of every
   existing playbook changes and every cache key moves. Test: serialize a
   plain list and compare with today's JSON.
2. A plain name made required (an existing playbook starts failing).
3. The contract checked after evidence or the script check (a check
   runs on an output that is already known bad).
4. `str::parse::<f64>` for numbers (`"inf"` and `"NaN"` pass).
5. A mismatch journaled `fatal` or without `rejected_for`.
6. The status-file note changed for a plain-only list.
7. The contract on the report path only, not the recovery path.
8. A list item checked against the node's `outputs.fields` instead of
   `item_outputs`.
9. The offending value (or a cut of it) in the reason. Test: a run whose
   connector secret is configured, a 200-character value that holds the
   secret in a field typed `number`; neither the journal nor the retry
   prompt holds any 8-character run of the secret.
10. A scenario's scripted script answer returned unchecked. Test: a
    scenario whose script node declares a `number` field and scripts
    `stdout: '{"count": "many"}'` ends the node `failed` with
    `output_contract:`.

## D2. Numeric thresholds on `output_field` edges

### Schema

`EdgeCondition::OutputField` (`schema.rs` lines 2152 to 2156) becomes:

```rust
OutputField {
    node: String,
    field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    equals: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lt: Option<Threshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lte: Option<Threshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gt: Option<Threshold>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gte: Option<Threshold>,
},
```

`Threshold(pub f64)`, `#[serde(transparent)]`, compared and hashed by
its bits exactly like `apb_core::judge::Probability`
(`crates/apb-core/src/judge.rs` lines 379 to 395), because `EdgeCondition`
derives `Eq` and `Hash` and is a routing key (`validate/graph.rs` line
527). YAML: `{ type: output_field, node: score, field: coverage, lt: 80 }`.

Next to `output_field_value` (line 2206):

```rust
pub enum FieldCompare<'a> { Equals(&'a str), Lt(f64), Lte(f64), Gt(f64), Gte(f64) }
impl EdgeCondition {
    /// The one comparator of an `output_field` condition; `None` for any
    /// other variant and for zero or several comparators (V169).
    pub fn field_compare(&self) -> Option<FieldCompare<'_>>;
}
/// Whether field `field` of `output` satisfies `cmp`. `Equals` is today's
/// `output_field_value(..) == equals`. A threshold reads the field with
/// `output_contract::numeric`; anything it cannot read is a non-match.
pub fn output_field_matches(output: &str, field: &str, cmp: &FieldCompare<'_>) -> bool;
```

A bool, null, array, object, non-numeric string, absent field or
non-JSON output never matches a threshold and never errors.

### Every site of the variant

The compiler reports the destructuring sites that name `equals`; the rest
use `..` and compile silently. Handle each:

1. `crates/apb-engine/src/parallel.rs` lines 83 to 91 (`edge_matches_at`):
   `field_compare()` then `output_field_matches` over
   `state.outputs.get(node)`.
2. `parallel.rs` line 147 (`condition_reads`): unchanged.
3. `crates/apb-engine/src/diagnostics/route.rs` line 82 (`kind_name`):
   unchanged. `observed()` line 105: `equals` keeps today's value (the
   clipped string); a threshold returns
   `json!({"value": <clipped text or null>, "number": <f64 or null>})`.
4. `crates/apb-engine/src/report_html/graph.rs` line 511 (`edge_label`):
   `{field}: {equals}`, or `{field} < 80`, `{field} <= 80`, `{field} > 80`,
   `{field} >= 80` (ASCII, the number in Rust's shortest form).
5. `crates/apb-core/src/validate/graph.rs` line 237 (V10/V40): unchanged;
   line 556 (`describe_route_key`): name the comparator
   (`equals=`x``, `lt=80`).
6. `crates/apb-core/src/validate/judge.rs` line 379 (V53):
   `equals: Some(equals)` in the pattern.
7. `crates/apb-core/src/validate/edges.rs` line 191 (V125): only an
   `equals` is checked; a threshold on a judge's `<id>_p` or `<id>_score`
   field is legitimate and not reported.
8. `crates/apb-core/src/validate/node_io.rs` line 139 (V46): unchanged.
9. Every constructor (`EdgeCondition::OutputField { node, field, equals }`
   in code and tests): `equals: Some(..)` and the four thresholds `None`.
10. `web/src/lib/graph.ts` `edgeLabel` (line 211) and the `output_field`
    exit caption (lines 346 to 358): the same labels as the HTML report,
    with titles `taken when ... whose "coverage" is below 80` (below,
    at most, above, at least). `playbookedit.ts` keeps the YAML node, so
    an edit round trip keeps the threshold: cover it with a vitest
    (load, save, reload, as `loopexit.test.ts` does).

### Validator

In `validate/edges.rs`, called next to `check_judge_values` (mod.rs
line 386) with `// V169`:

| Code | Severity | Rule |
|---|---|---|
| V169 | error | an `output_field` condition names no comparator or more than one of `equals`, `lt`, `lte`, `gt`, `gte`, or a threshold that is not a finite number |

Docs: `docs/HOWTO-authoring.md` "Human review and conditional edges"
(line 1936, the `output_field` bullet): the thresholds, the numeric
reading, the non-match rule, an example, the V169 row; the authoring
table of codes.

Wrong implementations to avoid:

1. A threshold compared as text (`"9" < "10"` is false as text).
2. `"inf"`, `"NaN"` or `" 80"` read as numbers.
3. A non-numeric value raising an error or failing the node.
4. `Threshold` with derived float `PartialEq` and no `Eq`/`Hash` (does
   not compile) or with `Hash` over something other than the bits.
5. Changing `output_field_value` (it also backs templates and the D0
   rule of the earlier change).
6. The editor dropping `lt` on save (the edge becomes a parse error).

## D3. Write scope

### Schema

On `Node` (`schema.rs`, next to `read_only`, line 1435), for the same
reason `read_only` sits there (a kind's unknown keys are dropped):

```rust
/// The files an `agent_task` or `script` attempt may change, relative to
/// the node's directory: globs, or one template that renders a list.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub writes: Option<WriteScope>,
/// At most this many files an attempt may change (within `writes` when
/// it is declared, else anywhere in the tree).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub max_changed_files: Option<u32>,
```

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WriteScope { Globs(Vec<String>), Template(String) }
```

YAML: `writes: ["src/export/**", "tests/export_*.rs"]`, or
`writes: "{{nodes.plan.output.files}}"`, with `max_changed_files: 5`.

### Pure rules: `crates/apb-core/src/write_scope.rs` (new)

```rust
pub const MAX_SCOPE_ENTRIES: usize = 200;
/// Why one glob cannot be a scope entry, or `None`.
pub fn entry_problem(glob: &str) -> Option<&'static str>;
/// The entries a rendered template names: a JSON array of strings, else
/// one entry per non-empty trimmed line.
pub fn parse_rendered(text: &str) -> Result<Vec<String>, String>;
/// The single template reference of a template-form scope, when the text
/// is exactly one `{{...}}` with only whitespace around it.
pub fn template_ref(text: &str) -> Option<&str>;
```

1. `entry_problem`: empty or whitespace only; starts with `/` or `\`, or
   has a drive prefix such as `C:`; a `..` or `.` segment; holds `{{`;
   `globset::Glob::new` fails.
2. `parse_rendered`: trimmed text empty is an error ("rendered empty");
   a text starting with `[` must parse as a JSON array whose every
   element is a non-empty string; a list with no entry (`[]`) is an
   error ("rendered an empty list"), so it refuses the node before any
   attempt like an empty render; more than `MAX_SCOPE_ENTRIES` entries
   is an error; every entry passes `entry_problem`.
Globs match as `build_globset` builds them everywhere else (`*` crosses
`/`), relative to the node's directory (`node_dir`, `node.rs` line 915).

### One resolution per execution

`crates/apb-engine/src/scheduler/write_scope.rs` (new):

```rust
pub(crate) const WRITE_SCOPE: &str = "write_scope";
pub(crate) struct ResolvedScope {
    pub globs: Option<Vec<String>>,  // None: max_changed_files alone
    pub max: Option<u32>,
    set: Option<globset::GlobSet>,
}
/// The node's scope for this execution, `Ok(Err(message))` when the node
/// must not start, `Ok(Ok(None))` when it declares neither key.
pub(crate) fn resolve(
    playbook: &Playbook,
    node_id: &str,
    render: impl FnOnce(&str) -> Result<String, EngineError>,
) -> Result<Result<Option<ResolvedScope>, String>, EngineError>;
/// The sentence the attempt prompt gets on its first turn.
pub(crate) fn note(scope: &ResolvedScope) -> String;
```

The shape of `scheduler/node_workdir.rs` `resolve` (lines 14 to 40), and
the template renders through the same `render_node_prompt` closure the
agent arm passes there (`node.rs` lines 915 to 935). A refusal message:

```text
node `fix` writes `{{nodes.plan.output.files}}` rendered empty: the value it reads was never published, so the node was not started
node `fix` writes entry `../x` (from `{{nodes.plan.output.files}}`) is not a path inside the node's directory, so the node was not started
```

The note, appended next to the status-file note (`node.rs` lines 1307 to
1320, `first_turn` only, outside the cache key):

```text
Write scope: change only files matching `src/export/**`, `tests/export_*.rs`, and at most 5 files in all. apb compares the tree after the attempt; a change outside the scope, or more files, fails the step without a retry.
```

(The parts are present only when declared.)

### The check reuses the read-only tree comparison

`crates/apb-engine/src/scheduler/node/read_only.rs`:

1. Split `Before::finish` (lines 333 to 386): a new
   `pub(crate) fn compare(&self) -> Comparison` takes the after state and
   returns `{ changes: Vec<TreeChange>, after_partial: Option<ReadOnlyLimit> }`;
   `finish` calls it and is otherwise unchanged (its message, events and
   evidence stay exactly as today). Make `evidence_stem` and
   `next_ordinal` `pub(super)`.
2. `write_scope::finish(before: read_only::Before, scope: &ResolvedScope,
   run_dir, project) -> Result<Option<String>, EngineError>` (the
   `Before` is taken with `peers: None`; a scoped node never runs beside
   another writer, below):
   - file changes only: `Modified`, `Added`, `Deleted`, `Mode`, `Link`.
     The repository state kinds (`GitHead`, `GitRefs`, `GitIndex`,
     `GitControl`) are not files and count for nothing (a commit of an
     in-scope file is in scope);
   - each changed path is `own` (matches the scope; every path when only
     `max_changed_files` is declared), else `outside`;
   - a violation when `outside` is non-empty (only with `writes`) or
     `own.len()` exceeds `max`;
   - writes the evidence `runs/<id>/writes/<stem>/<n>.json` (0600,
     `fsutil::atomic_write_private_under`): `node`, `attempt`, `scope`
     (the resolved globs), `max`, `own`, `outside`, `partial`, paths
     scrubbed and listed at most `MAX_LISTED`, never content;
   - a partial state (the before or the after state hit an entry or byte
     limit) is an unknown result and fails the node like a violation,
     whatever changes were seen: what the limit left out may hold a change
     outside the scope or more files than `max`. The evidence says
     `partial: true` and which limit.

Messages (prefix `write_scope`, at most 5 paths named, the count after):

```text
write_scope: node `fix` changed 2 files outside its write scope: docs/a.md modified (and 1 more)
write_scope: node `fix`'s tree is too large to compare whole (the entries limit), so its write scope cannot be shown to hold
write_scope: node `fix` changed 12 files, more than its max_changed_files 10: src/a.rs modified (and 11 more)
```

Violations join with `; `.

The observed boundary is the read-only check's (`TreeState`,
`crates/apb-core/src/fingerprint/tree_state.rs` lines 1 to 40): on a git
work tree the files git tracks or would track, every `.gitignore`, the
root's `.apb` definitions, initialized submodules, nothing below a
symlinked directory. `writes` therefore does not constrain git-ignored
files (build output, a `.env`), the run's own `.apb` files, paths outside
the node's tree, or external effects (a connector call, a network
request). The docs say so in those words.

### Where it runs and what a violation does

Agent arm, `node.rs`:

1. Resolve once per execution next to `node_dir` (after line 935); a
   refusal returns `Finished { Failed, message }`.
2. Lines 1782 to 1803: take `Before` when the node is detected read-only
   (today's condition) or has a scope, and not `scripted_check`; the
   batch interval stays read-only only. A scope is checked whatever the
   isolation tier: the sandbox binds directories, so it cannot enforce
   globs.
3. Lines 2391 to 2411: finish with `write_scope::finish` for a scoped
   node (a node is never both, V170), at the same point, before protect
   puts anything back.
4. Lines 2437 to 2470: a write violation is fatal like a read-only one:
   joined with a protect reason, `attempt_finished` `failed` with
   `failure_kind: Some("write_scope")`, then
   `Finished { Failed, reason }`. No retry, no fallback, no deferral.

Script arm (lines 3597 to 3646): the same `Before` and finish; a
violation returns `Finished { Failed, reason }` before the D1 contract.

A scenario run takes no tree state and checks nothing (as for read-only).

Why fatal, compared with the two existing checks:

| Check | What it has | On a violation |
|---|---|---|
| `protect` | a content copy of the protected files | restore, then an ordinary retry (honest: the retry starts from the restored files) |
| `read_only` | digests only | fail at once, change stays |
| `writes` | digests only (the same `TreeState`) | fail at once, change stays |

A retry after a write outside the scope would take a fresh before state
that already holds the stray change, and pass while it stays: the check
would launder it. A per-execution baseline would not survive a deferred
retry or a resume, since `TreeState` lives in memory only. To have
changes put back, combine `writes` with `protect`.

### Batches

A scoped node (one with `writes` or `max_changed_files`) never shares a
concurrent batch with another writer, scoped or not. A whole-tree
comparison cannot tell which process made a change, so two writers side
by side would blame each other or let one's stray write pass as the
other's in-scope one. Unscoped writers keep today's behaviour.

`crates/apb-engine/src/scheduler/batch_class.rs`: add
`pub(crate) fn is_scoped(node: &Node) -> bool` (`writes` or
`max_changed_files` set). `batch_admits` (line 45), `Writer` arm: refuse
when the batch holds a `Detected` member (today), or when the candidate
is scoped and the batch holds any `Writer`, or when the candidate is a
`Writer` and the batch holds a scoped member. `Enforced` (a sandboxed
read-only node, which cannot write) still mixes with a scoped node, and
the `Detected` rules are unchanged. Both admission sites (`scheduler.rs`
line 1411, `scheduler/pipeline.rs` line 126) already call `batch_admits`
one candidate at a time. `BatchIntervals` is not changed.

### No cache

A cache hit (`scheduler/cache.rs` `probe`, line 657) restores the stored
artifacts into the tree and settles the node before any attempt, so no
scope check would run. V170 refuses `cache` on a node with `writes` or
`max_changed_files`, and the engine treats such a node as uncached even
when validation was skipped: `cache::prepare` returns no context for it
(and for a proving node, D4), so neither the sequential probe nor the
batch member probe (`ProbeContext.batch_member`) can hit.

### Validator: `crates/apb-core/src/validate/write_scope.rs` (new)

Called from `validate()` after the V167 call with `// V170-V172`.
Add a `writes_texts(playbook)` beside `workdir_texts`
(`validate/templates.rs` line 107) and chain it into
`check_cross_branch_reads` (V38, line 158) and `check_declared_fields`
(V46, `node_io.rs` line 91).

| Code | Severity | Rule |
|---|---|---|
| V170 | error | `writes` or `max_changed_files` on a node that is not an `agent_task` or `script`, on a `read_only` node, on a list node or on a node with `cache`; `writes: []`; more than 200 entries; a list entry `entry_problem` refuses; the string form not exactly one template reference, or one that is not `nodes.<id>.output.<field>` of an existing node or `params.<name>` of a declared param; `max_changed_files: 0` |
| V171 | warning | two writers can run side by side and neither declares `writes`: both are targets of unconditional, non-fallback edges from one source, both `agent_task` or `script`, none `read_only`, `interactive`, a list or with `protect`, and `defaults.max_parallel` is not 1 |
| V172 | warning | the same pair shape where at least one of the two declares `writes` or `max_changed_files`: they run one after another |

### Run guard

An older apb drops `writes` in silence. Add `GuardedFeature::WriteScope`
(`"write_scope"`) to `crates/apb-core/src/validate/guarded.rs`, used when
any node declares `writes` or `max_changed_files`; extend `ALL` and the
test. The guard protects the resumption of a run this version created:
an older apb refuses to continue it. It does not protect a fresh launch
by an older apb, which drops the unknown node keys (`schema.rs`
`Node`, no `deny_unknown_fields`) and computes guards from the fields it
knows, so it runs the node with `writes` silently ignored. The docs and
the release notes state this downgrade limit.

Docs: a section "Write scope (writes, max_changed_files)" after
"Read-only node (read_only)" (`docs/HOWTO-authoring.md` line 1376) with
both forms, the rendering rules, the fatal rule and its reason, the
observed boundary and what `writes` does not constrain, the partial rule,
the batch rule, the no-cache rule, the downgrade limit, the V170 to V172
table; a `writes` row in
`docs/GUARDRAILS.md` next to the `read_only` row (line 60); one sentence
in `docs/ISOLATION.md` "Read-only nodes": a write scope is checked after
the attempt in every tier.

Wrong implementations to avoid:

1. A new snapshot instead of `read_only::Before` (two tree walks per
   attempt, two sets of limits).
2. A retry after a violation (launders the stray change).
3. Git state changes counted as changed files (every committing node
   would exceed its count).
4. Two scoped writers admitted to one batch, or a scoped node beside an
   unscoped writer. Test: two scoped fan-out siblings with disjoint
   globs and `max_parallel: 2` never overlap in time.
5. Changes attributed to a peer by matching its scope (a peer's stray
   write passes as the other's in-scope one).
6. A partial state treated as clean. Tests: lower
   `APB_READ_ONLY_MAX_ENTRIES` (before state) and grow the tree past it
   (after state) in a debug build; each fails the node with the
   `too large to compare whole` reason.
7. A claim that `writes` covers ignored files. Test: a write to a
   git-ignored path outside the scope passes, and the docs say why.
8. A cache hit on a scoped node. Tests: a unit test that
   `cache::prepare` returns no context for a scoped node and for a
   proving node (both arms go through it); with validation bypassed, a
   scoped node with `cache: auto` run twice sequentially journals no
   `node_cache_hit`, and a proving node run twice as a batch member beside
   another script journals none either (D4).
9. The template rendered per attempt (the scope moves between retries).
10. A rendered `..` or absolute entry accepted.
11. The check skipped for a sandboxed attempt.

## D4. A check that must fail first

### Schema

`SuccessCheck::Scoped` (`schema.rs` line 1753) and its `ScopedForm`
(line 1774, `deny_unknown_fields`) become:

```rust
Scoped {
    script: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "is_false")]
    prove_fails_first: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on_green_before: Option<OnGreenBefore>,
},
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnGreenBefore { #[default] Fail, Skip }
```

`SuccessCheck::scope()` returns `scope.as_deref().unwrap_or(&[])`; add
`SuccessCheck::prove(&self) -> Option<OnGreenBefore>`
(`Some(on_green_before.unwrap_or_default())` when `prove_fails_first`).
Existing `{ script, scope }` documents parse and serialize unchanged. The
other sites: `schema.rs` lines 1785, 1795, 1804, 1813;
`validate/nodes.rs` line 387 (V163 reads `scope: Some(list)` only).

Only the script form can carry it: the marker form and the decision-model
completion check judge the attempt's output, and before the first
attempt there is none. An older apb fails to parse the new keys
(`deny_unknown_fields`), loudly.

YAML: `success_check: { script: scripts/test_fails.sh, prove_fails_first: true }`.

### When it runs

`crates/apb-engine/src/scheduler/prove.rs` (new):

```rust
/// What the journal says about the proof of `key` now: due, or the open
/// `check_proved` that still decides the visit.
pub(crate) enum ProofState { NotDue, Due, Open(Proved) }
pub(crate) fn state(events: &[Event], key: ExecKey<'_>) -> ProofState;
pub(crate) enum Proved { FailedFirst, GreenBefore(OnGreenBefore) }
pub(crate) fn run(
    journal: &Journal, run_dir: &Path, node: &Node, node_dir: &Path,
    workdir: &Path, sandbox: Option<&crate::sandbox::SandboxPlan>,
) -> Result<Result<Proved, String>, EngineError>;
```

1. Once per effect scope (`connector::intent::effect_scope_floor`,
   `crates/apb-engine/src/connector/intent/ledger/mod.rs` line 298). Over
   the events at or above the floor: the latest `check_proved` of the key
   with no `node_finished` of the node after it is open. `state` is
   `Open(..)` when one is open (it decides, the script is not run again),
   else `NotDue` when an `attempt_started` of the key is there, else
   `Due`. So a first execution, a loop visit after a success and a
   supervisor retry after a green-before failure prove; a retry, a
   fallback step, a deferred retry, a resume after a pause or a crash, and
   a failure route back into a node that already ran an attempt do not
   (the tree may hold that attempt's work, and the check would wrongly
   read green); and a crash after `check_proved`, before the attempt
   started or the node finished, does not run the check twice: an open
   red proof goes on to the attempt, an open green one settles as it
   said.
2. Computed once before the attempt loop in the agent arm (a list node
   and a `script` node never prove; V173 refuses the key there), only when
   `node.success_check.prove()` is `Some`, `item.is_none()`,
   `resume.is_none()`, `continuation.is_none()`,
   `override_prompt.is_none()` and the run is not a scenario run
   (`scripted`, `node.rs` line 1211), over `crate::event::read_all(run_dir)`
   (an engine decision; `read_all_lossy_tail` is reserved for reporting
   surfaces, `event.rs` line 3537).
3. The proof is an observation. Run inside the first attempt, right after
   its placement (`node.rs` line 1766, `let attempt_workdir =
   place.cwd.clone()`), before the read-only or scope state is taken and
   before anything is spawned: take `read_only::Before::take(node_id, 0,
   &node_dir, None)`, then `crate::script::run_check(run_dir,
   &attempt_workdir, script, place.sandbox.as_deref())`, the same call
   `success_check_rejection` makes (line 317), so a sandboxed check stays
   in the sandbox; then `Before::compare()`. A changed tree, or a partial
   state (the comparison cannot show the tree was left alone), fails the
   node at once with
   `check_changed_tree: success_check `scripts/x.sh` changed the tree while proving it fails first: src/a.rs modified`
   (or `... cannot be shown to leave the tree alone: the tree is too large to compare whole`);
   `check_proved` is journaled with `changed_tree: true` and the agent is
   not started. A `check_git` violation after it ends the node the same
   way. External effects (a network call, a write outside the tree) are
   not observed: a shell exit code cannot prove their absence, so a
   check that reaches outside the tree needs an environment that is
   read-only there (an `isolation` sandbox, a read-only credential); the
   docs say so.
4. Journal `check_proved` (below) with `journal.append`, then:
   - the check failed: go on with the attempt;
   - it passed, `fail`: return `Finished { Failed, output }` with
     `check_green_before: success_check `scripts/x.sh` passed before the agent ran, so a pass after it would prove nothing; the agent was not started`;
   - it passed, `skip`: return `Finished { Succeeded, output }` with
     `already done: success_check `scripts/x.sh` passed before the agent ran; the agent was not started`.
     V173 makes sure such a node declares no outputs anyone reads, so the
     settlement claims nothing it did not produce.

No `attempt_started` or `attempt_finished` is journaled for the proof: it
is not an agent attempt and must not count in attempts, durations or
retries.

### Journal: one new event

`crates/apb-engine/src/event.rs`:

```rust
/// A `success_check` with `prove_fails_first` ran before the first attempt
/// of an effect scope (spec node-contracts D4). Safe to skip: not a
/// checkpoint; an older apb loses only the proof line.
CheckProved {
    node: String,
    script: String,
    /// The check failed, so a later pass can be told from no work.
    failed_first: bool,
    /// `fail` or `skip`; set only when `failed_first` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on_green_before: Option<String>,
    /// The check changed the observed tree, or the comparison was
    /// partial: the node failed and the outcome above decides nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    changed_tree: bool,
},
```

Do not add it to `is_checkpoint` (line 3333). Follow the
skip-safety tests the event module has for other additive events.

### Run outcome: a reason to look

`crates/apb-engine/src/run_outcome/summary.rs`:
`NeedsLookReason::ChecksGreenBefore`, `NeedsLook.green_checks: u32`
(`skip_serializing_if = "is_zero"`, ts optional, like `list_items`),
counted by a new `fn green_checks(events) -> u32`: the nodes whose latest
`check_proved` has `failed_first: false` and `on_green_before: "fail"`
and that have no `node_finished` `succeeded` after it. `NeedsLook::of`
gets the count; `line()` says `1 check green before work` /
`2 checks green before work`. `web/src/lib/runverdict.ts`
`needsLookCounts` (line 85) adds the same words; test in
`runverdict.test.ts`. Regenerate `api.gen.ts`.

`web/src/lib/journal.ts`: a `check_proved` line, `failed before the
work` or `passed before the agent ran (fail|skip)`.

### Validator

In `validate/nodes.rs` next to V163 (line 385):

| Code | Severity | Rule |
|---|---|---|
| V173 | error | `on_green_before` without `prove_fails_first: true`; a `{ script }` map with neither `scope` nor `prove_fails_first: true` (write the bare string); `prove_fails_first: true` on a list node or on a `script` node (the proof runs before a plain `agent_task`'s first attempt only, item 2 of "When it runs", so the engine would skip it in silence; a `script` also gets V33); `prove_fails_first: true` on a node with `cache` (a cache hit settles the node with no proof in this run, D3 "No cache"); `on_green_before: skip` on a node that declares `outputs.fields` or `results`, or whose output an edge condition reads (`output_match`, `output_field` or `no_new_items` naming the node): a skipped node must not claim an output it did not produce |

Docs: `docs/HOWTO-authoring.md` "Success checks" (line 405): a section
"A check that must fail first", with both outcomes, when it runs and
when it does not, the code example and a non-code example (a "the
invoice is in the ledger" check with `on_green_before: skip`), the V173
row.

Wrong implementations to avoid:

1. The proof on every attempt (a retry after partial work reads green).
2. The proof on a resume or a deferred retry continuation.
3. A proof run journaled as an attempt.
4. Running the check outside the attempt's sandbox.
5. A cache hit on a proving node (no proof in this run). Test: D3 wrong
   implementation 8.
6. Proving a marker or completion check.
7. A green proof with `fail` routed as success.
8. A check that edits the tree trusted. Test: a check script that
   touches a tracked file and exits 1 fails the node with
   `check_changed_tree:` and no attempt starts.
9. The proof run twice across a crash. Test: crash after `check_proved`
   (red) and before `attempt_started` (leave the journal cut after
   `check_proved`, the way the engine's resume tests leave a journal
   mid-execution), resume: exactly one `check_proved`, and the attempt
   runs; the same with a green `fail` proof: one `check_proved`, the node
   fails.
10. `skip` on a node whose declared fields are then read empty. Test:
    V173 for each of the three refused shapes.
11. The proof decided over `read_all_lossy_tail`.

## D5. Stall exit

### Schema

On `Node` (next to D3's keys) and on `Defaults` (after `retry_delay`,
line 886):

```rust
/// Stop retrying once the same rejection repeats this many times in a
/// row (`agent_task` only). Absent: off.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub stall_after: Option<u32>,
```

`Playbook::effective_stall_after(&self, node: &Node) -> Option<u32>`:
`None` for anything but a non-list `agent_task`, else the node's value,
else `defaults.stall_after`. A flat key, beside `max_retries` and
`retry_delay`, rather than a `retry:` block apb does not have.

`StatusEq` (line 2234) gets `Stalled` (`equals: stalled`).

### Fingerprint: `crates/apb-core/src/rejection.rs` (new)

```rust
/// The stall fingerprint of a rejection reason: the normalized text's
/// SHA-256, first 16 hex characters.
pub fn fingerprint(reason: &str, run_id: &str, run_dir: &str) -> String;
/// The normalized text (public for tests and for later grouping).
pub fn normalize(reason: &str, run_id: &str, run_dir: &str) -> String;
```

`normalize`, in this order:

1. every occurrence of `run_dir` (when not empty), then of `run_id`
   (when not empty), becomes `<run>`;
2. timestamps
   `\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}(:\d{2}(\.\d+)?)?(Z|[+-]\d{2}:?\d{2})?`
   become `<time>`;
3. UUIDs (`[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`)
   become `<id>`;
4. a maximal run of 7 or more hex characters bounded by non-word
   characters that contains at least one digit becomes `<hex>`;
5. every remaining maximal digit run becomes `#`;
6. whitespace runs collapse to one space; trim.

The input is the `rejected_for` text (already scrubbed and cut to 500
characters by `rejected_for_text`, `node.rs` line 253). Regexes are
compiled once (`std::sync::LazyLock`).

### Journal fields

`EventPayload::AttemptFinished` (`event.rs` line 223), after
`rejected_for` (line 309):

```rust
/// `rejection::fingerprint` of `rejected_for`; set exactly when it is.
#[serde(default, skip_serializing_if = "Option::is_none")]
rejection_fingerprint: Option<String>,
/// This rejection made the same fingerprint repeat `stall_after` times
/// in a row, so the node stopped retrying. Additive.
#[serde(default, skip_serializing_if = "std::ops::Not::not")]
stalled: bool,
```

Every constructor the compiler lists gets `rejection_fingerprint: None,
stalled: false`, except the four rejection sites.

### The streak: `crates/apb-engine/src/scheduler/stall.rs` (new)

```rust
pub(crate) struct Assessed { pub fingerprint: String, pub stalled_at: Option<u32> }
/// Trailing earlier attempts of `key` in its effect scope that carry
/// `fingerprint`, from the journal.
pub(crate) fn streak(events: &[Event], key: ExecKey<'_>, fingerprint: &str) -> u32;
pub(crate) fn assess(run_dir: &Path, run_id: &str, key: ExecKey<'_>,
    rejected_for: &str, limit: Option<u32>) -> Result<Assessed, EngineError>;
pub(crate) fn output(count: u32, reason: &str) -> String;
```

`streak`: the `attempt_finished` events of the key (node and item) with a
seq at or above `effect_scope_floor`, newest first. Skip an attempt whose
`failure_kind` is `transient` or whose status is `interrupted` (it never
finished its work); count while `rejection_fingerprint` equals the
fingerprint; stop at any other attempt. `assess` reads
`crate::event::read_all(run_dir)` (an engine decision, so an unreadable
journal is an `EngineError`, never a silent streak of 0), and `stalled_at` is `Some(streak + 1)` when `limit` is
set and `streak + 1 >= limit`.

The scope spans executions since the node's last success, like the retry
briefing: a supervisor retry or a failure route back into the node that
meets the same rejection stalls again at once. That is the point: nothing
changed what the check sees.

### The four rejection sites

The sites that set `rejected_for` today, all in `node.rs`: the report
path's rejection (append at lines 2797 to 2819) and completion enforce
(lines 2857 to 2877), and the recovery path's completion enforce (lines
3091 to 3111) and rejection (lines 3147 to 3168). At each, when
`rejected_for` is set:

1. compute `rejected_for_text` once; `stall::assess(run_dir, run_id,
   key, &text, playbook.effective_stall_after(node))`;
2. append with `rejected_for: Some(text)`,
   `rejection_fingerprint: Some(fingerprint)`,
   `stalled: stalled_at.is_some()`;
3. when `stalled_at` is `Some(n)`, return at once
   `Finished { Failed, output: stall::output(n, &reason), events }`:
   no retry, no fallback step, no deferral.

```text
stalled: the same rejection came back 3 times in a row, so apb stopped retrying: success_check `scripts/verify.sh` failed
```

Fingerprints are written whether or not `stall_after` is set, so a later
grouping can read them. Fatal rejections carry neither field. Keep the
four sites short: one helper in `stall.rs` that returns the two field
values and the optional output.

### Routing

`crates/apb-engine/src/state.rs` `RunState`:
`pub stalled: BTreeSet<String>`, the nodes whose latest execution ended
stalled. Fold: an `attempt_finished` with `item: None` (line 584)
records whether it was `stalled` for its node; a `node_finished`
(line 598) inserts the node when its status is `failed` and its last
attempt was stalled, else removes it. No new event, no new status: the
node is `failed`, so an older apb resumes it as failed.

`crates/apb-engine/src/parallel.rs`: `status_matches` (line 40) becomes
`pub(crate) fn status_matches(state: &RunState, node: &str, equals:
StatusEq) -> bool`: `success` and `failure` as today over
`state.current_status(node)` (`failure` still matches a stalled node),
`stalled` when the status is `Failed` and `state.stalled` holds the node.
`scheduler.rs` `dead_end_reason` (lines 229 to 237) adds the `Stalled`
arm with the same rule.

`crates/apb-core/src/validate/graph.rs` V09/V39 (lines 183 to 226):
coverage needs `Success` and `Failure`; `Stalled` covers neither, so
replace `covered.len() < 2` with the explicit check.
`crates/apb-core/src/fork.rs` line 61 stays: a `stalled` edge does not
handle a branch's failures in general.

Author pattern (first match wins, so `stalled` before `failure`):

```yaml
- { from: fix, to: ask_person, condition: { type: node_status, node: fix, equals: stalled } }
- { from: fix, to: reconsider, condition: { type: node_status, node: fix, equals: failure } }
- { from: fix, to: next_round, condition: { type: node_status, node: fix, equals: success } }
```

Web `graph.ts`: `edgeTone` (line 177) gives `stalled` the `fail` tone; the
`node_status` caption (lines 311 to 321) gives it the `failure` tone and
the title `taken when this node stalls (the same rejection repeated), goes
to X`. `journal.ts` `attempt_finished` adds `stalled` when set. The HTML
report's `node_status` edges need nothing (they show the status word).

### Notification

`crates/apb-core/src/notify/mod.rs` `Transition` (line 25): `Stalled`
(`"stalled"`), appended to `ALL`, not in `DEFAULT` (opt-in, like
`host_task` and `supervisor`, so no existing subscription changes).
`crates/apb-engine/src/notify/transitions.rs` `open_transitions`
(line 72): an `attempt_finished` with `stalled: true` and `item: None`
opens `Stalled` at its seq; the run's end does not close it (it is news
about a step, not a wait), and the send ledger keeps it to one send.
`notify/notice.rs`: a `stalled` composer (subject `<playbook> stalled at
<node>`; lines `Step <node> of run <id> of <playbook> stopped retrying:
the same rejection came back N times in a row.` and `Reason: <the
attempt's rejected_for, first 200 characters>`), `command_for` returns no
command for it (line 125). Regenerate `web/src/lib/consts.gen.ts`
(`NOTIFY_TRANSITIONS`). `docs/NOTIFY.md`: a table row.

### Validator

In `validate/retry.rs`, called next to `check_retry_delay` (mod.rs line
390) with `// V174, V175`:

| Code | Severity | Rule |
|---|---|---|
| V174 | error | `stall_after` outside 2 to 10 (on a node or `defaults`), or on a node that is not an `agent_task`, or on a list node |
| V175 | warning | a `node_status` edge with `equals: stalled` whose node has no effective `stall_after` (it never matches), or that comes after an edge from the same source with `node_status` `equals: failure` on the same node (the failure edge is always taken first) |

Docs: `docs/HOWTO-authoring.md` "Attempt failures: kinds, retries and
interruptions" (line 2556): a section "Stopping a retry loop that repeats
itself (stall_after)" with the fingerprint rules, the streak, the routing
pattern and the V174/V175 table; "Human review and conditional edges":
`node_status` takes `stalled`; "The retry briefing" (line 2764): the
fingerprint field.

Wrong implementations to avoid:

1. A new `NodeStatus::Stalled` (an older apb reads `Unknown` and runs the
   node again).
2. Counting the streak in memory (a deferred retry or a resume resets it).
3. Comparing raw reasons (a timestamp or a temp path defeats the stall).
4. Counting a transient or interrupted attempt as breaking the streak, or
   as part of it.
5. Stalling on a fatal rejection, or before `stall_after` repeats.
6. `failure` no longer matching a stalled node.
7. V09 satisfied by `success` plus `stalled`.
8. The stall checked on the report path only.

## D6. Bundled content

1. `bundled/playbooks/apb-feature-delivery/playbook.yaml`
   (`version: 1.2.3`, bump once to `1.2.4`): `review_correctness`,
   `review_security`, `review_tests` declare
   `{ name: role, type: enum, values: [<their role>], required: true }`,
   `{ name: blocking, type: number, required: true }`,
   `{ name: findings, type: array, required: true }`; `collect` declares
   `{ name: open_count, type: number, required: true }`,
   `{ name: findings, type: array, required: true }` and `roles` plain.
   `decompose` stays untyped: its route deliberately reads an unreadable
   `decomposed` as "not decomposed" (the comment above the
   `decompose_route` edges).
2. Its scenarios: the `collect` outputs `findings: "[c1 open]"` become a
   real JSON array (`'[{"id": "c1", "disposition": "open"}]'`); no other
   scripted value changes. New
   `evals/scenarios/reviewer-malformed-output-retries.yaml` (start from
   `single-change-clean-panel.yaml`): `review_security` attempt 1 writes
   `blocking: "none"`, attempt 2 writes `blocking: "0"`; the run succeeds
   at `assessed`. The second answer is used only if the first was
   rejected, so the default `unused_answers` check proves the rejection.
3. `bundled/playbooks/apb-code-review/playbook.yaml` (`1.1.2` to
   `1.1.3`): the three reviewers declare `role` (enum of their role,
   required), `coverage` plain, `findings` (array, required). New
   `evals/scenarios/reviewer-prose-findings-fails.yaml` (start from
   `reviewer-fails.yaml`): both attempts of `review_security` write
   `findings: "no issues found"`; expect `status: { review_security:
   failed }`, `outputs: [{ node: review_security, matches:
   "^output_contract:" }]`, the report runs and the run succeeds. Confirm
   that the node's terminal output after spent retries starts with the
   rejection reason (`last_msg`, set at `node.rs` line 2830 and returned at line 3511); if it does not,
   match what it is and say so in the report.
4. Follow the bundled-content tests for digests and version sidecars;
   `writes`, `prove_fails_first` and `stall_after` are not used in bundled
   content (proposal, out of scope 4).

## D7. Journal and compatibility

1. New fields: `attempt_finished.rejection_fingerprint`,
   `attempt_finished.stalled`. New `failure_kind` value `write_scope`.
2. One new event, `check_proved`: the proof's red result is the only
   record that a later pass means something, and a green one is no
   attempt; no existing event carries either without faking an attempt.
   It is not a checkpoint and is safe to skip.
3. No new `NodeStatus`. `StatusEq::Stalled`, the typed field form, the
   edge thresholds and the proof keys make an older apb fail to parse the
   playbook, loudly. `writes` and `max_changed_files` would be dropped in
   silence, hence the `write_scope` run guard, which protects the
   resumption of a run this version created and not a fresh launch by an
   older apb (D3 "Run guard"). `stall_after` dropped by an
   older apb only spends the retries the author allowed.
4. `NeedsLook` gains a reason and a count (TypeScript optional);
   `NOTIFY_TRANSITIONS` gains `stalled`.

## Release notes

The change is not tagged here. List the user-visible changes in the
final report: typed and required output fields; numeric thresholds on
`output_field`; write scopes (scoped writers never share a batch, a
partial comparison fails, no cache, and the downgrade limit: an older apb
launching such a playbook ignores `writes`); `prove_fails_first` (an
observation, no cache);
`stall_after`, the `stalled` route and transition; the bundled reviewers
and collector now reject a malformed `blocking`, `open_count` or
`findings` and retry.
