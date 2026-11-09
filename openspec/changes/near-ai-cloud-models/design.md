# Design: NEAR AI Cloud models

Read `proposal.md` first. Every decision names its files and symbols;
line numbers are from origin/main a012e4ae and were checked, so find the
code by name if it moved. Build order (tasks.md): D0, D1, D2, D3, D4, D5,
D6. D7 is a later phase and is not built.

## D0. Shared pieces

### Validator codes

| Code | Severity | Decision | Rule |
|---|---|---|---|
| V220 | warning | D2 | a node's profile chain reaches a catalog model that has no tool calling or is not a chat model |
| V221 | warning | D4 | a node's profile chain spans more than one privacy class |
| V222 | warning | D1 | a node's profile chain names a cloud model the catalog does not list |

V223 to V229 stay reserved for this change. V187 to V199 belong to
google-connectors, V200 to V209 to routine-operations, V210 to V219 to
routine-health. Verify first: `git grep -ohE '"V2[0-9][0-9]"' crates |
sort -u` prints nothing at or above V220 (the other changes may have
landed V200 to V219), and `git grep -ohE '"V1[89][0-9]"' crates | sort -u`
stops at V186 or at a google-connectors code. The rules live in their own
module `crates/apb-core/src/validate/cloud_models.rs`, declared in
`validate/mod.rs` and called from `validate()` (line 359) after
`write_scope::check_write_scope` as
`cloud_models::check_cloud_models(playbook, ctx, &mut r); // V220-V222`.

### Finding codes outside the validator

| Code | Where | Severity |
|---|---|---|
| `cloud_model_unknown` | `model_check`, profile write, doctor, fit | warning |
| `cloud_model_no_tools` | same | error (blocking) |
| `cloud_model_not_chat` | same | error (blocking) |
| `cloud_effort_unsupported` | same | error (blocking) |
| `profile_privacy_class_changes` | profile write, `apb validate`, resolved profile | warning |
| `cloud_catalog_stale` | doctor | warning |
| `cloud_key_missing` | doctor, fit, start refusal | warning; refusal at start |
| `cloud_catalog_refresh_failed` | `apb models refresh`, the refresh route | error of that command |
| `privacy_class_left` | run surfaces | note |

Start refusals (the run gate, D2): `cloud_model_unknown`,
`cloud_model_no_tools`, `cloud_model_not_chat`,
`cloud_effort_unsupported`, `cloud_key_missing`.

### Names

1. Provider id `near-ai-cloud` (the catalog, config, `/api/models`,
   `priced_from`, `decisions.yaml catalog:`).
2. Route `nearai` (the dsh route and the profile spelling
   `nearai/<catalog id>[@<effort>]`, for example
   `nearai/Qwen/Qwen3.8-27B@high`). The route matches the catalog's own
   `owned_by: nearai` and the conventional custom-provider id.
3. Key variable `NEARAI_API_KEY`. Fixed in code; there is no setting to
   rename it (open question 1 in tasks.md).

### Knowledge placement (decision record 0005)

Facts about a cloud provider (URLs, key variable name, the catalog's
shape, the owner to class rule) live only in its module under
`apb_core::cloud_models`. Facts about how an agent reaches a provider
(dsh's `llm-pi-ai` row shape, the route spelling) live only in that
agent's module. The link between them is one declaration on the agent's
model rules (`ModelPolicy::cloud_routes`, D1). A new decision record
`docs/adr/0014-cloud-provider-knowledge-lives-in-its-catalog-module.md`
states this, in the shape of 0005 and 0013.

## D1. The catalog

### Module layout

`crates/apb-core/src/cloud_models/` (declared `pub mod cloud_models;` in
`crates/apb-core/src/lib.rs`):

1. `mod.rs`: the shared types and the registry.

```rust
/// One cloud provider apb knows: its catalog and how it is reached.
pub struct CloudProvider {
    pub id: &'static str,            // "near-ai-cloud"
    pub display: &'static str,       // "NEAR AI Cloud"
    pub base_url: &'static str,      // "https://cloud-api.near.ai/v1"
    pub models_url: &'static str,    // "https://cloud-api.near.ai/v1/models"
    pub key_env: &'static str,       // "NEARAI_API_KEY"
    pub embedded: &'static str,      // include_str! of the asset
    pub parse: fn(&[u8]) -> Result<Vec<CloudModel>, String>,
}
pub fn all() -> &'static [&'static CloudProvider];
pub fn lookup(id: &str) -> Option<&'static CloudProvider>;

/// One model row, normalized; never holds a secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudModel {
    pub id: String,
    pub owned_by: String,
    pub privacy_class: PrivacyClass,
    #[serde(default)] pub name: Option<String>,
    #[serde(default)] pub context_tokens: Option<u64>,
    #[serde(default)] pub max_output_tokens: Option<u64>,
    #[serde(default)] pub price_in_usd_mtok: Option<f64>,
    #[serde(default)] pub price_cached_in_usd_mtok: Option<f64>,
    #[serde(default)] pub price_out_usd_mtok: Option<f64>,
    #[serde(default)] pub features: CloudFeatures, // tools, structured_outputs, json_mode, reasoning
    #[serde(default)] pub input_modalities: Vec<String>,
    #[serde(default)] pub output_modalities: Vec<String>,
}
impl CloudModel {
    /// Text in and text out.
    pub fn is_chat(&self) -> bool;
    /// What an agent step needs: a chat model with tool calling.
    pub fn runs_agent_steps(&self) -> bool { self.is_chat() && self.features.tools }
    /// The efforts a step may name: `low`, `medium`, `high` for a reasoning
    /// model, none otherwise (open question 2).
    pub fn efforts(&self) -> &'static [&'static str];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyClass { ConfidentialTee, ThirdPartyTee, Incognito, Unverified }
```

`PrivacyClass` deserializes leniently (an unknown string reads as
`Unverified`), like `CostBasis` (`crates/apb-core/src/agents/output.rs`
line 235), so a value a newer apb wrote never fails a manifest or a
journal. Its order is from most to least private; `Unverified` is last.

2. `near_ai.rs`: `pub static NEAR_AI: CloudProvider`, `fn parse`, and
   `fn class_of(owned_by: &str) -> PrivacyClass`:
   1. `nearai` is `ConfidentialTee` (NEAR AI's own TEE fleet).
   2. `attested 3p` is `ThirdPartyTee`.
   3. An owner on the gateway list `anthropic`, `openai`, `google`,
      `x-ai`, `deepseek`, `qwen`, `moonshotai` is `Incognito` (relayed
      under NEAR AI's account, not private).
   4. Any other owner (`typesafe`, `levanto`, a new one) is `Unverified`.
   The parser reads the body's `data` array (the live shape checked on
   2026-11-09: `id`, `owned_by`, `name`, `context_length`,
   `max_output_length`, `pricing.input`, `pricing.output` in USD per
   million tokens, `pricing.input_cache_read` in USD per token as a
   string, `input_modalities`, `output_modalities`,
   `supported_features`). Rules: a row whose `id` holds a character
   outside `[A-Za-z0-9._:/-]`, starts or ends with `/` or holds `//` is
   dropped and counted; a price that is not a finite number at least 0
   reads as absent; `input_cache_read` is multiplied by 1e6; unknown
   fields are ignored; a body with no row left is an error. Duplicate ids
   keep the first row.
3. `catalog.rs`: the snapshot and the refresh.

```rust
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogSnapshot {
    pub provider: String,                   // "near-ai-cloud"
    pub source: CatalogSource,              // embedded | refreshed
    pub fetched_at: String,                 // RFC 3339; the asset's date for embedded
    pub digest: String,                     // sha256 of the body bytes
    pub dropped: u32,                       // rows the parser refused
    pub models: Vec<CloudModel>,
}
pub fn load(provider: &CloudProvider) -> CatalogSnapshot;          // cache, else embedded
pub fn load_all() -> Vec<CatalogSnapshot>;
pub fn find(model_id: &str, snap: &CatalogSnapshot) -> Option<&CloudModel>;
pub fn refresh(provider: &CloudProvider) -> Result<CatalogSnapshot, RefreshError>;
```

   `load` reads `<config_dir>/state/cloud-models/<id>.json`
   (`{"fetched_at": ..., "body": <the response JSON>}`). A cache that does
   not read or parse is ignored (the doctor reports it) and the embedded
   asset is used; `load` never touches the network. `refresh` is the
   only network code:
   1. `crate::process_mode::guard()` (line 66) refuses it before any
      request in read-only mode (`apb mcp --read-only`).
   2. `GET models_url` with `ureq` (already an `apb-core` dependency,
      `crates/apb-core/src/decision_probe.rs` line 110 shows the agent
      setup), HTTPS only, no redirects to another host, no
      `Authorization` header ever (the key is never read here), a
      10-second timeout, a 4 MiB body cap.
   3. The body is parsed with the provider's `parse`; an error or a
      status other than 200 leaves the cache untouched and returns
      `RefreshError` (code `cloud_catalog_refresh_failed`, the HTTP status
      or the parser's sentence, never the body).
   4. On success the cache is written with
      `crate::fsutil::atomic_write_private` (temp, rename, 0600).
   Tests point `models_url` elsewhere only through
   `APB_TEST_CLOUD_CATALOG_URL`, read under `#[cfg(any(test, feature =
   "testing"))]` like `APB_TEST_SYSTEM_ROOT`
   (`crates/apb-engine/src/adapter.rs` `system_root`).
4. `check.rs`: the one judgement of a cloud model, shared by every
   surface (D2, D4):

```rust
pub enum CloudIssue { Unknown { as_of: String }, NoTools, NotChat, EffortUnsupported { allowed: Vec<String> } }
impl CloudIssue { pub fn code(&self) -> &'static str; pub fn is_blocking(&self) -> bool; }
pub fn check(r: &CloudModelRef, snaps: &[CatalogSnapshot]) -> Option<CloudIssue>;
```

### Embedded data

`assets/cloud-models/near-ai-cloud.json`: the body of `GET
https://cloud-api.near.ai/v1/models` as fetched (about 66 KB, 64 models on
2026-11-09), unauthenticated. `CloudProvider.embedded` is
`include_str!("../../../../assets/cloud-models/near-ai-cloud.json")`; the
fetch date is a sibling constant `EMBEDDED_FETCHED_AT`. A CI test parses
it (like `models_table` `builtin_parses`) and asserts the rows the specs
name: `Qwen/Qwen3.8-27B` (`confidential_tee`, tools),
`Qwen/Qwen3-VL-30B-A3B-Instruct` (`confidential_tee`, no tools),
`z-ai/glm-5.3` (`third_party_tee`), `anthropic/claude-sonnet-5-5`
(`incognito`), `typesafe/jev-1.13` (`unverified`, not chat: output
modality `decisions`).

### Agents declare the routes they serve

`crates/apb-core/src/agents/policy.rs` `ModelPolicy` gains:

```rust
/// The cloud catalogs this agent reaches, each under a model prefix.
fn cloud_routes(&self) -> &'static [CloudRoute] { &[] }
/// The catalog model a profile model names, if any: the default reads
/// `<prefix>/<id>[@<effort>]` for each route.
fn cloud_model(&self, model: &str) -> Option<CloudModelRef> { ... }
```

with `pub struct CloudRoute { pub provider: &'static str, pub prefix:
&'static str }` and `pub struct CloudModelRef { pub provider: &'static
str, pub id: String, pub effort: Option<String> }` defined in
`cloud_models/mod.rs`. `DshModels` (`crates/apb-core/src/agents/dsh/mod.rs`)
returns `&[CloudRoute { provider: "near-ai-cloud", prefix: "nearai" }]`
and keeps the default `cloud_model`, which must agree with
`dsh::model::parse` (a test asserts that for every embedded model id).
No other agent declares a route in this change.

### Where the lists show it

1. `crates/apb-core/src/models_table.rs` `ModelOption` (line 353) gains
   `#[serde(default, skip_serializing_if = "Option::is_none")] cloud:
   Option<CloudOption>` with `provider`, `privacy_class`, `tools`,
   `reasoning`, `price_in_usd_mtok`, `price_out_usd_mtok`,
   `context_tokens`.
2. `crates/apb-core/src/agent_catalog.rs`: `Catalog` (line 29) gains
   `cloud: Vec<CatalogSnapshot>`; `assemble` (line 47) takes it as a
   third argument and stays pure; after the existing options, each agent
   with `cloud_routes` gets one option per catalog row that
   `runs_agent_steps`, id `<prefix>/<row id>`, vendor the provider id,
   `detected: false`, in catalog order, appended even though dsh has a
   closed list (the closed list is DeepSeek's own; a cloud route is apb's
   declared extension of it). `load` and `agents` call
   `cloud_models::catalog::load_all()`.
3. `GET /api/models` (`crates/apb-server/src/routes/meta.rs`
   `list_models_handler`, line 48) adds `"cloud": [{provider, display,
   base_url, key_env, key_present, source, fetched_at, digest, dropped,
   models}]`. `key_present` is whether the variable is set in the
   server's environment, never its value.
4. `POST /api/models/refresh` (new handler in `routes/meta.rs`, router in
   `crates/apb-server/src/lib.rs` next to line 220): runs `refresh` for
   every provider off the async runtime and answers the new `cloud`
   block or the refusal. It is behind the loopback-only route layer the
   discover routes use (`crates/apb-server/src/routes/discover.rs`
   `local_gate`, line 96: loopback peer, loopback `Host`, no forwarding
   header; move it to `state.rs` so both routers share one function).
5. `apb dashboard` refresh task: at start and then every 24 hours, when
   `cloud_models.<provider>.auto_refresh` is not `false`, a background
   task calls `refresh`; a failure is logged once per run of the task
   and leaves the cache as it was.
6. CLI `apb models [--refresh] [--json]` (new `crates/apb-cli/src/models.rs`,
   `Command::Models` in `main.rs`): prints each catalog's source, age,
   row count and per model id, class, tools, prices; `--refresh` calls
   `refresh` first. It is a person's command: refused inside a run step
   (`apb_core::step_env`).
7. `apb doctor` (`crates/apb-core/src/doctor.rs`): one line per provider:
   source and age, `cloud_catalog_stale` past 30 days, an unreadable
   cache, and `key_present` by name.

### Global config

`crates/apb-core/src/config.rs` `GlobalConfig` (line 19) gains
`#[serde(default)] pub cloud_models: CloudModelsConfig` with
`near_ai_cloud: Option<CloudProviderConfig { auto_refresh: Option<bool> }>`,
both `deny_unknown_fields`. Only `auto_refresh` exists; the default is
true. This is a behaviour change for an older apb (proposal, Impact
6.1).

### Model check

`crates/apb-core/src/model_check.rs` `ModelIssue` (line 25) gains
`Cloud(CloudIssue)`; `ModelContext` (line 101) gains `cloud:
Vec<CatalogSnapshot>` loaded in `ModelContext::load`. `check` (line 127)
asks `rules.cloud_model(model)` after the agent's own refusal
(`list_id`) and after the person's `model_policy` (`policy_issue`, line
139), which keeps the last word over a cloud route as over any model:
for a cloud model it then returns `cloud_models::check::check` as
`ModelIssue::Cloud`, or `None`, and skips the closed-list and detection
steps (dsh's closed list says nothing about NEAR, and detection cannot
list a cloud catalog). `code`, `is_blocking` and `describe` delegate.
Three callers decide severity on their own and need a change each:

1. `apb validate` (`crates/apb-cli/src/run.rs` `validate_profile_models`,
   line 362): a blocking cloud issue prints as `error`, `Unknown` as
   `warning`.
2. The doctor's profile lines (`crates/apb-core/src/doctor.rs` line 306,
   the `match issue`): `ModelIssue::Cloud(_)` gets its own arm, `Fail`
   when blocking, else `Warn`; today an unknown variant reaches `_ =>
   continue` and would be dropped.
3. Fit and adoption (`crates/apb-engine/src/fit/setup.rs`):
   `check_profile` (line 108) iterates the executor and fallbacks only;
   it switches to `ProfileDoc::executor_pairs()` so tiers are checked
   too, and `model_finding` (line 164) takes `blocking` from
   `ModelIssue::is_blocking()` for `ModelIssue::Cloud`, keeping its own
   rule for the other variants. The adoption report
   (`crates/apb-mcp/src/advisory_tools.rs`) uses the same function.

`profile_write` (`crates/apb-mcp/src/profile_tools.rs` line 204) calls
`check` for every `executor_pairs()` entry after `doc.check()`: a
blocking cloud issue refuses the write with its code and sentence, an
unknown model adds a warning to the existing `warnings` vector (today
always empty), so the dashboard (`routes/profiles.rs` `write_profile`)
and MCP show it.

## D2. Execution on dsh

### The manifest keeps the row

`crates/apb-engine/src/invocation.rs` `ResolvedInvocation` (line 18)
gains:

```rust
/// The catalog row of a cloud model this executor runs, as the run
/// started; every later read (the patch, the price, the class) uses it.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub cloud: Option<CloudSnapshotRef>,
```

with `CloudSnapshotRef { provider, route, model: CloudModel, effort,
catalog_digest, fetched_at }` in `apb_core::cloud_models`.
`resolve_invocation` (line 188) fills it from
`lookup(agent).models().cloud_model(model)` and
`catalog::load_all()`. The manifest builder
(`crates/apb-engine/src/manifest.rs` lines 172 to 216, executor,
fallbacks, tiers) gets it with no change of its own; the legacy snapshot
(`legacy_snapshot.rs` line 221) too. Post-start reads come from the
manifest (decision record 0002): a refresh during a run changes nothing
for it, a resume included.

### Start refusals

A new gate check `crates/apb-engine/src/gate/cloud.rs`, called from
`gate::check_run` after the profile resolution and before the billing
half, walks every `ResolvedInvocation` of the run's profiles (primary,
fallbacks, tiers, over the whole tree as the billing half does,
`crates/apb-engine/src/gate/billing.rs`):

1. a cloud model the snapshot lacks: refused `cloud_model_unknown`
   ("not in the NEAR AI Cloud catalog fetched <date>; run `apb models
   --refresh`");
2. `NoTools`, `NotChat`, `EffortUnsupported`: refused with that code;
3. the primary executor's key variable does not reach the spawn
   (`adapter::SpawnEnv::reaches`, the same fact the cost basis reads):
   refused `cloud_key_missing` naming the variable. A fallback or tier
   whose key is missing is a start warning, not a refusal, since the
   chain may never reach it. dsh's headless profile disables its
   credential store (`--dump-config` of dsh 0.2.2-rc.1 shows `id:
   credentials ... disabled: true` under the headless patch), so the
   environment is the only source and this check is exact.

`acknowledge_untrusted` does not bypass these. Host execution mode
(`apb_core::execution`) skips them, as it skips every agent finding.

The check is one function, `gate::cloud::check_invocations(&[&ResolvedInvocation],
&SpawnEnv) -> Result<Vec<Warning>, Refusal>`, called from `check_run`
and from the mid-run rebind path: `gate::check_rebind`
(`crates/apb-engine/src/gate/mod.rs` line 1520) runs it on the new
profile's resolved chain before `scheduler/rebind.rs` `resolve_rebind`
snapshots it, so a supervisor's `supervisor_rebind_profile` to a profile
on the vision model (written by any means, on disk included) is refused
with the same code.

### The patch carries the provider

`crates/apb-core/src/agents/setup.rs` `HostSetup::scoped_args` (line 103)
gains a third parameter `cloud: Option<&CloudSnapshotRef>`; the one
caller `crates/apb-engine/src/adapter/spawn_env.rs` `with_scoped_args`
(line 310) passes `task.cloud`, a new field of `SpawnTarget` (line 244)
and `AgentTask` (`adapter.rs` line 166) filled from the attempt's
`ResolvedInvocation.cloud`. Outside a run (the doctor's paid probe
`apb doctor --probe-agent`) the spawn path builds it from
`catalog::load_all()`.

`crates/apb-core/src/agents/dsh/model.rs`:

1. `patch_body(sel, cloud)` (line 123) keeps the `agent-default-model`
   row and, when `cloud` is `Some` and `sel.route == "nearai"`, appends:

```yaml
- id: llm-pi-ai
  name: "@deepseek-ai/dsh-llm-pi-ai"
  config:
    providers:
      "nearai":
        displayName: "NEAR AI Cloud"
        api: "openai-completions"
        baseURL: "https://cloud-api.near.ai/v1"
        apiKeyEnv: "NEARAI_API_KEY"
        models:
          - id: "Qwen/Qwen3.8-27B"
            contextWindow: 262144
            maxTokens: 65536
            reasoningEfforts: { "low": "low", "medium": "medium", "high": "high" }
```

   Every value is JSON-quoted as today. `contextWindow` and `maxTokens`
   are left out when the row has none; a non-reasoning model writes
   `reasoningEfforts: false`. No price, no secret, no person's setting.
2. `patch_args(model, scoped_dir, cloud)` (line 153) passes it on; the
   file stays named after its content (`model-<digest>.patch.yml`), so
   two list items on different NEAR models each read their own.
3. A `nearai/` model with `cloud: None` writes today's patch (the
   model row alone), so the person's own `nearai` route, if any, serves
   it. This is the case of a run whose manifest an older apb wrote (it
   has no `cloud` field; `manifest.rs` reads it unchanged on resume, line
   814), and of a spawn outside a run whose model the catalog lacks. A
   new run never reaches it with an unknown model: the start refuses
   `cloud_model_unknown` first.

This was checked against dsh 0.2.2-rc.1 locally, with no model call:
`DSH_HOME=<empty dir> dsh --profile headless --patch <file>
--dump-config` composes both rows, the `llm-pi-ai` entry showing
`# patched by <file>` with the route. A `--patch` overlay replaces the
whole `config` of the entry it names (`cordis-plugin-include`
`applyPatches`: `target[key] = value`), so in a NEAR spawn the person's
own `llm-pi-ai` routes are not mounted. That is intended: the step runs
on `nearai` alone, and other spawns keep the person's routes. It is
documented in `docs/AGENT-SETUP.md`.

### The key reaches dsh and nothing else

1. `DshSetup::model_env` (`dsh/mod.rs` line 304) returns
   `["NEARAI_API_KEY"]` for the `nearai` route, read from
   `cloud_models::lookup("near-ai-cloud").key_env` through the route
   table, so a profile with `process_env: allowlist` keeps it
   (`crates/apb-engine/src/agent_env.rs` line 25). `agent_env_applied`
   names it; no value is journaled.
2. Store-time scrub. Redaction on read cannot cover this key: replies
   are journaled verbatim (`scheduler.rs` line 3397 through
   `EventPayload::node_settled`, attempt summaries at `scheduler/node.rs`
   line 3204), and a reader that runs later without the variable, or
   after the key was rotated, does not know the old value. So the
   adapter scrubs before anything is stored: when an attempt's
   invocation has `cloud`, the value of its `key_env` as the spawn saw it
   (read once from the `SpawnEnv` into an `apb_core::secret::Secret`,
   never written) is replaced with `[redacted:NEARAI_API_KEY]` in the
   reply, the failure text, the stream log and the per-attempt
   transcript the adapter writes (`crates/apb-engine/src/adapter.rs`, at
   the point `AgentReport` and `AgentFailure` are built, and the stream
   log writer), before the scheduler journals any of it. The match is
   exact on the value (at least 8 characters, else nothing is replaced
   and the start warns, like a short secret param). The value is never
   stored, cached or compared outside `Secret`.
3. Read-time redaction stays as a second layer:
   `crates/apb-engine/src/run_redact.rs` `RunRedactor::of` adds the
   current value of each key variable the manifest names, when set, to
   the values `redact_text` and `redact_json` scrub. Text an older apb
   stored is covered only while the same value is set.
4. The supervisor spawn passes the same `cloud` (decision record 0013:
   it gets the leading arguments).

### The node rule (V220, V222)

`ValidationContext` (`crates/apb-core/src/validate/mod.rs` line 272)
gains two fields, empty by default so callers that build a bare context
stay silent, as `connectors` does:

```rust
/// The executor pairs (agent, model) of each resolvable profile, keyed by
/// `<scope>:<name>` (`project:` and `global:`).
pub profile_chains: BTreeMap<String, Vec<(String, String)>>,
/// The cloud catalogs as this machine has them.
pub cloud: Vec<CatalogSnapshot>,
```

`ValidationContext::for_registry` (line 300) fills them: each project
profile of `reg` and each global profile (via
`apb_core::profile_store`), parsed with `ProfileDoc::from_yaml` and read
through `ProfileDoc::executor_pairs()`; an unreadable profile is left
out (V14 and run start report it). `check_cloud_models` resolves a
node's `profile` (or `defaults.profile`) as run start does for `scope:
auto` (project, then global), for every node that binds a profile
(`agent_task`, a list `agent_task`, a judge node's emulation `profile`),
then for each pair whose agent declares `cloud_routes`:

1. V220 (warning): `NoTools` or `NotChat`, naming the node, the profile,
   the pair and why ("`Qwen/Qwen3-VL-30B-A3B-Instruct` has no tool
   calling; agent steps need it; a cli run of this node is refused at
   start"). It is a warning, not an error, because run preparation
   rejects every validator error whatever the execution mode
   (`crates/apb-engine/src/scheduler/prepare.rs` line 725), and a run in
   host mode spawns no dsh. The errors are the profile finding
   `cloud_model_no_tools` (`profile_write` refuses, `apb validate` prints
   `error` on the profile line) and the start refusal in cli mode.
2. V222 (warning): `Unknown`, naming the catalog date.

An effort the model does not take is a profile finding
(`cloud_effort_unsupported`, D1), not a node rule.

## D3. Cost

### Pricing an attempt

`crates/apb-core/src/agents/output.rs` `AgentUsage` (line 182) gains:

```rust
/// Where `cost_usd` came from when apb priced it from a catalog, not the
/// CLI. Absent for a printed cost and in older journals.
#[serde(default, skip_serializing_if = "Option::is_none")]
#[cfg_attr(feature = "ts", ts(optional))]
pub priced_from: Option<PricedFrom>,
```

with `PricedFrom { catalog: String, model: String, digest: String,
fetched_at: String }`; its decode is lenient (a shape a newer apb wrote
reads as absent, like `AttemptFinished.usage`). The doc comment of
`cost_usd` adds "or as apb priced it from a catalog (`priced_from`)".

`crates/apb-engine/src/adapter.rs`: a new `fn stamp_catalog_price(task,
usage)` runs right after `stamp_cost_basis` at both sites (lines 1841 and
1845). When `usage.cost_usd` is `None` and `task.cloud` is `Some` with
both an input and an output price:

```text
cost = (input_tokens * in + cache_read_tokens * cached_in_or_in
        + cache_write_tokens * in + output_tokens * out) / 1e6
```

rounded like `run_stats` (`(cost * 1e8).round() / 1e8`), and
`priced_from` is set. A printed cost is never replaced. A row without
prices leaves the cost absent (the budget then imputes, as today).

### The basis

`crates/apb-core/src/agents/cost_basis.rs` `resolve` (line 23) gets a
first step before the module's own: when
`lookup(agent).models().cloud_model(model)` is `Some` and
`facts.present(key_env)`, the reading is `CostBasis::Billed` with
evidence `cloud:<provider>:<key_env>`. Without the key the step decides
nothing. The run gate predicts with the same function
(`adapter::cost_reading`, line 669), so the billing half of the launch
check (decision record 0012) sees `billed` for a NEAR route and the
`billing:` section decides it; every spawn reads it again.

### What reads it

Nothing else changes: the budget's spent fold
(`crates/apb-engine/src/budget/spent.rs` line 82), the estimate samples
(`crates/apb-engine/src/run_estimate/sample.rs` lines 42 and 47), `apb
stats` (`crates/apb-engine/src/run_stats.rs` line 749), the run view
total (`crates/apb-engine/src/run_view.rs` line 598) and the retro
(`run_retro.rs` line 424) all read `usage.cost_usd`. The run surfaces
show "priced from NEAR AI Cloud catalog <date>" next to a cost that
carries `priced_from`. `docs/AGENT-SETUP.md` drops "a run budget's money
limit (no cost is reported)" for the `nearai` route.

## D4. Privacy class

### In the resolved profile

`crates/apb-mcp/src/profile_tools.rs` `profile_get` (line 108) adds a
`resolved` block, built by a new `fn resolved_chain(doc, cloud)` in the
same file (the HTTP route `GET /api/profiles/{name}`,
`crates/apb-server/src/routes/profiles.rs` `get_profile`, returns the
same JSON):

```json
"resolved": {
  "chain": [
    {"role": "executor", "agent": "dsh", "model": "nearai/Qwen/Qwen3.8-27B",
     "cloud": {"provider": "near-ai-cloud", "id": "Qwen/Qwen3.8-27B",
               "privacy_class": "confidential_tee", "tools": true,
               "price_in_usd_mtok": 0.04, "price_out_usd_mtok": 1.35}},
    {"role": "fallback", "index": 0, "agent": "dsh",
     "model": "nearai/anthropic/claude-sonnet-5-5", "cloud": {"privacy_class": "incognito"}},
    {"role": "tier", "name": "quick", "agent": "claude", "model": "claude-haiku-5-5", "cloud": null}
  ],
  "privacy": {"classes": ["confidential_tee", "incognito", null],
              "uniform": false, "least_private": "incognito",
              "warnings": ["profile_privacy_class_changes"]}
}
```

A model outside any catalog has `cloud: null` and counts as an
unclassified entry (`null` in `classes`); `least_private` is `null` when
any entry is unclassified. `uniform` is true only when every entry has
the same class. The warning `profile_privacy_class_changes` is added
when `uniform` is false and any entry is classified; it also appears in
`profile_write`'s warnings and in `apb validate`'s profile lines. MCP
`profile_list` adds only `privacy.least_private` and `uniform` per
profile.

### On a node (V221)

`check_cloud_models` (D2) adds V221 (warning) when the chain a node
binds is not uniform by the rule above and at least one entry is
classified, naming the classes in chain order.

### On each attempt

`crates/apb-engine/src/event.rs` `EventPayload::AttemptStarted` (line
194) gains `#[serde(default, skip_serializing_if = "Option::is_none")]
cloud: Option<AttemptCloud>` with `{provider, model, privacy_class}`,
written from the attempt's `ResolvedInvocation.cloud`; older readers
skip the field (decision record 0001). The run view
(`crates/apb-engine/src/run_view.rs`) adds per node the classes its
attempts ran on and `privacy_class_left: true` when an attempt's class
is not its profile executor's (a fallback or a tier on another class,
or on a model outside any catalog). `apb runs <id>`, `run_status` and
`GET /api/runs/{id}` print it; the run page shows it next to the
attempt's model.

### In /api/models

Every catalog row in the `cloud` block and every cloud option carries
`privacy_class` (D1).

## D5. Decision provider

`crates/apb-core/src/decisions.rs`:

1. `ProviderDoc` (line 471) and `ProviderSpec` (line 264) gain
   `#[serde(default, skip_serializing_if = "Option::is_none")] catalog:
   Option<String>`.
2. In `load_file` (line 576) validation: `catalog` is only for `kind:
   llm_emulation` (else the error "`catalog` is only for kind
   llm_emulation"); it must name a provider of `cloud_models::all()`;
   `base_url` defaults to the provider's `base_url`; `api_key` defaults
   to `{{env.<key_env>}}` (`KeyRef::Env`); `model` is required and must
   be in `catalog::load_all()` and `is_chat()`, else an error naming the
   catalog date. `structured_output` defaults to `json_schema` when the
   row has `structured_outputs`, else `prompt_only`; an explicit
   `json_schema` on a row without `structured_outputs` is an error.
3. A new field `ProviderSpec.cloud: Option<CloudSnapshotRef>` (serde
   default, skipped when `None`), the whole row with its prices, class,
   catalog digest and fetch time, set when the file loads. The run's
   snapshot of the decisions (`RunExecutionManifest.decisions`,
   `crates/apb-engine/src/manifest.rs` line 319) keeps it, so a decision
   asked after a catalog refresh and a resume is priced and classed from
   the row the run started with.

`crates/apb-engine/src/decision/mod.rs`:

1. `list_price_per_million` (line 272) is kept for the TypeSafe ids and
   joined by the provider's catalog row: when the answering provider has
   `cloud`, the cost is `(input_tokens * in + output_tokens * out) /
   1e6` from `ProviderSpec.cloud`, and `cost_estimated` stays true (the
   price is a list price, not a billed figure).
2. `EventPayload::DecisionMade` (`event.rs` line 828) gains
   `#[serde(default, skip_serializing_if = "Option::is_none")]
   privacy_class: Option<String>`, filled from `ProviderSpec.cloud`.
3. `apb decisions` (the report under `decision/report/`) and the doctor
   line show the class next to the provider.

The doctor's free check for `llm_emulation` (`ProviderKind::free_endpoint`,
`/models`) reaches the NEAR listing, but today `decisions.rs` `probe`
resolves every environment key and hands `ProbeKey::Value` to
`probe_request` (line 1073), so `decision_probe.rs` `send` would attach
the key as a bearer, and without a key it falls back to a TCP connect.
For a provider with `catalog` the probe gets a new branch: a plain `GET`
with no key at all, whether the variable is set or not, and the line
reports whether the variable is set by name.

`typesafe/jev-1.13` on NEAR has output modality `decisions` and is not a
chat model, so `catalog: near-ai-cloud` refuses it; reaching it is a
`kind: systemone` question this change leaves alone (open question 3).

## D6. Docs and dashboard

1. `docs/CLOUD-MODELS.md` (new): the catalog, refresh and cache, the
   classes and their rule, the profile spelling, the key, the patch,
   pricing, the warnings, the decisions provider. Linked from
   `docs/PROFILES.md` and `docs/AGENT-SETUP.md`.
2. `docs/AGENT-SETUP.md` "DeepSeek Harness": the `nearai` route, the
   replaced `llm-pi-ai` config in a NEAR spawn, the cost now reported.
3. `docs/DECISIONS.md`: `catalog:` on `llm_emulation`.
4. `CLAUDE.md` and `AGENTS.md` (mirror rule): the `cloud_models` module
   in the apb-core list, the concept "Cloud models" in "Concepts that
   span files".
5. Web (`web/src/lib/api/core.ts` `ModelOption`, `ModelCatalog`;
   `web/src/lib/profileedit.ts`; `web/src/pages/ProfileEdit.svelte`): the
   model combobox shows cloud options with a class badge and a "no
   tools" marker for catalog rows it lists elsewhere, a "Refresh catalog"
   action calling `POST /api/models/refresh`, and a resolved chain panel
   (executor, fallbacks, tiers with their classes) with the
   `profile_privacy_class_changes` warning. No Tailwind; plain CSS over
   the existing variables, as the editor already does.
6. Generated TypeScript: `AgentUsage.priced_from` and any mirrored type
   regenerate with `APB_WRITE_TS=1 cargo test -p apb-server --lib
   ts_contract`.

## D7. Later phase: attestation evidence (not built)

For each NEAR model with class `confidential_tee` or `third_party_tee`
that a run's attempts used (`attempt_started.cloud`), a later change
would fetch `GET {base_url}/attestation/report?model=<id>` once per run
at its end, keep only its sha256, the fetch time and the HTTP status as
a run evidence record (`docs/RESULTS.md` evidence store), and show it on
the report page with a link to NEAR's verification UI. It must state
plainly that a report proves the model's enclave at fetch time, not any
particular response. Nothing in D1 to D6 depends on it.

## Risks and how the design meets them

1. A stale catalog. The embedded copy is always there, the cache has a
   date the doctor shows, an unknown model is a warning at authoring and
   a refusal at start that names `apb models --refresh`.
2. A hostile or broken catalog response. Ids are character-checked
   before they reach a patch, every patch value is JSON-quoted, the body
   is capped, nothing in it is executed, and a parse failure keeps the
   last good cache.
3. A key leak. Only the name is written (patch, manifest, journal); the
   value reaches the child through the spawn environment, is scrubbed
   from the agent's text before apb stores it and again on read; the
   catalog refresh and the decisions probe send no key.
4. A fallback that silently leaves the class. Profile warning, node
   warning, per-attempt class and `privacy_class_left`.
5. The person's own dsh routes vanish in a NEAR spawn. Documented; the
   step needs only `nearai`.

## As built: where the code differs from the text above

The decisions above were built as written except for these points, each
found against the code while building it.

1. `CloudProvider.parse` returns the rows and the count it refused
   (`Result<(Vec<CloudModel>, u32), String>`, type `ParseFn`), and the
   provider carries `embedded_fetched_at` beside the module's
   `EMBEDDED_FETCHED_AT`. `CatalogSnapshot` also carries `cache_problem`
   (why a cache file was not used), which the doctor line reports.
2. The helpers that ask an agent's model rules live in `apb_core::agents`,
   not in `cloud_models`: `agents::cloud_model(agent, model)`,
   `agents::cloud_snapshot(agent, model, snaps)` and
   `agents::chain_privacy(pairs, snaps)`. `cloud_models::privacy` keeps the
   pure rule (`summarize`, `describe`, `CLASS_CHANGES`). `cloud_models`
   then never imports `agents`, `config` never imports `cloud_models`
   (`CloudModelsConfig::provider` matches the id literally) and the catalog's
   doctor line returns a warn flag instead of a `doctor::CheckStatus`: each
   of those edges closed a module cycle the code-ranker gate refuses.
   `cloud_models::date` (the date of an RFC 3339 time) and
   `cloud_models::MIN_KEY_CHARS` (8) sit in the module root for the same
   reason.
3. The start check runs in `scheduler::prepare`, right after the isolation
   check, not inside `gate::check_run`: only prepare knows the run's
   execution mode (host mode skips the check), and it still refuses before
   the run directory exists, for every start path (CLI, MCP, dashboard,
   trial, child). `gate::check_rebind` applies the same judgement to the
   profile document a rebind names (`gate::cloud::check_profile`), before
   `scheduler/rebind.rs` snapshots it. Start warnings (a fallback's or a
   tier's missing key, a key too short to scrub) are printed to stderr.
4. The catalog row reaches the spawn on `SpawnEnv::cloud` (set per attempt
   from the executor step's `ResolvedInvocation::cloud`), not as new fields
   of `AgentTask` and `SpawnTarget`: `SpawnEnv` already reaches every spawn
   path, the supervisor's and the doctor's probe included, and every
   attempt literal stays unchanged.
5. The scrub lives in `adapter/key_scrub.rs` and is applied in
   `run_cancellable` to the stream log, the transcript directory, the
   reply (`output`, `summary`, `raw`, a question) and the failure text. The
   failure classifier (`failure_class::classify`) now ignores
   `[redacted:<name>]` markers: the marker names `NEARAI_API_KEY`, whose
   `api_key` would otherwise read as an auth failure and end the chain.
6. The privacy class of each attempt is shown on the existing per-attempt
   row of every run surface (`ExecutionIdentityView.cloud` and
   `privacy_class_left`, the `attempt_models` of `run_status` and
   `GET /api/runs/{id}`, the model line of `apb runs <id>`), against the
   first entry of the node's chain as `primary_models_of` reads it. The
   run's usage totals carry `priced_from` lines ("priced from NEAR AI Cloud
   catalog <date>").
7. `EffectiveDecisions::capped_by` keeps a snapshot provider when the live
   file lists the same route with another catalog row
   (`ProviderSpec::same_route` compares everything but `cloud`); with plain
   equality a catalog refresh would have dropped the run's NEAR provider.
   `ProviderSpec.catalog` is kept beside `ProviderSpec.cloud`.
8. Tasks 2.3 asked for both a `ScopedEnvError` and the model row alone for
   `nearai/<id>` without a row; design D2 item 3 (the model row alone) was
   built and the task text fixed.
9. The quota case of task 4.3 uses dsh's `RATE_LIMIT` error code: apb reads
   dsh's `QUOTA` code as a spent balance (a budget stop that blocks the
   chain on one account), so only a rate limit reaches the fallback.
10. The resume cases of tasks 2.3 and 5.2 are proven at unit level: a
    manifest entry without `cloud` reads back with none and the patch of a
    `nearai/` step without a row is the model row alone
    (`invocation::tests`, `dsh::model::tests`), and a refreshed live row
    keeps the snapshot's row (`decisions::tests::a_changed_catalog_row_keeps_the_snapshot_provider`).
11. The embedded asset is the listing fetched on 2026-11-09 by the first
    attempt at this change (64 rows, no key or personal data); it was not
    fetched again.

## D8. External data runs

Capability `run-external-data`. Built after D6 and independent of D1 to
D5: it touches no catalog code and can land on its own. A run may
process data that comes from outside the operator (a third party's
documents, messages or records) and must not be kept for learning. The
flag `external_data: true` at start marks such a run; apb then keeps
everything the run produces in its run directory and nothing in a store
that later runs or learning tools read. Line numbers are from a012e4ae.

### Field home

1. The authority is the manifest:
   `RunExecutionManifest.external_data: bool`
   (`crates/apb-engine/src/manifest.rs`, next to `unattended`), with
   `#[serde(default, skip_serializing_if = "std::ops::Not::not")]`, so an
   unflagged run's manifest is byte-identical to today's.
   `RunExecutionManifest::is_empty` gains `&& !self.external_data`, so a
   flagged run with no profiles still writes its manifest. It is written
   in `scheduler/prepare.rs` `build_run_manifest` (next to
   `manifest.unattended` at line 1220) and covered by the origin stamp
   (`apb_core::run_origin::stamp`).
2. The origin carries it too. A new variant `RunOrigin::ExternalData`
   (serialized `origin: { kind: external_data }`) is written instead of
   an absent origin when the run would otherwise be production; a
   scenario or eval run keeps its own origin and only the boolean.
   `RunOrigin::is_production` stays `matches!(self, RunOrigin::Production)`,
   so a reader gated on the origin alone skips a flagged production run,
   and an apb that knows `origin` but not the variant reads it as
   `RunOrigin::Unknown` through `lenient_origin`.
3. A downgrade guard: a flagged run's `execution.mode` carries the
   suffix `+external` (`ManifestExecution::EXTERNAL_SUFFIX`, beside
   `GUARD_SUFFIX` at line 488), stripped by `base_mode` and read by
   `RunExecutionManifest::is_external`. An apb without this change does
   not know the mode and refuses to drive the run with its version
   message (`RunExecutionManifest::execution_mode`), so an older binary
   can neither resume a flagged run (and then write the cache or the
   run-end stores) nor amend its manifest during a drive. Readers of an
   older apb that do not drive (stats, history) are covered by item 2.
4. A new `RunOrigin::runs_follow_up()` is true for `Production` and
   `ExternalData`. The readers that decide whether a run's own follow-up
   is scheduled and shown (`follow_up/mod.rs` line 134,
   `follow_up/read.rs` lines 42 and 64, `follow_up/waker.rs` line 30,
   `run_outcome.rs` line 140) switch to it. `run_stats/follow_up.rs`
   (line 240) switches to `learns_from`, so a flagged run's follow-up
   never enters statistics.
5. The journal: `EventPayload::RunStarted` (`event.rs` line 124) gains
   `#[serde(default, skip_serializing_if = "std::ops::Not::not")]
   external_data: bool`. No new event type (decision record 0001).
6. One module owns the rule: `crates/apb-engine/src/external_data.rs`
   with
   1. `pub fn is_flagged(run_dir) -> bool`: the manifest's boolean, its
      origin variant, its mode suffix or `run_started.external_data`
      (any source true means flagged; nothing reads a flag as cleared);
   2. `pub fn learns_from(run_dir) -> bool`: the manifest reads, its
      origin is production and `is_flagged` is false. Every learning
      reader calls this, never the origin alone: `run_history`
      (`counts_as_history` over `StatsRun`, which gains
      `learns: bool` filled by it in `StatsRun::load`, and
      `production_dir`), `previous_success.rs` line 107,
      `corrections::observe`, `scenario/capture.rs` line 427,
      `run_stats.rs` line 700;
   3. `pub fn refuse_if_flagged(run_dir, what) -> Result<(), EngineError>`:
      `EngineError::Invalid("external_data_run: ...")`, failing closed
      when the manifest does not read;
   4. `pub fn for_start(root, requested: Option<bool>, parent_run,
      continued_from) -> Result<bool, Refusal>`: the effective flag of a
      start (below), called by every surface before its own checks;
   5. `pub fn learnable_run_dirs(roots, cap) -> Vec<PathBuf>`: the newest
      run directories that `learns_from`, the filter applied before the
      cap within a scan bound of four times the cap, so flagged runs
      never crowd earlier history out of a capped window;
   6. `pub fn flagged_markers(roots) -> FlaggedMarkers`: the agent
      session ids (`attempt_finished.session`) and run ids of flagged
      runs under the given project roots, for discovery and the save
      hook.

Naming: `external_data` everywhere (manifest, journal, REST, MCP,
`RunDetail`, `run_status`), `--external-data` on the CLI, mode suffix
`+external`. It names where the data comes from, not a policy, so a
later privacy policy (out of scope 3 of the proposal) can build on it.

### Start surfaces

`RunOptions` (`scheduler/entry.rs` line 11) gains `external_data: bool`,
set from `external_data::for_start`:

| Surface | Where |
|---|---|
| REST run | `routes/playbooks.rs` `RunBody.external_data`; `for_start` runs before the remember capture at line 461 |
| REST scenario run | `routes/scenario.rs` `post_scenario_run_handler` body |
| Dashboard Run dialog | `web/src/lib/RunStartDialog.svelte`, a checkbox "External data: keep nothing for learning" |
| CLI | `crates/apb-cli/src/run.rs` `RunArgs.external_data`; `eval.rs` `start_run` (each repetition's `apb run` gets `--external-data`); `scenario.rs` (`apb eval --level quick|smoke --external-data`: there is no `apb scenario run`) |
| Supervised CLI start | `run.rs` line 1148: the `__drive-supervised` argv gains `--external-data`, since that process prepares the run itself |
| MCP | `server/args.rs` `PlaybookRunArgs`, `PlaybookTrialArgs`, `PlaybookExecutePlanArgs`; `server/scenario.rs` `PlaybookScenarioRunArgs` |
| Resume | `RunResumeArgs`, `routes/runs.rs` `ResumeBody`, `apb resume --external-data <bool>`: optional, checked only |
| Sub-playbook child | `scheduler/node.rs` line 4971, next to `opts.unattended`: `opts.external_data = external_data::is_flagged(run_dir)`, which also covers scenario children under `.apb/scenario-runs` |
| Detached drives | `__drive-run`, `apb __pause-waker`: they read the flag from the manifest; nothing on argv. `__scenario-run` prepares its run itself, so the flag crosses on its argv (`--external-data`, `ScenarioChild.external_data`), as for `__drive-supervised` |

`external_data::for_start`:

1. A parent (`parent_run`) or `continued_from` predecessor that is
   flagged makes the start flagged. An omitted request inherits; an
   explicit `false` is refused with `external_data_immutable`.
2. The surface then refuses `external_data && remember` with
   `external_data_remember_refused` (MCP: a `{ error, detail }` answer, as
   `notify_unavailable` is answered; CLI: exit 2; REST: 409), before the
   gate and before anything is written, right where the surface computes
   the effective flag (`external_data::check_remember`). Refusing, not ignoring: a caller that
   asked to remember expects the next start to recall, and a silent drop
   would surprise it later, while a refusal costs one flag and leaves no
   doubt that nothing was stored. Because the effective flag is known
   here, a `continued_from` successor that inherits is refused too.

In the engine:

3. Recall: `prepare.rs` line 879 `let recall = match scenario` gains the
   arm "flagged: the empty recall", the value a scenario gets, and
   `gate/params.rs` line 127 treats a flagged start's memory as empty
   (`without_default(params)`), so the `param_needs_value` check and the
   start agree. Values the caller passed are used as passed.
4. The candidate: the gate's candidate choice
   (`RunPermit::candidate_skipped`) skips a waiting candidate with the
   reason `external_data`; `manifest.candidate_trial` (`prepare.rs` line
   1222) gains `&& !opts.external_data`, so a flagged start that names
   the candidate version explicitly, or a fixed-choice retry, is no
   trial; `candidate/settle.rs` (line 43) and `candidate/quality.rs`
   also return early on `!learns_from`.
5. A resume compares a passed value with `is_flagged` and refuses any
   mismatch, in both directions, with `external_data_immutable`
   (`external_data::check_resume`, an argument check before the run gate);
   it never writes the field. `manifest::amend` keeps `external_data`, `origin`
   and the mode suffix (a test pins it).

No validator code is needed: the flag is a run argument, not part of a
definition. The refusal codes are `external_data_immutable`,
`external_data_remember_refused` and `external_data_run`, named in
`docs/GUARDRAILS.md` with the other start refusals.

### Every learning and reuse path

| # | Path | Where | Decision |
|---|---|---|---|
| 1 | Known problems, run-end observation | `known_problems::observe` (`known_problems/mod.rs` line 88), called by `run_end::record` | Excluded: returns 0 for a flagged run or one whose manifest does not read (`refuse_if_flagged`). It has no origin check today, so the guard goes in `observe` itself, which `scan` also calls; scenario and eval runs keep contributing as before (`learns_from` would have changed that too) |
| 2 | Known problems scan | `known_problems::scan` over `run_history::runs_of` | Excluded through `learnable_run_dirs` and `observe` |
| 3 | Known problems shown at start and the recheck | `known_problems::at_start`, `recheck` | Kept: they read the store into the run and a recheck reads playbook source only |
| 4 | Corrections ledger | `corrections::observe` (line 35) | Excluded through `learns_from`, which also covers a flagged scenario run |
| 5 | Node cache | `scheduler/cache.rs` `settle` (line 750), `capture_artifacts` | Excluded for writes: the cache context carries `keep: false` and `settle` journals `node_cache_rejected` ("the run processes external data") instead of storing. Lookups stay: a hit returns the operator's earlier output |
| 6 | Judge examples from a run | `tools/judge.rs` `judge_example_save`, `judge_run_values`; `judge_eval::example::save_from_run`, `run_values` | Excluded: `refuse_if_flagged` in the engine functions, so MCP, HTTP and `apb judge` refuse alike |
| 7 | Scenario capture | `scenario/capture.rs` line 427, `tools/scenario_capture.rs` | Excluded: `refuse_if_flagged` first, code `external_data_run` |
| 8 | Forward patches | `forward_patch::create`, `tools/supervisor.rs` `playbook_forward_patch` | Excluded: refused with `external_data_run` |
| 9 | In-run patches | `tools/supervisor.rs` line 297 `create_patch_version` writes a registry version; `scheduler/patch.rs` line 309 may promote it to `current` | Kept for the run, fenced for everyone else: the version is created (the run executes it) with `VersionProvenance.external_data_run: <run id>`; `patch.rs` never promotes it; `move_current`, `apb version use` and a start that names that version refuse it with `external_data_run` unless the start is itself flagged |
| 10 | Candidate trial and settlement | gate choice, `prepare.rs` line 1222, `candidate/settle.rs`, `candidate/quality.rs` | Excluded (start step 4) |
| 11 | Quality comparison | `run_stats/quality.rs`, `candidate/quality.rs` | Excluded through `counts_as_history` and `all_runs_of` |
| 12 | Grades | `grade/store.rs` (`runs/<id>/grades/`), the rules grader, `run_grade` | Kept in the run's own directory; never compared (11). `grade::backfill` (`grade/mod.rs` line 215) excluded through `learnable_run_dirs` |
| 13 | Run statistics | `run_stats.rs` line 361 and 700 | Excluded through `StatsRun.learns` |
| 14 | Pre-start estimates | `run_estimate/history.rs` over `runs_of` | Excluded through `learnable_run_dirs` (filter before the cap). A flagged run's own estimate reads unflagged history: kept |
| 15 | Retrospective baseline | `run_retro.rs` line 255 | Excluded as baseline for other runs through `StatsRun.learns`. The run's own `run_retro_context` and `{{run.retro}}`: kept, model-free numbers about itself |
| 16 | `run.previous_success` | `previous_success.rs` lines 95 to 112 | Excluded as a source through `learns_from`. A manifest that does not read still refuses the start (`HistoryUnreadable`, today's fail-closed rule), not a skip |
| 17 | Decisions report, labels, replay, thresholds | `decision/report/mod.rs` `RunJournal` loading, `report/replay.rs` line 170 `run_dirs` | Excluded at enumeration: the report's loader and `replay` iterate `report_dirs` (every run directory but the flagged ones, `external_data::flag::is_flagged`; not `learnable_run_dirs`, which would cap the report and drop the eval runs it reads today), so neither a label, a rate, a threshold a person sets from the report nor a replay request ever reads a flagged run; the report and the replay summary state how many they skipped (`skipped_external`) |
| 18 | Standalone decision log | `decision/standalone.rs` `<root>/.apb/decisions.jsonl` | Kept for the daily cap only: a request about a flagged run (`StandaloneDecider::for_run`) writes its line with `external_data: true`. The report reads run journals, never this log, so no reader learns from such a line |
| 19 | In-run decision cache and replay, retry advice, supervisor triage, attempt memory | `decision/mod.rs`, `scheduler/attempt_memory.rs` | Kept: run-scoped, in the run directory |
| 20 | Run memory at start | `apb_core::run_memory`, `prepare.rs` line 879, `gate/params.rs` line 127, `tools/memory.rs` `remember_at_start` | Excluded both ways (start steps 2 and 3) |
| 21 | Run memory from a step | `tools/memory.rs` line 84, `playbook_memory` `get` and `set` | Excluded: refused with `external_data_run` when the caller is a step (`StepCaller`, `APB_RUN_ID`) of a flagged run |
| 22 | History discovery | `apb_core::discover::walk::read_roots` | Excluded: `DiscoverRequest.exclude` (`apb_core::discover::Exclude`) from `flagged_markers`. A session file named after a flagged run's session id is never opened. A file whose read part names a flagged run id contributes nothing (its steps, requests and examples are dropped). Both are counted in the root report's `skipped` under the reason `external_data`. A marker past the walker's byte cap (`walk.rs` line 357) is not seen: documented limit |
| 23 | The end-of-turn save hook | `apb_core::save_nudge` (line 175 answer cache), `decision/save_nudge.rs` line 48 | Excluded the same way as 22: a session that names a flagged run id gets no nudge, no cached answer and no decision request |
| 24 | zcode desktop history | `zcode_ui_sync::wrap_step` (`scheduler/node.rs` line 2088) | Excluded: no row for a flagged run |
| 25 | Eval result store | `eval/store.rs` (`<config-dir>/evals/results/`) | Excluded: a flagged eval prints its result and writes no stored result |
| 26 | Scenario run index | `scenario/store.rs` line 225 `append`, `latest` | Excluded content: a flagged scenario run appends a line with its run id, scenario, level, time (`at_ms`, which retention orders by) and `external_data: true` only, which retention needs to prune its directory; `latest` and every reader skip such a line |
| 27 | Run search fields and the run overlay | `apb_core::run_search`, `run_outcome/overlay.rs` | Kept: computed at listing time, nothing persisted, a surface of the run itself |
| 28 | `continued_from` and the inherited baseline | `run_lineage.rs`, `baseline.rs` `inherited_from` | Kept with the successor flagged (step 1) |
| 29 | Notifications, review gates, follow-up check, report page | `notify/`, `review.rs`, `follow_up/`, `report_html/` | Kept: part of the run |
| 30 | Webhook inbox | `apb_core::connector::inbox` | Kept: an input store written before any run; a flagged run consuming a delivery stores nothing new |

### What apb cannot control

An agent CLI's own memory and session files sit outside apb: Claude
Code persists sessions under the person's home, and an agent may keep its
own notes. Codex already runs in a run-scoped `CODEX_HOME` under the run
directory (`agents/codex/home.rs`). apb's own readers skip those
sessions (paths 22 and 23); `docs/EXTERNAL-DATA.md` says plainly what
the flag does not reach and points to `process_env: allowlist` and node
`isolation` for a stricter setup. In host execution mode the host
session does the work; its tool calls name the run id, so paths 22 and
23 skip it, and the doc says the host's own memory is the host's.

### Docs

1. `docs/EXTERNAL-DATA.md` (new): the flag, the surfaces, the table
   above in plain words, the refusals, what it does not reach.
2. `docs/GUARDRAILS.md`: the three refusal codes.
3. `CLAUDE.md` and `AGENTS.md` (mirror rule): `external_data.rs` in the
   apb-engine list and a concept paragraph "External data runs".
4. Generated TypeScript: `RunDetail.external_data` and
   `RunSummary.external_data` regenerate through `ts_contract`.
