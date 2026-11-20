# eval-connector-doubles Specification

## Purpose
An eval case can answer the connector functions its playbook calls with doubles: every dispatched connector call of a doubled run is checked as a real call is and then answered by a matching double or refused `double_unmatched`, no secret is resolved and no request leaves the machine, every call is captured for `checks.connector_calls`, the run carries the `eval_doubles` run guard, and an `--external-data` repetition with doubles is never kept.

## Requirements

### Requirement: A case answers connector calls from recorded answers
An eval case SHALL accept `connectors:`, per connector function an ordered list of answers, each with an optional `match` (`args` subset, `contains` text, `site`), an `answer` of `status` and `body`, and an optional `times`. A call SHALL be answered by the first answer whose match holds and whose `times` is not used up, counted across every call process of the run.

#### Scenario: A sequence of answers
- **WHEN** a function has the answers `{answer: A, times: 1}` then `{answer: B}` and a run calls it three times
- **THEN** the calls get A, B and B

#### Scenario: An answer for one argument
- **WHEN** an answer matches `args: { repo: site }` and a call passes `repo: docs`
- **THEN** that answer does not serve the call

### Requirement: A doubled run never reaches a service
In a run whose eval settings carry doubles, every connector call SHALL be checked as a real call is and then answered by a double or refused with `double_unmatched`, without resolving any secret for the call and without opening a connection; a function's own `mock` SHALL NOT answer a doubled call. A dry run of a call SHALL stay a dry run.

#### Scenario: An unmatched call
- **WHEN** a doubled run calls a function no answer matches
- **THEN** the call fails with `double_unmatched`, the service receives nothing, and the journal's `connector_call.outcome` is `double_unmatched`

#### Scenario: No secret is resolved
- **WHEN** the account's secret is a command helper
- **THEN** the doubled run's calls are answered, the helper never runs, and the captured args are withheld because the storage redactor cannot account for that secret

#### Scenario: A function with its own mock
- **WHEN** a doubled run calls a function whose connector declares a `mock` for it
- **THEN** the call gets the double's answer, uses its `times` and is captured

#### Scenario: Invalid args
- **WHEN** a doubled run calls a function with args its schema refuses
- **THEN** the call is refused `invalid_args` and no answer is used

### Requirement: A doubled run's snapshot names no usable account
The manifest of a doubled run SHALL record each bound connector's account names only, with no account record a call can resolve, so that an `apb` that does not know doubles fails every call of the run before it loads the connector, resolves a secret or sends a request.

#### Scenario: An apb that does not know doubles
- **WHEN** an older `apb connector call`, run by an absolute path, calls a function of a doubled run
- **THEN** the call fails `config` naming the account, the service receives nothing and no secret helper runs

### Requirement: Secrets and notifications of a doubled run
The start of a doubled run SHALL check the accounts' variables for presence as for any run and SHALL never read their values. A doubled run SHALL NOT send a notification.

#### Scenario: A notification
- **WHEN** a doubled run reaches a notification
- **THEN** the notification call is refused and nothing is captured

### Requirement: A doubled call fails closed
A call SHALL fail when the run's settings cannot be read or contradict the run's provenance, and SHALL NOT be treated as a call of an undoubled run.

#### Scenario: Unreadable settings
- **WHEN** the run's provenance says it is doubled and its `run.yaml` cannot be read
- **THEN** the call fails `config` and nothing is sent

### Requirement: A doubled run carries the eval_doubles guard
A doubled run SHALL journal `run_provenance.eval_doubles: true` and carry the run guard `eval_doubles`, written in the same journal write as `run_provenance` and kept by every patch, so that an apb that knows run guards but not `eval_doubles` refuses to resume or pause a run whose journal holds the guard. A start that is not doubled SHALL write exactly the journal it writes without this change, with its provenance and its guard in separate appends.

#### Scenario: The guard
- **WHEN** a doubled eval run starts
- **THEN** its `run_guard` names `eval_doubles` and follows `run_provenance` at the next seq, from the same write

#### Scenario: A guarded start without doubles
- **WHEN** a run with a deadline and no doubles starts
- **THEN** its `run_started`, `run_provenance` and `run_guard` are three separate appends, as without this change

### Requirement: The account-free snapshot is the protection
The paired write of the guard SHALL be treated as a narrowing of the start's window in which a run can be left without its guard, not as an atomic boundary; the protection of a doubled run from a service through a connector SHALL rest on its account-free snapshot alone.

#### Scenario: A start torn before its guard
- **WHEN** a doubled run's journal holds `run_started` but no guard and an older apb drives it
- **THEN** every connector call of that drive fails `config` and nothing is sent

### Requirement: The step's apb is this apb
`apb eval` SHALL set the `PATH` of every agent and script a doubled run spawns to its own binary's directory followed by the `PATH` the `apb eval` process inherited, and nothing else.

#### Scenario: The step's apb
- **WHEN** a step of a doubled run looks up `apb` on its `PATH`
- **THEN** it finds the binary that runs `apb eval`, and its `PATH` is exactly that binary's directory followed by the inherited `PATH`

### Requirement: Doubled sends keep the send ledger
A doubled call of a function that declares `idempotency` SHALL go through the send ledger as a real send does, settled by the double's answer, and its reconcile function, and for a `marker` send the connector's identity function, SHALL be answered by a double as well.

#### Scenario: A doubled marker read-back
- **WHEN** a doubled `marker` send is answered 500 and the same send is made again
- **THEN** the read-back reads the doubled identity and the doubled listing, no call is unmatched, and the send stays unknown

#### Scenario: A repeated doubled send
- **WHEN** a node sends the same intent twice in a doubled run
- **THEN** the second call is answered `already_sent`

### Requirement: Every dispatched call is captured scrubbed
A doubled run SHALL append every dispatched call, matched or not, to `eval-calls.jsonl` in its run directory with its site, connector, function, account, item, the answer index, the call's input arguments (before the connector's text modes and send markers) scrubbed by the run's storage redactor, and whether the args were kept whole; args longer than 16 KiB SHALL be kept as clipped, and a list item id SHALL pass the same redactor.

#### Scenario: A secret in the args
- **WHEN** a call's args hold the run's secret param value
- **THEN** `eval-calls.jsonl` does not hold that value

### Requirement: Only dispatched calls are captured
A dispatched call SHALL be a call that reaches the answer step. A call refused before it, a repeat the send ledger answers included, SHALL leave no capture line.

#### Scenario: A repeat answered by the send ledger
- **WHEN** a node sends the same tracked intent twice in a doubled run and the second call is answered `already_sent`
- **THEN** the capture holds one call of that function

### Requirement: A capture that does not read stops the answers
A capture line that does not parse SHALL fail the next call of the run instead of being skipped.

#### Scenario: A corrupt capture line
- **WHEN** a line of `eval-calls.jsonl` does not parse and the run makes another call
- **THEN** the call fails `config` and no answer is handed out

### Requirement: A doubled call needs an identity
A doubled call SHALL be identified by a MAC under this installation's key; when no usable key exists the call SHALL be refused with `eval_call_key_unavailable` before any capture line is written and before any answer is picked.

#### Scenario: No usable key
- **WHEN** the installation's key cannot be created or read and a doubled run makes a call
- **THEN** the call fails `eval_call_key_unavailable`, no capture line is written and no `times` is used

### Requirement: Captured args are stored only when they can be scrubbed
The storage redactor SHALL refuse whenever any secret the workspace's connectors reference is unresolved, in every process mode, and SHALL never run a command helper; when it refuses, the captured args SHALL be withheld.

#### Scenario: A command-sourced secret in a driving process
- **WHEN** a connector installed in the workspace references a command-sourced secret and a doubled run's call is captured by a process that is not read-only
- **THEN** the capture's args read withheld and an args assertion over that call fails as indeterminate

### Requirement: The capture is read as a plain file
The capture SHALL be read without following a link, opening a regular file with one link only (never a FIFO), at most 64 MiB; a capture that cannot be read so SHALL fail the repetition.

#### Scenario: A link at the capture's path
- **WHEN** a step replaces `eval-calls.jsonl` with a link or a FIFO
- **THEN** the capture does not read, nothing blocks, and the repetition fails `connector_doubles`

### Requirement: A crashed call does not consume an answer
A doubled call SHALL reserve its answer in the capture, and the reservation line SHALL be written and synced before the call is dispatched. A reservation with no delivery whose reserving process is gone SHALL be replayed with the same answer to a retry of the same call, using no further `times`; a reservation held by a live call SHALL NOT be replayed.

#### Scenario: A crash between the reservation and the delivery
- **WHEN** a call process dies after its reservation and before its delivery, and the same call is made again
- **THEN** the retry gets the same answer, the next call gets the next answer, and the checks count one call

#### Scenario: A power loss after dispatch
- **WHEN** a doubled call's process stops after its reservation and before its `connector_call` reached the journal
- **THEN** the reservation is in the capture, the repetition fails `connector_doubles` naming it, and a `count: 0` check of its function is indeterminate

### Requirement: The capture's directory is synced before a dispatch
The run directory SHALL be synced before a doubled call is dispatched, for a new reservation and for a replayed one, unless this process already synced it; a sync that fails SHALL refuse the call.

#### Scenario: A replay after a failed sync
- **WHEN** a call is refused because the run directory could not be synced and the same call is made again
- **THEN** the replay syncs the directory before it is dispatched and the capture holds one delivered reservation

### Requirement: A missing or short capture is never no call
A doubled run's capture file SHALL be created empty and synced with its directory before the run's first node, and a missing capture file SHALL fail every connector check as `capture_missing`, whatever the journal says. When the journal records a doubled call the capture does not hold, every connector check SHALL fail `capture_incomplete`; a `count: 0` check SHALL pass only when both hold no call of its function.

#### Scenario: A capture deleted after a dispatched call
- **WHEN** the capture is removed after a doubled call was dispatched
- **THEN** `connector_doubles` fails `capture_missing` and a `count: 0` check is indeterminate

#### Scenario: A capture removed with an empty journal
- **WHEN** the capture file is removed and the journal records no doubled call
- **THEN** `connector_doubles` fails `capture_missing`

#### Scenario: A journal that cannot record the call
- **WHEN** the run journal cannot be written while a doubled call is dispatched
- **THEN** the call is refused `eval_call_unjournaled` and no answer is delivered

#### Scenario: A capture cut short
- **WHEN** the capture loses a whole call the journal records
- **THEN** `connector_doubles` fails `capture_incomplete`

### Requirement: A doubled call the journal cannot record is refused
A doubled call whose `connector_call` the run journal cannot record SHALL be refused before its answer is delivered (`eval_call_unjournaled`); a real call SHALL keep journaling best effort.

#### Scenario: An unwritable journal
- **WHEN** the journal cannot be appended to while a doubled call runs
- **THEN** the call is refused and its reservation stays undelivered

### Requirement: The capture decides the checks and the journal decides the run
When the capture and the run journal disagree about a doubled call, the checks SHALL read the capture only and the run (a resume, the send ledger, the run outcome and every run surface) SHALL read the journal only; neither SHALL be repaired from the other.

#### Scenario: A settled send without a delivery
- **WHEN** the journal holds a tracked send settled `confirmed` and the capture holds its reservation without a delivery
- **THEN** the repetition fails `connector_doubles` naming the call, and a repeat of the send is answered `already_sent` by the send ledger

### Requirement: An external-data repetition with doubles is never kept
An eval repetition run with `--external-data` whose case has doubles SHALL, after its checks ran, remove its call capture and then its whole repetition directory, whatever its outcome and whether or not a process of the run still runs, and SHALL be neither stored nor kept; its check results SHALL still be reported. When the directory cannot be removed, the repetition SHALL fail naming the path, and nothing SHALL report it removed.

#### Scenario: A passing external-data repetition
- **WHEN** a doubled case passes with `--external-data`
- **THEN** the result reports its checks and calls and nothing of the repetition remains on disk

#### Scenario: A driver that is still alive
- **WHEN** an `--external-data` repetition with doubles ends while its driver still seems to run
- **THEN** nothing is stopped for it and nothing of the repetition remains on disk

#### Scenario: A repetition that cannot be removed
- **WHEN** a step left a directory in the repetition that cannot be removed
- **THEN** the repetition fails with `external_data_removal` naming the path

#### Scenario: Without external data
- **WHEN** a doubled case runs without `--external-data` and its repetition cannot be stored
- **THEN** its tree and capture are kept as before

### Requirement: The removal follows no link
apb SHALL remove the repetition directory it created, following no link and touching nothing outside it. Detecting a moved run tree is best effort: a component that is a link, missing or unreadable SHALL fail the repetition naming the path; a directory put in the place of the original is not detected. The rule covers what apb wrote and what lies in the repetition directory; it does not contain the evaluated agent, whose writes elsewhere only the node's isolation bounds.

#### Scenario: A run tree moved behind a link
- **WHEN** a step moves `tree/.apb` out of the repetition and puts a link in its place
- **THEN** nothing outside the repetition is touched, the moved run is left where it went, and the repetition fails naming the path

### Requirement: An answer leaves only after its delivery is recorded
A doubled call SHALL return its answer only after its delivery line is written and synced; when the delivery cannot be recorded the call SHALL fail with `eval_call_delivery_unrecorded` instead. Checks SHALL count delivered calls only, and a reservation never delivered SHALL fail the repetition instead of being read as no call.

#### Scenario: A send settled before its delivery was recorded
- **WHEN** a tracked doubled send is settled `confirmed` by the send ledger and its delivery cannot be recorded
- **THEN** the call fails `eval_call_delivery_unrecorded`, a repeat is answered by the send ledger without replaying the reservation, and the repetition fails `connector_doubles` naming the undelivered call

### Requirement: Assertions over captured calls
`checks.connector_calls` SHALL read the dispatched calls only and SHALL select the delivered captures of one `<connector>.<function>`, narrowed by `site`, `args` and `contains`, and pass when the selection meets `count`, `min` and `max`, or is not empty when none is set. A doubled case SHALL fail its `connector_doubles` check when any call was unmatched or any reservation was never delivered.

#### Scenario: One issue filed for the permalink
- **WHEN** a case asserts `{ function: github.create_issue, contains: "WEB-12", count: 1 }` and the run filed one such issue
- **THEN** the check passes

### Requirement: Call assertions read the input arguments
The `args` and `contains` of a `connector_calls` entry SHALL assert the input arguments of the call, before the connector's text modes, the neutralizing of forged markers and an appended send marker.

#### Scenario: A marker send
- **WHEN** a case asserts `contains` over a call of a `marker` send function
- **THEN** the check reads the text the call was given and never the marker the real request would append

### Requirement: Assertions never decide over incomplete captures
A `connector_calls` entry that sets `args` or `contains` SHALL fail as indeterminate, and never pass, when a selected capture's args were clipped or withheld. A `connector_calls` entry SHALL also fail as indeterminate when an undelivered reservation of its function exists. When the capture file does not read, the `connector_doubles` check SHALL fail naming the line and every `connector_calls` check of that repetition SHALL fail as indeterminate.

#### Scenario: Clipped args
- **WHEN** a case asserts `{ function: github.create_issue, contains: "WEB-12", count: 0 }` and a captured call's args were clipped
- **THEN** the check fails as indeterminate

#### Scenario: A torn capture line
- **WHEN** a line of `eval-calls.jsonl` does not parse
- **THEN** the repetition fails with `connector_doubles` naming the line

### Requirement: Coverage lifts the connector refusal
`apb eval` SHALL run a case of a playbook that binds connectors only when its doubles answer every function of every binding site the playbook's binding-site walk lists and every reconcile function of a tracked send (with the connector's identity function for a `marker` send), and SHALL refuse the invocation before anything runs otherwise.

#### Scenario: A partly doubled case
- **WHEN** a playbook binds `sentry.list_issues` and `github.create_issue` and a case doubles only the first
- **THEN** `apb eval` refuses with `invalid_suite` naming V280 and `github.create_issue`, and nothing runs

### Requirement: Validation of doubled cases
The validator SHALL report each coverage gap of a case that declares doubles as V280, a malformed call assertion as V281, and today's V81 reasons for a case without doubles.

#### Scenario: A malformed assertion
- **WHEN** a `connector_calls` entry sets both `count` and `min`
- **THEN** validation fails with V281

### Requirement: A declared irreversible effect stays refused
A case SHALL be refused for a playbook whose effective effects contain `irreversible`, or one of whose nodes declares `irreversible` or `secrets` or is a shipping step, whatever its doubles cover, exactly as without doubles.

#### Scenario: A fully doubled case of an irreversible playbook
- **WHEN** a case doubles every bound function of a playbook with `effects: [network, external, irreversible]`
- **THEN** `apb eval` refuses the case before anything runs and `apb validate` reports V81 naming `irreversible`

#### Scenario: A playbook without irreversible effects
- **WHEN** a case doubles every bound function of `crm-cleanup` (`effects: [network, external]`)
- **THEN** the case runs and its calls are answered by the doubles
