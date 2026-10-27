# CRM cleanup

Kind: routine. Runnable example: examples/playbooks/crm-cleanup.yaml.

## 1. Purpose

Open deals without an owner are assigned by a rule, and the run proves the
assignment is complete: before 37, after 0. The scenario stands for the
bulk-update routine over an external store, where the hard part is not the
update but the accounting: a partial failure must leave a correct count of
what is left, and "zero" must mean "done", not "the filter broke".

## 2. Initial state of the world

- The CRM holds 137 open opportunities; 37 of them have no owner.
- The rule is declared: every unowned open deal goes to the pool owner, a
  param carrying the owner's id.
- While the run is going, sales closes 3 of the 37 (they stop being open).

## 3. Steps

1. Count and list the open deals without an owner. (agent node, a check:
   twenty list_opportunities, read-only)
2. Assign the rule's owner to each listed deal, one update per deal, keyed
   by the deal id. (agent node, connector write: twenty update_opportunity)
3. Recount the open deals without an owner, the same way as step 1, and
   report what is left. (agent node, a check: read-only)
4. The goal criteria check the three count markers (before, assigned,
   after); "the recount is zero and every deal got the rule's owner" is a
   reader's criterion. (goal criteria)

## 4. Expected outcome

The run reports 37 before, 37 assigned (or fewer, naming the 3 that closed
meanwhile as skipped with a reason), and 0 after. Every open deal that still
lacks an owner after the run is named with the error that stopped it. A
reader can take the three numbers and the skipped list and know the CRM is
consistent without re-querying.

## 5. Failure case

The assign step dies with a network error after 20 of the 37 updates. The
run is resumed the next morning.

- What apb must show: 20 assigned, 17 left (minus the 3 that closed: 14 to
  update), before anything else runs; and after the resume, an honest
  recount that distinguishes "assigned by this run" from "was already
  assigned".
- What must NOT happen: the resume re-deriving its worklist from a stale
  count and skipping deals; the recount reporting 0 because the filter
  broke, with the run claiming success; a deal assigned twice to different
  owners by two overlapping runs.

## 6. Today

examples/playbooks/crm-cleanup.yaml expresses count, assign, recount with
the markers, and a `baseline_check` the engine runs itself: `unowned` reads
the CRM's `totalCount` of open deals without an owner through the twenty
connector's read-only `list_opportunities`, once before the first step that
can act and once at the finish, and compares it with `equals: 0`. The run
outcome shows `37 -> 20, equals 0: not met` after a partial failure, and a
continued run (the next morning's resume) inherits the first run's before
count. Closed:

- A partial failure leaves an engine-checked count of what is left: the
  before and after counts come from the CRM, not from an agent's reply, and
  the comparison is journaled (engine test
  `a_partial_cleanup_reads_not_met_with_the_change_visible`).
- Zero is no longer ambiguous: a read that failed, came back cut short or
  did not hold its count reads unknown, never 0, and an unknown never counts
  as met (engine tests `a_connector_read_that_fails_is_unknown`,
  `a_connector_read_that_is_not_complete_is_unknown_never_zero`).

The gaps that remain:

1. The whole list goes to one agent call per step; there is no per-deal
   update the engine holds a verdict for.
2. No per-deal ledger keyed by the deal id: a resume re-runs the whole assign
   step and cannot skip what the earlier pass did (harmless only because the
   rule is a constant).
3. The 3 deals that closed mid-run are invisible unless the agent happens to
   report them; nothing requires the skipped set to be accounted for. The
   count shows the difference, not which deals make it up.

## 7. Target

The unowned list is a first-class value; each deal is updated per item with
a per-item verdict (assigned, skipped-closed, failed) recorded by the engine
against the deal id; the recount is a check the engine runs and compares
with the before-count, and a zero is believed only when the per-item ledger
adds up. A feature closing these gaps carries an engine test that kills the
assignment after 20 of 37 and asserts the resume accounts for every deal
with no double assignment and an honest final count.
