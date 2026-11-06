## ADDED Requirements

### Requirement: The rejection reason reaches the next attempt
When apb rejects a report the agent called a success, non-fatally (a `Rejection` with `fatal == false`, such as a success check, a scoped check, an evidence check, a protected path or a fully restored git guard, or the completion check's enforce rejection), its `attempt_finished` SHALL carry `rejected_for`: the reason, scrubbed, at most 500 characters. Fatal endings, timeouts, interruptions, quota, transient and auth failures SHALL NOT carry it.

#### Scenario: A fresh retry learns why it was rejected
- **WHEN** a success check rejects attempt 1 and attempt 2 starts in a fresh session
- **THEN** attempt 2's prompt contains `reported success; apb rejected it: "<reason>"` with the reason quoted as data

### Requirement: A repair-scope paragraph after a rejection
The retry briefing SHALL end with the fixed `REPAIR_SCOPE` paragraph when the last attempt in scope carries `rejected_for`, or when the latest event in scope is a list item check rejection, and SHALL NOT add it otherwise. The briefing SHALL stay outside the node's cache key.

#### Scenario: No scope paragraph after a crash
- **WHEN** attempt 1 was rejected and attempt 2 failed on a transient error
- **THEN** attempt 3's briefing has no repair-scope paragraph

#### Scenario: The cache key does not move
- **WHEN** a node with `cache: auto` gets a briefing with a rejection reason on attempt 2
- **THEN** the execution's `node_cache_stored` key equals its `node_cache_miss` key
