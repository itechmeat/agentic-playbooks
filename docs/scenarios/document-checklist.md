# Document checklist

Kind: routine. Runnable example: examples/playbooks/document-checklist.yaml.

## 1. Purpose

A contract, a report or a publication must meet a checklist before it goes
out, and a person, not an agent, edits it. An agent checks every item and
quotes the document; when a MUST item fails, the person fixes the document
and says so, and only the failed items are checked again. The scenario
stands for "an agent reviews, a person edits", with no code and no tests.

## 2. Initial state of the world

- A report `reports/q3-summary.md` whose revenue table names no data source.
- A checklist `docs/checklists/publication.yaml` with `items: [{ id, level:
  must | should, text }]`: nine items, among them `data-source` (MUST).

## 3. Steps

1. A check agent reads both files, gives every item a verdict with a quote,
   writes `check-report.md` and reports `must_failed`, `should_failed`,
   `unchecked` and `coverage`. (agent node, `outputs.fields`, `results:`)
2. Items without a verdict get one bounded re-check, then a person.
   (condition node, `max_traversals: 1`, fallback edge)
3. No failed MUST item ends the run. (condition node, finish node)
4. Otherwise a person edits the document and chooses `fixed`, accepts the
   open findings or rejects the document. (human_review gate with three
   options)
5. After `fixed`, a re-check looks at the failed items only; still failing,
   the person is asked again, at most three rounds, then a final decision;
   a re-check that leaves an item without a verdict goes to the final
   decision.
   (agent node, bounded loop edge back to the gate, fallback to a second
   human_review)

## 4. Expected outcome

The run ends `done`, `accepted_with_open_findings` (success) or `rejected`
(failure). Every check execution keeps `check-report.md` in a result
snapshot, so each round's verdicts stay readable. Only the person is meant
to change the document.

## 5. Failure case

The person chooses `fixed` without editing the document.

- What apb must show: the re-check's report with the same failed items, and
  after the bound the final decision, so a document is never reported as
  meeting its checklist because someone pressed a button.
- What must NOT happen: the run ending `done` while a MUST item still fails.

## 6. Today

examples/playbooks/document-checklist.yaml runs every step above. The gaps:

1. Coverage is the agent's own count; apb does not parse the checklist.
2. The gate cannot tell whether the document changed between rounds; the
   re-check is what finds out.
3. The check agents are told not to edit the document, but nothing
   enforces it: `protect` takes static globs and the document is a param.

## 7. Target

A structured rules register, as in [design-doc-check](design-doc-check.md),
and a gate that can show what changed in the document since the last
check before a person answers.
