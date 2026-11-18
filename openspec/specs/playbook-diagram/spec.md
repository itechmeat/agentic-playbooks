# playbook-diagram Specification

## Purpose
Every playbook version carries a flowchart text derived from its definition, so a person can read the graph without the dashboard and the text never drifts from what runs.

## Requirements

### Requirement: Every version has a derived diagram
Every write of a playbook version SHALL leave `playbook.mmd` beside its `playbook.yaml`: a flowchart text derived by one pure function from the definition, nodes in definition order, edges in edge-address order, every label escaped, and a first line carrying the YAML's digest.

#### Scenario: A saved version
- **WHEN** a person saves a new version
- **THEN** its directory holds a `playbook.mmd` whose first line names the digest of its `playbook.yaml`

#### Scenario: Author text stays in its label
- **WHEN** a node title holds a quote, a line break and an init directive
- **THEN** every line of the diagram starts with the header, the flowchart line or a node or edge line

### Requirement: No crash leaves a version without its diagram
No crash point SHALL leave a version directory that can be read or named by `current` holding a `playbook.yaml` without the diagram derived from exactly those bytes, and `current` SHALL NOT move to a version until its diagram matches its YAML.

#### Scenario: A retry after a crash
- **WHEN** a draft's creation or a schema migration stopped between writing the YAML and its diagram, and is run again
- **THEN** the version ends with the diagram of its YAML before `current` names it

### Requirement: An edit of the diagram is overwritten
Neither a person nor an agent SHALL maintain `playbook.mmd`: whenever apb writes or repairs it (every version write, every move of `current`, `apb diagram --rebuild`), the result SHALL be the diagram of exactly the YAML bytes, whatever was there. An entry at that name that is not a regular file SHALL be replaced by a rename over it, never followed and never first removed, and a directory at that name SHALL refuse the move of `current` with a message that names the file.

#### Scenario: An edited diagram and a move of current
- **WHEN** a person edits `playbook.mmd` of an older version and makes that version current
- **THEN** the move writes the diagram of its YAML over the edit

#### Scenario: A link at the diagram's name
- **WHEN** `playbook.mmd` of a version is a link to a file elsewhere and the version is made current or rebuilt
- **THEN** a regular file with the diagram replaces the link and the file it pointed at is unchanged

#### Scenario: No moment without a diagram
- **WHEN** a link or a FIFO at `playbook.mmd` is repaired
- **THEN** the new file is written under a temporary name in the same directory and renamed over the entry, so the version holds a `playbook.mmd` at every moment of the repair

#### Scenario: An entry that cannot be replaced
- **WHEN** `playbook.mmd` of a version is a directory, empty or not, and the version is made current
- **THEN** the move is refused with a message naming the file, and `current` does not move

### Requirement: The diagram is never read back
No apb surface SHALL read `playbook.mmd` to show, export or decide anything: `apb diagram`, MCP `playbook_get`, the dashboard and the HTML report SHALL render the text from the YAML, and no digest, approval or bundle SHALL include the file.

#### Scenario: An edited diagram
- **WHEN** a person edits `playbook.mmd`
- **THEN** `apb diagram`, MCP `playbook_get` and the dashboard still return the text of the YAML and the version's digest is unchanged

### Requirement: Doctor names diagrams that drifted and a verb repairs them
`apb doctor` SHALL name missing, stale and edited diagrams without writing anything, and `apb diagram --rebuild` SHALL write them again from the YAML.

#### Scenario: A missing diagram
- **WHEN** a version written by an older apb has no `playbook.mmd`
- **THEN** `apb doctor` warns and `apb diagram --rebuild --all` writes it

#### Scenario: A linked playbook directory
- **WHEN** a playbook or version directory in the store is a link to a directory elsewhere
- **THEN** `apb diagram --rebuild` writes nothing there, and `apb doctor` follows no link when it reads a diagram
