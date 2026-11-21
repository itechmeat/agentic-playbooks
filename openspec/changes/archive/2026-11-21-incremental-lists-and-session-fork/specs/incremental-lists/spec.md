## ADDED Requirements

### Requirement: A list skips items that passed in the previous success
A list node with `skip` SHALL settle as passed, without running it, each item whose id passed in the one earlier run `run.previous_success` resolves (through its existing checked lookup, or the window the run recorded) with the same skip digest. A skipped item SHALL keep its skip digest, so a later run skips it again while the digest holds.

#### Scenario: Only new advisories run
- **WHEN** yesterday's successful run passed items 1 and 2 and today's list holds items 1, 2 and 3 unchanged
- **THEN** items 1 and 2 are settled passed with `skipped_from` naming yesterday's run and only item 3 runs

#### Scenario: The chain continues
- **WHEN** an item was skipped in the previous success
- **THEN** the next run skips it again while its skip digest is unchanged

### Requirement: The skip digest covers the item and the configured work
The skip digest SHALL cover the item id, the item data (the whole item, or the `unless_changed` fields) and the run's work fingerprint, computed when the item settles. The work fingerprint SHALL cover everything the run was configured with, taken whole (D2 "What WorkInputs covers" lists the parts).

#### Scenario: A changed field
- **WHEN** item 2's `severity` changed and the skip says `unless_changed: [severity]`
- **THEN** item 2 runs again

#### Scenario: Any configured input
- **WHEN** only one configured input changed since the previous success (a new playbook version, a script, a param, the instruction, a profile, an invocation form, a connector account, a decision setting or the working tree)
- **THEN** every item runs again once

#### Scenario: A judge turned to enforce
- **WHEN** items were accepted while the judge use was off and the machine's decision file now enforces it
- **THEN** every item runs again

#### Scenario: An edited inline list
- **WHEN** a new version adds an item to an inline list
- **THEN** every item runs again, since the definition changed

### Requirement: The configured parts
The work fingerprint SHALL cover the definition it executes whole, inline items included, the scripts tree, the run's start request whole (params, instruction, context and output limits and every other field that can change work), the manifest's execution block and what the manifest resolved for the node, the effective decision configuration and the identity of the resolved working tree.

#### Scenario: A program-only change
- **WHEN** only the machine's configured program for the list's agent changed since the previous success
- **THEN** every item runs again

#### Scenario: An absent field becomes null
- **WHEN** an item had no `amount` field and now has `"amount": null`, with `unless_changed: [amount]`
- **THEN** the item runs again

#### Scenario: A context limit
- **WHEN** only `context_max_bytes` changed since the previous success
- **THEN** every item runs again

### Requirement: The resolved executor
What the manifest resolved for the node SHALL include every resolved invocation of its profile's chain and tiers, the canonical executable included and the executable fingerprint excluded.

#### Scenario: A changed program path
- **WHEN** only the configured program path of the list's agent changed since the previous success
- **THEN** every item runs again

### Requirement: The world stays outside the fingerprint
The work fingerprint SHALL NOT cover the file contents of the workspace, the outputs of earlier nodes, the time, the previous run's own state, the item's own data, the known-problems section of a prompt or the agent CLI's executable fingerprint, and every sensitive value SHALL enter only as an installation-keyed MAC.

#### Scenario: An agent CLI updated in place
- **WHEN** the agent binary at the same path is updated in place since the previous success and nothing else changed
- **THEN** the items that passed are skipped

#### Scenario: A credential in a plain param
- **WHEN** a plain param holds a credential-shaped value
- **THEN** the work inputs hold its keyed tag and never the value

#### Scenario: A secret param
- **WHEN** a run's params include a secret value
- **THEN** the work inputs hold its keyed tag and never the value or a bare hash of it

### Requirement: Some visits skip nothing
A visit's skip decision SHALL be computed before its `list_started` is appended and stored in it, and SHALL never be recomputed by the opening steps or recovery. A visit SHALL skip nothing, record the reason on its `list_started` and stamp no skip digest when a supervisor note is in force for the node, when the run's scripts copy differs from the digest pinned at start, or when the work inputs cannot be computed.

#### Scenario: A supervisor note
- **WHEN** a supervisor note is applied before the list's visit
- **THEN** every item works and the visit records `skip_supervisor_note`

#### Scenario: A worktree from a node output
- **WHEN** the playbook's working tree comes from an earlier node's output with a new path each run
- **THEN** every item works in every run

### Requirement: The scripts pin is checked before skipping
The scripts pin SHALL be checked again right before skip settlements are written, in the opening steps and in their recovery, and a difference SHALL fail the visit with `list_skip_scripts_changed` with no item settled as skipped.

#### Scenario: A rewrite after a crash
- **WHEN** a drive dies right after `list_started` and another node rewrites the run's scripts copy before the resume
- **THEN** the resumed visit fails with `list_skip_scripts_changed` and no item is settled as skipped

### Requirement: The skip needs only the manifest its work depends on
A node whose work depends on a profile or a connector and that has no execution manifest SHALL refuse its skip by name and work every item; a script list node with no connector SHALL skip without a manifest.

#### Scenario: A script list without a manifest
- **WHEN** a script list node with no connector runs in a run that has no execution manifest
- **THEN** its items carry skip digests and the next run skips them

### Requirement: The set is read once and journaled per node
The skip set SHALL be read once and journaled as one bounded `list_skip_read` per list node with `skip`, all of a run's lines in one journal write; every visit and resume SHALL read the journaled sets only. Child, scenario and eval runs SHALL record empty sets.

#### Scenario: A resume after another success
- **WHEN** a run is resumed after another run of the playbook passed more items
- **THEN** it skips exactly the items of its own journaled set

### Requirement: Patches and rebinds keep the skip honest
A supervisor patch that adds a `skip` the run did not read SHALL be refused. A run that already settled items by a skip SHALL refuse every in-run patch with `list_skip_work_changed`, and a rebind that changes the work fingerprint of such a node, its resolved invocations included, SHALL be refused, while tier routing follows main; a run without such settlements SHALL be patched as on main.

#### Scenario: A patch after a skip
- **WHEN** any patch, a description-only one included, arrives after the run skipped items
- **THEN** the patch is refused with `list_skip_work_changed`

#### Scenario: A patch before any skip
- **WHEN** a patch arrives before the run skipped any item
- **THEN** it is applied and the node's next visit skips nothing

#### Scenario: A patch that adds a skip
- **WHEN** a supervisor patch adds `skip` to a list node of a run that read no set for it
- **THEN** the patch is refused with `list_skip_unrecorded`

### Requirement: A drive records missing sets before it drives
A run that declares `skip` and journaled no set when a drive begins SHALL read and record every set before any node runs. A run that journaled some sets but not all SHALL be refused with `list_skip_incomplete`, an empty set SHALL never be substituted for a missing one, the check and the write SHALL be one decision under the required journal lock, and a run whose type cannot be told SHALL record its sets refused and read no history.

#### Scenario: A queued run paused before its turn
- **WHEN** a queued run is paused before its turn and then resumed
- **THEN** it records its sets in one write before its first node

#### Scenario: A continuation of a partial record
- **WHEN** a run is continued from a predecessor whose journal holds the set of one of its two skipping lists
- **THEN** the continuation is refused with `list_skip_incomplete` and leaves no run

#### Scenario: A partial record
- **WHEN** a run's journal holds the set of one skipping list node but not of the other
- **THEN** a resume refuses with `list_skip_incomplete`

### Requirement: The set write fails closed on its lock
A set write whose journal lock cannot be taken within its bounded wait SHALL be refused `list_skip_lock_unavailable` with nothing appended and no node run; a full disk SHALL be refused as `list_skip_disk_full`, and no other I/O error SHALL be named a lock failure.

#### Scenario: A held journal lock
- **WHEN** another holder keeps the journal lock while the sets are written
- **THEN** the write waits its bounded time, is refused `list_skip_lock_unavailable`, and the journal is unchanged

### Requirement: Where a start reads its set
A start that takes no concurrency slot SHALL read its set before its run directory exists, and an unreadable history SHALL refuse it with `list_skip_unreadable`, leaving no run. A start that takes a slot SHALL NOT read before admission: admitted at once, it SHALL read right after admission and a refusal SHALL leave no run; queued, it SHALL read once after its turn, and a failed read SHALL end the run failed with the reason `list_skip_unreadable`, as a failed `run.previous_success` lookup does.

#### Scenario: A queued run
- **WHEN** a start queues behind another run of its concurrency key and a run succeeds before its turn
- **THEN** it reads its set after its turn, from that newer success

#### Scenario: A queued run whose history does not read
- **WHEN** the earlier run's journal does not read when a queued run's turn comes
- **THEN** the run ends failed with the reason `list_skip_unreadable`

#### Scenario: An unreadable history at start
- **WHEN** a start without a concurrency slot finds that the earlier run's journal does not read
- **THEN** the start is refused with `list_skip_unreadable` and no run directory remains

### Requirement: The skip is an opening step of the visit
Skipped items SHALL be settled as an opening step of a new visit, before any pilot slice or batch, and a drive that died after the visit started SHALL skip the same items when the opening steps are completed. A skipped item SHALL never be in a pilot slice.

#### Scenario: A pilot over a skipping list
- **WHEN** a list with `pilot: { items: 2 }` skips its first three items
- **THEN** the pilot slice holds the first two items that were not skipped

### Requirement: A misspelled unless_changed field skips nothing
A visit SHALL skip nothing and stamp nothing, recording `skip_unless_changed_absent` with the field's name, when a field `unless_changed` names is held by no item of the visit.

#### Scenario: A misspelled unless_changed field
- **WHEN** `unless_changed` names a field no item of the visit has
- **THEN** every item works, the visit records `skip_unless_changed_absent` naming the field, and no item is stamped

### Requirement: Skipped rows are decided again in every visit
A skipped row SHALL never be carried into a later visit: `rerun: not_passed` SHALL carry only rows that were worked and passed, as on main, and each visit's skip step SHALL decide the earlier skipped rows again under that visit's recorded decision. A skip digest SHALL be minted only by a fresh settlement whose step executed in the visit, under the stamp rule below. `stop_when` SHALL leave every skipped row out of its share; carried rows SHALL count as on main.

#### Scenario: A note between visits
- **WHEN** a supervisor note is applied between two visits of a looping list and the first visit skipped an item
- **THEN** the second visit works that item

### Requirement: One rule mints a skip digest
A skip digest SHALL be minted only when the item's work ran under exactly what apb configured for it and the work fingerprint covers and its item check ran to a verdict and passed. No settlement SHALL be stamped while a supervisor note is in force for the run (a reopen, a resume and a supervisor retry included), after a pilot's revised guidance is applied, when its item check did not run to a verdict, or when the scripts differ from the pin.

#### Scenario: A reopen under a supervisor note
- **WHEN** a supervisor retry reopens a failed visit after a supervisor note was applied
- **THEN** the items reworked under the note carry no skip digest and the next run works them

#### Scenario: A revised pilot
- **WHEN** a pilot gate is answered `revise` with guidance and the visit goes on
- **THEN** no item worked after the revise carries a skip digest

#### Scenario: A judge that was unavailable
- **WHEN** an item's judge check is unavailable and the item is accepted with a note
- **THEN** the item carries no skip digest and the next run works it

#### Scenario: Restored scripts
- **WHEN** a visit ran under changed scripts and the pinned scripts are restored before the next visit, which carries a worked item
- **THEN** the carried item has no skip digest and the next run works it

### Requirement: Skipped items count as done
A skipped item SHALL count as passed and processed, and SHALL be counted as `skipped` in the node output, `apb runs <id>`, MCP `run_status`, the dashboard and the HTML report. Its output SHALL be null and its row SHALL name the run it passed in. A list without `skip` SHALL write no `skipped` field, so its journal is what main writes.

#### Scenario: A list goal
- **WHEN** a goal criterion `{ type: list, node: work }` checks a visit where every item was skipped
- **THEN** the criterion passes

#### Scenario: A plain list
- **WHEN** a list without `skip` runs
- **THEN** its journal equals the one main writes for it

### Requirement: A run that worked nothing earns no probation credit
A run SHALL earn attended probation credit only when every list node that skipped at least one item in the run has at least one fresh passed settlement whose step ran in a batch (not skipped, not carried, not settled without execution, not restored from the node cache).

#### Scenario: Everything skipped
- **WHEN** an attended run succeeds and its skipping list skipped every item
- **THEN** its probation entry is not eligible

#### Scenario: A skipping list never reached
- **WHEN** an attended run succeeds without reaching its skipping list
- **THEN** its probation entry stays eligible

#### Scenario: A cache restore
- **WHEN** the only passed item of a skipping list follows a node cache hit of the node in the visit
- **THEN** its probation entry is not eligible

#### Scenario: Only an oversized item
- **WHEN** a skipping list skipped every item but one oversized item settled needs look without running, and the list continues
- **THEN** its probation entry is not eligible

### Requirement: V283 and the guard
The validator SHALL report V283 for an empty `unless_changed`, a repeated field, or a field that is empty, dotted or has whitespace. A run of a playbook with any `skip` SHALL carry a `run_guard` naming `list_skip`.

#### Scenario: A dotted field
- **WHEN** a skip says `unless_changed: [meta.stage]`
- **THEN** validation fails with V283

### Requirement: The work is what executes
The work fingerprint SHALL take the directory a list node's step executes in as the canonical effective directory (the run's tree or the project root it falls back to, under the node's own `workdir`) and every script runtime its step and checks run as the run resolved it (the runner key, the configured candidates, the candidate chosen and the identity of the program it resolves to).

#### Scenario: A changed runtime
- **WHEN** the machine config moves the `sh` runner to another runtime between two runs
- **THEN** every item runs again, under the new runtime

#### Scenario: A copied project
- **WHEN** a project with its `.apb` is copied to another directory and run there
- **THEN** the earlier run is read and every item works

### Requirement: An item executes the value its work names
The directory, runtimes, check directory, `PATH`, check policy and MCP pins of a list execution SHALL be resolved once into one value, the work SHALL be computed from that value, and every item of a skip visit SHALL execute with that value, never resolving it again.

#### Scenario: A relative runner
- **WHEN** a configured runner is a relative path and the node's own directory holds another file of that name
- **THEN** the item runs the program taken against the project root

#### Scenario: A workdir link switched during a visit
- **WHEN** an item points the node's `workdir` link at another directory while the visit runs
- **THEN** the next item still runs in the directory the visit resolved

### Requirement: Scripts are checked at both ends of every item
A stamp SHALL be written only when the scripts copy at the item's settlement (after its work and its check) equals the pinned digest, the copy SHALL also be checked as each item's work starts, and once any end of any item saw a difference the visit SHALL stamp nothing after it.

#### Scenario: A rewrite after another item's settlement
- **WHEN** item a settles with its stamp and item b then rewrites the scripts copy, runs it and passes
- **THEN** b carries no stamp and the next run works b

### Requirement: The consumed journal is the judged journal
The source journal a skip consumes SHALL be judged as an eligible earlier success on the same read that consumes it, on the window path and the lookup path alike, and a source that is not SHALL give no skip, every skipping node recording `skip_source_ineligible`.

#### Scenario: A source reopened after the lookup
- **WHEN** the journal the sets are read from shows the source resumed after its success
- **THEN** no item is taken from it and every skipping node records `skip_source_ineligible`

### Requirement: A skipped item is never executed evidence
Skipped items SHALL be left out of the item check evidence and of its freshness; inherited passes SHALL be told apart with their source run, and a list whose every item was skipped SHALL give no executed check evidence.

#### Scenario: An all-skipped list
- **WHEN** run A checks two items with a script and run B skips both
- **THEN** B's outcome has no item check evidence and its list line names the items as passed in A, not checked again

### Requirement: A skipped item carries its output
A skipped item SHALL carry the output its source settlement recorded, taken from the same journal read the set comes from, of at most 2 KiB per item and within the set's 192 KiB; an item whose source output was clipped, holds a redaction marker or does not fit SHALL be worked and counted on the skip line, and an item whose source produced no output SHALL carry none.

#### Scenario: A chain of skipping lists
- **WHEN** list A skips its items and list B takes its items from A's output
- **THEN** B's items equal those of the run A's items passed in, and B skips them

#### Scenario: An output over the cap
- **WHEN** an item's output was clipped when it settled
- **THEN** a later run works the item and the skip line counts it

### Requirement: The scripts copy of a skipping run is read-only
In a run whose definition declares a list `skip`, the scripts copy SHALL be read-only exactly while a drive runs it and writable again on every exit of the drive; a run without `skip` SHALL keep the copy's modes.

#### Scenario: A script that writes next to itself
- **WHEN** an item's script writes a file under the run's scripts copy, not as root
- **THEN** that write fails and nothing latches

### Requirement: A latch is shown
When a visit stops stamping because the scripts copy differed from the pin, the reason SHALL be journaled once per list execution and shown on the skip line of every surface.

#### Scenario: A deliberate rewrite
- **WHEN** an item changes the copy's mode and adds a file
- **THEN** the next settlement journals `skip_scripts_diverged` and the list line says stamping stopped

### Requirement: A skipped row holds only under its work
A continued or reopened visit SHALL be decided by the rule a new visit is decided on whether it may hold skips; when it may not, every skipped row SHALL go back to pending (`skip_supervisor_note` under a note, else `skip_config_changed`); when it may, a skipped row whose skip digest no longer matches the work SHALL go back to pending with `skip_config_changed`. A row taken back SHALL be worked and counted by the skip line.

#### Scenario: A runtime changed while a visit was paused
- **WHEN** a visit skipped item a, was paused, the machine's runner changed and the run was resumed in a new process
- **THEN** item a is worked under the new runner and the skip line counts it

#### Scenario: A note before a supervisor retry
- **WHEN** a visit skipped item a while item b needed a look, then a supervisor note was applied and the visit was reopened by a retry
- **THEN** item a goes back to pending with `skip_supervisor_note` and is worked under the note

### Requirement: A row taken back keeps nothing it inherited
A skipped row taken back SHALL keep no inherited output and no skip provenance, so a later settlement without output leaves it without output.

#### Scenario: A row taken back that prints nothing
- **WHEN** a skipped row with an inherited output is taken back and its new execution prints no output
- **THEN** the row passes with no output, and while it waits to be worked it shows pending with no output

### Requirement: No skip on isolated nodes
The validator SHALL report V288 for `list.skip` on a node whose `isolation` is `full` or `best_effort`, with a sentence that says skip on isolated nodes is not supported yet, so validation, every save and run start refuse it, and an isolated execution SHALL never write a stamp.

#### Scenario: Skip on a best_effort node
- **WHEN** a list node declares `skip` and `isolation: { level: best_effort }`
- **THEN** validation fails with V288 and a run start is refused

#### Scenario: Skip on a node with isolation none
- **WHEN** a list node declares `skip` and `isolation: { level: none }`
- **THEN** the definition is valid and items that passed before are skipped

### Requirement: The check policy is part of the work
The item checks' policy flags the visit derives from the run's options (whether a check must leave the tree as it found it) SHALL be part of the work and the checks SHALL run from them.

#### Scenario: Another worker count
- **WHEN** a list's checks ran with two workers and a later run uses one
- **THEN** no item is skipped

### Requirement: What the work fingerprint claims
The work fingerprint SHALL cover what apb configures for an item, the directory its check runs in and the `PATH` its children receive included, and SHALL NOT be described as covering the machine around apb: other inherited environment variables, the values secrets resolve to, files outside the scripts copy and the state of outside services.

#### Scenario: A different worktree for the check
- **WHEN** a list's node runs in a fixed directory and its check runs in the run's worktree, and a later run names another worktree
- **THEN** no item is skipped

#### Scenario: Another PATH
- **WHEN** a later run's `PATH` selects another program of the same name
- **THEN** no item is skipped and the item runs that program
