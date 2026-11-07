# Proposal: loops and graphs that check themselves

## Why

Agent work is only as trustworthy as the checks around it, and only as fast
as the waits the graph forces. Reading apb against that idea found four
concrete gaps:

1. A template that reads an array field renders an empty string.
   `{{nodes.<id>.output.<field>}}` goes through
   `apb_core::schema::output_field_value`, which returns `None` for an array
   or an object. The bundled `apb-feature-delivery` asks reviewers and
   `collect` for `findings` as a JSON array and hands
   `{{nodes.collect.output.findings}}` to the reviewers and to `fix`. An agent
   that writes a real array hands `fix` no findings at all. The bundled
   scenarios pass only because they script the array as a string.
2. Read-only review panels run one node at a time. `is_batchable`
   (`crates/apb-engine/src/scheduler/node.rs`) keeps `protect` nodes and
   `read_only` nodes outside `isolation: full` out of every concurrent
   batch. The bundled reviewers use `protect: ["**"]`, so three reviewers
   run one after another everywhere, and `isolation: full` is refused on
   macOS.
3. An open-ended loop (bug hunting, research, review rounds) can stop only at
   its `max_traversals` cap or on the agent's own word. Nothing in apb counts
   whether a round found anything new.
4. A fresh-session retry after a rejected success is never told why it was
   rejected: the reason lives only in memory (`last_msg`), and the retry
   briefing quotes the agent's own summary. Nothing tells the retry to repair
   only what was rejected.

## What changes

1. Templates render an array or object field as compact JSON
   (capability `prompt-templates`). Edge conditions are unchanged.
2. A `read_only` node in a detected tier may share a concurrent batch with
   other read-only nodes, never with a node that can write the tree
   (capability `read-only-batches`). The bundled reviewers that run
   no commands move from `protect` to `read_only: true`.
3. A new edge condition `no_new_items` and a template
   `{{nodes.<id>.seen.<field>}}` (capability `loop-exit`), with validator
   codes V164 to V166.
4. `attempt_finished` gains `rejected_for`; the retry briefing quotes apb's
   rejection reason and adds a fixed repair-scope paragraph after a
   rejection (capability `repair-briefing`). The bundled `fix` prompt and the
   Repair row of the delivery skill say the same.

## Out of scope

1. Lessons written by agents and shown to later runs as rules. The safe form
   already exists: a finding with `location: node:<id>` or `skill:<name>` and
   a `workaround` is a known problem, shown only with a `known_problems:`
   block, after recurrence or a person's confirmation, as facts to check.
   An agent-written rule shown as an instruction would be a persistent
   prompt-injection path.
2. A validator warning for an edge that "carries nothing". Too many read
   channels (`nodes.*`, `run.context`, `run.retro`, `continue_session`, list
   sources, `evidence`, approvals, success checks) make it noisy, and an
   unconditional edge is often a deliberate cost gate.
3. One tree comparison around a whole batch. Batches are pipelined: a
   member is journaled and routed as soon as it ends, so a later batch-wide
   verdict would have to undo a routed success.
4. Batching `protect` nodes. Their restore undoes a sibling's write before
   the sibling's own check runs, so the writer escapes and an innocent node
   is blamed.

## Impact

1. Crates: `apb-core` (schema, new `loop_items.rs`, validator, templates),
   `apb-engine` (context, state, parallel, scheduler batch admission,
   pipeline admission, read-only check, attempt memory, event fields,
   diagnostics, the HTML report's edge label, run view), `apb-server` (ts contract), `web` (graph label
   and round-trip of the new condition).
2. Bundled content: `apb-feature-delivery`, `apb-code-review`, the
   `apb-delivery` skill and their scenarios.
3. Journal: two additive fields (`read_only_violated.concurrent_with`,
   `attempt_finished.rejected_for`). No new event type.
4. Behaviour change for release notes: a bundled reviewer that writes to a
   tracked file now fails at once, with no retry, and its change stays in
   the tree, where `protect` used to undo it and retry. In
   `apb-code-review` the report still runs and names the missing review;
   in `apb-feature-delivery` the review panel fails and a repair round
   starts.
