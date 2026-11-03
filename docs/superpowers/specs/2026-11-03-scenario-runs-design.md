# Scenario runs: check a playbook before an expensive run

Date: 2026-11-03. Status: accepted. Author-facing usage:
`docs/EVALS.md` ("Scenario runs"), the review route rules in
`docs/HOWTO-authoring.md` (V144, V145), the history rule in
`docs/STATS.md`, and the decision snapshot in `docs/DECISIONS.md`.

## 1. Goal

The only way to see a playbook route today is to run it: real agents,
real scripts, real reviews, real time and money. A mistake in the graph
(an edge that never matches, a join that waits forever, a retry loop
with no way out, a template that reads a field nobody writes) shows up
only after the expensive part has run. `apb eval` helps, but an eval
case also runs real agents in a scratch repository and cannot reach a
playbook with a publish node, a connector or a sub-playbook.

A scenario run drives the real engine with every answer that comes from
outside apb scripted in a file: what an agent attempt returned, what a
script printed, what a person decided at a review, what a judge
answered, how a wait ended, what a child playbook produced. It checks
the routing (edges, joins, retries, fallbacks, run outputs) in seconds,
without starting a program, calling a connector or touching the project
tree, and it says which expectation failed by name.

Beside it the same change:

1. moves the judge evaluation into one function that a run and a
   scenario run share, with a digest of the judge's definition;
2. adds two validator warnings for review routes that cannot work;
3. records a run's origin and keeps every run that is not a production
   run out of the project's history;
4. fixes defect 7: a playbook with judges but no agent nodes never asked
   a decision provider.

## 2. The scenario file

A scenario lives at `.apb/playbooks/<id>/evals/scenarios/<name>.yaml`,
beside the eval cases and outside every version, so editing one never
changes a version's trust digest. The suite digest covers the whole
`evals/` tree, so a scenario edit does ask for a new suite approval
before the next deep `apb eval`. A directory holding only `scenarios/`
is no eval suite: a playbook that an eval cannot run may still carry
scenarios.

Decisions:

1. **The name is a path segment, checked before it is one.** The name
   matches `[a-z0-9][a-z0-9-]*`, at most 64 characters, and equals the
   file's `id`. apb reads only regular files below the playbook
   directory, never through a symbolic link, at most 1 MiB, as UTF-8.
2. **Strict shape.** `schema: 1` and `id` are required, every other key
   is optional, and an unknown key at any level is an error, so a typo
   never becomes an answer nobody reads.
3. **Answers are keyed by node and visit.** `answers` maps a node id to
   one entry per visit (1-based, or `default` for every visit with no
   numbered entry). An entry carries the one answer kind the node takes:
   `attempts` for an agent step, `script`, `review`, `judge`, `wait`,
   `playbook` (a nested scenario of the child or its result), `publish`,
   or `items` for a list node. Two nodes cross two boundaries and take a
   pair: a list with a judge item check (`items` and `judge`), and a
   judge that falls back to emulation (`judge` and `attempts`).
   `judge_edges` answers the judge edges out of a node by edge address.
4. **Missing and extra answers are explicit.** `on_missing: error` (the
   default) makes the scenario `invalid` at the first boundary without
   an answer; `fail` fails that boundary the way the real one fails and
   goes on. `on_extra: error` (the default) fails a scenario that left an
   answer unused; `warn` only lists it. A `default` visit and an item
   answer keyed `"*"` are never unused.
5. **Expectations are named checks.** `expect` takes the eval case's
   `run` and `outputs`, plus `finish`, `route` (with `path`, the exact
   sequence of executions), `visits`, `edges.taken` and
   `edges.not_taken` by edge address, `joins.dead`, `run_outputs` by
   state, and `templates_empty` (`error` by default, `report` or
   `ignore`).
6. **Staleness.** `versions` (the eval case syntax) and an optional
   `playbook_digest` pin say which versions a scenario is for. A stale
   scenario plays only on request (`--allow-stale`).
7. **The scenario digest** is `sha256:` over the canonical JSON of the
   parsed file, with defaults written out, so formatting does not move
   it and any answer, expectation, param or mode does.

## 3. Levels and their guarantees

`apb eval <id> --level quick|smoke` plays the playbook's scenarios;
`deep`, the default, is the existing eval. A level flag apb does not
know is refused, never ignored.

| Level | What reaches outside apb |
|-------|--------------------------|
| `quick` | nothing: no program starts, no connector is called, no decision provider is asked, and files are written only below `.apb/scenario-runs` |
| `smoke` | as `quick`, except that this machine's decision providers may be asked, at most N HTTP requests per scenario |

### 3.1 How the guarantee holds

1. **One process per scenario, in scenario mode for its whole life.**
   Each scenario plays in a hidden child, `apb __scenario-run`, which
   enters scenario mode before anything else runs and never leaves it.
   In that mode every program start (an agent, a script, a success
   check, a project command, git, agent detection, a secret helper, the
   sandbox and process probes, a detached driver or waker), every
   connector call and every state write outside the scenario run's
   directory is refused with `scenario_guard: <operation>`. At `quick` a
   decision request is refused too, and the decision client sends none.
2. **A refusal makes the verdict `invalid`.** A scenario whose
   boundaries are all scripted reaches no guard, so a refusal names a
   site that would have acted for real. The process keeps every
   operation it refused, even where the caller went on without the
   result, and the verdict names them.
3. **The guard holds in read-only mode too**, and a read-only process
   cannot enter scenario mode.
4. **The check and the play agree.** The child refuses a scenario or a
   version whose digest differs from the one the command checked.
5. **Bounded in time.** The command waits 60 s for a quick scenario and
   300 s for a smoke one, then kills the child's process group; the
   scenario is `incomplete`.
6. **Nothing of the project is taken.** No busy lock, working tree,
   import approval, isolation contract, consent, launch check,
   notification, remembered input, known problems, budget, estimate or
   node cache. A param the playbook declares `secret` holds a
   placeholder and a scenario may not set one (V149).

### 3.2 Smoke

A smoke run sends template text to the machine's providers at their
cost, so it needs what a quick run does not:

1. the version approved by its digest, and an approved import when the
   version was imported with scripts;
2. a person's go-ahead: a terminal question naming the cap and the
   number of scenarios, or `--yes`;
3. a caller that is not a run step.

The cap (`--max-decision-requests`, 20 by default, 1 to 200) is strict
per HTTP request across the scenario's whole run tree: a nested child
scenario takes from its parent's count. Before an ask the run reserves
the most requests that ask can send (every provider on the route, each
with the client's retries) and an ask that does not fit journals
`budget` and the judge takes its `on_unavailable`. A reservation comes
back only when nothing went out, since the client does not report how
many requests an ask sent. Only the judge uses run; the engine's own
decision uses stay off.

### 3.3 The drive

A scenario run is an ordinary drive: the same routing, joins, retries,
fallbacks, failure classification, checks and journal as a real run.
Only preparation and the answer at each boundary differ.

1. Every node that binds a profile runs the scripted agent, journaled
   with agent `scenario`, no model and `executed_by: scenario`.
2. A quota pause is due at once and resumed by the engine, still within
   its limit of six per node. A delayed retry is journaled and due at
   once; no waker starts. A wait takes its scripted end at once.
3. A `read_only` node takes no tree state; a scripted `dirty` answer
   journals the real violation and fails the node without retry.
4. A review or publish with no answer makes the scenario `invalid`
   whatever `on_missing` says, since a real one cannot fail.
5. At `quick` a judge is answered by the `scenario` provider
   (uncalibrated), through the same evaluation a run uses (section 5).
6. A list item's `script` or `connector` check is not run: the item
   takes the `check` of its scripted attempt.
7. Each answer used and each template reference that rendered empty is a
   record in the run's diagnostics sidecar, never a journal event.

### 3.4 The report and storage

A report is built only from what the run left (journal, execution
index, diagnostics sidecar, the record of answers): every expectation
as a named check with what it expected and found, and the trace (each
execution with its visit, attempts and the source of each answer, the
edges taken by address, dead join inputs, empty template references,
answers used, defaulted, missing and unused, and at smoke the requests
and their cost). The verdict is `passed`, `failed`, `invalid`, `stale`
or `incomplete`; the exit code is 0, 1, 2 for a refusal before any run,
or 3 for invalid, stale or incomplete (3 wins over 1).

Runs live under `.apb/scenario-runs/<run id>/`, with a child playbook's
run next to its parent's, and an index of finished runs per playbook.
Retention keeps the newest 20 run directories per playbook, scenario
and level, and at most 1000 index lines per playbook.

## 4. Run origin and the history rule

A scenario run is no evidence of how a playbook performs, and an eval
repetition is not either. If either counted, a cheap scripted run could
inflate a candidate's trial count or quality evidence and push an
automatic promotion.

1. **The manifest records `origin`**: absent for a production run (every
   run before this change, and every production run after it),
   `scenario` with the level, the scenario name and its digest, or
   `eval`. It is written once at start and read leniently: an origin a
   newer apb wrote reads as unknown.
2. **Only production runs are history.** Statistics, the run estimate,
   the quality comparison of a candidate, candidate trials and a
   retrospective's baseline read one filter. A run with no manifest or
   no `origin` is production; an unknown origin, or a manifest that
   does not read, is no history, so a run whose kind cannot be told
   never becomes evidence.
3. **Scenario runs are stored apart.** `.apb/scenario-runs/` is outside
   `.apb/runs/`, so no run surface, `apb resume` or released binary sees
   them, and `.apb/.gitignore` ignores the directory.

## 5. The shared judge evaluation and the judge digest

A judge node, a judge edge and a list's judge item check now evaluate
through one function: render the state, ask, apply the thresholds and
the mode rule, and say what the node outputs. A run renders its inputs
from the run state; a scenario run takes them from its answers. Both
therefore agree on the state digest, the raw answers and the output,
and the run path's events are unchanged.

Each evaluation carries the judge's **node digest**: `j1:` and the
`sha256:` of the canonical JSON of everything that decides the answer
(the state templates, the questions with their type, instructions,
criteria and levels, the thresholds and their bands, `on_unavailable`),
in the order written, since a model reads them in that order. The
title, expected duration and profile are left out. The name is
`node_digest`, because `definition_digest` already names the playbook
digest; a change of the prefix starts a new digest family. It lets a
later tool tell that a label or a result was made against another
definition of the same judge.

## 6. Defect 7: a judge-only playbook never asked

A run snapshots the machine's decision settings at start, and every
decision use reads the snapshot. The snapshot was taken only when the
run had agent nodes, so a playbook whose decisions come from judges
alone ran every judge as `not_configured`. The snapshot is now taken
when the playbook asks a decision model itself: a judge node, a judge
edge, a list's judge item check, or a review with `auto_decide`, with
or without agent nodes.

This changes what leaves the machine. On a machine whose `judge_node`
or `judge_edge` use is above `off`, a judge-only playbook now sends its
judge state to the configured providers, under the same `privacy` rules
as every other use. That is what the settings ask for, but it is new
for such playbooks, so the release notes say so. There is no opt-in
flag: the use's mode is the switch.

## 7. Validator

| Code | Severity | Rule |
|---|---|---|
| V144 | warning | a `review_status` edge that can never match: its source is not a `human_review`, or its value is not one of the source's effective options |
| V145 | warning | a `human_review` option no edge can carry: no matching `review_status` edge, no unconditional edge, no other edge that may match, no fallback |
| V146 | error | the scenario file on its own: does not read or parse, unknown field, `schema` not 1, `id` not the file name or outside the grammar, malformed `versions`, a visit with no answer or a kind pair no node takes, a malformed wait or question answer, a succeeded attempt with a failure kind, a goal key that is not an index |
| V147 | error | an answer key: an unknown node, a kind the node does not take, a visit not positive or numbered twice, more than one `default`, a goal index that is not a script criterion, a baseline answer for an unknown check |
| V148 | error | an answer value: a review or publish decision that is not an option, a judge question, option, level or probability the node does not allow, a judge edge address that does not resolve to a judge edge of the node, a bad nested scenario name, a child output that is not an object where the parent reads fields |
| V149 | error | a param the playbook declares `secret` is set |
| V150 | error | an expectation names an unknown node, edge or run output, a finish that is not a finish node, a join input that does not feed the join, an outcome or state apb does not write, or a regex that does not compile |
| V151 | warning | a visit answers more attempts than the node's `max_retries` allows (infrastructure and quota failures not counted) |

V144 and V145 apply to every playbook. V146 to V151 are checked by
`apb validate` for every scenario of the playbook; a scenario whose
`versions` exclude the version is not held to it, but its file problems
still are. Before any scenario plays, `apb eval --level quick|smoke`
refuses a version with a validator error and any scenario error.

## 8. Compatibility

1. **Older binaries never see a scenario run.** It lives outside
   `.apb/runs/`, so `apb runs`, `apb runs <id>` and `apb resume` of a
   released binary find nothing; the journal corpus checks this against
   the released binaries (row S1).
2. **Manifest `origin`** is a new optional field. A production run never
   writes it, so a production manifest is byte-for-byte what it was.
3. **Diagnostics records** `scenario_answer` and `template_empty` are
   sidecar records, never journal events; a real run writes none.
4. **Defect 7** is the one change a real run sees: a judge-only playbook
   on a machine with a judge use above `off` now asks. The journal shape
   of a judge is unchanged.
5. **The scenario files** are outside every version and bundle digest,
   and inside the suite digest.

## 9. Tests

Every check drives a shipped entry point.

1. A quick scenario through `apb eval` with a `PATH` of shims for every
   shell, git and agent CLI and a counting HTTP stub as connector,
   notification channel and decision provider: exit 0, no shim started,
   no request, and a snapshot of the project tree (sizes and times
   outside `.apb/scenario-runs`) equal before and after.
2. The report holds the trace: executions, visits, attempts, edges by
   address, the dead join input.
3. Seeded mistakes in a fixture playbook each fail with the expected
   check name; the route rules give V144 and V145 in `apb validate`.
4. Extra and missing answers under each `on_extra` and `on_missing`.
5. Smoke with a cap of 2 against the stub provider sends two requests
   and the next judge question journals `budget`.
6. A real run with the fake agent and provider and a quick run with the
   same answers give equal normalized journals.
7. A judge-only playbook at `enforce` snapshots the settings and routes
   by the answer; without `decisions.yaml` there is no snapshot.
8. The deferred retry, read-only and unattended paths through quick
   scenarios.
9. `apb stats`, the estimate and the candidate comparison skip scenario
   and eval runs; an eval run's manifest says `origin: eval`.

## 10. Not covered

Scenario surfaces on MCP, the HTTP API and the dashboard (the CLI
proves the engine; the surfaces come with the judge sandbox and the
candidate summary); a model of the `llm_emulation` provider (a judge
that falls back to emulation goes to its profile's scripted attempts);
scripted connector and script item checks (they take the attempt's
`check`); redacting a local report beyond the run redactor; a budget in
a scenario run.
