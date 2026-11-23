# compatibility-freeze Specification

## Purpose

What apb 1.x keeps stable (the playbook schema, the run journal, the MCP tools, the validator codes, the machine names and the files it writes) is pinned by frozen fixtures and the tests that guard them, and released binaries read and resume the journals this build writes, so a 1.x release never breaks a playbook, a run or an agent integration an earlier 1.x accepted.

## Requirements

### Requirement: Frozen schema corpus
A frozen corpus of playbook definitions (every top-level example, every bundled playbook and the field fixtures) SHALL parse on this build and SHALL validate to its frozen error codes; its warning codes SHALL be recorded and not asserted. A frozen document SHALL keep its exact bytes under its name, and a name SHALL appear once.

#### Scenario: A frozen example still parses
- **WHEN** a frozen copy of an example is parsed by a later build
- **THEN** it parses and validates to the error codes frozen for it

#### Scenario: A new error on an old definition
- **WHEN** a change makes a frozen document produce an error code it did not have
- **THEN** the freeze test fails naming the document and the code

#### Scenario: An example changes without a new version
- **WHEN** an example's bytes change and its `version` stays the frozen one
- **THEN** the freeze test fails telling the person to bump the version of that file

### Requirement: No field is dropped in silence
Every typed path of a frozen document SHALL survive a parse and a serialization of the definition, compared at its exact place; the only exceptions SHALL be listed entries of a written form and a serialized form that parse to the same value.

#### Scenario: A renamed top-level field
- **WHEN** a top-level playbook field used by a frozen document is renamed
- **THEN** the freeze test fails naming the key path that was dropped

### Requirement: Every schema place has a fixture
Every serialized field of every schema struct, every variant of every schema enum and every field of each struct variant, every own field of a struct that flattens another, every accepted form of a value with more than one, and every node type SHALL be a line of a coverage inventory, and each line SHALL name a field fixture and the path where that fixture exercises it.

#### Scenario: A new field without a fixture
- **WHEN** a change adds an optional top-level playbook field and the inventory lacks it
- **THEN** the freeze test fails naming the field

#### Scenario: An inbox wait keeps its fixture
- **WHEN** the inbox form of `wait_for` loses a field a frozen fixture uses
- **THEN** the freeze test fails naming the inventory line and the path

### Requirement: The inventory is checked from the types
The completeness of the coverage inventory SHALL be checked from the types: the fields of each derived struct, the variants serde lists for each schema enum, the fields the derived serializer records for each struct variant, and the declaration of each struct that flattens another. It SHALL never be checked from the fixtures alone.

#### Scenario: A new optional field of a node kind
- **WHEN** a change adds an optional field to a variant of `NodeKind` and the inventory lacks it
- **THEN** the freeze test fails naming the variant and the field

#### Scenario: A new own field of a flattening struct
- **WHEN** a change adds an optional field to `Node` and the inventory lacks it
- **THEN** the freeze test fails naming the field

#### Scenario: A new variant without a fixture
- **WHEN** a change adds a variant to `WaitFor` and no fixture holds it
- **THEN** the freeze test fails naming the variant

### Requirement: Every event type has a frozen line
The journal freeze SHALL hold one line per event type this build knows. Each frozen line SHALL be read by this build, and its decoded event SHALL serialize to the same JSON value as the line, compared as parsed values so that the order of members is free. Every frozen checkpoint type SHALL stay a checkpoint; another type MAY become one. A field added to a frozen event SHALL add no member at its default, checked on the parsed values.

#### Scenario: A new event type
- **WHEN** a change adds an event type without a frozen line
- **THEN** the journal freeze test fails naming the type

#### Scenario: A renamed event field
- **WHEN** a change renames a field of a frozen event
- **THEN** that line no longer serializes to its frozen value and the test fails

#### Scenario: A new field written at its default
- **WHEN** a change adds a field to a frozen event that serializes at its default
- **THEN** the decoded line gains a member and the test fails naming it

#### Scenario: Members change order
- **WHEN** a change reorders the fields of a frozen event without renaming any
- **THEN** every journal freeze test passes

#### Scenario: An event becomes a checkpoint
- **WHEN** a change makes a frozen event type that was not a checkpoint a checkpoint
- **THEN** every journal freeze test passes

#### Scenario: A checkpoint stops being one
- **WHEN** a frozen checkpoint type is no longer a checkpoint
- **THEN** the test fails

### Requirement: Frozen MCP tools
Every frozen MCP tool SHALL still be served, SHALL accept every input its frozen normalized input schema accepts, SHALL gain no required member at any object of that schema, SHALL still deserialize each frozen union example through its own argument type, and SHALL stay served read-only when it was (a restricted tool may become full, a removed tool may become anything). Every served tool and every argument SHALL be in the freeze.

#### Scenario: A removed argument
- **WHEN** a change removes an argument of a frozen tool
- **THEN** the MCP freeze test fails naming the tool and the argument

#### Scenario: An optional argument is added
- **WHEN** a change adds an optional argument to a frozen tool
- **THEN** the freeze test fails with the command that adds the argument, and after the writer ran every freeze test passes

#### Scenario: A union loses an alternative
- **WHEN** a change makes an argument stop accepting one of its frozen alternatives
- **THEN** the freeze test fails naming the tool and the argument

#### Scenario: A new tool
- **WHEN** a change adds a tool the freeze lacks
- **THEN** the test fails with the command that adds it to the freeze

### Requirement: Frozen MCP input constraints
The normalized input schema of a frozen tool SHALL record every constraint that narrows input (bounds, integer formats, enumerated values, lengths, counts, patterns and additional properties), and a frozen constraint SHALL only widen. Each frozen boundary value and the frozen minimal argument object SHALL still deserialize through the tool's own argument type.

#### Scenario: An integer argument narrows
- **WHEN** a change makes `playbook_run`'s `budget.tokens` a 32-bit integer
- **THEN** the freeze test fails naming the tool, the path and the frozen boundary value it refuses

#### Scenario: A bound widens
- **WHEN** a change raises the maximum of a frozen numeric argument
- **THEN** every MCP freeze test passes

### Requirement: Frozen validator codes
Every validator code SHALL be in one registry with the severities it may be reported at, every code the production sources name SHALL be in it, a frozen code SHALL stay active with its severities or be listed as retired, and a retired code SHALL never be emitted and its number SHALL never be reused. V15, never emitted, SHALL be listed as retired with the reason "never emitted".

#### Scenario: A retired code fires
- **WHEN** a frozen document gets a retired code
- **THEN** the freeze test fails

#### Scenario: A code changes severity
- **WHEN** a change registers a frozen error code as a warning
- **THEN** the freeze test fails naming the code

#### Scenario: V15 is never reused
- **WHEN** a change registers a new rule under V15
- **THEN** the freeze test fails because V15 is retired

### Requirement: Every active code has an executed witness
Every active validator code SHALL be fired by a frozen document or have a witness that the freeze test executes through the checker that owns the code, asserting the code and its severity. The existence of a test name SHALL never count as a witness.

#### Scenario: A start rule code
- **WHEN** a change removes the code V263 of the start rule checks
- **THEN** the freeze test fails naming the code

#### Scenario: A witness stops firing
- **WHEN** a change makes `trigger::check_rule` report V263 as a warning while every test name stays
- **THEN** the witness of V263 fails naming the code and the severity it returned

### Requirement: The regeneration switch only adds
One writer per fixture group, run with `APB_WRITE_FREEZE=1`, SHALL only add entries, SHALL check the whole proposed snapshot before it writes, SHALL replace each file atomically, and SHALL write nothing when a removal, a rename, a narrowed input or a changed frozen error is found. No other test SHALL write a fixture.

#### Scenario: Regenerating over a removal
- **WHEN** the writer runs while a frozen argument is missing from its tool
- **THEN** it fails and leaves the fixture unchanged

#### Scenario: A writing run never passes
- **WHEN** the writer added entries
- **THEN** the test run fails telling the person to review the diff and run again without the switch

### Requirement: Released binary readers
The released binary corpus SHALL read, resume and stop journals between this build and the newest published release in both directions.

#### Scenario: The newest beta resumes a new run
- **WHEN** this build writes a run waiting at review
- **THEN** the newest published release reads it, resumes it to its end and stops it

### Requirement: Frozen machine names and files
The freeze SHALL hold the machine-facing reason and refusal codes, the run guard names, examples of a run's manifest and run configuration, of the files a person keeps (profiles, connector manifests, connector account files of the global and the project scope, `policy.yaml`, `config.yaml`, `decisions.yaml`, start rule documents) and of each signed store; each SHALL still be produced or read by this build with the same meaning.

#### Scenario: A renamed guard name
- **WHEN** a change renames a run guard name
- **THEN** the freeze test fails naming it

#### Scenario: A renamed refusal code
- **WHEN** a change renames the refusal code `patch_global_unsupported`
- **THEN** the freeze test fails naming the group and the code

#### Scenario: A start rule document keeps its keys
- **WHEN** a frozen start rule document is read by a later build
- **THEN** it parses, keeps every key and passes the start rule checks

#### Scenario: A signed store written by 1.0
- **WHEN** a frozen start key file signed with the test installation key is read by a later build
- **THEN** it verifies, reads, and its lookup answers the frozen bound run

#### Scenario: A global account file stops loading
- **WHEN** a change makes `load_merged` ignore the global account file
- **THEN** the freeze test fails naming the global-only account

### Requirement: Machine codes come from production values
A frozen machine code group SHALL list the values its production constants and functions export, including values that reach a result through a variable or a constant of another crate. A scan of literals SHALL serve only as the completeness check, and a code the scan cannot see SHALL be listed by hand in a checked table with its source symbol. `start_key_conflict` and `already_started` SHALL be in the frozen list.

#### Scenario: A refusal code from another crate
- **WHEN** a change renames `apb_engine::start_key::CONFLICT`, which `playbook_run` returns as its `policy`
- **THEN** the freeze test fails naming the group `mcp` and `start_key_conflict`

#### Scenario: A code passed through a variable
- **WHEN** a change adds a refusal whose `policy` member is a variable the scan cannot resolve
- **THEN** the freeze test fails asking for the codes of that function in the checked table

### Requirement: Host results keep their keys in every variant
The freeze SHALL hold the result keys of every documented result variant of every MCP tool `docs/HOST-INTEGRATION.md` or the server instructions tell a host to consume, and each SHALL still be returned in that variant. A tool or variant left out SHALL be named under "Not covered" in `docs/COMPATIBILITY.md`.

#### Scenario: A renamed result key
- **WHEN** a change renames a key of `run_wait`'s result
- **THEN** the freeze test fails naming the tool and the key

#### Scenario: A host result variant loses a key
- **WHEN** a change drops `pending_tasks` from `supervisor_wait_event`'s result while host tasks wait
- **THEN** the freeze test fails naming the tool, the variant and the key

### Requirement: Frozen files keep their meaning
Every frozen file example SHALL hold nondefault state, and its test SHALL assert named typed values of the parsed file and the decisions its reader makes, not only that it parses and keeps its keys. A frozen document without such an expectation SHALL fail the test.

#### Scenario: A field read with another meaning
- **WHEN** a change keeps a frozen manifest's keys but reads its launch record's `triggered_by` as absent
- **THEN** the freeze test fails naming the document and the field

### Requirement: The playbook store written by 1.0 still reads
A playbook store written by 1.0 SHALL read on every 1.x with the same meaning: the versions of a playbook, the `current` pointer with its history, the `candidate` pointer, the provenance sidecar (`created_by` included), the trust store and the launch floor store. The freeze SHALL hold such a store with nondefault state, written through the public write functions, and its test SHALL assert named typed values and the decisions the readers make.

#### Scenario: The history of current still reads
- **WHEN** a frozen store whose `current` moved twice is read by a later build
- **THEN** the history lists the frozen entries, complete and not pending, and `current` is the frozen version

#### Scenario: A provenance sidecar keeps its meaning
- **WHEN** a frozen sidecar with `created_by: template` is read by a later build
- **THEN** its provenance has `created_by` `template` and the other frozen fields

#### Scenario: Trust and launch floors keep their decisions
- **WHEN** the frozen trust store and launch floor store are read by a later build
- **THEN** a frozen digest is approved, another digest of a frozen id is changed, and each frozen floor key has its frozen policy

### Requirement: The written promise
`docs/COMPATIBILITY.md` SHALL state what 1.x keeps for the schema, the journal, the MCP tools, the validator codes, the frozen machine names and files and the playbook store, the deprecation rule, the platform position, what is not covered, and how the freeze is guarded and extended.

#### Scenario: A person looks up the promise
- **WHEN** a person reads `docs/COMPATIBILITY.md`
- **THEN** it names the frozen surfaces, the deprecation rule, what is not covered and the commands that extend the freeze
