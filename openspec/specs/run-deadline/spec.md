# run-deadline Specification

## Purpose
TBD - created by archiving change routine-operations. Update Purpose after archive.

## Requirements

### Requirement: A run ends at its deadline
A top-level production run with a deadline (the playbook's `deadline`, or a run argument that overrides it) SHALL, once its deadline clock reaches the deadline, start no new node, item, batch member or attempt, kill the agent and script processes in flight gracefully, close its open host tasks `cancelled`, stop its child runs, and end `run_aborted` with a reason that starts with `deadline_exceeded`, by `engine`. An attempt it ends SHALL close as aborted, never as paused.

#### Scenario: A digest past its time
- **WHEN** a run with a short deadline still runs an agent at expiry
- **AND** the agent traps SIGTERM
- **THEN** the agent receives SIGTERM
- **AND** the attempt closes `run_aborted`
- **AND** the run ends aborted `deadline_exceeded`

#### Scenario: A host task at expiry
- **WHEN** a host-mode run's deadline passes while a host task is open
- **THEN** the task is closed `cancelled` and the run ends aborted `deadline_exceeded`
- **AND** a later submission for that task is refused as not pending

#### Scenario: A parent waiting on its child
- **WHEN** a parent's deadline passes while its `playbook` node waits on a child
- **THEN** the child ends aborted and the parent ends aborted `deadline_exceeded`

#### Scenario: A child prepared while the parent expires
- **WHEN** a parent's deadline passes while its `playbook` node is still preparing the child
- **THEN** the child ends aborted before any of its nodes starts

### Requirement: No work starts after the deadline
While a run drives, the deadline SHALL be checked wherever work actually starts: every node execution, queued batch member, list item, deferred retry and attempt, and the baseline's reads.

#### Scenario: No work starts after expiry
- **WHEN** a run's deadline passes while batch members and list items still wait to start
- **AND** the deadline watch has not yet acted
- **THEN** no node, item or attempt starts at or after the deadline

### Requirement: A parked or queued run ends at its deadline
The deadline SHALL be enforced while the run is queued for its working tree and while it is parked (paused, released, or paused again by the engine) with no driver, where one deadline waker per run ends it.

#### Scenario: A parked run
- **WHEN** the engine paused a run without `until` and its deadline passes
- **THEN** the run ends aborted `deadline_exceeded` without being resumed

#### Scenario: A released run
- **WHEN** a run was released with no driver and its deadline passes
- **THEN** the run ends aborted `deadline_exceeded`

#### Scenario: A re-pause by the quota probe
- **WHEN** a quota pause's probe paused the run again and the deadline passes during the new pause
- **THEN** the run ends aborted `deadline_exceeded` with no further probe

#### Scenario: A queued run
- **WHEN** a run waits for a tree another run holds and its deadline passes
- **THEN** it ends aborted `deadline_exceeded` without taking the lock

### Requirement: The deadline survives a restart
After a restart of the machine, `apb resume --all` SHALL end every parked run past its deadline and re-arm the deadline waker of every other parked run with a deadline.

#### Scenario: After a reboot
- **WHEN** a run paused without `until` lost its deadline waker and its deadline passed
- **AND** `apb resume --all` runs
- **THEN** the run ends aborted `deadline_exceeded`

### Requirement: The deadline clock
The deadline clock SHALL be wall time since `run_started` minus the instants a person's pause held the run and the intervals it sat ended before a resume; while the run is ended the clock SHALL not advance. The owner of each paused instant SHALL be the latest pause in force at that instant, so a pause replaced by one of another owner without a resume splits there. Engine and supervisor pauses, gate and question waits and queues SHALL count.

#### Scenario: Two pauses
- **WHEN** within 30 minutes the engine paused the run for 10 minutes and a person for 10 minutes
- **THEN** the clock reads 20 minutes

#### Scenario: A person's pause replaced by the engine's
- **WHEN** a person's pause of 10 minutes was replaced, with no resume, by an engine pause of 10 minutes
- **THEN** the engine's 10 minutes count and the person's do not

#### Scenario: An ended run does not age
- **WHEN** a run failed after 10 minutes and a day passed without a resume
- **THEN** the clock reads 10 minutes

### Requirement: Resume and the deadline
Every resume path SHALL apply one deadline check. A resume with `--deadline D` SHALL set the total to the elapsed clock plus D, carried to a detached driver. A person's plain resume of a run past its deadline SHALL be refused `deadline_passed` before anything is journaled or spawned; an engine resume of such a run SHALL end it aborted `deadline_exceeded` without a quota probe.

#### Scenario: More time
- **WHEN** a run with a 30 minute deadline has 40 minutes elapsed
- **AND** a person resumes it with `--deadline 10m`
- **THEN** 10 minutes remain

### Requirement: A resume and the deadline stop are decided together
A resume's deadline check SHALL be decided last on the journal read under the run's event lock, in the same step that appends `run_resumed`, and an engine stop at the deadline SHALL be decided under that same lock.

#### Scenario: A grant and the deadline at the same moment
- **WHEN** a person's resume with `--deadline 10m` is committed while the engine is deciding to stop the run at its deadline
- **THEN** the run is not aborted `deadline_exceeded` after the grant

#### Scenario: Expiry between the check and the commit
- **WHEN** a plain resume passed its early check and the deadline passes before it commits
- **THEN** it is refused `deadline_passed` and no `run_resumed` is journaled

### Requirement: The deadline stays in force
A run with a deadline in force SHALL carry the guarded feature `deadline`, also when the deadline came from a run argument or a resume, and across supervisor patches. A supervisor patch that changes `deadline` SHALL be refused.

#### Scenario: A run-argument deadline is guarded
- **WHEN** a run of a playbook without `deadline` starts with `--deadline 30m`
- **THEN** its journal carries a `run_guard` naming `deadline`

### Requirement: Engine reads stay inside the deadline
A connector read the engine makes on the drive thread for a run's baseline check SHALL be bounded by the run's remaining deadline as well as by its own timeout, so the credential helpers, the token refresh and every page of the read end within it. A read cut short by the deadline SHALL journal no snapshot for its check: the run's surfaces SHALL show the check as not observed, never with a value or as met, and a resume with more time SHALL read it afresh.

#### Scenario: A slow baseline read
- **WHEN** a run with a 3 second deadline reads a baseline connector that answers after 30 seconds, with a 300 second check timeout
- **THEN** the run ends aborted `deadline_exceeded` within seconds of its deadline
- **AND** no snapshot is journaled for the check and it shows as not observed

#### Scenario: A slow credential helper
- **WHEN** the baseline connector's account takes its secret from a command that runs for 20 seconds, and the run's deadline is 3 seconds
- **THEN** the run ends aborted `deadline_exceeded` within seconds of its deadline

#### Scenario: More time after a cut read
- **WHEN** that run is resumed with `--deadline 10m` and the connector now answers at once
- **THEN** the check's before snapshot is journaled as observed

### Requirement: Scope, visibility and validation
Scenario runs SHALL ignore `deadline`: a scenario of a playbook with a `deadline` SHALL journal and report as one of the same playbook without it. Eval runs SHALL keep their own deadline. Every run surface SHALL show the deadline, the remaining time and whether the clock is frozen, and the next actions SHALL carry when the run ends at its deadline. The validator SHALL report V200 (error) for a deadline that is not a duration or lies outside 1 minute to 30 days.

#### Scenario: A paused run
- **WHEN** a person paused a run with a deadline
- **THEN** `apb runs <id>` shows the remaining time as frozen
