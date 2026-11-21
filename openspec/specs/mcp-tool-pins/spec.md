# mcp-tool-pins Specification

## Purpose
The program and every tool definition of an MCP server are pinned by digest when a person approves them, and every call checks those pins before any secret is resolved and again after the server's listing, so a changed program, definition or account is refused until a person approves it again.

## Requirements

### Requirement: Approving an MCP connector is a person's act at a terminal
For an MCP connector, `apb connector approve`, with or without `--account`, SHALL record anything only after a person typed the connector name at an interactive terminal for each confirmation. Without a terminal, inside a run step, over MCP and from the dashboard it SHALL be refused `mcp_approve_needs_a_person` before anything is resolved or started, and the refusal SHALL print no digest.

#### Scenario: No terminal
- **WHEN** the approval of an MCP connector runs without a terminal, with or without `--account`
- **THEN** it exits 2 with `mcp_approve_needs_a_person`, prints no digest, runs no secret helper, starts no program and approves and pins nothing

#### Scenario: Refused in a run step
- **WHEN** the approval runs with `APB_RUN_ID` set
- **THEN** it exits 2 and nothing is approved or pinned

#### Scenario: Two typed confirmations
- **WHEN** a person at a terminal types the connector name at the program confirmation and again at the tool confirmation
- **THEN** the program is started once, the tools are pinned and the account is approved

#### Scenario: A declined program confirmation
- **WHEN** the person types anything else at the program confirmation
- **THEN** no secret helper runs, no program starts and nothing is approved or pinned

#### Scenario: A listing whose server was not shown ended
- **WHEN** the server started for the approval's listing could not be confirmed reaped, or its process group was not shown empty
- **THEN** the tool confirmation says in plain words, before its question, that a process started for this listing may still be running with the account's secrets

### Requirement: The approval states its limits
The documentation SHALL state, next to "approved at a terminal", that the pin store is an unsigned file a process of the same user can write, so an approval is no protection against a run step of the same user with a shell, and that the terminal check is `isatty` plus `/dev/tty`, which an agent with a pseudo-terminal that clears `APB_RUN_ID` passes, as it passes every typed confirmation.

#### Scenario: The connector documentation
- **WHEN** a person reads the MCP section of the connector documentation
- **THEN** it names both limits next to the approval at a terminal

### Requirement: Other connector kinds approve as before
Approving a connector without an `mcp:` block SHALL behave as on main.

#### Scenario: Another kind is unchanged without a terminal
- **WHEN** an HTTP connector's account is approved without a terminal
- **THEN** it is approved as before

### Requirement: The connector path pins nothing and no route approves
Approving an MCP connector's tree SHALL write no pin and SHALL NOT make an existing pin usable. No MCP tool and no HTTP route SHALL approve an MCP connector or write a pin; the dashboard's approve route SHALL refuse one.

#### Scenario: The connector path at a terminal
- **WHEN** a person approves an MCP connector without `--account` at a terminal and types its name
- **THEN** the tree digest is approved and no pin record changes

#### Scenario: The dashboard refuses
- **WHEN** `POST /api/connectors/approve` names an MCP connector, with or without an account
- **THEN** it answers 403 and nothing is approved or pinned

#### Scenario: Another kind is unchanged
- **WHEN** an HTTP connector's account is approved without a terminal
- **THEN** it is approved as before

### Requirement: The approval pins the program and the mapped tools
The approval of an MCP connector's account SHALL resolve the stdio command once to an absolute canonical path, start that program, and record one pin record in `<config-dir>/mcp-tools.json`: the digest of the approved connector tree, the digest of the approved account, the program path, the program's file digest, the digest of each mapped tool the server lists, and a pin digest over them, one binding.

#### Scenario: Approving pins the mapped tools
- **WHEN** a person approves an MCP connector's account and confirms
- **THEN** the program and every mapped tool the server lists are pinned and an unmapped tool is not

#### Scenario: A missing tool is reported
- **WHEN** a function maps a tool the server does not list
- **THEN** the approval reports it and pins the rest

### Requirement: A call starts only the pinned program
Every call of an mcp function SHALL open the program path its pin record holds once, without following a link, compare the digest read from that descriptor with the pin, and start that same file, and SHALL be refused `mcp_server_changed` before anything starts when the program file is gone, a link or not a regular file, or its digest differs. A bare command SHALL never be looked up in the calling process's `PATH` at call time.

#### Scenario: A shadow program on the caller's PATH
- **WHEN** a directory earlier on the caller's `PATH` holds a program with the command's name
- **THEN** the call starts the pinned program and the shadow program is not started

#### Scenario: The program file changed
- **WHEN** the pinned program file changed after the approval
- **THEN** the call is refused `mcp_server_changed` and no program starts

#### Scenario: A program swapped while the secrets resolve
- **WHEN** a secret helper renames another script over the program after the pin check and before the start
- **THEN** the swapped-in script never runs: on Linux the hashed program runs, elsewhere the call is refused `mcp_server_changed`

#### Scenario: A program turned into a link
- **WHEN** the pinned program path is replaced by a symbolic link to identical bytes
- **THEN** the call is refused `mcp_server_changed`

### Requirement: Every start opens the program once
The approval's listing and the pin-check healthcheck SHALL open, compare and start the program file the same way a call does: one open, the digest read from that descriptor, and that file started.

#### Scenario: The approval's listing
- **WHEN** the program file at the confirmed path no longer has the confirmed digest when the listing opens it
- **THEN** the approval is refused `mcp_server_changed` and no program starts

### Requirement: A pin is one binding with the connector tree
A pin record SHALL bind the approved connector tree digest, which covers `mcp.command`, `mcp.args`, `mcp.env` and every other field, with the program and the tools, and every check SHALL compare that whole binding in one place. A call under another tree digest SHALL be refused `mcp_server_changed` before anything starts.

#### Scenario: Args changed and the tree approved again
- **WHEN** a pinned connector's `mcp.args` change and a person approves the new tree digest without `--account`
- **THEN** a call is refused `mcp_server_changed`, a start is refused `mcp_tools_unpinned`, and only the account approval under the new tree lets them through

### Requirement: The digest covers what the person saw
The tool digest SHALL be the SHA-256 of a domain tag and the canonical JSON (keys sorted) of the whole tool definition exactly as the server's `tools/list` answer carried it on the wire, never a typed decoding of it, with only `icons` and `_meta` removed, so a member a later revision adds is pinned too.

#### Scenario: Key order does not matter
- **WHEN** a server lists the same tool with its keys in another order
- **THEN** the digest is the same

#### Scenario: A member no list names
- **WHEN** a server changes only the `execution` member of a mapped tool's definition
- **THEN** the digest changes and the next call is refused `mcp_tool_changed` until a person approves again

### Requirement: Every call checks its tool
Every call of an mcp function SHALL list the server's tools and SHALL be refused before anything is sent when the mapped tool is missing (`mcp_tool_missing`), unpinned (`mcp_tool_unpinned`) or changed (`mcp_tool_changed`). The check SHALL run on every call, never once per run.

#### Scenario: A description changed between two calls
- **WHEN** the server changes a tool's description after a first call of an attempt
- **THEN** the second call is refused `mcp_tool_changed` and the server receives no second `tools/call`

#### Scenario: Re-approval lets the call through
- **WHEN** a person approves the changed tool
- **THEN** the next call goes out

#### Scenario: Refusals name no command
- **WHEN** a call is refused for an unpinned or changed program or tool
- **THEN** the refusal text names no command line an agent could run to clear it

#### Scenario: A run step with a redirected config dir
- **WHEN** a call is made from a run step whose config dir is redirected away from the default one
- **THEN** it is refused `mcp_config_dir_redirected` before any program starts

### Requirement: Reflected secrets never reach the pin file or the approve output
A listing whose raw tool definitions hold any resolved secret value of the account, in any member name or value, SHALL be refused `mcp_tools_reflect_a_secret` before anything is hashed, displayed or stored. Text the output shows from elsewhere SHALL still pass the account's redactions.

#### Scenario: A description reflects the token
- **WHEN** the server lists a tool whose description, input schema member name or tool name holds the account's token
- **THEN** the approval exits 2 with `mcp_tools_reflect_a_secret`, `mcp-tools.json` is unchanged and holds no digest of that definition, and the output holds neither the token nor a tool name

### Requirement: The run gate refuses unpinned tools
A run start whose granted mcp functions have no pin for a granted account, or whose record binds another connector tree, SHALL be refused `mcp_tools_unpinned` with a manual next action that names no command. The gate SHALL NOT start a server.

#### Scenario: A start before approval
- **WHEN** a playbook grants an mcp function whose tool was never pinned
- **THEN** the start is refused `mcp_tools_unpinned` and no server process was started

### Requirement: An unreadable pin store has its own refusal
When the pin store does not read, the run gate's `mcp_tools_unpinned` refusal SHALL carry an empty `tools` list and the store's path as `store_unreadable`, its next action SHALL say to move the store aside and have a person approve the MCP tools again at a terminal, and the fit report SHALL show one row saying the store does not read.

#### Scenario: A pin store that does not read
- **WHEN** a playbook grants an mcp function and `mcp-tools.json` does not parse
- **THEN** the start is refused `mcp_tools_unpinned` with `store_unreadable` naming the file and a next action to move it aside, not one per tool

### Requirement: The pin store is its own file
Pins SHALL live in `mcp-tools.json`, never in `trust.json`. The config dir SHALL be resolved once at its root (a config dir that is a link is followed there), and the store SHALL be read without following a link below it, written under its lock or not at all, and a store that does not read or parse SHALL fail calls with its path rather than read as empty.

#### Scenario: A corrupt store
- **WHEN** `mcp-tools.json` does not parse
- **THEN** a call fails naming the file and the store is not replaced

#### Scenario: A linked store
- **WHEN** `mcp-tools.json` is a symbolic link to a valid store
- **THEN** a call fails naming the file

#### Scenario: A linked config dir
- **WHEN** the config dir itself is a symbolic link to a directory holding a valid store
- **THEN** the store is read

### Requirement: Pins go with the connector and its approval
Uninstalling a connector SHALL remove every pin record of it, and revoking the approval of a connector or of a connector account SHALL remove the pin records of that connector or that account. Both SHALL be best effort: a store that does not read keeps its records. Removing nothing SHALL write nothing.

#### Scenario: A revoked account approval
- **WHEN** the approval of account `a` of a connector is revoked
- **THEN** the pin record of that account is gone and another account's record stays

#### Scenario: An uninstalled connector
- **WHEN** a connector with pin records is uninstalled
- **THEN** its records are gone and the records of other connectors stay

#### Scenario: Nothing to forget
- **WHEN** a connector without pin records is uninstalled and no pin store exists
- **THEN** no pin store file is written

### Requirement: The program confirmation shows what the program is started with
The first confirmation of an account approval SHALL show, before any secret is resolved or anything starts, the program path and file digest, the rendered non-secret arguments, every environment override with its rendered value, the account's non-secret fields and the command line of each command-sourced secret (extracted, not run), all sanitized, with secret values masked and their source named.

#### Scenario: An account field feeding an override
- **WHEN** `mcp.env` sets `HTTPS_PROXY` and `PATH` from account fields and `TOKEN` from a secret
- **THEN** the confirmation shows both fields' values in the overrides and `TOKEN` masked with its source, before any secret helper runs

#### Scenario: A command-sourced secret
- **WHEN** the token's field is a `{{cmd:...}}` reference
- **THEN** the confirmation shows that command line, sanitized, and the command has not run

### Requirement: The program confirmation states what the approval covers
The program confirmation SHALL warn, when the command is an interpreter or a launcher, that the approval covers that program file only, not the script or package it runs, and SHALL say once that the program runs as the person, unsandboxed, and can read and change what the person can, including apb's own files and the secrets of other connectors.

#### Scenario: An interpreter command
- **WHEN** the connector's command is `node`
- **THEN** the program confirmation warns that `node` is an interpreter or a launcher and says once that the program runs as the person, unsandboxed

### Requirement: Server text cannot scroll the digests away
The tool confirmation SHALL bound every field of server text (a row's description 80 characters, a changed description 300, changed members 300, a title 200, an input schema 2000, annotations 500, unmapped names 1000, the protocol version 64) and all server-text lines together to 20 000 characters, leaving the rest out with one line that counts the lines left out, and SHALL repeat right above the question the program digest line, the count of tools in each status and the pin digest.

#### Scenario: Hundreds of kilobytes of annotations
- **WHEN** a server lists a tool with 300 000 characters of annotations and 400 more tools
- **THEN** the server text shown stays within its bound and the program digest, the status counts and the pin digest are the last lines before the question

### Requirement: A pin binds the account it was approved for
The pin record and the pin digest SHALL include the digest of the approved account, and the single binding comparison SHALL refuse a call or a start under an account of the same name whose digest differs.

#### Scenario: Two projects with an account of the same name
- **WHEN** account `work` is pinned in one project root and another project root defines its own `work` account with other fields
- **THEN** a call there is refused `mcp_server_changed` and a start is refused `mcp_tools_unpinned`

### Requirement: Listings and the pin store have limits that compose
A listing SHALL hold at most 20 pages, 500 tools and 2 MiB of raw result bytes, else fail before anything is hashed or stored. A record SHALL be refused `mcp_pin_store_too_large` when the whole serialized store would exceed the 4 MiB read cap, leaving the file as it was.

#### Scenario: Every successful record loads again
- **WHEN** records are added until the next one would pass the cap
- **THEN** every record that succeeded loads again and the refused one leaves the file unchanged

### Requirement: The confirmed account and definition are the ones listed
The listing step of an approval SHALL compare the reloaded tree digest and account digest with the ones the program confirmation showed before it resolves any secret, refusing `mcp_approve_definition_changed` or `mcp_approve_account_changed` and starting nothing on a difference.

#### Scenario: An account edit between the confirmation and the listing
- **WHEN** the account file changes after the program confirmation and before the listing
- **THEN** the approval is refused `mcp_approve_account_changed`, no secret helper runs and no program starts

### Requirement: Pins are checked before secrets are resolved
Every path that can start a secret helper or a server SHALL check the pin store, the binding (tree, account, program path and file digest) and the tool's pin before it resolves any secret.

#### Scenario: A changed pin and a secret helper
- **WHEN** a call's account takes its token from a `{{cmd:...}}` helper and its pin record no longer matches
- **THEN** the call is refused and the helper never runs

### Requirement: Refusals carry no digest
A refusal of a call, a start, a healthcheck or an approval SHALL name what differs (the program file, the tree, the account, a tool by name) and SHALL carry no digest and no digest prefix; full digests SHALL appear only in the terminal confirmations.

#### Scenario: A changed tool
- **WHEN** a call is refused `mcp_tool_changed`
- **THEN** the message names the tool and holds no `sha256:` text and no hexadecimal digest prefix
