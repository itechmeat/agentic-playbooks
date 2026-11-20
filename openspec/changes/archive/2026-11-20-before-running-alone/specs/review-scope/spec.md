## ADDED Requirements

### Requirement: A form gate's decision names the request it answers
A decision on a review gate that uses `max_scope`, `fields` or `show` SHALL name the journal seq of the request it answers on every channel, the engine's own decisions and a scenario's script included. The post check SHALL refuse one that names no request, or a request the gate replaced or withdrew, with the retryable refusal `decide_again`, which tells the caller to read the gate's request again.

#### Scenario: A late decision after the scope changed
- **WHEN** a form gate's request A is withdrawn because its scope changed, the gate asks again with request B, and a person who read A posts a decision that names no request or names A
- **THEN** the decision is refused `decide_again` and nothing is queued, and a decision that names B is accepted and answers B

#### Scenario: A decision without a request
- **WHEN** a decision on a gate with `show` names no request
- **THEN** it is refused `decide_again`, naming the open request's seq

#### Scenario: The engine's and a script's decisions
- **WHEN** an unattended run declares a decision for a form gate, or a scenario's script answers one
- **THEN** the decision names the open request and is consumed

### Requirement: The request a decision names is the request it is stamped with
For a gate that consumes stamped decisions, the post check SHALL compare the request a decision names with the gate's open request and stamp the entry on the same journal read, so the stamp SHALL always be the request the decision named and was checked against.

#### Scenario: A request asked again between the check and the stamp
- **WHEN** a decision names a form gate's open request A, and the drive withdraws A and asks again with B while the post is being checked
- **THEN** the entry is stamped A or refused, never stamped B, and the drive never consumes it as B's decision

### Requirement: A form gate consumes only a decision stamped for its open request
The drive SHALL consume a form gate's decision only from a review channel entry stamped with the gate's open request, and SHALL NOT ask a passkey of a form gate that is neither person-only nor approving.

#### Scenario: An entry no poster stamped
- **WHEN** the review channel holds an entry for a form gate without a request stamp (an older apb's post, a hand-written line)
- **THEN** the drive never consumes it and `apb doctor --run` lists it

#### Scenario: No passkey asked of a plain form gate
- **WHEN** a passkey is enrolled on the machine and a person decides at a terminal a form gate that is neither person-only nor approving, naming its request
- **THEN** the drive consumes the decision

### Requirement: A plain gate takes a decision without a request
A `human_review` without `max_scope`, `fields` or `show` SHALL keep taking a decision that names no request, with its journal unchanged.

#### Scenario: A plain gate is unchanged
- **WHEN** a decision on a `human_review` without `max_scope`, `fields` or `show` names no request
- **THEN** it is accepted and consumed as before, and the journal holds the request alone with no `review_detail`

### Requirement: An unreadable definition never makes a form gate plain
When the run's definition snapshot cannot be read at post time and the gate's open request has no recorded detail, a decision SHALL NOT be treated as one for a plain gate if the run's latest run guard names `review_fields` or `review_max_scope`: it SHALL get the same `decide_again` refusal.

#### Scenario: A torn request and a definition that does not read
- **WHEN** a form gate's request has no `review_detail` (a write cut after the request's line) and the run's `playbook.yaml` cannot be read
- **THEN** a decision is refused `decide_again`, and after the definition reads again and the resumed drive records the detail, a decision naming the request is consumed

#### Scenario: A run without review forms
- **WHEN** the definition snapshot of a run whose run guard names neither `review_fields` nor `review_max_scope` cannot be read
- **THEN** a decision on its gate is accepted as before
