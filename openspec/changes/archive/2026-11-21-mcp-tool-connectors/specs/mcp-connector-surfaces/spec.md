## ADDED Requirements

### Requirement: The approve output shows the program
`apb connector approve <connector> --account <account>` for an MCP connector SHALL print, before it asks, the command, the absolute program path and the program's full digest, a warning line when the command is an interpreter or a launcher, and right above the question the program digest line, the count of tools in each status and the pin digest.

#### Scenario: A launcher in the approve output
- **WHEN** the connector's command is `npx`
- **THEN** the output says the approval covers the `npx` file only, not the script or package it runs

### Requirement: The approve output shows each tool
The output SHALL show each mapped tool with its status before the approval, its full digest, its description (for a changed tool the pinned and the new description and the changed members) and its annotations marked as shown and not used, and the unmapped tools by name, every server text sanitized for the terminal. A tool the server does not list SHALL be a warning and SHALL NOT fail the command.

#### Scenario: A missing tool in the approve output
- **WHEN** a function maps a tool the server does not list and the person confirms
- **THEN** the approval prints a warning for that tool, pins the others and exits 0

#### Scenario: Server text with an escape sequence
- **WHEN** a tool's description holds a terminal escape sequence
- **THEN** the output prints it without the escape sequence

### Requirement: The approve output shows each tool's title and input schema
The output SHALL print each mapped tool's title and input schema, redacted (strings, member names and numbers alike), sanitized for the terminal and cut with a marker past a length limit.

#### Scenario: A tool's title and input schema
- **WHEN** a mapped tool has a title and an input schema longer than the output limit
- **THEN** the output prints the title and the start of the schema, ending with the cut marker

### Requirement: Both confirmations show the definition safely
Every string either confirmation shows from the definition, the account or the file system SHALL be sanitized for the terminal, and the tree confirmation SHALL show every declared `env` name with its unrendered template, secret placeholders unresolved.

#### Scenario: Environment templates in the tree confirmation
- **WHEN** a connector declares `HTTPS_PROXY` from an account field and a token from a secret
- **THEN** the tree confirmation prints both names with their templates as written and reads no account value or secret

### Requirement: No live MCP call outside a run
Outside a run, a live call of an mcp function (the dashboard playground, a pinned notice) SHALL be refused `mcp_live_call_unsupported` before any secret is resolved and before any server starts, and a dry-run SHALL still render. The healthcheck of an MCP connector SHALL start the pinned server, list its tools, compare the whole binding of every mapped tool and close it, without a `tools/call`, a journal line or a written file.

#### Scenario: The healthcheck of an MCP connector
- **WHEN** a person runs the dashboard healthcheck of a pinned MCP connector account
- **THEN** it answers ok with the number of tools checked and the server received no `tools/call`

#### Scenario: A connector with no healthcheck
- **WHEN** the healthcheck is asked of an MCP connector that declares no `healthcheck` function
- **THEN** it is refused as on main for an absent healthcheck and no program starts

#### Scenario: A playground call
- **WHEN** the playground calls an mcp function live and the account's token comes from a marker-writing `{{cmd:...}}` helper
- **THEN** it is refused `mcp_live_call_unsupported`, the helper never runs and no server starts

### Requirement: Call arguments are an object
A call of an mcp function whose arguments are a scalar, an array or `null` SHALL be refused `invalid_args` before any server starts and before a send intent is recorded.

#### Scenario: An array as arguments
- **WHEN** an mcp function is called with `--args '[1]'`
- **THEN** the call fails `invalid_args`, no server starts and the journal holds no send intent

### Requirement: Calls record the verified tool
A `connector_call` of an mcp function that reached its server SHALL carry `tool_digest`, the digest it verified, and every other call, a doubled one included, SHALL omit the field.

#### Scenario: The journal of an MCP call
- **WHEN** an mcp function call is journaled
- **THEN** its `tool_digest` equals the pinned digest

#### Scenario: Another kind
- **WHEN** an HTTP function call is journaled
- **THEN** the event has no `tool_digest` field

### Requirement: Exposure is unchanged by MCP grants
An attempt of a node granting an MCP connector SHALL expect the shell tool only, as for any connector grant.

#### Scenario: A grant adds no expected server
- **WHEN** a node grants an MCP connector and the agent reports no MCP server beyond apb's own
- **THEN** no `tool_exposure_mismatch` is journaled

### Requirement: Doubles cover MCP functions with no new mechanism
In an eval run whose case doubles an mcp function through the existing `connectors:` doubles, a call of that function SHALL be answered by the double without starting the server, resolving a secret or checking a pin; the run gate SHALL still require the function's tools to be pinned.

#### Scenario: A doubled write
- **WHEN** an eval case doubles `create_comment` of an MCP connector and the run calls it
- **THEN** the call answers the double's body and the server was never started

#### Scenario: An eval case with its own config dir
- **WHEN** an eval case runs under the config dir it was handed and calls an mcp function it does not double, with the pins in that dir
- **THEN** the call is refused `mcp_config_dir_redirected` and no server starts
