# Cloud model catalogs

apb knows the public model catalog of one cloud gateway: NEAR AI Cloud
(`https://cloud-api.near.ai/v1`), an OpenAI-compatible gateway whose models
run in a confidential enclave (TEE) or are relayed to a model vendor. This
page covers the catalog, its privacy classes, how a profile names a NEAR
model, what a run does with it, and the decisions provider on top of it.
The code lives in `apb_core::cloud_models` (decision record 0014).

## The catalog

The catalog is the parsed body of the public, unauthenticated
`GET https://cloud-api.near.ai/v1/models`. apb ships an embedded copy in
the binary (`assets/cloud-models/near-ai-cloud.json`, with its fetch date)
and uses a refreshed copy from its config directory when one reads and
parses (`<config_dir>/state/cloud-models/near-ai-cloud.json`, private,
written atomically). A copy that does not read is ignored and the embedded
one is used; `apb doctor` says so.

Only three things fetch the listing:

1. `apb models --refresh` (a person's command, refused inside a run step);
2. `POST /api/models/refresh` on the dashboard (the "Refresh catalog"
   button of the profile editor), answered only for a client on this
   machine;
3. the dashboard's own refresh, at start and then once a day, unless
   `cloud_models.near_ai_cloud.auto_refresh: false` is set in the global
   `config.yaml`.

A refresh sends no credential at all (the key is never read for it), is
refused in a read-only process (`apb mcp --read-only`), follows no
redirect, gives up after 10 seconds and refuses a body over 4 MiB. A
failed refresh (`cloud_catalog_refresh_failed`, with the HTTP status or
the parser's reason, never the body) leaves the copy in use as it was.
Detection, validation, profile writes and runs never touch the network for
the catalog.

`apb models [--json]` prints each catalog's source, age and rows (id,
class, tool calling, prices). `GET /api/models` carries a `cloud` block per
provider: source, fetch time, digest, the key variable by name and whether
it is set for the server (never its value), and every row.

A row is checked before it is kept: an id with a character outside letters,
digits and `._:/-`, or with an empty path segment, is dropped and counted
(`dropped`); a price that is not a finite number at least zero reads as
absent; duplicate ids keep the first row.

## Privacy classes

Every row carries a privacy class from its owner:

| Class | Owner | Meaning |
|---|---|---|
| `confidential_tee` | `nearai` | NEAR AI's own confidential (TEE) fleet |
| `third_party_tee` | `attested 3p` | a third party's attested enclave |
| `incognito` | `anthropic`, `openai`, `google`, `x-ai`, `deepseek`, `qwen`, `moonshotai` | relayed to the vendor under NEAR AI's account, not private |
| `unverified` | any other owner | nothing known about where it runs |

The class is shown in `/api/models` (rows and options), in a profile's
`resolved` block (`GET /api/profiles/{name}`, MCP `profile_get`; MCP
`profile_list` shows `least_private` and `uniform`), on every attempt
(`attempt_started.cloud`) and on every run surface. A chain whose executor,
fallbacks and tiers do not share one class gets the warning
`profile_privacy_class_changes` (profile write, `apb validate`, the
resolved block) and V221 on each node bound to it. An attempt that ran on
another class than its profile executor's (a fallback or a tier on another
class, or on a model outside any catalog) is marked `privacy_class_left` on
`run_status`, `GET /api/runs/{id}` and `apb runs <id>`.

The class is data and warnings only: no gate refuses a chain for its class.

## Naming a NEAR model in a profile

Only dsh reaches NEAR AI Cloud in this release. A dsh profile names a
catalog model as `nearai/<catalog id>[@<effort>]`, for example
`nearai/Qwen/Qwen3.8-27B@high`. The profile editor offers every catalog row
that is a chat model with tool calling, after dsh's DeepSeek models, with
its class. A model that takes reasoning takes the efforts `low`, `medium`
and `high`; any other model takes none.

Every authoring surface (`apb validate`, `profile_write`, `apb doctor`,
the fit and adoption reports) judges a NEAR model through one check, after
the person's `model_policy`, which keeps the last word:

| Finding | When | Severity |
|---|---|---|
| `cloud_model_unknown` | the catalog does not list the id | warning (a newer model is possible) |
| `cloud_model_no_tools` | a chat model without tool calling | error: `profile_write` refuses it |
| `cloud_model_not_chat` | not text in and text out | error |
| `cloud_effort_unsupported` | an effort the row does not take | error |

On a node, V220 (warning) names a chain that reaches a model an agent step
cannot run on, and V222 (warning) one the catalog does not list. They stay
warnings because a run in host mode spawns no dsh.

## What a run does

1. **Start.** In cli execution mode a start is refused before any run
   directory is written when a chain names a cloud model the catalog lacks
   (`cloud_model_unknown`, naming `apb models --refresh`), one an agent
   step cannot run on (`cloud_model_no_tools`, `cloud_model_not_chat`,
   `cloud_effort_unsupported`), or when the primary executor's key variable
   does not reach its spawn (`cloud_key_missing`, naming the variable). A
   fallback's or a tier's missing key is a start warning. A mid-run rebind
   to such a profile is refused the same way. Host mode skips the check.
2. **The manifest keeps the row.** The run snapshots the model's catalog
   row (prices, class, context, output cap, catalog digest and fetch time)
   into its write-once manifest. Every later read uses it, a resume
   included, so a refresh during a run changes nothing for it.
3. **The patch carries the provider.** dsh's run-scoped `--patch` file
   (decision record 0013) holds, next to the `agent-default-model` row, an
   `llm-pi-ai` row with one route `nearai`: `api: openai-completions`, the
   base URL, `apiKeyEnv: NEARAI_API_KEY` and the one model with its
   context, output cap and efforts. No price, no key value, no setting of
   yours. A `--patch` overlay replaces the whole `config` of the entry it
   names, so in a spawn on a NEAR model your own `llm-pi-ai` routes are not
   mounted; other spawns keep them. A run an older apb started has no row
   in its manifest and gets the model row alone, as before.
4. **The key.** Only the name `NEARAI_API_KEY` is ever written (patch,
   manifest, journal, cache). The value reaches dsh through the spawn
   environment and is kept under `process_env: allowlist`. Whatever the
   agent prints of it is replaced with `[redacted:NEARAI_API_KEY]` in the
   reply, the failure text, the stream log, the attempt's transcript and
   its status file before apb stores or reads any of it (the logs as the
   agent prints, so a killed driver leaves no plain value), and the run
   surfaces redact the current value on read too. A key variable an
   installed connector references never reaches an agent, so such a
   start is refused `cloud_key_missing`; the check covers the
   supervisor's profile and an ephemeral executor override too. A value shorter than 8 characters is not replaced
   (the start warns). The failure classifier ignores the marker, so an
   echoed key never reads as an auth failure.
5. **Cost.** dsh prints no cost. An attempt on a catalog model that
   reports tokens is priced from the run's row: uncached input and cache
   writes at the input price, cache reads at the cached-input price (else
   the input price), output at the output price, per million tokens. The
   usage carries `priced_from` (catalog, model, digest, fetch time), and
   the run surfaces say "priced from NEAR AI Cloud catalog <date>". A cost
   the agent printed is never replaced; a row without prices leaves the
   cost absent. Run budgets, estimates and `apb stats` count it like a
   printed cost. With the key reaching the process the cost basis is
   `billed`, so the `billing:` section's `billed` policy (default `ask`)
   decides the start.

## Decisions provider

A `decisions.yaml` provider of `kind: llm_emulation` may name the catalog:

```yaml
providers:
  - { id: near, kind: llm_emulation, via: openai_compatible, catalog: near-ai-cloud, model: Qwen/Qwen3.8-27B }
```

apb then defaults `base_url` to `https://cloud-api.near.ai/v1`, the key to
`{{env.NEARAI_API_KEY}}`, and `structured_output` to `json_schema` when the
row has structured outputs (else `prompt_only`). Loading fails, naming the
provider, when `catalog` is set on another kind, names an unknown catalog,
or names a model the catalog lacks or that is not a chat model
(`typesafe/jev-1.13` is a decision model with output `decisions` and is
refused here), and when `json_schema` is asked of a row without structured
outputs. The row is kept with the run's decisions, so a decision asked
after a refresh is priced and classed from the row the run started with.
Each decision is priced from it (input and output tokens at list price,
`cost_estimated: true`) and carries `decision_made.privacy_class`; `apb
decisions report` and the doctor's decisions line show the class next to
the provider. The doctor's free check of such a provider is a plain `GET
/models` with no key, whether the variable is set or not.

## Later

Run-level attestation evidence (a report fetched per TEE model a run used,
kept by digest on the report page) is a later change. A report proves the
model's enclave at fetch time, not any particular response.
