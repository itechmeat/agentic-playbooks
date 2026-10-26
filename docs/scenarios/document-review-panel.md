# Document review panel

Kind: code work. Runnable example: docs only in this PR, because the schema
cannot express the panel loop yet (see section 6).

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

1. Fan out: one reviewer node per scope, in parallel, each reading the
   document and returning its findings with severity. (agent nodes, fork,
   max_parallel)
2. Collect: merge the findings into one list. (join: all, collector agent
   node)
3. Author: fix the findings, or answer them with a reason. (agent node)
4. Re-review: only reviewers whose findings were non-empty look again, at
   their own findings as fixed by the author. (conditional re-entry per
   reviewer)
5. Bound: at most 3 rounds. (loop edge with a traversal cap)
6. Escalate: findings still open after the bound, or a reviewer conflict,
   go to a person. (human_review gate)

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

Cannot be expressed today. The parallel first round is a plain fork and is
fine; the panel loop is not, for these reasons:

1. No per-item state across loop rounds: which reviewer still owes a look,
   and what each said last round, has nowhere to live that the loop can read.
2. A bounded loop re-runs the whole body; there is no "skip the nodes whose
   item is done" per reviewer.
3. The run context keeps only the latest run of each node, so a round-2
   reviewer cannot see its own round-1 findings without hand-rolled plumbing.
4. There is no condition form for "this node runs only when its own item is
   still open".

The example for this scenario is therefore docs only. Its closest expressible
cousin is examples/playbooks/review-triage.yaml, one reviewer, one round.

## 7. Target

Reviewer branches are keyed by reviewer id, each carrying its own status
(open findings or done). The loop traverses only the branches still open,
skips satisfied reviewers, counts rounds per branch and overall, and routes
the survivors to the human gate at the bound. The panel report lists every
round's findings and dispositions. A feature closing this gap carries an
engine test that plants the document above, makes one reviewer finish in
round 1 and another in round 3, and asserts the finished one is not re-run
and the bound escalates.
