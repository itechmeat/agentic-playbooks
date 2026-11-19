# review-scope Specification

## Purpose
A review gate that uses a form feature shows what the decision covers as counts, can withhold options above a size limit, and reports decisions made faster than their scope takes to read.

## Requirements

### Requirement: A form gate's request shows what the decision covers
The request of a review gate that uses a form feature (`max_scope`, `fields` or `show`) SHALL journal, in the same write as the request, the scope since the gate's previous decision on the same node or since the run's start: commits, changed files and added and removed lines of those commits, list items settled, connector writes, send intents and result snapshots, as counts only. Whether a gate needs this detail SHALL be decided by one function over the gate's definition that the writer and every reader share.

#### Scenario: A gate before publish
- **WHEN** three commits changing 6 files by 41 added and 38 removed lines precede a gate with `max_scope`
- **THEN** the pending review shows "3 commits, 6 files, +41 -38 lines"

#### Scenario: A plain gate keeps the journal of an older apb
- **WHEN** a run waits at a `human_review` without `max_scope`, `fields` or `show`, or at a publish consent
- **THEN** its journal holds the `review_requested` alone, with no `review_detail`, so an older apb still resumes the run

#### Scenario: An uncommitted change is not in the scope
- **WHEN** a node leaves a modified file it did not commit
- **THEN** that file is not counted

#### Scenario: Counts only
- **WHEN** a committed file is named secret-name.txt
- **THEN** no journal line contains that name

#### Scenario: A request without its recorded scope
- **WHEN** the open request of a gate that uses a form feature has no recorded detail (a write cut by a crash right after the request's line)
- **THEN** a decision is refused (on a gate with `max_scope`, naming the options it may withhold) until the resumed drive records the detail, which it does before it posts or waits for a decision

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
