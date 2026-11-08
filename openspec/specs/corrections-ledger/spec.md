# corrections-ledger Specification

## Purpose
TBD - created by archiving change run-feedback. Update Purpose after archive.

## Requirements

### Requirement: Corrections are collected at every run end
When a production run ends (finished or aborted) and its journal reads strictly whole, apb SHALL add its corrections to its playbook's ledger: a person's non-approving review note, a supervisor-channel note aimed at a node, a human answer to a question, and an attempt apb rejected. A run SHALL count once per entry.

#### Scenario: The same note in two runs
- **WHEN** two runs of a playbook get the review note "add tests for the parser" and "Add the parser tests, please"
- **THEN** the ledger holds one entry with `runs: 2`

#### Scenario: Not a correction
- **WHEN** a run's review is decided `approve` with a note, or a retry advice is applied
- **THEN** the ledger gains no entry

### Requirement: Grouping needs no model
Entries SHALL be keyed by `f:<rejection_fingerprint>` when an attempt carries one, else by a digest of the kind, the node and the sorted distinct lowercase tokens of the scrubbed text, with tokens holding a digit or a single character dropped.

#### Scenario: Line numbers do not split an entry
- **WHEN** two rejections differ only in a line number
- **THEN** they share one entry

### Requirement: The ledger is signed and scrubbed like known problems
The ledger SHALL be stored at `.apb/corrections/<scope>/<id>.json` through the same signed store as known problems (one implementation of the MAC, the lock and the atomic write), with every text through the scrubber's stored-text path; a file this installation did not sign SHALL read as empty and be replaced on the next write.

#### Scenario: An unsigned file
- **WHEN** the ledger file was not written by this installation
- **THEN** no entry of it is shown and its entry count is reported

### Requirement: A recurring entry offers a draft fix
An entry SHALL become `proposed` once it recurs in 2 runs (a fixed number), and SHALL offer a draft: one line for the prompt of the node it names. No draft SHALL be offered when the node is unknown or ambiguous (a gate fed by several branches), has no prompt, the entry came through the supervisor channel, or its text was withheld. apb SHALL never apply a draft or create a version or candidate from it.

#### Scenario: A proposed review note
- **WHEN** an entry for node `implement` reaches 2 runs
- **THEN** its status is `proposed`, its draft names `implement` and a line that quotes the leading wording, and the playbook store is unchanged

#### Scenario: A gate fed by a join
- **WHEN** a review note is made at a gate fed by two parallel agent tasks
- **THEN** the entry has no node and offers no draft

### Requirement: A withheld text leaves no trace
A correction whose text the scrubber flags SHALL be kept only when a valid engine fingerprint gives its key, with no text; otherwise it SHALL be dropped, and nothing derived from the raw text SHALL be stored.

#### Scenario: A token as a note
- **WHEN** a review note is only a credential-shaped token
- **THEN** the ledger gains no entry and its file holds neither the token nor a hash of it

### Requirement: A person marks the fix, and it is followed up per version
A person SHALL mark the version that carries a fix (CLI or dashboard), which snapshots the base version's run and hit counts; the entry is then `fixed`, and SHALL become `reopened` once 3 runs of the fix version were observed and its rate per run there is not lower than the snapshotted base rate.

#### Scenario: The correction did not stop
- **WHEN** the base snapshot is 2 hits in 2 runs and the fix version ran 3 times with the correction in all 3
- **THEN** the entry is `reopened`

### Requirement: Each run counts once, forever
A run SHALL count at most once in the ledger, also after its id left the bounded list of observed runs; a run that started before the oldest kept one SHALL never be counted.

#### Scenario: Observed again much later
- **WHEN** a run is observed again after 1001 later runs
- **THEN** no count changes

### Requirement: The ledger is shown only to people
No prompt, template, supervisor line or run surface SHALL read the ledger, and every ledger entry point, reads included, SHALL be refused from a run step.

#### Scenario: A run step asks
- **WHEN** `playbook_corrections` is called with `APB_RUN_ID` set
- **THEN** it is refused
