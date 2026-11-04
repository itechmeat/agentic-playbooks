---
name: apb-diagnosis
description: Find the cause of a bug, a failing check or unexpected behavior before changing code. Reproduce, isolate, name the root cause with evidence, then write a failing regression test before the fix. Use for every bug fix, a red test or build, a flaky check, or a report that something does not work.
metadata:
  version: "1.0.1"
---

# Diagnose before you fix

A fix without a known cause is a guess. This skill turns a symptom into a
cause you can show, and the cause into a test that fails until the fix lands.
It does not commit or publish. When a step may not edit files (a design
or diagnosis stage), run sections 1 to 4 and hand the root cause on: keep
reproductions and scratch input in a temporary directory outside the
working tree, bisect or revert only in a separate worktree or copy, and
leave the tree and HEAD as you found them.

## 1. Pin the symptom

- Write down what was observed, by whom, where, and what was expected
  instead. Quote the error, the output or the screen; do not paraphrase.
- Find out exactly what the reporter sees before you act. When the report is
  unclear, ask the issuer one precise question; never start fixing a guess.
  When nobody can answer (an unattended step), record your reading as an
  assumption and report the diagnosis as not ready if it rests on it.
- Note the version, the commit, the configuration and the environment the
  symptom was seen in.

## 2. Reproduce

- Reproduce through the same entry point the reporter used (the binary, the
  route, the tool, the screen), with the smallest input that still fails.
- Record the exact command or steps and the output. A reproduction someone
  else can rerun is the goal.
- A failure that comes and goes: run it enough times to state a rate, and
  note what changes between runs (timing, order, load, data, environment).
- When you cannot reproduce it, say so. Collect the evidence that exists
  (logs, journals, reports) and state your uncertainty. Do not invent a
  reproduction that fails for another reason.

## 3. Isolate

- Narrow the failure to the smallest unit that owns it: one input, one
  change, one component, one boundary.
- Use bounded experiments that tell causes apart: remove or vary one factor
  at a time, compare a working and a failing case, bisect the history when
  the failure is new.
- Keep symptom, hypothesis and cause apart in your notes. Each hypothesis
  gets the experiment that would refute it, and its result.
- Read the code on the failing path. Do not reason from what the code is
  supposed to do.

## 4. Name the root cause

- The root cause explains every observation, including why it fails where it
  fails and passes where it passes.
- Show it: a log line, a value at a breakpoint, a minimal case, a reverted
  change that makes the failure go away.
- Ask why once more: is this the cause, or the place the cause showed up?
  Fix the boundary that owns the rule, not the first caller that tripped.
- Look for the same cause elsewhere: other callers, other formats, other
  surfaces that share the faulty rule.
- When the cause stays unknown, a change that hides the symptom is a
  mitigation. Label it so and keep the investigation open.

## 5. Regression test first

- Write a test that reproduces the original scenario through the real entry
  point, or as close to it as the project's tests reach. An existing test
  that already fails on the unfixed code for the reported cause, and
  passes after the fix, is the regression test: add no near copy of it.
- Run it on the unfixed code and see it fail for the reason you named. A test
  that fails for another reason, or passes, does not guard this bug.
- Gate the test with the apb-test-audit skill in authoring mode: it must
  protect a distinct behavior, not restate the fix.
- When a test is not practical (a race you cannot force, an external
  outage), record why and what evidence stands in for it.

## 6. Fix and verify

- Make the smallest fix at the owning boundary.
- Rerun the regression test and see it pass. Rerun the original reproduction
  and the project's checks around the change.
- Check that the behavior around the fix still holds: the neighbors of the
  faulty rule, its other callers, the formats it writes.
- Claim "fixed" only with this fresh evidence from the current code. An
  earlier run, a similar case or "it should work now" is not evidence.

## 7. Report

1. symptom as observed, with its source;
2. reproduction: steps, input, rate for an intermittent failure;
3. hypotheses tried, with the experiment and result for each;
4. root cause, with the evidence that shows it;
5. regression test: name, failing output before the fix, passing after;
6. fix and the checks rerun, or the mitigation and what stays unknown;
7. other places the same cause could live, checked or left open.

## Limits

- Two fixes that did not hold mean the cause is not understood. Stop
  patching, go back to isolation, and tell the issuer.
- Never weaken, skip or delete a test to make a failure go away.
- A flaky check is a defect to diagnose, not a reason to rerun until green.
