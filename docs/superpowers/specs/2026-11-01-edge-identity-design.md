# Edge addresses, route facts and the diagnostics sidecar

Date: 2026-11-01. Status: accepted, built (first part). Author-facing usage:
`docs/HOWTO-authoring.md` ("Edge ids and edge addresses"), `docs/STATS.md`
(executions per node).

## 1. Goal

A playbook edge had no name. Two edges may share both ends, the editor
addressed an edge by its ends, the journal recorded a hop by its ends, and
the version diff folded edges into `from->to` strings. So deleting one of
two parallel edges in the editor deleted both, a validation issue could only
point at a node, and nothing recorded which of two identical edges a run
took or what each condition evaluated to.

This first part gives every edge an address, carries it through the
validator, the diff, the journal and the editor, keeps the facts a later
graph view needs (which definition a run executed at every point, which
execution of a node an event belongs to, how each routing decision went),
and fixes the per-node statistics. Nothing here changes how a run routes:
routing decisions, frontier contents, counted traversal records and budgets
are exactly what 1.0.0-beta.1 produces for the same journal. Where a fact is
not proven, every reader says it is unknown or not established; it never
reads an absence of proof as "not taken".

The graph layers that use these facts (the last run over a playbook, judge
answers on the run graph, the diff as a graph, "why did this not run",
validation issues in a panel, node statistics in the dashboard) are the
second part.

## 2. Edge address

Owner: `apb_core::edge_address`; the dashboard port is
`web/src/lib/edgeaddress.ts`. Both read
`crates/apb-core/tests/fixtures/edge_addresses.json`.

1. `Edge.id` is optional: `[a-z0-9][a-z0-9_-]*`, at most 64 characters,
   unique among the playbook's edges.
2. An address is structural: `{ kind: authored, id }` or
   `{ kind: derived, from, to, ordinal }`, where the ordinal is the 1-based
   position among the id-less edges with exactly those ends, in declaration
   order. Its string form is `id:<id>` or `e:<from>><to>#<ordinal>`, ends
   percent-encoded outside `[A-Za-z0-9_.-]` with upper-case hex. Parsing is
   exact, so equal strings mean equal addresses; `a->b` to `c` and `a` to
   `b->c` render differently.
3. Each addressed edge also carries its index in `edges`, its rank among its
   source's outgoing edges (the rank judge-edge answers are kept under) and
   a check order for display: `parallel` (unconditional), `ordered(n)`
   (conditional, in declaration order) or `fallback`, the same predicates
   edge selection uses.
4. An address names an edge of one definition. Equal strings in two
   versions do not mean the same edge; the diff says which edge corresponds
   to which.

## 3. Validator

`Issue` gains `edges`: the rendered addresses of the existing edges the
issue concerns, empty when none. `node` stays exactly as each rule set it
before. An address is never made up for an edge that does not exist.

| Code | `node` | `edges` |
|---|---|---|
| V06 (unknown node on an edge) | the unknown id | the edge |
| V11 (unbounded cycle) | first cycle member | none |
| V30 (`max_traversals: 0`) | none | the edge |
| V34, shadowed conditional edges | source | the unconditional and the shadowed edges |
| V34, contradictory keys | source | the edges of the group |
| V36, V79 | target | the edge |
| V46 from an `output_field` edge | as before | the edge |
| V53, V55 | the judge node | none |

New rules:

| Code | Severity | Rule |
|---|---|---|
| V121 | error | an edge id that is malformed or used twice (a malformed id is no address, so the issue names no edge for it) |
| V122 | warning | edges with the same ends carry different `max_traversals`: the count is shared by the pair, each edge stops at its own cap against that shared count |
| V123 | warning | a fallback edge of a one-of source is not its last outgoing edge |
| V124 | warning | a one-of source has two unbounded edges with the same ends, condition and fallback flag (the second is never reached); unconditional fan-out is exempt, and so is a bounded edge (`max_traversals`), since the next one is taken once it is exhausted |
| V125 | warning | an `output_field` edge on a judge node's question compares with a value that question never produces, or on its `decided_by` with a value outside `default`, `unavailable`, `<provider>/<model>` (split on the first `/`, so a model id may hold its own) and `emulated:<provider>` |

One view type, `IssueView { code, severity, message, node?, edges }`,
replaces the copies the playbook detail and MCP `playbook_validate` built by
hand; a refused save answers `{ error: "validation", codes, issues }`;
`apb validate` prints the edges after the node.

## 4. Version diff

`VersionDiff` keeps `edges_added` and `edges_removed` and gains:

- `edges`: one correspondence per edge of either side, `{ before, after,
  changed }`. Authored ids match by id; within each pair of ends, edges with
  identical attributes match first in order, then the rest by derived
  ordinal; what is left is added or removed. `changed` names the attributes
  that differ, and `address` when the rendered address moved. Swapping two
  id-less duplicates reads as two correspondences whose addresses moved;
  deleting the first of two reads as one removal plus the second edge moving
  from `#2` to `#1`.
- `edge_order_changed`: for a one-of source, its matched conditional edges
  in their old and new check order when that order changed, both lists by
  old address. A fallback (taken last wherever it is declared) and
  unconditional edges have no check order and are left out.

The identical-attribute pass compares the id too, so an id-less edge kept as
it was reads unchanged even when an authored edge with the same ends goes.

The import preview prints both, and the dashboard's diff lists show them.

## 5. Diagnostics sidecar

Owner: `apb_engine::diagnostics`.

Rule: diagnostics never influence execution; removing all of them leaves
execution identical.

1. Records go to `runs/<id>/diagnostics.jsonl`, never to the journal. No
   event type is added for them, so no released binary meets one.
2. Each line is one JSON object `{ v: 1, kind, after_seq, key, ... }`,
   appended by one write under the sidecar's own lock, in a file created
   0600. The writer refuses a sidecar that is not a regular file and stops
   at 8 MiB.
3. Execution code never opens the file. It hands a finished value to the
   writer, which swallows its own errors (counted, never returned). The
   index of existing keys keeps a record from being written twice after a
   resume and is visible only to the writer and the read surfaces.
4. The reader skips unknown kinds, unparsable lines and a torn last line;
   a missing file means no diagnostics. It never creates, repairs or
   backfills anything. Writes go through the guarded file operations, so a
   read-only process writes nothing.
5. Every text a record carries is scrubbed whole by the run's redactor
   first (the run's secret values, its ancestors' values, the secret
   declarations it keeps after a patch, and the workspace's connector
   secrets) and only then clipped; the writer scrubs every string of the
   finished record once more before it serializes it. Clipping first could
   cut a secret so the scrubber no longer recognizes the fragment.
6. The rule is tested: loops with an exhausted bounded edge, a `max_loops`
   exit, a dead end inside a fork, judge edges and a list node each run
   twice, once with the writer disabled and the sidecar deleted; the
   journals, normalized for timestamps, durations and ids, are equal event
   for event, and so are the final run states.

### 5.1 `route_evaluated`

Written from `advance_frontier` for a one-of decision or a dead end (a dead
end inside a fork branch included), and from the `max_loops` exit, from the
very evaluation that decided. A fan-out writes nothing.

```text
{ v: 1, kind: route_evaluated, after_seq, key, node, execution, item?,
  mode: one_of | dead_end | loop_exhausted,
  checks: [ { edge, rank, kind, result, observed? } ], chosen: [address] }
result: matched | not_matched | exhausted | not_reached | judge_unavailable | shadowed
```

`observed` is the smallest fact that explains a checked edge: the status a
`node_status` condition read, a review decision, an `output_field` value
(scrubbed, then clipped to 64 characters), a judge edge's `p` and `applied`;
nothing for `output_match`. The key is the node and its execution.

### 5.2 `definition_mismatch`

See section 7.

## 6. Identity

Owner: `apb_engine::identity`. Ordinals are derived from event order; no
event gains one.

- Execution `n` of a node ends at its `n`-th `node_finished` and begins at
  the first `node_started` after the previous finish, or at its own finish
  when none was written. Interactive re-invocations, retries and attempts a
  pause interrupted belong to the open execution. A race loser is an
  execution with status `cancelled`.
- An attempt is identified by the seq of its `attempt_started`.
- A list visit starts at a `list_started` that is not a reopen.
- Two attribution rules stay apart. `containing(node, seq)`: the execution
  an event happened inside (attempts, the judge node's decision, questions,
  reviews). `routed_by(node, seq)`: the latest execution of the source that
  ended before `seq`, for hops, judge-edge decisions and route records; a
  finish without a prompt qualifies, and a resume that advances again after
  the same finish attributes to that same execution. A judge-edge decision
  whose `attempt` disagrees with `routed_by` is unknown.

Read surfaces build these views from the one observation of a run
(`next_action::observation`), never by reading the journal again.

## 7. Stored definitions

Owner: `apb_engine::run_definitions`. This section is the contract for the
definition in force at any seq of a run.

1. `runs/<id>/playbook.yaml` keeps its role: a start writes it, a patch
   replaces it, a resume executes whatever it holds. That does not change in
   any binary.
2. Every drive journals the digest of the exact `playbook.yaml` bytes it
   executes, on its own event: `run_started.definition` for the start,
   `run_resumed.definition` for every resume, `patch_applied.definition`
   for a patch, each a `serde(default)` field decoded leniently (no event
   type is added). Beside `playbook.yaml`, each of them also stores those
   bytes as `runs/<id>/definitions/<sha256>.yaml` (write-once, private,
   refusing a `definitions` entry that is a link), best effort: a failed
   store still journals the digest and never stops the start, the resume or
   the patch. A patch stores, then replaces `playbook.yaml`, then journals,
   in that order. A detached driver (`apb __drive-run`) refuses a snapshot
   whose digest is not the one its start journaled. A stored definition is
   a run-local snapshot: it is not a version, publishes nothing, never moves
   `current` and never touches a launch floor.
3. Definition certainty needs positive evidence in the journal itself. A
   segment of the journal (from one boundary, a start, a resume or a patch,
   to the next) is `Known` only when the digest its own boundary journaled
   names stored bytes that hash to it at read time and parse. Every other
   segment is `Unknown` with its reason (`definition_not_journaled`,
   `definition_not_stored`, `definition_unreadable`). The diagnostics
   sidecar is never evidence either way: a lost, full or never written
   sidecar changes nothing. A run started before digests were journaled is
   therefore `Unknown` throughout, and a segment opened by a resume an older
   apb wrote is `Unknown` even when the segments around it are `Known`.
4. A resume compares the digest of the `playbook.yaml` it is about to
   execute with the last journaled digest (a start or resume an older apb
   wrote without one is skipped). On a difference, or when the last patch
   is a `patch_applied` an older apb wrote without a digest, it writes one
   `definition_mismatch` record `{ boundary_seq, journaled?, found, reason
   }` and executes `playbook.yaml` as before; it never restores and never
   refuses. The record is identified by its boundary and the digest found,
   so repeated resumes over one mismatch write it once, while two
   mismatches separated by a successful patch are two records. Surfaces
   show it as detail of the segment it names (`mismatch_recorded`).
5. A patch that crashed after replacing `playbook.yaml` and before
   journaling leaves the segment in force `Known` as the definition its
   drive journaled, which is what that drive executed up to the crash;
   `DefinitionsInForce::playbook_changed` says the file no longer matches.
   The resume that follows journals the new digest, so the next segment
   reads the patched definition: start on A, a crashed patch to B, a resume
   on B and a later patch to C read A, B and C.

## 8. Routing and taken edges

1. `parallel::evaluate_route(playbook, from, state, available)` holds the
   routing rule; it returns the edges chosen and what each outgoing edge did.
   `selected_edges` (live availability) and the re-derivation of a finished
   execution (a bounded edge that execution itself traversed stays
   available) keep their predicates, and every caller calls what it called
   before.
2. `edge_traversed.edges`: the rendered addresses of every selected edge
   that delivered into `to`. The routing decision writes one uncounted
   record per target naming the whole set (two identical edges used to write
   two identical records, which the fold read as one set entry); every
   counted record is unchanged in number and position and names the same
   set. A `max_loops` exit names its fallback; a policy hop names none.
3. `decision_made.edge_addresses`: for judge edges, each question id with
   its edge's address, for display. Routing still reads answers by rank.
4. A patch that adds, removes, reorders or changes any outgoing edge of a
   node whose latest execution has judge-edge answers (applied or shadow)
   that routing has not used yet is refused (`judge_edge_answers_pending`),
   unless `continue_from` runs that node again. Routing has used them once
   the drive moved past that execution's routing: a hop over a declared
   edge it routed (a policy hop reads no answer), or a later `node_started`
   or `run_finished`, so a dead end, which writes no hop, consumes them too.
   A pause or a crash between the answer and routing leaves them pending.
   Older binaries applying a patch keep their behaviour.

All three new fields default when absent and decode leniently: a shape this
binary cannot read (`edges: 5`, `edges: [1]`, `edge_addresses: "x"`) reads as
empty and the event stays readable.

## 9. Statistics

`NodeStats` counts executions from the identity index: `executions` (race
losers left out), `completed`, `open`, `failed`, `cancelled` apart, `runs`
(runs with at least one counted execution), `reentries = executions - runs`
and `in_runs` (runs whose definition contains the node, so a node no run
executed has a zero row). A finish without a prompt counts; a quota-paused
first execution gives no re-entry; a race loser no longer counts in `runs`.
The dashboard's display of these figures is part of the second part.

## 10. Editor

The canvas model keeps every edge key. A canvas edge's id is its address and
an exit handle is `edge:<address>`; the plain handle, where unconditional
edges leave, is `plain`, and an unconditional edge of a node with named exits
names it (without a handle id the canvas would draw it from the first exit).
Exits are numbered by check order with the fallback last, and a node shows
named exits as soon as it has one conditional or fallback edge. Selection
holds an address and the edge's position in `edges`, so two edges sharing an
authored id (a V121 error) stay apart; any YAML edit clears it, and Delete is
off while the YAML does not parse. Deleting removes exactly the one edge at
that position, and only while it still has that address. An alias item
(`- *e`) is the edge it names, as the parsed playbook reads it, so the canvas
and the document number the same edges; deleting an anchored edge moves it to
its first alias first. Dragging from a named exit of a node creates nothing
and says why. A new edge that would be a second id-less edge with the same
ends gets a generated id (none when every edge of the pair has one already);
existing duplicates never get one silently. A new conditional edge goes
before its source's first fallback. The edge panel shows the address, the
condition in words, `max_traversals` and `join`.

## 11. Journal compatibility

| Change | Older reader, resumer, stop |
|---|---|
| `edge_traversed.edges` | dropped; folds identical |
| one decision record per target | the hop set is the same; counted records unchanged |
| `run_started.definition`, `run_resumed.definition`, `patch_applied.definition` | dropped; resume runs `playbook.yaml` |
| `decision_made.edge_addresses` | dropped; routing by rank |
| `definitions/`, `diagnostics.jsonl` | never opened |
| the judge patch refusal | a patch an older binary applies keeps its behaviour |

The journal corpus runs released binaries against journals this one writes
(a bounded duplicate pair, a judge-edge run, a patched run, a crashed
patch resumed and patched again) and asserts the hops, the counted
traversals, the nodes reached and the digests each drive journaled, and runs
this binary on a patch an older one applied.
