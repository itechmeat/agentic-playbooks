## ADDED Requirements

### Requirement: Array and object fields render as compact JSON in templates
A `{{nodes.<id>.output.<field>}}` or `{{nodes.<id>.report.<field>}}` template SHALL render a field whose value is a JSON array or object as compact JSON. A string, bool or number SHALL render as today. A null or absent field, or an output that is not a JSON object, SHALL render as the empty string. Edge conditions SHALL keep reading fields through `output_field_value`, so an `output_field` condition on an array field does not match.

#### Scenario: A reviewer's findings array reaches the fix node
- **WHEN** a node's status file writes `{"findings": [{"id": "a"}]}`
- **AND** a later node's prompt holds `{{nodes.<that node>.output.findings}}`
- **THEN** the rendered prompt contains `[{"id":"a"}]`

#### Scenario: Routing on an array field is unchanged
- **WHEN** an `output_field` edge condition names a field whose value is an array
- **THEN** the condition does not match
