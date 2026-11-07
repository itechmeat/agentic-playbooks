## ADDED Requirements

### Requirement: Threshold comparators on output_field
An `output_field` condition SHALL carry exactly one of `equals`, `lt`, `lte`, `gt`, `gte`. A threshold SHALL match when the field's value is a JSON number, or a string that is exactly the text of a JSON number, and compares as stated.

#### Scenario: Below a threshold
- **WHEN** an edge has `{ type: output_field, node: score, field: coverage, lt: 80 }`
- **AND** the node's output is `{"coverage": "72.5"}`
- **THEN** the edge matches

#### Scenario: Text is not compared as text
- **WHEN** an edge has `lt: 10` and the field holds `9`
- **THEN** the edge matches

### Requirement: Unreadable values never match
A threshold SHALL NOT match, and SHALL NOT fail the run, for a bool, null, array, object, non-numeric string, absent field or output that is not a JSON object.

#### Scenario: A word in the field
- **WHEN** an edge has `gte: 10000` and the field holds `"large"`
- **THEN** the edge does not match and routing continues with the next edge

### Requirement: Labels and round trip
The dashboard graph and the HTML report SHALL label a threshold edge `<field> < N`, `<= N`, `> N` or `>= N`, and the playbook editor SHALL keep the comparator on save.

#### Scenario: The editor keeps a threshold
- **WHEN** a playbook with an `lt` edge is loaded, edited elsewhere and saved
- **THEN** the saved YAML still holds the `lt` comparator

### Requirement: V169 for a malformed comparator
The validator SHALL report V169 (error) for an `output_field` condition with no comparator, more than one, or a threshold that is not a finite number.

#### Scenario: Two comparators
- **WHEN** a condition has both `equals` and `gt`
- **THEN** validation fails with V169
