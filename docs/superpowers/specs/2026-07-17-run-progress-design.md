# Run progress percentage - design

Date: 2026-07-17
Status: implemented, in review

## Goal

Show a live "percent done" for every run on the web dashboard (runs list and
run view). The number must be honest about slow vs fast nodes and about loops
with a known amount of work (e.g. a book translation decomposed into chapters),
and it must update by itself while the run executes, without a page reload.

## Non-goals (out of scope for this iteration)

- ETA display ("about 12 minutes left"). The time-based model makes it a
  trivial follow-up (sum of expected durations of unfinished work), but it is
  not part of this design.
- Progress movement inside a single running node (time interpolation,
  animation-driven estimates). The percent changes only on real facts.
- Automatic rewriting of playbook estimates by the engine. Calibration stays
  an explicit `playbook_update` by the maintaining agent.

## 1. Schema: `expected_duration` on nodes

`Node` (apb-core `schema.rs`) gets one optional field, common to all kinds:

```yaml
- id: translate_chapter
  type: agent_task
  expected_duration: 10m
  ...
```

- Format: integer seconds (`90`) or a string with a single unit suffix -
  `30s`, `5m`, `2h`. Parsed to seconds at load; invalid values are a
  validation error.
- Semantics: the estimated wall time of ONE execution of the node. For nodes
  inside a cycle this is the per-iteration time.
- `#[serde(default)]`, additive to schema 2. No migration; existing playbooks
  stay valid.

Defaults when the field is absent:

| Node kind | Default |
|---|---|
| start, finish, condition, prompt | 0s (instant) |
| human_review, wait | 0s (waiting is not work, see section 6) |
| agent_task, script | 120s (named constant, one place in code) |

Authoring guidance: `playbook_howto` gains a rule - when creating or editing a
playbook, estimate `expected_duration` for every agent_task and script node.
A rough guess is fine; trial runs correct it (section 5).

## 2. Progress model

Progress is a pure fold over the run's persisted record, exactly like
`RunState::fold`: no separate mutable state, so resume, server restart, and a
second machine all show the same number.

Base formula:

```
percent = expected_seconds(finished nodes) / expected_seconds(all counted nodes)
```

- "Finished" = Succeeded. Failed/TimedOut/Interrupted nodes do not count as
  done work.
- "Counted nodes" starts as all nodes of the playbook version bound to the
  run. Nodes that become Skipped or Cancelled (pruned branches, join:any
  losers) leave the denominator together with their expected time, at the
  moment the pruning event is recorded.
- Parallel branches need no special casing: every node contributes its own
  expected time regardless of which branch it is on.
- Retries and fallback executors re-run the same node; its contribution is
  counted once, so a retry never moves the percent backward.

## 3. Cycles and explicit progress reports

A cycle (a node group reachable through a back edge, bounded by
`condition.max_loops`) has an unknown iteration count at planning time.

- Without any report: the cycle group is counted as ONE pass (each node once).
  Nodes of the current iteration count as done when they succeed; when the
  cycle loops back, their contribution stays at "one pass done" and does not
  grow further. The group is fully credited only when the run exits the cycle.
  Coarse, but never lies upward.
- With a report: a new event `RunProgress { node_id, done, total, label }`
  scales the group. Once `total` is known, the group's denominator becomes
  `total * expected_seconds(one pass)` and its numerator
  `done * expected_seconds(one pass)`. Later reports update `done` (and may
  update `total`).
- The latest `RunProgress` wins; the latest non-null `label` (e.g. "chapter 3
  of 14") is shown next to the bar in the UI. A report without a label keeps the
  previous text.
- The summary also carries a `plan_key`: a deterministic identity of the work
  plan, made of the playbook version bound to the run plus the latest reported
  total of each cyclic group. It changes exactly when a report moves a group's
  total or the run migrates to a patched version, and stays stable on ordinary
  done/label updates. The UI uses `plan_key` (not the display label) as the
  monotonicity reset signal.

## 4. Reporting channel: MCP tool + single-writer invariant

New MCP tool `run_progress_report(run_id, done, total, label?)`, callable by
a node's executing agent or by the supervisor.

Plumbing follows the existing supervisor-command pattern: the tool does NOT
append to `events.jsonl` itself. It posts a command via
`post_supervisor_command` (a new `Control::Progress` variant); drive drains
the channel at node boundaries and wake points and appends
`EventPayload::RunProgress` itself. The single-writer invariant on the event
log is preserved.

Latency note: a report posted mid-node materializes at the next drive
boundary. In the intended pattern (one cycle iteration = one node execution,
the agent reports at the end of its task) that boundary is immediately after
the report, so the practical lag is negligible.

New `EventPayload` fields follow the house rule: `#[serde(default)]` only.

## 5. Calibration from real runs

`events.jsonl` already carries timestamps for node start and completion, so
actual durations are known after any run:

- `playbook_trial` report and `run_report` gain a per-node table:
  `expected_duration` vs measured duration (last run / mean when several).
- The maintaining agent updates the playbook estimates via `playbook_update`
  (new minor version, normal trust flow). The engine never silently rewrites
  the playbook.

## 6. Waiting is not work

`human_review` and `wait` nodes carry 0 expected seconds. While a run sits on
one of them, the UI freezes the bar and shows a "waiting for decision" (or
"waiting for event/timer") badge instead of pretending progress. The paused
run status is already available; this is a display rule, not an engine change.

## 7. Display rules

- Runs list: each running run's card shows a progress bar plus the percent.
  Finished runs show status only, no bar.
- Run view header: bar, percent, and the latest `RunProgress.label` when
  present.
- Monotonicity: the displayed percent never decreases, with one exception -
  a change of the work plan itself (a `RunProgress` raising `total`, or a
  supervisor patch adding nodes). Then an honest drop is allowed.
- Terminal states: succeeded pins to 100 percent. On any terminal status
  (succeeded, failed, aborted) the bar is hidden; the status itself conveys
  the outcome.
- Live updates require nothing new: appending the event touches the run dir,
  `watch.rs` broadcasts `runs_changed`, the dashboard refetches.

## 8. Surfaces and API

- Engine: `progress.rs` (or an extension of `inspect.rs`) computing
  `RunProgress` summary `{ percent, label, waiting_on, waiting_kind, plan_key }`
  from events + the playbook version bound to the run.
- Server: the runs list and run detail endpoints include that summary.
- MCP: `run_status` includes the same summary, so driving agents can quote
  progress in chat.

## 9. Validation

- New warning V19: an `agent_task` or `script` node without
  `expected_duration` (nudges authors and authoring agents; never blocks).
- New error V20: unparsable `expected_duration` value. V15 stays retired;
  V19/V20 are the next free codes.

## 10. Edge cases

- Resume after crash: fold over persisted events reproduces the same percent.
- Supervisor patch: the denominator is recomputed against the patched
  version bound to the run; an honest percent drop is allowed (section 7).
- `RunProgress` with `done > total` or `total = 0`: clamped, never a panic;
  the event is still recorded verbatim.
- Multiple cycles in one playbook: `RunProgress.node_id` binds a report to
  its cycle group; groups scale independently.
- Legacy runs without the new event or field: formula degrades to equal
  defaults per node kind; nothing breaks.

## 11. Testing

- apb-engine unit tests: fold math (weights, skips, retries, cycle scaling,
  clamping), boundary materialization of `Control::Progress`.
- apb-mcp test: `run_progress_report` posts the command; `run_status` carries
  the summary.
- Validator test: V19 fires and does not block.
- Web (vitest): bar rendering states - running, waiting badge, terminal,
  label display, monotonic display across refetches.
