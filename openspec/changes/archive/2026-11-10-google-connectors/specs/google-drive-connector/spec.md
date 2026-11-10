## ADDED Requirements

### Requirement: The google-drive connector
apb SHALL ship an official `google-drive` connector with `verify`, `search_files`, `get_file_metadata`, `export_text` and `read_text_file`. Upload SHALL NOT be offered.

#### Scenario: A document as Markdown
- **WHEN** an agent calls `export_text` on a Google Docs file with `text/markdown`
- **THEN** the result body is the document's text

### Requirement: Response content type guard
An HTTP function MAY declare `content_types`; a 2xx answer whose media type matches none SHALL fail `service` without its body being read.

#### Scenario: A binary file
- **WHEN** `read_text_file` gets an `image/png` answer
- **THEN** the call fails `service` naming the type, also when the body is over the read cap

### Requirement: Document readers need no projection
The official connector gate SHALL exempt a read-only function that declares non-empty `content_types` from the `response_pick` requirement, JSON documents included; every other read-only HTTP function SHALL still declare one.

#### Scenario: A JSON file read
- **WHEN** the gate checks `google-drive.read_text_file`, which accepts `application/json`
- **THEN** it passes without `response_pick`

### Requirement: One content-type check for live and offline
The live call and the offline contract runner SHALL apply the same content-type check to a response before its body is used.

#### Scenario: Scripted binary answer
- **WHEN** a contract case scripts an `image/png` answer for `export_text`
- **THEN** the case sees the same `service` error a live call gets
