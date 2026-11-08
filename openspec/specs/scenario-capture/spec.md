# scenario-capture Specification

## Purpose
TBD - created by archiving change run-feedback. Update Purpose after archive.

## Requirements

### Requirement: Captured answers replay what each execution reported
apb SHALL build a scenario from a finished production run's journal with one answer per node execution, in the format's own kinds: agent attempts (status, failure kind, output or declared fields, a check rejection, a read-only violation, a question and its answer), a script's result, a review decision, judge answers, a wait's outcome, a child's result and a publish consent.

#### Scenario: A rejected attempt replays as a rejection
- **WHEN** a run's success check rejected attempt 1 of node `build` and attempt 2 succeeded
- **THEN** the captured scenario answers `build` visit 1 with a `succeeded` attempt carrying `check: reject`, then a `succeeded` attempt
- **AND** playing it at quick level ends with the same status and visit count for `build` as the run

#### Scenario: Declared fields replay as fields
- **WHEN** a node's recorded output is a JSON object
- **THEN** the captured attempt carries it in `outputs`, and a later node's `{{nodes.<id>.output.<field>}}` renders the same value in the replay

### Requirement: Captured expectations assert what the run did
The captured `expect` SHALL hold the run's outcome, its finish node when one finished, and for each node in scope its last status and its number of executions; `--node` SHALL narrow the nodes in scope.

#### Scenario: Visits count executions
- **WHEN** a node ran once with two attempts
- **THEN** the scenario expects `visits: 1` for it and answers one visit with two attempts

### Requirement: A capture is whole or refused
A capture SHALL be refused, naming every gap, when the run holds anything the scenario format cannot replay exactly (among them a list node, a host task, a directive that moved the run, an interrupted attempt, a timed-out script node, a cache hit, a fatal check rejection, an enforced baseline check), when the run has not finished, was aborted or is not a production run, or when its journal does not read strictly whole (a broken line, an event type this apb does not know).

#### Scenario: A run with a list node
- **WHEN** a capture is asked of a run with a list node
- **THEN** it is refused with the gap `list_node` and no file is written

### Requirement: Nothing secret reaches the file
Every captured text and value SHALL pass through the run's scrubber before it is placed, the result SHALL be checked with the scrubber's refusal check, and a capture whose run secrets cannot all be read SHALL be refused. The file SHALL be produced only by serializing the scenario type.

#### Scenario: A secret in an output
- **WHEN** a node's output holds a configured secret value
- **THEN** the preview's YAML does not hold it and its redacted count is at least 1

### Requirement: A person saves what they previewed
A capture SHALL first return a preview (the YAML, warnings, the redacted count and a digest) and write nothing; a save SHALL come only from a person's channel (the CLI or the dashboard, never MCP), recompute the capture, refuse when the digest differs from the preview's, and write a new file below the playbook's `evals/scenarios/` with a write that never replaces an existing file. Previewing and saving SHALL be refused from a run step.

#### Scenario: A stale preview
- **WHEN** a save names a digest that differs from the capture's current digest
- **THEN** it is refused with `stale_preview` and no file is written

#### Scenario: Never overwrite
- **WHEN** a save names a scenario that already exists
- **THEN** it is refused with `scenario_exists`

### Requirement: A scripted attempt can time out
A scenario attempt answer SHALL accept `timed_out: true`, optionally with `output`, and nothing else; the scripted agent SHALL end that attempt as a timeout at once, without waiting, so the real drive journals it `timed_out` and applies its own retry, fallback and node status. The field SHALL be left out of the serialized form when false, so the digest of every scenario without it is unchanged.

#### Scenario: A timeout without waiting
- **WHEN** a quick scenario answers a node with `max_retries: 0` and `timeout_seconds: 600` with `- { timed_out: true }`
- **THEN** the node finishes `timed_out` and the scenario ends in seconds

#### Scenario: Old digests stay
- **WHEN** a scenario file without `timed_out` is loaded by this apb
- **THEN** its digest equals the digest the previous apb computed

#### Scenario: A timed-out attempt is captured
- **WHEN** a run's agent attempt ended `timed_out`
- **THEN** the capture answers it with `timed_out: true` and the replay ends with the same node status

#### Scenario: A malformed timeout
- **WHEN** an attempt answer has `timed_out: true` and `status: succeeded`
- **THEN** the scenario file is refused with V146

#### Scenario: Two saves at once
- **WHEN** two saves of the same name run at the same time
- **THEN** exactly one writes the file and the other is refused with `scenario_exists`

#### Scenario: MCP previews only
- **WHEN** an MCP caller asks to capture a run
- **THEN** it gets the preview and no way to save it

#### Scenario: An aborted run
- **WHEN** a capture is asked of a stopped run
- **THEN** it is refused with the gap `aborted`
