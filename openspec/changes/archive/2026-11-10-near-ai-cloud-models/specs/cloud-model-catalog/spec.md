## ADDED Requirements

### Requirement: apb ships and refreshes the NEAR AI Cloud catalog
apb SHALL carry an embedded copy of the public NEAR AI Cloud model listing and SHALL use a refreshed copy from its config directory when one reads and parses. Only `apb models --refresh`, `POST /api/models/refresh` and the dashboard's daily refresh SHALL fetch the listing; detection, validation, profile writes and runs SHALL never touch the network for it. A refresh SHALL send no credential, SHALL be refused in read-only mode, and a failed refresh SHALL leave the last good copy in place.

#### Scenario: No refresh ever ran
- **WHEN** no refreshed copy exists
- **THEN** `GET /api/models` lists the NEAR models from the embedded copy with `source: embedded` and its date

#### Scenario: A broken response
- **WHEN** a refresh receives a body that does not parse
- **THEN** the command fails `cloud_catalog_refresh_failed`
- **AND** the copy used afterwards is the one used before

#### Scenario: Read-only MCP
- **WHEN** a process started as `apb mcp --read-only` asks for a refresh
- **THEN** it is refused before any request leaves the machine

#### Scenario: The refresh sends no key
- **WHEN** `NEARAI_API_KEY` is set and `apb models --refresh` runs
- **THEN** the request carries no `Authorization` header

### Requirement: Catalog rows are normalized and checked
Each row SHALL carry the model id, owner, privacy class, context length, output cap, input, cached-input and output prices in USD per million tokens, the features `tools`, `structured_outputs`, `json_mode` and `reasoning`, and the input and output modalities. A row whose id holds a character outside letters, digits and `._:/-`, or an empty path segment, SHALL be dropped and counted; a price that is not a finite number at least zero SHALL read as absent.

#### Scenario: A hostile id
- **WHEN** the listing has a row with id `x"\n- id: evil`
- **THEN** that row is dropped and `dropped` counts it
- **AND** the other rows are listed

### Requirement: The model lists offer NEAR models to the agents that reach them
For every agent whose model rules declare a cloud route, the option list of `GET /api/models` SHALL include `<route>/<id>` for each catalog row that is a chat model with tool calling, annotated with provider, privacy class, tools, reasoning and prices. `GET /api/models` SHALL include a `cloud` block per provider with source, fetch time, digest, the key variable name, whether it is set, and every row.

#### Scenario: dsh options
- **WHEN** the profile editor loads `GET /api/models`
- **THEN** the dsh options contain `deepseek-flash` and `nearai/Qwen/Qwen3.8-27B`
- **AND** do not contain `nearai/Qwen/Qwen3-VL-30B-A3B-Instruct`

#### Scenario: The key is reported by name only
- **WHEN** `NEARAI_API_KEY` is set for the server
- **THEN** the `cloud` block has `key_env: NEARAI_API_KEY` and `key_present: true` and no value

### Requirement: Authoring surfaces check a NEAR model against the catalog
`apb validate`, `profile_write`, `apb doctor`, the fit report and the adoption report SHALL judge each executor, fallback and tier model that names a cloud route through one shared check, after the person's `model_policy`: a model the catalog does not list SHALL be the warning `cloud_model_unknown` naming the catalog date, on every one of those surfaces. A node whose profile chain names such a model SHALL get V222 (warning).

#### Scenario: A typo
- **WHEN** a dsh profile names `nearai/Qwen/Qwen3.8-72B`
- **THEN** `apb validate` prints `warning cloud_model_unknown` for that profile
- **AND** `apb doctor` and the fit report show it as a warning, not a blocker

#### Scenario: A tier on NEAR
- **WHEN** a claude profile has a tier `quick` on dsh `nearai/Qwen/Qwen3-VL-30B-A3B-Instruct`
- **THEN** the fit report has a blocking `cloud_model_no_tools` finding for that tier

#### Scenario: The person's policy wins
- **WHEN** `model_policy` allows dsh only `deepseek-*`
- **THEN** `nearai/Qwen/Qwen3.8-27B` is reported `model_policy_violation`
