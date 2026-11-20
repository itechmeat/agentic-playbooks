## ADDED Requirements

### Requirement: The fit report says whether a playbook may run alone
The fit report SHALL carry a readiness section: done is defined by a `script` goal criterion under `goal.enforce: true`, every acting `agent_task` has a deterministic check, every acting node has a boundary in each effect domain it acts in, a budget is set, every irreversible step is behind a gate on every path a run can take, and the probation is done; it SHALL be shown by `apb adopt`, MCP `playbook_adopt_report` and the dashboard fit card.

#### Scenario: A writing node without a check
- **WHEN** a CRM clean-up's agent node writes through a connector and has no deterministic check
- **THEN** readiness names that node and is not ready

### Requirement: Only a check apb runs and whose failure fails something is proof
A node SHALL count as deterministically checked only through a script `success_check`, a `script` list item check under `on_needs_look: fail`, or a `script` goal criterion under `goal.enforce: true`. A completion marker, an output-field item check, a connector or judge item check, a list without a check, a `list` goal criterion and any goal criterion that is not enforced SHALL NOT count.

#### Scenario: A marker is not a check
- **WHEN** a node's only check is a completion marker
- **THEN** readiness counts it as unchecked

#### Scenario: An output check is the agent's own word
- **WHEN** a list node's item check compares a field of the agent's own output
- **THEN** readiness counts the node as unchecked

#### Scenario: A goal that is only reported
- **WHEN** the goal has a script criterion and `goal.enforce` is false
- **THEN** the criterion checks no node and `done_defined` is not ok

### Requirement: Boundaries are judged per effect domain
Readiness SHALL judge an actor's boundary in each effect domain apart: the working tree is bounded only by `read_only`, `writes` or an isolation whose contract this machine enforces (a `best_effort` node that runs without a sandbox is not bounded); each connector binding is bounded only by allowing read-only functions or by naming its functions with `max_calls`. A local bound SHALL NOT bound a connector.

#### Scenario: A read-only node with an open connector
- **WHEN** an agent node is `read_only: true` and binds a connector with `functions: all`
- **THEN** readiness lists the node as unbounded at that connector and not in its working tree

#### Scenario: Best effort without a sandbox
- **WHEN** a node declares `isolation: best_effort` on a machine that cannot isolate it and has no other tree boundary
- **THEN** readiness lists the node as unbounded in its working tree

#### Scenario: A finish node with a prompt
- **WHEN** a playbook's `finish` node has a prompt, so it runs an agent
- **THEN** readiness lists it as an actor without a local boundary, since a finish node can declare neither `read_only`, `writes` nor isolation

### Requirement: Sub-playbook and publish nodes are not actors
A sub-playbook node and a `publish` node SHALL NOT be judged as actors by the boundary rules: the child's own report and probation judge the child, and a publish node is a gate held by a person's consent.

#### Scenario: A sub-playbook and a publish node
- **WHEN** an otherwise bounded playbook has a sub-playbook node and a `publish` node
- **THEN** readiness lists neither of them as an unbounded actor

### Requirement: An actor that can reach the network is bounded only by a closed network
An actor apb infers or declares to have `network` or `external` capability SHALL be counted as bounded on the network only by an isolation this machine enforces that closes the network (`network: none`), whatever its `read_only`, `writes` or connector bindings, and SHALL otherwise be reported as not bounded externally.

#### Scenario: A read-only script that can send requests
- **WHEN** a script node is `read_only: true`
- **THEN** readiness counts its working tree as bounded and lists it as not bounded externally

#### Scenario: An isolated agent without network
- **WHEN** an agent node declares `isolation: { level: full, network: none }` on a machine that enforces it
- **THEN** readiness counts it as bounded on the network

### Requirement: Failure routes are paths to an irreversible step
The irreversible-gate check SHALL follow every path a run can take without passing a gate: along edges from the start, and along the failure transitions (`defaults.on_failure`, and a `fail_fast` fork's `on_failure` from its branches) of the nodes it reaches that can fail, every node kind but `start` included, carrying whether a gate was passed on the way. A failure handler SHALL NOT be treated as reachable on its own.

#### Scenario: A failure before the gate
- **WHEN** a node before any gate can fail and `defaults.on_failure` names an irreversible node
- **THEN** readiness lists that node as reached without a gate

#### Scenario: A failure only after the gate
- **WHEN** the start leads straight to a `human_review`, then to a `fail_fast` fork whose `on_failure` names an irreversible node, and no `defaults.on_failure` is set
- **THEN** readiness does not list that node

#### Scenario: A gate that fails
- **WHEN** the start leads to a `human_review` that can time out, and `defaults.on_failure` names an irreversible node
- **THEN** readiness lists that node, since a gate that fails has not been passed

### Requirement: A gate a model may decide is not a gate
In the irreversible-gate check a gate SHALL be a `publish` node or a `human_review` that no decision model may decide; a `human_review` with `auto_decide` and `auto_decide_ok: true` SHALL NOT count as a gate.

#### Scenario: A gate a model may decide
- **WHEN** the only gate before an irreversible node is a `human_review` with `auto_decide` and `auto_decide_ok: true`
- **THEN** readiness lists the irreversible node as reached without a gate

### Requirement: An unattended start warns and may refuse
An unattended start of a playbook that is not ready SHALL print a warning naming what is missing. With `--require-ready`, run preparation SHALL refuse the start with `not_ready` when the definition it loaded, or a sub-playbook it pinned, is not ready, judging every readiness item but probation, which preparation decides on its own; no launch surface SHALL judge readiness on a definition of its own, and a child run SHALL take its parent's demand.

#### Scenario: Require ready
- **WHEN** `apb run --unattended --require-ready` starts a playbook without a budget
- **THEN** the start is refused with `not_ready` and no run is created

#### Scenario: A warning without the flag
- **WHEN** `apb run --unattended` starts a playbook without a budget
- **THEN** the run starts and stderr names what is missing

#### Scenario: The version the gate selects
- **WHEN** version 1.0.0 has a budget, the current version 1.1.0 has none, and `--require-ready` starts each
- **THEN** the start of 1.0.0 is admitted and the start of 1.1.0 is refused `not_ready`

#### Scenario: An unready child
- **WHEN** a ready parent pins a sub-playbook without a budget and starts with `--require-ready`
- **THEN** the start is refused `not_ready` naming the sub-playbook and its node, and no run is created

### Requirement: A patch is held to the start's checks
An in-run patch of a run started unattended SHALL be refused unless the replacement definition and the run's pinned sub-playbooks pass the probation check (with the run's recorded waiver), and of a run started with `--require-ready` unless they also pass readiness; the refusal SHALL be journaled as a patch refusal whose reason starts with `probation_required` or `not_ready`, and the run SHALL keep its running definition. An attended run's patch SHALL be checked by neither.

#### Scenario: A patch to an unproven version
- **WHEN** a supervisor patches an unattended, unwaived run to a version with no attended successes
- **THEN** the patch is refused with a reason starting `probation_required` and the run's definition is unchanged

#### Scenario: A patch on an unattended run whose manifest is gone
- **WHEN** a supervisor patches a run whose `run.yaml` says unattended and whose manifest is missing
- **THEN** the patch is refused `probation_required`: the waiver and the readiness demand live only in the manifest, and the run is never taken for an attended one

#### Scenario: A patch to an unready version
- **WHEN** a supervisor patches an unattended `--require-ready` run to a version without a budget
- **THEN** the patch is refused with a reason starting `not_ready`

### Requirement: Probation is counted from a signed ledger written by run ends
Every run that ends and learns (a production run, not an eval, a scenario or an external-data run) SHALL add one entry for its run id to its playbook's signed probation ledger in its project, written once and by nothing else; no MCP tool, HTTP route, CLI command or run step SHALL write it.

#### Scenario: A run counted once across a prune
- **WHEN** a playbook asking for two attended successes has two, and the older run is pruned
- **THEN** the next unattended start is admitted, and the ledger still holds one entry per run

### Requirement: The ledger entry follows the terminal event
A ledger entry SHALL be written only after the run's terminal event is in its journal, with its outcome read from that event. A run whose entry was not written before a crash SHALL stay uncounted, with no reconcile pass, and an aborted run SHALL never have an eligible entry.

#### Scenario: A crash before the terminal event, then a stop
- **WHEN** an attended run's drive dies after its finish hooks and before `run_finished`, and the run is then stopped
- **THEN** its entry records `aborted`, it is not eligible, and probation counts 0

#### Scenario: A crash after the terminal event
- **WHEN** an attended run's `run_finished` is in its journal and the process dies before the entry is written
- **THEN** the run has no entry and counts nothing, and a later stop changes nothing

### Requirement: A ledger entry says whether its run is eligible
An entry SHALL record the run id, the trust digest the start verified, the started definition's digest, whether the run was unattended, the outcome, and whether it is eligible: a successful run that executed one definition from start to end (one `run_started` with a definition digest, no `patch_applied`, no `profile_rebound`, no `run_migrated`, every `run_resumed` on the same definition), applied no run override, and was not started from a run step (its launch record has no `started_by`).

#### Scenario: A patched run
- **WHEN** an attended run applies an in-run patch and succeeds
- **THEN** its entry is not eligible and it counts nothing

#### Scenario: An overridden run
- **WHEN** an attended run started with run overrides succeeds
- **THEN** its entry is not eligible and it counts nothing

#### Scenario: A run a run step started
- **WHEN** a run step starts an attended run of a playbook, answers its gates, and the run succeeds
- **THEN** its entry is not eligible and it counts nothing

#### Scenario: Runs before this change
- **WHEN** a playbook had attended successes that ended before this change and asks for probation
- **THEN** none of them counts

### Requirement: A run's mode comes from a record that exists
Whether a run is unattended SHALL come from its manifest, else its persisted `run.yaml`; no reader of probation, a patch or a child start SHALL take a missing record for attended. An entry whose mode is not established SHALL not be eligible, and a missing `run.yaml` SHALL NOT read as a run without overrides.

#### Scenario: A run whose manifest is gone
- **WHEN** an unattended run succeeds and its manifest is missing when its entry is written
- **THEN** its entry is recorded unattended from its `run.yaml` and counts nothing; with neither record, the entry is not eligible

#### Scenario: A child of an unattended parent whose manifest is gone
- **WHEN** a child run is started under a parent whose `run.yaml` says unattended and whose manifest is missing
- **THEN** the child start is refused `probation_required` and no child run is created

### Requirement: Probation counts eligible attended successes of the exact definition in the project
With `unattended.after_attended_successes` in the playbook or a `policy.yaml`, an unattended start SHALL be refused `probation_required` until the project's ledger holds that many eligible attended successes whose trust digest (definition and scripts) and definition digest equal those of the definition the start loaded. A global playbook SHALL earn its probation in each project, and the refusal SHALL say that probation is counted per project.

#### Scenario: A new version restarts probation
- **WHEN** version 1.0.0 had two attended successes and 1.1.0 asks for two
- **THEN** an unattended start of 1.1.0 is refused

#### Scenario: A changed script resets the count
- **WHEN** a version had two attended successes and one of its scripts then changed and was approved
- **THEN** probation counts 0

#### Scenario: Unattended, failed and external-data runs do not count
- **WHEN** the only runs are an unattended success, an attended failure and an attended success started with `external_data: true`
- **THEN** probation counts 0

### Requirement: The count is of attended runs
"Attended" SHALL mean only that a run was started without `--unattended`: every surface and refusal that shows the probation count SHALL call it attended runs, and none SHALL say that a person decided anything in them.

#### Scenario: The count's wording
- **WHEN** the fit report shows the probation of a playbook with one of two attended runs counted
- **THEN** it reads `1 of 2 attended runs of this definition in this project` and claims no person

### Requirement: An unattended start runs the definition its probation counted
An unattended start under probation that carries non-empty run overrides SHALL be refused `probation_required` with the detail "an unattended start runs the definition its probation counted; start it without --overrides or waive probation at a terminal", unless the start is waived.

#### Scenario: Overrides on an unattended start
- **WHEN** a playbook has counted its attended runs and `apb run --unattended --overrides o.yaml` starts it
- **THEN** the start is refused `probation_required` and no run is created

### Requirement: A policy file that does not read fails closed
The probation check SHALL load the machine's and the project's `policy.yaml` once and read the floor and the machine waiver from that one load. When either file does not read, an unattended start SHALL be refused `probation_required` with a text naming the file and its error, unless the start carries the typed waiver.

#### Scenario: An invalid project policy under a machine floor
- **WHEN** the machine `policy.yaml` asks for one attended run and the project `policy.yaml` sets `unattended.waive_probation: true`
- **THEN** an unattended start of a playbook with no `unattended:` block is refused `probation_required` naming the project file

### Requirement: A ledger that does not verify refuses
A probation ledger that does not verify or does not parse SHALL count nothing: an unattended start under probation SHALL be refused `probation_required` with a text naming the file, unless waived by the typed confirmation or by the machine's `policy.yaml` (the fit report's probation item agrees), a run's end SHALL write nothing to it, and `apb doctor` SHALL name it. Moving the file aside SHALL start the count again from zero.

#### Scenario: A tampered ledger
- **WHEN** one byte of a playbook's probation ledger is changed and an unattended start under probation follows
- **THEN** the start is refused naming the ledger, no run is created, and `apb doctor` warns about the file

#### Scenario: A machine waiver over a ledger that does not verify
- **WHEN** the machine `policy.yaml` sets `unattended.waive_probation: true` and the playbook's ledger does not verify
- **THEN** an unattended start is admitted and the fit report's probation item is ok, waived on this machine

### Requirement: The ledger is bound to its project
The probation ledger SHALL record the project it belongs to (the workspace id registered at the project root, else the canonical project root), covered by its signature and compared on read, so a ledger of another project does not verify; it SHALL be opened without following a link.

#### Scenario: A ledger of another project
- **WHEN** a signed probation ledger of a global playbook is copied, or linked, from project A into project B
- **THEN** it does not verify in B and counts nothing there

### Requirement: Probation is checked once, on the definition the start executes
The probation check SHALL run in run preparation, after the selected version is loaded and before the run directory is created, on that loaded definition and version; no launch surface SHALL decide it on a definition of its own.

#### Scenario: The version the start loads
- **WHEN** version 1.0.0 finished its probation, the current version 1.1.0 has not, and both ask for one
- **THEN** an unattended start of version 1.0.0 is admitted, one of the current version is refused, and the refused start leaves no run directory

### Requirement: Every playbook of the tree has its own probation
Probation SHALL be checked for every playbook of a run's tree: at the start for every sub-playbook the gate pinned, with its own definition and its own ledger, so the refusal comes before anything runs and names the child and its node; and again where a child run is created, for a child resolved only then. A child run SHALL write its entry into its own playbook's ledger, and a child run of an attended parent SHALL count as an attended success of the child's definition.

#### Scenario: A child under probation
- **WHEN** a parent that finished its probation pins a sub-playbook whose definition has no attended success and asks for one
- **THEN** the unattended start of the parent is refused `probation_required` naming the sub-playbook and its node, and no run is created

#### Scenario: An attended parent proves its child
- **WHEN** a parent ran attended and succeeded twice and its child asks for two attended successes
- **THEN** an unattended start of the parent is admitted

### Requirement: A waiver is a typed confirmation or a machine setting
Probation SHALL be waived only by `apb run --waive-probation` after the playbook id is typed at an interactive terminal outside a run step, or by `unattended.waive_probation` in the machine's `policy.yaml`. The waiver SHALL be refused in a run step and without an interactive terminal, SHALL NOT be offered on MCP or HTTP, and a project file SHALL NOT set it. apb SHALL NOT claim that a waiver proves a person.

#### Scenario: A waiver through MCP
- **WHEN** an MCP start asks for an unattended run under probation
- **THEN** it is refused whatever it sends

#### Scenario: No terminal
- **WHEN** `apb run --unattended --waive-probation` runs without an interactive terminal
- **THEN** it exits 2 naming the typed confirmation it needs and no run is created

#### Scenario: A project waiver
- **WHEN** a project `policy.yaml` sets `unattended.waive_probation: true`
- **THEN** the file is invalid

#### Scenario: A redirected config dir
- **WHEN** a run step, or the CLI without a terminal, starts an unattended run with its config dir redirected to one whose `policy.yaml` waives probation, or asks for fewer attended runs than the default config dir's
- **THEN** the start is refused `policy_file_invalid` and no run is created

### Requirement: A waiver is always asked and covers the run's tree
The CLI SHALL ask the typed confirmation every time `--waive-probation` is given at a terminal, whatever the probation of the playbook or its sub-playbooks stands at. A waived start SHALL be recorded in the run's manifest and SHALL reach every child run of its tree.

#### Scenario: Always asked
- **WHEN** `apb run --unattended --waive-probation` starts a playbook that asks for no probation
- **THEN** the typed confirmation is asked all the same

#### Scenario: The waiver reaches the children
- **WHEN** a waived unattended run starts a sub-playbook under probation
- **THEN** the child run is created and its manifest records the waiver

### Requirement: The older-binary limit is stated
The documentation and release notes SHALL state that an apb older than this change ignores the top-level `unattended:` block and starts such a playbook unattended without probation, how older binaries treat a list's `pilot` and `stop_when` (list-pilot), and that the playbook format has no way to demand a minimum apb. No marker in the playbook or the machine's `policy.yaml` SHALL be added to fence older binaries.

#### Scenario: The release notes
- **WHEN** the release notes of this change are read
- **THEN** they say that probation holds with this apb or newer only and that every apb on such a machine must be this release or newer
