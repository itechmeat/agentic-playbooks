# previous-success Specification

## Purpose
TBD - created by archiving change recurring-routines. Update Purpose after archive.

## Requirements

### Requirement: The previous success namespace
A template SHALL be able to read `run.previous_success.id`, `run.previous_success.started_at`, `run.previous_success.finished_at` and `run.previous_success.nodes.<id>.output[.<field>]` (with `report` as an alias of `output`). Times SHALL render as RFC 3339 UTC in whole seconds, truncated. A field SHALL render as `nodes.<id>.output.<field>` renders it, arrays and objects as compact JSON.

#### Scenario: A routine reads its window
- **WHEN** an earlier run of the playbook succeeded after starting at 2026-11-05T07:00:00.900Z
- **AND** a new run's prompt holds `since {{run.previous_success.started_at}}`
- **THEN** the rendered prompt holds `since 2026-11-05T07:00:00Z`

#### Scenario: An array field of the earlier run
- **WHEN** the earlier successful run's node `draft` output `{"lead_ids":[3,7]}`
- **THEN** `{{run.previous_success.nodes.draft.output.lead_ids}}` renders `[3,7]`

### Requirement: Which earlier run counts
The earlier run SHALL be the newest by start time among the production runs apb created on this machine under the same execution root, of the same playbook id and scope, of any version, that are top-level runs and whose journal folds to succeeded. Scenario and eval runs SHALL never count. A child run SHALL NOT look up and SHALL render the "none" window.

#### Scenario: A failed run is skipped
- **WHEN** the newest earlier run failed and the one before it succeeded
- **THEN** `run.previous_success.id` renders the id of the one that succeeded

#### Scenario: A child run gets no window
- **WHEN** a child run of a playbook starts after a top-level run of it succeeded
- **THEN** `run.previous_success.id` renders empty in the child

#### Scenario: Another scope does not count
- **WHEN** only a global playbook of the same id succeeded in this project
- **THEN** a run of the project playbook renders `run.previous_success.id` empty

### Requirement: Unreadable history refuses the start
A start whose definition reads the namespace SHALL be refused, before anything is written, with `previous_success_unreadable` when the runs directory, or a run directory apb created here that could be the latest success, cannot be read. A run directory without the origin stamp and a start in progress without `run_started` SHALL NOT refuse it.

#### Scenario: A corrupt journal newer than the last success
- **WHEN** the newest stamped run of the playbook has a journal line that is not JSON and an older run succeeded
- **THEN** the start is refused with `previous_success_unreadable` naming that run, and no run directory is created

### Requirement: When there is no earlier success
Without an earlier success, `id` and node outputs SHALL render empty, and `started_at` and `finished_at` SHALL render the playbook's `previous_success.when_none` values, rendered over params, or empty without them.

#### Scenario: The first run of a routine
- **WHEN** no earlier run succeeded and `when_none.started_at` is `{{params.since}}` with `since` `2026-01-01T00:00:00Z`
- **THEN** `{{run.previous_success.started_at}}` renders `2026-01-01T00:00:00Z`

#### Scenario: A fallback that is not a time
- **WHEN** `when_none.started_at` renders `yesterday`
- **THEN** the start is refused before anything is written, and the refusal does not hold the value

### Requirement: Read once and journaled
A start whose definition reads the namespace SHALL record what it renders on `run_provenance.previous_success`, with node output values redacted by the earlier run's redactor and clipped at 16 KiB, and every render of the run SHALL read that record. A run continued from another SHALL keep its predecessor's record. A run of such a definition SHALL carry a `run_guard` naming `previous_success`.

#### Scenario: A resume renders the same window
- **WHEN** a run is resumed after another run of the playbook succeeded
- **THEN** its prompts render the same `run.previous_success` values as before the resume

#### Scenario: A secret of the earlier run is not copied
- **WHEN** the earlier run's node output holds that run's secret param value
- **THEN** the current run's journal does not hold that value

### Requirement: A patch cannot add an unrecorded read
A supervisor patch SHALL be refused when the patched definition reads `run.previous_success` while the run recorded no snapshot, or reads an output key that the definition in force at start did not read.

#### Scenario: A patch adds a new earlier output
- **WHEN** a patch adds `{{run.previous_success.nodes.draft.output.ids}}` to a prompt and the start definition did not read it
- **THEN** the patch is rejected with `previous_success_unrecorded`

### Requirement: V176 for a malformed previous success read
The validator SHALL report V176 (error) for a `run.previous_success` reference with an unknown key, a deeper path than one field, or an unknown node, for any read of the namespace in the run's `worktree` template, for a `when_none` value that is neither an RFC 3339 time nor a template over `params.*` only, and for a `when_none` template that reads a secret param.

#### Scenario: An unknown key
- **WHEN** a prompt reads `{{run.previous_success.ended}}`
- **THEN** validation fails with V176 and not with V13

### Requirement: The record on the run surfaces
`apb runs <id>`, MCP `run_status`, the dashboard's run view and the HTML report SHALL show the earlier run's id and times, or that there was none and which `when_none` times were used, and SHALL NOT carry the snapshotted output values.

#### Scenario: The CLI names the earlier run
- **WHEN** a run's record names earlier run `r1`
- **THEN** `apb runs <id>` prints a `previous success: r1, started ...` line

#### Scenario: The dashboard API holds no earlier output
- **WHEN** the run detail of a run that snapshotted `nodes.draft.output.ids` is fetched
- **THEN** its JSON has no `outputs` key and no snapshotted value
