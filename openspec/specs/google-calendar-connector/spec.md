# google-calendar-connector Specification

## Purpose
TBD - created by archiving change google-connectors. Update Purpose after archive.

## Requirements

### Requirement: The google-calendar connector
apb SHALL ship an official `google-calendar` connector over Calendar API v3 with `verify`, `list_calendars`, `list_events`, `get_event`, `query_free_busy`, `create_event`, `update_event` and `delete_event`, authenticated by `oauth2_refresh`, each function declaring its scopes.

#### Scenario: Events of a day
- **WHEN** an agent calls `list_events` with a time window
- **THEN** the request carries `singleEvents=true` and `orderBy=startTime` and later pages are followed

### Requirement: A created event is never duplicated by a repeat
`create_event` SHALL send a client-chosen event id derived from the send intent's nonce in the base32hex alphabet, and a 409 to a repeat of the same intent SHALL settle the send as confirmed.

#### Scenario: The answer was lost
- **WHEN** a create's answer is lost and the repeat with the same intent gets HTTP 409
- **THEN** the repeat answers `already_sent` with the event id and both requests carried the same id

### Requirement: Key encoding and conflict settings
The `key` send-once mode SHALL accept `encoding: base32hex` and `conflict: confirms`, both refused on other modes. Without `conflict: confirms`, a 409 SHALL keep settling the send as unknown.

#### Scenario: Encoding of a nonce
- **WHEN** the nonce is `abcdefghijklmnopqrstuvwx` and the encoding is `base32hex`
- **THEN** the key is `0123456789abcdefghijklmn`

### Requirement: Busy intervals, not computed slots
`query_free_busy` SHALL return the service's busy intervals; computing free slots SHALL be left to the agent.

#### Scenario: Free-busy query
- **WHEN** an agent queries two calendars for a window
- **THEN** the result holds the `calendars` busy lists

### Requirement: Calendar writes that mail people are irreversible
`create_event` and `update_event` SHALL be `irreversible`, and `update_event` SHALL declare send-once `mode: none`, so a repeat of an update whose outcome is unknown is refused until a person decides.

#### Scenario: A lost update answer
- **WHEN** an `update_event` answer is lost and the step repeats it with the same intent
- **THEN** the repeat is refused `unknown_effect` and no second request leaves
