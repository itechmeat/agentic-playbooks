## ADDED Requirements

### Requirement: One active and one waiting run per key
For a top-level production run of a playbook with `concurrency:`, the engine SHALL render the key once at start from params and SHALL keep at most one active and at most one waiting run per key in the project. A newer start SHALL replace the waiting run, also when the waiting run was not yet promoted after the active run ended, and the replaced run SHALL end `run_aborted` with a `superseded:` reason whatever the timing of the stop.

#### Scenario: The newest start replaces the waiting one
- **WHEN** run A holds key `acme` with `on_busy: queue_latest`
- **AND** runs B, C and D start in turn with key `acme`
- **THEN** B and C end `run_aborted` with a `superseded:` reason
- **AND** D runs after A ends

#### Scenario: A late waiter is not overtaken
- **WHEN** A ended and its waiter B has not yet looked at the slot
- **AND** C starts with key `acme`
- **THEN** C runs and B ends `run_aborted` with a `superseded:` reason without running a node

### Requirement: Supersede and refuse
With `on_busy: supersede` a new start SHALL stop the active run through the graceful stop and wait for it to end before it runs. With `on_busy: refuse` a start whose key is held SHALL be refused `concurrency_busy` naming the holder.

#### Scenario: A stale run is replaced
- **WHEN** run A holds the key and its agent traps SIGTERM
- **AND** run B starts with the same key and `supersede`
- **THEN** A's agent receives SIGTERM before it is killed
- **AND** A ends aborted `superseded:` and B then runs

#### Scenario: Concurrent refusals
- **WHEN** eight starts with one key race under `refuse` while the first admitted run is still running
- **THEN** exactly one is admitted and seven are refused `concurrency_busy`

### Requirement: Key identity and display
Ownership SHALL be decided by a keyed MAC of the rendered key, journaled on `run_provenance.concurrency` with a scrubbed, clipped display text that is never compared. A key that reads a value classified sensitive SHALL refuse the start `concurrency_key_sensitive` without the value in any file or message.

#### Scenario: Two keys with the same display
- **WHEN** two rendered keys scrub to the same display text
- **THEN** they hold separate slots

### Requirement: The key before the tree, for starts and resumes
A keyed start SHALL take no working-tree lock in preparation and SHALL wait for the tree only after it holds its key; a run waiting for its key SHALL hold no working-tree lock. A resume SHALL reserve the run's slot before the tree lock, keep it through `run_resumed`, and give it back when the resume is refused; a second resume of the same run while one is in progress SHALL be refused.

#### Scenario: The holder is not blocked by its waiter
- **WHEN** run B waits for key holder A
- **THEN** A's next node takes the tree lock and runs

#### Scenario: A keyed start that shares the tree
- **WHEN** a keyed start with a free key and `--allow-shared-workdir` begins while another run holds the tree
- **THEN** it runs beside that run without waiting for the tree

#### Scenario: Two starts meet at the key
- **WHEN** holder A holds the tree and starts B and C of the same key with `queue_latest` are prepared without a tree wait
- **THEN** neither is refused `WorkdirBusy`
- **AND** C runs after A and B ends superseded

#### Scenario: A resume races a new start
- **WHEN** a failed run of key k is resumed while a new start of k is admitted
- **THEN** exactly one of them is active

### Requirement: A provisional claim belongs to a live owner
A claim SHALL stay provisional while its owner (the preparing or resuming process, or the waiter) holds the claim's owner lock, whatever the age of the claim and whatever its run directory holds. A provisional claim whose owner is gone SHALL be confirmed when its run's start or resume is durable and cleared otherwise.

#### Scenario: A slow preparation keeps its key
- **WHEN** start A was admitted and its preparation is still running with no `run_started` yet
- **AND** start B of the same key with `on_busy: refuse` is prepared
- **THEN** B is refused `concurrency_busy` naming A
- **AND** A runs to its end

#### Scenario: A crash before provenance
- **WHEN** the owner of a provisional claim is gone and its run never recorded `run_provenance`
- **THEN** the next start of the key is admitted

### Requirement: Promised stops survive a crash and wait for a provisional claim
Stops a claim promises SHALL be recorded before they are posted, posted only after the slot's transaction ended, and posted by the next access when a crash intervened. A stop aimed at a provisional claim SHALL wait until that claim is confirmed or cleared.

#### Scenario: A crash after a supersede was promised
- **WHEN** the process crashes after recording that waiter W must stop and before posting it
- **THEN** the next start of the key posts the stop and W ends aborted

#### Scenario: A supersede of a start still preparing
- **WHEN** start A holds the key provisionally and has not journaled `run_provenance`
- **AND** start B with `on_busy: supersede` promises A's stop and confirms first
- **THEN** the promised stop stays recorded and A ends aborted once it confirms

#### Scenario: A supersede during a resume
- **WHEN** a failed run F is being resumed and has reserved its key but not yet journaled `run_resumed`
- **AND** a start with `on_busy: supersede` promises F's stop
- **THEN** F ends `run_aborted` with a `superseded:` reason after its resume
- **AND** the new start runs after it

#### Scenario: A slow notifier does not block the key
- **WHEN** a promised stop ends a dead run whose health notice waits on a slow connector
- **THEN** another start of the playbook is admitted without waiting for that notice

### Requirement: Signed slot state
The slot file SHALL be a file of the signed store; a file that does not verify SHALL refuse starts and resumes of the playbook `concurrency_state_unreadable`. A slot holder whose run is not this installation's run of the same playbook, scope and key digest SHALL be void.

#### Scenario: A forged holder
- **WHEN** a signed slot names a run of another playbook as active
- **THEN** a new start of the key is admitted

### Requirement: A queued routine reads its window when admitted
A queued run that reads `run.previous_success.*` SHALL resolve its window after it became the key's active run and before its first node, and SHALL keep it on resume. A start admitted without waiting SHALL keep the window its preparation read, whichever process drives it.

#### Scenario: The window moves while waiting
- **WHEN** B queues behind A and A succeeds
- **THEN** B's prompt renders A as its previous success

#### Scenario: A detached start admitted at once
- **WHEN** a detached keyed start is admitted at once, and a run of another key succeeds before its driver begins
- **THEN** the run keeps the window its preparation read

### Requirement: Guard, patches, children and validation
The run SHALL carry the guarded feature `concurrency`; a supervisor patch that changes the block SHALL be refused. A sub-playbook child SHALL not take a slot. The validator SHALL report V210 (error) for a key that reads anything but declared non-secret `params.*` or is empty or over 500 characters, and V211 (warning) for `supersede` on a playbook with irreversible effects.

#### Scenario: A secret param in the key
- **WHEN** the key reads `{{params.token}}` and `token` is `secret: true`
- **THEN** validation fails with V210

### Requirement: A run never waits for or replaces the run that started it
A start SHALL be refused `concurrency_own_run`, whatever `on_busy` says, when the key's active or waiting run is the run step's own run, one of its parents, or a run in its chain of step-started runs in this project.

#### Scenario: A step starts its own playbook with its own key
- **WHEN** run A holds key `acme` and a step of A starts the same playbook with key `acme`
- **THEN** the start is refused `concurrency_own_run`
- **AND** A goes on and ends on its own

### Requirement: A refused start leaves nothing behind
A start refused `concurrency_busy`, `concurrency_own_run` or `concurrency_key_sensitive` SHALL leave no run directory, and a start key it carried SHALL stay free for a later start of the same event.

#### Scenario: A keyed start refused while the key is busy
- **WHEN** run A holds key `acme` with `on_busy: refuse`
- **AND** a start with key `acme` and start key `e1` is refused `concurrency_busy`
- **THEN** no run directory was created for it
- **AND** after A ends, the same start with start key `e1` runs

### Requirement: A waiting run keeps its deadline
A run waiting for its key SHALL end `run_aborted` with a `deadline_exceeded` reason when its deadline passes while it waits and the deadline stop is due, and SHALL keep waiting when it is not due.

#### Scenario: The deadline passes in the key queue
- **WHEN** run B with a deadline of two seconds waits behind holder A for longer than that
- **THEN** B ends `run_aborted` with a `deadline_exceeded` reason
- **AND** the slot no longer names B

### Requirement: A waiting run is driven by its waiting process
A run waiting for its key SHALL be seen as driven by its waiting process, so a stop or a pause of it is posted to that process; a posted pause SHALL be applied at the run's first boundary after it got its key. A waiting run whose waiting process is gone SHALL be released from the slot and end aborted with a `superseded:` reason.

#### Scenario: A waiter lost after a reboot
- **WHEN** run W waits for key `acme` and its driver process is gone
- **THEN** the next access to the slot releases W and W ends `run_aborted` with a `superseded:` reason

#### Scenario: A pause of a waiting run
- **WHEN** a person pauses run B while it waits for key holder A
- **THEN** the pause is posted, B takes the key after A ends and pauses at its first boundary

### Requirement: A waiting run starts its supervisor after its turn
A supervisor agent of a run that waits for its key SHALL be started only after the run got its key.

#### Scenario: A supervised run in the key queue
- **WHEN** a run that expects a supervisor agent waits behind holder A
- **THEN** no supervisor is started for it until A ended and it got the key

### Requirement: Slot holders are read strictly
A slot holder whose journal does not read strictly (a torn tail, an unknown event after the last checkpoint) SHALL be kept, and a holder whose recorded key digest differs from the slot's SHALL be void.

#### Scenario: An unreadable holder
- **WHEN** the active run's journal ends with an unknown event type and no checkpoint after it
- **THEN** a new start of the key queues behind it
