## ADDED Requirements

### Requirement: A secret param is never rendered
Validation SHALL report V340 as an error for every string of a playbook definition, mapping keys included, that references a param declared `secret: true` in a form the template renderers resolve, except in `quality.task_type` and `concurrency.key`, which V104 and V210 already refuse at validation whatever else the definition gets wrong; a reference in `previous_success.when_none` is V340's. A definition that declares a secret param and cannot be walked SHALL be refused, never passed.

#### Scenario: A prompt reads a secret param
- **WHEN** an `agent_task` prompt reads `{{ params.token }}` and `token` is declared secret
- **THEN** validation reports V340 naming the node and the field `prompt`

#### Scenario: A connector argument of an item check reads a secret param
- **WHEN** a list item check passes `{{params.token}}` as a connector argument and `token` is declared secret
- **THEN** validation reports V340 naming the node and the argument's path

#### Scenario: A sub-playbook instruction reads a secret param
- **WHEN** a `playbook` node's `instruction` reads a secret param
- **THEN** validation reports V340, and no pass-through to the child is allowed

#### Scenario: A free-form block with a non-string key
- **WHEN** `supervisor.policy` holds a null key and a sequence key whose values read `{{params.token}}`, and a string key that is itself `{{params.token}}`, and `token` is declared secret
- **THEN** validation reports V340 for each of the three

#### Scenario: A definition that cannot be walked
- **WHEN** the definition declares a secret param and its value tree cannot be built
- **THEN** validation reports V340 for the definition instead of reporting nothing

#### Scenario: A plain param is unaffected
- **WHEN** a playbook references only params that are not declared secret
- **THEN** validation reports no V340

### Requirement: A secret stays secret for the run and its patches
A param declared `secret: true` at any point of a run SHALL stay secret for that run: an in-run patch or a forward patch that removes `secret: true` from it, or removes the param while a string still references it, SHALL be refused `secret_declaration_dropped`; and every check of a patched, resumed or candidate definition SHALL judge references by the run's full secret set (every name the run's manifest and snapshot declare secret), not by that definition's own declarations alone.

#### Scenario: An in-run patch drops a declaration
- **WHEN** a supervisor's patch of a running run removes `secret: true` from a param the run declares secret
- **THEN** the patch is journaled `patch_rejected` with `secret_declaration_dropped`, and the run's `playbook.yaml` and manifest are unchanged

#### Scenario: A patch names a stored version that renders a secret
- **WHEN** a patch names a version written to the store without validation whose prompt reads a param the run declares secret
- **THEN** the patch is journaled `patch_rejected` with `secret_param_rendered` and the snapshot is not replaced

#### Scenario: A forward patch drops a declaration
- **WHEN** a forward patch from a run removes `secret: true` from a param that run declares secret
- **THEN** the forward patch is refused `secret_declaration_dropped` and no candidate is written

### Requirement: A patch records new secret names only once they are in force
An in-run patch SHALL record in the manifest, before it replaces the snapshot, only the names the running definition declares secret, and the names the patched definition newly declares secret only after the new snapshot is written, so that no refused or interrupted patch leaves the manifest calling a param secret that the executing snapshot calls plain.

#### Scenario: A patch refused after its secret check
- **WHEN** a patch declares a plain param secret and loosens `agent_launch`, and is refused for the launch policy
- **THEN** the manifest does not name the param, `playbook.yaml` is unchanged, and a later resume of the run is not refused

#### Scenario: A crash between the new snapshot and the manifest write
- **WHEN** a patch that declares a param secret has replaced the snapshot and the run stops before the manifest names the param
- **THEN** the run's secret set still holds the param, read from the snapshot, and the resume is not refused

### Requirement: A forward patch rechecks the run's secret set before it writes
A forward patch SHALL read its source run's secret set and run its secret check under the candidate lock it already holds, immediately before it writes the candidate, so that the candidate is judged against the run as it stands when the candidate is written. An in-run patch SHALL take no lock for this.

#### Scenario: The run's set changed before the write
- **WHEN** a forward patch's YAML renders a param as plain and, before the forward patch is made, an in-run patch of its source run declares that param secret
- **THEN** the forward patch is refused `secret_declaration_dropped` and no candidate is written

### Requirement: A stored definition that renders a secret is refused
Saving a version that V340 refuses SHALL be refused; a start of a stored version that V340 refuses SHALL be refused before the run directory is written; and a waiting candidate that renders a secret or drops a declaration `current` holds SHALL be skipped with `candidate_skipped`, the start running `current`. A start that fixed the candidate as its choice (`launch_version` with `launch_choice: candidate`) SHALL run the same check and, having no fallback, SHALL be refused with its code.

#### Scenario: A version stored before the rule
- **WHEN** a run is started from a stored version whose prompt reads a secret param
- **THEN** the start is refused naming V340 and no run directory is left

#### Scenario: A fixed candidate that renders or declassifies
- **WHEN** a start fixed the candidate as its choice and that candidate renders a secret param or keeps a param plain that `current` declares secret
- **THEN** the start is refused `secret_param_rendered` or `secret_declaration_dropped` and no run is prepared

#### Scenario: A waiting candidate stored before the rule
- **WHEN** `current` is trusted and the waiting candidate's prompt reads a secret param
- **THEN** the start runs `current` and reports `candidate_skipped` with `refused: secret_param_rendered`

### Requirement: A run that renders a secret is refused at resume
A resume of a run whose definition references a secret param SHALL be refused `secret_param_rendered` in the synchronous part of every resume entry and returned to the caller through its surface's existing refusal mapping, before any event is journaled, any slot is reserved or any lock is taken; stop and pause SHALL still work.

#### Scenario: A detached resume of a run started before the rule
- **WHEN** a run whose definition reads a secret param in a prompt is resumed detached
- **THEN** the caller receives the `secret_param_rendered` refusal, no driver is spawned, no event is journaled, and the run can still be stopped

#### Scenario: An in-process resume of a run started before the rule
- **WHEN** the same run is resumed in process
- **THEN** the resume is refused `secret_param_rendered` before the concurrency slot is reserved or the tree lock is taken, and no `run_resumed` is journaled

#### Scenario: A refused resume of a schema-1 run
- **WHEN** a schema-1 run with no manifest whose prompt reads a secret param is resumed in process
- **THEN** the resume is refused `secret_param_rendered` and no `manifest.yaml` is written

### Requirement: The value stays in the private run config
apb SHALL write a secret param's value only to the run's `run.yaml` (mode 0600) and SHALL NOT copy it into the start key file, remembered input, the manifest, the estimate, the report, a scenario capture, a judge example, a host task file or a child run.

#### Scenario: A script reads the value
- **WHEN** a run's script reads a secret param from `$APB_RUN_DIR/run.yaml` and prints nothing of it
- **THEN** after the run only `run.yaml` holds the value, in its name or bytes, under the project, the config directory and the run's temporary directory, the start key file it wrote included, its mode is 0600, and parsed as YAML its `params.<name>` equals the value

#### Scenario: A capture after a dropped declaration
- **WHEN** a run whose manifest names a secret param that its current definition no longer declares secret is captured as a scenario
- **THEN** the scenario leaves the param out and warns

### Requirement: No process apb starts carries a secret value on its argv
The detached supervised drive SHALL take the run's params from its launch handoff, and an eval case run from its private settings file, which the case run SHALL remove once it read it, so a kept repetition holds the params only in `run.yaml`.

#### Scenario: A detached supervised start
- **WHEN** `apb run --supervise` starts a run with a secret param
- **THEN** the `__drive-supervised` process's argv holds neither the value nor a `--param`, and the run's `run.yaml` holds the value

#### Scenario: An eval case with params
- **WHEN** an eval case sets a secret param and its repetition runs
- **THEN** the project, the config directory and the run directory hold the value only in `run.yaml` files and the case file, and no settings file remains

### Requirement: Only an absent manifest is no manifest
A run's `manifest.yaml` path where nothing exists SHALL count as no manifest; a directory, a link (dangling or not), a fifo or a file that cannot be read there SHALL be an error, which the strict read of the secret set turns into `secret_set_unreadable`.

#### Scenario: A manifest that is not a regular file
- **WHEN** a run's `manifest.yaml` is a directory or a dangling link and a resume, the resume preflight or an in-run patch meets it
- **THEN** it is refused `secret_set_unreadable` and nothing is written

### Requirement: Surfaces withhold a run whose secret set does not read
Every surface that shows a run's text through the run redactor SHALL withhold it as `[secrets_unresolved]` when the manifest or the snapshot of the run, or of a run of its tree, is there and does not read or parse, or when the snapshot of a prepared run whose `run.yaml` holds a param value is missing.

#### Scenario: A corrupted manifest after a dropped declaration
- **WHEN** a patch dropped an unused secret declaration from a run's snapshot and the run's manifest then does not parse
- **THEN** the run detail, `run_status` and `apb runs <id>` show `[secrets_unresolved]` and never the value

#### Scenario: A prepared run without its snapshot
- **WHEN** a prepared legacy run with no manifest has lost its `playbook.yaml`
- **THEN** its run detail shows `[secrets_unresolved]` and never a value from its `run.yaml`

### Requirement: A file that carries params is never left half written
A launch handoff whose write or sync fails after the file was created SHALL be removed before the error is returned.

#### Scenario: A failed handoff write
- **WHEN** writing a handoff fails after its file was created
- **THEN** no handoff file remains

### Requirement: The run config is a documented place for a step
A step that needs a param SHALL find it in the YAML file `$APB_RUN_DIR/run.yaml`, under the top-level mapping `params`, keyed by the param name, its value a YAML string quoted wherever YAML requires it. `params` SHALL hold the run's resolved values: the value the start was given, else the value recalled from remembered input, else the param's default; a param with none of the three SHALL be absent.

#### Scenario: A value YAML must quote
- **WHEN** a run is given a param value that YAML must quote, such as `007: #a`
- **THEN** `params.<name>` of the parsed `run.yaml` reads back as exactly that string

#### Scenario: Given, recalled and defaulted values
- **WHEN** a run is given its secret param, a plain param is recalled from remembered input, another plain param takes its default, and a fourth has none of the three
- **THEN** `params` of the parsed `run.yaml` holds the first three with those values and not the fourth

### Requirement: Checks outside validation judge every string
Every secret check that runs without the validator (a waiting candidate, an in-run patch, a forward patch, a resume) SHALL judge every string of the definition, `quality.task_type`, `previous_success.when_none` and `concurrency.key` included; only validation SHALL leave `quality.task_type` and `concurrency.key` to V104 and V210.

#### Scenario: An owned field outside validation
- **WHEN** a waiting candidate, a version an in-run patch names, or a resumed run's snapshot reads a secret param in `quality.task_type`, `previous_success.when_none` or `concurrency.key`
- **THEN** the candidate is skipped with `refused: secret_param_rendered`, the patch is journaled `patch_rejected` with `secret_param_rendered`, and the resume is refused `secret_param_rendered`

### Requirement: A refused resume writes nothing on a consent surface
A surface that records an irreversible-effects consent before it resumes a run (the CLI, MCP and the dashboard) SHALL run the read-only secret preflight first and write no consent, clear none and write no legacy manifest when the snapshot it reads refuses the resume. When the snapshot changes after a passing preflight, the engine SHALL still refuse the resume; a consent recorded in between is outside this guarantee.

#### Scenario: A resume with a consent confirmation
- **WHEN** such a run of an irreversible tree with no consent recorded is resumed through the CLI, MCP or the dashboard with a confirmation that would record the consent
- **THEN** the surface answers `secret_param_rendered` through its own refusal mapping and the run's `manifest.yaml` is unchanged

#### Scenario: The snapshot changed after a passing preflight
- **WHEN** a run's consent is recorded and its `playbook.yaml` is then replaced by one whose prompt reads a secret param
- **THEN** the engine's resume is refused `secret_param_rendered` and nothing is journaled

### Requirement: The resume preflight rejects an unsafe run id
The resume preflight SHALL reject a run id that is not a safe path segment before joining it to the runs directory, answering not found, as the resume entries do.

#### Scenario: An unsafe run id
- **WHEN** the preflight is called with a run id such as `..` or `a/b`
- **THEN** it answers not found and reads nothing outside the runs directory

### Requirement: The run's secret set is read strictly
Every check that judges by a run's full secret set SHALL read the run's manifest and its snapshot strictly: a manifest that exists and does not read, or a snapshot that is missing, does not read or does not parse, SHALL give `secret_set_unreadable` as the caller's own refusal, never an empty or partial set. The set is read this way for every run, with or without a secret param.

#### Scenario: A run without a secret param whose manifest does not read
- **WHEN** an in-run patch or a resume meets a run that declares no secret param and whose `manifest.yaml` exists and does not read
- **THEN** it is refused `secret_set_unreadable`, as a run with a secret param is

#### Scenario: A failed snapshot read after a crashed patch
- **WHEN** an in-run patch declared `token` secret, replaced the snapshot and stopped before the manifest named `token`, and the snapshot then cannot be read
- **THEN** a forward patch from the run whose YAML renders `token` as a plain param is refused `secret_set_unreadable` and no candidate is written

#### Scenario: A consent surface meets a manifest that does not read
- **WHEN** a paused run whose manifest does not read is resumed through the CLI, MCP or the dashboard
- **THEN** the surface answers `secret_set_unreadable` from the preflight before any consent step, and nothing is written

#### Scenario: An unreadable manifest at an in-run patch
- **WHEN** a supervised run's manifest does not read and a patch is applied
- **THEN** the patch is journaled `patch_rejected` with `secret_set_unreadable` and the run's `playbook.yaml` is unchanged

### Requirement: The journal of a run with a secret param is unchanged
This change SHALL add no event, event field, run guard name or manifest mark for a run, with or without a secret param.

#### Scenario: A run with a secret param
- **WHEN** a playbook that declares a secret param runs
- **THEN** its journal carries no `run_guard`, and the same sequence of event types as the same playbook without the declaration

### Requirement: Runs without a secret param are unchanged
For a definition that declares no secret param, every check this change adds SHALL return no finding without inspecting the definition further, and no write site, renderer, event, event field, manifest field, run guard name or routing rule SHALL change for its run.

#### Scenario: The shipped playbooks
- **WHEN** each bundled playbook and each playbook under `examples/` is checked
- **THEN** the secret reference walk returns no hit and the patch and resume check passes

#### Scenario: The existing journal tests
- **WHEN** the journal compatibility, journal corpus and equal-journal tests run after the change
- **THEN** their existing rows pass unchanged

#### Scenario: A run without a secret param
- **WHEN** a playbook that declares no secret param runs
- **THEN** its journal carries no `run_guard` it did not carry before
