## ADDED Requirements

### Requirement: A dsh step on a NEAR model needs no hand-written provider config
A dsh executor whose model is `nearai/<id>[@<effort>]` SHALL run on NEAR AI Cloud with only `NEARAI_API_KEY` in the environment. apb SHALL write, in the run-scoped patch it already passes to dsh, an `llm-pi-ai` route `nearai` with `api: openai-completions`, base URL `https://cloud-api.near.ai/v1`, `apiKeyEnv: NEARAI_API_KEY` and the one model with its context length, output cap and efforts, taken from the catalog row the run snapshotted at start.

#### Scenario: The patch composes
- **WHEN** a step runs on `nearai/Qwen/Qwen3.8-27B@high`
- **THEN** the patch file holds the `agent-default-model` row with provider `nearai` and the `llm-pi-ai` row with that route
- **AND** `dsh --profile headless --patch <file> --dump-config` shows both rows

#### Scenario: A catalog refresh during a run
- **WHEN** the catalog is refreshed while a run on a NEAR model is paused
- **AND** the run is resumed
- **THEN** its steps use the catalog row recorded in its manifest

### Requirement: The key is a name everywhere apb writes
apb SHALL write only the variable name `NEARAI_API_KEY` into patches, manifests, journals, caches and logs. The value SHALL reach the agent only through the spawn environment and SHALL be kept when the profile's `process_env` is an allowlist. apb SHALL replace the value in the agent's reply, failure text, stream log, attempt transcript and status file before it stores or reads any of them, and SHALL also redact it on read while it is set.

#### Scenario: Allowlist mode
- **WHEN** a profile with `process_env: { policy: allowlist }` runs a step on `nearai/Qwen/Qwen3.8-27B`
- **THEN** the agent process sees `NEARAI_API_KEY`

#### Scenario: An echoed key
- **WHEN** an agent's reply contains the value of `NEARAI_API_KEY`
- **THEN** `run_status`, `apb runs <id>` and `GET /api/runs/{id}` show it redacted
- **AND** no file under the run directory holds the value

#### Scenario: A key in the status file
- **WHEN** an agent writes the value of `NEARAI_API_KEY` into its status file's outputs, findings or warnings
- **THEN** the node output and the journal hold the redaction marker, and no file under the run directory holds the value

#### Scenario: A rotated key
- **WHEN** that run is read after `NEARAI_API_KEY` was changed to another value
- **THEN** its stored reply still shows the redaction marker, not the old value

### Requirement: The key is replaced as the agent prints
apb SHALL replace the value in the stream log and transcript copies as the bytes are written, including a value split across two reads. An executor whose manifest entry has no catalog snapshot SHALL be scrubbed and redacted through the provider its model route names.

#### Scenario: A driver killed mid-attempt
- **WHEN** an agent prints the value of `NEARAI_API_KEY` and the driver is killed before the attempt returns
- **THEN** the stream log and transcript copies already written hold the redaction marker, not the value

#### Scenario: A manifest without a snapshot
- **WHEN** a run whose manifest names `nearai/<id>` without a `cloud` field is read while `NEARAI_API_KEY` is set
- **THEN** the value is redacted in what the run surfaces return

### Requirement: Models that cannot run an agent step are blocking findings
A profile model on a cloud route whose catalog row has no tool calling SHALL be the blocking finding `cloud_model_no_tools`; one that is not a chat model (text in and text out) SHALL be `cloud_model_not_chat`; an effort the row does not offer SHALL be `cloud_effort_unsupported`. `profile_write` SHALL refuse such a profile and `apb validate` SHALL print an error for it.

#### Scenario: An effort on a non-reasoning model
- **WHEN** `profile_write` gets `nearai/deepseek/deepseek-v3.2@high`
- **THEN** it is refused `cloud_effort_unsupported`

### Requirement: Such models are refused at start and rebind, and flagged on the node
A node bound to a chain containing such a model SHALL get V220 (warning), and a run start in cli execution mode and a mid-run profile rebind SHALL be refused with the finding's code.

#### Scenario: The vision model
- **WHEN** a node is bound to a profile whose executor is `nearai/Qwen/Qwen3-VL-30B-A3B-Instruct`
- **THEN** `apb validate` reports V220 for the node naming the model and "no tool calling"
- **AND** an error `cloud_model_no_tools` on the profile line

#### Scenario: A rebind to the vision model
- **WHEN** a supervisor rebinds a running node to a profile on `nearai/Qwen/Qwen3-VL-30B-A3B-Instruct`
- **THEN** the rebind is refused `cloud_model_no_tools`


### Requirement: A run start checks the catalog and the key
A run start in cli execution mode SHALL be refused, before any run directory is written, when its profiles name a cloud model the snapshot lacks (`cloud_model_unknown`, naming `apb models --refresh`) or when the primary executor's key variable does not reach the spawn (`cloud_key_missing`, naming the variable). A fallback or tier whose key is missing SHALL be a start warning.

#### Scenario: No key
- **WHEN** `NEARAI_API_KEY` is unset and a playbook whose node runs on `nearai/Qwen/Qwen3.8-27B` is started
- **THEN** the start is refused `cloud_key_missing` naming `NEARAI_API_KEY`
- **AND** no run directory is created

#### Scenario: A run an older apb started
- **WHEN** a run whose manifest has no `cloud` field and whose model is `nearai/<id>` is resumed
- **THEN** its steps get the model row alone, as before this change

### Requirement: The start check covers every executor a run binds
The cloud start check SHALL judge the background supervisor's profile when an agent supervises the run, and SHALL judge a node's ephemeral executor override in place of its profile's chain. A key variable an installed connector references SHALL count as not reaching the spawn, since every agent spawn drops it. The wake-up probe of a quota pause SHALL run on the route from the run's catalog snapshot.

#### Scenario: A supervisor on NEAR without a key
- **WHEN** `NEARAI_API_KEY` is unset and an agent-supervised start's supervisor profile runs on `nearai/Qwen/Qwen3.8-27B`
- **THEN** the start is refused `cloud_key_missing` naming the supervisor profile

#### Scenario: An ephemeral override
- **WHEN** a start overrides a node's executor with `dsh` on `nearai/does-not-exist`
- **THEN** the start is refused `cloud_model_unknown`

#### Scenario: A key a connector references
- **WHEN** an installed connector's account references `NEARAI_API_KEY` and the primary executor runs on a NEAR model
- **THEN** the start is refused `cloud_key_missing`

#### Scenario: A quota probe
- **WHEN** a NEAR run paused for quota is woken up
- **THEN** the probe runs on the provider route from the run's catalog snapshot
