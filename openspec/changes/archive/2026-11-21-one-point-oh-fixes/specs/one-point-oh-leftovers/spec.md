## ADDED Requirements

### Requirement: A GitLab release is sent once
GitLab `create_release` SHALL be sent in marker mode on `description`, looked up with `list_releases` among the 20 newest releases the connector's own account created, and its text SHALL be sent as written.

#### Scenario: A lost answer
- **WHEN** the answer to a release creation is lost and the node is retried
- **THEN** the release is found by its marker and the retry settles as already sent instead of failing with GitLab's 409 for the existing tag

#### Scenario: Another account's release
- **WHEN** the listed release carrying the marker was created by another account
- **THEN** the send stays unknown and no second release is created

#### Scenario: A release beyond the first page
- **WHEN** the marker is not among the 20 newest releases and GitLab names a further page
- **THEN** the send stays unknown and no second release is created

### Requirement: Failing tests from JUnit reports
A `project_checks` source with `junit` SHALL read the failing tests of each `test` command other than `cargo test` from the JUnit XML reports its glob matches that the command wrote. A report it did not write, or one that cannot be read, SHALL make the list unknown.

#### Scenario: A failing make test
- **WHEN** a `make test` writes a JUnit report with one failure before and two after
- **THEN** the comparison is not met and names the new failing case

#### Scenario: A failure no report explains
- **WHEN** a `make test` writes a report whose cases all pass and exits 1
- **THEN** the failing tests read unknown before and after, and the comparison is unknown, never met

#### Scenario: A report the command did not write
- **WHEN** the command writes no report and an older one matches the glob
- **THEN** the failing tests read unknown

#### Scenario: A bad glob
- **WHEN** `junit` is empty, absolute, holds a backslash, has `..`, has an empty or `.` component, has a wildcard in its first component, is not a glob, or `only` names no `test`
- **THEN** validation reports V320 as an error


### Requirement: The JUnit reader is safe on any report
The reader SHALL return a list only for a report it understood completely; anything else SHALL be unknown with a named reason, never a partial or guessed list. No report content SHALL make the reader panic or stall the drive: it checks the command's deadline and the run's cancel flag at bounded intervals in every loop and once before it returns. A failing test's identity SHALL never be cut: one too long for an item, or one the observation text rules refuse, SHALL make the list unknown.

#### Scenario: A long failing test name
- **WHEN** two failing tests' names agree on their first 300 characters
- **THEN** the failing tests read unknown naming the report, and the two never become one item

#### Scenario: A report a runner stopped writing
- **WHEN** a report ends before its root element is closed, or inside a comment, CDATA section or attribute value
- **THEN** the failing tests read unknown, never the cases read so far

#### Scenario: Legal XML in another shape
- **WHEN** a report has an end tag with whitespace before `>`, single-quoted attributes, a UTF-8 byte order mark, a character reference with leading zeros, or a comment after its root
- **THEN** it reads as XML means it

#### Scenario: A report the reader does not understand
- **WHEN** a report is UTF-16, declares another encoding, holds a DOCTYPE or an entity declaration, an unknown entity, an out-of-range reference, or content after its root
- **THEN** the failing tests read unknown with that reason

#### Scenario: A walk stopped by a cap
- **WHEN** a failing report lies deeper than the walk's depth limit, or a link lies where a report could
- **THEN** the failing tests read unknown naming the cap, never a shorter list

#### Scenario: Text that is not ASCII
- **WHEN** a report holds multi-byte characters in a suite name, a test name, a failure message, `system-out`, a CDATA section or a comment
- **THEN** the report reads like an ASCII one, and no byte string makes the reader panic

#### Scenario: A hidden character in a test name
- **WHEN** a failing test's name decodes to a control or invisible format character
- **THEN** the failing tests read unknown and the reason does not repeat the name

### Requirement: Failing tests are compared on an unambiguous key
Each failing test SHALL be compared on a key that encodes its report, classname (absent apart from empty), name and occurrence injectively; the text shown is derived from the key and SHALL never be compared.

#### Scenario: Two tests whose joined text agrees
- **WHEN** one failing run has the case classname `A::B` name `C` and the next the case classname `A` name `B::C`, or two failing `x` become a failing `x` and a failing `x #2`
- **THEN** the comparison reports a new failure: items are compared on an unambiguous key of report, classname, name and occurrence, never on their display text

### Requirement: A JUnit report counts only when the command wrote it
A report SHALL count as written only when it changed during the command (absent before it ran, or its length, content digest or modification time changed) and the run holds its working tree for the command; it is untouched when all three are unchanged. In a run that does not hold its tree the failing tests SHALL read unknown with a reason naming that. The reports SHALL be found by walking from the glob's longest literal directory prefix, so a literal report path is read.

#### Scenario: A literal report path
- **WHEN** `junit` is `reports/junit.xml` and a `make test` writes that file with one failure
- **THEN** the failing tests list that case

#### Scenario: A run that shares its working tree
- **WHEN** a run started with `--allow-shared-workdir` runs a `make test` that writes a JUnit report
- **THEN** the command runs for its status and the failing tests read unknown, naming that the run does not hold its working tree

#### Scenario: An equal-length rewrite within one timestamp tick
- **WHEN** the command rewrites a report with a different failing case of the same byte length and the same modification time
- **THEN** the report counts as written and its failing case is read

### Requirement: JUnit scans are charged to the command's deadline
The report listings before and after a `test` command SHALL count against that command's own deadline and SHALL stop when the run is cancelled; the command's allowance SHALL be computed after the listing before it, and the cancel flag SHALL be checked again immediately before the command starts. A listing before the command that fails SHALL NOT stop the command, which still runs for its status; the failing tests then read unknown with that listing's reason.

#### Scenario: A scan that uses up the allowance
- **WHEN** the listing before the command reaches the command's deadline
- **THEN** the command is not started, its status reads "the check ran out of time" and the failing tests read unknown naming the scan

#### Scenario: A cancel the listing saw
- **WHEN** the run is cancelled before or while the listing before the command runs, and the command names a program that does not exist
- **THEN** the command is not started and its status reads cancelled, not "could not start"

#### Scenario: A listing before the command that cannot be read
- **WHEN** the report directory is a link, or holds more than 10000 entries
- **THEN** the command runs and its status is recorded, and the failing tests read unknown with the listing's reason

### Requirement: process_env in the profile editor
The dashboard profile editor SHALL show a profile's stored `process_env`, read from its YAML, and let a person change it. A save SHALL send `process_env` only when the person changed it.

#### Scenario: An untouched save
- **WHEN** a person saves a profile without touching its process environment
- **THEN** the save sends no `process_env` and the stored value is unchanged

### Requirement: Doctor names a stale claimed playbook directory
`apb doctor` SHALL name every playbook directory without `current` older than ten minutes in the project and the global store whose id lock is not held under the lock's own 60 s rule. It SHALL give a removal command, `rmdir` with the path shell-quoted byte for byte, only for an empty directory; for any other it SHALL give the path and ask a person to inspect it. Only the exact names the draft creation writes SHALL count as draft debris. It SHALL remove nothing and take no lock.

#### Scenario: A copy killed after its claim
- **WHEN** an empty `playbooks/<id>` is older than ten minutes and its id lock is not held
- **THEN** doctor names it once, on the playbook's failed load line, with `rmdir` and the quoted path, and the directory stays

#### Scenario: A copy still at work
- **WHEN** an empty `playbooks/<id>` is older than ten minutes and `playbooks/<id>.lock` was written less than 60 s ago
- **THEN** doctor does not name it

#### Scenario: A draft creation killed after its directory
- **WHEN** a `playbooks/<id>` older than ten minutes holds only the draft's staging directory and the pointer's lock, intent and history files
- **THEN** doctor names it as a stopped draft creation, asks to inspect it, and gives no removal command

#### Scenario: A dot file that is not draft debris
- **WHEN** such a directory also holds `.env`
- **THEN** doctor names it as holding entries apb did not leave and gives no removal command

#### Scenario: A path with spaces or shell syntax
- **WHEN** the empty directory's path holds a space, `$(x)` or a quote
- **THEN** the `rmdir` command quotes it so the shell receives the path unchanged as one word, and a path that cannot be quoted gets no command

### Requirement: Doctor names each stale directory once and never a running copy
A directory the doctor's report already names as a failed load SHALL be named on that line, not on a second one, and an empty directory beside which a staged copy of the same id sits SHALL get no removal command.

#### Scenario: A copy still staging
- **WHEN** an empty `playbooks/<id>` is older than ten minutes, its id lock reads stale, and a staged copy of `<id>` is still in the staging area
- **THEN** doctor names it as a copy that may still be running and gives no removal command

### Requirement: hermes resume form
The hermes resume form SHALL give every value option its value, and the session id taken from hermes's reply SHALL be used only when it has the shape every captured session id must have (ASCII letters, digits, `-`, `_`, `.` and `:`, not starting with `-`, one to 128 characters); any other value SHALL be no session, so the next step starts cold with the downgrade journaled.

#### Scenario: A resume
- **WHEN** a hermes node resumes a session
- **THEN** `-z` is followed by the prompt, `-m` by the model and `--resume` by the session

#### Scenario: A session id of another shape
- **WHEN** hermes's reply holds `{"session_id": "--yolo"}`
- **THEN** no session is captured and no resume is attempted

### Requirement: Behaviour pinned by tests
A list item of a CLI run that cannot start its agent SHALL fail without a host fallback; list items of one worker SHALL each record their own commits and items of several workers none; a join in a loop SHALL wait for a branch of the new round that has not started.

#### Scenario: Commits of list items under one worker
- **WHEN** two items of one worker each commit once
- **THEN** the journal holds two `artifacts_committed` events for the node, the first within item `a`'s execution and the second within item `b`'s, each with one commit carrying the `Apb-Run` trailer and its item's subject

#### Scenario: A join in a loop
- **WHEN** one branch of the new round reaches the join while the other has not started
- **THEN** the join does not start until the other branch of the new round finished

### Requirement: One adapter type for every agent
The engine type that runs every command-line agent SHALL be named `CliAdapter`, and no crate SHALL name `ClaudeAdapter`.

#### Scenario: The rename is complete
- **WHEN** the crates are searched for `ClaudeAdapter`
- **THEN** nothing is found and every agent still runs through `CliAdapter`

### Requirement: Connector approval is a person's act
`apb connector approve` SHALL refuse inside a run step, and `POST /api/connectors/approve` SHALL take an approval only from a signed-in dashboard session, deciding from the request's credential and not from the server's own environment. Both SHALL hold for the connector and for an account, for every connector kind, before anything is read or written.

#### Scenario: A step approves a connector on the CLI
- **WHEN** a process with `APB_RUN_ID` set runs `apb connector approve` for a connector or one of its accounts
- **THEN** it exits 2 and the connector stays unapproved

#### Scenario: A keyless request reaches the dashboard
- **WHEN** an approval reaches a dashboard without keys whose own environment has no run id, posted by a local process without an `Origin` header, by the keyless page itself with its CSRF marker, or as a same-origin browser request
- **THEN** the route answers 403 `keyless_dashboard` naming the CLI command, and the connector and the account stay unapproved

#### Scenario: A bearer key
- **WHEN** an approval of the connector or an account is posted with a bearer key
- **THEN** the route answers 403 `bearer_key` and nothing is approved

#### Scenario: A signed-in session
- **WHEN** a person signed in to the dashboard approves a connector that is not an MCP connector and an account, while the server's own environment names a run
- **THEN** both approvals are recorded

#### Scenario: A signed-in session and an MCP connector
- **WHEN** a person signed in to the dashboard approves an MCP connector or one of its accounts
- **THEN** the route answers 403 `mcp_approve_needs_a_person`, which names no digest, and nothing is approved

### Requirement: Untrusted outside text is announced beside an inbox
A node granted an inbox function and another function with `untrusted_output` SHALL get both the inbox paragraph and the untrusted-output paragraph, each once.

#### Scenario: An inbox and a web search
- **WHEN** a node is granted an inbox read and a search function marked `untrusted_output`
- **THEN** its prompt carries both paragraphs once each
