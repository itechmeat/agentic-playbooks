# External data runs

Some runs process data that does not come from the person who runs apb: a
third party's documents, messages or records. Such data must stay with the
run that processed it. A run started with the flag `external_data: true`
keeps everything it produces in its own run directory and adds nothing to
a store that later runs, statistics or learning tools read.

## Starting one

| Surface | How |
|---|---|
| CLI | `apb run <id> --external-data` (also with `--supervise` and `--detach`) |
| Evals and scenario runs | `apb eval <id> --external-data`, with `--level quick` or `--level smoke` for scenario runs |
| MCP | `external_data: true` on `playbook_run`, `playbook_trial`, `playbook_execute_plan` and `playbook_scenario_run` |
| HTTP | `external_data: true` in the body of `POST /api/playbooks/{id}/run` and of the scenario run route |
| Dashboard | the Run dialog's "External data: keep nothing for learning" box |

The flag is fixed for the run's lifetime. It is written once into the
run's `manifest.yaml` (`external_data: true`, the origin `external_data`
for a run that would otherwise be production, and the suffix `+external`
on the execution mode) and into `run_started.external_data`. A
sub-playbook child of a flagged run is flagged whatever its start asked,
and so is a run started with `continued_from` naming a flagged run.

A resume takes the flag only as a check (`apb resume --external-data
<true|false>`, MCP `run_resume`, the HTTP resume body): the run's own
value is accepted, any other is refused with `external_data_immutable`.

## Seeing it

`apb runs` marks the run `external data`, `apb runs <id>` prints a line
for it, MCP `run_status` and `runs_list`, `GET /api/runs` and `GET
/api/runs/{id}` carry `external_data: true`, and the run page and the
report page say it. The field is left out for every other run, and no
surface shows the `+external` suffix inside a mode.

## What it keeps out

| Store or reader | For a flagged run |
|---|---|
| Remembered run input | nothing is applied at start (the gate's param check counts memory as empty too, so a param only memory would fill must be passed), and asking to remember is refused with `external_data_remember_refused` |
| Run memory from a step | `playbook_memory` `get` and `set` from a step of the run are refused with `external_data_run`, for any workspace; a step whose run cannot be found is refused too |
| Node cache | nothing is stored; a lookup may still hit an earlier run's entry |
| Known problems | nothing is added at run end or by a scan |
| Corrections ledger | nothing is added |
| Candidates | a waiting candidate is never taken (`candidate_skipped: external_data`), and a start that names the candidate's version is no trial |
| Patch versions | a current-run patch makes a version that records the run (`external_data_run` in its provenance); it never becomes `current` or a candidate, and moving `current` to it, `apb version use` and an unflagged start refuse it |
| Forward patches, judge examples and run values, scenario captures | refused with `external_data_run` |
| Statistics, estimates, quality comparisons, grade backfill, the retrospective baseline, `run.previous_success` | the run is skipped, and it never crowds earlier runs out of a capped window |
| Decisions report and replay | the run is not read; the report counts it as `skipped_external` |
| Standalone decision log | a line about the run carries `external_data: true`, kept for the daily cap only |
| Eval results | a flagged eval prints its result and stores none |
| Scenario run index | the line holds only the run id, the scenario, the level, the time and the flag, which retention needs; `latest` skips it |
| History discovery and the save hook | a session file named after one of the run's agent sessions is never opened, and a session whose read part names the run id contributes nothing (counted under the skip reason `external_data`); the end-of-turn save hook gives such a session no reminder and asks nothing |
| zcode desktop history | no row is added |

Everything else runs as it would without the flag: the journal, outputs,
results, usage and cost, the budget, notifications, review gates, the
follow-up check, grades in the run's own directory, the run report and
the run's own retrospective.

## What it does not reach

The flag governs what apb itself keeps. An agent CLI keeps its own memory
and session files outside apb: Claude Code stores sessions under the
person's home directory, and an agent may write notes of its own. apb's
own readers skip those sessions (discovery and the save hook above), but
it cannot delete them. For a stricter setup, give the profiles
`process_env: allowlist` and the nodes `isolation: full` (see
[PROFILES.md](PROFILES.md) and [ISOLATION.md](ISOLATION.md)). In host
execution mode the host session does the work, and what the host keeps is
the host's.

Discovery reads only the start of a long session file: a run id named past
that point is not seen.

## Older apb binaries

An apb without this capability does not know the `+external` mode suffix,
so it refuses to drive, resume or amend a flagged run with its "written by
a newer apb" message instead of running it without these guards. Its
readers of earlier runs see the origin `external_data` as an unknown origin,
which is not history either.
