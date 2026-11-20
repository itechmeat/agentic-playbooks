# honest-decision-labels Specification

## Purpose
Completion check labels say only what a run proves about the reply: a decision is labelled `keep` only when a person, through a person's channel, approved the reply's stored result at the next gate that approves it; nothing is labelled `act`, a later failure, a supervisor retry or a move back labels nothing, and the labels carry the rule version `reply/1`, so `apb decisions report` and a fitted threshold never score a finished reply as unfinished.

## Requirements

### Requirement: Completion labels read the reply only
The `completion_check` labeller SHALL label a decision `keep` only when the checked node's attempt succeeded and a person approved the reply at the gate that follows it (next requirement), and SHALL leave every other decision unlabelled. It SHALL label no decision `act`. A later node's failure, a supervisor retry, a move back, a rejection at the gate and a publish consent SHALL NOT label it.

#### Scenario: A finished report and a failing next node
- **WHEN** the reply is a finished report and the next node fails
- **THEN** the decision is unlabelled

### Requirement: A keep needs a gate that approves the reply
A gate's decision SHALL count as a person's approval of the reply only when the gate is the next visit after the reply, its `approves.results` names the checked node in the definition the run executed when the gate asked, its request binds this execution's stored result, and a person chose one of its approving options (next requirement); a decision whose gate definition is not stored SHALL stay unlabelled.

#### Scenario: A person approves at the next gate
- **WHEN** the next visit is a review gate that approves the node's result and a person decides one of its approving options
- **THEN** the decision is labelled `keep`

#### Scenario: A gate without approves
- **WHEN** the next visit is a review gate with no `approves` and a person decides `approve`
- **THEN** the decision is unlabelled

#### Scenario: A declared approving option
- **WHEN** the gate approves the node with `approves.options: [ship]` and a person decides `ship`
- **THEN** the decision is labelled `keep`

#### Scenario: An unattended approval
- **WHEN** the next visit is a review gate decided `approve` by an unattended run
- **THEN** the decision is unlabelled

#### Scenario: A rejection
- **WHEN** a person decides `reject` at the next gate
- **THEN** the decision is unlabelled

### Requirement: An item's reply is not the list's
A `completion_check` decision about a list item's reply (`decision_made.item`) SHALL stay unlabelled: the list node's `node_finished` and the approval of the list's gate SHALL NOT be credited to it.

#### Scenario: A list whose item needs a look
- **WHEN** a list with `on_needs_look: continue` has an item that needs a look, the list succeeds and a person approves the gate that approves the list
- **THEN** the item's completion decision is unlabelled

### Requirement: A keep needs a person's channel
A gate's decision SHALL count as a person's only when it has no `decided_by` and its recorded channel is a person's (`terminal` or `dashboard_session`); a decision with no recorded channel, or one through MCP, a run step or any other surface, SHALL stay unlabelled.

#### Scenario: An approval through MCP
- **WHEN** the next visit is a review gate that approves the node's result and its approving option is decided through MCP
- **THEN** the decision is unlabelled

#### Scenario: An approval at the terminal
- **WHEN** the same option is decided by a person at the terminal
- **THEN** the decision is labelled `keep`

### Requirement: Scope re-asks continue the visit
A scope re-ask that keeps binding the same stored result SHALL continue the gate's visit, and the person's decision on the last request of that chain SHALL be the one read, judged against the gate in the stored definition of that last request's segment; any other withdrawal, and any patch applied after the chain's first request, re-asked or not, SHALL leave the decision unlabelled.

#### Scenario: An approval on a scope re-ask
- **WHEN** the gate's request is withdrawn because its scope changed, the gate asks again at once binding the same stored result, and a person approves the re-ask
- **THEN** the decision is labelled `keep`

#### Scenario: A re-ask after a patch
- **WHEN** a patch is applied while the gate waits, its scope changes, the gate asks again binding the same stored result, and a person approves the re-ask
- **THEN** the decision is unlabelled

#### Scenario: A patch while the gate waits
- **WHEN** a patch is applied while the gate waits and a person then approves the same request
- **THEN** the decision is unlabelled

#### Scenario: A re-ask that binds another result
- **WHEN** the re-ask after a scope change binds a different stored result of the node
- **THEN** the decision is unlabelled

### Requirement: The labelling rule has a version
`completion_check` labels SHALL be computed under rule `reply/1`; a threshold stored in the legacy thresholds file SHALL NOT enforce, and `apb decisions report` SHALL report the `completion_check` row under `reply/1` and suggest no threshold while it holds no `act` label.

#### Scenario: An old threshold
- **WHEN** a `completion_check` threshold is stored in the legacy file
- **THEN** the completion check does not enforce with it and `apb decisions thresholds list` marks it legacy, not enforced
