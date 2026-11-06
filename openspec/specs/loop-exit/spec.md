# loop-exit Specification

## Purpose
TBD - created by archiving change loops-and-graphs. Update Purpose after archive.

## Requirements

### Requirement: The no_new_items edge condition
An edge condition `{ type: no_new_items, node, field, key?, rounds }` SHALL match when the node has at least `rounds` successful visits in this run and each of the last `rounds` of them is readable and output only keys that an earlier successful visit had already output (it added no new key). Failed, timed-out and cancelled visits SHALL be ignored. The verdict SHALL come from the journal alone, with no new event.

#### Scenario: Two dry rounds end the loop
- **WHEN** a node's successful visits output keys `[a, b]`, then `[a]`, then `[b, a]`
- **AND** an edge holds `no_new_items` with `rounds: 2`
- **THEN** the edge does not match after the second visit and matches after the third

#### Scenario: A failed visit is ignored
- **WHEN** a visit between two dry rounds failed
- **THEN** the two dry rounds still count as consecutive

### Requirement: Readable visits
A successful visit SHALL be readable when its output is a JSON object whose `field` is an array, or a string holding one, and every element yields a valid key: the element itself without `key`, the element's `key` value with it. An unreadable visit SHALL break the streak and add nothing to the seen keys.

#### Scenario: A forgotten field does not end the hunt
- **WHEN** the latest successful visit has no `field`
- **THEN** the condition does not match and the loop runs on to its `max_traversals` cap

#### Scenario: The same verdict after a resume
- **WHEN** a run crashes between two rounds and is resumed
- **THEN** routing after the next round is the same as without the crash

### Requirement: The seen keys template
A `{{nodes.<id>.seen.<field>}}` template SHALL render the keys seen so far over every successful visit of the node, one per line in first-seen order, at most 200 keys and 8 KiB, with a final `(N more not shown)` line when keys are left out. The cache key and the attempt prompt SHALL be rendered from the same seen text.

#### Scenario: The next round is told what was found
- **WHEN** round 2 of a loop starts after round 1 output keys `a` and `b`
- **THEN** its prompt lists `a` and `b`
- **AND** with `cache: auto` its `node_cache_miss` key differs from round 1's

### Requirement: V164 for a malformed no_new_items
The validator SHALL report V164 (error) for a `no_new_items` condition whose node is unknown, is not an `agent_task` or `script`, or is a list node, whose `field` or `key` is empty or holds `.`, whitespace or `}`, or whose `rounds` is outside 1 to 10.

#### Scenario: Rounds out of range
- **WHEN** a `no_new_items` condition has `rounds: 0`
- **THEN** validation fails with V164

### Requirement: V166 for a condition outside a loop
The validator SHALL report V166 (warning) when the edge's source and the condition's node are not on one cycle; a single node is on a cycle exactly when it is in a cyclic strongly connected component.

#### Scenario: An exit edge out of a node in no loop
- **WHEN** a `no_new_items` edge leaves node `hunt` and `hunt` is on no cycle
- **THEN** validation warns with V166

### Requirement: V165 for a seen template without its condition
The validator SHALL report V165 (error) for a seen template with no `no_new_items` condition on that node and field, or with two such conditions that disagree on `key`.

#### Scenario: A seen template without a condition
- **WHEN** a prompt reads `{{nodes.hunt.seen.findings}}` and no edge has `no_new_items` on `hunt` and `findings`
- **THEN** validation fails with V165
