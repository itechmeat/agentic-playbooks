# Dependency security fix

Kind: code work. Runnable example:
examples/playbooks/dependency-security-fix.yaml.

## 1. Purpose

The dependency audit reports findings that need code changes. The scenario
stands for measured code repair: record the world before touching it, change
one thing at a time, prove the end state against the recorded beginning, and
be honest about what is left. It is the reference scenario for before/after
evidence: the run must be able to show not only where it ended but where it
started.

## 2. Initial state of the world

- The dependency audit lists 3 findings: 2 high, 1 moderate. Both high ones
  have a compatible upgrade; the moderate one has none.
- The test suite has 2 failing tests that predate the run.
- The working tree is clean, on the base branch.

## 3. Steps

1. Record the baseline: run the audit and the test suite, write down every
   finding and every already-failing test, and cut a fix branch. Change
   nothing else. (agent node, read-only by prompt instruction only, the
   baseline as a `BASELINE:` marker line in the node output)
2. Work the findings one by one, highest severity first: fix, run the
   affected tests, commit, next. Keep a FIXED list and a NOT FIXED list with
   reasons. (agent node, session continuation from the baseline node)
3. Prove the final state: run the audit and the full suite again and compare
   against the baseline; report findings remaining and new test failures, if
   any, by name. (agent node, a check against the baseline)
4. Open exactly one change request carrying the fixed findings, the two
   result lines and the NOT FIXED list with reasons. (agent node, connector
   write: github create_pull)
5. The goal criteria check the baseline, both result lines and the change
   request marker; a reader confirms the unfixed findings carry reasons.
   (goal criteria, marker plus manual)

## 4. Expected outcome

The branch exists with one commit per fixed finding. The audit on it reports
the 2 high findings gone. The suite fails on exactly the 2 pre-existing
tests and nothing new. One change request is open, its body names the
moderate finding as not fixed with the reason "no compatible release". The
baseline block lets a reader verify all of this without re-running anything.

## 5. Failure case

The fix for the second high finding requires a breaking major upgrade. The
agent takes it silently, the suite now fails on 9 tests, and the proof step
still reports success.

- What apb must show: the test result line with the count of new failures
  against the baseline, the run report flagging that the proof did not hold,
  and the change request body carrying both lists.
- What must NOT happen: a run reporting success when the final audit or test
  state is worse than the baseline; a second change request quietly opened
  for the unfixed finding; the baseline rewritten to match the outcome.

## 6. Today

examples/playbooks/dependency-security-fix.yaml expresses the shape: baseline
node, fix node, proof node, one change request. The gaps:

1. The baseline is a text block inside a node output, not a snapshot the
   engine holds; nothing stops a later step from drifting from it, and no
   check compares against it mechanically.
2. "No new failing tests" is the agent grading its own work; no enforced
   check re-runs the suite at run end and diffs against the baseline.
3. The whole findings list goes to one agent call; there is no fix-by-finding
   loop with per-finding proof, so a crash mid-list leaves the work's state
   readable only in the transcript.
4. Exactly one change request is prompt discipline; nothing counts the pull
   requests a run opened.

## 7. Target

The baseline is recorded by the engine as evidence at run start (the audit
output and the failing-test list), read-only checks run at run end against
it, and the run is success only when findings-gone and no-new-failures both
hold. The findings are processed per item with per-item verdicts (fixed,
skipped with reason, failed), and the run report carries the per-item list.
The change request count is engine-counted, not agent-claimed. A feature
closing these gaps carries an engine test that plants the initial state
above, makes the fix worse-than-baseline, and asserts the run fails with the
diff named.
