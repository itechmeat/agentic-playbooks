# param-pattern Specification

## Purpose
TBD - created by archiving change recurring-routines. Update Purpose after archive.

## Requirements

### Requirement: Text params may declare a pattern
A `text` param MAY declare `pattern`, a Rust regular expression that the whole value SHALL match (anchored at both ends, the value neither trimmed nor case folded), secret params included.

#### Scenario: A partial match is not enough
- **WHEN** `repo` has `pattern: '[a-z]+'` and a start passes `repo=abc; rm`
- **THEN** the value does not match

### Requirement: Every start checks the pattern
Every launch surface (MCP, the dashboard, the CLI) SHALL refuse a start whose given value does not match its param's pattern with policy `param_pattern_mismatch`, naming the params and patterns and never the value. Run preparation SHALL refuse such a value on every other path (a trial, an eval case, a detached drive), except a scenario run's secret placeholder.

#### Scenario: The CLI refuses a bad value
- **WHEN** `apb run triage --param repo='a b'` runs and `repo` has `pattern: '[A-Za-z0-9._-]{1,100}'`
- **THEN** the start is refused with `param_pattern_mismatch` and no run directory is created

#### Scenario: A secret value is never echoed
- **WHEN** a secret param's value does not match its pattern
- **THEN** the refusal names the param and its pattern and does not hold the value

### Requirement: Remembered values must match
A remembered value that does not match its param's pattern SHALL be incompatible with reason `pattern_mismatch`, and a value that does not match SHALL NOT be remembered.

#### Scenario: A remembered value under a new pattern
- **WHEN** memory holds `region=emea` and the param now has `pattern: '[a-z]{2}'`
- **THEN** recall reports it incompatible with `pattern_mismatch` and the param needs a value

### Requirement: V177 and V178
The validator SHALL report V177 (error) for a pattern on a param that is not `text`, an empty pattern, one over 512 characters, or one that does not compile, and V178 (error) for a default, or a scenario param value, that does not match.

#### Scenario: A pattern on an int param
- **WHEN** a param of `type: int` has a pattern
- **THEN** validation fails with V177

#### Scenario: A default that does not match
- **WHEN** a param has `default: "EU"` and `pattern: '[a-z]{2}'`
- **THEN** validation fails with V178
