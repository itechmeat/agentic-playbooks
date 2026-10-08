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
- **run duration**: `run_started` to the terminal event. Median and count.
- **tokens and cost** per run, over the runs whose attempts reported usage
  (`attempt_finished.usage`); the cost is what the agent CLIs reported, over
  the runs that reported one. Usage marked `estimated` (a source reserved for
  counts apb makes itself; nothing records it yet) is summed together with
  reported usage and is not marked apart in these figures.
- **missing deliverables and output fields**: `deliverable_missing` and
  `output_fields_missing` counts.
- **goal**: per criterion, how often it passed over the runs that checked it
  automatically, failed, could not run (`error`), or was left to a person
  (`manual`), from the `goal_checked` events.
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
  `experimental`, `stable`). So `opus` matches `claude-opus-4-1` and
  `sonnet` matches `claude-sonnet-4-5-20250929`, while `glm-5.3` and
  `glm-5.3-flash`, or `gpt-5` and `gpt-5-mini`, are different models.

Per node: the runs it ran in; first pass (its first result was a success on
attempt 1, with no retry, fallback or later re-entry); retries, fallbacks and
re-entries; the median duration (`node_started` to `node_finished`) and, when
the node declares `expected_duration`, how many executions took longer; the
models its attempts ran on and `model_mismatch`.

Every rate is printed with its count (`7/9 (78%)`), and a version with fewer
than 10 runs carries a note: small samples mislead.

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
                 "runs_with_cost": 0, "cost_usd": 0.0 },
      "deliverable_missing": 0, "output_fields_missing": 0,
      "models": { "GLM-5.3-Flash": 3 }, "model_mismatch": 3,
      "goal": [ { "index": 0, "description": "tests pass", "check": "script",
                  "checked": 2, "passed": { "count": 1, "of": 2, "rate": 0.5 },
                  "failed": 1, "errors": 0, "manual": 0 } ],
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
