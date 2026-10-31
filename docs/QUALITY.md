# Run quality: grading a run, promoting by quality

A run that ends `succeeded` can still have gone badly: it went in circles,
changed files outside its task, sent something irreversible nobody agreed
to, or wrote a reply nobody would send. Run quality makes that visible and
comparable:

1. A playbook declares a short `quality:` block: the built-in criteria
   (on by default) and at most three criteria of its own, each with a
   `good`, `partial` and `poor` level.
2. apb grades every run of it by rules as the run ends, for free. A host
   agent grades the playbook's own criteria when a person asks, in a
   bounded reading session, or the project's decision model does, when
   the project turns that on.
3. Every level cites the records it rests on. The grade keeps references,
   counts and one short redacted `why` per criterion, never the source
   text it read.
4. A candidate version (a supervisor's forward patch on trial) can be
   promoted by quality: its graded trial runs are compared with the graded
   runs of `current`, criterion by criterion. "Change nothing" is a valid
   outcome.

## The quality block

```yaml
quality:
  task_type: "{{params.kind}}"   # optional; params only; default "default"
  grade_on_finish: rules         # rules (the default) | off
  builtins:                      # every built-in is on unless switched off
    scope: { off: "the task is to reorganize the whole repository" }
  criteria_grader: host          # host (the default) | model
  criteria:                      # at most 3
    - id: reply-tone
      subject: run               # or { node: draft } (an agent_task)
      applies_when: "the run sent at least one reply"
      evidence: presence         # presence | absence | both
      reads: [outputs, connector_calls]
      levels:
        good: "every reply matches the brand voice and answers the question asked"
        partial: "replies answer the question but at least one breaks the voice"
        poor: "a reply misleads the customer or answers a different question"
```

- `task_type` names the kind of task a run does, rendered from `params.*`
  when the run starts and kept in its manifest. Runs are compared only
  within one task type, so a later edit cannot move a past run. A secret
  param is refused: the task type is stored with every grade.
- `grade_on_finish: rules` grades the built-ins as every run ends (a
  `run_graded` event right before `run_finished`). `off` leaves grading to
  `apb grade` and the dashboard.
- `builtins.<name>.off` switches a built-in off and says why.
- `criteria_grader` names whose grades of the playbook's own criteria a
  candidate comparison expects: a host's (`host/1`, the default) or the
  model grader's (`model/1`). Either may grade a run at any time; the
  comparison counts only the named one. A forward patch that changes it
  is refused, like any change to the block.
- `criteria` are the playbook's own, graded by a host or by the model
  grader. Each one:
  - `id`: lowercase letters, digits, `_` and `-`, starting with a letter or
    a digit, at most 40 characters, unique, not a built-in's name;
  - `subject`: the whole run, or one `agent_task` node;
  - `applies_when` (optional): when the criterion is judged at all; a run
    it does not apply to is graded `not_applicable`;
  - `evidence`: what a level may rest on. `presence` needs cited records,
    `absence` a claim that something did not happen, `both` either;
  - `reads`: the views a level may rest on: `journal`, `outputs`,
    `connector_calls`, `transcripts`, `findings`;
  - `levels`: one plain sentence each for `good`, `partial` and `poor`.

Write a level as something a reader can check against a record: "every
reply names the order number" grades; "replies are good" does not. Keep
the criteria few. Three sharp ones beat ten vague ones, which is why the
block takes at most three.

Every criterion has a digest over its definition. Editing a criterion's
text starts a new history: grades under the old digest are never compared
with grades under the new one.

### Validator codes

| Code | Severity | What |
|---|---|---|
| V102 | error | a malformed criterion: a bad, duplicate or built-in id, a missing level, empty or too long text, hidden characters |
| V103 | error | more than three criteria, or a `subject.node` that is unknown or not an `agent_task` |
| V104 | error | `task_type` reads anything but `params.*`, or a secret param |
| V105 | error | an unknown built-in, or a built-in switched off without a reason |
| V106 | warning | `promote_supervisor_patches` or `trial_candidates` does not read (an unknown word or key, a value out of range, two forms mixed) and falls back to its default |
| V106 | error | a `when: quality` gate that does not read: it falls back to `manual`, so a person decides every candidate until it is fixed |
| V107 | error | `promote_supervisor_patches: { when: quality }` without a `quality:` block, with no criterion in force, or with the built-in `outcome` switched off |
| V107 | warning | `when: quality` with `grade_on_finish: off`: no trial is graded unless someone grades it |
| V110 | error | an unknown `criteria_grader` |
| V111 | warning | `criteria_grader: model` with no criteria of the playbook's own: the model grader has nothing to grade |

## Levels

A criterion is graded at one of five levels:

| Level | Meaning |
|---|---|
| `good` | the `good` description holds |
| `partial` | the `partial` description holds |
| `poor` | the `poor` description holds |
| `not_applicable` | the criterion does not apply to this run |
| `not_enough_data` | the records cannot tell: a view is incomplete, the agent kept no readable transcript, the evidence did not hold |

Only `good`, `partial` and `poor` are determinate and count in a comparison.
Each level carries a `reason_code` (a short slug such as `repeated_calls`:
`[a-z0-9_]`, at most 40 characters; anything else is stored as the level's
name), one sentence `why` for a person (redacted, at most 300 characters),
and its citations.

## Built-in criteria

The rules grader (`rules/1`) grades the built-ins from the journal and the
attempt transcripts, deterministically and without a model. Each one reads
both the transcripts and the connector facts in the journal, so it grades a
code run and a mail or CRM run alike.

| Criterion | Graded from |
|---|---|
| `outcome` | the run's end: succeeded without a failed node is `good`, succeeded after a failed node it recovered from is `partial`, failed is `poor`; a run a person stopped is `not_enough_data` (it says nothing about the playbook) |
| `loops` | the same tool called with the same input three times in one session; a retry that failed the same way as the attempt before it; a bounded edge that spent its `max_traversals`; the same connector function failing three times for one node. None is `good`, one sign `partial`, two or more `poor` |
| `scope` | a protected path changed (`poor`); a file written outside the attempt's working directory, the run's working tree, the run directory and the temp directories (`/tmp`, `/var/tmp`, `/private/tmp`, `/private/var/folders`) (`partial`). A path is resolved first: `..` is folded and a relative path is joined to the working directory. A sub-agent's writes count |
| `consent` | a connector call to a function the connector declares `irreversible`, or a destructive shell command (a recursive forced delete in any spelling of its flags, a forced push including a `+` refspec, a hard reset, a forced clean, a forced branch delete, `find -delete`, dropping or truncating a table, formatting a disk, a raw disk write), a sub-agent's included. A connector action is consented by the run's irreversible consent or by an approving review gate right before its node; a shell command only by such a review. Without any consent the action is `poor` (`irreversible_without_consent`); a shell command under the run's consent alone is `partial` (`destructive_without_review`). A review decision covers only the node the run went to from that gate next, and the latest such decision before the action counts: an approval at an unrelated gate consents to nothing. No such action is `not_applicable`. When something cannot be seen (a connector that changed since the run, `connector_changed`; one the run's manifest does not record, `connector_unrecorded`; an attempt without a readable transcript), anything short of `poor` is `not_enough_data` |
| `skill_use` | whether each skill a node's profile gives it was read (a `Skill` tool call or a `Read` of its `SKILL.md`; writing one is not a read), and for a read skill whether its node succeeded at once (`good`), after a failed attempt (`partial`) or not at all (`poor`). A skill nobody read is `not_applicable`; when no read is seen but an attempt's reads cannot be told (`unknown`), it is `not_enough_data` |

`rules/1` reads tool calls from claude session transcripts only. For an
attempt of any other agent the built-ins that rest on something not
happening (`loops`, `scope`, `consent` when the journal shows no action)
are `not_enough_data`, and its skill reads are `unknown`.

A codex attempt's `exec --json` stdout is now read as a transcript with
tool facts too (commands, file changes, MCP calls), so a host grader can
read and cite it. `rules/1` still does not grade from it, for
three reasons:

1. The grader id is part of the comparable key a promotion compares by.
   If `rules/1` began to grade codex runs, one key would hold two
   behaviours: a candidate tried after the change would be compared with
   runs of `current` graded before it, on other evidence.
2. A new grader version (`rules/2`) leaves every earlier `rules/1` grade
   outside its key, so every candidate would fall back to `insufficient`
   until enough runs are graded again. That needs grading under both
   versions for a while, and the comparison picking the newest rules
   version.
3. A claim that something did not happen needs a complete view. The codex
   stream reports completed items only, and work a sub-agent or a tool does
   in its own thread may not show in it. A codex view can count as
   complete only when every started item completed, no item type is
   unknown and no sub-agent was spawned, which needs its own rule and
   tests.

Until then a codex run graded by rules gets the same levels as before:
`outcome` from the journal, and `not_enough_data` where an absence would
have to be claimed. An agent attempt apb keeps no transcript directory
for (a finish answer, a judge) gets the same levels: it is an empty,
incomplete view. A transcript that was cut (the transcript
cap, a lost stream) makes a claim of absence `not_enough_data` too. When a
re-run restarts the attempt counter and reuses an attempt's directory,
only the latest attempt of it is read.

## Evidence

Every grader's output goes through one validator, so a level always rests
on what the grader actually read:

- A citation names a record the grader read in full: `j:<seq>` a journal
  event, `o:<node>` a node's output, `s:<node>-<attempt>` a status file,
  `t:<stem>/<session|stdout|stderr>#<record>` a transcript record. A span
  `[from, to]` narrows it to characters of the record as the reader showed
  it (redacted). The record must be in a view of a kind the criterion
  reads.
- Outline entries (`n:<view>`), child listings and search hits route a
  reader and never prove anything; citing one is refused.
- A determinate level cites what it rests on, or claims an absence. A claim
  that something did not happen needs at least one view of the kinds the
  criterion reads, each complete and read to its end.
- One level per criterion: two for the same criterion are refused.
- One correction round: a refused submission may be corrected once. What
  is still wrong after it becomes `not_enough_data`, and so does a level
  that changed without a record newly read in full (an outline, a listing
  or a search does not count as reading).

The grade stores the citations and a reading trace: the budget, the
characters read, each reading step (operation, reference, characters) and
per view whether it was complete and read to its end. No source text is
stored, in the grade or in the session file; the only free text is the
short redacted `why`. Every text the reader returns goes through the
workspace's redactor first, and every character it returns counts against
the budget: a listing or a search stops when the budget is spent, as a
read does.

## Grading by a host

A host agent grades the playbook's own criteria only when a person asks
for it. Over MCP:

1. `run_grade_start { run_id, budget_chars? }` opens a session (one per run
   at a time, open for an hour, 200000 characters by default) and returns
   the criteria with their levels and the outline of the run's views. A
   run takes one host grade: once it has one, a new session is refused
   until a person reopens it (`apb grade <run> --reopen`, or Grade again on
   the run page), so no one grades a run again until the level suits them.
2. `run_grade_read { run_id, session, op, ... }` reads: `outline`,
   `children` (a view's references with short snippets), `read` (one
   record whole, or a span), `page` (consecutive records, each whole) and
   `search` (references whose text holds a phrase). Only items marked
   `citable: true` may be cited.
3. `run_grade_submit { run_id, session, grades: [...] }` submits one level
   per criterion with its citations. The answer is `status: graded` with
   the stored grade, or `status: rejected` with the problems to correct.

A host's `not_applicable` is stored but does not count toward promoting a
candidate (`counts_for_promotion: false`): a criterion a host left out can
hold a candidate back, never let one through.

Grading, a grading session and a decision on a candidate are refused from
a run step (a process with `APB_RUN_ID` or `APB_HOST_TASK` set, which is
also how an MCP server an agent CLI started inside a run is told apart):
a run never grades its own playbook's runs.

`run_grades { run_id }` lists a run's grades, and `run_status` carries a
short `grades` block: the latest grade of each grader (the rules grader
first), criterion by level.

## The model grader

The decision-model grader (`model/1`) grades the playbook's own criteria
of an ended run, only when someone asks: `apb grade <run> --model`, `apb
grade --backfill <playbook> --model`, MCP `run_grade { run_id, grader:
model }`, `POST /api/runs/{id}/grades` with `{ "grader": "model" }`, or
Grade by model on the run page. It asks the project's decision model
(docs/DECISIONS.md, "Model grader"), so it is off until `decisions.yaml`
turns `uses.grade` on, lets run outputs be sent (`privacy.send` includes
`outputs`) and a provider resolves; it is capped per project and UTC day
(50 by default). Per criterion:

- only the views the criterion reads that fit whole within
  `privacy.max_state_bytes` are sent, scrubbed; a view is never clipped;
- it asks the level, and for each determinate level the record that
  supports it, offering only the records of the views sent; the cite is
  the support of the chosen level, and every view sent counts as read to
  its end in the trace;
- `none` on a criterion resting on what happened gives `not_enough_data`;
  an absence is claimed only when every view the criterion reads was sent
  whole and is complete, and every record was offered as a cite (at most
  254);
- an answer below `uses.grade.thresholds.min_confidence` (0.7 by default)
  gives `not_enough_data`;
- everything goes through the one evidence validator, as a host's levels
  do.

A model grade counts for promoting a candidate only when the use is in
`enforce` and the provider is calibrated. When nothing is stored the
answer says why: `skipped` is `off` (the use is off, outputs may not be
sent, or no provider resolves), `no_criteria`, `budget` (the day's cap is
spent), `send`, `busy` or `state_too_large`, when the model answered no
criterion. A stop after the first answer stores the grade: the criteria
it stopped before are `not_enough_data` under that reason, none of the
grade counts for a promotion, and a later request grades the run again.
A reader budget spent part way (`read_budget`) stops it the same way. A model backfill stops at the first run the
model skips. It suits criteria over outputs and connector
calls, which fit whole; a criterion over long transcripts is better left
to a host.

## Storage

- `runs/<id>/grades/<grade_id>.json`: one file per grade, written once,
  atomically, owner-only, signed with this installation's key (`mac`). A
  run may have several grades; a comparison reads the latest of each
  grader (on a tie in time, the one listed later), and only grades this
  installation signed for that run.
- `runs/<id>/grading/<session>.json`: a host session's reading state
  (references and counts only); `reopened.json`: a person's leave for one
  more host grade.

The inline grade is taken once: an end replayed after a crash between
`run_graded` and `run_finished` grades nothing again.

A grade stored after a run ended writes no journal event: a finished
journal stays sealed and an older apb keeps reading it.

## Promotion by quality

```yaml
supervisor:
  policy:
    promote_supervisor_patches: { when: quality, compare: not_worse, min_runs: 5, max_trials: 10, window: 20, margin: 0.35 }
```

The quality gate applies only when a playbook asks for it: the default
policy (`on_success`) promotes a candidate by successful trials, with no
comparison. Under the gate apb promotes a candidate on its own only when
it is shown not worse; a person may still promote it or keep `current` at
any time.

- `compare`: `not_worse` (the default) or `better`.
- `min_runs`: determinate grades needed per side and per criterion, at
  least 3 (default 5).
- `max_trials`: trials after which an undecided comparison goes to a
  person (default 10), at least `min_runs`.
- `window`: how many of `current`'s latest graded runs are compared with
  (default 20), at least `min_runs`.
- `margin`: the non-inferiority margin, 0 to 1 (default 0.35): how far the
  candidate's `poor` rate may plausibly be above `current`'s. With the
  defaults and a `current` with no poor run, a candidate with no poor trial
  is shown not worse after 8 trials; a smaller margin asks for more.

An unknown key, two forms mixed, or a value out of range does not read: a
quality gate that does not read falls back to `manual` (V106, an error).

The criteria and the gate come from `current`. A forward patch that
changes the `quality:` block is refused, and an in-run patch under a
quality policy is never promoted on its own.

### What is compared

Grades are comparable only under one key: the same criterion and criterion
digest, the same grader and grader version, the same task type. On top of
that:

- only runs apb created on this machine count (each run directory's
  origin stamp is checked: one that came with a repository or was copied
  is left out), and only grades this installation signed for that run;
- on both sides only runs that ended without migrating onto an in-run
  patch count: the candidate's trial runs, and `current`'s latest graded
  runs;
- every run of the playbook is looked at, however many other runs the
  project has;
- a criterion that applied to no run on either side (every grade
  `not_applicable`) is left out of the comparison;
- a criterion no trial has a grade of yet shows `current`'s grades and "no
  graded trials yet".

Per criterion, only determinate levels count:

1. Fewer than `min_runs` on either side, or more than half
   `not_enough_data` on the candidate side, is insufficient.
2. "Worse": the Wilson 95 % lower bound of the candidate's `poor` rate is
   above the upper bound of `current`'s (shown worse).
3. "Inconclusive": not shown worse, but the candidate's Wilson 95 % upper
   bound for `poor` is above `current`'s `poor` rate plus `margin`: the
   candidate is not shown to be not worse yet. A small sample never passes
   for "no harm seen": four poor trials of five against none of five is
   inconclusive, never a promotion.
4. "Better": the Wilson 95 % lower bound of the candidate's `good` rate is
   above the upper bound of `current`'s.

### The verdict

| Verdict | When | What happens |
|---|---|---|
| `keep_current` | any criterion is worse; or `compare: better` and none is better ("no measurable improvement") | the candidate is dropped, `current` stays |
| `wait` | a criterion is insufficient or inconclusive, or no criterion compared at all, and trials are under `max_trials` | the candidate stays on trial |
| `needs_person` | the same at `max_trials` | the candidate stays on trial until a person decides |
| `promote` | no criterion worse, insufficient or inconclusive, at least one compared (and one better under `better`) | the candidate becomes `current` |

No evidence never promotes: a comparison with no criterion, or with none
that applied to a run on both sides, waits and then goes to a person.
When a criterion is the playbook's own and no trial has a host grade of it,
the reason says a host grades the trials (`run_grade_start`).

Under a quality policy one failed trial decides nothing on its own: the
failure is in its grade (`outcome: poor`). The comparison is taken:

- at the end of every trial run (journaled as `candidate_promoted` with
  `basis: quality`, `candidate_rejected` or `candidate_held`);
- again whenever a grade of the playbook's runs is stored later, on any
  surface (`apb grade`, `apb grade --backfill`, MCP `run_grade_submit`,
  the dashboard's "Grade by rules"); this writes no journal event, the
  decision goes to the candidate's trial record, and a held trial run's
  `candidate_trial` then reports what the record says now.

### A person decides

A person may decide at any time, whatever the verdict:

- `apb candidate <playbook>` shows the candidate, the gate, the comparison
  per criterion and the decision recorded last;
- `apb candidate <playbook> --promote` makes it `current`;
- `apb candidate <playbook> --keep-current [--reason <why>]` drops it and
  changes nothing;
- the dashboard's playbook page shows the same card in the version history
  (a `candidate` chip in the title opens it), with Promote and Keep current.

MCP `playbook_candidate` shows the comparison read-only: an agent reports
it, a person decides. A decision from a run step is refused.

## Surfaces

| Surface | What |
|---|---|
| CLI | `apb grade <run>` (rules) or `--model` (the model grader), `--show`, `--json`, `--reopen` (a host may grade the run once more); `apb grade --backfill <playbook> [--version V] [--last N] [--model]` (grades for up to N earlier ended runs without one of that grader, so a comparison has a history; runs already graded or live are skipped and do not count); `apb candidate <playbook>` with `--promote`, `--keep-current [--reason]`; the `grade` lines of `apb runs <id>` |
| MCP | `run_grade` (`grader` `rules` or `model`), `run_grade_start`, `run_grade_read`, `run_grade_submit`, `run_grades`, `playbook_candidate`, the `grades` block of `run_status` |
| HTTP | `GET` and `POST /api/runs/{id}/grades` (POST grades by rules, or by the model grader with `{ "grader": "model" }`, answering `skipped` when it stored nothing; 409 while the run is live); `POST /api/runs/{id}/grades/reopen` (409 without a host grade); `GET /api/playbooks/{id}/candidate`; `POST /api/playbooks/{id}/candidate/promote`; `POST /api/playbooks/{id}/candidate/keep-current` with `{ reason? }` |
| Run report | the Quality section of the HTML run report (`apb runs <run> --html <path>`, the run page's Save report): the latest trusted grade of each grader, one row per criterion with its level, reason, why and first citations, and how many unsigned grade files were left out |
| Stats | `apb stats` and the dashboard's stats card: per version, each criterion's good and poor shares over the graded runs, and with `--compare` how they moved (docs/STATS.md) |
| Dashboard | the run page's Quality panel (levels, reasons, citations that open the journal event or name the transcript file, the reading line, "Grade by rules", "Grade by model", and "Grade again" once a host graded the run); the playbook page's candidate card |

## Example: a code task

`examples/playbooks/ci-failure-triage.yaml` reads a failed build's log and
writes a three-field summary. The built-ins catch an agent that loops on
the same command, writes outside its working directory or runs a
destructive command; the one criterion of its own grades the summary
itself:

```yaml
quality:
  criteria:
    - id: first-error
      subject: { node: triage }
      evidence: presence
      reads: [outputs, transcripts]
      levels:
        good: "likely_cause cites the first error in the log, before any follow-up errors"
        partial: "likely_cause names the failing step but cites a later follow-up error"
        poor: "likely_cause names an error or a step the log does not show"
```

A host grading it reads the node's output (`o:triage`) and the log reads
in the transcript, and cites both.

## Example: a routine without code

`examples/playbooks/inbox-triage.yaml` triages unread mail into Asana
tasks and marks the filed messages read. Nothing in it is code, and the
built-ins still apply: `consent` reads the `create_task` and `mark_read`
calls in the journal against the run's consent and the review gates before
them, `loops` reads repeated failing calls. Its own criterion is plain
text over the outputs and the connector calls, compared per mail folder:

```yaml
quality:
  task_type: "{{params.mail_folder}}"
  criteria:
    - id: task-names
      subject: { node: file_tasks }
      applies_when: "the run filed at least one task"
      reads: [outputs, connector_calls]
      levels:
        good: "every task name says what the sender asks for, in a few words"
        partial: "every task exists but at least one name is vague or generic"
        poor: "a task name misstates the request, or a newsletter was filed as work"
```
