# gmail-connector Specification

## Purpose
TBD - created by archiving change google-connectors. Update Purpose after archive.

## Requirements

### Requirement: The gmail connector
apb SHALL ship an official `gmail` connector with `verify`, `list_labels`, `search_messages`, `get_message`, `get_thread`, `create_draft`, `send_draft`, `add_labels`, `remove_labels` and `archive`, and no delete. `send_draft` SHALL be a separate, irreversible function.

#### Scenario: Drafting does not send
- **WHEN** a node is granted `create_draft` but not `send_draft`
- **THEN** it can prepare a reply and cannot send it

### Requirement: A composed message in a JSON body
An HTTP function MAY declare `mime` with a body path and a message block; the engine SHALL compose the RFC 5322 message with the SMTP composer, keep `Bcc`, and place its URL-safe base64 at the path. Dry-runs and contract tests SHALL show a readable preview instead.

#### Scenario: A reply in a thread
- **WHEN** `create_draft` is called with `thread_id`, `in_reply_to` and `references`
- **THEN** the body carries `threadId` and `message.raw` decodes to a message with those headers and the account's address as `From`

### Requirement: Base64url text decoded in responses
An HTTP function MAY declare `decode_base64url` paths; on a 2xx the engine SHALL decode each string there to UTF-8 text before the projection, cap it at 256 KiB, and replace bytes that are not UTF-8 with null and a `_binary` flag.

#### Scenario: A message body
- **WHEN** `get_message` returns a part whose `body.data` is base64url of `Hello`
- **THEN** the result shows `Hello`

### Requirement: One label filter per search
`search_messages` SHALL take an optional single `label_id`, sent once as `labelIds`; several labels SHALL go into `q`.

#### Scenario: Unread inbox
- **WHEN** `search_messages` is called with `q: is:unread` and `label_id: INBOX`
- **THEN** the URL carries `labelIds=INBOX` once and `q=is%3Aunread`
