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

This page describes 0.24.0, the first half of the design. Not in this
release: mock connectors (and so any playbook that binds a connector),
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
  eval, and mocks come later),
- a `playbook` node (the child's effects are not visible to the case).

Such a playbook is refused before anything is created
(`eval_refused_effects`, exit 2), and `apb validate` reports it as V81.
There is no case-level switch that lifts the refusal. Use the case `env` to
cut the logged-in CLIs you know about (for example `GH_CONFIG_DIR` pointing
into the scratch directory and an empty `GH_TOKEN`).

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
```

The registry only lists `major.minor.patch` directories as versions, so
`evals/` is never a version, and editing a case never changes a version's
trust digest. One suite applies across versions; a case can narrow itself
with `versions`. Only project playbooks can be evaluated in this release.

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
env:                              # overlay for the run's processes
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
- The playbook's own directory (without `evals/`) and the project's
  `profiles/`, `skills/` and `config.yaml` are copied into its `.apb`, so
  the run uses exactly the definition the project has now, uncommitted
  edits included. The digests, and therefore trust, are the project's.
- That tree is committed as `main` and pushed to a bare repository next to
  it, which is the tree's `origin`: a push succeeds locally and reaches no
  real remote.
- `change:` is copied over the base and committed on `branch`, which stays
  checked out: a branch to review against `main`.

### Checks

A repetition passes when every check passes. Each check is `passed`,
`failed`, or `error` (the check could not run, for example a missing
script); an error counts as not passed and is shown apart. All checks read
the run journal and the tree; none asks a model.

| Check | Reads |
|---|---|
| `run.outcome` | the terminal event, or `stopped` when the runner stopped the run |
| `goal` | the playbook's own `goal.criteria`, from their `goal_checked` events (never re-run). `required`: every script and marker criterion passed; a run stopped before its finish node counts as `not_reached` when the case accepts `stopped`. Skipped when the playbook has no such criteria |
| `route` | the visited nodes in order: `node_started`, a gate's `review_requested`, and start and finish nodes |
| `outputs` | the node's latest output; `field` reads a key of it parsed as JSON |
| `files` | the tree after the run; `unchanged_from_fixture` compares with the fixture commit |
| `events` | journal event types and counts |
| `deliverables` | no `deliverable_missing` or `output_fields_missing` |
| `scripts` | `sh <script>` in the tree, exit 0 passes, 10 minute cap |

Case scripts get `APB_EVAL_RUN_DIR`, `APB_RUN_ID`, `APB_EVAL_CASE`,
`APB_EVAL_REPETITION` and `APB_EVAL_SCRATCH`. Regular expressions use the
`regex` crate (linear time).

## Running

```
apb eval <id> [--case NAME ...] [--tag TAG ...] [--version X.Y.Z]
              [--repeat N] [--overrides FILE]
              [--profile-override NODE=PROFILE ...] [--model AGENT:MODEL]
              [--max-usd X] [--draft] [--dry-run] [--yes] [--json]
apb eval <id> --compare [--json]
```

- `--model claude:claude-haiku-4-5-20251001` puts one ephemeral executor on
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

After the checks the run directory moves to
`<config-dir>/evals/runs/<playbook>/<run-id>/`, with a copy of every file a
`files` check names under `eval-files/`, and the scratch directory is
removed. Eval runs therefore never appear in the project's `.apb/runs`, in
`apb stats` or in `apb decisions report`. A repetition whose driver does not
exit after a stop keeps its tree and says where.

Exit codes: 0 when every repetition passed, 1 otherwise, 2 for a refusal or
a usage error.

### Suite approval

Case scripts are code outside every version digest. The suite runs from a
copy made at the start of the invocation, and the digest of that copy must
be approved on this machine: `--yes` (or answering yes on a terminal)
approves it. Approvals are kept in `<config-dir>/evals/approved.json`, apart
from the trust store, so an older apb never meets a trust kind it does not
know.

## Results and comparison

Results live in `<config-dir>/evals/<playbook>/<key>.json` (mode 0600), one
file per configuration key, each holding every invocation that ran under
it. The key is the SHA-256 of the canonical JSON of:

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
stored invocation of the same playbook: "configuration: unchanged" or
"configuration changed" with the digest, bundle, executor or overrides that
moved, then per case `baseline 3/3 -> candidate 1/3`, the delta, the check
that failed most often and the median cost and duration on both sides.
`apb eval <id> --compare` prints the same for the latest stored result
against the one before, without running anything.

`--json` prints `{ result, comparison, stored }`; `result` has
`eval_id`, `playbook`, `version`, `started_at_ms`, `finished_at_ms`,
`apb_version`, `workspace`, `config_key`, `config`, `cases` (each with
`case`, `case_digest`, `passes`, `of`, `wilson95`, `repetitions`),
`incomplete`, `total_cost_usd` and `total_tokens`; a repetition has
`repetition`, `run_id`, `run_dir`, `verdict`, `outcome`, `stopped`,
`checks`, `goal`, `usage`, `duration_ms` and `kept_worktree`.

## Validation codes

`apb validate` checks a suite together with its playbook:

- **V80** (error): a case does not parse, its id is not its file stem, it
  uses a field of a later release, a check names a node the version does
  not have, a path leaves `evals/scripts/` or `evals/fixtures/`, a regex
  does not compile, a param is not declared, a duration or range does not
  parse.
- **V81** (error): the playbook cannot be evaluated at all (the refusal
  above).
- **V82** (warning): a `human_review` or interactive node; the run is stopped
  when it waits there.
- **V83** (warning): no case applies to the version.

## The repository's own suite

`.apb/playbooks/branch-quality-review/evals/` holds the first two cases of
the design: `review-planted-defect` (a branch adds `window_max` with an
off-by-one loop at line 16 of `src/lib.rs`; the report must exist, be the
only change, and point at the defect within three lines) and
`review-clean-branch` (the same function written correctly; no finding may
be ranked above the lowest severity, a text heuristic documented in its
script). `branch-quality-review` is a draft, so run them with
`apb eval branch-quality-review --draft`.
