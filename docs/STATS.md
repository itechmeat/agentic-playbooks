# Run stats: `apb stats`

`apb stats` answers "is this playbook getting better or worse" from the run
journals alone. It asks no model, sends nothing anywhere and writes nothing:
every figure comes from the `.apb/runs/<id>/events.jsonl` journals (and each
run's playbook snapshot, for `expected_duration`). Like
`apb decisions report`, it reads only runs apb created on this machine; a
run directory that came with a repository is skipped and counted on stderr.
Only production runs count: a run whose manifest names another `origin`
(a scenario or eval run, or an origin this apb does not know) is left out,
and so is a run whose manifest cannot be read; a run with no manifest or
no `origin` is a production run (see "Scenario runs are not history" in
`docs/EVALS.md`). The run estimate below reads the same runs.

```
apb stats [--playbook ID] [--since 30d|2026-09-20] [--compare VERSION] [--all-projects] [--json]
```

- `--playbook ID`: only runs of this playbook. Without it every playbook of
  the project is reported.
- `--since`: only runs started since a UTC date or a duration back from now
  (`30d`, `24h`, `90m`).
- `--compare VERSION` (needs `--playbook`, exit 2 without it): puts VERSION
  next to the latest other version that has runs and prints the difference.
  When VERSION itself has no runs (in the `--since` window) the text says
  `no runs of VERSION` and the JSON carries no deltas.
- `--all-projects`: every registered project, not only the current one.
- `--json`: the report as JSON (shape below).

The dashboard shows the same report on a playbook's page (the Stats button),
per version, newest first, from `GET /api/stats?workspace=<id>&playbook=<id>`
(`since` and `compare` are accepted too; `compare` without `playbook` is a
400, like the CLI's exit 2).

## What is measured

Per playbook version:

- **outcome**: succeeded runs over finished runs (succeeded, failed,
  aborted); runs still running or paused count as `other`.
- **first pass**: runs that succeeded with no retry, no fallback and no loop
  traversal, and whose every node finished on attempt 1, over finished runs.
- **retries, fallbacks, loop traversals** per run: `retry_started`,
  `fallback_triggered` (and `execution_fallback`, a step handed to the host
  because none of its CLIs could start), and `edge_traversed` over a bounded edge (one with
  `max_traversals`) back into a node that already ran in that run. Only
  bounded edges count: a hop over an edge without `max_traversals` (journaled
  with `uncounted: true`) and a `defaults.on_failure` policy hop are left out.
  A loop of two nodes counts both hops only when both edges carry
  `max_traversals`; a loop closed by one bounded edge counts one per round.
  An infrastructure retry raises the attempt number without a
  `retry_started`: it is not in the retries figure, but a run whose node
  finished on an attempt above 1 is no first-pass run.
- **gate wait**: each `review_requested` to the `review_decided` of the same
  gate; **question wait**: each `question_asked` to its
  `question_answered`. Median and count. A request closed by
  `review_withdrawn` (a supervisor moved the run off the gate) records no
  wait, and a gate or question still open when the report is made is not in
  the figures, so a long wait only shows once it was answered.
- **run duration**: `run_started` to the terminal event, leaving out the time the
  run spent paused (`run_paused` to the next `run_resumed`, issue #200). Median and count.
- **tokens and cost** per run, over the runs whose attempts reported usage
  (`attempt_finished.usage`); the cost is what the agent CLIs reported, over
  the runs that reported one, whatever its basis, and `spend.cost_by_basis`
  splits it (`billed_usd`, `subscription_equivalent_usd`,
  `unknown_basis_usd`; the text line `cost by basis: $0.0200 billed; $0.0300
  subscription equivalent, not billed`). See "What counts as money" in
  `docs/HOWTO-authoring.md`. Usage marked `estimated` (a source reserved for
  counts apb makes itself; nothing records it yet) is summed together with
  reported usage and is not marked apart in these figures. **cost known**:
  the finished attempts that reported a cost over every finished attempt,
  so a cost figure says how much of the work it covers (`spend.cost_known`;
  the text line ends with `attempts with a known cost 3/4 (75%)`).
- **missing deliverables and output fields**: `deliverable_missing` and
  `output_fields_missing` counts.
- **goal**: per criterion, how often it passed over the runs that checked it
  automatically, failed, could not run (`error`), or was left to a person
  (`manual`), from the `goal_checked` events.
- **findings**: the findings the attempts reported (`attempt_notes`, see
  "Findings and warnings" in `docs/HOWTO-authoring.md`), per kind (how many
  and in how many runs), and the top 20 groups of one kind at one location,
  by the runs they appeared in. A location is grouped without its trailing
  `:line` or `:line:col`, so `src/auth.rs:12` and `src/auth.rs:40` are one
  group. `runs_with_warnings`: the runs whose attempts reported a warning,
  over the version's runs. Text: `findings: security: 3 findings in 2 runs`, then one
  indented `security at src/auth.rs: 2 runs, last <run id>` line per group.
- **baseline**: per baseline check observation, how many runs compared it,
  how often it was met (over those runs), and in how many runs it regressed
  or was unknown, from the latest `baseline_compared` of each run. Text:
  `baseline unowned.count: met 3/4 (75%), regressed in 1`.
- **follow_up**: per playbook with a `follow_up:` block, how many runs of
  the version were observed (`met`, `not_met`, `unknown`), how many are
  `missed` and how many still `pending`, from the latest
  `follow_up_observed` of each run. Unknown and missed are counted apart,
  never as met; the met rate is over observed runs only. Text:
  `follow-up deal_won: met 3/4 (75%), unknown 1, missed 0, pending 2`.
  `--compare VERSION` puts `follow_up_met_rate` (base and against, absent
  when either side has no observed run) next to the baseline deltas.
- **models**: how many attempts ran on each model, and `model_mismatch`, how
  many of them ran on another model than the node profile's primary model
  (from the run manifest). A CLI attempt counts under the model apb
  requested of the CLI (its `attempt_started`); a host attempt (host
  execution mode, or the host fallback) under a model only when every one
  of its host tasks reported the same one in `host_task_submitted.model`,
  and under `unreported` otherwise: a task that named none, or tasks that
  named different models, are never credited to a model, and a hint never
  counts as the model that ran. In host mode the profile's model is only a
  hint, so this is where a run that went entirely to another model shows. Two names count as the same model when their lowercase tokens
  (split on every non-alphanumeric character) are equal, or the shorter
  name's tokens appear in order in the longer one and every extra token is
  a date (six or more digits), a vendor or channel word (`claude`,
  `anthropic`, `openai`, `google`, `gemini`, `latest`, `preview`, `exp`,
  `experimental`, `stable`), or a version number while the shorter name
  carries no number at all. So `opus` matches `claude-opus-5-5` and
  `sonnet` matches `claude-sonnet-5-5`, while `glm-5.3` and
  `glm-5.3-flash`, `gpt-5` and `gpt-5-mini`, or `claude-sonnet-4` and
  `claude-sonnet-4-5`, are different models. Tasks are credited to one
  model only when every two of their names match, whatever their order.
- **quality**: the levels the runs' grades gave each criterion
  (docs/QUALITY.md), one row per comparable key: the criterion, the digest
  of its definition, the grader (`rules/1`, `model/1`, `host/1`) and the
  task type. Only grades this installation signed for that very run count,
  and of each grader only the latest grade of a run: the grades a
  promotion reads. Unlike a promotion, which weighs only the criteria that
  count for it, every criterion is listed, informational ones too. `good` and `poor` are shares of the determinate levels
  (good, partial, poor); `not_applicable` and `not_enough_data` stay in the
  `tally` and out of the shares. Text: `quality outcome (rules/1, default):
  good 2/3 (67%), poor 1/3 (33%) over 3 graded runs`. A version with no
  graded run has no quality rows, and `apb grade` (or the dashboard's run
  page) is how a run gets graded.

Per node: its executions, counted from the journal's order rather than from
`node_started` alone. Execution `n` of a node is the work its `n`-th
`node_finished` ended; it begins at the first `node_started` after the
previous finish, or at the finish itself when the node wrote none (a finish
node without a prompt). The figures:

- `executions`: executions begun, a `join: any` race loser left out;
  `completed` the ones that ended, `open` the ones still running, paused
  (a quota pause included) or cut short, and `failed` the completed ones
  that ended `failed` or `timed_out`;
- `cancelled`: race losers, counted apart;
- `runs`: the runs with at least one execution counted in `executions`;
- `re-entries`: `executions - runs`, the executions after the first in the
  same run, never negative;
- `in_runs`: the runs the node is established to belong to: a definition
  the run executed is known to declare it (the definitions the run's
  journal names by digest, every segment counted once per node and run), or
  the run executed it. So `runs` never exceeds `in_runs`, also for a run
  patched to a definition without the node. Every such node has a row, so a
  node no run executed shows as `not executed in N of M runs` with zero
  executions;
- `in_runs_unknown`: runs whose only mention of the node is the final
  snapshot while a definition the run executed is not established (a run an
  older apb started): counted apart, never guessed into `in_runs`;
- `not_run`: why the node did not run in the runs it belongs to and did
  not execute in, one explanation per run, counted by class (`observed`,
  `inferred`, `unknown`) and code (`no_failure_to_route`,
  `condition_not_matched`, `source_not_executed`, `cannot_tell` and the
  others the run graph uses). `unknown` is shown, never left out, and no
  count is read as "dead branch". Text: `node report: not executed in 3 of
  3 runs (why: 3 observed no_failure_to_route)`.

Then first pass (its first result was a success on attempt 1, with no retry,
fallback or later re-entry); retries and fallbacks; the median duration
(`node_started` to `node_finished`) and, when the node declares
`expected_duration`, how many executions took longer; the models its
attempts ran on and `model_mismatch`.

Changed meaning since 1.0.0-beta.2: a race loser no longer counts in `runs`
(it was a run the node "started in"), and a finish without a prompt now
counts as executed.

Every rate is printed with its count (`7/9 (78%)`), and a version with fewer
than 10 runs carries a note: small samples mislead.

**Per executor digest.** When the runs of one version bound a node to more
than one executor (its profile, model, SOUL, skills or chain changed
between runs, or a run overrode it), the node lists its figures per digest
group: `profile` (the profile digest: `profile.yaml` plus SOUL), `bundle`
(the profile plus its skills) and `executor` (the executor digest: the
chain's agents, models, invocations and SOUL delivery, the routed tiers,
and whether the binding is a run-local `ephemeral_executor` override; the
paths of the machine are left out, so a reinstall elsewhere keeps the
group), each shortened to 12 characters, `ephemeral` for an override, the
runs, first pass, median duration and the median tokens of a visit that
reported usage on every attempt (`digests` in the JSON; text: `    profile
1a2b3c4d5e6f (bundle 9f8e7d6c5b4a, executor 0c1d2e3f4a5b, override): 3
runs, first pass 2/3 (67%), median 1.0 min, 200 tokens per visit`). An
override equal to the base profile's executor is its own group: what ran
was chosen for this run. The executor digest is recomputed from the run
manifest, so runs an older apb made are grouped the same way. With
`--compare`, `executor_changes` names each node whose latest executor
differs between the two versions (`node`, `base_profile`,
`against_profile`, `base_bundle`, `against_bundle`, `base_executor`,
`against_executor`; text: `executor of a: profile X -> Y, bundle X -> Y,
executor X -> Y`), so a change in the figures can be told from a change of
model or prompt. The dashboard's stats card shows the same groups.

**Executor report.** Per node, `executors` lists each executor group (the
`profile`, `bundle` and `executor` digests, `ephemeral`, `runs`,
`attempts`) with its chain steps and the transitions between them:

- `steps`: one row per chain position an attempt ran on (`binding`, the
  binding snapshot's shortened digest; `step`, the position; `source`:
  `primary`, `fallback`, `tier`, `host_fallback` or `ephemeral`;
  `executed_by`: `cli`, `host` or `scenario`), with `attempts`,
  `successes`, `median_attempt_ms`, `cost_usd` over `cost_known` and
  `cost_by_basis` (as the version's spend), `reported_models` (attempts by
  `one:<model>`, `mixed`, `partial` or `none`) and `closes_by_reason` (how
  attempts on the step ended when the chain moved on). Attempts an older
  apb journaled without a position form one row with no `step` and
  `source: not_recorded` ("position not recorded"). A declared step no
  attempt reached is listed with `attempts: 0` ("never ran"), but only
  when every attempt's position is recorded: an attempt of unknown
  position may have run on any declared step.
- `transitions`: moves from one step to the next by reason (`from_step`,
  `to_step`, `reason`, `count`). The reason is how the step before closed,
  as the drive established it at the attempt's final verdict:
  `check_rejected` (a success check, completion check or contract
  rejected the result), `protected_paths`, `quota`, `transient`
  (infrastructure retries ran out), `timed_out`, `unstartable` (the CLI
  could not start, or the host step of a `cli` run), `agent_failed` (the
  agent reported failure), `supervisor_interrupted`, `routing` (a tier
  cascade), `failed` when nothing more is known, and others the journal
  names. A transition an older apb journaled without a reason reads
  `not_recorded` and is never reclassified. A pause or an abort ends the
  chain: no transition follows it.
- `hint_follow` (host groups only): of the host attempts apb gave a model
  hint, how many reported that model (`followed`), another one
  (`not_followed`) or none (`not_reported`); an unreported model is never
  taken for the hint.

A node is listed in the text when one of its groups has more than one step,
a transition or a hint line:

```
  executor 0c1d2e3f4a5b (profile 1a2b3c4d5e6f, bundle 9f8e7d6c5b4a): 3 runs, 5 attempts
    step 0 primary (cli): 3 attempts, succeeded 1/3 (33%), cost 0.0300 USD over 3/3 (100%)
    step 1 fallback (cli): 2 attempts, succeeded 2/2 (100%)
    step 2 fallback: never ran
    0 -> 1 check_rejected x2
```

`apb runs <id>` names each attempt's step and why it was entered
(`model w attempt 2 (cli): haiku requested by apb, step 1 fallback 1 of 1,
entered from step 0: check_rejected`), and `apb doctor --run <id>` has one
`step` row per attempt that entered a later step. The retrospective
(`run_retro_context`, `{{run.retro}}`) counts each node's
`fallback_reasons` by the same reasons.

**Quality deltas.** With `--compare`, `compare.quality` lists, for every
comparable key both versions graded under the same definition digest (the
delta carries that `digest`), how
the good and poor shares moved (`against` minus `base`; text: `quality
outcome (rules/1, default): good -50 points, poor +50 points`). A criterion whose
definition changed between the versions has no delta: its levels were
given against different text. The dashboard's stats card shows each
version's quality rows and, with two versions or more, the deltas of the
newest against the one before it.

## Interventions

Each version carries `interventions` over its finished runs (absent when it
has none): what came into the runs from outside the playbook's own flow.
The text block of `apb stats`, the `interventions` of `--json`, `GET
/api/stats`, MCP `playbook_stats`, the dashboard's stats card and the run
retro (`run_retro_context`, `{{run.retro}}`) all read the same fold
(`apb_engine::run_stats::interventions`).

| Category | Counted from | Over |
|---|---|---|
| `review` | every `review_requested` is a gate visit; its decision is `automatic` (an applied `review_triage` decision of the visit, which the engine journals just before the visit's `review_requested`, `decided_by: model` or `unattended`, or `via` the engine) or through the review channel; a `review_withdrawn` is `withdrawn` | gate visits; split by `channel`, `automatic`, `withdrawn`, `option:<name>` and `via:<surface>` |
| `question` | `question_answered` by `answered_by`; `human` and `supervisor` are interventions, `unattended` an automatic answer, `timeout` neither; `question_abandoned` apart | questions asked |
| `automatic` | applied `decision_made` of `completion_check`, `retry_advice`, `supervisor_triage`, `review_triage` and `routing`, once per use, node, list item, execution and attempt (a cached replay after a resume counts once; the same question in a later execution counts again). Judge answers are the playbook's own routing and are not counted | decisions asked; split `asked:<use>` and `applied:<use>` |
| `operator` | pause, resume, abort and `retry_expedited` with `by: operator`. The abort a parent's stop propagates into a sub-playbook child (`run_aborted` with reason `parent aborted`, `by` the parent's actor) is the parent's operator action, never the child's: the child keeps it apart as the engine's `abort_propagated` | runs |
| `supervisor_node` | `node_retry`, `run_continue_from`, a note aimed at a node, an interrupt whose control entry targeted one node (counted once per control entry), a profile rebind | node executions |
| `supervisor_run` | a note for the run, a patch, a broadcast interrupt (an interrupt with no node in its control entry is the run's correction whatever attempts observed it), and pause, resume, abort or expedite `by: supervisor` | runs |

The total counts every gate decision through the review channel, every
human and supervisor answer, every operator action and every correction.
It is called interventions, never human interventions: the supervisor
channel does not tell a supervising agent from a person (`apb note` posts
the same `context_append` as the MCP tool), and the review channel says
only the surface a decision came through. Automatic decisions, automatic
gate decisions and unattended answers are a separate `automatic_total`.

Kept apart and never in a total: `engine` (the engine's own markers:
`infra_retry`, `timeout_continuation`, `quota_probe`, `attempt_stalled`,
`interaction_downgraded`, `retry_superseded_by_stop`, a billing hold, a
refused directive, an expedite by a caller not shown to be a person
(`expedite_by_host`: the host is not the operator), the abort a parent's
stop propagated into a child (`abort_propagated`), and pause, resume,
abort or expedite `by: engine`);
`actor_unknown` (a pause, resume, abort or expedite without `by`); `other`
(a `supervisor_action` name this apb does not know, a newer apb's). The
`triage_retry` and `retry_advice` markers are the effects of automatic
decisions and are counted once, through their decision. A refused patch
(`patch_rejected`) or rebind (`rebind_rejected`) is its own event, not a
`supervisor_action`, and is not counted anywhere: the correction it
refused never happened. The refusal to move a run past a gate is the
engine's `directive_refused`, kept apart like the other engine markers.

**Absence is not zero.** Every count is `{ value, known_runs, runs }`. A
run written by an apb older than 1.0.0-beta.3 (`run_provenance.apb`, and
the `apb` of every resume; a run with none predates them) does not say who
decided a gate without `via` or `decided_by`, so its review and total
figures are unknown for that run and the text reads `known in N of M
runs`. A pause, resume or abort without `by` leaves the operator,
run-correction and total figures of its run unknown, never read as the
supervisor. Requests, decisions, questions, directives, rebinds and patches
exist in every journal and are known for every run. Medians and means are
over the runs where the total is known.

```
  interventions: 4 over 3 finished runs, median 1 and mean 1.33 per run, runs without any 1 of 3
    decided through the review channel: 2 over 3 gate visits (automatic 1, channel 2, option:approve 2, option:reject 1, via:dashboard_session 1, via:not_recorded 1)
    operator actions: 1 over 3 runs (pause 1)
    run corrections through the supervisor channel: 1 over 3 runs (context_append 1)
    automatic total: 1
    engine actions (not interventions): infra_retry 1
```

The `via` split names the surface a gate decision came through. A gate
with `approves` or `person_only` stamps it; ordinary gates never stamp a
surface, so their decisions read `via:not_recorded` (the surface is not
stamped, not unknown).

## The run estimate

`apb run --estimate`, MCP `playbook_get(estimate: true)`, the dashboard's
Run dialog and `GET /api/playbooks/{id}/estimate` estimate a run before it
starts; a top-level run with a budget, or one a launch surface showed an
estimate for (MCP and HTTP starts, `apb run` with a budget), keeps the
estimate it started with in `runs/<id>/estimate.json`, which its budget gate
weighs (see "budget" in `docs/HOWTO-authoring.md`). A run started with run
overrides keeps an estimate computed for its overridden bindings instead of
the one shown. A run with neither reads
no history at start. The method:

- **Comparable runs.** The newest 50 runs of the same playbook id, version
  and execution mode (`cli` or `host`: host steps often report no usage,
  and their cost differs) that apb created on this machine, among the
  project's newest 200 run directories (by the time their journal was last
  written), so a long history is never read in full. A run whose execution
  mode this apb does not know (a newer apb's) is never comparable. A node's spend samples come only from runs where it was bound to
  the same profile digest (its `profile.yaml` and SOUL), the same bundle
  digest (its skills) and the same executor digest (its chain, tiers and
  any run override, resolved for the run's overrides by the resolver the
  start uses): a run that differs is left out for that node alone and
  counted under the outermost part that differed, in
  `comparable.profile_mismatch`, `bundle_mismatch` or `executor_mismatch`.
  The estimate text names it: `not estimated: a (no history with this
  executor)` (or `profile`, or `skill set`). `apb run --estimate` takes
  `--overrides`, so the estimate of a run with an override is the one it
  starts with. `comparable.key_version` is `2` for this key; an
  `estimate.json` written before it has no such field, reads as `1`, and
  its text says it was matched by the profile-only key. Params are not
  matched.
- **Samples.** Time comes from succeeded runs only (active time, paused time
  left out; `human_wait_p50_ms` is the part spent waiting for gates and
  questions). Spend comes from every finished visit, failed ones too. A
  visit is a token sample only when every attempt in it reported usage, and
  a cost sample only when every attempt reported a cost: a partial sum is no
  sample; such attempts are counted (`attempts_without_usage`,
  `attempts_without_cost`, `runs_without_usage`). Tokens are input plus
  output, cache excluded, as in the stats above. A `playbook` node's spend
  is its child run's total.
- **Figures.** Per node: the median and range of one visit (what the budget
  gate weighs), of one run (all visits, so loops and retries as they
  happened), the median visits per run, and the median of one attempt (what
  an attempt that reported nothing is estimated as). A range is p25 to p75
  from 4 samples, the smallest and largest from 2 or 3, the value alone from
  one. A list node's run figure is its spend per item times its item count
  (the inline list after its filter, else the median count of earlier
  visits; unknown otherwise), and its time the median batch time times its
  batches. The run figures add up the nodes' medians, lows and highs: the
  sum of the medians stands in for the median of the sum, and the range
  assumes the nodes move together, so it is wide on purpose.
- **Without history** time falls back to the nodes' declared
  `expected_duration` (`basis: declared`). A gated node with no spend
  sample is listed in `unestimated` and left out of the run figures; below
  3 comparable runs the estimate carries a note that it is indicative only.
  Per dimension, `unestimated_tokens` and `unestimated_cost` list the gated
  nodes with no token or no cost figure (a node with token samples only is
  in `unestimated_cost`): the run's figure in that dimension leaves them
  out, and a limit in that dimension cannot weigh them. A visit with no
  finished attempt (a cache hit, a child run whose journal does not read)
  is no sample, never a zero one.

## JSON shape

```
{
  "runs": 4,
  "versions": [
    {
      "playbook": "demo", "version": "1.0.0", "runs": 2,
      "outcomes": { "succeeded": 2, "failed": 0, "aborted": 0, "other": 0 },
      "success":    { "count": 2, "of": 2, "rate": 1.0 },
      "first_pass": { "count": 1, "of": 2, "rate": 0.5 },
      "retries":         { "total": 1, "runs": 2, "per_run": 0.5 },
      "fallbacks":       { "total": 0, "runs": 2, "per_run": 0.0 },
      "loop_traversals": { "total": 0, "runs": 2, "per_run": 0.0 },
      "gate_wait":     { "count": 0 },
      "question_wait": { "count": 0 },
      "duration":      { "count": 2, "median_ms": 5000, "max_ms": 5000 },
      "spend": { "runs_with_usage": 0, "tokens": { "total": 0, "runs": 0 },
                 "runs_with_cost": 0, "cost_known": { "count": 0, "of": 2, "rate": 0.0 },
                 "cost_usd": 0.0 },
      "deliverable_missing": 0, "output_fields_missing": 0,
      "models": { "GLM-5.3-Flash": 3 }, "model_mismatch": 3,
      "goal": [ { "index": 0, "description": "tests pass", "check": "script",
                  "checked": 2, "passed": { "count": 1, "of": 2, "rate": 0.5 },
                  "failed": 1, "errors": 0, "manual": 0 } ],
      "findings": { "by_kind": [ { "kind": "security", "occurrences": 2, "runs": 2 } ],
                    "top": [ { "kind": "security", "location": "src/auth.rs", "runs": 2,
                               "occurrences": 2, "last_run": "demo-20260920-2" } ],
                    "runs_with_warnings": { "count": 1, "of": 2, "rate": 0.5 } },
      "baseline": [ { "check": "tests", "observation": "cargo_test", "compared": 2,
                      "met": { "count": 1, "of": 2, "rate": 0.5 }, "regressed": 1, "unknown": 0 } ],
      "interventions": { "runs": 2, "total": { "value": 1, "known_runs": 2, "runs": 2 },
                         "per_run_median": 0.5, "per_run_mean": 0.5,
                         "without_any": { "value": 1, "known_runs": 2, "runs": 2 },
                         "automatic_total": { "value": 0, "known_runs": 2, "runs": 2 },
                         "categories": [ { "category": "review", "unit": "gate visits", "denominator": 1,
                                           "count": { "value": 1, "known_runs": 2, "runs": 2 }, "split": {} } ],
                         "engine": {}, "other": {}, "actor_unknown": {} },
      "quality": [ { "criterion": "outcome", "source": "builtin", "grader": "rules/1",
                     "task_type": "default", "digest": "sha256:...", "graded_runs": 2,
                     "tally": { "good": 2, "partial": 0, "poor": 0, "not_applicable": 0,
                                "not_enough_data": 0 },
                     "good": { "count": 2, "of": 2, "rate": 1.0 },
                     "poor": { "count": 0, "of": 2, "rate": 0.0 } } ],
      "nodes": [ { "node": "w", "runs": 2, "first_pass": { "count": 1, "of": 2, "rate": 0.5 },
                   "retries": 1, "fallbacks": 0, "reentries": 0,
                   "duration": { "count": 2, "median_ms": 4000, "max_ms": 4000 },
                   "expected_s": 10, "over_expected": { "count": 0, "of": 2, "rate": 0.0 },
                   "deliverable_missing": 0, "output_fields_missing": 0,
                   "models": { "GLM-5.3-Flash": 3 }, "model_mismatch": 3,
                   "executors": [ { "profile": "1a2b3c4d5e6f", "bundle": "9f8e7d6c5b4a",
                                    "executor": "0c1d2e3f4a5b", "ephemeral": false,
                                    "runs": 2, "attempts": 3,
                                    "steps": [ { "binding": "5e6f7a8b9c0d", "step": 0,
                                                 "source": "primary", "executed_by": "cli",
                                                 "attempts": 2, "successes": { "count": 1, "of": 2, "rate": 0.5 },
                                                 "cost_known": { "count": 0, "of": 2, "rate": 0.0 },
                                                 "reported_models": { "none": 2 },
                                                 "closes_by_reason": { "check_rejected": 1 } },
                                               { "binding": "5e6f7a8b9c0d", "step": 1,
                                                 "source": "fallback", "executed_by": "cli",
                                                 "attempts": 1, "successes": { "count": 1, "of": 1, "rate": 1.0 },
                                                 "cost_known": { "count": 0, "of": 1, "rate": 0.0 },
                                                 "reported_models": { "none": 1 },
                                                 "closes_by_reason": {} } ],
                                    "transitions": [ { "from_step": 0, "to_step": 1,
                                                       "reason": "check_rejected", "count": 1 } ] } ] } ],
      "note": "2 runs: fewer than 10, the rates are indicative only"
    }
  ],
  "compare": { "playbook": "demo", "base": "1.0.0", "against": "1.1.0",
               "success_delta": -0.5, "first_pass_delta": 0.0,
               "retries_per_run_delta": -0.5, "loops_per_run_delta": 0.0,
               "median_duration_delta_ms": 0,
               "quality": [ { "criterion": "outcome", "grader": "rules/1",
                              "task_type": "default", "digest": "sha256:...",
                              "good_delta": -0.5, "poor_delta": 0.5 } ] }
}
```

Versions are sorted by playbook id, then by version number, oldest first. A
`rate`, `per_run`, `median_ms`, `max_ms`, `expected_s`, `over_expected`,
`cost_per_run_usd`, a `compare` delta or `note` is left out when there is
nothing to compute it from, and so are `quality` and `compare.quality` when
they would be empty. `compare.against` is absent when no other version
has runs. With no matching run the report is `{ "runs": 0, "versions": [],
"note": "no runs recorded" }`.
