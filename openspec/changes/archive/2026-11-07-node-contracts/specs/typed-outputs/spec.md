## ADDED Requirements

### Requirement: Typed output field declarations
An entry of `outputs.fields` or `list.item_outputs` SHALL be either a plain name or a map `{ name, type, values, required }` with `type` one of `string`, `number`, `bool`, `array`, `object`, `enum`. A list of plain names SHALL parse, serialize and key the node cache exactly as before.

#### Scenario: A plain list is unchanged
- **WHEN** a node declares `outputs: { fields: [a, b] }`
- **THEN** its canonical node JSON and its cache key equal those of the same node before this change

### Requirement: A contract mismatch rejects the attempt
When an attempt reports success and its output misses a required field, holds a value of the wrong type or holds an enum value outside `values`, apb SHALL reject the attempt as a non-fatal rejection whose `rejected_for` starts with `output_contract:` and names every mismatched field.

#### Scenario: A word where a number was declared
- **WHEN** a node declares `{ name: blocking, type: number, required: true }` with `max_retries: 1`
- **AND** attempt 1 writes `blocking: "two"`
- **THEN** attempt 1 is journaled `failed` with `rejected_for` containing ``field `blocking` must be a number``
- **AND** attempt 2 runs with that reason in its retry briefing

#### Scenario: An optional field left out
- **WHEN** a typed field is not `required` and the output leaves it out
- **THEN** the attempt is not rejected and `output_fields_missing` is journaled as before

### Requirement: A reason never holds the value
A contract reason SHALL name the field, the expected type and the JSON kind it got, and SHALL NOT hold the offending value or any part of it.

#### Scenario: A secret in a mismatched field
- **WHEN** a field typed `number` holds a long string that contains a configured connector secret
- **THEN** neither the journal nor the retry prompt holds any part of the secret

### Requirement: Tolerant readings
A `number` field SHALL accept a JSON number or a string that is exactly the text of a JSON number; a `bool` field `true`, `false` or those two strings; an `array` or `object` field the value or a string that parses as one; an `enum` field the text of a string, number or bool that is one of `values`. Strings such as `inf`, `NaN` or ` 80` SHALL NOT read as numbers.

#### Scenario: A numeric string passes
- **WHEN** a `number` field holds the string `"3"`
- **THEN** the contract accepts it

### Requirement: Scripts and list items
A `script` node's stdout SHALL be checked against its `outputs.fields` and a mismatch SHALL fail the node with the contract reason as its output. A list item's step SHALL be checked against `item_outputs`, never against the node's `outputs.fields`.

#### Scenario: A scripted script answer is checked too
- **WHEN** a scenario scripts a script node's stdout `{"count": "many"}` against a `number` field
- **THEN** the node fails with an output starting `output_contract:`

#### Scenario: A script prints the wrong type
- **WHEN** a script node declares `{ name: count, type: number, required: true }` and prints `{"count": "many"}`
- **THEN** the node fails with an output starting `output_contract:`

### Requirement: The status file note names the types
For a declaration with a typed entry the attempt prompt SHALL name each key with its type and whether it is required; for a plain-only declaration the note SHALL be unchanged.

#### Scenario: The note of a typed field
- **WHEN** a node declares `{ name: blocking, type: number, required: true }`
- **THEN** its first attempt's prompt contains ``blocking` (a number; required)``

### Requirement: V167 and V168
The validator SHALL report V167 (error) for a malformed typed entry and V168 (warning) when an edge, a list source, a loop condition or a write scope template reads a typed field in a way its type cannot satisfy.

#### Scenario: Values without enum
- **WHEN** a typed entry has `values` and `type: string`
- **THEN** validation fails with V167

#### Scenario: Equals on a number field with a word
- **WHEN** an `output_field` edge has `equals: high` on a field declared `type: number`
- **THEN** validation warns with V168
