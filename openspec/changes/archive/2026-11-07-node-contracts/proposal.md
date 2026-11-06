# Proposal: node contracts

## Why

A node's output and its effect on the world are only as useful as the
promises apb can check about them. Reading apb against that idea found five
concrete gaps (line numbers from origin/main a118ae4e):

1. A declared output field is a name and nothing more.
   `apb_core::schema::NodeFiles.fields` is a `Vec<String>`
   (`crates/apb-core/src/schema.rs` lines 344 to 352). A missing field only
   journals `output_fields_missing` after a success
   (`crates/apb-engine/src/scheduler/cache.rs` `settle`, lines 760 to 772),
   and a wrong shape is never noticed: a reviewer that writes
   `blocking: "two"` or `findings: "none"` succeeds, and the edge or the
   list that reads the field quietly does the wrong thing. The bundled
   `apb-feature-delivery` routes on `collect.open_count` and reads
   `findings` arrays that no check ever looks at.
2. An `output_field` edge compares one string with `equals`
   (`schema.rs` lines 2152 to 2156, matched in
   `crates/apb-engine/src/parallel.rs` lines 83 to 91). There is no way to
   say "below 80" or "at least 10000": an author enumerates values or
   reaches for a judge node and a decision model.
3. Nothing bounds what a writing node may change. `protect`
   (`docs/HOWTO-authoring.md` "Protected paths") lists paths that must not
   change; globset has no negation, so "change only `src/export/**`" cannot
   be written, and there is no limit on how many files an attempt touches.
   `read_only` (`crates/apb-engine/src/scheduler/node/read_only.rs`) is all
   or nothing.
4. A `success_check` that passes before the agent ran proves nothing, and
   apb never looks. The check runs only after a reported success
   (`crates/apb-engine/src/scheduler/node.rs` `success_check_rejection`,
   line 263). A test that was already green, or a "the invoice is filed"
   check against a ledger that already holds it, turns every attempt into
   an unearned pass.
5. A retry loop that gets the same rejection every time spends every retry
   and every fallback step. `attempt_finished.rejected_for`
   (`crates/apb-engine/src/event.rs` line 309) holds the reason, but nothing
   compares one attempt's reason with the next. The only early stop is
   `retry_advice: enforce` (`schema.rs` line 870), which needs a decision
   model, a stored threshold and the machine's enforce setting.

Each gap matters for code work and for a routine that is not code:

| Gap | Code example | Non-code example |
|---|---|---|
| 1 | a reviewer's `blocking` must be a number and `findings` an array | an invoice extraction's `amount` must be a number and `currency` one of `EUR`, `USD` |
| 2 | route to a repair when `coverage` is below 80 | route a deal of `amount` at least 10000 to a manager's approval |
| 3 | the fix of a planned change may touch only the files the plan names | a drafting step may write only `drafts/**` of a shared folder, at most one file |
| 4 | the regression test must fail before the fix | a read-only lookup "this invoice is in the ledger" must be false before the filing step, and true means it is already filed |
| 5 | the same failing test after each retry | the same "mailbox full" refusal after each retry |

## What changes

1. Typed and required output fields (capability `typed-outputs`):
   `outputs.fields` and a list's `item_outputs` accept, per entry, either
   today's plain name or `{ name, type, values, required }`. A mismatch
   rejects the attempt through the existing non-fatal rejection path, so
   `rejected_for` and the retry briefing name it. Validator codes V167 and
   V168.
2. Numeric thresholds on `output_field` edges (capability
   `numeric-edges`): `lt`, `lte`, `gt`, `gte` as alternatives to `equals`,
   exactly one per condition. A value that is not a JSON number or a
   numeric string is a non-match. Validator code V169.
3. A write scope (capability `write-scope`): `writes:` (globs, or one
   template that renders a list) and `max_changed_files:` on `agent_task`
   and `script` nodes, checked after every attempt with the read-only
   check's tree comparison. A violation, or a comparison a limit cut
   short, fails the node at once. A scoped writer never shares a
   concurrent batch with another writer, and a scoped node is never
   cached. Validator codes V170 to V172.
4. A check that must fail first (capability `proven-checks`):
   `success_check: { script, prove_fails_first: true, on_green_before:
   fail | skip }` runs the script once before the first attempt of an
   effect scope, as an observation: a check that changes the tree fails
   the node. Green before the agent ran fails the node (default) and
   marks the run as needing a look, or with `skip` settles the node as
   already done without running the agent. Validator code V173.
5. A stall exit (capability `stall-exit`): every rejection gets a
   normalized fingerprint on `attempt_finished`; with `stall_after: N` the
   node stops retrying once the same fingerprint repeats N times in a row,
   ends `failed` as stalled, routes on a new `node_status` value
   `stalled`, and can notify through a new opt-in transition `stalled`.
   Validator codes V174 and V175.
6. Bundled content uses (1): the reviewers and the collector of
   `apb-feature-delivery`, the reviewers of `apb-code-review` and the
   decomposition's `decomposed` and `subtasks`, with one new scenario per
   playbook.

## Out of scope

1. Nested schemas (an array's item type, an object's fields). A flat type
   per field catches the failures seen (a prose `findings`, a word in
   `blocking`); a nested schema language is a much larger contract that
   list sources and judge nodes already check where they need it
   (`apb_core::list::items::items_from_output`).
2. Enforcing a write scope in the sandbox. bubblewrap binds directories,
   not globs (`crates/apb-engine/src/sandbox/view.rs`), so a glob scope can
   only be checked after the attempt, like the detected read-only tier.
3. Putting back a change outside the scope. The tree comparison keeps
   digests, never content (`apb_core::fingerprint::TreeState`); `protect`
   is the mechanism that copies and restores, and an author combines both.
4. A write scope on `apb-feature-delivery`'s `fix`. Its open findings name
   locations, not files, and a repair routinely adds a test file the
   implementation never touched (the `fix` prompt and the delivery skill's
   Repair row); a scope from `implement.changed_paths` would fail correct
   repairs. No bundled node has a fixed file scope today, so `writes` ships
   unused in bundled content and is covered by engine tests.
5. `prove_fails_first` for a marker check or the decision-model completion
   check. Both judge the attempt's output, and before the first attempt
   there is no output to judge.
6. A stall exit for a list item's check rounds. They are bounded by
   `batch.retries` and settle `needs_look`, already a distinct visible
   state (`apb_core::list::verdict`). An item's own attempts (a rejected
   item step) do get fingerprints and the stall rule, through the same
   attempt loop.
7. A new `NodeStatus` value `stalled`. An older apb reads an unknown label
   as `Unknown` (`crates/apb-engine/src/state.rs` `from_label`, lines 79
   to 92), which is not finished (`is_finished`, line 49), so it would run
   a stalled node again on resume. The node ends `failed` and the stall is
   a fact of its last attempt.
8. Two scoped writers side by side, even with disjoint globs. A
   whole-tree comparison cannot tell which process made a change, so one
   writer's stray write could pass as the other's in-scope one. Scoped
   writers run one after another until writes can be attributed to the
   process that made them.
9. Constraining git-ignored files, the run's own `.apb` files, paths
   outside the node's tree, or external effects with `writes`. The tree
   comparison does not observe them
   (`crates/apb-core/src/fingerprint/tree_state.rs` lines 1 to 40).
10. Protecting a fresh launch by an older apb. It drops the unknown node
   keys and runs the node without the scope; the `write_scope` run guard
   only stops an older apb from continuing a run this version created.

## Impact

1. Crates: `apb-core` (schema: `OutputField`, the edge comparators,
   `writes`, `max_changed_files`, `stall_after`, the success check form,
   `StatusEq::Stalled`; new `output_contract.rs`, `write_scope.rs`,
   `rejection.rs`; validator modules `output_types`, `write_scope`,
   additions to `edges`, `nodes`, `retry`, `graph`, `templates`,
   `node_io`, `list`, `guarded`; notify `Transition::Stalled`),
   `apb-engine` (rejection path, new `scheduler/write_scope.rs`,
   `scheduler/prove.rs`, `scheduler/stall.rs`, `batch_class`,
   `read_only::Before`, status file note, state fold, parallel matching,
   diagnostics, HTML report label, run outcome, notify), `apb-server`
   (ts contract), `web` (edge labels and tones, the output fields input,
   journal lines, the needs-a-look counts).
2. Bundled content: `apb-feature-delivery` and `apb-code-review` (typed
   fields, one scenario each, the `collect` scenario strings made real
   JSON arrays), their version bumps.
3. Journal: new optional fields `attempt_finished.rejection_fingerprint`
   and `attempt_finished.stalled`; one new event `check_proved` (safe to
   skip, not a checkpoint); one new `attempt_finished.failure_kind` value
   `write_scope`; a run guard name `write_scope`.
4. Behaviour changes for the release notes: none for an existing playbook
   that does not use the new keys. A plain field list, its status-file
   note, its cache key and its warning are unchanged. The bundled
   reviewers and collector now reject a malformed `blocking`, `open_count`
   or `findings` and retry, where they used to pass it on. A node with a
   write scope fails at once, with no retry, when it changes a file outside
   its scope or the comparison is partial, and its change stays in the
   tree. An older apb launching a playbook with `writes` ignores it.
