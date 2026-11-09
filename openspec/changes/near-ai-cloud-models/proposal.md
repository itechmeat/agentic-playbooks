# Proposal: NEAR AI Cloud models

## Why

NEAR AI Cloud (`https://cloud-api.near.ai/v1`) is an OpenAI-compatible
gateway whose catalog mixes models that run inside a confidential TEE
with provider models it only relays. Its public, unauthenticated
`GET /v1/models` lists every model with its owner, prices per million
tokens, context length, features (`tools`, `structured_outputs`,
`reasoning`, `json_mode`) and modalities. apb knows none of it today:

1. No model list. The curated table (`assets/models.yaml`, read by
   `crates/apb-core/src/models_table.rs` `builtin`, `load_merged`) has no
   NEAR rows, and the one source of agent and model lists
   (`crates/apb-core/src/agent_catalog.rs` `assemble`, served by
   `GET /api/models`, `crates/apb-server/src/routes/meta.rs`
   `list_models_handler`) offers dsh only its closed DeepSeek list
   (`crates/apb-core/src/agents/dsh/mod.rs` `DshModels::closed_list`). A
   profile that names a NEAR model passes `apb validate` and
   `profile_write` unchecked: `DshModels::on_closed_list` accepts any id
   with a `/`.
2. No executor config. dsh reaches a provider only through a route the
   person declared by hand in its `llm-pi-ai` adapter
   (`docs/AGENT-SETUP.md`, "DeepSeek Harness"); opencode and codex need a
   hand-written custom provider. A model that has no tool calling (the
   vision model `Qwen/Qwen3-VL-30B-A3B-Instruct`, the reranker) fails at
   the first request, mid-run.
3. No cost. dsh prints token usage but no cost
   (`crates/apb-core/src/agents/dsh/output.rs`), its cost basis is
   `unknown` (`crates/apb-core/src/agents/cost_basis.rs` `resolve`), so a
   run budget's money limit cannot work and `apb stats` shows no spend.
4. No privacy data. Which models are confidential is a fact of the
   catalog (`owned_by: nearai` is NEAR AI's own TEE fleet, `attested 3p`
   a third party's enclave, provider owners are relayed), but nothing in
   apb carries it, so nobody can check that a profile, with its
   fallbacks and tiers, only ever reaches confidential models.
5. No decision provider. `decisions.yaml` can point an `llm_emulation`
   provider at any OpenAI-compatible base URL
   (`crates/apb-core/src/decisions.rs` `ProviderKind::LlmEmulation`), but
   nothing knows the NEAR model's price or class, and the decision cost
   falls back to a hard-coded price list
   (`crates/apb-engine/src/decision/mod.rs` `list_price_per_million`).

Every decision serves code work and routines outside code alike:

| Decision | Code example | Non-code example |
|---|---|---|
| D1 catalog | the profile editor offers `nearai/Qwen/Qwen3.8-27B` for a dsh code-review profile, priced and marked confidential | a mail-triage profile picks `nearai/z-ai/glm-5.3-flash`; `apb validate` knows the id |
| D2 execution | a refactoring node on dsh runs on NEAR with only `NEARAI_API_KEY` set; no dsh config written by hand | an invoice-extraction node bound to the vision model is refused at validation: it has no tool calling |
| D3 cost | a code run's budget of 2 USD stops before the next node would pass it | a daily digest's `apb stats` shows its spend per day |
| D4 privacy | a security-review profile whose fallback is `anthropic/claude-sonnet-5-5` on NEAR gets a warning: the fallback leaves the confidential class | a profile reading customer contracts is shown as `confidential_tee` end to end |
| D5 decisions | a completion check asks a NEAR model, priced and classed in `decision_made` | an intake routing decision on mail runs on a confidential NEAR model |

## What changes

1. Cloud model catalog (capability `cloud-model-catalog`): a new module
   `apb_core::cloud_models` owns the knowledge of one cloud provider per
   submodule, the first being NEAR AI Cloud (`near_ai`: provider id
   `near-ai-cloud`, base URL `https://cloud-api.near.ai/v1`, key variable
   `NEARAI_API_KEY`). Its catalog is the parsed `/v1/models` body: an
   embedded snapshot shipped in the binary, overridden by a cache that
   `apb models refresh`, `POST /api/models/refresh` and the dashboard's
   daily refresh write; nothing else touches the network, and detection
   stays free. `GET /api/models` gains a `cloud` block, and the dsh
   options list every NEAR chat model with tool calling as
   `nearai/<id>`. `apb validate`, `profile_write`, the doctor, fit and
   adoption check a NEAR model against the catalog (shared
   `model_check::check`).
2. Execution on dsh (capability `cloud-model-execution`): a dsh profile
   naming `nearai/<id>[@<effort>]` runs with no hand-written config. The
   run snapshots the model's catalog row into the manifest; dsh's
   run-scoped patch (decision record 0013) then carries, next to the
   `agent-default-model` row, an `llm-pi-ai` row with one route `nearai`
   (`api: openai-completions`, the base URL, `apiKeyEnv:
   NEARAI_API_KEY`, the one model with its context, output cap and
   efforts). Only the variable's name is ever written; its value reaches
   dsh through the spawn environment, is kept in allowlist mode, and is
   replaced in the agent's text before apb stores it. A model without
   tool calling or not a chat model is a blocking profile finding, a
   start and rebind refusal in cli mode, and V220 on the node; a missing
   key refuses the start.
3. Cost (capability `cloud-model-cost`): an attempt on a catalog model
   whose agent printed no cost is priced from the run's snapshot of the
   catalog prices (input, cached input, output) and stamped
   `priced_from`; its basis is `billed` when the key reaches the
   process. Budgets, estimates, `apb stats` and the run surfaces read it
   unchanged, since all of them read `usage.cost_usd`.
4. Privacy class (capability `cloud-model-privacy`): every catalog row
   carries `privacy_class` (`confidential_tee`, `third_party_tee`,
   `incognito`, `unverified`), exposed in `/api/models`, in the resolved
   profile (`/api/profiles/{name}`, MCP `profile_get`) for the executor,
   every fallback and every tier, and on each attempt
   (`attempt_started.cloud`). A chain that spans classes gets the
   profile warning `profile_privacy_class_changes` and the node warning
   V221; an attempt that ran on a class other than its executor's is
   shown as `privacy_class_left` on every run surface.
5. Decision provider (capability `cloud-decision-provider`): an
   `llm_emulation` provider in `decisions.yaml` may name `catalog:
   near-ai-cloud`; apb fills the base URL and the key reference
   (`{{env.NEARAI_API_KEY}}`), checks the model against the catalog,
   picks the structured-output mode from its features, prices each
   decision from the catalog and records the class in
   `decision_made.privacy_class`.

## Later phase (not in this change)

1. Run-level attestation evidence: for each NEAR TEE model a run used,
   fetch `GET /v1/attestation/report?model=<id>` once and keep its digest
   and fetch time as run evidence for the report page. Deferred because a
   report fetched at start or end proves nothing about a particular
   response, and per-response signatures need the exact request and
   response bytes, which dsh does not hand apb. `design.md` D7 records
   the shape a follow-up change should take; nothing here precludes it.

## Out of scope

1. OpenCode and codex as NEAR executors. dsh already has the run-scoped
   patch path (decision record 0013); opencode has no run-scoped config
   mechanism in apb yet (`crates/apb-core/src/agents/opencode/mod.rs`
   declares no `scoped_env`). A later change can add one behind the same
   `cloud_models` data.
2. End-to-end encryption, response signatures, the privacy endpoints
   (`/v1/privacy/*`), speech, images and embeddings.
3. A privacy requirement a playbook or profile declares and the gate
   enforces (for example "confidential only"). This change gives the
   data and the warnings; a policy on top of it is a separate decision.
4. A price-based estimate for a playbook with no earlier run. Estimates
   stay history-based (`crates/apb-engine/src/run_estimate/history.rs`);
   from the first priced run on they have figures.
5. Other cloud catalogs. The module is shaped for more than one
   provider, but only NEAR AI Cloud is added.

## Impact

1. Crates: `apb-core` (new `cloud_models/` with `mod.rs`, `near_ai.rs`,
   `catalog.rs`, `check.rs`; `agents/policy.rs` `ModelPolicy::cloud_routes`;
   `agents/setup.rs` `HostSetup::scoped_args` gains the cloud row; dsh
   `mod.rs` and `model.rs`; `agents/cost_basis.rs`; `agents/output.rs`
   `AgentUsage.priced_from`; `agent_catalog.rs`; `models_table.rs`
   `ModelOption` fields; `model_check.rs` new issues; `config.rs`
   `GlobalConfig.cloud_models`; `decisions.rs` `ProviderSpec.catalog`;
   `validate/cloud_models.rs` V220 to V222 and `ValidationContext`
   facts; `doctor.rs`), `apb-engine` (`invocation.rs`
   `ResolvedInvocation.cloud`; `manifest.rs`; `adapter.rs` pricing next
   to `stamp_cost_basis`; `adapter/spawn_env.rs`; `event.rs`
   `AttemptStarted.cloud`, `DecisionMade.privacy_class`; `run_view.rs`;
   `run_redact.rs`; `gate/` start refusals; `decision/mod.rs` and
   `decision/providers`), `apb-mcp` (`profile_tools.rs` resolved block
   and findings), `apb` CLI (`apb models`, `apb validate`, `apb doctor`),
   `apb-server` (`/api/models` cloud block, `POST /api/models/refresh`,
   the daily refresh task, ts contract), `web` (profile editor options,
   class badges, resolved chain panel).
2. New embedded data: `assets/cloud-models/near-ai-cloud.json`, the
   public `/v1/models` body.
3. New machine state: `<config_dir>/state/cloud-models/near-ai-cloud.json`
   (the refreshed body and its fetch time). Nothing new under `.apb/`.
4. Journal: additive fields `attempt_started.cloud`,
   `attempt_finished.usage.priced_from`, `decision_made.privacy_class`;
   no new event type.
5. Validator codes V220 to V222 (V223 to V229 reserved for this change).
   V187 to V199 belong to google-connectors, V200 to V209 to
   routine-operations, V210 to V219 to routine-health.
6. Behaviour changes for release notes:
   1. `config.yaml` accepts the section `cloud_models:`. An older apb
      refuses a config with it (`GlobalConfig` denies unknown fields).
   2. A dsh step on `nearai/<id>` runs on NEAR AI Cloud with
      `NEARAI_API_KEY`; its cost basis is `billed`, so the `billing:`
      section's `billed` policy (default `ask`) decides it.
   3. dsh profiles naming a NEAR model without tool calling, not a chat
      model, or unknown to the catalog now get findings; a run of one is
      refused at start. apb now owns the dsh route name `nearai`: a
      person's own `nearai` route in their dsh layers is replaced in a
      spawn on it, and a model it served that the catalog lacks is
      refused until the catalog lists it.
   4. `apb dashboard` fetches the public NEAR catalog once a day unless
      `cloud_models.near_ai_cloud.auto_refresh: false`.
   5. A NEAR key value an agent prints is replaced before apb stores the
      reply, so stored replies differ from what the agent printed in
      that one respect.
7. Built on main a012e4ae (dsh as a built-in agent).
