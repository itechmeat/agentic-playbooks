## ADDED Requirements

### Requirement: Read-only nodes share a batch only with nodes that cannot write
A `read_only: true` node outside `isolation: full` SHALL be admitted to a concurrent batch, at batch formation and at mid-batch admission, only when no member of that logical batch, finished members included, is a node that can write the tree. A node that can write SHALL NOT be admitted to a batch that holds such a read-only node. A `read_only` node with `isolation: full` SHALL mix with any batchable node. A node with `protect` and a list node SHALL stay out of every batch.

#### Scenario: Three read-only reviewers run at once
- **WHEN** three `read_only` agent or script nodes are ready together and `max_parallel` is at least 3
- **THEN** their attempts overlap in time

#### Scenario: A writer waits for running read-only members
- **WHEN** a read-only member of a batch finishes while another read-only member still runs
- **AND** its successor is a batchable node that can write
- **THEN** the successor starts only after every read-only member of the batch has ended

#### Scenario: A protect node never runs beside a read-only node
- **WHEN** a `protect` node and a `read_only` node are ready together
- **THEN** their attempts do not overlap

### Requirement: A change seen in a shared batch fails every member that saw it
Each read-only member SHALL keep its own before and after check. A member that sees a change SHALL fail with a message that starts with `read_only_dirty`, with no retry, and its `read_only_violated` event SHALL list in `concurrent_with` the batch members whose attempts overlapped its own. On the sequential arm `concurrent_with` SHALL be empty and omitted.

#### Scenario: One member writes
- **WHEN** two read-only members run at once and one writes a tracked file
- **THEN** both fail with `read_only_dirty`
- **AND** each `read_only_violated` names the other in `concurrent_with`
- **AND** no `retry_started` follows

#### Scenario: An old journal decodes
- **WHEN** a journal holds a `read_only_violated` without `concurrent_with`
- **THEN** it decodes with an empty list
