# CI failure triage

Kind: code work. Runnable example: examples/playbooks/ci-failure-triage.yaml.

## 1. Purpose

A CI job failed and its log is long. The scenario stands for the read-only
code-work playbook: an agent looks at evidence, produces a small structured
result, and changes nothing. It is the smallest scenario in the set, and the
one that pins what "the run reports honestly" means when the only output is a
diagnosis that the engine itself cannot verify.

## 2. Initial state of the world

- A repository with a build log saved at build.log, from one failed job.
- The log holds one root-cause error near its middle (a missing feature flag
  in a config table) and eleven follow-up errors downstream of it.
- Two warnings in the log predate the run and are unrelated.
- The working tree is clean before the run.

## 3. Steps

1. A run starts with the log path as a param. (params)
2. One agent node reads the log and the source it points to, finds the first
   error rather than the last, and answers with exactly three fields:
   failing_step, likely_cause, next_action. (agent node, declared output
   fields, status file)
3. The node's success report is rejected unless the `likely_cause` field is
   present (success_check, marker form); a missing `failing_step` or
   `next_action` only journals an `output_fields_missing` warning.
4. The goal criteria check that the summary names the failing step and
   proposes a next action. (goal criteria, marker checks; the root-cause
   criterion is manual)

## 4. Expected outcome

The run ends success. A person reading only the summary knows which step
failed, the first real error and its log line, and the one next action to
take. No file in the working tree changed afterwards.

## 5. Failure case

The agent summarizes the last error of the cascade instead of the first. The
field markers all pass, the run reports success, and only a reader comparing
the summary with the log catches the mistake.

- What apb must show: the manual criterion ("the cause is the first real
  error, not a later symptom") listed as unconfirmed, next to the summary, so
  a reader knows which part is machine-checked and which is not.
- What must NOT happen: an enforced criterion passing on a wrong diagnosis,
  or the agent editing files to make the failure go away (the playbook
  declares fs_read only; today that is prompt discipline, see gap 4).

## 6. Today

examples/playbooks/ci-failure-triage.yaml covers the scenario as written. The
gaps:

1. The root-cause criterion is manual: no read-only check today can verify a
   summary against the log it came from, so a wrong diagnosis still shows a
   green run.
2. The run report cannot distinguish "the agent answered" from "the answer
   was checked correct"; honest run results have nothing to attach a
   verification to.
3. The log lives in the tree, the summary in the run output; no check runs at
   run end that can read both and compare.
4. Read-only is prompt discipline: declared effects only ever widen what a
   node may do, an agent node keeps its write permissions, and a write does
   not fail the attempt. Only the prompt line keeps the tree unchanged.
5. Only `likely_cause` is enforced; the other two declared fields are
   warned about when missing, never required.

## 7. Target

The root-cause criterion becomes a check the engine runs at run end: a script
or a decision-model question that reads the log and the summary and answers
whether the quoted line is the first error. The run stays success only when
that check holds, and the run report shows the check's verdict beside the
summary. A feature closing this gap carries an engine test that builds the
initial state above, plants the wrong diagnosis, and asserts the run does not
report an unqualified success.
