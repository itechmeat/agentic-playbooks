# Review triage

Kind: code work. Runnable example: examples/playbooks/review-triage.yaml.

## 1. Purpose

A change needs a review before it merges. An agent reviews, a judge node
classifies the review with a decision model, and the graph routes on the
verdict: fix, or done, or a person. The scenario stands for "route on a
judgment" and for the panel-style flows that judge nodes enable. Its known
gap is the loop that is missing: one review, one fix, and the fix is never
looked at again.

## 2. Initial state of the world

- A change described in the run instruction, carrying exactly one blocking
  defect (an off-by-one in a date parser) and two style notes.
- The decision model of the machine is either configured or not; the playbook
  must run either way.

## 3. Steps

1. A review agent reads the change and lists every blocking defect with file
   and line, or says there is none; the reply starts with a BLOCKING count.
   (agent node)
2. A judge classifies the review output: clean, needs_fix, or unclear, plus a
   risky question about deleted tests, secrets and migrations. (judge node)
3. On needs_fix, a fix agent repairs the listed defects. (agent node, output
   field edge)
4. On clean, the run ends. (finish node)
5. When the decision model is unavailable or unsure, a person decides.
   (human_review gate, on_unavailable route, review_status edges)

## 4. Expected outcome

With a decision model: a clean review ends the run, a needs_fix verdict
produces a fix commit that addresses every listed defect, and every answer is
journaled as a decision. Without one: the review lands on the human gate and
the run still completes.

## 5. Failure case

The fix node removes the off-by-one and, in the same edit, deletes the test
that covered it. The run ends success: nothing looks at the fix.

- What apb must show: that the fix was never re-reviewed, in the run report
  and on the run surface, so "no open blocking defect" is never claimed for an
  unchecked fix.
- What must NOT happen: the run reporting the goal ("no open blocking defect,
  either found-none or fixed") as held when the fix itself introduced one.

## 6. Today

examples/playbooks/review-triage.yaml covers one round exactly as above. The
gaps:

1. One review and one fix, no second review round: the fix edge goes straight
   to done, so a bad fix ends the run as success.
2. A second round is expressible only as a bounded loop re-entering the same
   review node, and this example does not use it, because the judge state
   would have to be rebuilt by hand per round (what the fix changed, what the
   previous round said).
3. The run context keeps only the latest run of each node, so a round-two
   review cannot see its own round-one findings without plumbing the history
   back in manually.

For several reviewers, each reviewing again only while its own findings
are open, see [document-review-panel](document-review-panel.md) and its two
examples, examples/playbooks/review-panel-list.yaml (one list node with
`rerun: not_passed`) and examples/playbooks/review-panel.yaml (the same
plan from plain nodes); both bound the rounds and hand what is left to a
person.

## 7. Target

The fix loops back through the same review, bounded (at most N rounds), each
round's verdict and fix diff kept in the run report as a list of rounds, and
exhaustion of the bound lands on the human gate instead of success. The judge
sees the round's diff against the previous review. A feature closing this gap
carries an engine test on the initial state above with a fix that introduces
a new defect, asserting the second round runs and the bound routes to the
gate.
