# review-scope Specification

## Purpose
A review gate that uses a form feature shows what the decision covers as counts, can withhold options above a size limit, and reports decisions made faster than their scope takes to read.

## Requirements

### Requirement: A form gate's request shows what the decision covers
The request of a review gate that uses a form feature (`max_scope`, `fields` or `show`) SHALL journal, in the same write as the request, the scope since the gate's previous decision on the same node or since the run's start: commits, changed files and added and removed lines of those commits, list items settled, connector writes, send intents and result snapshots, as counts only.

#### Scenario: A gate before publish
- **WHEN** three commits changing 6 files by 41 added and 38 removed lines precede a gate with `max_scope`
- **THEN** the pending review shows "3 commits, 6 files, +41 -38 lines"

#### Scenario: An uncommitted change is not in the scope
- **WHEN** a node leaves a modified file it did not commit
- **THEN** that file is not counted

#### Scenario: Counts only
- **WHEN** a committed file is named secret-name.txt
- **THEN** no journal line contains that name

#### Scenario: A request without its recorded scope
- **WHEN** the open request of a gate that uses a form feature has no recorded detail (a write cut by a crash right after the request's line)
- **THEN** a decision is refused (on a gate with `max_scope`, naming the options it may withhold) until the resumed drive records the detail, which it does before it posts or waits for a decision

### Requirement: One function decides whether a gate journals its scope
Whether a gate needs the scope detail SHALL be decided by one function over the gate's definition that the writer and every reader share.

#### Scenario: A plain gate keeps the journal of an older apb
- **WHEN** a run waits at a `human_review` without `max_scope`, `fields` or `show`, or at a publish consent
- **THEN** its journal holds the `review_requested` alone, with no `review_detail`, so an older apb still resumes the run

### Requirement: The scope window holds every undecided commit
A withdrawn request SHALL NOT move the scope window, and the files and lines SHALL be counted over every commit of the window, so a change a later commit takes back still counts.

#### Scenario: A withdrawn request
- **WHEN** a supervisor moves the run back to a gate whose open request covers a 90-file change, withdrawing it
- **THEN** the reopened request still counts the 90 files

#### Scenario: A change taken back
- **WHEN** one commit adds 3 files and the next removes them before the gate
- **THEN** the scope counts 2 commits, 3 files, +3 -3 lines

### Requirement: A decision is taken on the scope it covers
Before the drive consumes a decision on a gate whose request carries a scope, it SHALL measure the scope again with the function and window the request used. When it is unchanged the decision SHALL be consumed with nothing journaled. When it changed, the decision SHALL NOT be consumed: the request SHALL be withdrawn with `review_withdrawn` and a `scope_changed` reason, and the gate SHALL ask again at once with a fresh request and scope, against which `max_scope` is checked.

#### Scenario: Work lands while the request waits
- **WHEN** a gate with `max_scope: { files: 1 }` asks on a 1-file scope, a deferred retry commits one more file while the request waits, and then a person approves
- **THEN** the approval is not consumed, the request is withdrawn once with the reason, and the gate asks again on a 2-file scope that withholds approve

#### Scenario: An unchanged scope
- **WHEN** nothing lands while a gate's request waits
- **THEN** the decision is consumed and no withdrawal or second request is journaled

### Requirement: A re-asked gate is one visit
`apb stats` and the run outcome SHALL count a request withdrawn with the `scope_changed` reason and the request asked after it as one gate visit, and SHALL measure a fast decision from the request decided.

#### Scenario: Counted once
- **WHEN** a gate's request is withdrawn because its scope changed and a person decides the request asked after it
- **THEN** `apb stats` counts one gate visit and no withdrawn one

### Requirement: A gate's later visit counts from its decision
A request that follows a decision of the same gate SHALL count only what the run did after that decision.

#### Scenario: A second visit
- **WHEN** a gate is decided, the run loops back with one more commit and asks again
- **THEN** the second request counts 1 commit

### Requirement: An unread count is never zero
The scope SHALL count every commit the journal recorded in its window once, by its own change against its first parent, with files deduplicated by path and lines summed, whatever the branch looks like now. When git cannot read a recorded commit, the scope SHALL name each unread count as not read, and a `max_scope` limit on it SHALL count as passed.

#### Scenario: A range git cannot read
- **WHEN** the recorded commits are not in the run's working tree
- **THEN** the scope says the files and the lines were not read

#### Scenario: A binary file, a rename and a root commit
- **WHEN** the recorded commits add a binary file, rename a 3-line file and merge in an unrelated history
- **THEN** the binary file counts as a file with 0 lines, the rename as its two paths with +3 -3 lines, and the root commit against the empty tree

#### Scenario: A change put into a merge commit
- **WHEN** a node merges a side branch with no changes and adds 90 files in the merge commit itself
- **THEN** the scope counts the 90 files, and a gate with `max_scope: { files: 20 }` withholds approve

#### Scenario: History reset between nodes
- **WHEN** one node commits 90 files, another resets the branch to the base, and a third commits 1 file
- **THEN** the scope counts 2 commits and 91 files, and a gate with `max_scope: { files: 20 }` withholds approve

### Requirement: A gate withholds options above its max_scope
A gate with `max_scope` SHALL refuse, through every decision channel (a person-only or approving gate included) and again when the drive consumes a decision, an option of `max_scope.withhold` (left out: `approve` when the gate offers it) while the scope passes a limit, and every surface SHALL show the withheld options with the reason.

#### Scenario: Ninety files
- **WHEN** a gate with `max_scope: { files: 20 }` asks about a 90-file change
- **THEN** approve is refused and reject is accepted

#### Scenario: No surface offers a withheld option
- **WHEN** a gate's scope passes its `max_scope` and approve is withheld
- **THEN** the gate's next action, the CLI hint, the MCP action and the dashboard offer only reject as a decision, and name approve as withheld with the reason

#### Scenario: A person-only gate
- **WHEN** a person posts approve at a terminal to a person-only gate whose scope passes its `max_scope`
- **THEN** the decision is refused when posted and the gate keeps waiting

#### Scenario: A gate without approve
- **WHEN** a gate offers `[yes, no]` and its `max_scope` names no `withhold`
- **THEN** the validator reports that the limit withholds nothing (V243), never that the gate does not offer `approve`

#### Scenario: Unattended
- **WHEN** an unattended run declares the decision approve for a gate whose scope passes its `max_scope`
- **THEN** the run fails instead of approving

### Requirement: A decision faster than its scope is reported
A person's decision (never an engine's or a scenario's scripted one) SHALL count as decided without review when the active run time between the request and the decision is below a floor that grows with the scope; it SHALL be reported in `apb stats` and the run outcome and SHALL change no outcome, notification or routing.

#### Scenario: A two-second approval of a large change
- **WHEN** a person approves a gate with 40 changed files two seconds after the request
- **THEN** `apb stats` reports one decision without review and the run's outcome is unchanged

#### Scenario: A scripted decision on an ordinary gate
- **WHEN** a scenario's script decides a gate that is neither person-only nor approving, at once
- **THEN** the decision is journaled with its channel and never counts as decided without review

#### Scenario: Paused time is not reading time
- **WHEN** a gate stays open ten minutes of which the run was paused for all but two seconds
- **THEN** a decision then counts as decided without review

#### Scenario: A person decides after a withheld recommendation
- **WHEN** a model recommendation was applied at a gate but `max_scope` withheld its option, and a person decides two seconds after the request of a 40-file scope
- **THEN** the decision counts as the person's and as decided without review, read from the decision's own `decided_by` and channel

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
