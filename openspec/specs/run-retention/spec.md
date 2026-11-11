# run-retention Specification

## Purpose
TBD - created by archiving change routine-health. Update Purpose after archive.

## Requirements

### Requirement: Prune is a person's dry run first
`apb runs prune` SHALL print what it would remove and keep with a reason, and write nothing, unless `--yes` is passed. With `--yes` it SHALL be refused inside a run step. No MCP tool or HTTP route SHALL prune.

#### Scenario: The default is a dry run
- **WHEN** a person runs `apb runs prune`
- **THEN** no run directory is removed and no archive file is written

### Requirement: What is never removed
Pruning SHALL never remove a live or paused run, one of the newest `keep_last` runs of a playbook, the latest successful top-level production run of a playbook as the previous-success lookup finds it, a run with a pending follow-up check, a graded run, a trial run of the open candidate, a run with an unknown send, a run a live start key or a concurrency slot names, or a run whose journal does not read strictly.

#### Scenario: A newer success reopened after the plan
- **WHEN** the latest success is resumed after the plan and before an older success is removed
- **THEN** the older success, now the latest, is kept

#### Scenario: An eval success does not stand in
- **WHEN** a playbook's only production success is old and a newer eval run succeeded
- **THEN** the production success is kept with the reason `latest success`

#### Scenario: A journal a newer apb wrote
- **WHEN** a run's journal ends with an event type this apb does not know and no checkpoint after it
- **THEN** it is kept as `unreadable`

### Requirement: A family is removed whole or kept whole
A run SHALL be kept when any of its descendants at any depth is kept, a child SHALL be removed only with its parent, and the rules that can change after the plan SHALL be checked again for every member of a family under the members' event locks before the removal commits.

#### Scenario: A graded child keeps its parent
- **WHEN** an old parent run has a graded child run
- **THEN** both are kept

#### Scenario: A graded grandchild keeps the whole family
- **WHEN** an old run's child has a graded child
- **THEN** all three runs are kept

#### Scenario: A child resumed after the plan
- **WHEN** a child of a planned family is resumed before the removal commits
- **THEN** the whole family is kept

#### Scenario: A child started after the plan
- **WHEN** a planned parent is resumed, starts a new child and ends again before its removal takes its locks
- **THEN** the whole family and the new child are kept

#### Scenario: A grade stored during the removal
- **WHEN** a grade of a planned run is being stored when the removal takes the family's locks
- **THEN** the removal waits for that grade under the run's event lock or is refused, and a later prune keeps the family with its grade

#### Scenario: A grade after the run left
- **WHEN** a grade of a run is written after the removal took the run's event lock
- **THEN** the grade is refused and nothing is made where the run was

### Requirement: Pruning stays inside its anchors
Pruning SHALL touch only safe run ids that are real directories under `.apb/runs` and `.apb/runs-archive`, SHALL use a child id only when the child names the parent back, and SHALL refuse links and non-directory entries anywhere in the tree.

#### Scenario: A planted link
- **WHEN** a run entry is a link to a directory outside the project
- **THEN** it is kept as `unsafe` and nothing outside the project changes

#### Scenario: A recovery through a linked runs directory
- **WHEN** `.apb/runs` is a link and an interrupted removal left a staging directory
- **THEN** the recovery is refused and nothing is moved
- **AND** a staged entry that is not a real directory is never moved under `.apb/runs`

### Requirement: Retention settings
`policy.yaml` `runs:` SHALL set `keep_days` (default 30), `keep_last` (default 20) and `keep_failed_days` (default 90); a project file SHALL only lengthen what the machine sets.

#### Scenario: A project keeps longer
- **WHEN** the machine sets `keep_days: 30` and the project `keep_days: 60`
- **THEN** a success 45 days old is kept

#### Scenario: A project that sets nothing
- **WHEN** the machine sets `keep_days: 7` and the project file sets no `keep_days`
- **THEN** `keep_days` is 7

### Requirement: A signed summary of each removed run
Each removed run SHALL leave a signed summary with its outcome, its health class, origin, parent, times, spend and an abort code from a fixed list, never a param value, an output or a reason text; the health class SHALL be the one its live journal gave. Health SHALL count verified top-level production and external data summaries; `apb stats` SHALL count verified top-level production summaries, including versions that have no run directory left.

#### Scenario: Health survives a prune
- **WHEN** five failed runs trip `consecutive_failures: 5` and three are pruned
- **THEN** the rule is still tripped

#### Scenario: A run health never counted
- **WHEN** a run without `run_provenance`, or without this installation's origin stamp, is pruned
- **THEN** its summary's class is `ignored` and health still does not count it

#### Scenario: A version with only pruned runs
- **WHEN** every run of version 1.0 was pruned
- **THEN** `apb stats` lists version 1.0 with its archived counts

### Requirement: Every run is counted once during a removal
A family's summary SHALL be written before its directories leave `.apb/runs`, readers SHALL count a run from its directory when it has one and from its summary otherwise, and the readers and the removal SHALL exclude each other so that a run is counted exactly once at every moment.

#### Scenario: A reader during a removal
- **WHEN** health reads the runs of a playbook while a prune has moved a family into staging and not yet committed it
- **THEN** every run of the family is counted exactly once

#### Scenario: A resume races the prune
- **WHEN** a planned run is resumed before its removal
- **THEN** it is kept

### Requirement: An interrupted removal is recovered by the next prune
An interrupted removal SHALL be finished when it was committed and undone otherwise by the next prune, and only one prune SHALL run at a time in a project.

#### Scenario: A crash mid-removal
- **WHEN** a prune crashes after writing a family's summary and before all its directories were moved
- **THEN** the next prune moves the directories back, deletes the summary, and every run is counted once

#### Scenario: Two prunes at once
- **WHEN** a second `apb runs prune --yes` starts while one runs in the project
- **THEN** it is refused `prune_in_progress` and changes nothing

### Requirement: Keyed runs and unreadable key stores
Pruning SHALL keep a run that a live start-key entry or a concurrency slot names, and SHALL keep every run of a playbook whose start-key file or concurrency slot file does not verify. A summary SHALL record `external_data` as the origin of a run started with that flag, and `apb stats` SHALL not count such summaries.

#### Scenario: A failed keyed run stays while its key is live
- **WHEN** an old failed run is bound to a start-key entry within `runs.start_key_days`
- **THEN** it is kept with the reason `start key`

#### Scenario: A start-key file that does not verify
- **WHEN** the playbook's start-key file was changed outside apb
- **THEN** every run of that playbook is kept with the reason `start key`
