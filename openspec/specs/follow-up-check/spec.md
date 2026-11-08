# follow-up-check Specification

## Purpose
TBD - created by archiving change run-feedback. Update Purpose after archive.

## Requirements

### Requirement: A follow-up is read once after a successful run
A playbook's `follow_up: { after, grace?, check }` SHALL be read once, `after` the run's latest `run_finished` with outcome `succeeded`, for production runs only, and journaled as one `follow_up_observed` event. A failed, aborted or resumed-and-unfinished run SHALL have no follow-up, and a run SHALL have at most one `follow_up_observed`.

#### Scenario: Due from the end
- **WHEN** a run with `follow_up.after: 1h` finished successfully at 10:00
- **THEN** its follow-up is pending, due at 11:00, and is journaled once after 11:00

#### Scenario: A failed run
- **WHEN** a run with a follow-up fails
- **THEN** no waker is left and the outcome shows the follow-up as not scheduled

### Requirement: The check reuses the baseline check's shape
The follow-up `check` SHALL be one check in the baseline check's shape with a connector or a script source and absolute rules (`equals`, `at_most`, `at_least`, `empty`, `contains`), its connector read under the engine site `@follow_up`.

#### Scenario: A deal stage
- **WHEN** the check reads `deal.stage` from a CRM connector with `expect: { equals: won }` and the stage is `won`
- **THEN** `follow_up_observed` has `status: observed` and `verdict: met`

### Requirement: Unknown is never met
The event's `verdict` SHALL be `met` only when every observation was met, `not_met` when any was not met, and `unknown` otherwise; `unknown` and `missed` SHALL never count as met on any surface.

#### Scenario: The source cannot be read
- **WHEN** the connector call fails
- **THEN** the verdict is `unknown` and `apb stats` counts it apart from met

### Requirement: A machine that was off
A follow-up whose time passed while no waker ran SHALL be read by `apb resume --all` when it is at most `grace` late (default 24 hours), and journaled `missed` without reading the source when later; one still ahead SHALL get a new waker.

#### Scenario: Two days late
- **WHEN** `apb resume --all` runs two days after a follow-up's time with the default grace
- **THEN** it journals `status: missed` and makes no connector call

### Requirement: One reader at a time
Every read of a follow-up SHALL take the lease `follow-up-<finished seq>` and re-read the journal under it, so a waker and `apb resume --all` never journal two observations.

#### Scenario: Two readers
- **WHEN** two processes try to read the same due follow-up
- **THEN** one `follow_up_observed` is journaled

### Requirement: The event is a checkpoint
`follow_up_observed` SHALL be a checkpoint: an event type this apb does not know that comes before it is settled, one that comes after it is not.

#### Scenario: Unknown before it
- **WHEN** a journal holds `run_finished`, an event type this apb does not know, then `follow_up_observed`
- **THEN** `read_all` reads the journal

#### Scenario: Unknown after it
- **WHEN** an event type this apb does not know follows `follow_up_observed`
- **THEN** `read_all` refuses the journal

### Requirement: Only a run this machine created is followed up
A follow-up SHALL be armed, read and journaled only for a run directory that passes the resume check of the run gate (a run this machine created); any other run SHALL be left untouched and no source read.

#### Scenario: An imported run
- **WHEN** `apb resume --all` finds a run directory that came with the repository, whose follow-up script writes a marker file
- **THEN** nothing is journaled and the marker file is absent

### Requirement: Nothing secret is journaled
The verdict SHALL be computed from the raw values in memory, and the observations, details and reason SHALL be scrubbed with the run's scrubber before the event is appended; when the run's secrets cannot be read, the event SHALL be `unknown` with no observations.

#### Scenario: A secret in an observed value
- **WHEN** an observed field holds a configured secret value
- **THEN** `events.jsonl` does not hold it

### Requirement: The follow-up shows where the outcome shows
The run outcome (CLI, MCP `run_status`, the dashboard, the HTML report) SHALL show a pending follow-up with its due time and an observed one with its verdict and results; `not_met` SHALL mark the run as needing a look. `apb stats` SHALL count observed, met, not met, unknown, missed and pending per version, and a `follow_up` notification transition SHALL be offered opt-in.

#### Scenario: Notified on opt-in
- **WHEN** a subscription names `follow_up` and a follow-up is observed
- **THEN** one notification is sent for that observation

### Requirement: V183 to V186
The validator SHALL report V183 (error) for an `after` that is not a duration or outside 1 minute to 30 days, V184 (error) for a `grace` that is not a duration or outside 1 minute to 7 days, V185 (error) for a `project_checks` source, and V186 (error) for a `change` rule.

#### Scenario: A change rule
- **WHEN** a follow-up observation states `expect: { change: decrease }`
- **THEN** validation fails with V186
