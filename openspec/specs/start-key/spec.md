# start-key Specification

## Purpose
TBD - created by archiving change routine-operations. Update Purpose after archive.

## Requirements

### Requirement: A repeated keyed start answers the existing run
A start with a start key SHALL start nothing when a live entry of the same playbook (scope and id) holds that key with the same request, and SHALL answer that run's id with `already_started: true` on every launch surface.

#### Scenario: A CI retry follows the first run
- **WHEN** `apb run digest --start-key k --detach` started run R
- **AND** the same command runs again
- **THEN** no second run directory exists
- **AND** the second call prints `run already started: R`

#### Scenario: A foreground repeat exits like apb wait
- **WHEN** the keyed run R failed
- **AND** `apb run digest --start-key k` runs again in the foreground
- **THEN** it exits with code 1, as `apb wait R` would

### Requirement: The request is what the caller passed
The request a key binds SHALL be every explicit execution-affecting argument the caller passed (params, instruction, version, worktree, overrides, execution mode, supervision mode, cache policy, parallelism, supervisor patch limit, compaction settings, deadline, budget, unattended, shared workdir, continued-from) and the effective `external_data` flag. The queue wait, `remember`, notify settings and consent arguments SHALL not be part of it.

#### Scenario: Remembered input changed between two keyed starts
- **WHEN** run R was started with key `k` and no params
- **AND** the playbook's remembered input changed
- **AND** a start passes key `k` and no params
- **THEN** it answers R with `already_started: true`

#### Scenario: Another cache policy
- **WHEN** run R was started with key `k`
- **AND** a start passes key `k` and `--no-cache`
- **THEN** it is refused `start_key_conflict`

### Requirement: The request is captured before anything changes it
The launch surface SHALL capture the request from the caller's own arguments before any gate, candidate choice, prompt or default, and SHALL carry it unchanged to the engine. Values that remembered input, defaults, a chosen candidate version or a budget prompt fill in SHALL never be part of it.

#### Scenario: A candidate appeared between two keyed starts
- **WHEN** run R was started over MCP with key `k` and no version
- **AND** a candidate version of the playbook now waits
- **AND** a start passes key `k` and no version
- **THEN** it answers R with `already_started: true`, not `start_key_conflict`

#### Scenario: A global playbook started twice without a version
- **WHEN** run R of a global playbook was started with key `k` and no version
- **AND** the same start runs again
- **THEN** it answers R with `already_started: true`

#### Scenario: A budget question answered at the terminal
- **WHEN** run R was started with key `k` and its `on_no_estimate: ask` question was answered
- **AND** the same command runs again
- **THEN** it answers R with `already_started: true`

### Requirement: Another request under a key is refused
A start whose key names a live entry with a different request SHALL be refused `start_key_conflict` naming the entry's run, before any gate, prompt or write.

#### Scenario: Other params under the same key
- **WHEN** run R was started with key `k` and param `day=1`
- **AND** a start passes key `k` and `day=2`
- **THEN** it is refused `start_key_conflict` naming R

#### Scenario: Another external data flag
- **WHEN** run R was started with key `k` without `external_data`
- **AND** a start passes key `k` and `external_data: true`
- **THEN** it is refused `start_key_conflict` naming R

#### Scenario: Another explicit version
- **WHEN** run R was started with key `k` and no version
- **AND** a start passes key `k` and `--version 1.2.0`
- **THEN** it is refused `start_key_conflict`

### Requirement: The key store is signed and never forgets silently
apb SHALL store only keyed MACs of the key and of the request, in a file of the signed per-playbook store. A start-key file that is present but does not verify or parse SHALL refuse every keyed start of that playbook `start_key_state_unreadable` and SHALL be left unchanged until a person moves it; unkeyed starts SHALL be unaffected. A credential-shaped key SHALL be refused `start_key_invalid` with nothing written.

#### Scenario: The store holds no key
- **WHEN** a run starts with key `order-4711`
- **THEN** no file under `.apb/start-keys/` contains `order-4711` or its plain SHA-256

#### Scenario: A tampered file
- **WHEN** the start-key file of a playbook was rewritten without a valid MAC
- **THEN** a keyed start is refused `start_key_state_unreadable`
- **AND** the file's bytes are unchanged
- **AND** an unkeyed start runs

### Requirement: A key is owned while its run is prepared
A keyed start SHALL record its key as pending, owned by the preparing process, before the working-tree lock, and SHALL bind it to its run only once the run carries its origin stamp, `run_started` and `run_provenance`. While the owner lives, every other start with the key SHALL answer the preparing run, however long preparation takes, or `start_key_busy` before the run has an id. Ownership, not age, SHALL decide.

#### Scenario: A lookup during preparation
- **WHEN** a keyed start allocated run R and has not yet stamped it
- **AND** another start with the same key and request arrives
- **THEN** it answers R with `already_started: true`
- **AND** exactly one run directory exists afterwards

#### Scenario: A slow preparation
- **WHEN** a keyed preparation has held its key for two minutes and is still alive
- **THEN** a repeat answers its run, and no second run starts

### Requirement: A failed preparation and its key
A preparation that fails after its run directory exists SHALL leave the key naming that failed run; one refused before any run directory exists SHALL leave the key free. A pending entry whose owner ended SHALL count only when its run passes the run check.

#### Scenario: A failure before provenance
- **WHEN** a keyed start fails in preparation after its run directory was allocated and before `run_provenance`
- **AND** the same keyed start runs again
- **THEN** it answers the failed run with `already_started: true`

#### Scenario: A refusal before any run directory
- **WHEN** a keyed start is refused because the working tree is busy and no queue wait was given
- **AND** the tree is free again
- **THEN** the same keyed start runs

### Requirement: Stored run references are checked
An entry SHALL count only when its run directory is a real directory with this installation's origin stamp whose `run_started` names the file's playbook and whose `run_provenance` names the file's scope, or which has no `run_provenance` and ended in preparation. Any other entry SHALL be void.

#### Scenario: A dead owner frees the key
- **WHEN** a pending entry's owner ended and its run directory holds an empty journal
- **THEN** a keyed start with that key runs

#### Scenario: A forged entry
- **WHEN** an entry names a run of another playbook or a directory without an origin stamp
- **THEN** a keyed start with that key runs

### Requirement: Concurrent keyed starts make one run
The engine SHALL decide a keyed start in a transaction of the playbook's signed start-key file, before the working-tree lock, so that concurrent starts with one key and request make one run.

#### Scenario: Eight starts at once
- **WHEN** eight threads start one playbook with the same key and request
- **THEN** exactly one run exists and the other seven answer its id

### Requirement: A failed or aborted run holds its key until reuse
A key whose run failed or was aborted SHALL keep answering that run until a start passes `start_key_reuse` outside a run step, the run directory is deleted, or the entry is older than `runs.start_key_days` (default 7). Reuse SHALL be refused for a running, paused or succeeded run.

#### Scenario: Reuse after a failure
- **WHEN** keyed run R failed
- **AND** a person starts with the key and `--start-key-reuse`
- **THEN** a new run starts and the key names it
