# Proposal: incremental lists and session fork

## Why

This is a change of the 1.0 sequence (people-in-the-loop,
before-running-alone, typed-inbox-reads, start-rules-and-schedules,
eval-doubles, inbox-waits, incremental-lists-and-session-fork,
mcp-tool-connectors, mcp-http-and-surfaces, one-point-oh-fixes,
one-point-oh-freeze), with `wait-park` merged before it. It is read on
main efae5822 (1.0.0-beta.7), where `typed-inbox-reads` (#287),
`people-in-the-loop` (#288), `before-running-alone` (#289) and
`wait-park` (#290) are merged; every name in the design was read there.
It is implemented after `eval-doubles` and `inbox-waits` are merged;
nothing here needs either of them, and the design lists the files all
three touch. Two gaps:

1. A list node redoes every item on every run. Its items are loaded and
   run in full each visit (`crates/apb-engine/src/scheduler/list/mod.rs`
   `open_visit`, line 132); the only carry-over is `rerun: not_passed`
   inside one run (`carry_passed`, line 308). A daily routine over 80
   advisories or 300 CRM deals pays for the 75 or 290 it finished
   yesterday, and repeats their outside actions.
2. `continue_session` can only continue the source's conversation
   (`crates/apb-core/src/schema.rs` line 2112, a bare string), which
   writes into the source session; two nodes that start from one source
   in parallel are warned off by V45
   (`crates/apb-core/src/validate/node_io.rs` lines 64 to 76) and start
   cold.

| Decision | Code example | Non-code example |
|---|---|---|
| D2 incremental lists | `dependency-security-fix` with `list.skip: { passed_in: previous_success, unless_changed: [severity, fixed_in] }` works only new or changed advisories, and works them all again once its fix script changes | monthly invoice reminders skip each customer whose reminder passed last month unless its `amount` changed (`unless_changed: [amount]`); a daily follow-up over open deals skips each deal that passed in the last successful run unless its `stage` changed |
| D3 session fork | two review nodes fork the implementer's session in parallel, each reads the full context without writing into it, and V45 no longer warns | two reply drafts in different tones fork one research session and run at once; a contract-drafting session forked into an "English summary" node and a "risk list" node |

## What changes

1. Incremental lists (capability `incremental-lists`). `list.skip: {
   passed_in: previous_success, unless_changed: [fields] }`. apb reads
   exactly one earlier run, the one `run.previous_success` resolves
   through its existing checked lookup, and journals, per list node with
   `skip`, which item ids passed there and the skip digest they passed
   with (`list_skip_read`, one bounded line per node, all of a run's
   lines in one journal write; a drive that finds none records them
   before it drives, and a partial record is refused). The skip digest
   covers the item (its id and its data, or the named fields) and the
   run's work fingerprint: everything the run was configured with,
   taken whole (the definition, inline items included, the scripts tree
   pinned at start, the manifest's execution block, the start request with every param and
   the instruction, what the manifest
   resolved for the node, the decision configuration and the working
   tree's identity), while the contents of the world stay outside. Any
   new playbook version, changed param or instruction, changed profile,
   connector account or decision setting makes every item run again
   once; `unless_changed` narrows what counts as an item change, and
   nothing else does. A visit that carries a supervisor note skips
   nothing. Once a run settled items by a skip, every in-run patch is
   refused. Each new visit settles every item whose skip digest
   is unchanged as passed and `carried`, with `skipped_from: <run id>`
   and its skip digest, as one more opening step before any pilot slice
   or batch, so a skipped item stays skippable in the next success. The
   set is read once (at start, or after the turn for a queued run); a
   resume reads the journaled set, never the history. Surfaces show the
   skipped count. V283 checks the block; a run of such a playbook carries
   a `run_guard` naming `list_skip`; a run whose skipping list ran no
   fresh passed item earns no probation credit.
2. Session fork (capability `session-fork`). `continue_session` also
   takes `{ from: <node>, mode: continue | fork }`; the bare string stays
   and means `continue`. A fork starts the node from a copy of the source
   conversation through the agent's fork form (claude `--resume <id>
   --fork-session`, opencode `run --session <id> --fork`, both fixed from
   the CLIs' help) and never writes into the source. Within one attempt
   loop a retry continues the fork; every re-entry of the node (a
   deferred retry, on the step it resumes, a pause, a driver death, a
   resume) forks the source again, with the retry briefing; no fork id is persisted. An agent
   without a fork form, or an installed CLI whose help lacks the flag,
   starts cold with a journaled reason, as a cold handoff does today.
   Two forks of one source may run in parallel without V45.

## What stays unchanged from main

The `run.previous_success` lookup and its patch refusal, the read-once
points and `after_the_turn`, the visit's opening steps and their crash
recovery (`complete_opening`), the settlement path, the node cache key,
the pilot slice, the batch queue, the early-stop rule, the goal
criterion, readiness, the review channel, the wait park, the deadline,
the concurrency slot, the doctor's free agent check, the retry briefing
and the run guard machinery are reused with other arguments; no
hand-over, control, waker or recovery protocol is added, and nothing
about a fork is persisted. The design's D0 "What stays unchanged from
main" lists each with its site.

## Plan items changed against the code

1. A per-agent `forms.rs` does not exist: each agent module returns a
   shared `Forms` value (`crates/apb-core/src/agents/forms.rs` line 10)
   from its `forms()`. The fork form is a new `Forms.fork` field. codex
   has no headless fork, so codex starts cold.
2. No CLI command prints `session_handoff`; only the dashboard's journal
   does (`web/src/lib/journal.ts` line 138). The "(fork)" mark goes
   there.
3. The skip counts reach every surface through the list view `RunList`
   (one `skip` view shaped like `pilot`) and `ListOutcome`, the values
   `apb runs <id>`, MCP `run_status`, the dashboard and the HTML report
   already read.

## Out of scope

1. An id-only skip (`unless_changed: []` meaning "never look at the
   content"): an item whose content changed would never be worked
   again. Absent `unless_changed` compares the whole item.
2. Reading more than one earlier run, or any run but the one
   `run.previous_success` names (an "any earlier success" source), and
   keeping a pass across a change of the node or its scripts.
3. A skip set that spans projects or reads child runs: a child run
   records an empty set, as it records the none window of
   `run.previous_success`, because two parents of one child would share
   one history.
4. A fork for codex and the other agents: none has a headless fork form
   that writes nothing back; each starts cold with the reason.
5. Carrying a skipped item's earlier output without a bound: the set
   carries each skipped item's output only up to
   `LIST_SKIP_OUTPUT_MAX_BYTES` and the whole set within
   `LIST_SKIP_MAX_BYTES` (D2 "Output"), so its journal line stays
   bounded; an item whose output cannot be carried (clipped when it
   settled, a redaction marker in it, over the per-item cap) or that the
   set's cap leaves out is worked again.
6. Defending the journals of earlier runs against a writer under the
   operator's own account (inside the trust boundary, as for every other
   apb guard).

## Impact

1. Crates: `apb-core` (schema, list types and `fields_digest`, V283,
   the guard, the V45 race rule, preflight, the agents' forms and the
   doctor's flags), `apb-engine` (new `list/skip.rs`, the list visit,
   ledger and view, the work fingerprint beside the node cache, prepare,
   the turn and the drive entry, the patch and rebind refusals, the
   grouped journal write, probation, the journal, the handoff
   and the attempt loop), `apb` CLI (`apb runs <id> --items`, the
   journal corpus rows), `apb-server` (ts contract), `web` (list
   progress, the journal lines, the node editor). `apb-mcp` needs no
   code.
2. Bundled content: none.
3. Journal: one new event type `list_skip_read { node, set }`, one line
   per list node with `skip` (not a checkpoint, safe to skip up to the
   first checkpoint); additive fields `list_item_settled.skipped_from`,
   `list_item_settled.skip_digest`, `session_handoff.mode`, all
   `serde(default)` and written only by a skipping list or a fork, so a
   run that uses neither journals what main journals (a test compares a
   plain list run with a fixture recorded on main). One new run guard
   name: `list_skip`, the fifteenth (`GuardedFeature::ALL` is 13 on main
   and 14 after `eval-doubles`). The fork needs no guard: an older apb
   cannot parse its definition.
4. An older apb: 1.0.0-beta.1 and later refuse either definition at
   parse; 0.25.0 predates list mode: it drops the whole list block and
   runs the step once, skipping nothing and repeating its outside
   actions, which the guard keeps it from doing to a run of this
   release. Every released apb
   reads and stops a run of this release at any time, and refuses to
   pause or resume a skip run at any time: before the first checkpoint
   the unknown `list_skip_read` stops it, after it the run guard that
   follows every checkpoint does. The journal corpus proves it in the
   gate with the old binaries required.
5. New files under a run directory: none. New state: none.
6. Behaviour changes for release notes:
   1. A list node with `skip` settles earlier passed items as passed
      without running them; its output gains `skipped`.
   2. A node can fork another node's session.
7. Validator code V283, the one code this change takes in the V280 to
   V299 family (V280, V281: `eval-doubles`; V282 reserved unused;
   V284 to V286: `inbox-waits`; V287 to V299 reserved).
8. Size: 82 changed files (design "Code map"), about the target of 80.
