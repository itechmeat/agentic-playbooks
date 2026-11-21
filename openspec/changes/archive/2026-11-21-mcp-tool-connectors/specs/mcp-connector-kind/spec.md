## ADDED Requirements

### Requirement: A connector can map the tools of an MCP server
A connector manifest SHALL accept a connector-level `mcp:` block naming the transport `stdio` (with a literal `command`, `args` and `env`) and a `protocol` of `auto`, `modern` or `legacy`, and a function-level `mcp: { tool }` naming one tool. A transport this apb does not support SHALL fail to load with "mcp transport `<t>` is not supported by this apb". A connector with an `mcp:` block SHALL hold only mcp and mock functions and SHALL NOT set `auth`, `webhook` or `error_when`.

#### Scenario: A stdio connector loads
- **WHEN** a manifest declares `mcp: { transport: stdio, command: tracker-mcp-server }` and a function `mcp: { tool: search_issues }`
- **THEN** it loads and the function is an mcp function

#### Scenario: An unsupported transport is refused
- **WHEN** a manifest declares `mcp: { transport: http }` and this apb has no HTTP transport
- **THEN** the manifest fails to load with the message that the transport is not supported

#### Scenario: An mcp function with an HTTP field is refused
- **WHEN** an mcp function also sets `headers`
- **THEN** the manifest fails to load naming `headers`

#### Scenario: A command with a relative path is refused
- **WHEN** the stdio `command` is `./server`
- **THEN** the manifest fails to load

#### Scenario: A timeout the discover probe always outlasts
- **WHEN** a connector with `protocol: auto` has an mcp function with `timeout_sec: 10`
- **THEN** the manifest fails to load with a message starting `mcp_timeout_too_small:`

#### Scenario: A short timeout without the probe
- **WHEN** a connector with `protocol: modern` has an mcp function with `timeout_sec: 2`
- **THEN** it loads

### Requirement: The author declares every effect
An mcp function's `read_only`, `irreversible` and `idempotency` SHALL come from the manifest only. Tool annotations from the server SHALL be shown to a person and SHALL NOT change what a call may do. A function that is not `read_only` SHALL declare `idempotency`.

#### Scenario: A write without idempotency is refused
- **WHEN** an mcp function is not `read_only` and declares no `idempotency`
- **THEN** the manifest fails to load

#### Scenario: A read-only annotation does not make a write read-only
- **WHEN** the server annotates a tool `readOnlyHint: true` and its function is not `read_only`
- **THEN** a call through it is tracked by the send ledger

### Requirement: Secrets reach only the server's environment
`{{secret.*}}` SHALL be allowed in stdio `env` values only; `command` SHALL take no placeholder, `args` SHALL take `{{account.*}}` only, and `{{args.*}}` SHALL be refused in the whole `mcp:` block.

#### Scenario: A secret in args is refused
- **WHEN** a stdio `args` entry holds `{{secret.token}}`
- **THEN** the manifest fails to load

### Requirement: The agent never calls the server
An mcp function SHALL be called only through `apb connector call`. No agent configuration apb writes SHALL name the server, and the agent prompt SHALL show the manifest's description and arguments, never the server's. In the minimal agent environment the agent SHALL be started with only apb's own MCP servers; under `environment: full` the agent loads the person's own MCP configuration as before, to which apb adds nothing.

#### Scenario: A grant adds no MCP server to the agent
- **WHEN** a node whose profile uses the minimal environment grants an MCP connector
- **THEN** the agent is started with only apb's own MCP servers

### Requirement: MCP answers are untrusted output
Every mcp function SHALL count as `untrusted_output`, so V187 and the prompt's untrusted paragraph apply.

#### Scenario: An unmarked MCP grant draws V187
- **WHEN** a node grants an mcp function without `untrusted: true`
- **THEN** validation warns V187

### Requirement: Binding rules for MCP connectors
Validation SHALL warn V300 when a binding of an MCP connector has no `functions` allowlist, SHALL fail V302 when an MCP connector is bound at an engine site (the baseline check, the follow-up check or a list item check), and V113 SHALL keep refusing any connector on an isolated node, naming the MCP reason when the connector is an MCP connector.

#### Scenario: A binding without a list
- **WHEN** a node binds an MCP connector with no `functions`
- **THEN** validation warns V300

#### Scenario: An MCP read in a baseline check
- **WHEN** a playbook's baseline check reads a function of an MCP connector
- **THEN** validation fails V302

#### Scenario: An isolated node with an MCP connector
- **WHEN** a node with `isolation: full` binds an MCP connector
- **THEN** validation fails V113 and the message names the MCP server

### Requirement: Interpreters and launchers draw V301
Validation SHALL warn V301 when a binding's MCP connector has a stdio command that is a known interpreter or launcher (such as `node`, `python3`, `sh`, `npx`, `uvx` or `docker`, a versioned interpreter name such as `python3.12` included), saying that its approval covers that program file only, not the script or package it runs.

#### Scenario: A launcher command
- **WHEN** a node binds an MCP connector whose `command` is `npx`
- **THEN** validation warns V301 and says the approval covers the `npx` program file only, not the script or package it runs

#### Scenario: An interpreter command
- **WHEN** a node binds an MCP connector whose `command` is `/usr/bin/node`
- **THEN** validation warns V301 naming `node` as an interpreter or a launcher

### Requirement: The transport block holds no control character
`mcp.command`, every `mcp.args` entry and every `mcp.env` name and value template SHALL hold no control character, and a manifest with one SHALL fail to load naming the field.

#### Scenario: An escape sequence in args
- **WHEN** an `mcp.args` entry holds an escape sequence
- **THEN** the manifest fails to load with a message naming `args[0]` and no character

#### Scenario: An escape sequence in the command or a tool name
- **WHEN** `mcp.command` or an mcp function's tool name holds an escape sequence
- **THEN** the install error holds no control character

### Requirement: A definition cannot change what code the server loads
An `mcp.env` name that changes what code a process loads SHALL fail to load with a message starting `mcp_env_loads_code:`. The names SHALL be one list: the dynamic loader's (every name starting `LD_` or `DYLD_`) and the start-up and module path variables of the interpreters the list names.

#### Scenario: The interpreters' variables
- **WHEN** a connector's `mcp.env` declares any of `NODE_OPTIONS`, `NODE_PATH`, `PYTHONPATH`, `PYTHONSTARTUP`, `PYTHONHOME`, `PYTHONUSERBASE`, `PYTHONINSPECT`, `RUBYOPT`, `RUBYLIB`, `GEM_HOME`, `GEM_PATH`, `PERL5OPT`, `PERL5LIB`, `PERLLIB`, `PERL5DB`, `BASH_ENV`, `ENV`, `ZDOTDIR`, `CLASSPATH`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`, `JDK_JAVA_OPTIONS`, `GCONV_PATH` or `LUA_PATH`
- **THEN** the manifest fails to load with a message starting `mcp_env_loads_code:` that names it

#### Scenario: A preload variable
- **WHEN** a connector's `mcp.env` declares `LD_PRELOAD` or `NODE_OPTIONS`
- **THEN** the manifest fails to load with a message starting `mcp_env_loads_code:` that names the variable

#### Scenario: An ordinary variable
- **WHEN** a connector's `mcp.env` declares `NODE_ENV` or `HTTPS_PROXY`
- **THEN** it loads
