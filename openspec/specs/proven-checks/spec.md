# proven-checks Specification

## Purpose
TBD - created by archiving change node-contracts. Update Purpose after archive.

## Requirements

### Requirement: The check runs once before the work
A `success_check` of the script form with `prove_fails_first: true` SHALL run once before the first attempt of the node's effect scope, inside that attempt's placement, and SHALL NOT run before a retry, a fallback step, a deferred retry, a resume or a scenario run. Each run SHALL journal `check_proved` and SHALL NOT journal an attempt.

#### Scenario: A red check lets the agent run
- **WHEN** the check fails before the first attempt
- **THEN** `check_proved` with `failed_first: true` is journaled and the attempt starts

#### Scenario: No proof on a retry
- **WHEN** attempt 1 is rejected and attempt 2 starts
- **THEN** no second `check_proved` is journaled

### Requirement: The proof is an observation
The check SHALL run between a before and an after tree state of the node's tree; a change, or a comparison cut short by a limit, SHALL fail the node with an output starting `check_changed_tree:` without starting the agent.

#### Scenario: A check that edits a tracked file
- **WHEN** the check script modifies a tracked file and exits 1
- **THEN** the node fails with `check_changed_tree:` and no `attempt_started` is journaled

### Requirement: The proof runs once across a crash
A `check_proved` with no `node_finished` after it SHALL decide the visit on resume: the check SHALL NOT run again, a red proof SHALL go on to the attempt and a green one SHALL settle as it said.

#### Scenario: A crash after a red proof
- **WHEN** the run crashes after `check_proved` and before `attempt_started`, and is resumed
- **THEN** the journal holds exactly one `check_proved` and the attempt runs

### Requirement: A green check before the work
When the check passes before the agent ran, the node SHALL fail with an output starting `check_green_before:` and the run outcome SHALL need a look for it, unless `on_green_before: skip`, which SHALL settle the node `succeeded` with an output starting `already done:` without starting the agent.

#### Scenario: Green means fail by default
- **WHEN** the check passes before the first attempt and `on_green_before` is not set
- **THEN** the node fails without an `attempt_started`
- **AND** the run outcome's needs-a-look lists `1 check green before work`

#### Scenario: Green means already done with skip
- **WHEN** `on_green_before: skip` and the check passes before the first attempt
- **THEN** the node succeeds without an `attempt_started` and its output starts `already done:`

### Requirement: V173
The validator SHALL report V173 (error) for `on_green_before` without `prove_fails_first: true`, a `{ script }` map with neither `scope` nor `prove_fails_first`, `prove_fails_first` on a node with `cache`, and `on_green_before: skip` on a node that declares `outputs.fields` or `results` or whose output an edge condition reads. The engine SHALL NOT serve a proving node from the cache even when validation was skipped.

#### Scenario: A proof with a cache
- **WHEN** a node has `cache: auto` and `prove_fails_first: true`
- **THEN** validation fails with V173

#### Scenario: Skip with declared fields
- **WHEN** a node with `on_green_before: skip` declares `outputs: { fields: [verdict] }`
- **THEN** validation fails with V173
