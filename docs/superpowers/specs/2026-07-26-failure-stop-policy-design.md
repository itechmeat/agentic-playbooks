# Failure policy (what an unhandled failure does)

Date: 2026-07-26
Status: approved design, implemented
Depends on: 2026-07-08-workflows-cli-design.md

## 1. Purpose and scope

A playbook that routes every possible failure into one negative finish node
buries its own structure. In `apb-task-implement@1.7.0`: 19 nodes, 34 edges,
and 14 of those edges enter the single `aborted` node. Fifteen edges are
`node_status: failure`, thirteen of which go straight to that finish. Two out
of five edges on the canvas exist only to say "stop here, this went wrong".

The engine already ends such a run correctly when the edge is simply absent.
A node that ends `failed` or `timed_out` with no outgoing edge to take makes
`drive_inner` return `EngineError::Invalid`, and the `drive` wrapper turns
that into `RunError` plus `run_finished(failed)`, so the run is `Failed` and
the reason is folded into `RunState::failure_reason`. Two things are missing:

- The reason is an engine complaint (``node `implement` has no outgoing edge
  and is not finish``), not what the node itself reported.
- Intent is indistinguishable from a mistake. A deliberate stop and a
  forgotten edge produce the same message.

This story adds one playbook-level declaration that turns the second case into
the first, so the failure edges can be deleted. It does not add a new node
kind, a new edge kind, or any per-node field.

Out of scope: any change to how a supervised run handles a failure. That is
also a limit on the feature, not just on the work: a supervised run parks a
failed node for its supervisor and never consults routing at all, so the policy
governs autonomous runs only. Both `docs/HOWTO-authoring.md` and the web
predicate say so, because a playbook that runs supervised (which is what
`playbook_run` does) would otherwise look as if the policy applied to it.

## 2. The declaration

`defaults.on_failure` accepts three values:

- `route` (default): today's behavior in full. A failed node with no route
  onward is an engine error, reported as it is now.
- `stop`: a failed node with no route onward ends the run as `failed`, and the
  reason is the node's own last output.
- a node id: the failure goes to that node, exactly as a `node_status: failure`
  edge into it would have.

```yaml
defaults:
  on_failure: aborted
```

The third value exists because both playbooks in this repository end a failed
run at a `finish` node with a `prompt`: an agent composes an answer in the
user's language, quotes the failing node verbatim, and describes the state of
the branch, the PR and the board. `stop` would replace that with the first line
of the failing node's output, which is a worse run report, not a tidier one.
Pointing the policy at the node keeps the composed answer and still deletes
every edge.

The value is written as a plain string, so `on_failure: aborted` reads the way
an edge target does. `route` and `stop` are the only reserved words; anything
else is a node id, which means a misspelled reserved word becomes a V35
validation error rather than being silently ignored. The policy never applies
to the target itself, so a failure of the handler does not route in a circle.

The default keeps every existing playbook byte-identical in behavior; the
policy only exists once an author opts in. `route` is the serialization
default and is omitted from written YAML (`skip_serializing_if`), matching how
`answer_by` is handled.

An explicit edge always wins. The policy changes nothing for a node whose
failure already has somewhere to go, which is what keeps the meaningful
branches (`review -> fix_review`, `qa -> fix_qa`) on the canvas and removes
only the noise.

## 3. Engine behavior

The rule fires at exactly one point: after a node's terminal status is
journaled, retries and profile fallbacks are exhausted, and edge selection
produced nothing. There
are two such points in the scheduler, since a concurrent batch of parallel
branches has its own dead end; both are covered, and the batch is scanned in
graph order rather than completion order so the branch named is deterministic.

When the node's status is `Failed` or `TimedOut` and the policy names a node,
that node joins the frontier exactly as an explicit failure edge into it would
have. Nothing else changes: the run continues, the handler executes, and its
own outgoing edges (or its being a `finish`) decide the rest.

When the policy is `stop`:

1. Every node still waiting in the frontier is journaled as
   `node_finished(cancelled)`, the same way a winning `any` join cancels its
   siblings. Without this the run report leaves them looking pending.
2. `RunError { node: Some(id), reason }` is appended, where `reason` is the
   first line of the node's last output, capped, and a fixed phrase when the
   output is empty (reusing `failure_detail`).
3. `run_finished(failed)` is appended and the run returns `RunStatus::Failed`.

Stop means stop: the run ends even if another parallel branch is still in the
frontier. A policy whose effect depended on whether a sibling branch happened
to be alive would be unpredictable, and it would let a failure the author
declared fatal produce a `success` run.

A successful node with no outgoing edge stays an engine error under both
policies. `stop` is about failures only.

## 4. Validator

One new rule, V35: `on_failure`, when it names a node, must name one that
exists and is not the start node (which may have no incoming route at all).
Because anything that is not `route` or `stop` parses as a node id, this is
also what catches a typo in a reserved word.

V07 (unreachable node) needs the policy target seeded into its reachability
walk. Without that, the handler a playbook points every unhandled failure at
would read as unreachable the moment its last incoming edge is deleted, which
is exactly the state the policy exists to allow. Nothing else in the graph
rules changes: the cycle check keeps working on real edges only, so the policy
cannot invent a cycle.

V08 (no path to any finish) is a warning that does not fire here: a node keeps
its success path, and that path still reaches a finish node.

An author who chooses `stop` rather than a node gives up the composed failure
answer a `finish` node with a prompt produces. That is a real trade, not a
detail, and it is why the node form exists.

## 5. Web graph

A node whose failure ends the run must say so on the canvas, otherwise the
graph reads as if the failure case was forgotten.

The marker is shown when the policy applies and every outgoing edge of the
node is `node_status: <this node>: success` (a node with no outgoing edges at
all is already a terminal shape and is not marked; it cannot run to a next
node regardless of status). Any other combination stays unmarked: an
unconditional edge is taken whatever the status, a `fallback` edge catches the
failure, an `output_match` edge may match a failed node's output too, a
condition on another node's status says nothing about this one, and none of
those can be decided statically. The marker never claims more than it knows,
and it claims nothing on the handler itself.

Rendering: a small glyph in the node's footer reading either `stop on failure`
or `on failure: <node>`. No new node kind and no change to edge rendering.

The editor parses the playbook YAML client-side, so `PlaybookModel` gains
`defaults.on_failure`; nothing else in the model changes.

## 6. Documentation

`docs/HOWTO-authoring.md` gains a short section on the policy, since that file
is what the MCP `playbook_howto` tool serves to authoring agents. It states the
three values, the "explicit edge wins" rule, that anything which is not a
reserved word is read as a node id, and the trade `stop` makes against a
composed failure answer.

## 7. Testing

Core:

- All three forms round-trip through YAML, the default is omitted from the
  serialized form, and a playbook without the key parses as `route`.
- `target_for` returns the handler for every node but the handler itself.
- V35 for an unknown node and for the start node; `route` and `stop` are not
  read as node ids; a policy target with no incoming edge does not trip V07.

Engine:

- Under `stop`, a failed node with only a success edge ends the run `failed`
  with the node's output as the reason, and the journal carries `RunError`
  naming that node.
- Under a node policy, the same graph reaches that node with no edge into it,
  and journals no `RunError`.
- The handler's own failure does not route to itself: it runs once and then
  produces today's ordinary unhandled-failure error.
- Under any policy, a failed node that has a failure edge takes the edge.
- Under `route` (the default), the same graph produces today's engine-error
  reason, unchanged.
- A stop inside a concurrent batch names the branch first in graph order.
- A stop journals the frontier branches that will never run as cancelled.

Web:

- The marker predicate: `stop` and route forms for success-only edges,
  unmarked for unconditional, fallback, `output_match`, another node's status,
  no-edge, non-executing kinds, the handler itself, and the `route` policy.

## 8. Migrating this repository's two playbooks

Both end a failed run at a `finish` node with a prompt, so both take the node
form rather than `stop`.

- `apb-task-implement`: `defaults.on_failure: aborted`, and the 13
  `node_status: failure` edges into `aborted` are deleted. 34 edges become 21.
  The node stays, still reachable through the `review_status: abort` edge from
  `merge_gate` (a human decision is not a node failure, so the policy does not
  cover it) and now also through the policy.
- `apb-task-brainstorm`: `defaults.on_failure: refused`, and its 3 failure
  edges are deleted. 7 edges become 4, and `refused` keeps composing its
  refusal message with no edge into it at all.

Both keep every branch that handles something: `review -> fix_review`,
`review2`, `qa -> fix_qa`, `qa2`, and the loops between them are untouched.
