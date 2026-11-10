## ADDED Requirements

### Requirement: The oauth2_refresh auth kind
A connector SHALL be able to declare `auth: { kind: oauth2_refresh }` with a token URL, a client id, an optional client secret and a refresh token. The client secret and the refresh token SHALL each be exactly one `{{secret.<field>}}` placeholder. Every other template of the block SHALL accept `account.*` placeholders only.

#### Scenario: A literal refresh token is refused
- **WHEN** a manifest sets `refresh_token_template: "abc"`
- **THEN** the manifest does not load and the error names `refresh_token_template`

### Requirement: The access token stays in memory
The engine SHALL mint the access token from the refresh token at call time and keep it in process memory only. It SHALL NOT appear in the journal, a result, an error message, a prompt or any file.

#### Scenario: A token echoed by the service is redacted
- **WHEN** the service answers with a body that contains the access token
- **THEN** the call result shows `[redacted:oauth:access_token]` in its place
- **AND** neither the access token nor the refresh token appears in the run's `events.jsonl`

### Requirement: Refresh before expiry and once after a 401
A cached access token with 60 seconds or less left SHALL be refreshed before use. After an HTTP 401 the engine SHALL refresh once and send the same request once more; that answer SHALL be final. One call SHALL stay one journal event and one `max_calls` unit.

#### Scenario: An expired token is renewed inside the call
- **WHEN** the service answers 401, then 200 after the refresh
- **THEN** the call succeeds after two token requests and two service requests

#### Scenario: A second 401 is final
- **WHEN** the service answers 401 twice
- **THEN** the call fails `auth` after two service requests

### Requirement: Required scopes
A function MAY declare `scopes`; the call SHALL need any one of them. The granted set SHALL come from the account's granted scopes when set, else from the token response's `scope`. A missing scope SHALL fail the call as `auth` before the service request, naming the scopes and the `apb connector oauth` command.

#### Scenario: A read-only token cannot draft
- **WHEN** an account declares `granted_scopes` without any scope `create_draft` lists
- **THEN** the call fails before any request leaves and the message names `apb connector oauth`

### Requirement: A rotated refresh token is not stored
When the token endpoint returns a different refresh token, apb SHALL NOT store it and SHALL add a warning to the call result's `warnings`.

#### Scenario: Rotation warns
- **WHEN** a refresh answer carries a new `refresh_token`
- **THEN** the result has a `warnings` entry naming the `{{cmd:...}}` remedy and the new value is redacted

### Requirement: Person-only consent command
`apb connector oauth <connector> [--account <a>] [--scope <s>]...` SHALL run an authorization-code flow on 127.0.0.1 with a random port, PKCE (S256) and a checked `state`, and print the refresh token once on stdout. It SHALL be refused inside a run step and without a terminal, and SHALL write nothing to disk.

#### Scenario: Refused inside a run
- **WHEN** the command runs with `APB_RUN_ID` set
- **THEN** it exits 2 before contacting anything

#### Scenario: A forged callback is rejected
- **WHEN** the callback carries a `state` other than the one sent
- **THEN** no code is exchanged and the flow fails

### Requirement: IMAP over the same token source
The IMAP `auth_method: oauth2` SHALL authenticate with XOAUTH2 using an access token from the connector's `oauth2_refresh` block.

#### Scenario: IMAP mints and logs in
- **WHEN** an imap account uses `auth_method: oauth2`
- **THEN** the AUTHENTICATE payload carries the minted access token and the call result does not

### Requirement: Token failures leave no unknown send
Minting the access token and checking scopes SHALL happen before any request reaches the service. A failure there SHALL settle a recorded send intent `not_sent`, journal no `connector_call`, and never report a token-endpoint status as the service's HTTP status.

#### Scenario: Token endpoint times out on a draft
- **WHEN** the token endpoint of a `mode: none` function times out
- **THEN** the call fails `timeout`, the intent is `not_sent`, and a repeat with the same intent is sent

### Requirement: One deadline per call
Token minting, the service request, the refresh and retry after a 401, and every page SHALL share one deadline: the function's `timeout_sec`, lowered by a caller's bound, from the start of the call.

#### Scenario: A stalled token endpoint
- **WHEN** a call with `timeout_sec: 2` meets a token endpoint that never answers
- **THEN** it fails `timeout` about 2 seconds after it started

### Requirement: The whole result is scrubbed
Every credential minted or received in a call, including after a 401 refresh and for IMAP, SHALL be redacted from the result body, the `link` value, warnings and error messages.

#### Scenario: A token echoed in a Link header
- **WHEN** the service echoes the access token in its `Link` header
- **THEN** the returned `link` shows the redaction label instead

### Requirement: A bounded consent callback
The consent command SHALL bound accepting, reading the callback (at most 8 KiB of headers) and the code exchange by one deadline, refuse a callback that repeats `code`, `state` or `error`, and resolve only the client secret of the account.

#### Scenario: A stalled browser connection
- **WHEN** a client connects to the loopback port and never finishes its request
- **THEN** the flow ends with a timeout at the deadline

#### Scenario: First consent without a refresh token value
- **WHEN** the account's refresh token variable has no value yet
- **THEN** the consent plan succeeds and no refresh token helper runs
