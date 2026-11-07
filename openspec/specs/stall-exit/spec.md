# stall-exit Specification

## Purpose
TBD - created by archiving change node-contracts. Update Purpose after archive.

## Requirements

### Requirement: A fingerprint on every rejection
Every `attempt_finished` that carries `rejected_for` SHALL also carry `rejection_fingerprint`: the first 16 hex characters of the SHA-256 of the reason with the run directory and run id, timestamps, UUIDs, hex ids and digit runs normalized and whitespace collapsed.

#### Scenario: Numbers do not change the fingerprint
- **WHEN** two rejections read `3 tests failed at 2026-11-06T10:00:00Z` and `5 tests failed at 2026-11-06T10:05:12Z`
- **THEN** their fingerprints are equal

### Requirement: Stop after repeated identical rejections
With `stall_after: N` on an `agent_task` (or in `defaults`), when a rejection's fingerprint equals the fingerprint of the N - 1 attempts of the node before it in its effect scope, skipping transient and interrupted attempts, the node SHALL end `failed` at once with an output starting `stalled:`, its last `attempt_finished` SHALL carry `stalled: true`, and no retry or fallback step SHALL start.

#### Scenario: Three times the same rejection
- **WHEN** `stall_after: 3`, `max_retries: 5`, and a success check rejects attempts 1, 2 and 3 with the same reason
- **THEN** the node ends `failed` after attempt 3 with an output starting `stalled:`

#### Scenario: A different reason resets the streak
- **WHEN** attempts 1 and 3 have one fingerprint and attempt 2 another
- **THEN** attempt 3 does not stall the node under `stall_after: 2`

#### Scenario: The streak survives a deferred retry
- **WHEN** the node has a `retry_delay` and the stall limit is reached on an attempt after a deferral
- **THEN** the node stalls exactly as without the delay

### Requirement: Routing a stall
A `node_status` condition SHALL accept `equals: stalled`, matching a node whose latest execution ended stalled; `equals: failure` SHALL still match it. The node's status SHALL stay `failed` in the journal.

#### Scenario: A stalled route taken first
- **WHEN** a node stalls and its edges are `stalled`, then `failure`
- **THEN** the run takes the `stalled` edge

### Requirement: The stalled notification
A subscription that names the opt-in transition `stalled` SHALL be sent one notice per stalled node execution; the default transitions SHALL NOT include it.

#### Scenario: Opt-in only
- **WHEN** a run's subscription covers the default transitions and a node stalls
- **THEN** no `stalled` notice is sent

### Requirement: V174 and V175
The validator SHALL report V174 (error) for `stall_after` outside 2 to 10, on a node that is not an `agent_task`, or on a list node, and V175 (warning) for a `stalled` edge that can never match or that follows a `failure` edge on the same node from the same source.

#### Scenario: A stalled edge without stall_after
- **WHEN** an edge has `equals: stalled` and the node has no effective `stall_after`
- **THEN** validation warns with V175
