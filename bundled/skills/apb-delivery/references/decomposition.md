# Decomposing a large change

A large change is split into subtasks before implementation. Decide with the
triggers below, not with a feeling about the size.

## Triggers

Decomposition is mandatory when any of these holds:

1. The expected-result card has more than about five outcomes.
2. The change crosses several layers, for example the domain core, the
   execution layer, the command or API surfaces and the user interface.
3. The change alters a persisted format (stored data, a file format, a
   journal or a schema) and behavior together.
4. The result cannot be reviewed in one pass: a reviewer could not hold the
   whole candidate and the card in view at once.

When no trigger holds, do not decompose. Say which triggers you checked.

## What a subtask is

- An independently verifiable slice of the change.
- It covers its own part of the card: name the card outcomes it delivers.
  Every card outcome belongs to at least one subtask.
- It has its own checks: how each of its outcomes is observed, and the
  project checks that must pass after it.
- It names its dependencies. List the subtasks in an order where each one
  depends only on subtasks before it.
- It is a unit of work and review, not a pull request. Integration and the
  pull request stay as the project already does them.

## Where the breakdown goes

- When the project uses a spec workflow, put the breakdown into that
  workflow's own artifacts: its proposal, design and task files.
- Otherwise put it into the existing task or design record.
- When the project uses a work tracker (issues, a graph tracker or similar),
  the subtasks and their status go there. Otherwise they go into the task
  record.
- Do not create a new mandatory file for it.

## Rules while working through subtasks

- The card from the original request is never rewritten to fit the
  breakdown. A subtask that cannot deliver its part reports the gap.
- Every subtask is reviewed with the apb-review skill right after it is
  implemented, and its confirmed findings are repaired in one self-check
  repair round. A subtask that still fails its checks after that round
  needs a look; it is not repaired further in the same run. A usage limit
  routes the review to the next rung of the fallback chain; it never skips
  the review.
- The full review panel then reviews the integrated result, with at most
  two repair rounds of its own.
- Inside an apb run the playbook owns the subtask state. Do not keep a
  second list of subtask states beside it; a work tracker mirrors it only
  when the project asks for that.
- The final check runs against the whole original card, not only against
  the sum of the subtasks: outcomes that only show once the slices work
  together are checked there.
