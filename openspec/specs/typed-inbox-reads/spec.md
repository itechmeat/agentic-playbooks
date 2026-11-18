# typed-inbox-reads Specification

## Purpose
TBD - created by archiving change typed-inbox-reads. Update Purpose after archive.

## Requirements

### Requirement: A webhook block may name the event type
A connector's `webhook:` block SHALL accept at most one of `event_type_path` (a dot path into the body) and `event_type_header` (a header name), both literals. The listener SHALL store each verified delivery's type on its inbox event, cleaned to 1 to 100 characters of `[A-Za-z0-9._:/-]`, and an event stored before this field SHALL read as untyped.

#### Scenario: A header type
- **WHEN** a github delivery arrives with `X-GitHub-Event: issues`
- **THEN** its inbox event has `event_type: issues`

#### Scenario: Both fields
- **WHEN** a webhook block names both `event_type_path` and `event_type_header`
- **THEN** the connector does not load

### Requirement: An inbox read may filter
`inbox_read` SHALL accept `types` and `where` (a list of `{ path, equals }` over the body, compared like a list's `where`). An untyped event SHALL never pass a `types` filter. `ack` and `peek_depth` SHALL refuse a filter with `invalid_args`.

#### Scenario: Only new issues
- **WHEN** a read passes `types: [issues]` and `where: [{ path: action, equals: opened }]`
- **THEN** it returns only events of type `issues` whose `action` is `opened`

### Requirement: Skipped events are acknowledged for the reader only
A filtered read SHALL acknowledge, for its own consumer only, the non-matching events before the first match, SHALL leave non-matching events after an unacknowledged match unacknowledged, and SHALL report `skipped`.

#### Scenario: Two consumers
- **WHEN** consumer `triage` reads with a filter that skips three events
- **THEN** consumer `default` still reads all three

#### Scenario: A skip after a match
- **WHEN** pending events are skip, match, skip
- **THEN** the cursor moves past the first only, and past the third once the match is acknowledged and read again

### Requirement: Only a caller that may ack moves a cursor
A filtered read SHALL move a cursor only for a node's own call whose grant also covers the connector's `ack` function, or a person's live call. A call under a grant of read-only functions only, every engine site (an item check, a baseline or follow-up read, a reconcile) and a read-only process SHALL move no cursor and SHALL still report `skipped` and the gap.

#### Scenario: A read-only grant
- **WHEN** a node granted read-only functions only reads consumer `deploy` with a filter that matches nothing
- **THEN** the read reports every pending event as skipped and the cursor of `deploy` does not move

#### Scenario: A read-only process
- **WHEN** a read-only process calls a filtered `inbox_read`
- **THEN** the call is refused and no cursor moves

### Requirement: A filter never hides a gap
A filtered read SHALL report the inbox gap computed before any cursor moved, whatever it skipped.

#### Scenario: Every remaining event skipped
- **WHEN** retention dropped unread events and every remaining event is skipped
- **THEN** the read still returns the gap

### Requirement: A gap is journaled before a cursor moves past it
A filtered read in a run SHALL record its gap in the run journal, synced, before it moves the cursor past the gap, and SHALL move no cursor when the journal cannot take the gap; the read still returns the gap. A person's live call outside a run has no journal and SHALL return the gap in its result.

#### Scenario: The journal cannot be written
- **WHEN** a filtered read that may ack finds a gap and the run journal cannot be written
- **THEN** the cursor does not move and the result carries the gap

#### Scenario: The journal is written
- **WHEN** the same read runs again with a writable journal
- **THEN** the journal holds one `inbox_gap` event for the gap and only then the cursor moves

### Requirement: The bundled connectors name their types
The bundled github connector SHALL receive signed deliveries typed by `X-GitHub-Event` and offer `inbox_read`, `inbox_ack` and `inbox_peek_depth`; the bundled whatsapp connector SHALL type its deliveries by `entry.0.changes.0.field`.

#### Scenario: A github issue delivery
- **WHEN** a signed `issues` delivery reaches `/hooks/github/main`
- **THEN** `inbox_read` with `types: [issues]` returns it

#### Scenario: A whatsapp message delivery
- **WHEN** a signed delivery whose `entry.0.changes.0.field` is `messages` reaches `/hooks/whatsapp/main`
- **THEN** its inbox event has `event_type: messages`, and a filtered `inbox_read` reports `skipped`
