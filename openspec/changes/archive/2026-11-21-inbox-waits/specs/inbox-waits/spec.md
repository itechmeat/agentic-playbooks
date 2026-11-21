## ADDED Requirements

### Requirement: A wait for an inbox event
`wait_for: { type: inbox }` SHALL name a connector and an account, and MAY set `match` conditions over body paths, `types` and `take` entries; the node's `timeout_seconds` SHALL bound it. A visit SHALL end at its first matching event or at the timeout. The wait SHALL read the account's stored events and make no connector call.

#### Scenario: Waiting for one customer's reply
- **WHEN** a wait matches `{ path: from, equals: "{{params.customer}}" }` and a reply from that customer is stored
- **THEN** the visit ends with that reply and the node succeeds

#### Scenario: Nothing arrived
- **WHEN** the timeout passes with no matching event
- **THEN** the visit times out and journals how far it examined

### Requirement: The wait is a trusted binding site
Every inbox wait SHALL be the binding site `<node>#inbox` of its connector and exactly its account, granting no function, so the run gate's trust check, the permit and the manifest's connector snapshot cover it. Before a read, the drive and the waker SHALL check that the manifest grant names that account and that the live connector and account digests equal the manifest's and are approved. A visit whose end is journaled SHALL be finished from the journal with no check and no read.

#### Scenario: An unapproved account
- **WHEN** a playbook's inbox wait names an account whose digest is not approved
- **THEN** the start is refused `unapproved_connector_account`

#### Scenario: A changed account on resume
- **WHEN** the account is edited while the wait is parked and the run resumes
- **THEN** the node fails `inbox_wait_unauthorized` and nothing is journaled for the wait

### Requirement: The position lives in the run's journal
A visit SHALL read only events with seq above the `through_seq` of its node's latest `inbox_wait_read` in the run's journal. The node's first visit SHALL journal a start line at the log's head in one write with `wait_started`; a start line not followed by its `wait_started` SHALL be superseded by a new start pair at the current head. The wait SHALL hold no consumer, acknowledge nothing and write nothing outside its run directory.

#### Scenario: Events before the wait
- **WHEN** events 1 to 3 are stored before a wait starts and event 4 after
- **THEN** the start line holds 3 and the visit examines event 4 only

#### Scenario: A torn start line
- **WHEN** a drive stops after the start line and before `wait_started`, and events are stored before the resume
- **THEN** the resumed drive writes a new start pair at the current head and examines only events above it

#### Scenario: Two runs on one account
- **WHEN** two runs wait on the same account and one event matching both arrives
- **THEN** each run ends with it and the account's inbox directory is unchanged

### Requirement: Receipt decides the timeout
An event's receipt SHALL be the moment the store assigns its seq under the store's lock, where `received_at` SHALL be stamped, never below the previous event's. An event SHALL match only when received before the wall instant at which the run's paused-interval clock, measured from the visit's `wait_started`, reaches the wait's timeout. The drive and the park waker SHALL apply the same rule; the first late event SHALL end the examination and stay above the position for the next visit.

#### Scenario: A delivery just before the deadline, polled late
- **WHEN** a matching event is stored 1 s before the deadline and first read 5 s after it
- **THEN** the visit ends with that event

#### Scenario: A delivery inside the read interval before the deadline
- **WHEN** a read succeeds 0.8 s before the deadline and a matching event is stored 0.3 s before it
- **THEN** the visit ends with that event

#### Scenario: A delivery just after the deadline, polled late
- **WHEN** a matching event is stored 1 s after the deadline and first read later
- **THEN** the visit times out and the next visit examines that event

#### Scenario: A delivery waits on the store's lock across the deadline
- **WHEN** a delivery arrives before the deadline but takes the store's lock after it
- **THEN** it is stamped after the deadline and does not end the visit

#### Scenario: A person's pause in between
- **WHEN** a person pauses the run during the wait
- **THEN** the deadline moves by the pause's length

### Requirement: The end is one write
A visit SHALL end by appending, in one write, an `inbox_wait_read` with `through_seq` and `end` (`matched` or `timeout`) and the existing `wait_signalled` or `wait_timeout`. On a match `through_seq` SHALL be the matching event's seq, so a later event stays for the next visit.

#### Scenario: An event after the match
- **WHEN** events 5 and 6 both match
- **THEN** the visit ends with event 5 and the next visit examines event 6

### Requirement: Only declared values are stored
The output SHALL be an object of the event's `seq`, `received_at`, `type` when present, and one value per `take` entry read by the baseline observation reader, `null` when unknown. It SHALL pass the run's storage redactor; no event body or undeclared field SHALL be journaled. When the redactor refuses, the visit SHALL journal nothing for the wait and fail `inbox_wait_secrets_unresolved`.

#### Scenario: An undeclared field
- **WHEN** a matching event carries a marker in a field no `take` entry names
- **THEN** neither the journal nor the node output holds the marker

#### Scenario: A short known secret
- **WHEN** a declared field holds a known secret shorter than 6 characters
- **THEN** it is stored as it is, the limit the docs state

### Requirement: An ended inbox wait is finished from the journal
When an inbox wait's latest visit has an end event and no `node_finished` after it, the resume SHALL finish the node with its journaled output and declare no new visit. Timer and webhook waits SHALL keep main's behaviour, crash included, so a run without an inbox wait SHALL write the journal main writes in every state.

#### Scenario: A crash after the end
- **WHEN** a drive stops after an inbox wait's end pair and before `node_finished`
- **THEN** the resumed drive appends `node_finished` with the journaled output and declares no new visit

#### Scenario: A timer wait crashes after its end
- **WHEN** a drive stops after a timer wait's `wait_signalled` and before `node_finished`
- **THEN** the resumed drive writes the events main writes

### Requirement: A timeout end follows a read
A visit SHALL end by its timeout only after a read made at or after the deadline was examined, or after that read failed twice in a row, in which case the end line SHALL keep the position held before it.

#### Scenario: The final read fails
- **WHEN** the deadline passes and the log cannot be read twice in a row
- **THEN** the visit times out, its end line keeps the earlier position and the next visit examines the stored events

### Requirement: A supervisor patch re-checks an open wait
After a supervisor patch is applied, an open inbox wait SHALL be authorized and its conditions rendered again before its next read.

#### Scenario: A patch names another account
- **WHEN** a patch changes an open wait's account to one the manifest does not grant
- **THEN** the node fails `inbox_wait_unauthorized` and that account's log is not read

### Requirement: A failed read is retried
A failed read of the account's log SHALL never be cached: the drive and the waker SHALL read again at their next poll whether or not the log changed. A failed head read before the visit is declared SHALL be retried within the engine's bounded infrastructure retries before the node fails `inbox_wait_unreadable`.

#### Scenario: A transient read error
- **WHEN** one read fails and the log is not written again
- **THEN** the next poll reads it and the visit ends with the stored matching event

### Requirement: A long inbox wait parks
An inbox wait whose last read succeeded with no match and nothing stored since SHALL park through the wait park with reason `wait (<node>)` and its timeout as the wake time, journaling a progress `inbox_wait_read` first when it examined past the position. The waker SHALL resume the run once an event of a listed type or a gap is stored above the position.

#### Scenario: A delivery resumes a parked wait
- **WHEN** a parked inbox wait's account stores a matching event
- **THEN** the waker resumes the run within one poll and the visit ends with that event

### Requirement: Retention gaps are journaled before the position moves
When the oldest stored seq is above the position plus one, the visit SHALL journal the existing `inbox_gap` for the missing range once under the wait node's id, even when another node recorded it, before it examines anything. When that append fails, the visit SHALL examine nothing, not park, and try again at the next poll; a timeout SHALL keep `through_seq` at the position.

#### Scenario: Retention dropped unread events
- **WHEN** a visit's position is 5 and the oldest stored event is 9
- **THEN** the run journals one `inbox_gap` from 6 through 8 before the visit examines event 9

#### Scenario: Another node recorded the gap first
- **WHEN** a connector read of the same account journaled the range under its own node
- **THEN** the wait journals it again under its own id before its position advances

#### Scenario: The gap line cannot be written
- **WHEN** the journal refuses the gap append
- **THEN** no progress or end line moves the position past the gap

### Requirement: Inbox wait output is untrusted
An inbox wait node SHALL be an entry point of untrusted text, so a node that renders its output is a follower under V179 to V182, whatever its place in node order.

#### Scenario: The reader is declared before the wait
- **WHEN** an agent declared before the wait in node order renders its output and can send outward
- **THEN** V179 names that agent

#### Scenario: An agent reads the wait's output
- **WHEN** an agent prompt renders `nodes.<wait>.output`
- **THEN** the untrusted-input analysis treats the agent as a follower

### Requirement: Validator codes
V284 SHALL refuse a malformed block (a zero timeout, an empty path or segment, a type the store could never hold, over 20 types or 10 conditions, over 16 `take` entries, a `take` name invalid, repeated or reserved, a `take` path or type the reader refuses). `wait_park` SHALL be named for a definition with an inbox wait.

#### Scenario: A reserved name
- **WHEN** a `take` entry is named `seq`
- **THEN** V284 refuses it

#### Scenario: A type with a space
- **WHEN** a wait lists the type `Pull Request`
- **THEN** V284 refuses it

### Requirement: Validator codes of the binding site
V285 SHALL refuse a connector that is not installed or has no webhook block when a connector store is known; V286 SHALL warn on `types` for a connector that names no event type; V287 SHALL refuse two binding sites with one id. A site SHALL be classified by the node that owns it, never by its id's suffix.

#### Scenario: A node whose id only looks derived
- **WHEN** an `agent_task` named `fetch#inbox` binds an MCP connector and no inbox wait is named `fetch`
- **THEN** no V302 is reported for it

#### Scenario: A node named like a derived site
- **WHEN** a playbook has an inbox wait `a` and a node `a#inbox`, or a list node `a` and a node `a#check`
- **THEN** V287 refuses the definition

### Requirement: Evals and scenarios
An eval case SHALL refuse a playbook with an inbox wait. A scenario run SHALL script an inbox wait as any wait: `signal` or `elapsed` end it with `{}`, `timed_out` times it out, and no inbox is read.

#### Scenario: An eval case
- **WHEN** an eval case names a playbook with an inbox wait
- **THEN** the case is refused with a reason naming the wait node

### Requirement: An inbox wait meets the MCP and secret param rules
V285 SHALL say that an MCP connector cannot declare `webhook:` when an inbox wait names one; V302 SHALL refuse an MCP connector at the derived inbox site `<node>#inbox`; the shared V340 secret param walk SHALL check every string of the inbox wait block.

#### Scenario: An MCP connector has no inbox
- **WHEN** an inbox wait names an MCP connector
- **THEN** V285 refuses the wait because the MCP connector cannot declare `webhook:`, and V302 names the site `<node>#inbox`

#### Scenario: A secret param in a condition
- **WHEN** a string of the inbox wait block references a `secret: true` param
- **THEN** V340 refuses the definition
