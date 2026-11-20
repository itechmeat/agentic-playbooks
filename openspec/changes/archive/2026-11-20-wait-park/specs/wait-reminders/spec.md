## ADDED Requirements

### Requirement: An open wait gets reminders on the person's cadence
With `notify.remind` set in the machine's `policy.yaml`, while the run is driven by an apb of this change or later, an open review, question or host task covered by the run's subscription SHALL get one reminder after each listed duration of active run time, each through the send ledger under its own label `notify:<kind>.<seq>.r<n>`, and never before its notice was confirmed sent.

#### Scenario: Four hours without an answer
- **WHEN** a review notice went out and the gate is still open after 4 hours of active run time under `remind: [1h, 4h]`
- **THEN** a second reminder labelled `.r2` is sent

#### Scenario: No reminders by default
- **WHEN** `policy.yaml` sets no `notify.remind`
- **THEN** no reminder is ever sent

### Requirement: A sub-playbook follows the cadence in force above it
A sub-playbook whose subscription is inherited SHALL be reminded on the cadence and to the target of the subscription in force at the top of its chain of parent runs as it reads at each sweep, not on the copy journaled when the sub-playbook started.

#### Scenario: The parent turns reminders on
- **WHEN** a sub-playbook started while its parent had no reminder cadence, its review notice was confirmed, and the parent's subscription then sets `remind: [10m]` with the same target while the review stays open past ten minutes of active run time
- **THEN** the sub-playbook's reminder `.r1` is sent

#### Scenario: A sub-playbook of a sub-playbook
- **WHEN** the same happens one level deeper, with the top run turning reminders on
- **THEN** the innermost run's reminder `.r1` is sent

### Requirement: A failed reminder is tried again
A reminder whose try failed or was refused for a passing reason SHALL be tried again under the send ledger's retry rule while its threshold is the latest due one.

#### Scenario: A failed reminder is tried again
- **WHEN** the first try of a reminder `.r1` failed and the retry gap has passed while `.r1` is still the latest due threshold under `remind: [1h, 4h]`
- **THEN** `.r1` is tried again under the same label

### Requirement: A reminder never follows a closed wait
No reminder SHALL be sent for a wait that was decided, answered, submitted, withdrawn or ended with the run, and paused time SHALL not count toward a reminder.

#### Scenario: Decided before the reminder
- **WHEN** a gate is decided at 59 minutes under `remind: [1h]`
- **THEN** no reminder is sent

#### Scenario: A pause in the middle
- **WHEN** a gate is open two hours of which the run was paused ninety minutes under `remind: [1h]`
- **THEN** no reminder is due yet

### Requirement: A reminder's openness is decided at send-intent admission
The openness of the wait a reminder follows SHALL be rechecked under the run's journal lock, over the journal as it is then, before any intent of the reminder is appended, a `not_sent` intent included; a reminder whose wait closed before that point SHALL NOT be sent and SHALL record no intent, and a reminder admitted while its wait was open MAY complete after the wait closes and SHALL be recorded under its own label, which names its request.

#### Scenario: Decided between the sweep and the send
- **WHEN** a sweep finds a reminder due and the gate is decided before the reminder's send intent is admitted
- **THEN** no intent is recorded for the reminder's label and no request reaches the target

#### Scenario: Decided while the reminder is being sent
- **WHEN** the gate is decided after the reminder's intent was admitted and while its request is in flight
- **THEN** the reminder settles confirmed under its label and the next sweep sends nothing for that wait

#### Scenario: A reminder that could not go out
- **WHEN** a reminder is due, its notify target no longer resolves, and the gate is decided after the sweep's read and before the reminder's `not_sent` intent would be appended
- **THEN** no intent and no settlement are recorded for the reminder's label

#### Scenario: The subscription narrowed between the sweep and the send
- **WHEN** a sweep finds a review's reminder due and a person's change of the subscription drops `review` before the reminder's send intent is admitted
- **THEN** no intent is recorded for the reminder's label and no request reaches the target

#### Scenario: Two sweeps at once
- **WHEN** two sweeps of the same run find the same reminder due at the same time
- **THEN** exactly one intent is recorded for its label

#### Scenario: Two sweeps at once with a target that does not resolve
- **WHEN** two sweeps of the same run find the same reminder due at the same time and its notify target does not resolve
- **THEN** exactly one `not_sent` intent is recorded for its label

### Requirement: A reminder is admitted only while it is still the one due
Under the same journal lock, before any intent of a reminder is appended, the reminder due as of that journal under the subscription in force SHALL be computed again, and the intent SHALL be appended only when it is the reminder the sweep selected, under the same target and cadence.

#### Scenario: Reminders turned off between the sweep and the send
- **WHEN** two sweeps find a review's reminder due and a person's change of the subscription turns reminders off, with the same target, before either reminder's send intent is admitted
- **THEN** no intent is recorded for the reminder's label and no request reaches the target

#### Scenario: The cadence changed between the sweep and the send
- **WHEN** a sweep finds reminder `.r1` due and a person's change of the subscription sets another cadence before the reminder's send intent is admitted
- **THEN** no intent is recorded for the reminder's label, and the next sweep selects the reminder due under the new cadence

### Requirement: A reminder goes only to the target its notice reached
A reminder SHALL be sent only to the target (connector, account, function, recipient and pinned digests) that the request's notice was confirmed sent to, as its send intent records it. After a target change, an open request SHALL get no reminder at a target its notice did not reach; a request opened after the change SHALL get its notice and reminders at the new target. The label's ledger admission and the target SHALL be rechecked under the journal lock with the openness of the wait.

#### Scenario: A target change while a request is open
- **WHEN** a review's notice was confirmed to target A, the subscription's target is changed to B, and the review stays open past the first reminder time
- **THEN** no reminder is sent to A or to B for that review

#### Scenario: Only the recipient changes as the notice goes out
- **WHEN** a sweep reads target A, the subscription's recipient alone is changed before the notice's intent is admitted, the notice is confirmed to A, and the review stays open past the first reminder time
- **THEN** no reminder is sent to the new recipient

#### Scenario: A notice an older apb recorded
- **WHEN** a review's notice was confirmed with no target digest on its intent and the subscription's target then changes
- **THEN** no reminder is sent for that review

#### Scenario: The next request after a target change
- **WHEN** after that change the first review is decided and a second review is requested and stays open past the first reminder time
- **THEN** the second review's notice and its reminder are sent to B

#### Scenario: Narrowing the subscription keeps the reminders
- **WHEN** a review's notice was confirmed and a person narrows the subscription's transitions without changing its target
- **THEN** the review's reminders are sent as before

### Requirement: Reminders are best effort across binary versions
Reminders SHALL carry no run guard, so an apb older than this change that drives the run sends each notice once and no reminder, and the run is not refused for its reminders. When an apb of this change or later drives the run again, it SHALL send the latest due reminder of each open request once.

#### Scenario: An older apb drives the run
- **WHEN** a run started with `notify.remind: [1h]` waits on a review for two hours of active run time while an older apb drives it
- **THEN** no reminder is sent while the older apb drives it, and the run is not refused because of reminders

#### Scenario: This apb drives the run again
- **WHEN** this apb then drives the same run with the review still open
- **THEN** one reminder, `.r1`, is sent for the review

### Requirement: A re-asked request starts its own reminders
When a gate's request is withdrawn and the gate asks again (a scope change of a form gate, or a supervisor directive), the reminders of the withdrawn request SHALL stop and the new request SHALL get its own notice and its own reminders, counted from the new request.

#### Scenario: A scope re-ask
- **WHEN** a gate's request is withdrawn `scope_changed` after 50 minutes and asked again under `remind: [1h]`
- **THEN** no reminder of the first request is sent and the new request's reminder is due one hour of active run time after it

### Requirement: An outage sends one reminder
After a driver was down past several reminder times, only the latest due reminder SHALL be sent.

#### Scenario: A day down
- **WHEN** the driver comes back 25 hours into an open gate under `remind: [1h, 4h, 24h]`
- **THEN** exactly one reminder, `.r3`, is sent

### Requirement: A project may only turn reminders off
A project's `policy.yaml` SHALL accept `notify.remind: []` to turn reminders off for its runs and SHALL refuse any other value.

#### Scenario: A project cadence
- **WHEN** a project file sets `notify.remind: [5m]`
- **THEN** the file is invalid

### Requirement: A park is never reminded
A run paused by the engine on a timer wait (a park) SHALL get no reminder: a park is not a wait on a person, and no reminded wait is open while a run is parked.

#### Scenario: A long park
- **WHEN** a run with `notify.remind: [1h]` is parked on a timer wait for two hours
- **THEN** no reminder is sent for the run
