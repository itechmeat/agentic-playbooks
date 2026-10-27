# Document review panel

Kind: code work. Runnable examples: examples/playbooks/review-panel-list.yaml
(one list node, the recommended form) and examples/playbooks/review-panel.yaml
(the same plan from plain nodes and edges). Section 6 compares them.

## 1. Purpose

One document or change is looked at by several narrow reviewers at once: a
correctness reviewer, a security reviewer, a style reviewer. The author
fixes what they found, and only the reviewers that found problems look
again. Rounds are bounded; what survives the bound goes to a person. The
scenario stands for per-item state inside a loop: the loop does not advance
uniformly, because each reviewer's item (their open findings) finishes at a
different round.

## 2. Initial state of the world

- A 6-page design document in the working tree, with 2 planted correctness
  errors and 1 planted security concern.
- Three reviewer profiles exist, each with a narrow role prompt (correctness,
  security, style) and no knowledge of the others.
- No review has happened yet; round count is 0.

## 3. Steps

1. Review: one reviewer per scope reads the change and returns its blocking
   findings. (a list node with one inline item per reviewer, or one review
   node per reviewer behind a gate)
2. Keep each reviewer's state: open or clean, with its remarks. (the list
   node's item states, or a collector agent node)
3. Author: fix the findings, or answer them with a reason. (agent node with
   `protect` on the tests)
4. Re-review: only reviewers whose findings were open look again, at their
   own findings as fixed by the author. (`rerun: not_passed`, or a gate per
   reviewer routing to a re-review node)
5. Bound: at most 3 rounds. (an edge with `max_traversals: 2`)
6. Escalate: findings still open after the bound go to a person; a
   person's acceptance ends at its own finish node, apart from a clean
   panel. (human_review gate)

## 4. Expected outcome

After at most 3 rounds, the 2 correctness errors and the security concern
are fixed and re-checked by the reviewer that raised them. The style
reviewer, who found nothing in round 1, is never asked again. The final
document version and a panel report (per reviewer, per round: findings and
dispositions) are attached to the run.

## 5. Failure case

The security reviewer asks to rename a module; the style reviewer asks to
keep the old name because the document references it. The author node
silently picks one side and both reviewers' next round passes.

- What apb must show: the conflict, as two open findings on the same object,
  routed to the human gate with both statements visible.
- What must NOT happen: an infinite ping-pong between author and reviewers;
  a fabricated "resolved" verdict written by the author node on the
  reviewers' behalf; a reviewer asked again after its findings closed (wasted
  rounds), or one silently dropped while findings are still open.

## 6. Today

Two examples ship, each driven end to end against the shape above by its
own engine tests (crates/apb-engine/tests/suite/review_panel_test.rs): at
most three review rounds and two fixes, each fix reviewed in the round
after it.

**The list form** (examples/playbooks/review-panel-list.yaml) is one list
node, `panel`, whose items are the reviewers:

```yaml
list:
  items:
    inline:
      - { id: correctness, focus: "wrong results, missed cases, broken error handling" }
      - { id: security, focus: "injection, secrets, unsafe input handling, missing permission checks" }
      - { id: style, focus: "readability and the project's conventions" }
  batch: { size: 3, parallel: 3, retries: 0 }
  item_outputs: [blocking, findings]
  check: { output: { field: blocking, equals: "0" } }
  rerun: not_passed
  on_needs_look: fail
```

A reviewer passes when its own `blocking` output is the text `0`; the
engine compares the field, so a reviewer must write exactly `0`. Any open reviewer fails the node, a bounded edge
(`max_traversals: 2`) routes to the fix, and the fix routes back to the
panel. On the next visit `rerun: not_passed` carries the reviewers that
passed and runs only the open ones, each told in its item section what it
reported last round and what the check said. Once the bound is spent the
fallback edge hands the rest to a person. The goal's `type: list`
criterion counts the reviewers.

**The graph template** (examples/playbooks/review-panel.yaml) builds the
same rounds from plain nodes. Each reviewer has a `condition` gate, a review
node and a re-review node that continues the reviewer's session. The gate
reads the collector's last state: clean goes straight to the collector,
open goes to the re-review, and no state yet (the first round) goes to the
first review. Every round forks into the three gates, so the reviewers run
side by side, and the collector is a `join: all` on every edge into it: it
waits for each reviewer that runs again this round, and a reviewer its gate
kept out counts as arrived. A collector agent keeps every reviewer's state
and remarks; the edge into the fix carries `max_traversals: 2` with a
fallback to `human_review`, so the third round's open findings go to a
person instead of a fix nobody reviews.

Why the list form is the recommended one:

1. The engine keeps each reviewer's state. In the template the state is a
   collector agent's output, and a collector that forgets a reviewer
   silently drops it; in the list form every reviewer is an item whose
   state the journal holds, so none can be dropped between rounds.
2. Adding a reviewer is one more inline item. In the template it is a gate,
   a review node, a re-review node, seven edges and a change to the
   collector's prompt and outputs.
3. The outcome names the open reviewer (`panel/security`, with the check's
   note) as an item that needs a look, and the run surfaces count the
   panel; the template's outcome has only the collector's own report and
   its `OPEN REVIEWERS:` marker.
4. The list's pool caps how many reviewers run at once (`batch.parallel`);
   the template's fork runs every reviewer of a round at once, up to
   `max_parallel`.

The template still shows how far plain nodes and edges go, and keeps a
reviewer's own session across rounds where the engine can continue it,
which the list form does not (a later round of a reviewer gets its earlier
outputs and the check's note in its item section instead).

Still open in both: the panel report keeps each reviewer's latest round,
not every round's findings and dispositions; and a conflict between two
reviewers is not detected as such, it reaches a person only through the
bound.

## 7. Target

Reviewer branches are keyed by reviewer id, each carrying its own status
(open findings or done). The loop traverses only the branches still open,
skips satisfied reviewers, counts rounds per branch and overall, and routes
the survivors to the human gate at the bound. The panel report lists every
round's findings and dispositions. A feature closing this gap carries an
engine test that plants the document above, makes one reviewer finish in
round 1 and another in round 3, and asserts the finished one is not re-run
and the bound escalates.
