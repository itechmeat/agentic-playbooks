# cloud-model-privacy Specification

## Purpose
TBD - created by archiving change near-ai-cloud-models. Update Purpose after archive.

## Requirements

### Requirement: Every catalog model carries a privacy class
Each NEAR AI Cloud row SHALL carry `privacy_class`: `confidential_tee` for owner `nearai`, `third_party_tee` for owner `attested 3p`, `incognito` for the gateway owners `anthropic`, `openai`, `google`, `x-ai`, `deepseek`, `qwen` and `moonshotai`, and `unverified` for any other owner. The class SHALL appear in `GET /api/models` rows and options.

#### Scenario: The four classes
- **WHEN** the embedded catalog is loaded
- **THEN** `Qwen/Qwen3.8-27B` is `confidential_tee`, `z-ai/glm-5.3` is `third_party_tee`, `anthropic/claude-sonnet-5-5` is `incognito` and `typesafe/jev-1.13` is `unverified`

### Requirement: The resolved profile shows the class of every executor
`GET /api/profiles/{name}` and MCP `profile_get` SHALL return a `resolved` block listing the executor, every fallback and every tier with its cloud row or `null`, and a `privacy` summary with the classes in chain order, `uniform` and `least_private` (`null` when any entry is outside a catalog).

#### Scenario: All confidential
- **WHEN** a profile's executor and its two fallbacks are NEAR models owned by `nearai`
- **THEN** `privacy.uniform` is true and `privacy.least_private` is `confidential_tee`

### Requirement: A chain that leaves its class is made visible
A profile whose chain is not uniform and has at least one classified entry SHALL get the warning `profile_privacy_class_changes` in `profile_write`, `apb validate` and its resolved block, and a node bound to it SHALL get V221 (warning) naming the classes in chain order. Each agent attempt on a cloud model SHALL record `attempt_started.cloud` with provider, model and class, and every run surface SHALL mark an attempt whose class is not its profile executor's as `privacy_class_left`.

#### Scenario: An incognito fallback
- **WHEN** a profile runs `nearai/Qwen/Qwen3.8-27B` with the fallback `nearai/anthropic/claude-sonnet-5-5`
- **THEN** `profile_write` answers the warning `profile_privacy_class_changes`
- **AND** `apb validate` reports V221 for each node bound to it

#### Scenario: The fallback ran
- **WHEN** the executor hit a quota and the fallback ran the node
- **THEN** `run_status` shows the attempt as `incognito` with `privacy_class_left: true`
