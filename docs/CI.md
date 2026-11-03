# Running apb in CI

apb has no scheduler of its own: a run starts when a person, an agent or a
program calls it. CI is the usual program. This page shows how to start a
playbook from a CI job, wait for it without polling, read the verdict from the
exit code, and keep the trust model intact when nobody is at the keyboard.

The example used throughout is `examples/playbooks/ci-failure-triage.yaml`: a
read-only triage that turns a failed build's log into a three-field summary
(`failing_step`, `likely_cause`, `next_action`).

## The two commands

```sh
apb run <playbook> --detach [--unattended] [--version X.Y.Z] [--param k=v]... [--instruction "..."] [--worktree DIR]
apb wait <run_id> [--timeout SECS] [--follow-auto]
```

`apb run --detach` starts the run in a detached background process, prints
`run started: <run_id>` and returns at once. `apb wait` blocks until the run
finishes, needs input or stops, costs nothing while it blocks, and reports the
verdict in its exit code:

| `apb wait` exit | Meaning | Typical CI handling |
|---|---|---|
| 0 | the run succeeded | pass |
| 1 | the run failed or was aborted | fail the job |
| 2 | error (unknown run id, unreadable run) | fail the job |
| 3 | the run needs input: a question, a `human_review` gate, a supervisor decision, or host tasks of a host-mode run (`apb tasks <run>` lists them); an unattended run fails at such a gate or question instead | fail, or hand the run id to a person (see below) |
| 4 | the run is paused, its driver is gone, or it is released while a deferred retry waits | fail; `apb resume <run_id>` continues it (`--retry-now` starts a waiting retry at once) |
| 5 | `--timeout` elapsed while the run was still going | fail, and `apb stop <run_id>` |

### Following automatic progress: `--follow-auto`

The default `apb wait` returns as soon as anything stops the run itself: a
pause with `until` exits 4 even though the run resumes on its own, and a
sub-playbook run waiting for a review keeps the wait blocked because only the
run's own gates count. `apb wait <run_id> --follow-auto` is the form for CI:
it keeps waiting while the run and its live sub-playbook runs go on by
themselves (a timer `wait` node, a question that gets its default answer
under a live driver, a pause with `until` and nothing else open, up to one
minute past `until`; a new pause starts that minute again), and returns on
one verdict over all of them. A node whose retry waits out its `retry_delay`
under a live driver goes on by itself. A run released while such a retry
waits (its driver exited and a waker resumes it at the retry's wake time) is
followed like a pause with `until`: up to one minute past the wake time of
that release, and a new release starts that minute again.

| `--follow-auto` exit | Meaning |
|---|---|
| 0 or 1 | the run finished, as for the default wait |
| 3 | something needs a person or a host, in the run or a live child: a question without a default, a review, a publish gate, host tasks, a supervisor decision, a webhook `wait`; also a paused run that has such a gate open |
| 4 | the run or a child is paused without `until`, by its budget, or its driver is gone, and no gate is open; or `until` (a release's wake time) passed by a minute and the run did not resume |
| 5 | `--timeout` elapsed |

Both forms print the next actions after the verdict, each with its reason, the
`apb` command that takes it (quoted for a POSIX shell; `<...>` marks a value
you supply, and such a line does not run as it is) and what happens without
anyone. An answer and a decision name the request they are for
(`apb answer --request=question:12 ...`, `apb review --request=review:7 ...`),
so a command printed for an earlier round of a looping gate is refused
instead of deciding the next one. The default `apb wait` keeps its exit codes.

Without `--detach`, `apb run` drives the run in the foreground and exits 0 on
success, 1 on any other outcome, 2 on an error before the run starts. A
foreground run (and a foreground `apb resume`) whose driver releases the run
while a long deferred retry waits prints `waiting to retry <node> at <time>`
and exits 4, like `apb wait`; the waker it leaves resumes the run then. The
detached form is preferred in CI because `apb wait` also distinguishes "needs
input" and "timed out" from "failed", and a wait can be bounded with `--timeout`
without stopping the run itself.

`apb runs <run_id>` prints the node statuses, the failure reason, the token
usage and cost the agents reported, the goal criteria results and the
execution mode (`execution: cli`, `execution: host (argument, client ...)`,
and `host fallback: <nodes>` for steps the journal records as
`execution_fallback`); `apb runs <run_id> --json` prints the same object as
the MCP `run_status`, for a step that parses it. The `apb runs` table gets a
mode column only when some run has an execution block. The journal
itself is `.apb/runs/<run_id>/events.jsonl`. Upload `.apb/runs/<run_id>/` as a
build artifact to keep the evidence.

## Configuration on the runner

- **Config directory.** apb reads its global configuration (`config.yaml`,
  global profiles and connectors, `decisions.yaml`, the trust store) from
  `APB_CONFIG_DIR`, else `$XDG_CONFIG_HOME/apb`, else `~/.config/apb`. On a
  runner, point `APB_CONFIG_DIR` at a directory inside the job workspace so each
  job starts from a known, empty state. With `CI` set (every common CI system
  sets it), apb does not register the checkout in the workspace registry.
- **Profiles and playbooks.** Commit what the job runs: the playbook versions
  under `.apb/playbooks/` and the project-scope profiles under `.apb/profiles/`.
  A playbook that uses global profiles or connectors needs them written into
  `APB_CONFIG_DIR` by the job.
- **Agent credentials.** apb holds no model credentials. Each agent CLI reads
  its own (an API key environment variable or a login file the agent
  documents). Pass them as CI secrets to the step that runs apb; apb removes
  connector secrets from the agent's environment, but the agent's own key is
  the agent's business.
- **Connector accounts.** A connector account config may reference
  `{{env.NAME}}`; set `NAME` from a CI secret. Connector and account approvals
  live in the trust store under `APB_CONFIG_DIR` (`apb connector approve`).
- **A bot identity for commits and pushes.** When a scheduled playbook commits,
  pushes, comments or opens pull requests, let it act as a dedicated bot
  account with a short-lived token scoped to the one repository and the
  permissions the playbook needs (for example a CI job token or an app
  installation token), never a person's account or personal token. History
  then shows which changes a machine made, and a token that leaks from a log
  or an artifact expires on its own instead of opening the person's whole
  account.
- **The agent binary.** Install the agent CLI the profile binds (and pin its
  version). `apb doctor` in the job prints what apb detects and what is missing.

## Trust in CI

A person typing `apb run` is the confirmation for a playbook whose digest is not
approved: the CLI start acknowledges it for that person. In CI the "person" is
the pipeline, so the confirmation moves to code review:

- **Pin the playbook version.** Run `apb run <id> --version X.Y.Z`, not the
  `current` marker, so a new version never runs without a change to the CI
  file.
- **Review `.apb/` changes in pull requests.** A playbook, a profile, its SOUL,
  its skills and the scripts under a version's `scripts/` are all executable
  instructions. Require review for changes under `.apb/` (for example with a
  code owners rule), exactly as for the CI configuration itself.
- **Do not run playbooks from untrusted pull requests** with secrets available.
  The same rule applies as for any CI step that executes repository code.
- **Keep irreversible steps behind a person.** A playbook whose effects include
  `irreversible` (merge, deploy, publish) should stop at a `human_review` gate;
  in CI that surfaces as `apb wait` exit 3. A CI trigger is not a person's
  approval: a headless `apb run` of such a playbook is refused
  (`irreversible_requires_confirmation`, printing a `consent_nonce`) unless
  the step says `--confirm-irreversible=<consent_nonce>`, which is the workflow
  author's standing consent for that version of the tree and is recorded in
  the run manifest as `consent: { by: cli_flag }`. A new version of the
  playbook (another digest, another irreversible step) prints a new nonce and
  is refused until the workflow is updated; a bare `--confirm-irreversible`
  still works for one release with a deprecation warning.
- **Evals are opt-in there too.** `apb eval` is a paid run per repetition and
  asks before it starts; a job refuses without `--yes`, which also approves the
  suite's scripts on that runner (`<config>/evals/approved.json`). Pin the
  version with `--version`, set `--max-usd`, and review `evals/` changes like
  any other executable instruction (docs/EVALS.md).

## Working trees

`--worktree DIR` makes the run's agent and script nodes work in `DIR` (absolute,
or relative to the project) and takes the busy lock on that tree instead of the
project's, so two runs over two checkouts do not wait on each other. In CI the
checkout is usually the working tree already; use `--worktree` when the job
prepares a separate `git worktree` for the run, or when the playbook declares a
`worktree` that should be overridden.

## Gates in CI

A run parked on a `human_review` gate or a question keeps waiting after the job
ends; its state is on the runner's disk. On a persistent runner (or a
self-hosted machine), print the run id and let a person decide with
`apb review <run_id> <node> --decision <option>` or `apb answer <run_id> <text>`,
then continue with `apb wait <run_id> --follow-auto`, the form for CI. On an
ephemeral runner nobody will come back to the run: start it unattended.

## Unattended runs

`apb run --unattended` (MCP `playbook_run { unattended: true }`, the HTTP
start body `{ "unattended": true }`) starts a run that nobody attends. Every
wait that would need a person and declares no outcome for this case ends
the run `failed` at once, with a run error that names the node and starts
with `hitl_required:`, for example ``hitl_required: human_review `review`
has no unattended outcome``. No failure route is taken: neither the
node's failure edge nor `defaults.on_failure` runs, because a route could
lead into another wait. The job sees the failure right away (`apb run`
exits 1, `apb wait` exits 1) instead of a run parked on a disk that is about
to disappear.

The mode is fixed when the run starts. It is recorded in the run's
manifest, which is written once, so a resume cannot change it: a run
resumed by the pause waker, by `apb resume --all` or by a person stays
unattended, even when an older apb rewrote the run's `run.yaml` meanwhile.
A sub-playbook run inherits it, and a child that fails `hitl_required` (or
`budget_exhausted`) fails its parent with the same code and ``via child
<run>`` in the reason, again without the parent's failure route.

Who may start a run unattended: a person, a CI job, an agent session that
is not a run step, and a step of a run that is itself unattended. A run step
of an attended run (or a host task) that asks for it is refused before any
run file exists (`unattended_unsupported`, requirement
`unattended_caller`): the person that run waits on would otherwise be
taken out of the gates of the run it starts. apb recognizes a run step by
its environment (`APB_RUN_ID`, `APB_RUN_DIR`, `APB_HOST_TASK`), as for
every other run-step refusal, so this rule keeps a cooperating step from
choosing unattended by mistake; it is not a boundary against a step that
changes its own environment. On the dashboard it reads the dashboard's
own environment, so it applies to a dashboard a run step started. What
an unattended decision may skip stays bounded either way: V142 and the
run's recheck refuse a declared decision before a shipping step.

Unattended does not mean consent. A playbook with irreversible effects still
needs `--confirm-irreversible=<consent_nonce>`, and the launch policy
(`agent_launch: ask` or `deny`) refuses the start as it would without the
flag. Notifications behave as for any failed run.

### What each wait does in an unattended run

1. **A `human_review` without `unattended`** (or with `unattended: fail`):
   the run fails `hitl_required`.
2. **A `human_review` with `unattended: { decide: <option> }`**: the review
   is decided at once with that option, on every visit, and journaled as
   `review_decided` with `decided_by: unattended`. The validator refuses an
   option that reaches an irreversible or secret step before the next gate,
   over its edges and its failure handlers (HOWTO-authoring.md, "A review in
   an unattended run"); the run checks it again with its pinned
   sub-playbooks and fails `hitl_required` when that check refuses. A drive
   that stopped between asking and deciding decides on resume; a decision
   that cannot be recorded fails the run `hitl_required`.
3. **A `publish` node**: the run fails `hitl_required`. A push always needs
   a person's consent to its plan; there is no way to declare one.
4. **A question with a `default_answer`**: answered at once with the
   default (`answered_by: unattended`), without waiting for its timeout.
5. **A question without a `default_answer`**: the run fails
   `hitl_required`.
6. **Host tasks and supervisors**: an unattended start that would run its
   agent steps in the host (`execution: host`), under a supervisor
   (`apb run --supervise`, MCP `supervise: self`) or in another supervised
   mode is refused before any run file exists, with a start action
   `fix_requirement` that says what to change (policy
   `unattended_unsupported`); `apb run` exits 2. An unattended MCP start
   also never falls back from an agent CLI to the host.
7. **A timer `wait`**: elapses and the run goes on, as attended.
8. **A webhook `wait`**: waits for its signal, which a program may still
   send; at `timeout_seconds` the node fails and takes its failure route,
   as attended.
9. **A deferred retry (`retry_delay`)**: the retry runs at its time. An
   unattended run never releases its driver while it waits: the driver
   sleeps in its own process, so a foreground `apb run --unattended` ends
   with the run's real outcome instead of exiting 4 and leaving a waker the
   job's container would kill. `apb resume --retry-now` works as for any
   run.
10. **The run's budget**: where an attended run would pause for its budget,
    an unattended run fails with a run error that starts with
    `budget_exhausted:`, and journals no pause. This holds for a node on
    its own, a node in a concurrent batch and the items of a list node.
11. **A quota pause**: pauses as today and resumes at `until`; the resumed
    run is still unattended. `apb wait` exits 4 for it; `--follow-auto`
    keeps waiting up to a minute past `until`.
12. **An operator pause**: unchanged; it is a person's act, not a wait.
13. **A start that needs consent** (irreversible effects, `agent_launch:
    ask`): refused before the run, as without the flag.

A list item never asks a question (`interactive` is refused on a list
node), so items have no unattended case of their own.

### Seeing it before the run

Every next action of every run, attended or not, carries `unattended`: what
an unattended run would do at that wait. It is one of `proceeds` (with the
automatic outcome: the timer elapses, the wait times out, the retry runs,
the resume is tried), `decides` (with the option), `answers_default`,
`fails_run` (with the code, `hitl_required` or `budget_exhausted`) and
`refused_at_start` (with the code). So a CI author can run the playbook
once attended, read `apb runs <id> --json` or MCP `run_status`, and see
which gates still need an `unattended` declaration. A gate whose declared
decision the run's own check would refuse (a pinned sub-playbook or an
installed connector after it) shows `fails_run`, as the run would. The action itself is
the one an attended run shows at that wait (`decide_review`,
`answer_question`, `retry_now`, ...); a run that already failed
`hitl_required` shows `read_report`. The run page marks an unattended run
with a badge and shows the outcome under each action; the dashboard does
not start a run unattended.

## GitHub Actions

```yaml
name: triage-failed-build
on:
  workflow_run:
    workflows: [build]
    types: [completed]

jobs:
  triage:
    # Only builds of this repository's own branches: a build started by a
    # fork's pull request must never reach a job that holds secrets.
    if: >-
      github.event.workflow_run.conclusion == 'failure' &&
      github.event.workflow_run.head_repository.full_name == github.repository &&
      github.event.workflow_run.event != 'pull_request'
    runs-on: ubuntu-latest
    permissions:
      contents: read
      actions: read
    env:
      APB_CONFIG_DIR: ${{ github.workspace }}/.apb-ci-config
      APB_VERSION: vX.Y.Z   # the apb release to install
    steps:
      # The default branch, not the failed build's head: `.apb/` (playbooks,
      # profiles, scripts) comes from reviewed code only.
      - uses: actions/checkout@v4   # pin to a commit SHA in real use

      - name: Install apb
        run: |
          curl --proto '=https' --tlsv1.2 -LsSf \
            "https://github.com/itechmeat/agentic-playbooks/releases/download/${APB_VERSION}/apb-installer.sh" | sh
          echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"

      - name: Install the agent CLI
        run: echo "install the agent CLI your profile binds, pinned to a version"

      - name: Fetch the failed build's log
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh run view ${{ github.event.workflow_run.id }} --log-failed > build.log

      - name: Triage
        env:
          AGENT_API_KEY: ${{ secrets.AGENT_API_KEY }}   # the variable your agent CLI reads
        run: |
          mkdir -p "$APB_CONFIG_DIR"
          out=$(apb run ci-failure-triage --version 1.0.0 --detach --param log_path=build.log)
          run_id=${out#run started: }
          echo "run_id=$run_id" >> "$GITHUB_ENV"
          set +e
          apb wait "$run_id" --timeout 900 --follow-auto
          code=$?
          set -e
          apb runs "$run_id"
          exit $code

      - name: Keep the run record
        if: always()
        uses: actions/upload-artifact@v4   # pin to a commit SHA in real use
        with:
          name: apb-run
          path: .apb/runs/${{ env.run_id }}/
```

A `workflow_run` job runs in the base repository with its secrets, even when
the build it reacts to came from a fork's pull request, and a fresh
`APB_CONFIG_DIR` plus a CLI start approves whatever `.apb/` the checkout holds.
So the job skips builds from other repositories and pull requests, checks out
the default branch (reviewed `.apb/`), and treats the downloaded build log as
data only; it never checks out or executes the failed build's code.

The installer places `apb` in `CARGO_HOME` (`~/.cargo/bin` by default, see
INSTALL.md), which the `GITHUB_PATH` line adds for later steps. The summary is the `triage` node's output in
`apb runs` and in the journal; a follow-up step can post it as a job summary or
a pull request comment.

## Any other runner

The same sequence works under any scheduler (a cron job, a systemd timer, a
different CI system). A POSIX shell version:

```sh
#!/bin/sh
set -eu
export APB_CONFIG_DIR="${APB_CONFIG_DIR:-$PWD/.apb-ci-config}"
mkdir -p "$APB_CONFIG_DIR"

out=$(apb run ci-failure-triage --version 1.0.0 --detach --param log_path=build.log)
run_id=${out#run started: }
echo "apb run: $run_id"

set +e
apb wait "$run_id" --timeout 900 --follow-auto
code=$?
set -e

case $code in
  0) echo "triage succeeded" ;;
  3) echo "run $run_id needs input: apb review / apb answer, then apb wait $run_id --follow-auto" ;;
  5) echo "timed out, stopping"; apb stop "$run_id" ;;
  *) echo "triage did not succeed (apb wait exit $code)" ;;
esac
apb runs "$run_id"
exit $code
```

This is also the interim answer to scheduled and event-driven runs: an external
scheduler starts the playbook, and `apb wait` turns the outcome into an exit
code the scheduler understands.
