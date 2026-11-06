# Node contracts: typed outputs, numeric edges, write scopes, proven checks, stall exit

Design record for the node-contracts change. Read with
[docs/HOWTO-authoring.md](../../HOWTO-authoring.md), which documents the
authoring side of everything below.

## Why

A node's output and its effect on the world are only as useful as the
promises apb can check about them. Five concrete gaps:

1. A declared output field is a name and nothing more. A missing field only
   journals `output_fields_missing` after a success, and a wrong shape is
   never noticed: a reviewer that writes `blocking: "two"` or
   `findings: "none"` succeeds, and the edge or the list that reads the
   field quietly does the wrong thing.
2. An `output_field` edge compares one string with `equals`. There is no way
   to say "below 80" or "at least 10000": an author enumerates values or
   reaches for a judge node and a decision model.
3. Nothing bounds what a writing node may change. `protect` lists paths that
   must not change; globset has no negation, so "change only
   `src/export/**`" cannot be written, and there is no limit on how many
   files an attempt touches. `read_only` is all or nothing.
4. A `success_check` that passes before the agent ran proves nothing, and
   apb never looks. A test that was already green, or a "the invoice is
   filed" check against a ledger that already holds it, turns every attempt
   into an unearned pass.
5. A retry loop that gets the same rejection every time spends every retry
   and every fallback step. The only early stop was `retry_advice: enforce`,
   which needs a decision model, a stored threshold and the machine's
   enforce setting.

Each gap matters for code work and for a routine that is not code: a
reviewer's `blocking` must be a number while an invoice extraction's
`amount` must be a number and its `currency` one of two values; a repair
routes on `coverage` below 80 while a deal of `amount` at least 10000 routes
to a manager; a fix may touch only the files the plan names while a drafting
step may write only `drafts/**` of a shared folder; the regression test must
fail before the fix while a ledger lookup "this invoice is in the ledger"
must be false before the filing step; the same failing test after each retry
is the same shape as the same "mailbox full" refusal after each retry.

## D1. Typed and required output fields

`outputs.fields` and a list's `item_outputs` accept, per entry, either
today's plain name or `{ name, type, values, required }` (`FieldDecl`, an
untagged enum, so a plain list parses, serializes and keys the node cache
exactly as before). The type is one of `string`, `number`, `bool`, `array`,
`object`, `enum`.

When an attempt reports success, the engine runs
`apb_core::output_contract::check` over the output. A required field absent
or null, or a value of the wrong type, rejects the attempt through the
existing non-fatal rejection path: `rejected_for` starts `output_contract:`,
the retry briefing quotes the reason, and when the retries are spent the
node fails with the reason as its output. Tolerant readings: a `number`
field takes a JSON number or a string that is exactly the text of a JSON
number (`"80"`, `"-1.5"`, `"1e3"`; never `" 80"`, `"+80"`, `"inf"`, `"NaN"`,
never `str::parse::<f64>`); a `bool` field takes `true`, `false` or those
two strings; `array` and `object` take the value or a string that parses as
one; an `enum` field takes the text of a string, number or boolean that is
one of `values`, compared exactly. A reason names the field, the expected
type and the JSON kind it got, and never the offending value, whole or in
part: a cut value no longer matches the full secret the scrubber looks for,
and the reason is journaled and sent in the retry prompt.

A `script` node's stdout is checked the same way and a mismatch fails the
node at once (a script has no retry), on the real run and on a scenario's
scripted answer alike. A list item's step is checked against `item_outputs`,
never against the node's `outputs.fields`. An optional typed field left out
is not rejected; the `output_fields_missing` warning covers it. For a
declaration with a typed entry the status-file note names each key with its
type and whether it is required; for a plain-only declaration the note is
byte for byte today's. A cache hit is not checked: the declaration is part
of the node's canonical JSON, so a changed contract is a different key.

V167 (error) refuses a malformed typed entry: an empty name or one holding
`.`, `{`, `}` or whitespace, a name declared twice where one of the two is
typed, `values` without `type: enum`, an `enum` with no `values`, an empty
value or a repeated value. V168 (warning) names a reader that cannot read a
typed field's type: an `output_field` `equals` on a `bool` field other than
`true`/`false`, on a `number` field no number reads as, on an `enum` field
outside its `values`, or on an `array` or `object` field; a threshold on a
field typed other than `number`; a list's `items.from` field typed other
than `array`; a `no_new_items` field typed other than `array`; a `writes`
template reading a field typed other than `array` or `string`.

## D2. Numeric thresholds on `output_field` edges

`EdgeCondition::OutputField` carries exactly one of `equals`, `lt`, `lte`,
`gt`, `gte` (V169, error: none, several, or a threshold that is not a finite
number). `Threshold` is a newtype over `f64` compared and hashed by its
bits, so the condition stays a routing key. A threshold reads the field with
`output_contract::numeric`; anything it cannot read (a boolean, null, an
array, an object, a non-numeric string, an absent field, an output that is
not JSON) is a non-match, never an error: routing falls through to the next
edge. The dashboard graph and the HTML report label a threshold edge
`coverage < 80` (`<=`, `>`, `>=`), and the editor keeps the comparator on
save. The route diagnostics record for a threshold edge names the text it
read and the number it made of it.

## D3. Write scope

An `agent_task` or `script` node declares `writes` (a list of globs relative
to the node's directory, or one template that renders a JSON array of
strings or a newline list) and `max_changed_files`. The template renders
once per execution, before the first attempt, through the same render the
node's prompt and workdir use; a render that comes out empty or names a path
outside the node's directory refuses the node before anything starts. At
most 200 entries.

After every attempt apb compares the node's tree with its state before the
attempt, reusing the read-only check's tree comparison (`Before::compare`,
split from `finish` so the two checks share one walk and one set of limits).
Only file changes count: the repository state kinds (HEAD, refs, index,
control files) are not files, and a commit of an in-scope file is in scope.
A changed file outside `writes`, or more changed files than
`max_changed_files`, fails the node at once with `failure_kind: write_scope`
and an output that starts `write_scope:` and names at most five paths and
the count, with no retry, no fallback, no deferral and no restore: a retry
would take a fresh before state that already holds the stray change and pass
while it stays, so the check would launder it. To have changes put back, an
author combines `writes` with `protect`. The evidence of every check is
written under `runs/<id>/writes/` (0600, paths scrubbed, never content).

The observed boundary is the read-only check's: the files git tracks or
would track, every `.gitignore`, initialized submodules, nothing below a
symlinked directory. `writes` does not constrain git-ignored files, the
run's own `.apb` files, paths outside the node's tree, or external effects.
A comparison cut short by a limit is an unknown result and fails the node
whatever changes were seen: what the limit left out may hold the violation.

A node with a scope never shares a concurrent batch with another writer,
scoped or not (one writer's stray write could pass as the other's in-scope
one); unscoped writers keep today's batching. A scoped node is never served
from the node cache, whether or not validation ran (`cache::prepare` returns
no context). The check runs whatever the isolation tier: bubblewrap binds
directories and cannot enforce globs. A scenario run checks nothing.

V170 (error) refuses a misplaced or malformed declaration (the keys on any
node but an `agent_task` or `script`, on a `read_only` node, on a list node,
beside `cache`; an empty list; more than 200 entries; an entry outside the
node's directory or not a valid glob; a template that is not exactly one
`{{...}}` reference to `nodes.<id>.output.<field>` of an existing node or
`params.<name>` of a declared param; `max_changed_files: 0`). V171 (warning)
names two writers that can run side by side and neither declares a scope.
V172 (warning) names the same pair shape where at least one declares
`writes` or `max_changed_files`: they run one after another.

A definition with `writes` or `max_changed_files` carries the
`write_scope` run guard: an apb that does not know the name refuses to
continue a run this version created. It does not protect a fresh launch by
an older apb, which drops the unknown node keys and runs the node unchecked;
the docs and the release notes state this downgrade limit.

## D4. A check that must fail first

`success_check`'s script map form takes `prove_fails_first: true` and
`on_green_before: fail | skip`. The proof runs once before the first attempt
of the node's effect scope (since its last success, the window the retry
briefing reads), inside that attempt's placement, before the read-only or
scope state is taken and before anything is spawned. Not before a retry, a
fallback step, a deferred retry, a resume, a supervisor retry after a
settled node, or on a list item, and never in a scenario run: the tree may
already hold that attempt's work, and the check would wrongly read green. A
crash after the proof leaves an open `check_proved` that a resume consumes
without running the script again: a red proof goes on to the attempt, a
green one settles as it said.

The proof is an observation: the node's tree state before, the script (in
the attempt's sandbox, the same call the success check makes), the tree
state after. A changed tree, or a comparison cut short by a limit, fails the
node at once with `check_changed_tree:` and no attempt; `check_proved` is
journaled with `changed_tree: true` and decides nothing. No
`attempt_started` or `attempt_finished` is journaled for the proof: it is
not an agent attempt and must not count in attempts, durations or retries.
What the check does outside the tree (a network call, a write elsewhere) is
not observed: a shell exit code cannot prove their absence.

A green proof with `fail` (the default) fails the node with
`check_green_before:` and no attempt, and the run outcome needs a look for
it (`1 check green before work`). With `skip` the node settles `succeeded`
with `already done:` and no attempt; V173 refuses `skip` on a node that
declares `outputs.fields` or `results` or whose output an edge condition
reads: a skipped node must not claim an output it did not produce. V173 also
refuses `on_green_before` without `prove_fails_first: true`, a `{ script }`
map with neither `scope` nor the proof (write the bare string), the proof
on a list node or a `script` node (only a plain `agent_task` proves, so the
engine would skip it there in silence), and the proof on a node with
`cache` (a cache hit settles the node with no proof in
this run, so the engine also treats such a node as uncached even when
validation was skipped).

## D5. Stall exit

Every non-fatal rejection journals `attempt_finished.rejection_fingerprint`:
the normalized reason's SHA-256, first 16 hex characters. The normalization
(`apb_core::rejection`) replaces the run directory and id with `<run>`,
timestamps with `<time>`, UUIDs with `<id>`, a maximal run of at least 7 hex
characters holding a digit with `<hex>`, every remaining digit run with `#`,
and collapses whitespace: the same rejection normalizes to the same text
whether or not `stall_after` is set.

With `stall_after: N` on an `agent_task` (or in `defaults`, effective for a
non-list agent task only), when a rejection's fingerprint equals the
fingerprint of the N minus 1 attempts of the node before it in its effect
scope, the node stops retrying: `attempt_finished` carries `stalled: true`,
and the node ends `failed` with an output that starts `stalled:` and names
the rejection. No retry, no fallback step, no deferral. The streak is read
from the journal (an engine decision, so an unreadable journal is an error,
never a silent streak of 0), skipping transient and interrupted attempts
(they never finished their work: they neither join nor break the streak), so
a deferred retry, a resume or a supervisor retry that meets the same
rejection stalls again at once. Fatal rejections carry neither field and
never stall.

Routing: `StatusEq` gains `Stalled`. A `node_status` condition with
`equals: stalled` matches a node whose latest execution ended stalled; the
node's status stays `failed` in the journal, so `equals: failure` still
matches it and an older apb resumes it as an ordinary failure. V09/V39 keep
requiring explicit `success` and `failure` coverage: `stalled` covers
neither. The run state folds a `stalled` set (a later execution of the node
that is not a stall clears it), and the dead-end reason names a stalled
edge that a cap blocked.

Notification: a new opt-in transition `stalled` (not in the defaults, so no
existing subscription changes), opened by a stalled attempt, never closed
by the run's end (the send ledger keeps it to one send). The notice names
the step, how many times the same rejection came back and the rejection
itself.

V174 (error) refuses `stall_after` outside 2 to 10 (on a node or
`defaults`), on a node that is not an `agent_task`, or on a list node. V175
(warning) names a `stalled` edge whose node has no effective `stall_after`
(it never matches), or that comes after an edge from the same source with
`node_status` `equals: failure` on the same node (the failure edge is always
taken first).

## D6. Bundled content

The reviewers and the collector of `apb-feature-delivery` and the reviewers
of `apb-code-review` declare typed fields (their `role` an enum of their
role, `blocking`/`open_count` a number, `findings` an array, all required;
the collector's `roles` stays plain), with one new scenario each:
`reviewer-malformed-output-retries` (a reviewer writes a word where a number
was declared, the rejection retries, the run succeeds) and
`reviewer-prose-findings-fails` (a reviewer writes prose where an array was
declared, both attempts reject, the node fails with the contract reason as
its terminal output, the join delivers the failed reviewer and the run
succeeds stating the gap). The `collect` scenario strings made real JSON
arrays. `writes`, `prove_fails_first` and `stall_after` are not used in
bundled content: no bundled node has a fixed file scope today.

## Journal and compatibility

New optional fields `attempt_finished.rejection_fingerprint` and
`attempt_finished.stalled`; one new event `check_proved` (safe to skip, not
a checkpoint); one new `attempt_finished.failure_kind` value `write_scope`;
a run guard name `write_scope`; `NeedsLook` gains the `checks_green_before`
reason and count (TypeScript optional); `NOTIFY_TRANSITIONS` gains
`stalled`. No new `NodeStatus`. A playbook that uses none of the new keys
behaves exactly as today: same parse, same canonical node JSON, same prompts,
same events. Typed fields, thresholds, the proof keys and `stall_after`
make an older apb fail to parse the playbook, loudly; `writes` and
`max_changed_files` would be dropped in silence, hence the run guard.

## Out of scope

Nested schemas (a flat type per field catches the failures seen);
enforcing a write scope in the sandbox (bubblewrap binds directories, not
globs); putting back a change outside the scope (the comparison keeps
digests, never content: `protect` is the mechanism that restores, and an
author combines both); a write scope on a repair step whose open findings
name locations rather than files; `prove_fails_first` for a marker check or
the decision-model completion check (both judge the attempt's output, and
before the first attempt there is none); a stall exit for a list item's
check rounds (bounded by `batch.retries`, they settle `needs_look`); a new
`NodeStatus` value `stalled` (an older apb would read it as unknown and run
the node again); two scoped writers side by side, even with disjoint globs
(a whole-tree comparison cannot tell which process made a change);
constraining git-ignored files, the run's own `.apb` files, paths outside
the node's tree, or external effects with `writes`; protecting a fresh
launch by an older apb, which drops the unknown node keys.
