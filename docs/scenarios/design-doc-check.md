# Design document check

Kind: code work. Runnable example: examples/playbooks/design-doc-check.yaml.

## 1. Purpose

A design document is about to be implemented, and the project has an
engineering checklist it must meet first. An agent checks the document
against every item, an agent revises it where a MUST item fails, and only
the failed items are checked again. The scenario stands for "a structured
review of a text against a list of rules, fixed by an agent", the code-side
twin of [document-checklist](document-checklist.md).

## 2. Initial state of the world

- A design document `docs/design/cache.md` that names no failure modes and
  has no rollout plan.
- A checklist `docs/checklists/design.yaml` with `items: [{ id, level:
  must | should, text }]`: six items, among them `failure-modes` (MUST) and
  `rollout` (SHOULD).

## 3. Steps

1. A check agent reads both files, gives every item a verdict (`pass`,
   `fail`, `not_applicable`) with a quote from the document, writes
   `check-report.md`, and reports `must_failed`, `should_failed`,
   `unchecked` and `coverage` (`CHECKLIST: <checked>/<total>`). (agent node,
   `outputs.fields`, `results:` keeps the report, `protect` on the checklist
   and the design documents)
2. Items without a verdict get one bounded re-check, then a person.
   (condition node, `output_field` edge, `max_traversals: 1`, fallback edge)
3. No failed MUST item ends the run; SHOULD findings stay in the report.
   (condition node, finish node)
4. Otherwise a revise agent fixes the document for the failed MUST items,
   never touching the checklist, and a re-check looks at those items only,
   at most three revisions in all; a re-check that leaves an item without
   a verdict goes to a person. (agent nodes, `protect`, bounded loop
   edge, condition node)
5. Still failing, a person accepts the open findings (the run ends success
   as `accepted_with_open_findings`) or rejects the document (failure).
   (human_review gate, review_status edges)

## 4. Expected outcome

The run ends `done` with a report in which every item has a verdict and a
quote, or `accepted_with_open_findings` after a person's decision, or
`rejected`. Each check execution keeps its `check-report.md` in a result
snapshot, so the report of every round can be read after the tree moved on.
The goal's marker criterion passes on the `CHECKLIST:` line; the second
criterion is manual.

## 5. Failure case

The check agent skips two items and still reports `unchecked: 0`.

- What apb must show: the report and the snapshot of `check-report.md`, so
  a reader sees the items without a verdict; the quality criterion
  `fail-cites-document` reads the check's output and transcript.
- What must NOT happen: apb claiming coverage on its own. apb does not parse
  the checklist; coverage is what the agent reports, and the example says so.

## 6. Today

examples/playbooks/design-doc-check.yaml runs every step above. The gaps:

1. Coverage is the agent's own count: the checklist is a file the agent
   reads, not a structured register apb checks item by item.
2. `protect` takes static globs, so the example protects `docs/design/**`
   rather than exactly the `document` param, and `docs/checklists/**`
   rather than exactly the `checklist` param: a checklist kept elsewhere is
   not kept from the revise agent.
3. A missing `unchecked` field only warns (`output_fields_missing`); the
   example routes it through the fallback to the re-check and the person.

## 7. Target

A structured rules register the engine reads, so every item's verdict is
counted by apb and an item without one is a gap the run reports by itself,
not a number the agent supplies. Until then the example keeps the agent's
count, the report snapshot and a person at the end of every bound.
