# Proposal: inbox waits

## Why

This is a change of the 1.0 sequence (people-in-the-loop,
before-running-alone, typed-inbox-reads, start-rules-and-schedules,
eval-doubles, inbox-waits, incremental-lists-and-session-fork,
mcp-tool-connectors, mcp-http-and-surfaces, one-point-oh-fixes,
one-point-oh-freeze), with `wait-park` merged before it. It is read on
main efae5822, where `typed-inbox-reads` (#287), `people-in-the-loop`
(#288), `before-running-alone` (#289) and `wait-park` (#290) are merged.
It is rebased on main 3bb424be, where `eval-doubles` (#291) is merged,
and uses one thing that change added, the storage redactor
`RunRedactor::for_storing`, marked "on main since eval-doubles" where
it appears. Nothing here needs `start-rules-and-schedules`.

History: this change and `eval-doubles` were drafted as one change,
`evals-and-inbox-waits`, and split after its third design review round.
The closure pass of 2026-11-20 rebuilt it around a journal-only position
(design "Cut"); the third pass of the same day put the wait under the
connector trust of a binding site, cut batching and stopped storing
event bodies (design "Cut in the third pass"); the fourth pass limited
the ended-visit rule to inbox waits and fixed the torn start, the
deadline, site id collisions and the gap's node; the fifth fixed the
taint of a reader placed before the wait, the receipt stamp, gaps
another node recorded and read retries (design "Review record",
"Fourth pass", "Fifth pass").

The gap: a run cannot wait for a matching inbox event. `WaitFor` has
`Timer` and `Webhook { key }` only (`schema.rs` line 2259); the wait arm
of the drive (`crates/apb-engine/src/scheduler.rs` lines 2647 to 2754)
ends with an empty output and never reads an event. The documented way
is an agent polling `inbox_read` in a loop (`docs/CONNECTORS.md` lines
454 to 456), which spends an agent turn per poll.

| Decision | Code example | Non-code example |
|---|---|---|
| D4 inbox waits | a run answers each review comment on one pull request: a loop of `wait_for: inbox` (taking `action` and the comment fields) plus an agent, left by an `output_field` edge when `action` is `closed`; a deploy node waits for the CI status event of its commit with `timeout_seconds: 1800` | a run waits for one customer's next reply (taking the sender and the subject) and loops until an `output_field` edge finds the exact subject `accepted`, or 3 days pass |

## What changes

Inbox waits (capability `inbox-waits`): `wait_for: { type: inbox,
connector, account, match, types, take }` with `timeout_seconds` as
today.

1. The wait is one more binding site, `<node>#inbox`: the connector and
   exactly the one account, no function. The run gate's existing trust
   check, the permit and the manifest's connector snapshot cover it like
   any binding; the connector must be installed and trusted and the
   account approved. The drive and the park waker check the manifest
   grant, the live digests and their approval before they read, so a
   resume checks again.
2. The wait keeps its position in its own run's journal: the
   `through_seq` of the node's latest `inbox_wait_read` line. The node's
   first visit starts at the log's head, journaled with `wait_started`.
   It holds no consumer, acknowledges nothing and writes nothing outside
   its run. A start line not followed by its `wait_started` (a torn
   write) is superseded: the next drive writes a new start pair at the
   current head.
3. A visit ends at its first matching event or at the timeout, with one
   write: an `inbox_wait_read` line and the existing `wait_signalled` or
   `wait_timeout`. No window, count or closing condition: several
   events are a loop around the wait. Receipt decides the timeout: an
   event matches only when it was stored (its seq assigned under the
   store's lock, where its `received_at` is now stamped, so stamps are
   monotonic with seq) before the wait's due time on the run's
   paused-interval clock, so a late poll neither loses an event
   that arrived in time nor takes one that arrived after.
4. The output holds the event's seq, receive time and type and the
   values of the fields the playbook declares in `take`, read with the
   baseline observation reader and scrubbed through the fail-closed
   storage redactor. No event body is stored; a declared field can still
   hold a secret apb does not know, which the docs state.
5. Retention: the wait holds no cursor; events removed above its
   position are journaled as the existing `inbox_gap` under the wait
   node's id (a range another node already recorded is journaled again
   for the wait), and the position
   never advances before that line is in the journal.
6. A failed read of the log is never cached: the drive and the waker
   read again at their next poll; the first head read is retried within
   the engine's bounded infrastructure retries (5 s, then 30 s).
7. A long wait with nothing matched parks through the merged wait park
   with the timer park's own reason `wait (<node>)` and its timeout as
   the wake time, after a progress line. The park's existing waker also
   reads the account's log at each poll and resumes the run once an
   event of a listed type, or a gap, is stored above the position.
8. An inbox wait that ended before its node was finished is finished on
   resume from the journaled end, with no authorization and no read,
   because nothing more is read. Timer and webhook waits keep main's
   behaviour exactly, crash included (main's gap there stays a
   pre-existing note), so a run without an inbox wait writes the journal
   main writes in every state.
9. V284 (the block), V285 (a connector that is not installed or has no
   webhook block), V286 (a warning: `types` on a connector that names no
   event type), V287 (two binding sites with one id, such as a node
   `a#inbox` beside the inbox wait `a` or `a#check` beside the list
   node `a`). Eval cases refuse an inbox wait; scenarios script it as
   any wait.
10. The park reads "waiting at `<node>` until <T> at the latest" on
   every surface, for timer and inbox waits alike.

## Plan items changed against the code

1. Plan 3.3 D4 item 2 named a run-scoped consumer acknowledged after the
   node succeeds. Cut: the run's journal is the position (design "Cut"
   items 1 and 2); start-rules made the same decision for its rules.
2. Plan 3.3 D4 item 2 ("releases the driver like PR 2 D3") is the wait
   park with other arguments: the same reason, request and waker, with
   one more check in the waker (design D4 "Parking").
3. Plan 3.3 D4 named a quiet window and a batch. Cut in the third pass:
   one event per visit (design "Cut in the third pass" item 1).
4. Size: 49 changed files (design "Code map"), down from 51.

## Out of scope

1. Collecting several events in one visit (a loop around the wait
   expresses it).
2. Scripted inbox events in eval cases and scenarios (an eval case
   refuses an inbox wait; a scenario scripts its end).
3. `from: stored` (every stored event regardless of when the wait
   started).
4. List values in `take`.
5. An indexed read of the account log; every read of it is whole, as
   on main.
6. History and state outside the run (plan-2 rows U49 and U56): after
   1.0.
7. Incremental lists and the session fork: the change
   `incremental-lists-and-session-fork`.

## Impact

1. Crates: `apb-core` (`schema.rs`, new `inbox_wait.rs`, new
   `validate/inbox_wait.rs`, `validate/mod.rs`, `validate/guarded.rs`,
   `validate/node_io.rs`, `connector/sites.rs`, `connector/inbox.rs`
   `log_stamp`, a public `lost_prefix` and the receipt stamp under the
   store's lock, `connector/resolve.rs`
   `names_event_type`, `untrusted.rs`, `eval.rs`, `lib.rs`), `apb-engine`
   (new `inbox_wait/`, `connector/inbox.rs` `journal_gap_in`, `event.rs`,
   `state.rs`, `scheduler.rs`, `scheduler/journal.rs`,
   `scheduler/wait_park.rs`, `scheduler/control_apply.rs`,
   `pause/waker.rs`, `pause/display.rs`, `next_action/deadlines.rs`,
   `next_action/verdict.rs`, `next_action/tests.rs`, `deadline_hooks.rs`,
   `lib.rs`), `apb-mcp` (`run_wait`'s park text), `apb` CLI (the early
   resume note), `web` (the park line). `apb-server` and the run gate's
   code are not touched.
2. Bundled content: none.
3. Journal: one new event type `inbox_wait_read`, not a checkpoint; an
   older strict reader refuses a journal while a visit's line is not yet
   followed by a checkpoint, and its stop still goes through. No new
   field on any existing event, no new control kind, no new guarded
   feature, no manifest mark; `wait_park` is also named for a definition
   with an inbox wait.
4. Manifest: the existing `connectors` and `connector_grants` record the
   wait's connector, account and site.
5. State outside runs: none.
6. Validator codes V284 to V287 of the reserved V280 to V299, free on
   3bb424be and against every change in flight (design D0).
7. Size: 49 changed files; 13 of them are also touched by
   `eval-doubles`.

## Decisions taken in the closure pass

No question is left for the owner or the implementer.
