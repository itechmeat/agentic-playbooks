# List processing and the review panel

Date: 2026-10-27. Status: accepted, built. Scenarios:
`docs/scenarios/inbox-triage.md`, `bulk-reminders.md`,
`document-review-panel.md`.

## 1. Goal

A step that works through a list (messages, rows, findings, reviewers) runs
once per item instead of once for the whole list, and the engine, not the
agent, holds what became of every item. A retry, a pause, a crash, a quota
wait or a loop back repeats only the items that did not pass; a send is
never repeated for an item whose earlier send may have gone out; every run
surface counts the items; and a goal criterion can require that all of
them were handled.

This document records the design as built. Author-facing usage is in
`docs/HOWTO-authoring.md` ("List processing"), the surfaces in
`docs/MCP.md` ("List nodes").

## 2. Schema

1. `list:` is an optional block on a node, additive to schema 2 with no
   migration (`Node.list`, `apb_core::list::ListSpec`). It is allowed only
   on `agent_task` and `script` nodes.
2. `items`: exactly one of `from: { node, field }` and `inline: [...]`;
   `key` (default `id`) names the item field that identifies it; `max_items`
   (default 100, at most 500); `where` with `field` plus `equals`, and/or
   `state` (only when the source is a list node); with both, an item is
   kept only when both hold.
3. `batch`: `size` (default 10), `parallel` (default 1), `retries` (re-runs
   of a rejected item, default 1, at most 5).
4. `item_outputs`: the fields each item's step must write. They replace
   `outputs.fields` as the status-file contract of an item's step.
5. `check`: exactly one of `output: { field, equals }`, `script: <path>`,
   `connector: { name, account?, function, args, accept_when }` and
   `judge: { state, instructions, criteria, min_confidence?,
   on_unavailable }`.
6. `on_needs_look`: `continue` (default) or `fail`.
7. `rerun`: `all` (default) or `not_passed`.
8. A goal criterion gains `check: { type: list, node }` (`GoalCheck::List`).

Items are JSON objects. An id is the key field as a non-blank string or an
integer of at most 128 characters, with no backtick, no separator other
than a plain space and no hidden character (controls, invisible formatting
marks, bidi embeddings, overrides, isolates). `where.equals: null` keeps an
item whose field is present and null; a missing field never matches, and a
field holding an object or an array never matches any `equals`. An item from a list node is that node's
`items[]` entry: its `item` object with the earlier step's outputs added as
`output`, keyed by the entry's `id`. Each item carries a digest of its
canonical JSON (keys sorted), and a visit carries a digest over every id and
item digest in order.

## 3. Item state machine

The states are `pending`, `running`, `passed` and `needs_look`; the last
two are terminal for a visit. One pure function
(`apb_core::list::verdict::transition`) maps what happened to an item, its
round, its `retries` and whether it had its reconcile run, to where it goes:

| Event | Transition |
|---|---|
| check `accept` / `accept_with_note` | passed (with a note for the second) |
| check `reject`, round below `retries` | requeued at round + 1 |
| check `reject`, rounds spent | needs_look `rejected` |
| check could not tell | needs_look `check_unknown` |
| step failed, a `none` send unknown | needs_look `unknown_send` |
| step failed, a `key` or `marker` send unknown, not reconciled yet | requeued in the same round as the reconcile run |
| step failed, a `key` or `marker` send still unknown after it | needs_look `unknown_send` |
| step failed, no unknown send | needs_look `failed` |
| interrupted (a `--now` pause, a quota pause, a crash) | pending, same round |

An item whose JSON is larger than 8 KiB needs a look (`item_too_large`)
before it ever runs. A step that succeeded without writing every
`item_outputs` field is rejected with a note naming the fields. A list
without a check accepts every item whose step succeeded.

There are two retry layers. Inside a round, an item's step goes through the
node's ordinary attempt loop (`max_retries`, profile fallbacks,
infrastructure backoff, the retry briefing). Across rounds, `batch.retries`
re-runs an item its check rejected.

## 4. Journal

Three variants are added, each safe to skip up to the next checkpoint (the
node's `node_finished` or a `run_paused` always follows):

1. `list_started { node, total, filtered_out, items_digest, batch_size,
   parallel, reopened }`: a visit begins (or a failed one is reopened by a
   supervisor retry), before any item runs.
2. `list_batch_started { node, batch, items }`: the ids admitted into a
   batch, numbered from 1 per visit.
3. `list_item_settled { node, item, state, verdict?, note?, reason?,
   output?, round, carried, check?, digest? }`: where one item went.
   `state` is `passed`, `rejected` (it runs again in `round`),
   `needs_look` or `interrupted` (back to pending; `reason: reconcile`
   marks the free reconcile run). `output` keeps the item's declared
   outputs as compact JSON, at most 2 KiB. `carried` marks an item a
   `rerun: not_passed` visit kept; `digest` is set on a pass so a later
   visit can tell whether the item changed.

The item-scoped events that already existed gain an optional `item` field,
`#[serde(default)]`: `attempt_started`, `attempt_exited`,
`attempt_finished`, `connector_call`, `send_intent_recorded` and
`decision_made`. An item's journal (`Journal::for_item`) stamps them, so no
caller builds an item-scoped event by hand. Every item of a batch settles
before the next batch starts.

`ListFold` (`apb_engine::list::ledger`) folds a node's latest visit (or the
one before it) from these events and the item attempts. It is the one
reader behind resume, supervisor retry, `rerun: not_passed`, progress, the
goal criterion and every surface.

## 5. Execution key

Items of one node run at the same time, so everything that used to be keyed
by the node is keyed by `ExecKey { node, item }` (`apb_engine::exec_key`):
the open-attempt set, liveness and reaping, the attempt a connector call
binds to, the send ledger's identity and effect scope, attempt memory and
the per-attempt file names. A per-attempt stem is `<node>-<attempt>`
without an item, exactly as before, and `<node>.i-<12 hex of the item id's
digest>-<attempt>` with one, so an item id never reaches a path. An item's
JSON and its step output for a script check live under
`list-items/<node>/<hash>.json` in the run directory. A send's effect scope
for an item also starts after the item's last pass.

## 6. Runner

1. The drive dispatches a list node to `scheduler::list` (one entry,
   `scheduler/dispatch.rs`); every other node keeps `execute_node`, which
   is also what each item runs.
2. A list node never joins a parallel batch of the drive: it runs alone on
   the sequential arm. Its items run on its own worker pool of
   `min(batch.parallel, max_parallel)` threads.
3. The visit loads its items (`source`): a source that is missing, not an
   array, holds duplicate or malformed ids, or exceeds `max_items` fails the
   node before any item runs, then `where` filters them.
4. It decides the visit: an open visit with the same items digest is
   resumed; a failed visit with the same digest that a supervisor
   `node_retry` action followed is reopened; anything else starts a new
   visit, which under `rerun: not_passed` first carries the items that
   passed in the previous visit with an unchanged digest.
5. An `agent_task` prompt is rendered once per visit. Each item's step gets
   that text followed by a final `## item` section: the ordinal and total,
   then one fenced JSON data block with `id` and `item`, introduced as data,
   never instructions, behind a fence longer than any backtick run in it.
   On a later round, or a later visit under `rerun: not_passed`, the block
   adds `previous_round`, `previous_outputs` and `check_note` (the last two
   cut to 2 KiB each).
6. Batches admit at most `batch.size` ids; a requeued item goes to the tail
   of the queue. After each batch a `run_progress` event carries the counts
   and a label ("12 of 43 processed, 11 passed, 1 needs a look").
7. When every item is terminal the node publishes `total`, `processed`,
   `passed`, `passed_with_note`, `needs_look`, `filtered_out` and `items`
   (each `id`, `state`, `note`, `item`, `output`). It fails under
   `on_needs_look: fail` when an item needs a look, else succeeds.

Each item's step runs with `APB_ITEM_ID` in its environment; a script step
also gets `APB_ITEM_FILE`. An item's step does not get the decision-model
completion check when the list has a check, and tier routing is not asked
per item.

## 7. Checks

1. `output`: the named item output must equal the value, with the
   semantics of the `output_field` edge.
2. `script`: a script under the version's `scripts/`, run from the pinned
   copy with `APB_ITEM_ID`, `APB_ITEM_FILE` and `APB_ITEM_OUTPUT_FILE`,
   120 seconds at most, prints `{ verdict, note }`. A non-zero exit rejects;
   no verdict is a check that cannot tell.
3. `connector`: one `read_only` function, called by the engine with `args`
   templated from `item.*`, `item_output.*` and `params.*`; `accept_when`
   holds a dot path and exactly one of `equals` or `exists`. The check is
   its own binding site, `<node>#check`, in the one walk over binding
   sites (`connector/sites.rs`), so grants, effects, trust, the run gate,
   preflight and the eval refusal all see it. A failed read cannot tell.
4. `judge`: one choice question, `verdict`, over the rendered `state`,
   asked through the decision runner like a judge node and journaled as a
   `decision_made` stamped with the item. An answer below `min_confidence`
   counts as none, and `on_unavailable` decides what none means.

A check that cannot tell never accepts.

## 8. Send-once

A list repeats steps by design, so every write function a list node is
granted must declare `idempotency` (V88). The send ledger keys a send by its
node and item, so one item's send never answers another's. Without
`--intent` an item's send is labelled `item:send`, one send per function per
item; distinct sends of one function take distinct `--intent` labels. A
call naming an item with no running attempt of its own is refused. A failed step
with an unknown `key` or `marker` send gets one free reconcile run (the key
replays, the marker is looked up). An unknown `none` send leaves the item
needing a look; a supervisor retry skips it until a person resolves the
send; after either resolution the next retry runs the item again, a
repeat of a send resolved as sent is answered `already_sent`, and the
item's check decides. To make the inbox and tracker examples
legal list steps, `asana.create_task` declares `{ mode: none, result_ref:
data.gid }` and `imap.mark_read` declares `{ mode: none }`; imap functions,
like smtp ones, may declare only `none`. These declarations apply outside
lists too.

## 9. Interactions

1. Pause: admission checks the control channel as well as the stop flags,
   so no item starts after a pause request returned. A drain pause lets the
   running items finish; a `--now` pause kills them (`attempt_finished
   interrupted`, back to pending). The visit stays open and the node gets no
   `node_finished`.
2. Quota: one item's quota pause stops admission, lets the other running
   items finish and pauses the run with the item's `until`; the resume
   continues the visit.
3. Crash: a dead item attempt is reaped under its item and runs again
   alone.
4. Stop: the visit ends cancelled with its open items left open.
5. Supervisor: a failed visit is reopened only after a `node_retry`
   supervisor action; the passed items stay, the items that need a look run
   again except `item_too_large` and an unresolved unknown send.
6. Loops: each later visit of a list node is a new visit, under `rerun`.
7. Progress: a running list node earns its weight times the share of its
   items processed, and is not counted as a cycle's iterations.
8. Host execution mode refuses a playbook with a list node at run start.
9. Not allowed on a list node: interactive, `continue_session` either way,
   `cache`, `success_check`, `protect` with `batch.parallel` above 1.

## 10. Surfaces

1. `run_status` and `run_report`: a `lists` block, one entry per list node
   that started a visit, with its counts, `batch`, `batches` and items
   (`id`, `state`, `round`, `verdict`, `note`, `reason`, `attempts`).
   `run_status` lists at most 50 items that need a look per list;
   `run_report` lists every item.
2. Run outcome: one line per list under the headline; `lists` with each
   list's counts and how it was checked; an `item_check` evidence entry per
   list once an item was processed; an `item_needs_look` gap per item that
   needs a look (at most 30, then one summary entry); a
   `self_reported_items` gap for a list without a check; `needs_look`
   gains `list_items`. A list node is not also reported as a self-reported
   node.
3. Goal: a `list` criterion passes when the latest visit processed every
   item and none needs a look; before the finish the surfaces show where
   the list stands. `enforce` applies to it like any non-manual criterion.
4. Progress report: the counts per list node.
5. Dashboard: the run detail carries `lists`; the run page shows a card per
   list (counts, batch, an item table filtered by state) and the graph shows
   `processed/total` on the list node.
6. CLI: `apb runs <id> --items [--node N] [--state ...]`, one sanitized line
   per item.
7. Evals: list nodes run; a connector item check is refused like any
   connector site; the list event types are valid `events` check names; a
   `list` criterion counts for `checks.goal: required`.

## 11. Validator codes

| Code | Level | Rule |
|---|---|---|
| V85 | error | placement and numbers (section 9.9, zero size or parallel, retries above 5, `max_items` outside 1 to 500, a key that is not an identifier) |
| V86 | error | the source (one of `from` and `inline`, a source guaranteed to finish first, a declared field or `items` of a list node, `where` shapes, usable inline items) and `{{item.*}}` / `{{item_output.*}}` in any prompt or `workdir` |
| V87 | error | the check (one source, `output` field among `item_outputs`, script path, read-only connector function, `accept_when`, templates limited to `item.*`, `item_output.<declared>` and `params.*`) |
| V88 | error | a granted write function without `idempotency` |
| V89 | error | a judge check's criteria keys, `min_confidence`, `on_unavailable` |
| V93 | warning | no check; `rerun: not_passed` on a node in no cycle |
| V94 | warning | `batch.parallel` above the playbook's `max_parallel` |
| V95 | error | a `list` goal criterion naming a node without `list:` |

A list node's output fields count as declared for V46, whatever its
`outputs.fields` say.

## 12. Prompt cache

Templates of a list node may not read the item (V86) because the shared
part of the prompt must be byte-identical for every item: providers cache a
prompt prefix, so when items differ only in a short final section the
shared instructions can be served from that cache for every item after the
first. The item reaches the step as data in the `## item` section, and the
places that legitimately need item values (the check's connector args,
`accept_when.equals` and judge state) get their own template scope.

## 13. Review panel

Two shipped examples express the document review panel scenario.

1. `review-panel-list.yaml`: one list node whose inline items are the
   reviewers, an `output` check on `blocking`, `rerun: not_passed` and
   `on_needs_look: fail`; a `max_traversals: 2` edge to the fix, a fallback
   to `human_review`, a separate `accepted` finish for a person's
   acceptance, and a `type: list` goal criterion.
2. `review-panel.yaml`: the same plan from plain nodes, as a template for
   authors who cannot use a list. Each reviewer has a `condition` gate that
   reads the collector's last state (clean to the next gate, open to a
   re-review continuing the reviewer's session, no state to the first
   review). The reviewers run one after another through the gates: with a
   fork and a `join: all` collector inside the loop, the collector fired
   before a reviewer being re-run had finished. The round bound is
   `max_traversals: 2` on `fix -> gate_correctness` with a fallback to
   `human_review`, and acceptance ends at its own `accepted` finish.

The list form is the recommended one: the engine holds each reviewer's
state, so a reviewer cannot be dropped by a collector; adding a reviewer is
one item instead of a gate, two nodes and five edges; the outcome names the
open reviewer; and the reviewers run side by side. Engine tests run both
examples end to end (`review_panel_test.rs`).

## 14. Compatibility

Every new event and field is additive and skip-safe. An older apb reads a
playbook with `list:` but ignores the block (the node would run once for the
whole list), so a list run must be started and resumed with this apb or a
newer one. Runs without a list node write exactly the events, keys and file
names they wrote before.

## 15. Deferred

1. List nodes in host execution mode.
2. Sharing a fork's slot pool between list items and other running nodes,
   instead of a list running alone with its own pool.
3. A profile per item and warm per-item sessions across rounds.
4. A `marker` idempotency mode for asana.
5. A form editor for `list:` in the dashboard.
6. A paginated per-list API route for very long lists.
7. List metrics in `apb stats`.
