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
apb run <playbook> --detach [--version X.Y.Z] [--param k=v]... [--instruction "..."] [--worktree DIR]
apb wait <run_id> [--timeout SECS]
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
| 3 | the run needs input: a question, a `human_review` gate, a supervisor decision, or host tasks of a host-mode run (`apb tasks <run>` lists them) | fail, or hand the run id to a person (see below) |
| 4 | the run is paused or its driver is gone | fail; `apb resume <run_id>` continues it |
| 5 | `--timeout` elapsed while the run was still going | fail, and `apb stop <run_id>` |

Without `--detach`, `apb run` drives the run in the foreground and exits 0 on
success, 1 on any other outcome, 2 on an error before the run starts. The
detached form is preferred in CI because `apb wait` also distinguishes "needs
input" and "timed out" from "failed", and a wait can be bounded with `--timeout`
without stopping the run itself.

`apb runs <run_id>` prints the node statuses, the failure reason, the token
usage and cost the agents reported, and the goal criteria results; the journal
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
  approval.

## Working trees

`--worktree DIR` makes the run's agent and script nodes work in `DIR` (absolute,
or relative to the project) and takes the busy lock on that tree instead of the
project's, so two runs over two checkouts do not wait on each other. In CI the
checkout is usually the working tree already; use `--worktree` when the job
prepares a separate `git worktree` for the run, or when the playbook declares a
`worktree` that should be overridden.

## Gates in CI

A run parked on a `human_review` gate or a question keeps waiting after the job
ends; its state is on the runner's disk. On an ephemeral runner, treat exit 3 as
a failure and design CI playbooks without gates. On a persistent runner (or a
self-hosted machine), print the run id and let a person decide with
`apb review <run_id> <node> --decision <option>` or `apb answer <run_id> <text>`,
then continue with `apb wait <run_id>`.

## GitHub Actions

```yaml
name: triage-failed-build
on:
  workflow_run:
    workflows: [build]
    types: [completed]

jobs:
  triage:
    if: github.event.workflow_run.conclusion == 'failure'
    runs-on: ubuntu-latest
    permissions:
      contents: read
      actions: read
    env:
      APB_CONFIG_DIR: ${{ github.workspace }}/.apb-ci-config
      APB_VERSION: vX.Y.Z   # the apb release to install
    steps:
      - uses: actions/checkout@v4   # pin to a commit SHA in real use
        with:
          ref: ${{ github.event.workflow_run.head_sha }}

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
          apb wait "$run_id" --timeout 900
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
apb wait "$run_id" --timeout 900
code=$?
set -e

case $code in
  0) echo "triage succeeded" ;;
  3) echo "run $run_id needs input: apb review / apb answer, then apb wait $run_id" ;;
  5) echo "timed out, stopping"; apb stop "$run_id" ;;
  *) echo "triage did not succeed (apb wait exit $code)" ;;
esac
apb runs "$run_id"
exit $code
```

This is also the interim answer to scheduled and event-driven runs: an external
scheduler starts the playbook, and `apb wait` turns the outcome into an exit
code the scheduler understands.
