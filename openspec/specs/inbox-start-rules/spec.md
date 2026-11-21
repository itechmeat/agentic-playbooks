# inbox-start-rules Specification

## Purpose
A start rule lets a person say, once, which inbox events of a connector account start a run of a named playbook, project or global, and with which params. Each matching event starts its run at once or is recorded with its reason; the rule's rendered key makes one run per event, and the run is always unattended. Rules are written and approved only by a person, live signed in their registered project, and are swept where events arrive, with a journal that explains every verdict in codes and never in values.

## Requirements

### Requirement: Only a person changes a start rule
A rule SHALL be created, changed, approved, enabled, disabled and removed only at an interactive terminal with the rule's name typed, or in a signed-in dashboard session; MCP and run steps SHALL be refused. Every rule SHALL need an approval for its current digest to start anything. The approval guards against mistakes and agents acting through apb's surfaces, not against another process of the same user that writes the signed file.

#### Scenario: A run step cannot write a rule
- **WHEN** `apb trigger set` runs with `APB_RUN_ID` set
- **THEN** it exits 2 with the run-step refusal and the rules file is unchanged

#### Scenario: A bearer key cannot disable a rule
- **WHEN** a request with a bearer key posts `/api/triggers/x/disable`
- **THEN** it answers 403 `trigger_requires_session` and the rule is unchanged

#### Scenario: A saved rule waits for its approval
- **WHEN** a person saves a rule and no one approves it
- **THEN** it starts nothing

### Requirement: Only enable and disable change whether a rule is enabled
A rule document SHALL NOT name `enabled`; `set` SHALL refuse one that does with `trigger_enabled_not_settable`. `enable` and `approve` SHALL delete the rule's state before they commit, so the next pass positions the rule at the head it reads then.

#### Scenario: A set while disabled
- **WHEN** a disabled rule at seq 100 receives event 101 and a person saves a changed rule naming `enabled: true`
- **THEN** the save is refused `trigger_enabled_not_settable` and event 101 never starts a run

### Requirement: A rule change waits for one start at most
Every change of a rule SHALL hold the rule's change lock and then its sweep lock through its writes. A sweep SHALL hold the sweep lock, SHALL yield before a start while the change lock is held, and SHALL start only while the rule it read is enabled and unchanged. No start of the old rule SHALL follow a committed change.

#### Scenario: A disable during a pass
- **WHEN** a person disables a rule while a pass is inside the first of three matching starts
- **THEN** the disable is stored once that start is recorded, and neither other event starts a run

### Requirement: A rule is checked before it is saved
A rule SHALL be saved only without errors among V260 to V267; warnings SHALL be saved and shown. A param that reads event text SHALL be declared `untrusted: true` (V262); a `pattern` SHALL only constrain its value further. A target param that is `secret: true` and has no default SHALL refuse the rule (V262): no rule can fill it, so no event could start the playbook. A rule SHALL have a budget with a token or dollar limit and `on_no_estimate` other than `ask` (V263).

#### Scenario: Event text into a param with a pattern only
- **WHEN** a rule maps `{{body.issue.title}}` to a param with `pattern: .*` and without `untrusted: true`
- **THEN** the save is refused with V262

#### Scenario: A secret param without a default
- **WHEN** the target playbook declares a `secret: true` param without a default
- **THEN** the save is refused with V262 saying the param is secret and has no default, so no event can start the playbook

#### Scenario: A rule without a budget
- **WHEN** neither the rule nor the playbook limits tokens or dollars
- **THEN** the save is refused with V263

#### Scenario: Pause with external data
- **WHEN** a rule whose runs are external data has an effective `on_no_estimate: pause`
- **THEN** it is saved with warning V267

#### Scenario: A draft target
- **WHEN** a rule pins a playbook version whose lifecycle is `draft` or `retired`
- **THEN** the save is refused with V261

### Requirement: Rules live in their registered project, signed
Rules and their state SHALL be stored in the project, signed with the installation key, bound to the project's registered workspace id, read with no link followed and git-ignored. A rule SHALL be saved only in a registered project. A file that does not verify, or belongs to another project, SHALL refuse every write and start nothing.

#### Scenario: An unregistered project
- **WHEN** `apb trigger set` runs in a project with no registered workspace id
- **THEN** it is refused `trigger_project_unregistered`, naming how to register, and nothing is written

#### Scenario: A rules file copied from another project
- **WHEN** a project's rules file was signed on this machine for another project
- **THEN** the sweep starts no run in that project and `apb doctor` names the file

### Requirement: A file that does not verify has one remedy
Every surface SHALL name a rules file that does not verify, or belongs to another project, with one remedy: move it aside to write rules again. No write can repair it, and `apb trigger set` SHALL say so before it asks anything.

#### Scenario: A set over a copied rules file
- **WHEN** `apb trigger set` runs in a project whose rules file was copied from another project
- **THEN** it is refused naming the file and saying to move it aside, and the file is unchanged

### Requirement: A rule name stays inside the trigger state
A rule name SHALL be checked as one safe path segment of the rule name pattern before it reaches any lock or store file; a name that is not one SHALL be answered as no such rule.

#### Scenario: A name that climbs out
- **WHEN** a person disables a rule named `../../outside`
- **THEN** it is refused as no such rule and no lock file is created outside `.apb/trigger-state`

### Requirement: A rule's state belongs to its incarnation
Each creation of a rule and each change of its connector or account SHALL give it a random incarnation binding its state, journal and start keys. A rule SHALL register no inbox consumer and SHALL journal one gap when retention removed events it had not seen. `approve` and `enable` SHALL delete the rule's position state; only `remove` SHALL delete a rule's journal; an orphan state or journal of an incarnation no rule names SHALL be inert and listed by `apb doctor`.

#### Scenario: A removed rule created again
- **WHEN** a rule is removed, the removal stops before its state is deleted, and a rule of the same name is created and approved
- **THEN** the new rule starts nothing from the old state, its approval deletes the old state entry, and the old journal stays inert and listed by `apb doctor`

### Requirement: A rule's position starts at the current head
A rule's position SHALL live only in its state and SHALL be written by a pass only: the first pass that finds an approved, enabled rule with a missing or foreign state SHALL position it at the head it reads then, with one journal line, never at an older seq, and SHALL start nothing for it in that pass. A pass SHALL read the rule's state only after it holds the rule's sweep lock, and SHALL position the rule only while its state is still missing or of another incarnation.

#### Scenario: A state removed long after
- **WHEN** a person removes an unreadable state eight days after an approved rule started a run for event 1, whose start key has expired
- **THEN** the next pass positions the rule at the current head and event 1 does not start again

#### Scenario: Events before the approval
- **WHEN** a rule is approved on an account holding 10 matching events
- **THEN** none of them starts a run

#### Scenario: The first pass after approval
- **WHEN** an event arrives after `approve` and before the next pass
- **THEN** that pass positions the rule past it and starts nothing

#### Scenario: A changed account
- **WHEN** a rule whose account A started a run for seq 11 is moved to account B, approved, and B's seq 11 matches with another key
- **THEN** a second run starts and each start has its own journal line

#### Scenario: No consumer
- **WHEN** a rule has swept an account
- **THEN** the account's cursors name no consumer the rule registered

### Requirement: Each event starts now or is recorded
A sweep SHALL evaluate a rule's events in seq order and, for each, start its run at once or record its verdict, moving the position past it; nothing SHALL be queued or merged. A busy start key, the hourly cap and a changed approval binding SHALL leave the position before the event and end the pass. The cap SHALL be read from launch records before each new start; an unattributable run or an unreadable runs directory holds it. Every refusal SHALL be the event's verdict.

#### Scenario: The hourly cap
- **WHEN** a rule with `max_per_hour: 2` started 2 runs in the last hour and a third event matches
- **THEN** it is recorded `held` once, the position stays before it, and it starts once the first start is an hour old

#### Scenario: An unreadable runs directory
- **WHEN** the project's runs directory cannot be read
- **THEN** a matching event is held as at a reached cap and starts once it reads again

#### Scenario: A start lost to a crash
- **WHEN** a rule with `max_per_hour: 1` started a run, crashed before recording it, and retention removed the event
- **THEN** a new matching event within the hour is held and not started

#### Scenario: A refusing concurrency block
- **WHEN** the playbook's `concurrency:` block says `refuse` and its key is held
- **THEN** the event is recorded `refused` `concurrency_busy` and the next matching event is evaluated

### Requirement: A pass that ends early resumes before its event
A pass that ends early (a waiting change, the per-pass start bound, a journal write error, a rule that changed under it) SHALL leave the position before the event it did not decide, and the next pass SHALL decide that event.

#### Scenario: The per-pass start bound
- **WHEN** a pass has started as many runs as one pass may and another event matches
- **THEN** the pass ends with no line for that event and the next pass starts it

### Requirement: The hourly cap fails closed
A run directory of the rule whose journal is missing or does not read SHALL count as a start inside the hour when its launch record names the rule's incarnation; a directory SHALL be left out only when its remaining records prove the incarnation did not start it.

#### Scenario: A run whose journal is lost
- **WHEN** a rule with `max_per_hour: 1` started a run whose journal is gone while its launch record remains, and a new event matches
- **THEN** the event is held at the cap

### Requirement: The start key decides before any limit
A start key bound to a started run SHALL answer `already_started` before the hourly cap and the per-pass start bound are applied. A start key whose entry another start still prepares, not bound to a started run, SHALL hold the event as a busy key, the same request or another one: that run may yet never start.

#### Scenario: A start still being prepared
- **WHEN** another start with the event's key holds its claim and has not started its run yet
- **THEN** the event is held `start_key_busy`, the position stays before it, and once that start is bound the next pass records `already_started`

### Requirement: Conditions are decided in a fixed order
An event SHALL start a run only when its type is in the rule's `types`, every `where` condition holds, its body carries no apb send marker, its key renders to a valid start key and every param renders. The first condition that fails SHALL be the reason recorded; a missing body path and a value that is not a scalar SHALL be told apart. A send marker SHALL count only when its run is one safe path segment naming a run directory of this project this installation created.

#### Scenario: An event apb itself caused
- **WHEN** a string the rule reads ends with the exact hidden send marker of a run this installation created
- **THEN** it is skipped with `own_send`, while the keyword quoted elsewhere or a marker in a field the rule does not read skips nothing

#### Scenario: A marker naming another project's run
- **WHEN** a string the rule reads ends with a send marker whose run is the absolute path of another project's run
- **THEN** it is not the rule's own send and the event is evaluated as any other

#### Scenario: A missing body path
- **WHEN** the key template reads a path the body does not have
- **THEN** the event is skipped with code `key_missing` naming the template's path

### Requirement: One run per rendered key
A start's key SHALL be `trigger:<incarnation>:<rendered key>` and the only dedupe, for as long as start keys live (`runs.start_key_days`). A delivery resent with a new provider id, a start repeated after a crash, or the same key with other params SHALL be recorded `already_started` with the run the key names.

#### Scenario: A resent delivery
- **WHEN** two stored events with different provider ids render the same key
- **THEN** one run starts and the second event is recorded `already_started`

#### Scenario: A crash before the outcome was recorded
- **WHEN** the sweeper died after the run started and before it recorded the outcome
- **THEN** the next pass records `already_started` with that run and starts nothing, even when that run reached the hourly cap, and the cap counts that run from its launch record

### Requirement: A rule start goes through the run gate as a rule
A rule's start SHALL go through the one start sequence with `Initiator::Trigger`: never a person, decided like the CLI without a terminal, never acknowledging an untrusted playbook, never confirming billed use (`billing_refused_for_trigger`). An `ask` launch and irreversible effects SHALL need the rule's approval for this exact permit and rule digest; `deny` SHALL refuse.

#### Scenario: An ask playbook without approval
- **WHEN** a rule's playbook has `agent_launch: ask` and the rule is not approved
- **THEN** the start is refused `launch_requires_rule_approval` and nothing starts

#### Scenario: An approval after a profile edit
- **WHEN** a profile of the playbook changed after the approval
- **THEN** the start is refused until a person approves the rule again

#### Scenario: A global playbook of a project playbook's id
- **WHEN** an approved rule names a global playbook whose id a project playbook also has, and a matching event arrives
- **THEN** the global playbook runs in the project, the run names the global origin, and the next event of the same key answers that run

### Requirement: An approval names the rule it approves
An approval SHALL carry the rule's digest and one binding over its target's permit digest, launch policy in force and full billing decision, both compared with a fresh need at approve. A rule SHALL never start a run under a binding other than the approved one: the binding SHALL be compared in exactly one place, admission of an attempted start, and a start attempted under a changed binding SHALL revoke the approval durably after its journal line.

#### Scenario: A stricter policy after approval
- **WHEN** a rule approved under `agent_launch: allow` meets a raised launch floor of `ask` and a matching event attempts a start
- **THEN** nothing starts, one `binding_changed` line is journaled and the approval is cleared

#### Scenario: A binding restored before any start
- **WHEN** the binding changes and is restored while only events the start key answers `already_started` arrive
- **THEN** nothing is revoked and the next new key starts under the approved binding

#### Scenario: Billing denied, then allowed again
- **WHEN** a rule approved under billing `allow` meets billing `deny` while a matching event attempts a start, and billing is later set back to `allow`
- **THEN** the approval is revoked at the deny and does not come back

#### Scenario: A profile changed after approval
- **WHEN** an approved rule's playbook allows launch and one of its profiles is edited and trusted
- **THEN** the next matching event starts no run, one `binding_changed` line is journaled, the approval is cleared, and restoring the profile does not bring it back

#### Scenario: A rule edited between showing and approving
- **WHEN** a rule's params change after its approval need was shown and before it is approved
- **THEN** the approval is refused `trigger_approval_changed` and nothing is stored

### Requirement: A rule's run is always unattended
A rule's run SHALL be unattended, with probation never waived and, with `require_ready`, the readiness check, both decided in preparation as for any unattended start; a rule has no attended option. Params SHALL get the pattern check of every start; a rule's run SHALL be flagged `external_data` unless the rule sets `external_data: false`.

#### Scenario: A gate that needs a person
- **WHEN** a rule's playbook reaches a gate only a person can decide
- **THEN** the run fails there with `hitl_required`, as any unattended run does

#### Scenario: A param that misses its pattern
- **WHEN** a rendered `issue` value does not match the param's pattern
- **THEN** the start is refused `param_pattern_mismatch` and the journal names the param, not the value

#### Scenario: Probation not done
- **WHEN** an unattended rule's playbook asks for one attended success and has none in this project
- **THEN** the start is refused `probation_required` and nothing starts

### Requirement: A rule's run never earns probation credit
No run of a tree a rule started SHALL count as an attended success toward a playbook's probation: every such run is unattended, its children inheriting the mode. A rule's run with `external_data: false` SHALL leave a ledger entry with `unattended: true`; a default external-data rule run SHALL leave no ledger entry.

#### Scenario: A rule run that succeeds
- **WHEN** a rule with `external_data: false` has a run that succeeds, with a sub-playbook
- **THEN** both ledger entries are `unattended: true` and the playbook's count of attended successes does not change

#### Scenario: A default rule run
- **WHEN** a rule with the default `external_data` has a run that succeeds
- **THEN** no probation entry is written for it

### Requirement: A rule's run keeps its driver
A rule's run SHALL keep its driver on a long timer wait and on a deferred retry, as any unattended run does.

#### Scenario: A rule's run on a six hour wait
- **WHEN** a rule's run reaches a timer wait of 6 hours
- **THEN** the driver keeps waiting and the journal holds no `run_paused`

### Requirement: The trigger journal explains every verdict
Every evaluated event, held event, start, refusal, position change and gap SHALL be recorded in the rule's trigger journal as codes, names from the rule and per condition its index and whether it passed, never a value, a condition literal or an engine sentence, bounded to 1000 entries, with its time in milliseconds. Each line SHALL be appended and synced, after any torn tail is repaired, before the state change it explains commits; a repeated id SHALL appear once.

#### Scenario: Why it did not start
- **WHEN** an event fails a `where` condition
- **THEN** `apb trigger log` shows it `skipped` with each condition's index and whether it passed, not the condition's literal

#### Scenario: A crash after the line
- **WHEN** the sweeper died after syncing an event's line and before moving the position
- **THEN** the next pass evaluates the event again and the journal shows its line once

#### Scenario: A torn tail
- **WHEN** the sweeper died while writing a line
- **THEN** the next append cuts the partial line first and every complete line stays readable

#### Scenario: A value in a refusal
- **WHEN** a start is refused `concurrency_busy` for a key that renders an event value
- **THEN** the journal holds the code and not the value

### Requirement: A person reads why from the journal
`apb trigger log` and `apb trigger show` SHALL print each journal line with its event seq, its words, its condition marks, its param name, its validator codes and its run id, through the one renderer the dashboard mirrors.

#### Scenario: A refusal by the rule's own checks
- **WHEN** an event is refused because the rule no longer passes V262
- **THEN** `apb trigger log` shows the event's seq, that the rule no longer passes its own checks, and `V262`

### Requirement: A journal line is durable before it is relied on
A line the journal already holds (a replay of an id whose sync may have failed, or a tail the repair completed) SHALL be synced again, file and directories, before the state change it justifies commits. The journal SHALL be read and written with no link followed.

#### Scenario: A replay after a failed sync
- **WHEN** a line was written but its sync failed, and the next pass repeats the same verdict
- **THEN** the line is synced before the position moves and appears once

#### Scenario: A journal that is a link
- **WHEN** the rule's journal file is a link to another file
- **THEN** nothing is read through it, its target is unchanged, and no position moves

#### Scenario: The first line after a failed directory sync
- **WHEN** a first positioning's directory sync fails and the retry writes a line of another id
- **THEN** the directory is synced before the position commits

### Requirement: A refused journal stops its rule and names the way out
A journal that cannot be read (over its cap, a link, a hard link, not a regular file, or a link at a component of its path) SHALL stop its rule: the pass SHALL evaluate nothing for it, start nothing for it and move no position, while the project's other rules go on. Every surface SHALL name the refusal in one sentence with its remedy: removing the rule and saving it again, or deleting the journal file, for the journal itself; replacing the link with a directory for a component of its path.

#### Scenario: A journal over its cap
- **WHEN** an approved rule's journal is over its read cap and a matching event arrives
- **THEN** two passes start nothing for that rule, its position stays and its journal does not grow, and another rule of the project still starts its run

#### Scenario: A link at a component
- **WHEN** `trigger-state/log` is a link
- **THEN** the rule's view names `log` and says to replace it with a directory, never to save the rule again

### Requirement: A rules save never makes the store unreadable
A save, an update or an approval whose signed rules file would exceed the reader's limit SHALL be refused `rules_store_full`, leaving the stored file as it was.

#### Scenario: The rule past the cap
- **WHEN** rules are saved up to the cap and one more is saved
- **THEN** the save is refused `rules_store_full`, and every earlier rule is still read, swept, approved, disabled and removed

### Requirement: A removal never fails after it removed
Once a rule's entry is removed, a state entry or a journal file that cannot be deleted SHALL be reported on the answer (`state_left`, `journal_left`) as a warning on every surface, never as an error, and nothing SHALL be deleted through a link.

#### Scenario: A journal directory that is a link
- **WHEN** `trigger-state/log` is a link to another directory holding the rule's journal name and the rule is removed
- **THEN** the removal succeeds with `journal_left`, and the file in the other directory is unchanged

### Requirement: A test explains one event and starts nothing
`apb trigger test <rule> [--event <seq>]` and MCP `trigger_test` SHALL report the filter's verdict, the rendered params and key, whether the key was used (as the start's own key lookup answers it), and the result of the checks a surface runs before preparation, and SHALL name probation, readiness, concurrency and the working tree as checked at start, never saying "would start". It SHALL write nothing and work in a read-only MCP process.

#### Scenario: A test writes nothing
- **WHEN** `trigger_test` runs on a matching event in a read-only MCP process
- **THEN** the project tree and the config directory are unchanged and the answer holds no rendered value

#### Scenario: From a run step
- **WHEN** `trigger_test` runs with `APB_RUN_ID` set
- **THEN** it is refused `trigger_test_refused_in_run_step`

#### Scenario: Probation not done
- **WHEN** a test runs on an event whose start would be refused `probation_required`
- **THEN** the report passes the checks before start and lists probation as checked at start

### Requirement: The sweep runs where events arrive
The trigger sweep SHALL run inside the ingest listener, every 5 seconds and at once after a stored delivery, once per process, over the registered projects, and once in `apb resume --all`; it SHALL never run inside a run step, and no other process SHALL be started for it. Two sweepers SHALL never start one event twice. `apb doctor` SHALL warn when enabled rules exist and no sweep is live.

#### Scenario: Two hosts
- **WHEN** the listener's sweep and `apb resume --all` sweep the same project while an event matches
- **THEN** one run starts

#### Scenario: A sweeper that waited on another rule
- **WHEN** one sweeper holds a rule's lock while another positions a second rule and starts an event of it, and a later event arrives
- **THEN** the first sweeper, reaching the second rule, evaluates only the later event and starts it once

#### Scenario: Inside a run step
- **WHEN** `apb resume --all` runs with `APB_RUN_ID` set while an event matches
- **THEN** no rule is swept, no journal line is written and no position moves

### Requirement: The run names its rule
A rule's run SHALL carry `triggered_by` in its launch record as an optional field, and SHALL otherwise be an ordinary unattended run with no mark for older binaries. Every run surface SHALL show the rule; a run whose record lost `triggered_by` SHALL show no rule. A rule start whose preparation fails SHALL be handled as any failed start.

#### Scenario: A started run
- **WHEN** a rule starts a run
- **THEN** `apb runs <id>` prints "started by rule" with the rule's name and the event

#### Scenario: An older apb
- **WHEN** an apb that does not know `triggered_by` resumes a rule's run
- **THEN** it drives it as an ordinary unattended run
