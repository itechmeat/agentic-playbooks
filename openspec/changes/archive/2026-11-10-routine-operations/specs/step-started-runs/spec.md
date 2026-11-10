## ADDED Requirements

### Requirement: The new run names its starter
A new start made from inside a run step (CLI or MCP, an agent, a script or a host task's subagent) SHALL record `started_by` (the starting run, its workspace, node, attempt number, a verified flag and the chain depth) in the new run's launch record. The attempt token SHALL never be stored.

#### Scenario: An agent starts a run
- **WHEN** an agent of run T, node `triage`, attempt 2 starts playbook `fix`
- **THEN** the new run's manifest names T, `triage` and attempt 2, verified

#### Scenario: A forged run id
- **WHEN** `APB_RUN_ID` names a finished run, or `APB_RUN_DIR` is a link
- **THEN** the record is not verified and its chain is 1

#### Scenario: A host task adopted after a resume
- **WHEN** a run's host task stayed open across a pause and resume
- **AND** its subagent, with the environment of the attempt that first opened the task, starts a run
- **THEN** the record is verified and carries the starting run's chain

### Requirement: Reverse links come from the records
The starting run's surfaces SHALL list the runs its steps started in the same project, read from the verified `started_by` records of runs created by this installation. Nothing SHALL be written into the starting run's journal.

#### Scenario: Both directions
- **WHEN** run T started run F from a step, in the same project
- **THEN** `apb runs T` lists F under started runs
- **AND** `apb runs F` names T as its starter
- **AND** T's journal holds no event about F

### Requirement: Reverse links are bounded and stay in their project
The reverse list SHALL come from a bounded scan of the project's newest runs; when the project holds more runs than the scan bound, every surface SHALL say the list may be incomplete. Runs started in another project SHALL not be listed; their own surfaces SHALL name the starter and its workspace, and a link SHALL lead to the starter only within its own workspace.

#### Scenario: A truncated scan
- **WHEN** more runs than the scan bound were started in the project after T
- **THEN** `apb runs T` says its started runs may be incomplete

#### Scenario: A starter in another workspace
- **WHEN** a step of run T in project A started run F in project B
- **THEN** `apb runs F` in B names T and A's workspace
- **AND** the dashboard shows T as text, not as a link to a run of B

### Requirement: Chains are bounded across projects
A new start from a run step whose chain depth would exceed 5 SHALL be refused `start_chain_too_deep` on every surface; a starter in another registered project SHALL keep its chain. Resumes SHALL never be refused for their chain, and sub-playbook nesting SHALL not count toward it.

#### Scenario: The sixth start
- **WHEN** five runs each started the next from a step, alternating between two projects
- **AND** a step of the fifth starts another run
- **THEN** it is refused `start_chain_too_deep`

### Requirement: A step of a flagged run starts a flagged run
A new start from a run step whose run has `external_data: true` SHALL be flagged `external_data: true`, as a sub-playbook child or a `continued_from` successor of a flagged run is, on every new-start surface (a run, a trial, an executed plan), decided before the start's gate; an explicit `external_data: false` on such a start SHALL be refused `external_data_immutable`. A step whose run is not found SHALL pass no flag on.

#### Scenario: A flagged starter
- **WHEN** a step of run T, started with `external_data: true`, starts playbook `fix` without the flag
- **THEN** the new run's manifest records `external_data: true`

#### Scenario: A trial and an executed plan from a flagged step
- **WHEN** a step of a flagged run starts a trial, or executes a plan, without the flag
- **THEN** the new run's manifest records `external_data: true`

#### Scenario: An explicit false under a flagged starter
- **WHEN** a step of a flagged run starts a run with `external_data: false`
- **THEN** the start is refused `external_data_immutable`

### Requirement: Children are not marked
A sub-playbook child of a run started from a step SHALL have no `started_by`.

#### Scenario: A child of a step-started run
- **WHEN** a step-started run runs a `playbook` node
- **THEN** the child's launch record has no `started_by`
