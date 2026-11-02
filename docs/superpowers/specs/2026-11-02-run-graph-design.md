# The graph shows what the run proves

Date: 2026-11-02. Status: accepted, built (second part of the edge identity
work). First part: `2026-11-01-edge-identity-design.md`. Author-facing usage:
`docs/STATS.md` (membership and reasons), `docs/DECISIONS.md` (judge answers
on the run graph).

## 1. Goal

The first part recorded the facts: every edge has an address, every hop
names the edges it crossed, every start, resume and patch journals the digest
of the definition it executes, and the diagnostics sidecar keeps what each
routing decision evaluated. This part reads those facts back and draws them:
the run graph on the run page, the last run over a playbook's graph, judge
answers, "why did this node not run", validation issues in a panel, the
version diff as a graph, and per-node statistics with reasons.

One rule governs every view: a statement is made only when the journal
proves it. Absence of proof reads as "not established" or "unknown", never
as "not taken". Nothing here changes how a run routes, what it journals or
what it counts.

## 2. Definitions in force

A run's journal is cut into segments at every start, resume and patch.
`apb_engine::run_definitions::DefinitionsInForce` resolves each segment by
its own journaled digest and the stored bytes under that digest; it is the
only source of certainty:

1. a boundary without a digest (a run an older apb started) is unknown, even
   when the run's mutable snapshot happens to match some version;
2. a `definition_mismatch` diagnostic is detail for the surfaces, never
   evidence that a segment with a journaled digest and matching bytes is
   unknown;
3. every boundary is checked: a run that executed A, was patched to B, then
   went back to A is not "exactly A".

## 3. Route evidence

`apb_engine::run_outcome::route_evidence` reads the journal once, in three
layers, and the graph and the explanations share it.

1. **Records.** Every `edge_traversed` as written, with its accounting flag
   (counted or not), the segment in force at its seq and the source
   execution that routed it (`identity::routed_by`). A policy hop is
   dispatched first: it is a delivery over the failure policy, never
   resolved through an explicit edge between the same nodes, and its owner
   is a fork when a `branch_failed` naming the same node and target precedes
   it, else `defaults.on_failure`. A record without addresses resolves
   through its pair only when the pair is unique in a known definition.
2. **Deliveries.** Records normalized by segment, source execution and edge,
   so one delivery counts once whatever records describe it: the decision
   record and the counted record of one push, a resume that advanced the
   same execution again, a join push made later, an older journal's counted
   record without addresses. Counted records stay available apart for budget
   inference.
3. **Episodes.** One routing decision per ended execution. Only a complete
   episode supports a negative claim. Complete means: a `route_evaluated`
   diagnostic attributed to that execution; a policy delivery (the policy
   routes only when no edge takes the failure); a one-of edge taken (one-of
   routing takes exactly one edge); a fan-out whose every unconditional edge
   is named or provably spent before the decision; or a source with no
   outgoing edges. One hop of a fan-out after which the driver stopped is not
   complete, so its sibling stays not established.

A diagnostic carries `after_seq`, the journal seq it follows. It is
attributed to the latest execution of its node that ended at or before that
seq (inclusive: a dead end follows its own finish with no hop in between),
only when that execution's ordinal equals the one the record stored, and
only when `after_seq` lies inside the journal read, so a sidecar line written
after the read never explains events the read does not contain.

## 4. The run graph

`apb_engine::run_outcome::graph::RunGraph`:

1. **Edges** of every known segment, each `taken` (with the number of
   distinct deliveries made in that segment only, so a reader combining
   segments sums them: the run canvas over every segment of the shown
   bytes, the overlay over every projected segment), `not_taken` (every execution of its source in the
   segment ended with a complete episode that took another way, or the
   source did not run in a closed segment) or `not_established` with a
   reason and no count. An unknown segment has no edge states.
2. **Nodes**: executions, open ones, race losers, ended executions by status,
   the status the run reports now (liveness included) and the status of the
   latest ended execution.
3. **Traversals**: every record with what it resolved to.
4. **Shown segment**: the latest known segment. The run canvas draws that
   segment's definition (`RunDetail.graph_model`), not the mutable snapshot,
   and matches edge states to canvas edges by address. Without a known
   segment it draws the snapshot without edge states and says why.

The endpoint-based classifier the run page used before (both of two
parallel edges marked walked, edges animated from endpoint statuses when no
hop was recorded) is replaced: edge state comes only from the graph, and
animation decorates an established delivery only.

## 5. Judge answers

Per judge node execution: the decisions journaled inside it (`native`, and
`emulation` for an `emulate` fallback, kept apart), each question's answer
apart from the value the output carries, and the comparison its kind routes
on: a choice's confidence against `min_confidence`, a noul's `p` against
`yes_at`, a score's value against its bands. The view exists without any
decision too: a judge that is off or not configured shows `decided_by`, the
reason and its route. Per judge edge answer: `p` against `min_p`, and whether
routing took it when that is established.

## 6. Why a node did not run

`apb_engine::run_outcome::route_explain` gives every node of a known
definition that never executed one fact per way into it: each incoming
edge, the `defaults.on_failure` route when the node is its handler, each
fork `on_failure` naming it. Each fact has a class (`observed`, `inferred`,
`unknown`) and a code. An edge whose source never ran is observed
`source_not_executed`; a complete episode that took another way is observed
`routed_elsewhere`, or `condition_not_matched` / `budget_spent` /
`loop_exhausted` from its diagnostic; without a diagnostic, a finished run's
last execution of the source is re-evaluated against the state folded up to
its own finish, in its known definition with the as-run availability, which
gives an inferred fact. A policy route reads `no_failure_to_route` when no
execution failed. The node summary is made only when every entry is proven;
otherwise it is `unknown`. Something delivered into a node that did not run
reads `run_ended_first` or `cancelled_by_race`. Unreachable (by the same
policy-aware reachability the validator uses) is inferred.

## 7. Overlay of a run on a version

`GET /api/playbooks/{id}/overlay?workspace=&version=&run=&before=`. A run is
`exact` when it executed the shown version and every segment's digest equals
the digest of that version's stored bytes; otherwise `patched`,
`same_version_other_definition` (run overrides), `other_version` or
`unknown`. The default run is the newest exact one, searched over every run
of the playbook, independent of the page; candidates come in pages of 20,
newest first; `run=` resolves any run. Each known segment is mapped onto the
shown version by its own edge correspondence, and each traversal is
projected through its own segment's mapping; an unknown segment is never
projected.

## 8. Statistics

A node belongs to a run when a known definition of the run declares it or
the run executed it, counted once per run across segments; a node only the
final snapshot names while a definition is unknown is counted apart
(`in_runs_unknown`). `runs` never exceeds `in_runs`. Each member node a run
did not execute adds that run's explanation summary to `not_run`, counted by
class and code.

## 9. Surfaces and redaction

`RunDetail.graph` and `graph_model`, the overlay endpoint, MCP
`run_report.route`, a "route" block in `apb runs <id>` and a textual route
section in the HTML report all read the same `RunGraph`. Every fact is
derived from original evidence; a surface serializes the graph, applies the
owning run's redactor and only then clips long strings
(`RunGraph::present`), so a secret is never left as an unrecognizable
fragment. HTTP and MCP add the graph before their final redaction, the CLI
prints it inside its run-redaction scope, and the overlay endpoint applies
the chosen run's redactor. A read-only process that cannot resolve the run's
secrets gets the graph withheld as a string, which the dashboard shows as
withheld.

`POST /api/validate` validates an editor draft with the checks the
playbook detail runs and writes nothing.

## 10. Dashboard

1. Run page: taken edges solid with a count above one, not taken dimmed, not
   established dotted with its reason and no count; judge answers on judge
   nodes and `p` against `min_p` on judge edges; not-run tooltips with the
   class and the entry facts.
2. Playbook page: a "last run" layer (off by default, remembered per
   browser) with a paged run picker and alignment badges, a "counts" layer
   over the statistics, and an issues panel whose entries select the node or
   edges they name and open the editor there.
3. Editor: draft validation on the parse timer, issues from the last valid
   YAML kept and marked while the YAML does not parse, judge exits per
   question kind, and the version diff as a graph with removed edges as
   ghosts.
4. Statistics card: per node executions, completed, open, failed, race
   losers, zero rows, the membership denominator and reasons by class.

## 11. Tests

Engine: edge states for parallel edges, unaddressed duplicates, a bounded
retry over several executions with a resume re-advance, a policy delivery
beside an explicit edge, a partial fan-out, diagnostic attribution and
bounds, inference, each explanation code, a two-input node, an old journal,
a patched run per segment, judge comparisons per kind, a judge without a
decision, native and emulation evidence, every judge fallback form through a
real run, the overlay alignments and paging, the statistics membership, and
redaction before clipping. Server: draft validation writes nothing, the
overlay and the run detail graph, the HTML route section. Dashboard: unit
tests for the run overlay projection, the issues mapping, judge exits, the
diff graph and the statistics display.
