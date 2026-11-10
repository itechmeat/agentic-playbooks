# run-external-data Specification

## Purpose
TBD - created by archiving change near-ai-cloud-models. Update Purpose after archive.

## Requirements

### Requirement: Every start surface takes the external data flag
Every surface that starts a run SHALL accept the boolean `external_data` (default false) with one meaning: the run's input comes from outside the operator and must not be kept for learning. The surfaces are REST `POST /api/playbooks/{id}/run` and the scenario run route, the dashboard's Run dialog, `apb run` and `apb eval` (`--external-data`; `apb eval --level quick|smoke` is the CLI's scenario run), and MCP `playbook_run`, `playbook_trial`, `playbook_scenario_run` and `playbook_execute_plan`.

#### Scenario: The same start on three surfaces
- **WHEN** one playbook is started with `external_data: true` through REST, through `apb run --external-data` and through MCP `playbook_run`
- **THEN** each of the three runs has `external_data: true` in its manifest and on its `run_started` event

#### Scenario: A supervised CLI start
- **WHEN** `apb run --supervise --external-data` starts a run through its detached supervised drive
- **THEN** the run's manifest carries the flag

#### Scenario: A plan execution and a trial
- **WHEN** `playbook_execute_plan` or `playbook_trial` is called with `external_data: true`
- **THEN** the run it starts carries the flag

#### Scenario: An eval
- **WHEN** `apb eval --external-data` runs three repetitions
- **THEN** every repetition run carries the flag

### Requirement: The flag adds no other start check
A start with the flag SHALL be refused by every check that refuses the same start without it, and by no other check except `external_data_remember_refused` and `external_data_immutable`.

#### Scenario: An untrusted playbook
- **WHEN** an untrusted playbook is started over MCP with `external_data: true` and without `acknowledge_untrusted`
- **THEN** the refusal is the same trust refusal an unflagged start gets

### Requirement: The flag is fixed for the run's lifetime
The flag SHALL be written once at start into `manifest.yaml` and into `run_started.external_data`, and SHALL NOT change afterwards: a manifest amendment, a patch, a rebind, a supervisor action and a resume keep it. A resume that asks for a different value SHALL be refused with `external_data_immutable`; a resume that repeats the run's own value SHALL be accepted.

#### Scenario: A resume cannot set it
- **WHEN** an unflagged run is resumed with `external_data: true`
- **THEN** the resume is refused with `external_data_immutable`

#### Scenario: A resume cannot clear it
- **WHEN** `apb resume` or MCP `run_resume` is called on a flagged run with `external_data: false`
- **THEN** the resume is refused with `external_data_immutable` and the journal is unchanged

#### Scenario: A secret param added by a patch
- **WHEN** a current-run patch adds a secret param to a flagged run and the manifest is amended
- **THEN** the amended manifest still says `external_data: true`

### Requirement: Descendants and successors inherit the flag
A sub-playbook child of a flagged run SHALL be flagged whatever its start asked. A run started with `continued_from` naming a flagged run SHALL be flagged; such a start that explicitly asks for `external_data: false` SHALL be refused with `external_data_immutable`.

#### Scenario: A child of a scenario run inherits
- **WHEN** a flagged scenario run starts a sub-playbook node
- **THEN** the child, placed under `.apb/scenario-runs`, is flagged

#### Scenario: A child inherits
- **WHEN** a flagged run starts a sub-playbook node
- **THEN** the child's manifest and `run_started` carry `external_data: true`

#### Scenario: A successor inherits
- **WHEN** a run is started with `continued_from` naming a flagged run and no `external_data`
- **THEN** the new run is flagged

### Requirement: The flag is visible on every run surface
The flag SHALL be shown, read from the run's manifest, by `GET /api/runs/{id}` (`RunDetail.external_data`), the runs list (`RunSummary.external_data`), MCP `run_status` and `runs_list`, `apb runs <id>`, `apb runs` and the run report page. The field SHALL be left out for a run without the flag.

#### Scenario: A caller verifies the flag
- **WHEN** a caller started a run with `external_data: true` and reads `run_status`
- **THEN** the answer has `external_data: true`

### Requirement: A flagged run recalls no run input
A flagged start SHALL apply no remembered input in the gate's param check or in preparation, as a scenario start does, and `run_provenance.remembered` SHALL be empty. Values the caller passes SHALL be used as passed. The dashboard's Run dialog SHALL drop prefilled remembered values the person did not edit when the external data box is checked.

#### Scenario: Remembered values are not applied
- **WHEN** the project remembers `customer: acme` for a playbook and a flagged start passes no `customer`
- **THEN** the run's `customer` is the param's default (or empty) and `run_provenance.remembered` is empty

### Requirement: A flagged start never remembers its input
A flagged start that also asks to remember its input (`remember: true`, `apb run --remember`) SHALL be refused before anything is written with `external_data_remember_refused`. The dashboard's Run dialog SHALL disable "Remember for this project" while the external data box is checked.

#### Scenario: Remember on an inheriting successor
- **WHEN** a REST start names a flagged run in `continued_from`, omits `external_data` and asks `remember: true`
- **THEN** it is refused with `external_data_remember_refused` and the memory file is unchanged

#### Scenario: Remember with the flag
- **WHEN** MCP `playbook_run` is called with `external_data: true` and `remember: true`
- **THEN** it is refused with `external_data_remember_refused`, no run directory is created and the memory file is unchanged

### Requirement: A flagged run adds nothing to a learning store
For a flagged run apb SHALL write nothing into the known problems store, the corrections ledger, the node cache, judge examples, scenario captures, forward patches and their candidates, candidate trial records, the eval result store, the zcode desktop history, and the scenario run index beyond the line retention needs (the run id, the scenario, the level, the time and the flag). A standalone decision log line about a flagged run SHALL carry `external_data: true`.

#### Scenario: Known problems and corrections
- **WHEN** a flagged run ends with a `tool_problem` finding and a correction a person typed at a review
- **THEN** the playbook's known problems store and corrections ledger are byte-identical to before the run

#### Scenario: The node cache
- **WHEN** a flagged run finishes a cache-eligible node
- **THEN** no record or object is added under `.apb/cache`, and a later unflagged run of the same node with the same key misses

### Requirement: Tools that learn from one run refuse a flagged run
`run_scenario_capture`, `judge_example_save` and `judge_run_values` with a run id, `supervisor_patch_playbook` with `scope: next_runs`, and `playbook_memory` from a step of the run, with their CLI and HTTP twins, SHALL refuse a flagged run with `external_data_run`. A flagged start SHALL never take a waiting candidate: it runs `current` and records `candidate_skipped: external_data`.

#### Scenario: Scenario capture
- **WHEN** `run_scenario_capture` is called on a flagged run
- **THEN** it is refused with `external_data_run` and no scenario file is written

#### Scenario: A candidate named explicitly
- **WHEN** a flagged start names the waiting candidate's version explicitly and succeeds
- **THEN** the manifest has no `candidate_trial` and the candidate's trial record is unchanged

#### Scenario: Run memory from a step
- **WHEN** an agent step of a flagged run calls `playbook_memory` with `get` or `set`, for its own workspace or another one
- **THEN** it is refused with `external_data_run` and the memory file is unchanged

#### Scenario: A step whose run is not found
- **WHEN** a step calls `playbook_memory` with `get` or `set` and its run is found neither through `APB_RUN_DIR` nor under the target workspace
- **THEN** it is refused with `external_data_run`

#### Scenario: A failed preparation
- **WHEN** a flagged start fails in preparation before its manifest is written
- **THEN** the `run_started` the failure writes carries `external_data: true` and no reader of earlier runs counts the run

#### Scenario: A forward patch
- **WHEN** the supervisor of a flagged run calls `supervisor_patch_playbook` with `scope: next_runs`
- **THEN** it is refused with `external_data_run` and no candidate version exists

### Requirement: Readers of earlier runs skip flagged runs
Every reader of earlier runs SHALL skip a flagged run through one predicate: run statistics, pre-start estimates, quality comparisons, candidate settlement, grade backfill, the retrospective's baseline, `run.previous_success`, the decisions report with its labels and replay, and history discovery. A run whose manifest does not read SHALL never count as history; a reader that cannot do without it SHALL refuse rather than guess, as `run.previous_success` does today.

#### Scenario: Stats and estimates
- **WHEN** a playbook has two unflagged finished runs and one flagged finished run
- **THEN** `apb stats` counts two runs and the next start's estimate is built from the two

#### Scenario: Previous success
- **WHEN** the latest successful run of a routine is flagged and an earlier one is not
- **THEN** a new run's `run.previous_success.id` names the earlier unflagged run

#### Scenario: Discovery
- **WHEN** a Claude Code session file is named after a session id that a flagged run's `attempt_finished.session` recorded
- **THEN** `apb discover` does not open it, and its result counts it under the skip reason `external_data`

#### Scenario: A host session that names a flagged run
- **WHEN** a session file's tool inputs name a flagged run id within the part the walker reads
- **THEN** none of its steps, requests or examples appear in a discovery result, and the end-of-turn save hook gives it no nudge

#### Scenario: Flagged runs do not crowd out history
- **WHEN** a playbook has two unflagged finished runs followed by more flagged runs than the estimate's history window
- **THEN** the next start's estimate is still built from the two unflagged runs

### Requirement: A reader that checks only the origin skips a flagged run
A flagged run whose origin would otherwise be production SHALL carry `origin: { kind: external_data }` in its manifest, so a reader that checks only the origin, including an older apb that reads an unknown origin as not production, skips it.

#### Scenario: The origin of a flagged run
- **WHEN** a flagged run is started through `playbook_run`
- **THEN** its manifest has `origin.kind: external_data` and the run does not count as production history

### Requirement: A flagged run otherwise runs as it would without the flag
Apart from the rules above, a flagged run SHALL behave as the same run without the flag: journal, outputs, results, usage and cost, budget, notifications, review gates, follow-up check, grades in its own directory, run report and its own retrospective. Everything the run produces SHALL stay in its run directory.

#### Scenario: The same journal
- **WHEN** a playbook with no cache-eligible node, no remembered input and no waiting candidate runs twice on the fake agent with the same input, once flagged and once not
- **THEN** the two journals are equal except for `run_started.external_data`, the run ids and the times

### Requirement: Patch versions of a flagged run stay with the run
A playbook version a current-run patch of a flagged run creates SHALL record the run in its provenance, SHALL never be promoted to `current` or become a candidate, and SHALL be refused with `external_data_run` by `apb version use`, by a move of `current` and by an unflagged start that names it.

#### Scenario: A patched flagged run
- **WHEN** the supervisor of a flagged run applies a `scope: current_run` patch and the patched node succeeds under `promote_supervisor_patches`
- **THEN** `current` is unchanged and an unflagged start naming the patch version is refused with `external_data_run`

### Requirement: An apb without this capability cannot drive a flagged run
A flagged run's `execution.mode` SHALL carry the suffix `+external`, which every surface strips for display, so an apb that does not know it refuses to drive, resume or amend the run instead of running it without the guards.

#### Scenario: A downgrade
- **WHEN** a paused flagged run is resumed by an apb build whose mode reader does not know `+external`
- **THEN** that build refuses to drive it with its version message and writes nothing
