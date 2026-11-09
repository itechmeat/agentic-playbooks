# dsh-agent Specification

## Purpose
TBD - created by archiving change dsh-agent. Update Purpose after archive.

## Requirements

### Requirement: dsh is a built-in agent
apb SHALL know `dsh` as a built-in agent: found as `dsh` on PATH, version from `dsh --version`, help from `dsh --profile headless --help`, an aggregator, verified against 0.2.2-rc.1.

#### Scenario: Detection
- **WHEN** `dsh` is on PATH
- **THEN** `apb detect` lists `dsh` as installed with its version and the doctor compares it with the verified version

### Requirement: The launch form is headless with the prompt on stdin
The built-in launch form SHALL be `--profile headless --json --permission-mode read-only --approval-policy never -` with the prompt on stdin and the SOUL as a prompt prefix; a run that grants autonomy SHALL append `--permission-mode danger-full-access`.

#### Scenario: A dash-led prompt
- **WHEN** a node's prompt starts with `-`
- **THEN** it reaches dsh on stdin and never as an argument

### Requirement: The model travels in a run-scoped patch
A non-empty model SHALL be written as an `agent-default-model` row of a dsh patch file in the node's run-scoped agent home (outside a run, a content-addressed file in apb's state directory) and passed as `--patch <file>` in front of the form's arguments. The model string SHALL be `[<route>/]<model>[@<effort>]`, a bare model naming the route `deepseek-official`; a string with characters outside the allowed set SHALL be refused before any spawn.

#### Scenario: A DeepSeek model with an effort
- **WHEN** a node runs on `dsh` with model `deepseek-v4-pro@high`
- **THEN** the patch names provider `deepseek-official`, model `deepseek-v4-pro`, reasoning effort `high`, and the argv starts with `--patch`

#### Scenario: No model
- **WHEN** the model is empty
- **THEN** no patch is passed and the dsh profile's own default applies

### Requirement: The event stream is read as machine output
The reader SHALL take the reply from the `final` event, the session id from `sessionId`, the token usage as the sum of every `step_end` usage (`inputTokens`, `outputTokens`, `cacheReadTokens`, `cacheWriteTokens`, as printed, no cost), and SHALL mark the reply failed when the final reason is not `completed` or the stream ends in an `error` event, with the error code put in the words apb's failure classifier reads.

#### Scenario: A rate limit
- **WHEN** the final reason is an error with code `RATE_LIMIT`
- **THEN** the failure classifies as a quota failure

#### Scenario: A missing session
- **WHEN** a resume names a session dsh does not have
- **THEN** the failure reads as a missing session and the engine starts fresh

### Requirement: Sessions continue
apb SHALL continue a dsh session with `--session-id <id> -- <prompt>` after the form's own arguments.

#### Scenario: Resume
- **WHEN** an interactive node's answer round resumes a dsh attempt
- **THEN** the argv carries `--session-id <id>` before `--` and the prompt last

### Requirement: apb setup registers apb's MCP server in dsh
`apb setup` SHALL register apb's MCP server as one `insert` patch of a `@deepseek-ai/dsh-mcp-client` row appended to `$DSH_HOME/cordis.patch.yml` (default `~/.dsh`), and SHALL remove exactly those bytes again.

#### Scenario: Round trip
- **WHEN** apb's server is registered and then removed
- **THEN** the file is byte for byte what it was before
