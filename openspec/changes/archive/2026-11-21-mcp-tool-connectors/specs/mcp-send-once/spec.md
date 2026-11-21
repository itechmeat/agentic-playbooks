## ADDED Requirements

### Requirement: Send-once modes over MCP
An mcp function SHALL NOT declare `idempotency: key`. `marker` SHALL put the marker in a string argument of the tool and reconcile through a read-only mcp function of the same connector; `none` and `repeatable` SHALL behave as for HTTP functions.

#### Scenario: Key is refused
- **WHEN** an mcp function declares `idempotency: { mode: key }`
- **THEN** the manifest fails to load

#### Scenario: A forged marker is neutralized
- **WHEN** the agent's text for a write carries a marker keyword
- **THEN** the tool receives it neutralized and exactly one marker apb added

### Requirement: One call runs in a fixed order
A live call SHALL run in this order: the admission decision, then the program and pin checks, the start and the listing, then the send intent, the dispatch, the journal and the settlement, and only then the end of the server; a call that failed SHALL end its server before its journal and settlement, so the settlement carries what the end proved. A repeat the ledger answers SHALL start no server.

#### Scenario: An already sent repeat
- **WHEN** the ledger answers a repeat `already_sent`
- **THEN** no server is started

#### Scenario: The settlement comes before the end of the server
- **WHEN** a server keeps running after a successful call answered
- **THEN** the intent is settled before apb ends the server

#### Scenario: A failed call whose group is not shown empty
- **WHEN** a failed call's server was reaped but its process group could not be shown empty
- **THEN** the settled intent's detail starts with the warning that a process it started may still run and act

### Requirement: No intent before the server started and passed its pins
No send intent SHALL be recorded before the server started and its pins were checked, on any path. When the admission decision sent the call to the gate before any start and the gate would now admit it, the gate SHALL record nothing, the server SHALL be started and checked, and the gate SHALL decide again.

#### Scenario: A gate that may not record
- **WHEN** the gate is asked for a call whose server is not started and the call would go out
- **THEN** no send intent is recorded and the gate answers that the server must start first

### Requirement: A spent budget refuses before any start
Before the gate answers that the server must start, it SHALL make the locked checks a recording gate makes (a reminder still due, the `max_calls` budget and the node's other limits) and SHALL refuse on any of them, so no secret helper or server of the call starts.

#### Scenario: A budget spent between the preview and the gate
- **WHEN** a function has `max_calls: 1`, call B's preview finds call A in flight, and A then settles `not_sent` and spends the budget before B's gate
- **THEN** B's gate refuses on the budget, no intent is recorded for B, and B's secret helper and server never start

### Requirement: Nothing handed over means not sent
A failure before the send intent is recorded (program check, start, connect, list, tool pin check) SHALL record no intent and SHALL journal no `connector_call`; a failure after the intent and before the `tools/call` request is handed to the transport SHALL settle it `not_sent`, and so SHALL a call no byte of whose request line reached the server's stdin (the session closed, its worker gone, or the pipe refused the first write).

#### Scenario: A server that cannot start
- **WHEN** the pinned program cannot be started
- **THEN** no intent is recorded and no call is journaled

#### Scenario: A kill before the call
- **WHEN** the calling process is killed after the server started and before the call
- **THEN** the journal holds no send intent for it and a repeat goes out

#### Scenario: A server gone before the call
- **WHEN** the server closes its stdin, answers the listing and exits before the call is written
- **THEN** the intent settles `not_sent` once and a repeat goes out

### Requirement: Not sent only with proof
A call SHALL be `not_sent` only with proof that its request can no longer be written: the session SHALL first stop every further write to the server's stdin and then find no byte of the request handed over. A request that was queued and cannot be proven unwritten SHALL be a lost answer (`unknown`).

#### Scenario: The deadline passes while the call is queued
- **WHEN** the server stopped reading, the pipe is full and the deadline passes while the `tools/call` is still queued
- **THEN** the call is `not_sent`, and no `tools/call` of that session reaches the server afterwards, even once the server reads again

#### Scenario: A call partly written before the deadline
- **WHEN** the pipe took part of the `tools/call` line before the deadline passed
- **THEN** the call is a lost answer and its intent stays `unknown`

### Requirement: A lost answer is unknown
A transport failure or a timeout after the request was handed over SHALL settle `unknown`.

#### Scenario: The server exits after reading the call
- **WHEN** the server reads the `tools/call` and exits without answering
- **THEN** the intent settles `unknown` and a `none` repeat is refused `unknown_effect`

#### Scenario: The call times out
- **WHEN** the server never answers within `timeout_sec`
- **THEN** the intent settles `unknown`

#### Scenario: A kill once the intent is recorded
- **WHEN** the calling process is killed as soon as the intent is journaled and the marker is not listed on the repeat
- **THEN** the repeat is refused `unknown_effect` and nothing is sent again

### Requirement: A tool error on a write is unknown
For a write, a result with `isError: true`, a task handle, an `input_required` answer, or a JSON-RPC error of any code SHALL be the outcome `tool_error` and settle `unknown`: every error received after the `tools/call` request was handed over is unknown, and `not_sent` SHALL come only from failures known to precede the hand-over.

#### Scenario: A tool error on a marker send
- **WHEN** a write answers `isError: true` and the next attempt repeats it
- **THEN** the repeat reconciles by its marker and answers `already_sent` without a second `tools/call` when the item exists

#### Scenario: An input_required answer on a write
- **WHEN** a write answers `resultType: input_required`
- **THEN** the call fails `tool_error`, the intent settles `unknown` and a `none` repeat is refused `unknown_effect`

#### Scenario: A request error after the tool acted
- **WHEN** the server performs part of a write and then answers the call with error `-32602`
- **THEN** the call fails `tool_error`, the intent settles `unknown` and a `none` repeat is refused `unknown_effect`

#### Scenario: A JSON-RPC server error
- **WHEN** the server answers a write with error `-32603`
- **THEN** the call fails `tool_error`, the intent settles `unknown` and a `none` repeat is refused `unknown_effect`
