# write-scope Specification

## Purpose
TBD - created by archiving change node-contracts. Update Purpose after archive.

## Requirements

### Requirement: Declared write scope
An `agent_task` or `script` node MAY declare `writes` (a list of globs relative to the node's directory, or one template that renders a JSON array or a newline list) and `max_changed_files`. A template SHALL render once per execution, before the first attempt, and a node whose scope renders empty or names a path outside its directory SHALL fail without starting.

#### Scenario: A scope from an earlier node
- **WHEN** `writes: "{{nodes.plan.output.files}}"` and `plan` output `{"files": ["src/a.rs"]}`
- **THEN** the node's scope is `src/a.rs`

### Requirement: A change outside the scope fails at once
After every attempt apb SHALL compare the node's tree with its state before the attempt, using the read-only check's tree comparison, and a changed file outside `writes`, or more changed files than `max_changed_files`, SHALL fail the node at once with a `write_scope:` reason and `failure_kind` `write_scope`, with no retry, no fallback and no restore. Changes to the repository state (HEAD, refs, index, control files) SHALL NOT count as changed files.

#### Scenario: A stray edit
- **WHEN** a node with `writes: ["src/**"]` changes `docs/a.md`
- **THEN** the node fails with a reason starting `write_scope:` that names `docs/a.md`
- **AND** no retry is started and `docs/a.md` keeps the change

#### Scenario: A commit inside the scope
- **WHEN** the node commits a change to `src/a.rs`
- **THEN** the attempt passes the scope check

### Requirement: An incomplete comparison fails
A scope check whose before or after tree state hit an entry or byte limit SHALL have an unknown result and SHALL fail the node. The scope SHALL NOT be described as constraining git-ignored files, the run's own `.apb` files, paths outside the node's tree or external effects.

#### Scenario: The tree grows past the limit
- **WHEN** an attempt of a scoped node makes the tree larger than the entry limit
- **THEN** the node fails with a `write_scope:` reason saying the tree is too large to compare whole

### Requirement: Scoped writers run alone
A node with `writes` or `max_changed_files` SHALL NOT share a concurrent batch with any other writer, scoped or not. Unscoped writers SHALL batch as before.

#### Scenario: Disjoint scopes still run one after another
- **WHEN** two fan-out siblings declare `writes: ["src/a/**"]` and `writes: ["src/b/**"]` and `max_parallel` is 2
- **THEN** their attempts never overlap

### Requirement: No cache for a scoped node
A node with `writes` or `max_changed_files` SHALL NOT be served from the node cache, whether or not validation ran.

#### Scenario: A second run executes again
- **WHEN** a scoped node with `cache: auto` runs twice with validation bypassed
- **THEN** the second run journals no `node_cache_hit`

### Requirement: V170 to V172
The validator SHALL report V170 (error) for a malformed or misplaced `writes` or `max_changed_files`, `cache` on such a node included, V171 (warning) for two unscoped writers that can run side by side, and V172 (warning) for a scoped writer that could run side by side with another writer.

#### Scenario: Writes on a read-only node
- **WHEN** a node has `read_only: true` and `writes: ["src/**"]`
- **THEN** validation fails with V170

### Requirement: A run guard for the write scope
A run whose definition declares `writes` or `max_changed_files` SHALL carry the run guard name `write_scope`, so an apb that does not know it refuses to continue that run; a fresh launch by such an apb is not protected, and the docs SHALL say so.

#### Scenario: The guard is named
- **WHEN** a playbook declares `writes` on any node
- **THEN** `guarded_features` returns `write_scope`
