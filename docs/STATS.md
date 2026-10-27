# Run stats: `apb stats`

`apb stats` answers "is this playbook getting better or worse" from the run
journals alone. It asks no model, sends nothing anywhere and writes nothing:
every figure comes from the `.apb/runs/<id>/events.jsonl` journals (and each
run's playbook snapshot, for `expected_duration`). Like
`apb decisions report`, it reads only runs apb created on this machine; a
run directory that came with a repository is skipped and counted on stderr.

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
  the runs that reported one. Usage marked `estimated` (a source reserved for
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
- **models**: how many attempts ran on each model, and `model_mismatch`, how
  many of them ran on another model than the node profile's primary model
  (from the run manifest). A CLI attempt ran on the model its
  `attempt_started` names; a host attempt (host execution mode, or the host
  fallback) on the model the host reported in `host_task_submitted.model`,
  or `unreported` when it named none. In host mode the profile's model is
  only a hint, so this is where a run that went entirely to another model
  shows. Two names count as the same model when their lowercase tokens
  (split on every non-alphanumeric character) are equal, or the shorter
  name's tokens appear in order in the longer one and every extra token is
  a version or date number or a vendor or channel word (`claude`,
  `anthropic`, `openai`, `google`, `gemini`, `latest`, `preview`, `exp`,
  `experimental`, `stable`). So `opus` matches `claude-opus-5-5` and
  `sonnet` matches `claude-sonnet-5-5`, while `glm-5.3` and
  `glm-5.3-flash`, or `gpt-5` and `gpt-5-mini`, are different models.

Per node: the runs it ran in; first pass (its first result was a success on
attempt 1, with no retry, fallback or later re-entry); retries, fallbacks and
re-entries; the median duration (`node_started` to `node_finished`) and, when
the node declares `expected_duration`, how many executions took longer; the
models its attempts ran on and `model_mismatch`.

Every rate is printed with its count (`7/9 (78%)`), and a version with fewer
than 10 runs carries a note: small samples mislead.

**Per executor digest.** When the runs of one version bound a node to more
than one executor (its profile, model, SOUL or skills changed between runs),
the node lists its figures per digest pair: `profile` (the profile digest:
`profile.yaml` plus SOUL) and `bundle` (the profile plus its skills), each
shortened to 12 characters, the runs, first pass, median duration and the
median tokens of a visit that reported usage on every attempt (`digests` in
the JSON; text: `    profile 1a2b3c4d5e6f (bundle 9f8e7d6c5b4a): 3 runs,
first pass 2/3 (67%), median 1.0 min, 200 tokens per visit`). With
`--compare`, `executor_changes` names each node whose latest executor
differs between the two versions (`node`, `base_profile`, `against_profile`,
`base_bundle`, `against_bundle`; text: `executor of a: profile X -> Y,
bundle X -> Y`), so a change in the figures can be told from a change of
model or prompt. The dashboard's stats card shows the same groups.

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
  the same profile digest (its `profile.yaml` and SOUL, so its model and the
  run's overrides): a run where another profile drove the node is left out
  for that node alone and counted in `comparable.profile_mismatch`. Skills
  (the bundle digest) and params are not matched.
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
      "nodes": [ { "node": "w", "runs": 2, "first_pass": { "count": 1, "of": 2, "rate": 0.5 },
                   "retries": 1, "fallbacks": 0, "reentries": 0,
                   "duration": { "count": 2, "median_ms": 4000, "max_ms": 4000 },
                   "expected_s": 10, "over_expected": { "count": 0, "of": 2, "rate": 0.0 },
                   "deliverable_missing": 0, "output_fields_missing": 0,
                   "models": { "GLM-5.3-Flash": 3 }, "model_mismatch": 3 } ],
      "note": "2 runs: fewer than 10, the rates are indicative only"
    }
  ],
  "compare": { "playbook": "demo", "base": "1.0.0", "against": "1.1.0",
               "success_delta": -0.5, "first_pass_delta": 0.0,
               "retries_per_run_delta": -0.5, "loops_per_run_delta": 0.0,
               "median_duration_delta_ms": 0 }
}
```

Versions are sorted by playbook id, then by version number, oldest first. A
`rate`, `per_run`, `median_ms`, `max_ms`, `expected_s`, `over_expected`,
`cost_per_run_usd`, a `compare` delta or `note` is left out when there is
nothing to compute it from. `compare.against` is absent when no other version
has runs. With no matching run the report is `{ "runs": 0, "versions": [],
"note": "no runs recorded" }`.
