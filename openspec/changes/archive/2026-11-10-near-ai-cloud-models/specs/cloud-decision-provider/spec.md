## ADDED Requirements

### Requirement: A decisions provider may name the NEAR catalog
A `decisions.yaml` provider of `kind: llm_emulation` MAY set `catalog: near-ai-cloud`; apb SHALL then default its base URL to `https://cloud-api.near.ai/v1` and its key to `{{env.NEARAI_API_KEY}}`, and SHALL default `structured_output` to `json_schema` when the model's row has `structured_outputs` and `prompt_only` otherwise.

#### Scenario: Minimal provider
- **WHEN** `decisions.yaml` has `{ id: near, kind: llm_emulation, via: openai_compatible, catalog: near-ai-cloud, model: Qwen/Qwen3.8-27B }`
- **THEN** the provider loads with the NEAR base URL and the key reference `NEARAI_API_KEY`

### Requirement: A catalog provider is checked when it loads
Loading `decisions.yaml` SHALL fail, naming the provider, when `catalog` is set on another kind, names an unknown catalog, or names a model the catalog lacks or that is not a chat model.

#### Scenario: The decision model on NEAR
- **WHEN** a provider sets `catalog: near-ai-cloud` and `model: typesafe/jev-1.13`
- **THEN** loading fails naming the provider and "not a chat model"

#### Scenario: Another kind
- **WHEN** a provider of `kind: systemone` sets `catalog: near-ai-cloud`
- **THEN** loading fails with "`catalog` is only for kind llm_emulation"

### Requirement: NEAR decisions are priced and classed
A decision answered by a provider with `catalog` SHALL be priced from the catalog row snapshotted with the run's decisions, with `cost_estimated: true`, and `decision_made` SHALL carry `privacy_class` from that row. `apb decisions` and the doctor's decisions line SHALL show the class next to the provider. The price and class SHALL come from the row recorded with the run's decisions, and the doctor's free check of such a provider SHALL send no key.

#### Scenario: A completion check on NEAR
- **WHEN** a completion check is answered by the provider above with 2,000 input tokens and 50 output tokens
- **THEN** `decision_made.cost_usd` is 0.0001475 and `decision_made.privacy_class` is `confidential_tee`

#### Scenario: The doctor's check
- **WHEN** `NEARAI_API_KEY` is set and `apb doctor` checks that provider
- **THEN** its request to the NEAR listing carries no `Authorization` header
