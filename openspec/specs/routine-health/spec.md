# routine-health Specification

## Purpose
TBD - created by archiving change routine-health. Update Purpose after archive.

## Requirements

### Requirement: Health rules across runs
A playbook's `health:` block SHALL be evaluated over its top-level production and external data runs created on this machine whose journals read strictly and the verified summaries of pruned ones of the same origins, with the rules `consecutive_failures`, `failure_rate` and `overdue`. A run a person stopped, a superseded run and an unfinished run SHALL not count, and a pruned run SHALL keep the class its live journal gave.

#### Scenario: Three failed nights
- **WHEN** `consecutive_failures: 3` and the third failed run ends
- **THEN** the rule is tripped when that run's end returns

#### Scenario: A person's stop is not a failure
- **WHEN** two runs failed and the newest was stopped by `apb stop`
- **AND** `consecutive_failures: 3`
- **THEN** the rule is not tripped

#### Scenario: An older evaluation finishes last
- **WHEN** an evaluation read two failures, the third failed run ends and its evaluation trips the rule
- **THEN** the older evaluation does not clear the trip or record a recovery

#### Scenario: An evaluation that waited measures at its own time
- **WHEN** an evaluation is called before an `overdue` threshold passes and waits for the evaluation lock while a later one trips the rule
- **THEN** the waiting evaluation measures at the time it holds the lock and does not record a recovery

#### Scenario: A removal undone while a run is read
- **WHEN** a listed run directory is moved away while its fact is read and moved back with its summary deleted
- **THEN** the run is read again from its directory and counted once

#### Scenario: Runs that cannot be read
- **WHEN** the runs of a playbook cannot be read (the archive is a link, a removal holds it too long), also when a run found gone is looked up again
- **THEN** the evaluation fails and the health state is left as it was

### Requirement: What the first evaluation and a clear count
The first evaluation of a playbook SHALL count every run there is; after a person's clear only runs started since the clear SHALL count. A start that failed in preparation before its provenance was recorded SHALL not count.

#### Scenario: A new routine's first failures count
- **WHEN** a playbook with `consecutive_failures: 3` has no health state yet and its first three runs fail
- **THEN** the rule is tripped when the third run's end returns

#### Scenario: A failure before the run was set up
- **WHEN** a start fails in preparation before its provenance was recorded
- **THEN** it is not counted
- **AND** `overdue` still trips when no run succeeds within its time

#### Scenario: A new routine gets its grace
- **WHEN** `overdue: 26h` and the playbook has no run yet
- **THEN** the first evaluation does not trip
- **AND** an evaluation 26 hours and a minute later trips

### Requirement: One notice per trip, claimed by one sender
A trip SHALL send one notice through the machine's notify target when its `notify.on` covers `health`. The notice SHALL be recorded with its text and target pins in the playbook's signed ledger, claimed by one sender, and settled sent, not sent or unknown from the connector call's class. A notice whose outcome is unknown, or whose claim is older than 60 seconds, SHALL never be sent again.

#### Scenario: Simultaneous evaluations
- **WHEN** a run end, `apb health check` and the dashboard sweep evaluate one tripped rule at once
- **THEN** exactly one notice is sent

#### Scenario: A drifted target
- **WHEN** the target's account changed after the notice was recorded
- **THEN** the notice is not sent

### Requirement: Recovery and cooldown
A recovery notice SHALL follow only a delivered trip notice and SHALL not be sent while muted. No trip notice of a rule SHALL go out within its `cooldown` of the rule's last delivered trip notice, and a delivery time SHALL survive a recovery; a trip still open when the cooldown ends SHALL be notified then.

#### Scenario: A trip again within the cooldown
- **WHEN** a trip notice was delivered, the rule recovered and it trips again an hour later with `cooldown: 24h`
- **THEN** no trip notice goes out before 24 hours after the first delivery

### Requirement: Mute
No notice SHALL be claimed, retried or dispatched while the playbook is muted, and a trip still open when a mute ends SHALL be notified then.

#### Scenario: A trip while muted
- **WHEN** a rule trips while the playbook is muted
- **AND** a person unmutes it while the rule is still tripped
- **THEN** the next evaluation sends the trip notice

#### Scenario: A failed notice and a mute
- **WHEN** a trip notice failed once and a person mutes the playbook
- **THEN** it is not retried while muted

### Requirement: Clear and mute fence claimed notices
A clear, a mute or an unmute SHALL retire every notice not yet in a final state. A claimed notice SHALL be dispatched only when no clear, mute or unmute happened since its claim, and the outcome of a request already dispatched before one SHALL change no rule state.

#### Scenario: A clear while a notice is about to be sent
- **WHEN** a sender has claimed a trip notice and a person clears the playbook before it dispatches
- **THEN** the notice is not sent

#### Scenario: A clear while a notice is in flight
- **WHEN** a trip notice was dispatched and a person clears the playbook before its outcome is settled
- **THEN** the outcome records no delivery time on the cleared rules

### Requirement: Hold refuses starts until a person clears it
With `on_trip: hold` a tripped playbook SHALL be refused `playbook_on_hold` by the run gate and again by preparation of every top-level production start that is not a trial, until a person runs `apb health clear`. A clear SHALL start the count and the `overdue` reference over and retire outstanding notices. Clear, mute and unmute SHALL be refused inside a run step.

#### Scenario: A hold after the gate
- **WHEN** a hold is set after a start passed the gate and before it is prepared
- **THEN** the start is refused

#### Scenario: A clear starts over
- **WHEN** a person clears a playbook held for `overdue`
- **THEN** an evaluation right after is not tripped

### Requirement: Signed health state
The health state SHALL be a file of the signed store; a state file that does not verify SHALL read as held with the detail `health state unreadable`, and only a person's clear SHALL replace it.

#### Scenario: A forged clear
- **WHEN** the state file of a held playbook is rewritten without a valid MAC
- **THEN** starts are still refused `playbook_on_hold`

### Requirement: Evaluation points and surfaces
Health SHALL be evaluated after every terminal event of a top-level run is durable, whichever code path wrote it, by `apb health check`, by `apb resume --all` and by the dashboard every 10 minutes. `apb health check` SHALL exit 0 when a start would be admitted and 3 when the playbook is on hold. The state SHALL show in `apb health`, `apb doctor`, the dashboard playbook list, MCP `playbook_catalog` and `playbook_stats`, and `apb stats`.

#### Scenario: The scheduler asks first
- **WHEN** the playbook is on hold
- **THEN** `apb health check digest` exits 3

#### Scenario: Health without runs
- **WHEN** a watched playbook is overdue and has no run left
- **THEN** `apb stats` shows its health line

### Requirement: Machine narrowing and validation
`policy.yaml` `health.playbooks` SHALL narrow which playbooks a machine or project watches (empty: none). The validator SHALL report V212 (error) for a malformed `health:` block.

#### Scenario: A laptop without a timer
- **WHEN** the machine sets `health.playbooks: []`
- **THEN** no health notice is sent and `apb health` reports the playbook unwatched

### Requirement: Deadline aborts, superseded runs and external data runs
A run the engine ended at its deadline (`deadline_exceeded`) SHALL count as a failure; a run ended `superseded:` SHALL never count. A top-level run started with `external_data: true` SHALL count like a production run, and its health facts SHALL hold only its run id, its class and its times.

#### Scenario: Three runs past their deadline
- **WHEN** `consecutive_failures: 3` and three runs in a row end `run_aborted` with a `deadline_exceeded` reason
- **THEN** the rule is tripped

#### Scenario: Superseded runs are not failures
- **WHEN** `consecutive_failures: 3`, two runs failed and the two newest ended `superseded:`
- **THEN** the rule is not tripped

#### Scenario: A routine over external data
- **WHEN** every run of a routine is started with `external_data: true` and the newest three failed
- **AND** `consecutive_failures: 3`
- **THEN** the rule is tripped
