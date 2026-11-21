# mcp-connector-listing Specification

## Purpose
A person sees the tools of an approved MCP connector's server outside an approval: `apb connector tools`, the doctor rows, MCP `connectors_list` and the dashboard's MCP section list them against the pins, write no pin and show no digest, and a live listing that starts the server with the account's secrets takes only a person's channel.

## Requirements

### Requirement: A person can list an approved server's tools
`apb connector tools <connector> [--account <a>]` SHALL start only the pinned server of an approved account, list each mapped tool with its status (`pinned`, `unpinned`, `changed`, `missing`) and no digest, write nothing, exit 3 when a row is not `pinned` and exit 2 on a refusal or for a connector without `mcp:`.

#### Scenario: A changed tool
- **WHEN** a pinned tool's description changed on the server
- **THEN** its row says `changed`, no digest is printed, and the command exits 3

#### Scenario: An account nobody approved
- **WHEN** the account has no pin record
- **THEN** the command exits 2 with `mcp_server_unpinned`, no secret is resolved and no server starts

#### Scenario: A run step
- **WHEN** the command runs inside a run step
- **THEN** it is refused and nothing starts

### Requirement: Listing surfaces serialize an explicit public shape
`apb connector tools --json` and the dashboard's live listing route SHALL answer a public listing shape of named fields only, never the internal listing serialized whole; a field of the internal listing outside that shape SHALL NOT reach the CLI, MCP or dashboard output until it is added to the shape.

#### Scenario: The named fields
- **WHEN** a person lists an approved account's tools with `--json`
- **THEN** the output holds connector, account, era, the cleanup note, the server's command, program, launcher and status, each row's function, tool, status, title, description, pinned description, members, annotations and input schema, and the unmapped tool names, and nothing else

#### Scenario: An unlisted field
- **WHEN** the internal listing carries members the public shape does not name (the digests)
- **THEN** the output's keys are exactly the shape's named fields and none of those members appears

### Requirement: The changed-tool refusal names the listing
A call refused `mcp_tool_changed` SHALL name `apb connector tools <c> --account <a>` and SHALL NOT name the approve command or carry a digest.

#### Scenario: A refused call
- **WHEN** a call is refused because its tool changed
- **THEN** the message names `apb connector tools` and holds neither `apb connector approve` nor a digest

### Requirement: Surfaces show MCP approvals without starting servers
`apb connector doctor`, MCP `connectors_list` and the dashboard's connector detail SHALL report each MCP connector's transport and which accounts' approvals cover each tool from the pin store, without starting a server, printing a digest or naming the approve command.

#### Scenario: Doctor
- **WHEN** a person runs `apb connector doctor` with an MCP connector installed
- **THEN** it shows one pin row per account and starts no server

#### Scenario: connectors_list
- **WHEN** an agent calls `connectors_list`
- **THEN** each mcp function names its tool and approved accounts, and no server starts

### Requirement: The dashboard lists live tools and writes nothing
The dashboard SHALL show each MCP account row with its project's identity (workspace id and label) and its own approval, and SHALL list an approved account's live tools only when a person asks, in the project the row names, for a signed-in session only, through a route that takes the healthcheck's order, writes nothing, shows no digest and no approve command, and renders server text as text.

#### Scenario: The live listing route
- **WHEN** the dashboard lists an MCP connector's tools
- **THEN** the answer carries the rows without digests and the pin store is unchanged

#### Scenario: The same account name in two projects
- **WHEN** two projects each configure an account `work` of an MCP connector and only the first is approved
- **THEN** the machine-wide detail shows two `work` rows, each naming its project with its own approval, and the live listing route lists only the project its `workspace` names, refusing a request without one on a machine-wide dashboard

#### Scenario: A keyless dashboard
- **WHEN** a keyless dashboard request or a bearer key asks the live listing route for an MCP connector's tools
- **THEN** it is refused 403 and nothing starts

#### Scenario: The refused state on the page
- **WHEN** the live listing route answers a row's request 403, or the page knows it has no key
- **THEN** the row shows "Sign in to the dashboard to list tools, or run `apb connector tools` at a terminal." in place of the server text, and without a key no "List tools" button is offered

#### Scenario: Rows a screen reader tells apart
- **WHEN** the section shows two account rows
- **THEN** each row's button is named for its account and project, and each row's result is a polite live region that is busy while it lists

#### Scenario: Server text on the page
- **WHEN** a tool's description holds markup
- **THEN** the page shows it as text

### Requirement: Contract cases check mcp functions offline
A `tests.yaml` case SHALL accept the expectation kind `mcp` with a `tool` and an `arguments_contain` subset, checked against the dry-run's prepared arguments without starting a server; a case of this kind against a function of another kind SHALL fail.

#### Scenario: A marker preview
- **WHEN** a case checks a marker write of an mcp function
- **THEN** the prepared arguments carry the marker preview and the case passes

#### Scenario: Another kind
- **WHEN** an `mcp` case names an HTTP function
- **THEN** the case fails naming the function

### Requirement: The MCP healthcheck route takes only a signed-in session
The dashboard's healthcheck route SHALL refuse a keyless request and a bearer key for a connector with `mcp:`, before anything starts, as a connector approval is refused; the healthcheck of every other connector SHALL stay as it is. The caller's channel SHALL be decided where the MCP branch is selected, on the one loaded definition that is then executed, before any trust read or secret, and a definition that does not load SHALL fail closed; the live listing SHALL be decided the same way.

#### Scenario: Decided on the executed definition
- **WHEN** the healthcheck or the live listing of an approved, pinned MCP connector is asked from a keyless dashboard, a bearer key, MCP or a run step
- **THEN** it is refused `mcp_needs_a_person` before the account's secret helper runs and nothing starts, while a signed-in session runs it

#### Scenario: A bearer key
- **WHEN** a bearer key asks the healthcheck of an MCP connector
- **THEN** the answer is 403 and no server starts
