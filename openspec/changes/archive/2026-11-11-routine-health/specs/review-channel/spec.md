## ADDED Requirements

### Requirement: A decision being written does not fail the run
The drive SHALL read `reviews.jsonl` as bytes and SHALL treat a last line that does not end with a newline and does not decode or parse (including one cut inside a multi-byte character) as a decision not yet written, reading the file again on its next poll; such a last line that parses SHALL be returned. Any other line that does not parse SHALL still fail the read.

#### Scenario: The drive reads while a decision is appended
- **WHEN** a gate waits and `reviews.jsonl` ends with half of a decision line
- **THEN** the run keeps waiting
- **AND** once the line is complete the gate consumes the decision and the run goes on

#### Scenario: A torn line in the middle
- **WHEN** a line that does not parse is followed by another line
- **THEN** reading the review channel fails

### Requirement: A poster repairs a tail a crashed poster left
A poster SHALL, under the reviews lock and before any check or sequence number, complete an unterminated last line that parses as a whole decision by adding its newline, cut an unterminated last line that does not, and SHALL write its own line and the newline in one write.

#### Scenario: A decision after a crashed poster
- **WHEN** `reviews.jsonl` ends with half a line and a person posts a decision
- **THEN** the file holds only whole lines and the new decision is the last one

#### Scenario: A whole decision missing its newline
- **WHEN** `reviews.jsonl` ends with a whole decision line without its newline and a person posts a decision
- **THEN** the earlier decision stays with its sequence number and the new decision follows it

#### Scenario: A line cut inside a character
- **WHEN** `reviews.jsonl` ends with part of a decision cut inside a multi-byte character
- **THEN** the run keeps waiting
