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
apb SHALL write only the variable name `NEARAI_API_KEY` into patches, manifests, journals, caches and logs. The value SHALL reach the agent only through the spawn environment and SHALL be kept when the profile's `process_env` is an allowlist. apb SHALL replace the value in the agent's reply, failure text, stream log and attempt transcript before it stores any of them, and SHALL also redact it on read while it is set.

#### Scenario: Allowlist mode
- **WHEN** a profile with `process_env: { policy: allowlist }` runs a step on `nearai/Qwen/Qwen3.8-27B`
- **THEN** the agent process sees `NEARAI_API_KEY`

#### Scenario: An echoed key
- **WHEN** an agent's reply contains the value of `NEARAI_API_KEY`
- **THEN** `run_status`, `apb runs <id>` and `GET /api/runs/{id}` show it redacted
- **AND** no file under the run directory holds the value

#### Scenario: A rotated key
- **WHEN** that run is read after `NEARAI_API_KEY` was changed to another value
- **THEN** its stored reply still shows the redaction marker, not the old value

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
The run gate SHALL refuse a start whose profiles name a cloud model the snapshot lacks (`cloud_model_unknown`, naming `apb models --refresh`) or whose primary executor's key variable does not reach the spawn (`cloud_key_missing`, naming the variable). A fallback or tier whose key is missing SHALL be a start warning.

#### Scenario: No key
- **WHEN** `NEARAI_API_KEY` is unset and a playbook whose node runs on `nearai/Qwen/Qwen3.8-27B` is started
- **THEN** the start is refused `cloud_key_missing` naming `NEARAI_API_KEY`
- **AND** no run directory is created

#### Scenario: A run an older apb started
- **WHEN** a run whose manifest has no `cloud` field and whose model is `nearai/<id>` is resumed
- **THEN** its steps get the model row alone, as before this change
