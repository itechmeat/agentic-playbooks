# Eval suites (`apb eval`)

`apb stats` measures what real runs of a playbook did. An eval suite
measures what a configuration of a playbook (its version, profiles and
models) does on fixed inputs, repeatedly, before real runs depend on it. A
suite is a set of cases next to the playbook's versions; `apb eval` runs
each case as an ordinary run in a disposable repository, checks the
outcome, stores the result under a key made of the configuration's digests
and compares it with the previous stored result.

Evals are opt-in and paid: every repetition is a full agent run. Nothing
starts one except an explicit `apb eval`; saving a version, writing a
profile or promoting a patch never does.

`apb eval` shipped in 0.24.0 as the first half of the design. Not built
yet: mock connectors (and so any playbook that binds a connector),
scripted gate and question answers, `stop_before`, push checks, reporting
which cases a `playbook_update` or `profile_write` invalidates, the
`after_eval` promotion policy, a dashboard card, and an MCP tool. `apb eval`
is CLI only.

## Never with irreversible effects

An eval run is not a sandbox. It runs in a scratch repository whose only
remote is a local bare repository, but the agent keeps the network and
every CLI the operator is logged in to. apb therefore never runs a playbook
in an eval that could ship something:

- a playbook whose effective effects contain `irreversible`,
- a node that declares `irreversible` or `secrets` effects or looks like a
  merge, push, deploy or publish step (the V73 evidence chain),
- a node that binds a connector (real connectors are never called from an
  eval, and mocks come later), including a list node whose item check reads
  a connector (`check.connector`, the `<node>#check` binding site),
- a `playbook` node (the child's effects are not visible to the case).

Such a playbook is refused before anything is created
(`eval_refused_effects`, exit 2), and `apb validate` reports it as V81.
There is no case-level switch that lifts the refusal.

List nodes (`list:`) are allowed otherwise: an eval runs their items and
their `output`, `script` and `judge` item checks as a real run does. To
assert how many items a
case handled, give the playbook a goal criterion `{ type: list, node: <id> }`
(HOWTO-authoring, "goal"): with `checks.goal: required` the case then
passes only when that list processed every item and none needs a look. An
`events` check may also count `list_started`, `list_batch_started` and
`list_item_settled`.

What the agent still has:

- The operator's environment. Agents inherit the environment `apb eval`
  runs in, minus the variables an installed connector references (the same
  scrub as every run); `environment: minimal` changes which claude settings
  load, not the environment variables. `ANTHROPIC_API_KEY`, `GH_TOKEN`,
  cloud credentials and the like reach eval agents unless the case env cuts
  them.
- Logged-in CLIs and git credential helpers. The scratch repository clears
  `credential.helper` for itself, but an agent can still run `gh`, a cloud
  CLI or git in another directory with the operator's credentials. Use the
  case `env` to cut the ones you know about (for example `GH_CONFIG_DIR`
  pointing into the scratch directory and an empty `GH_TOKEN`).
- The operator's filesystem. Nothing confines the agent to the scratch
  tree; the run journal the checks read lives in that tree, so an agent
  could edit it (the result records `journal_agent_writable: true`).

The agent's own configuration follows the node's profile, as in any run
(PROFILES.md, "Agent environment"). With the default `environment: minimal`
a claude node loads only the scratch repository's project and local
settings, so the operator's user-scope hooks, plugins and MCP servers stay
out; hooks in a project `.claude/settings.json` that the fixture carries do
run, as they would in that repository. A profile with `environment: full`
loads the operator's whole setup in every eval run: a user-scope hook can
then write outside the scratch repository (a memory or logging hook
records the eval as real work) and its extra context changes what is being
measured. The plan names each such node before anything starts; point the
case at a `minimal` profile with `--profile-override` when the full setup
is not what the playbook needs.

## Layout

```
.apb/playbooks/<id>/
  1.4.0/playbook.yaml       versions, unchanged
  current, lifecycle, meta/
  evals/
    suite.yaml              optional defaults
    <case>.yaml             one case per file, id = file stem
    scripts/                case check scripts
    fixtures/               fixture directories
    scenarios/              scenario files (see Scenario runs below)
    judge/<node>/           labelled judge examples (docs/DECISIONS.md, "Judge sandbox and labelled examples")
```

The registry only lists `major.minor.patch` directories as versions, so
`evals/` is never a version, and editing a case never changes a version's
trust digest. One suite applies across versions; a case can narrow itself
with `versions`. Eval cases (deep) run for project playbooks only;
scenarios (quick and smoke) run for project and global playbooks (see
"Scenario runs" below).

A playbook has a suite when `evals/` holds `suite.yaml` or a case file
directly. A directory holding only `scenarios/` is no suite: `apb validate`
checks no case for it and raises no V81, so a playbook an eval cannot run
(a publish node, a connector, a sub-playbook) may still carry scenarios.
The suite digest covers the whole `evals/` tree, `scenarios/` included, so
editing a scenario, or saving a labelled judge example, asks for a new
suite approval before the next `apb eval`.

## A case

```yaml
schema: 1
id: review-planted-defect         # [a-z0-9-], = file stem
title: The review finds a planted off-by-one error
source: "issue #74 findings 4 and 5"
versions: ">=1.4.0"               # optional: comma-separated >=, >, <=, <, =
instruction: |
  Review the checked-out branch `eval-change` against `main`.
params: {}                        # must be declared by the playbook
fixture:
  dir: fixtures/window-max/base   # or git: <commit, tag or branch>
  change: fixtures/window-max/planted
  branch: eval-change             # default eval-change
env:                              # overlay for the agents and scripts
  GH_CONFIG_DIR: "{{eval.scratch}}/gh"
checks:
  goal: required                  # required (default) | report | ignore
  run: { outcome: [succeeded] }   # default [succeeded]; also failed, aborted, stopped
  route:
    visits: [review, done]
    in_order: true
    not_visits: [failed]
    max_visits: { review: 1 }
  outputs:
    - { node: review, matches: "(?i)off-by-one" }
    - { node: review, field: verdict, equals: approve }
  files:
    - { path: docs/reviews/_date_time_review.md, exists: true }
    - { path: src/lib.rs, unchanged_from_fixture: true }
    - { path: README.md, matches: "window_max" }
  events: { absent: [run_error], max: { retry_started: 2 } }
  deliverables: complete
  scripts: [scripts/names-the-planted-defect.sh]
limits:
  timeout: 30m                    # default 45m
  max_usd: 0.50                   # per repetition, when the CLI reports cost
  max_tokens: 2000000             # per repetition, input plus output
repeat: 3                         # default 1
tags: [quick]
```

`suite.yaml` takes `env`, `limits`, `repeat`, `tags` and
`budget.max_usd_per_invocation` (default 10 USD); a case's values win, the
env maps merge and the tag lists add up.

The `env` overlay is set on every agent, script node, goal script and case
script the run spawns, after the connector scrub. It never reaches apb
itself: the `apb run` that starts the repetition gets it as a run setting,
and its config directory is pinned to the one `apb eval` resolved. So a
suite cannot set a variable that reconfigures apb, the shell, the loader or
git (V80): `APB_*`, `HOME`, `XDG_*`, `PATH`, `LD_*`, `DYLD_*`, `GIT_DIR`,
`GIT_WORK_TREE` and the other repository-location variables, `GIT_CONFIG*`,
and the shell variables (`SHELL`, `ENV`, `BASH_ENV`, `IFS`, `CDPATH`,
`BASH_FUNC_*` and similar).

A case that sets `connectors`, `answers`, `stop_before`,
`checks.connector_calls` or `checks.pushed` is refused (V80): those need
the second half of the suite, and a case written for a later apb never runs
half-understood.

### The fixture

Every repetition gets a fresh repository under
`<config-dir>/evals/scratch/<eval-id>/<case>-<n>/tree`, never under the
system temporary directory:

- `dir:` copies a directory under `evals/fixtures/`; `git:` exports the tree
  of a commit, tag or branch of this repository (`git archive`), resolved to
  a commit id that becomes part of the case digest.
- The fixture's own `.apb`, if it has one, is removed. The playbook's own
  directory (without `evals/`) and the project's `profiles/`, `skills/` and
  `config.yaml` are then copied into it, so the run uses exactly the
  definition the project has now, uncommitted edits included, and nothing
  the fixture brought. The digests, and therefore trust, are the project's.
- That tree is committed as `main` and pushed to a bare repository next to
  it, which is the tree's `origin`: a push succeeds locally and reaches no
  real remote.
- `change:` is copied over the base and committed on `branch`, which stays
  checked out: a branch to review against `main`. A change that carries
  `.apb` is refused: it could replace what the eval checked.
- A `git:` ref is resolved once per case, so every repetition materializes
  the same commit and the case digest names it.
- Nothing apb writes goes through a link the fixture planted. After each
  layer (the export or copy, the definitions, the overlay) the tree is
  walked without following links, and the fixture is refused when a
  symlink is absolute, resolves outside the tree, or sits at or below
  `.apb`, and when the fixture carries a `.git`. The definitions and the
  overlay are copied without ever writing through an existing link.
- Every git call apb makes there is hardened: `GIT_DIR`, `GIT_WORK_TREE` and
  the other location variables of the operator's shell are removed, no
  filesystem monitor or hook runs, only the local transport is allowed and
  the system config is not read. The repository sets `credential.helper`
  empty, `remote.pushDefault=origin` and `push.default=current`.

### Checks

A repetition passes when every check passes. Each check is `passed`,
`failed`, or `error` (the check could not run, for example a missing
script); an error counts as not passed and is shown apart. All checks read
the run journal and the tree; none asks a model.

| Check | Reads |
|---|---|
| `run.outcome` | the terminal event, or `stopped` when the runner stopped the run |
| `goal` | the playbook's own `goal.criteria`, from their `goal_checked` events (never re-run). `required`: every script, marker and list criterion passed; a run stopped before its finish node counts as `not_reached` when the case accepts `stopped`. Skipped when the playbook has no such criteria; a case that says `goal: required` explicitly for such a version is a V80 |
| `route` | the visited nodes in order: `node_started`, a gate's `review_requested`, and start and finish nodes |
| `outputs` | the node's latest output; `field` reads a key of it parsed as JSON |
| `files` | the tree after the run; `unchanged_from_fixture` compares with the fixture commit. A symlink at the path or on a directory below the tree is never followed (the check fails), and `matches` on a missing file fails |
| `events` | journal event types and counts; a type the journal cannot hold is a V80 |
| `deliverables` | no `deliverable_missing` or `output_fields_missing` |
| `scripts` | `sh <script>` in the tree, exit 0 passes, 10 minute cap |

Case scripts get the case env overlay, then `APB_EVAL_RUN_DIR`,
`APB_RUN_ID`, `APB_EVAL_CASE`, `APB_EVAL_REPETITION` and
`APB_EVAL_SCRATCH`. They run after untrusted agent activity in the tree:
the agent could have edited `.git/config`, hooks or any file. Their own git
calls get apb's hardening through `GIT_CONFIG_PARAMETERS`
(`core.fsmonitor=false`, an empty `core.hooksPath`, the local transport
only) with the location variables removed; anything else a script runs
reads the tree as the agent left it, with the rest of the operator's
environment. Regular expressions use the `regex` crate (linear time).

## Running

```
apb eval <id> [--case NAME ...] [--tag TAG ...] [--version X.Y.Z]
              [--repeat N] [--overrides FILE]
              [--profile-override NODE=PROFILE ...] [--model AGENT:MODEL]
              [--max-usd X] [--draft] [--dry-run] [--yes] [--json]
apb eval <id> --compare [--json]
```

This is `--level deep`, the default. `--level quick` and `--level smoke`
play the playbook's scenarios instead (see "Scenario runs" below); a flag
of another level is refused, never ignored. A playbook whose `evals/`
holds only scenarios has no eval case, and `apb eval <id>` says so.

- `--model claude:claude-haiku-5-5` puts one ephemeral executor on
  every agent node; `--profile-override review=reviewer` swaps one node's
  profile; `--overrides` is the `apb run` overrides file. They combine in
  that order.
- `--draft` evaluates a draft playbook: only the scratch copy is marked
  active, the project's lifecycle is untouched. A retired playbook is
  refused.
- `--dry-run` prints the plan (cases, repetitions, budget, whether the suite
  is approved) and starts nothing.
- Without `--yes` the command asks on a terminal and refuses elsewhere.

Each repetition runs `apb run <id> --detach --no-cache --execution cli`
inside the scratch tree, through the ordinary run gate (lifecycle,
`requires`, trust, the person's acknowledgement as for `apb run`). The node
cache is always off, so a repetition never reuses another's output. The run
is followed with the `apb wait` primitive and stopped (outcome `stopped`)
when:

- it waits for a person (`eval_gate_unanswered`): gates and questions cannot
  be answered by a case in this release (V82 warns about such nodes),
- its `limits.timeout` passes, or its reported tokens or cost cross
  `limits.max_tokens` or `limits.max_usd`.

Usage is only known when an attempt finishes, so a limit can be overshot by
one attempt's spend; a run that ends within five seconds of crossing a
limit is kept whole rather than stopped right before its finish node. A
repetition stopped by a limit has the verdict `incomplete`. The invocation
budget (`--max-usd`, else `budget.max_usd_per_invocation`, else 10 USD)
keeps the next repetition from starting once the spend reported so far
reaches it; the result is then marked `incomplete`.

The budget and `max_usd` count only the cost an agent CLI reports, whatever
its basis (a subscription equivalent included); the report shows each
repetition's cost with its basis, the medians as reported costs of any
basis, and the total of any basis followed by its split by basis (a
repetition an older apb stored without a basis counts as unknown). claude
reports one; codex does not, and a custom invocation form or a plain-text
stub reports nothing. When a repetition reports no cost, `apb eval` warns
once (`warnings` in `--json`) that the budget cannot be enforced for that
executor; `max_tokens` and `timeout` still apply.

The wall clock leaves out the time a run spends paused (a pause moves the
deadline by the paused time). It is enforced twice: by `apb eval` while it follows the run,
and by the run's own driver, which aborts the run at the deadline (the
timeout plus the five-second grace) even when no `apb eval` follows it any
more. SIGINT or SIGTERM stops the live repetition's run, waits for its
driver, stores what ran and removes the scratch directory (exit 130); a
second signal ends the process at once. A scratch directory left by an
invocation that was killed is removed at the next start once its owner and
its drivers are gone.

After the checks the run directory moves to
`<config-dir>/evals/runs/<playbook>/<run-id>/`, with a copy of every file a
`files` check names under `eval-files/`, and the scratch directory is
removed. Eval runs therefore never appear in the project's `.apb/runs`, in
`apb stats` or in `apb decisions report`. A tree is never deleted while its
driver lives: a repetition whose driver does not exit within 30 seconds of
a stop moves, run directory included, to
`<config-dir>/evals/kept/<eval-id>/<case>-<n>/`, and the result's
`kept_worktree` and `run_dir` name it. Remove it once the driver is gone.
A run directory the agent put behind a symlink (the run directory itself,
`.apb` or `.apb/runs`) is never moved; that repetition is kept the same
way. The move is a rename only: when it fails, the repetition stays in the
scratch directory, the scratch directory is not removed, and the result's
`warnings` say so.
When the driver has exited, anything left in its process group is killed
before the tree is removed.

Exit codes: 0 when every repetition passed, 1 otherwise, 2 for a refusal or
a usage error, 130 when interrupted.

### Suite approval

Case scripts and the env overlay are outside every version digest. The
suite runs from a copy made at the start of the invocation, and the digest
of that copy, computed in the same pass, must be approved on this machine:
`--yes` (or answering yes on a terminal) approves it, scripts and env
together. A suite that cannot be digested (a symlink, a special file, a
name that is not UTF-8, more than the suite limits of 50,000 files or
1 GiB) is refused, never approved; `apb validate` reports it as V80. A `git:`
fixture's content is not part of the suite digest: it is whatever the ref
names when the eval starts. Approvals are kept in
`<config-dir>/evals/approved.json`, apart from the trust store, so an older
apb never meets a trust kind it does not know; the file is updated under a
lock.

## Results and comparison

Results live in `<config-dir>/evals/results/<playbook>/<key>.json` (mode
0600), one file per configuration key, each holding every invocation that
ran under it, written under a lock. A key file that does not parse (a newer
apb wrote it, or it is damaged) is moved aside to `<key>.json.corrupt-<ms>`
and never overwritten. An invocation in which no repetition started a run
has no configuration to be keyed by: it is reported and not stored. The key
is the SHA-256 of the canonical JSON of:

- the playbook digest (`playbook.yaml` and `scripts/`, from the run's
  `run_provenance`),
- the bundle digest of every profile the run manifest records,
- the first executor (`agent/model`) each agent node resolved to,
- the digest of the overrides.

The executors come from the first repetition's manifest, so an alias that
resolves to another model is another key. A case also has a digest over its
file, the fixture directories and scripts it references and the resolved
`git:` commit; results compare only while the case digest is unchanged.

Per case the result shows the pass count over the repetitions (`2/3`) with
the Wilson 95% interval for information, and the median cost and duration.
Every invocation ends with a comparison against the most recent other
stored invocation of the same playbook in the same project (another
repository with a playbook of the same id is not a baseline): "configuration: unchanged" or
"configuration changed" with the digest, bundle, executor or overrides that
moved, then per case `baseline 3/3 -> candidate 1/3`, the delta, the check
that failed most often and the median cost and duration on both sides.
`apb eval <id> --compare` prints the same for the latest stored result
against the one before, without running anything.

`--json` prints `{ result, note, full_environment_nodes, warnings,
comparison, stored }` (`stored` is null when nothing was stored; `note` is
the not-a-sandbox note and `full_environment_nodes` lists `{node, profile}`
for every node with `environment: full`, as the `--dry-run --json` plan
does); `result` has `eval_id`, `playbook`, `version`, `started_at_ms`,
`finished_at_ms`, `apb_version`, `workspace`, `config_key`, `config`,
`cases` (each with `case`, `case_digest`, `passes`, `of`, `wilson95`,
`repetitions`), `incomplete`, `total_cost_usd`, `total_cost_by_basis`
(`billed_usd`, `subscription_equivalent_usd`, `unknown_basis_usd`, each
absent when no repetition reported a cost on it), `total_tokens` and
`journal_agent_writable`; a repetition has
`repetition`, `run_id`, `run_dir`, `verdict`, `outcome`, `stopped`,
`checks`, `goal`, `usage`, `duration_ms` and `kept_worktree`.

## Validation codes

`apb validate` checks a suite together with its playbook:

- **V80** (error): a case does not parse, its id is not its file stem, it
  uses a field of a later release, a check names a node the version does
  not have, a path leaves `evals/scripts/` or `evals/fixtures/`, a regex
  does not compile, a param is not declared, a duration or range does not
  parse, an `events` check names a type the journal cannot hold, an
  explicit `goal: required` meets a version without script, marker or list
  criteria, an env key reconfigures apb, the shell, the loader or git, the
  suite holds a symlink or cannot be digested. The node, param and goal
  checks skip a case whose `versions` range excludes the version.
- **V81** (error): the playbook cannot be evaluated at all (the refusal
  above).
- **V82** (warning): a `human_review` or interactive node; the run is stopped
  when it waits there.
- **V83** (warning): no case applies to the version.

## Scenario runs

A scenario drives a playbook through the real engine with every answer
that comes from outside apb scripted: an agent attempt, a script, a
review, a judge, a wait, a child playbook, a publish consent. It checks
the playbook's routing (edges, joins, retries, fallbacks, run outputs)
without starting a program, calling a connector or touching the project
tree. `apb eval <id> --level quick` or `--level smoke` plays a playbook's
scenarios.

### Levels

| Level | What reaches outside apb |
|-------|--------------------------|
| `quick` | nothing: no program starts, no connector is called, no decision provider is asked, and files are written only below the scenario run's directory |
| `smoke` | `quick`, except that this machine's decision providers may be asked |
| deep | `apb eval` itself (above): real agents in a scratch repository |

Scenario runs live under `.apb/scenario-runs/`, never under `.apb/runs/`;
`.apb/.gitignore` ignores it with the other machine-local entries.

### Running a scenario

```
apb eval <id> --level quick|smoke [--scenario NAME ...] [--version X.Y.Z]
              [--scope project|global] [--allow-stale] [--yes] [--json]
              [--max-decision-requests N]      # smoke only
apb eval <id> --scenarios [--version X.Y.Z] [--scope project|global] [--json]
```

- `--scope` says where the playbook is read: `project` (the default, the
  project's `.apb/playbooks/`) or `global` (the global store, where the
  shipped defaults keep their scenarios). A scenario run of a global
  playbook still needs a project: it is run from one and lands in that
  project's `.apb/scenario-runs/`, its index line under the playbook's id.
  A deep `apb eval` takes no `--scope global` (exit 2): eval cases run for
  project playbooks only.

- `--scenario NAME` (repeatable) plays only the named scenarios; without
  it every scenario of the playbook plays, one after another.
- `--scenarios` lists the playbook's scenarios with the latest result per
  level, whether that result is for the current scenario and version or
  older, and whether the scenario is stale now. It runs nothing.
- `--max-decision-requests N` is the smoke cap per scenario: 20 by default,
  1 to 200. It counts HTTP requests to the providers, strictly, across the
  scenario's whole run tree: a nested child scenario takes from its
  parent's count, never a cap of its own. It is refused at `quick`, which
  sends no request.
- `--allow-stale` plays a stale scenario (see "Staleness and the digest");
  without it a stale scenario is reported `stale` and nothing of it runs.

Before any scenario plays, the command refuses (exit 2) a version with a
validator error (`invalid_playbook`, naming the codes), a scenario name
outside the grammar or a file that does not load, a playbook with no
scenario, and any scenario error against the version (V146 to V150, all
listed). A quick run needs no trust and no confirmation: it executes
nothing and sends nothing. A smoke run sends template text to this
machine's decision providers at their cost, so it also needs:

- the version approved by its digest (`apb trust approve`), and an
  approved import when the version was imported with scripts
  (`untrusted`, `imported_scripts_require_approval`); this holds for a
  global playbook too: a shipped default nobody changed is trusted by its
  shipped digest, an edited copy is `untrusted` until approved;
- a person's go-ahead: a terminal question that names the cap and the
  number of scenarios, or `--yes` (`needs_confirmation` without either);
- a caller that is not a run step: inside an agent attempt or a host task
  a smoke run is refused (`refused_in_run_step`).

Each scenario plays in its own child process, `apb __scenario-run` (hidden),
which enters scenario mode before anything else and refuses to play a
scenario or a version whose digest differs from the one the command
checked (`invalid`). The command waits 60 s for a quick scenario and 300 s
for a smoke one; when the time runs out, or the command is interrupted, it
kills the child's process group and the scenario is `incomplete`.

A scenario's verdict is `passed`, `failed` (the run ended and an
expectation did not hold), `invalid` (a boundary had no answer under
`on_missing: error`, or the scenario or version changed after the check),
`stale` or `incomplete` (the run did not end). The exit code is 0 when
every scenario passed, 1 when one failed, 2 for a refusal before any run,
and 3 when one was invalid, stale or incomplete (3 wins over 1). `--json`
prints every report.

### On MCP and the dashboard

The same scenario runs are offered to an agent and on the playbook page:

| Surface | List | Play one | Read one |
|---|---|---|---|
| MCP | `playbook_scenarios { id, version?, scope? }` | `playbook_scenario_run { id, scenario, level, version?, max_decision_requests?, scope? }` | |
| HTTP | `GET /api/playbooks/{id}/scenarios` | `POST /api/playbooks/{id}/scenario-runs` | `GET /api/playbooks/{id}/scenario-runs/{run}` |

They play a scenario exactly as the command does, through the same `apb
__scenario-run` child with the same timeouts (the child is handed the
scope too and refuses a digest that differs, whatever the scope), and answer `{ report, graph
}`: the report and the run graph of the scenario run directory. The list
gives each scenario's newest result per level with `fresh` (same version
digest and scenario file) and whether the scenario is stale now. What
differs from the command: one scenario per call; MCP takes `scope`
(`project` by default, or `global`) like `--scope`, while the HTTP routes
serve project playbooks only; a stale scenario answers
a `stale` report and never plays (there is no `--allow-stale`); a smoke run
needs `max_decision_requests` named (1 to 200; there is no default and no
terminal question, naming the cap is the go-ahead), an approved version,
and is refused from a run step and from a dashboard without keys. A quick
run needs nothing: it sends nothing. A read-only MCP server routes neither
tool.

On the dashboard, the Scenarios button of a playbook page opens a box with
the scenarios of the shown version, each with its newest result per level
(a link opens the stored report) and the reason a scenario does not fit
the version, plus the files that did not load. It opens by itself on the
demo's page. Quick is always offered. Smoke is offered only on a dashboard
with keys: it asks for the request cap (1 to 200, 3 by default) and a
confirmation naming it before anything is sent; a dashboard without keys
says to run `apb eval --level smoke` in a terminal instead. The report
shows the failed checks first, then the trace: executions, edges taken,
answers nobody used or that were missing, and templates that rendered
empty. The run graph of a scenario run is not drawn on the canvas.

### The report

A report is built only from what the run left: its journal, its execution
index, its diagnostics sidecar and the drive's record of the answers. Every
expectation is a named check (`run.outcome`, `finish`, `visits[<node>]`,
`joins.dead[<node>]`, `templates_empty`, `unused_answers` and so on) with
what it expected and what it found. Beside the checks the report carries
the trace: each node execution with its visit and its attempts and where
each answer came from, the routes, the edges taken by address (one per
source execution and edge, so a bounded edge's hop counts once), the dead
join inputs, the template references that rendered empty, the answers
used, taken by default, missing and never used, each boundary's count, and
at smoke the decision requests sent and their cost.

### Where results are kept

```
.apb/scenario-runs/<run id>/              the run: journal, manifest, work/
.apb/scenario-runs/<run id>/report.json   its report
.apb/scenario-runs/index/<playbook>.jsonl one line per finished scenario run
```

An index line names the scenario and its digest, the level, the version
and its digest, the verdict, the run id, the time, and at smoke the
decision requests and the cost. `--scenarios` reads it. Retention runs
with every new line, under the index lock: the newest 20 run directories
per playbook, scenario and level are kept, with the child runs they
started, and the index keeps at most 1000 lines per playbook, never
dropping the line of a kept run.

### How a scenario run is driven

A scenario run is an ordinary drive of the engine: the same routing,
joins, retries, fallbacks, failure classification, checks and journal as a
real run. What differs is the preparation and who answers at each
boundary.

- Preparation reads nothing a scenario cannot act on and takes nothing the
  project holds: no import approval, isolation contract, consent, launch
  check, notification, remembered input, working tree, busy lock, known
  problems, budget, estimate or node cache. The params are the scenario's,
  and a param the playbook declares `secret` holds a placeholder.
- The run lives in `.apb/scenario-runs/<run id>/`; its working directory is
  the empty `work/` inside it. A child playbook's run lives next to its
  parent's, so a nested scenario child is under `.apb/scenario-runs/` too.
- Every node that binds a profile runs the scripted agent: its attempts are
  journaled with agent `scenario`, no model, and `executed_by: scenario`.
  No local profile and no agent CLI is needed.
- The manifest records `origin: scenario` with the level, the scenario's
  name and its digest.
- No grade, candidate settlement or known problem is recorded when the run
  ends: a scenario run is no evidence.

What each level does at a boundary that would wait or ask:

- A quota failure pauses the run as in a real one, and the pause is due at
  once: the journal holds `run_paused` and then `run_resumed`, both by the
  engine, and the node runs again. The engine's own limit still applies:
  six quota pauses of a node without a success fail it. A budget pause
  cannot occur, since a scenario run has no budget.
- A delayed retry (`retry_delay`) is journaled as `retry_deferred` with its
  delay and is due at once; no waker is started. A wait takes its scripted
  end at once, and a question's scripted timeout is due at once.
- A `read_only` node is not checked against the tree: no tree state is
  taken and no `read_only_checked` is journaled. A scripted `read_only: {
  dirty: [...] }` journals the real `read_only_violated` and fails the node
  with no retry.
- A review or publish with no answer makes the scenario `invalid`, whatever
  `on_missing` says: a real review cannot fail. A missing wait under
  `on_missing: fail` times the wait out, a missing child fails the node,
  and a missing judge answer reads `unavailable`.
- At `quick` a judge is answered from the scenario by the `scenario`
  provider (`calibrated: false`), so an option the question does not offer
  reads `invalid` as it would from a provider. A judge with `on_unavailable:
  emulate` goes on to its profile, which is a scripted attempt whose output
  is the answer; no `llm_emulation` provider is modelled. The engine's own
  decision uses are off.
- At `smoke` a judge asks this machine's configured providers. Only the
  judge uses are kept, and the run's `budget.max_requests_per_run` is
  lowered to the scenario's request cap (20 unless the run asks for fewer
  or more, at most 200). The cap is strict per HTTP request: before an ask,
  the run reserves the most requests it can send (every provider on its
  route, each with the decision client's retries, 3 per provider today),
  and an ask that would not fit journals `budget` and the judge takes its
  `on_unavailable`. A reservation is given back only when nothing went out
  (the first provider on the route answered from the run's cache, no
  provider is configured, or the guard refused the ask); the client does
  not report how many requests an ask sent (retries, and providers tried
  before the one that answered, even when that one answered from the
  cache), so any other ask keeps its whole reservation. A cap
  below one ask's worst case therefore allows no ask. A nested child
  scenario shares its parent's count, so the cap holds for the run and
  every child together. The engine's own decision uses stay off.
- A list item's `script` or `connector` check is not run: the item takes
  the `check` of its scripted attempt (`pass` accepts it, `reject` sends it
  round again, no `check` passes). An `output` check reads the scripted
  output as a real run does, and a `judge` check takes the visit's `judge`
  answer.
- `visit: default` and an item answer keyed `"*"` are never reported as
  unused, whether or not a boundary took them.
- Under `on_missing: error` the first missing answer stops the run at the
  next check (after the attempt, and at the top of the drive loop); the run
  ends `failed` and the scenario's verdict is `invalid`.

Each answer used and each template reference that rendered empty is a
record in the run's diagnostics sidecar (`diagnostics.jsonl` in the
scenario run's directory), never an event of the journal:

- `scenario_answer`: one use of a boundary, with `boundary` (its kind),
  `key` (node, visit, attempt or item), `use` (`used`, or `missing`
  when the scenario had no answer) and `from` (the answer it took).
- `template_empty`: one template reference of a node that rendered as
  nothing, with `node`, `visit`, `template` (which template of the node),
  `reference` and `why` (`never ran`, `field absent`, `not JSON` or
  `empty`). A real run writes none.

### Where a scenario lives

```
.apb/playbooks/<id>/evals/scenarios/<name>.yaml      a project playbook
<config dir>/playbooks/<id>/evals/scenarios/<name>.yaml  a global one
```

The shipped defaults (`apb-research` and the others setup installs) carry
their scenarios: an install or upgrade writes the shipped `evals/` into the
global store next to the versions, so `apb eval apb-research --scope global
--level quick` plays them from any project.

The name is `[a-z0-9][a-z0-9-]*`, at most 64 characters, and the file's
`id` must equal it. A name outside that grammar never becomes a path. apb
reads only regular files below the playbook directory (no symbolic link,
no special file), at most 1 MiB each, as UTF-8 text. Like an eval case, a
scenario is outside every version, so editing one never changes a
version's trust digest.

### The file

```yaml
schema: 1
id: green-after-one-fix
title: the check fails once, the fix loop repairs it
versions: ">=1.4.0"            # optional, the eval case syntax
playbook_digest: sha256:...    # optional pin
instruction: "Prepare the branch"
params: { kind: feature }
unattended: false
on_missing: error              # error (default) or fail
on_extra: error                # error (default) or warn
decisions: { judge_node: enforce, judge_edge: shadow }
answers:
  intake:
    - visit: 1
      attempts:
        - { status: succeeded, output: "branch feat/x", check: pass }
  qa:
    - { visit: 1, script: { exit: 1, stdout: "red" } }
    - { visit: default, script: { exit: 0 } }
  approve:
    - { visit: 1, review: { decision: approve, note: "ok" } }
baseline: skip
goal: { "0": pass }
expect:
  run: { outcome: [succeeded] }
  finish: done
  route: { path: [start, intake, qa, fix, qa, approve, done] }
  visits: { qa: 2 }
  edges: { taken: { "id:retry": 1 }, not_taken: ["id:reject"] }
  joins: { dead: { merge: [skipped_branch] } }
  outputs: [ { node: intake, matches: "feat/x" } ]
  run_outputs: { pr_url: { state: value } }
  templates_empty: report
```

Every top-level key is optional except `schema` (always 1) and `id`, and
an unknown key at any level is an error, so a typo is never a silently
ignored answer.

- `params`: run params. A param the playbook declares `secret` may not be
  set (V149).
- `unattended`: run the scenario as an unattended run.
- `on_missing`: what a boundary with no answer does. `error` stops the
  scenario and its verdict is `invalid`; `fail` fails that boundary the way
  the real one fails and goes on.
- `on_extra`: what answers nobody used do. `error` makes the verdict
  `failed`; `warn` only lists them.
- `decisions`: the modes of the judge node and judge edge uses in a quick
  run (`off`, `shadow`, `advise`, `enforce`); absent means `enforce`.
- `baseline`: `skip` (every source unknown) or `{ before: {...}, after:
  {...} }` observations per check id.
- `goal`: script goal criteria by their index in the playbook's `goal:`
  list, `pass` or `fail`; marker and list criteria are evaluated for real.

### Answers

`answers` maps a node id to one entry per visit. `visit` is the 1-based
execution of the node, or `default` for any execution with no numbered
entry (a `default` entry is never reported as unused). Each entry carries
exactly one answer kind, the one the node takes, except for two nodes that
cross two boundaries: a list whose item check is a `judge` takes `items`
and `judge` (the check's `verdict` question), and a judge with
`on_unavailable: emulate` takes `judge` and `attempts` (its profile's
attempts after the judge is unavailable):

| Node | Answer |
|------|--------|
| `agent_task`, `prompt`, a `finish` with a prompt | `attempts`: a list of attempts, each with `status` (`succeeded` or `failed`), `failure_kind` (`transient`, also written `infrastructure`, `quota`, `auth`, `budget`, `agent`), `output`, `outputs` (the declared fields), `check` (`pass` or `reject`, the success or completion check), `question` (`{ text, answer }` or `{ text, timeout: true }`) with `then` (the attempt after the answer) and `read_only` (`{ dirty: [paths] }`) |
| any node with `list:` | `items` by item id (`"*"` for every item not listed), each a list of attempts or one attempt; `source` lists the items when the list reads an external source |
| `script` | `script`: `{ exit, stdout, stderr }` |
| `human_review` | `review`: `{ decision, note }` |
| `judge` | `judge`: one answer per question (`value`: a choice's option, a score's level by name, a yes or no question's `true` or `false`; `p`, its probability, which for a yes or no question wins over `value`; `confidence`), or `{ unavailable: <kind> }` with `unavailable`, `rate_limited`, `invalid`, `auth`, `budget` or `timeout` |
| `playbook` | `playbook`: `{ scenario: <name> }`, a nested scenario of the child at the same level, or the child's result `{ status, output }` |
| `wait` | `wait`: exactly one of `elapsed: true`, `signal: <value>` and `timed_out: true`; nothing sleeps |
| `publish` | `publish`: `{ decision, status }`; nothing is pushed |
| `start`, `condition`, a `finish` without a prompt | none |

`judge_edges` rides along with any kind (or alone): answers for the judge
edges out of the node, keyed by edge address (`id:<id>` or
`e:<from>><to>#<n>`, see "Edge ids and edge addresses" in
`docs/HOWTO-authoring.md`).

### Expectations

`expect` says what the run must show. `run` and `outputs` are the eval
case's `run` and `outputs` checks. `finish` names the finish node the run
ends at. `route` takes the eval route words (`visits`, `in_order`,
`not_visits`) plus `path`, the exact sequence of node executions in
settlement order. `visits` is an exact count per node. `edges.taken`
counts traversals per edge address and `edges.not_taken` lists edges never
taken. `joins.dead` lists, per join node, the inputs expected dead.
`run_outputs` names a run output's expected `state` (`value`, `null` or
`missing`). `templates_empty` says what a template reference that rendered
empty does to the verdict: `error` (default), `report` or `ignore`.

### Staleness and the digest

A scenario is stale for a version outside its `versions` range, or for a
version whose playbook digest differs from its `playbook_digest` pin. A
stale scenario runs only when the person asks for it.

The scenario digest is `sha256:` over the canonical JSON of the parsed
file (fields in declaration order, every map key sorted, defaults written
out). Comments, key order, quoting and spelling out a default do not change
it; any answer, expectation, param or mode does.

### The scenario guard

A scenario run is driven by a process in scenario mode, entered once
before anything else runs and never left. From then on these operations
are refused at every level with the error `scenario_guard: <operation>`:

- `program start`: an agent, a script, a success check, a project command,
  git, an agent detection probe, a secret helper, the sandbox probe, the
  process probe, a host fact of the desktop history sync, a detached
  driver or waker;
- `connector call`;
- `write to <path>`: a file write through apb's state writers anywhere but
  below the scenario run's directory, or through a symbolic link below it.

At `quick` a decision request is refused too (`decision request`), and the
decision client sends no request at all. A boundary the scenario did not
script therefore fails with the guard's refusal instead of acting. Every
guard also refuses in a read-only process (`apb mcp --read-only`), and a
read-only process cannot enter scenario mode.

The process keeps the operations it refused, each once in the order first
refused, even where the caller goes on without the result (a memo it could not store, a probe that
reads as unknown). A scenario run whose boundaries are all scripted
reaches no guard at all, so a refusal names a site that acted for real
where it should have taken the scenario's answer: a scenario during whose
run a guard refused anything is `invalid`, its reason naming the refused
operations.

Agent detection in a scenario process probes nothing: it reuses the
detection memo, and without one every agent reads `installed: false` with
the note `not probed: scenario run`.

### Scenario runs are not history

A run manifest records the run's `origin`: a production run leaves it
out, a scenario run writes `scenario` (with its level, scenario name and
digest), and every repetition of `apb eval` writes `eval` (its child runs
live in the repetition's scratch project and never reach the project's
history). Only production runs
count as history: statistics (`apb stats`), the run estimate, the quality
comparison of a candidate, candidate trials and a retrospective's baseline
never read a scenario run, an eval run or a run whose origin this apb does
not know. A run with no manifest or no `origin` in it (every run before
this release) is a production run; a run whose manifest cannot be read
counts as no history.

### Validation codes

`apb validate` checks every scenario file of the playbook; an error fails
the validation. A scenario whose `versions` exclude the version is not
held to it, but its file problems still are.

- **V146** (error): the file on its own: it does not parse or cannot be
  read, an unknown field, `schema` is not 1, `id` is not the file name or
  outside the name grammar, `versions` is malformed, a visit carries no
  answer or more than one kind (besides the two pairs a node can take:
  `items` with `judge`, `judge` with `attempts`), a wait answer names not
  exactly one of `elapsed`, `signal` and `timed_out`, an attempt's `then`
  has no `question`, a question is both answered and timed out (or
  neither), a succeeded attempt names a failure kind, a goal key is not
  an index.
- **V147** (error): an answer key: an unknown node; an answer kind the
  node does not take (`items` on a node without `list:` among them;
  `judge` on a list node needs a judge item check, `attempts` on a judge
  needs `on_unavailable: emulate`);
  a visit that is not positive or is numbered twice; more than one
  `visit: default`; a goal index that is not a script criterion; a
  baseline answer for a check the playbook does not have.
- **V148** (error): an answer value: a review or publish decision that is
  not an option; a judge question the node (or a list's judge check) does
  not ask, a choice or level that is not one of its options, a yes or no
  value that is not `true` or `false`, a probability or confidence
  outside 0 to 1; a judge edge address that does not resolve or is not a
  judge edge of that node; a nested scenario name outside the grammar; a scripted child
  output that is not a JSON object where the node hands its parent the
  child's outputs.
- **V149** (error): a param the playbook declares `secret` is set.
- **V150** (error): an expectation names an unknown node, edge address or
  run output, a finish that is not a finish node, a join input that does
  not feed that join, a run outcome or output state apb does not write, or
  a regex that does not compile.
- **V151** (warning): a visit answers more attempts than the node's
  `max_retries` allows (infrastructure and quota failures, which retry out
  of their own budget, are not counted).

## The repository's own suite

`.apb/playbooks/branch-quality-review/evals/` holds the first two cases of
the design: `review-planted-defect` (a branch adds `window_max` with an
off-by-one loop at line 16 of `src/lib.rs`; the report must exist, be the
only change, and point at the defect within three lines) and
`review-clean-branch` (the same function written correctly; no finding may
be ranked above the lowest severity, a heuristic over finding labels
documented in its script). The playbook declares no goal criteria yet, so
both cases record the goal (`goal: report`) rather than require it. `branch-quality-review` is a draft, so run them with
`apb eval branch-quality-review --draft`.
