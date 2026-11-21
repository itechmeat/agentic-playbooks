## ADDED Requirements

### Requirement: An HTTP connector loads
A manifest SHALL accept `mcp: { transport: http, protocol: modern, url }` with `url` starting with the literal `https://` and using `{{account.*}}` only; an http block SHALL NOT set `command`, `args` or `env`, a stdio block SHALL NOT set `url`, and `transport: http` SHALL require `protocol: modern`.

#### Scenario: An http connector
- **WHEN** a manifest declares `transport: http`, `protocol: modern` and `url: "https://{{account.server_host}}/mcp"`
- **THEN** it loads

#### Scenario: A plaintext url
- **WHEN** the `url` is `http://127.0.0.1:8080/mcp`, `http://localhost:8080/mcp` or `{{account.server_url}}`
- **THEN** the manifest fails to load naming the literal `https://`

#### Scenario: A secret in the url
- **WHEN** the `url` holds `{{secret.token}}`
- **THEN** the manifest fails to load

#### Scenario: An older revision over http
- **WHEN** an http block leaves `protocol` at `auto`
- **THEN** the manifest fails to load naming `protocol: modern`

### Requirement: HTTP authorization reuses the connector auth block
An http MCP connector SHALL take the connector-level `auth:` of kind `header` or `oauth2_refresh` only, minted and consented by the existing connector OAuth code; a stdio MCP connector SHALL NOT set `auth`, and no `mcp.auth` block SHALL exist.

#### Scenario: A query token
- **WHEN** an http MCP connector sets `auth: { kind: query }`
- **THEN** the manifest fails to load naming `header` or `oauth2_refresh`

#### Scenario: The consent
- **WHEN** a person runs `apb connector oauth` for an http MCP connector with `oauth2_refresh` and a `healthcheck`
- **THEN** the existing consent runs unchanged

### Requirement: The endpoint is bound by the approval
The endpoint SHALL be rendered from the approved definition and account's non-secret fields, carry no query, and be checked through the same pin binding as stdio; the rendered endpoint and an `oauth2_refresh` token URL SHALL be checked before any secret is resolved, any secret helper runs or any request leaves, on calls, live listings and approvals.

#### Scenario: A changed account URL
- **WHEN** the account's server host is edited after approval
- **THEN** the call is refused `mcp_server_changed` and neither the token endpoint nor the server receives a request

#### Scenario: A refused endpoint starts no helper
- **WHEN** the endpoint or the token URL is refused and the account's secret comes from a `{{cmd:...}}` helper
- **THEN** the call and the live listing are refused and the helper never runs

### Requirement: An HTTP MCP endpoint is https only
The endpoint and an `oauth2_refresh` token URL SHALL be `https`; plaintext `http`, on the loopback and by the name `localhost` included, SHALL be refused at load and again at call time, and only apb's own test builds SHALL admit `http://127.0.0.1:` through the existing `testing` feature gate. The TLS client SHALL keep the default certificate verification, so web PKI authenticates the peer on every call while the approval pins the URL and the account.

#### Scenario: A plaintext endpoint at call time
- **WHEN** the rendered endpoint or token URL is plaintext `http`, the loopback included
- **THEN** the call is refused before any request

### Requirement: The HTTP transport takes nothing from the caller
The HTTP transport and the token request of an MCP connector's `oauth2_refresh` account SHALL ignore the calling process's proxy variables, the transport SHALL follow no redirect, send only the definition's and account's headers and the protocol headers `mcp-protocol-version`, `mcp-method` and `mcp-name`, read each answer up to the line cap, count event-stream messages toward the session bound, and digest the raw `tools/list` JSON.

#### Scenario: A caller's proxy
- **WHEN** `HTTPS_PROXY` points at a listener in the calling process's environment
- **THEN** the listener sees no connection

#### Scenario: A caller's proxy and the token request
- **WHEN** `HTTPS_PROXY` and `HTTP_PROXY` point at a listener and an MCP connector's `oauth2_refresh` call mints its access token
- **THEN** the token endpoint receives the request and the listener sees no connection

#### Scenario: A changed member only the raw JSON carries
- **WHEN** the server changes only a tool's `execution` member
- **THEN** the next call is refused `mcp_tool_changed` and the server receives no `tools/call` request

### Requirement: Errors after the hand-over are unknown
A `tools/call` SHALL count as handed over before its POST opens a connection, and every failure of a write after that (a 401 or 403, any other HTTP status, any JSON-RPC error, a broken stream, a refused frame, a refused connection, a timeout) SHALL settle the send intent `unknown`, through the same classifier the stdio transport uses; only a POST never started SHALL settle `not_sent`.

#### Scenario: A revoked token on the call
- **WHEN** the server answers a write's `tools/call` with 401
- **THEN** the outcome is `tool_error` and the intent settles `unknown`

#### Scenario: A JSON-RPC error, a status, a broken stream or a timeout on the call
- **WHEN** a write's `tools/call` is answered with a JSON-RPC error, a 500, an event stream that ends without the answer, or no answer before the deadline
- **THEN** the journaled intent settles `unknown`, never `not_sent`

#### Scenario: A body that stalls after its headers
- **WHEN** the response head of a JSON body or of an event stream arrives and its body stalls past the deadline
- **THEN** the failure is reported as a timeout, however the HTTP library wraps it, never as a network failure, and a write's intent settles `unknown`

### Requirement: A 401 while connecting refreshes once with the new token
A 401 before the `tools/call` hand-over SHALL refresh an `oauth2_refresh` token once and connect once more, with the new access token replacing the bearer and the new access token and any rotated refresh token registered for redaction and for the reflected-secret check before the reconnect; a 401 on `tools/call` SHALL NOT be retried.

#### Scenario: A stale token at connect
- **WHEN** `server/discover` answers 401 for an `oauth2_refresh` account
- **THEN** the token is refreshed once and the connection retried once before any intent is recorded, and the retried request carries the new access token, never the rejected one

#### Scenario: A listing that echoes the refreshed token
- **WHEN** after a 401 refresh the `tools/list` answer carries the new access token or the rotated refresh token
- **THEN** the call is refused `mcp_tools_reflect_a_secret`, nothing is hashed or stored, no `tools/call` is sent, and neither value reaches the journal or the result

#### Scenario: A refused connection on the call
- **WHEN** the server stops listening after the listing and a write's `tools/call` cannot connect
- **THEN** the journaled intent settles `unknown`, never `not_sent`

#### Scenario: A rotated refresh token outside a run
- **WHEN** the token endpoint answers a new refresh token to the healthcheck, the approval's listing or the live listing of an `oauth2_refresh` account
- **THEN** that surface's answer carries the rotation warning (the healthcheck's `warnings`, the listing's note), with neither token in it, and the new refresh token is not stored

#### Scenario: A cached access token keeps its mint's rotation
- **WHEN** a listing or a call in the same process takes the cached access token of a mint that rotated the refresh token (the MCP surfaces and the HTTP and IMAP connectors share that cache)
- **THEN** the rotated refresh token is registered for redaction and for the reflected-secret check exactly as on the use that minted, so a listing that carries it is refused `mcp_tools_reflect_a_secret` and an answer that carries it is redacted, and the answer carries the rotation warning again, for as long as the cache entry lives

#### Scenario: A refusal after a rotating mint
- **WHEN** after a mint that rotated the refresh token the discovery or the listing fails, a pinned tool changed, the listing reflects a secret, or another verification refuses
- **THEN** the refusal carries the rotation warning as the result's `warnings` (and the CLI prints it after the error), with neither token in it

### Requirement: One strict reader decides every HTTP answer
Every HTTP answer SHALL be read whole and passed through the same strict reader and listing capture as a stdio line before any message of it reaches the protocol library: a JSON body as one frame, an event stream framed by one strict event-stream reader and every event accepted first. A part that is not exactly one frame SHALL end the session with nothing of that answer handed on, and nothing SHALL be dropped as log text.

#### Scenario: A duplicated member in a tool definition
- **WHEN** a `tools/list` body names a member of a tool twice
- **THEN** the call is refused `mcp_message_invalid`, nothing is hashed and no `tools/call` is sent

#### Scenario: A bad event before the answer
- **WHEN** an event stream carries an event with an unknown field and then the answer
- **THEN** the session ends and the answer is not used

#### Scenario: A plain text body
- **WHEN** a 200 `application/json` answer holds text that is not JSON
- **THEN** the session ends `mcp_message_invalid` and no line is counted as dropped

### Requirement: Only the answer of a request is handed on
A 2xx JSON body SHALL be handed on only when it answers the request (the response or error for its id), as an event stream's answering frame and a non-2xx body are; anything else SHALL end the session `mcp_message_invalid`, so a server request over HTTP is never answered.

#### Scenario: A JSON body that is not the answer
- **WHEN** a `tools/call` is answered 200 `application/json` with a server request or another id's response
- **THEN** the call ends `mcp_message_invalid` at once and no further POST is sent

### Requirement: The pin binds the endpoint and the token URL
The approval of an http MCP connector account SHALL record, with the tree and account digests, an endpoint digest over the rendered endpoint and the `oauth2_refresh` token URL, and every call, live listing and approval SHALL recompute it from the rendered endpoint and token URL and compare it before any secret is resolved; a fault SHALL carry no server-chosen text.

#### Scenario: An endpoint digest that differs
- **WHEN** the recorded endpoint digest differs from the one the call renders
- **THEN** the call is refused `mcp_server_changed` and neither the token endpoint nor the server receives a request

### Requirement: An http MCP connector is approved only at a terminal
The approval of an http MCP connector or its account SHALL go through the same terminal and run-step refusals as a stdio one, and SHALL send nothing to the token endpoint or the server before the first typed confirmation.

#### Scenario: No terminal
- **WHEN** `apb connector approve` runs for an http MCP connector account without an interactive terminal
- **THEN** it is refused `mcp_approve_needs_a_person`, no pin is written and neither endpoint receives a request

### Requirement: An event stream is read up to its answer
An event with no `data` or an empty `data` SHALL dispatch nothing and be skipped, its bytes counted; one leading space after a field's colon SHALL be stripped; a content type SHALL be matched on its media type, ASCII case-insensitive, parameters ignored; reading SHALL stop once the answer's event is whole and the connection SHALL then be closed; a 202 or 204 SHALL answer only a notification.

#### Scenario: A priming event
- **WHEN** a stream starts with an event that has an `id` and an empty `data`
- **THEN** that event is skipped and the answer after it is read

#### Scenario: A stream kept open
- **WHEN** the server sends the answer and keeps the stream open
- **THEN** the call is answered without waiting for the deadline and nothing after the answer is read

### Requirement: Header annotations are not supported over HTTP
A `tools/list` answer whose tool definitions carry an `x-mcp-header` member SHALL be refused before it reaches the protocol library, ending the session; the client SHALL refuse any request header name the library hands over other than `mcp-protocol-version`, `mcp-method` and `mcp-name`, so no argument value is ever sent in a header.

#### Scenario: An annotated tool
- **WHEN** an HTTP server lists a tool whose input schema annotates a property with `x-mcp-header`
- **THEN** the listing fails with `mcp_header_annotation_unsupported` and no `tools/call` is sent

### Requirement: A reserved auth header is refused
An http MCP connector whose `auth` of kind `header` names a header the transport sets (`mcp-protocol-version`, `mcp-method`, `mcp-name`, `content-type`, `accept`, `host`, `content-length`, `transfer-encoding`, `mcp-session-id`, `last-event-id`, and `authorization` with OAuth) SHALL be the validator error V303 at its binding sites, and the client SHALL refuse such a header before anything is sent.

#### Scenario: Content-Type as the auth header
- **WHEN** a playbook binds an http MCP connector whose auth header is `Content-Type`
- **THEN** validation reports V303
