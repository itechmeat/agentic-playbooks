## ADDED Requirements

### Requirement: A long timer wait posts an engine pause
A timer wait in a run that may release (top level, no eval, not supervised, not unattended, more than `release_after` left) and whose latest run guard names `wait_park`, with no other frontier node, no open deferred retry and no control entry pending after the drive's cursor, SHALL post a `pause` control entry by `engine` and SHALL NOT journal, lock or spawn anything itself.

#### Scenario: A six hour wait
- **WHEN** a write run reaches a timer wait of 6 hours
- **THEN** the journal holds `run_paused` by `engine` with reason `wait (<node>)`, `until` 6 hours ahead and `keep_lock` true, the drive having held the tree's busy lock
- **AND** the control cursor stands at the pause's control entry
- **AND** the tree's lock names the run

#### Scenario: A short wait
- **WHEN** a timer wait has 30 seconds left
- **THEN** the driver keeps waiting

#### Scenario: A webhook wait
- **WHEN** a run reaches a webhook wait of 6 hours
- **THEN** the driver keeps waiting

#### Scenario: An unattended run
- **WHEN** an unattended run reaches a timer wait of 6 hours
- **THEN** the driver keeps waiting and the journal holds no `run_paused`

#### Scenario: A run without the guard
- **WHEN** a run whose latest run guard does not name `wait_park` reaches a timer wait of 6 hours
- **THEN** the driver keeps waiting

#### Scenario: A control entry is pending
- **WHEN** a control entry the scan did not consume is pending after the drive's cursor at a timer wait of 6 hours
- **THEN** no pause is posted for the wait

#### Scenario: A node id that is not a safe segment
- **WHEN** a run reaches a timer wait of 6 hours on a node whose id is not a safe segment, such as `deploy/prod`
- **THEN** no pause is posted for the wait and the driver keeps waiting

#### Scenario: A discarded park is not posted on every poll
- **WHEN** the scan discarded the park the drive posted and nothing changed since
- **THEN** the drive posts no new park before a new event, a consumed control entry or a fixed backoff

### Requirement: The park's pause is an ordinary engine pause
The park's pause SHALL carry the reason `wait (<node>)`, mode `drain`, `until` at the wake time and `keep_lock` true exactly when the drive holds a busy lock of the working tree, and false otherwise (a read-only run, or a write run started with a shared working directory, which takes no lock). Once the control scan applies it, it SHALL be journaled as every engine pause is. No new event type or event field SHALL be written.

#### Scenario: The journaled pause
- **WHEN** a read-only run reaches a timer wait of 6 hours
- **THEN** the journal holds `run_paused` by `engine` with reason `wait (<node>)` and `keep_lock` false

#### Scenario: A write run on a shared working directory
- **WHEN** a write run started with a shared working directory reaches a timer wait of 6 hours
- **THEN** the journal holds `run_paused` by `engine` with `keep_lock` false and no lock file exists for the tree

#### Scenario: Any safe node id round-trips
- **WHEN** the waiting node's id holds a comma and parentheses, such as `a, b (c)`
- **THEN** the reason is `wait (a, b (c))` and every surface names the node `a, b (c)`

### Requirement: Control entries are numbered in append order
Every writer of a run's control file in this apb and later SHALL take its entry's sequence number and append the entry, with its newline in one write, under one lock, so that no two entries such writers append share a number and every such entry carries a higher number than every entry appended before it. The lock SHALL be taken after the run's event lock by a caller that holds that lock, and never before it. Readers SHALL keep reading without the lock.

#### Scenario: Two posts at once
- **WHEN** two writers post a control entry to the same run at the same moment
- **THEN** the two entries carry distinct numbers, in the order the file holds them

#### Scenario: A park, then a pause
- **WHEN** a park is posted and a person's pause is posted to the same run right after it
- **THEN** the two entries carry consecutive numbers, the person's `run_paused` is journaled and no `run_paused` with the wait reason is

#### Scenario: A pause, then a park
- **WHEN** a person's pause is posted and a park is posted to the same run right after it
- **THEN** the person's `run_paused` is journaled, no `run_paused` with the wait reason is, and the control cursor stands past the park

### Requirement: A stop is never refused for the control lock
A stop that cannot take the control file's numbering lock within its bounded wait SHALL be appended without it, numbered above every entry it reads. It SHALL be the only writer allowed to do so; every other writer SHALL keep the bounded wait and its error.

#### Scenario: A stop while the lock is held
- **WHEN** another process holds the control lock longer than its bounded wait while a person stops the run
- **THEN** the stop is posted and numbered above every entry, and any other writer gets the lock's error

### Requirement: An older apb's control writes are not ordered
The numbering of control entries SHALL be claimed only for writers of this apb and later. A writer of an older apb takes no control lock, and two apb versions posting to one live run at once SHALL remain unsupported, as on main for every control.

#### Scenario: An older apb posts to a live run
- **WHEN** an apb older than this change posts a control entry while a drive of this apb runs the same run
- **THEN** the design makes no claim about that entry's number or order, and the documentation states that this use is not supported

### Requirement: A park yields to every other control
The control scan SHALL apply a park only when it is the last pending entry of the scan's pass, no other entry was applied in that pass, it names the node the drive is at, and no entry is pending after it when checked again under the run's event lock. Otherwise the scan SHALL discard the park, moving the control cursor past it and journaling nothing for it, and SHALL go on with the next entry. A discarded park SHALL NOT stop the admission of other work.

#### Scenario: A pause posted after the park
- **WHEN** a person's pause is pending after the park's entry
- **THEN** the person's `run_paused` is journaled, no `run_paused` with the wait reason is journaled
- **AND** the wait's timer does not run while the person's pause holds

#### Scenario: A stop posted after the park
- **WHEN** a stop is pending after the park's entry
- **THEN** the run ends aborted and no `run_paused` with the wait reason is journaled

#### Scenario: A patch before the park
- **WHEN** a patch is pending before the park's entry
- **THEN** the patch is applied, the park is discarded, and the run decides its wait again on the patched definition

#### Scenario: A park for another node
- **WHEN** a pending park names a node other than the one the drive is at
- **THEN** it is discarded and nothing is journaled for it

#### Scenario: The event lock cannot be taken
- **WHEN** the run's event lock stays held past its bounded wait while the scan decides a park
- **THEN** the park is discarded, nothing is journaled for it, and the drive goes on

### Requirement: A stale park is discarded
Under the run's event lock, before a pending park is applied, the control scan SHALL compute the wait's due time again on the current clock, and SHALL discard the park by the same rule when its wake time no longer equals that due time.

#### Scenario: A pause journaled directly while a park is pending
- **WHEN** the driver posted a park and died before applying it, a person's pause found no live driver and was journaled directly, and the person later resumes the run
- **THEN** the park is discarded because its wake time no longer equals the wait's due time, nothing is journaled for it, and the wait parks again with a wake time that leaves out the person's paused time

### Requirement: No park is left behind the end of a pass
Every exit of a scan pass that consumed an entry (an abort, a pause, an applied patch, a patch that paused the run, a salvaged stop) SHALL discard a park that is the entry right after it, in the same cursor write. A pending park SHALL NOT be reported as a pause in progress.

#### Scenario: A patch that pauses the run
- **WHEN** a patch pending before the park's entry meets the run's patch limit, so the run pauses without applying it, and a person resumes the run after the park's wake time
- **THEN** no `run_paused` with the park's old wake time is journaled, and the wait decides again with its remaining time

#### Scenario: A pause posted before the park
- **WHEN** a person's pause is pending right before the park's entry
- **THEN** the person's `run_paused` is journaled, the park is discarded in the same cursor write, and a later resume never applies the park's old wake time

#### Scenario: A pending park is not pausing
- **WHEN** a park's entry is pending while the drive still runs
- **THEN** the run's surfaces do not report the run as pausing

### Requirement: The waker of a park wakes the run for a pending control
The pause waker of an applied park SHALL look for a pending control entry on every poll of its loop while the park holds the run, and on the first it finds SHALL resume the run once and end. It SHALL NOT interpret, apply or move any control entry itself; the resumed drive's first scan applies the entries in order. When that resume is refused, the entries SHALL stay pending as for any paused run.

#### Scenario: A control posted in the exit window
- **WHEN** a stop is posted after the park was applied and before the driver exited
- **THEN** the park's waker resumes the run at its first poll, long before `until`, and the resumed drive applies the stop

#### Scenario: A note posted while the run is parked
- **WHEN** a note is posted an hour into a six hour park, after the waker settled
- **THEN** the run is resumed within one waker poll, the note is applied, and the run parks again for the time left

### Requirement: A failed read does not end the waker of a park
A read of the control file or its cursor by the waker of a park that fails (a line that does not parse, an I/O error) SHALL NOT end the waker: it SHALL count as nothing pending for that poll, SHALL be read again at the next poll, and SHALL be logged at most once per waker. It SHALL NOT keep the waker from resuming the run at `until`.

#### Scenario: A half-written control line
- **WHEN** the waker polls while a note's line in the control file is only partly written
- **THEN** the waker keeps running, and once the line is whole it resumes the run at its next poll and the note is applied

### Requirement: A park holds no driver
Once the park's pause is applied, the driver SHALL exit, and while the run is parked no driver process and no agent SHALL run for it. The processes left for the run SHALL be those every pause with `until` leaves: the pause waker, and the deadline waker of a run with a deadline.

#### Scenario: What runs while parked
- **WHEN** a write run is parked on a timer wait of 6 hours and the hand-over to the waker has settled
- **THEN** `driver.pid` is gone and no driver process of the run is alive
- **AND** exactly one pause waker of the run is alive

### Requirement: A parked run wakes when its wait is due
A parked run SHALL wake at its `until` through its pause waker, or through `apb resume --all` when no waker lives. The resumed drive SHALL finish the same wait without starting it again and without a second `node_started`. A pending park entry left by a driver that crashed before applying it SHALL be applied by the next drive's first control scan.

#### Scenario: The waker resumes the run
- **WHEN** a parked wait reaches its `until`
- **THEN** the run is resumed by `engine` and the wait ends as due

#### Scenario: The machine slept
- **WHEN** no waker lives and the wake time passed
- **THEN** `apb resume --all` resumes the run and the wait ends

#### Scenario: A crash before the pause was applied
- **WHEN** the park's control entry is pending after the cursor, the driver is dead, and nothing was journaled since
- **THEN** the drive that `apb resume --all` starts journals the park's `run_paused` at its first control scan while its wake time is still ahead, and otherwise discards it and ends or parks the wait afresh

### Requirement: Resuming a parked run
A resume before `until` SHALL re-enter the open wait, which posts a new park for the time left; a resume at or after `until` SHALL end the wait as due. A retry-now of a parked run SHALL be refused `no_deferred_retry`.

#### Scenario: An early resume
- **WHEN** a person resumes a parked run an hour before its `until`
- **THEN** the run parks again with the same wake time within one poll

#### Scenario: A pause before the wake
- **WHEN** a 2 hour wait parks, a person pauses the run 30 minutes later, and resumes it 1 hour after that
- **THEN** the run parks again and the wait ends 90 minutes after the resume

### Requirement: A run that can park is guarded
A run whose definition has a timer wait SHALL carry a run guard naming `wait_park` from its start, written with the run's other guarded features right after `run_provenance` and before any node runs, and kept after every checkpoint by the event log. A patch SHALL keep or drop the name as the patched definition has or lacks a timer wait.

#### Scenario: The guard precedes the park
- **WHEN** a write run with a timer wait of 6 hours parks
- **THEN** a `run_guard` naming `wait_park` is in the journal before the park's `run_paused`, and another follows that `run_paused` in the same write

#### Scenario: A run without a timer wait journals as main
- **WHEN** a run whose definition has no timer wait (a script node, then a webhook wait that times out) runs to its end
- **THEN** its journal holds no `run_guard`, no `run_paused` and no `run_resumed`, its run has no control entry, and its sequence of event types is the one main writes for the same definition

#### Scenario: A short timer wait adds only its guard
- **WHEN** a run whose timer wait is shorter than the release threshold runs to its end without parking
- **THEN** its journal with every `run_guard` line removed is the one main writes for the same definition, and each `run_guard` names exactly `wait_park` and stands right after `run_provenance` or right after a checkpoint

### Requirement: An older apb does not resume a parked run
An apb that does not know `wait_park` SHALL refuse to resume or pause a parked run while the park is the run's open state, and SHALL still read and stop it. This is the run guard contract of every guarded feature. Its limit SHALL be documented, not closed: after such an apb stopped the run, a person's explicit resume with that apb counts the park as paused time, so the timer waits again and a run deadline may end the run.

#### Scenario: An older apb
- **WHEN** an apb that does not know `wait_park` is asked to resume or pause a parked run
- **THEN** it refuses as for a run written by a newer apb
- **AND** it still lists the run and can stop it

#### Scenario: A stop by this apb
- **WHEN** this apb stops a parked run and an apb that does not know `wait_park` is then asked to resume it
- **THEN** that apb refuses as for a run written by a newer apb

### Requirement: A park is not paused time
The paused-interval clock SHALL count a park (an engine pause whose reason is `wait (<node>)`) as active time for every reader of paused time: the wait's clock, deferral due times, durations, estimates, statistics, the retro, progress and next-action deadlines. A person's pause that replaces a park SHALL be paused time from its own `run_paused` on. The run deadline SHALL count a park as it counts every engine pause.

#### Scenario: The node's duration
- **WHEN** a 2 hour wait parks and wakes on time
- **THEN** `apb stats` reports the wait node's duration as about 2 hours

#### Scenario: A quota pause stays paused time
- **WHEN** a run holds a quota pause and a park
- **THEN** only the quota pause is paused time

### Requirement: A park is not an intervention
The intervention counts of `apb stats` and the retro SHALL count neither a park nor the engine's resume of it, and SHALL count a person's or a supervisor's resume of a park as theirs.

#### Scenario: No intervention
- **WHEN** a run parks twice, the waker resumes the first park and a person resumes the second
- **THEN** the intervention counts hold one resume by the person and no engine pause or resume

### Requirement: Surfaces show a park as a wait
A park SHALL keep the journal status `paused` and SHALL carry the cause `wait`, derived from the reason of an engine pause and never journaled; a person's or a supervisor's pause with the same reason SHALL NOT carry it. `apb runs`, `apb wait`, MCP `run_status` and `run_wait`, the dashboard, next actions and the HTML report SHALL show "waiting on timer `<node>` until <time>".

#### Scenario: The run list
- **WHEN** `apb runs` lists a parked run
- **THEN** its line reads "waiting on timer `<node>` until <time>"

#### Scenario: The HTML report
- **WHEN** a parked run is exported with `apb runs <id> --html`
- **THEN** its header shows the waiting-on-timer line with the wake time

#### Scenario: A pause reason with a secret in the HTML report
- **WHEN** a paused run whose pause reason quotes a secret value of the run is exported with `apb runs <id> --html`
- **THEN** the header's pause line shows the redaction marker in place of the value

#### Scenario: The run_wait instruction
- **WHEN** MCP `run_wait` returns for a parked run
- **THEN** its instruction says the run waits on a timer and its waker resumes it at `pause.until`

### Requirement: A park sends no paused notice
The notify sweep SHALL NOT send a paused notice for a park.

#### Scenario: No paused notice
- **WHEN** a run with notifications on parks on a timer wait
- **THEN** no paused notification is sent, and no reminder (capability `wait-reminders`)

### Requirement: Next actions follow a park
Next actions SHALL keep the resume action of a park with its automatic progress at `until`, SHALL show the wait's remaining time running, SHALL say the wait is overdue once `until` passed by more than a minute with no waker holding it, and the unattended outcome of a park SHALL be a resumed attempt at `until`.

#### Scenario: Following a park
- **WHEN** `apb wait --follow-auto` follows a parked run
- **THEN** it waits to the park's `until`

#### Scenario: A lost wake
- **WHEN** no waker resumed a park and its `until` passed more than a minute ago
- **THEN** next actions say the wait was due and offer the resume
