## ADDED Requirements

### Requirement: Global playbooks start from the CLI
`apb run <id> --scope global` SHALL start the global playbook `<id>` in the current project on the synchronous, the detached and the host foreground start paths, and every check of the start (the run gate, the param check, the start key, the budget check, the estimate, the notify check, remembered input, the unattended refusal and the readiness warning) SHALL read the global playbook, never a project playbook of the same id.

#### Scenario: A global and a project playbook share an id
- **WHEN** both scopes hold `dup` and a person runs `apb run dup --scope global`
- **THEN** the global one runs, the run directory is in the project and the run's provenance origin is `global`

#### Scenario: A detached global start
- **WHEN** a person runs `apb run <id> --scope global --detach`
- **THEN** the command prints the run id and `apb wait` on it ends with the run's outcome

#### Scenario: Remembered input of a global start
- **WHEN** a person runs `apb run <id> --scope global --remember --param k=v`
- **THEN** `apb memory show <id> --scope global` lists `k` and `apb memory show <id>` does not

#### Scenario: An unknown scope
- **WHEN** a person runs `apb run <id> --scope team`
- **THEN** the command exits 2 with "--scope must be project or global" and nothing is started

### Requirement: A supervised global start is refused
`apb run --supervise --scope global` SHALL be refused with the code `supervise_global_unsupported` before any definition is read and before any run or remembered input is written.

#### Scenario: Supervise with the global scope
- **WHEN** a person runs `apb run <id> --scope global --supervise`
- **THEN** the command exits 2, names `supervise_global_unsupported` and creates no run directory

### Requirement: A global run never touches a project's candidate line
A run of a global definition SHALL never be marked a candidate trial, SHALL never settle or re-decide a candidate of a project playbook, SHALL never be counted for a project candidate's comparison or summary or graded by a project playbook's backfill, and a supervisor patch of it SHALL be refused `patch_global_unsupported` before any project definition is read or written.

#### Scenario: The same id and candidate version in a project
- **WHEN** a project holds `dup` with a candidate 1.1.0 and a global `dup` 1.1.0 runs in that project and ends, succeeded or failed
- **THEN** the project's candidate pointer, trial record and `current` are unchanged and the run journals no candidate event

#### Scenario: Grading the global run
- **WHEN** the global run is graded and the project's policy promotes by quality
- **THEN** the grade is stored and the project's candidate is not re-decided

#### Scenario: Patching the global run
- **WHEN** a supervisor patches the global run, in-run or forward
- **THEN** the patch is refused `patch_global_unsupported` and the project playbook is unchanged

### Requirement: Global playbooks in the dashboard
The dashboard SHALL list every global playbook once, whatever the number of projects and whichever project is selected, and SHALL show a global playbook on a read-only page with its fit for a chosen project and the commands that run it; no control of that page SHALL act on a project playbook.

#### Scenario: The global page
- **WHEN** a person opens a global playbook in the dashboard
- **THEN** the page shows `apb run <id> --scope global` and offers no run, edit, version or delete control

### Requirement: The HTTP list and detail carry the scope
`GET /api/playbooks` SHALL aggregate project and global entries, each carrying its `scope`; the detail and the fit SHALL take `scope` with `project` as the default, validate a global playbook as a global definition, and answer 400 to any other value.

#### Scenario: Two projects
- **WHEN** the dashboard serves two projects and the global store holds one playbook
- **THEN** the list answers that playbook once, with `scope: global`

#### Scenario: A global detail is validated as global
- **WHEN** a global playbook binds a profile with `scope: project`
- **THEN** its detail with `scope=global` carries V14 in `validation`

#### Scenario: The same id in both scopes
- **WHEN** a project and the global store both hold `dup` and the detail is asked with `scope=global`
- **THEN** the answer is the global definition

#### Scenario: An unknown scope over HTTP
- **WHEN** a detail or fit request names `scope=team`
- **THEN** the answer is 400 "scope must be project or global"

### Requirement: An unreadable origin is no project evidence
A run whose journal does not read SHALL count for no project candidate comparison, SHALL trigger no candidate decision and SHALL NOT be patched, and a manifest's trial mark SHALL NOT let a global run settle a project candidate.

#### Scenario: A torn journal
- **WHEN** a global run's journal ends in a torn line
- **THEN** the run is left out of the project candidate's comparison and summary

#### Scenario: A trial mark written by an older apb
- **WHEN** a global run whose manifest carries the trial mark finishes
- **THEN** the project's candidate pointer, trial record and `current` are unchanged
