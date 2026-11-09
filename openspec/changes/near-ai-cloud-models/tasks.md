# Tasks: NEAR AI Cloud models

Work in order, one section at a time. Before a section, read its part of
`design.md` and the spec delta it names. Tick each box (`- [x]`) as soon as
it is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

For every behaviour: write the failing test first, run it and see it fail,
then write the code. A compile error is the expected first failure for a
test that names a type that does not exist yet, but it is never the
proof: each test below must fail on a real assertion before the code that
makes it pass exists.

This change is built on main a012e4ae (dsh as a built-in agent). Line
numbers in `design.md` are from that commit: find every site by its name
if it moved.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-near-ai-cloud-target TMPDIR=$HOME/.cache/apb-near-ai-cloud-tmp
mkdir -p "$TMPDIR"
```

Never put a target dir, cache or temp data inside the worktree, under
`/dev/shm` or in a shared temporary directory without a named parent.
Delete what you created under `$TMPDIR` when the work is done.

Never touch the real `~/.config/apb` or `~/.dsh`: tests that need a config
dir or a dsh home get one under `$TMPDIR` (`DSH_HOME` for dsh). Never set
or read a real `NEARAI_API_KEY`; tests use a fixed fake value and assert it
never appears in written files. Make no network call other than the one
fetch in 1.2. Never run a billed model call. Do not commit, push or open a
pull request. Do not start subagents.

During a section run only the tests of the crates it touches
(`cargo test -p <crate> --lib <module>`,
`cargo test -p <crate> --test main <module>`; the CLI package is named
`apb`), one filter per invocation. New integration test files go in
`crates/<crate>/tests/suite/` and are declared in that suite's `main.rs`.
Web: `(cd web && bun install --frozen-lockfile)` if `web/node_modules/.bin/vite`
is missing, then `bun run test` and `bun run check`.

## 0. Preconditions

- [x] 0.1 `git log --oneline -1` shows a012e4ae or a later main; note it.
- [x] 0.2 Confirm the codes: `git grep -ohE '"V2[2-9][0-9]"' crates | sort -u`
  prints nothing. If V220 to V222 are taken, use the next free codes in
  V223 to V229, update `design.md` and the specs, and say so in the final
  report.
- [x] 0.3 Confirm no module `cloud_models` exists in `crates/apb-core/src`
  and no `Command::Models` in `crates/apb-cli/src/main.rs`.
- [x] 0.4 Confirm `HostSetup::scoped_args` has exactly one caller
  (`crates/apb-engine/src/adapter/spawn_env.rs` `with_scoped_args`):
  `git grep -n 'scoped_args(' crates`. If there are more, list them and
  change each in 2.3.

## 1. The catalog (design D0, D1; spec cloud-model-catalog)

- [x] 1.1 Types. Create `crates/apb-core/src/cloud_models/mod.rs` with
  `CloudProvider`, `CloudModel`, `CloudFeatures`, `PrivacyClass` (lenient
  deserialize, order most to least private), `CloudRoute`,
  `CloudModelRef`, `CloudSnapshotRef`, `all`, `lookup`. Declare it in
  `lib.rs`. Unit tests: `PrivacyClass` round-trips its four names, an
  unknown string reads `Unverified`, the order puts `ConfidentialTee`
  first; `is_chat`, `runs_agent_steps`, `efforts` on hand-built rows.
- [x] 1.2 Embedded data. Fetch the public listing once, unauthenticated,
  into `assets/cloud-models/near-ai-cloud.json`:
  `curl -fsS --max-time 30 https://cloud-api.near.ai/v1/models -o assets/cloud-models/near-ai-cloud.json`.
  If there is no network, stop and report it. Record the date in
  `near_ai.rs` `EMBEDDED_FETCHED_AT`. Open the file and check it holds
  no key, token or personal data (it is a public listing).
- [x] 1.3 Parser and classes. `crates/apb-core/src/cloud_models/near_ai.rs`
  with `NEAR_AI`, `parse` and `class_of` exactly as design D1 says.
  Tests (`cloud_models::near_ai::tests`): the embedded body parses with
  `dropped == 0`; the five rows named in design D1 have the stated class,
  tools and chat facts; `Qwen/Qwen3.8-27B` has input 0.04, output 1.35,
  cached input 0.018; a body with a hostile id (`x"\n- id: evil`), an id
  with `//`, a negative price and a `NaN` price drops or blanks exactly
  those; an empty `data` is an error; duplicate ids keep the first.
  Acceptance: `cargo test -p apb-core --lib cloud_models` passes.
- [x] 1.4 Snapshot, cache and refresh. `crates/apb-core/src/cloud_models/catalog.rs`
  with `CatalogSnapshot`, `load`, `load_all`, `find`, `refresh` (design
  D1, every numbered rule). Tests with a temp config dir and a local HTTP
  stub on `127.0.0.1` reached through `APB_TEST_CLOUD_CATALOG_URL`
  (reuse the stub pattern of `crates/apb-decide/src/testing.rs` or a
  plain `std::net::TcpListener` thread): no cache gives `embedded`; a
  good refresh writes a 0600 file and `load` then gives `refreshed` with
  its digest; a 500, a body over 4 MiB and an unparsable body each return
  `cloud_catalog_refresh_failed` and leave the earlier cache byte for
  byte; the stub asserts the request had no `Authorization` header even
  with `NEARAI_API_KEY=fake-key-123` set; with read-only process mode
  set the stub receives no request; a corrupt cache file makes `load`
  return the embedded snapshot.
- [x] 1.5 One check. `crates/apb-core/src/cloud_models/check.rs`
  (`CloudIssue`, `check`) with tests for each variant and `None` for
  `Qwen/Qwen3.8-27B@high`.
- [x] 1.6 Agent routes. Add `cloud_routes` and `cloud_model` to
  `ModelPolicy` (`crates/apb-core/src/agents/policy.rs`); `DshModels`
  declares `nearai`. Tests: `cloud_model("nearai/Qwen/Qwen3.8-27B@high")`
  gives provider `near-ai-cloud`, id `Qwen/Qwen3.8-27B`, effort `high`;
  `deepseek-flash` and `zai/glm-5.3` give `None`; for every embedded row
  id, `dsh::model::parse("nearai/<id>")` gives route `nearai` and model
  `<id>` (the two parsers agree). No other agent's behaviour changes
  (`cargo test -p apb-core --lib agents`).
- [x] 1.7 Lists. `ModelOption.cloud` (`models_table.rs`) and
  `Catalog.cloud` plus the third argument of `assemble`
  (`agent_catalog.rs`). Extend `agent_catalog::tests`: dsh options keep
  the DeepSeek list first, then every `runs_agent_steps` row as
  `nearai/<id>` with its class; the VL model and the embedding model are
  absent; claude and codex options are unchanged; the existing tests
  still pass with an empty cloud argument.
- [x] 1.8 Model check. `ModelIssue::Cloud`, `ModelContext.cloud`, and the
  branch in `model_check::check` after `policy_issue` (design D1), plus
  the three severity callers: `validate_profile_models`, the doctor's
  `match issue` arm, and `fit/setup.rs` (`check_profile` over
  `executor_pairs()`, `model_finding` blocking from `is_blocking` for
  cloud issues). Tests in
  `crates/apb-core/tests/suite/` (new `model_check_cloud_test.rs`):
  unknown id is a non-blocking `cloud_model_unknown`; the VL model is a
  blocking `cloud_model_no_tools`; `typesafe/jev-1.13` is
  `cloud_model_not_chat`; `nearai/deepseek/deepseek-v3.2@high` is
  `cloud_effort_unsupported`; a good id gives `None` with no installed
  dsh in the context; a `model_policy` rule allowing dsh only
  `deepseek-*` reports `model_policy_violation` for a NEAR id. Fit tests
  (`crates/apb-engine` fit tests): a claude profile with a dsh tier on
  the VL model has a blocking `cloud_model_no_tools` finding; an unknown
  NEAR id is a non-blocking finding. Doctor test: an unknown NEAR id is a
  `Warn` line, the VL model a `Fail` line.
- [x] 1.9 Config. `GlobalConfig.cloud_models` (`config.rs`) with
  `deny_unknown_fields` inside; tests: absent section is the default,
  `auto_refresh: false` parses, an unknown key under it is an error.
- [x] 1.10 Surfaces. `GET /api/models` `cloud` block and `POST
  /api/models/refresh` behind the shared `local_gate` (design D1 items 3
  and 4); the dashboard refresh task (item 5); `apb models [--refresh]
  [--json]` refused in a run step (item 6); the doctor line (item 7);
  `apb validate` and `profile_write` findings (end of D1). Tests: an
  `apb-server` route test asserting the block's fields and that no value
  of a set `NEARAI_API_KEY` appears in the body; the refresh route
  answers 403 for a forwarded request (same test shape as the discover
  routes); an `apb` CLI test for `apb models --json` against a temp
  config dir with no cache (no network: no `--refresh`), and for the
  refusal with `APB_RUN_ID` set; an `apb validate` test with a dsh
  profile on an unknown NEAR id printing `warning cloud_model_unknown`;
  a `profile_write` test refusing the VL model and warning on an unknown
  id.
- [x] 1.11 Gate the section: `cargo test -p apb-core --lib`, `cargo test
  -p apb-core --test main`, `cargo test -p apb-server --lib`,
  `cargo test -p apb --test main validate`, `cargo clippy --workspace
  --all-targets -- -D warnings`.

## 2. Execution on dsh (design D2; spec cloud-model-execution)

- [x] 2.1 Manifest. `ResolvedInvocation.cloud` filled in
  `resolve_invocation`. Tests in `crates/apb-engine/src/invocation.rs`
  tests: a dsh pair on a NEAR id gets the row, a claude pair gets
  `None`; a manifest written with the field reads back equal; a manifest
  without the field (an older run) still reads.
- [x] 2.2 Start refusals. `crates/apb-engine/src/gate/cloud.rs` called
  from `gate::check_run`. Integration tests (`crates/apb-engine/tests/suite/`,
  new `cloud_gate_test.rs`) with a temp project, a fake `dsh` program on
  `PATH` that exits 0 (no real dsh), and `NEARAI_API_KEY` set or unset
  per case: unknown id refused `cloud_model_unknown` with the refresh
  hint; VL model refused `cloud_model_no_tools`; key unset refused
  `cloud_key_missing` and no run directory exists; key unset only for a
  fallback gives a start warning and the start proceeds; host execution
  mode skips the checks; `gate::check_rebind` refuses a rebind to a
  profile on the VL model written directly to disk (extend the existing
  rebind tests under `crates/apb-engine/tests/suite/`).
- [x] 2.3 Patch. Third parameter of `HostSetup::scoped_args`;
  `SpawnTarget.cloud` and `AgentTask.cloud`; `patch_body` and
  `patch_args` in `dsh/model.rs` as design D2. Unit tests: the body for
  `nearai/Qwen/Qwen3.8-27B@high` equals a golden string (both rows,
  JSON-quoted values, no price, no key value); a non-reasoning row
  writes `reasoningEfforts: false`; a missing context length leaves the
  key out; two different NEAR models give two files and neither is
  rewritten; `nearai/<id>` with `cloud: None` is a `ScopedEnvError`; the
  existing tests of `dsh::model` still pass unchanged for non-NEAR
  routes; `nearai/<id>` with `cloud: None` writes the model row alone
  (today's body), byte for byte (design D2 item 3; an earlier draft of
  this task also asked for a `ScopedEnvError` there, which contradicted
  the design and was dropped). A resume test: a run directory whose
  manifest has no `cloud` field on a `nearai/` executor resumes and its
  spawn gets the model-only patch.
- [x] 2.4 Composition check against a real dsh, skipped when `dsh` is
  not on `PATH`: write the golden patch to a temp dir and run
  `DSH_HOME=<empty temp dir> dsh --profile headless --patch <file>
  --dump-config`; assert exit 0 and that the output contains `provider:
  nearai`, `baseURL: https://cloud-api.near.ai/v1` and `apiKeyEnv:
  NEARAI_API_KEY`. This makes no model call. Mark it `#[ignore]` with a
  reason if the CI image has no dsh, and run it once locally.
- [x] 2.5 Environment. `DshSetup::model_env` for `nearai`. Extend
  `crates/apb-engine/tests/suite/agent_env_spawn_test.rs`: a profile
  with `process_env: { policy: allowlist }` on a NEAR model passes
  `NEARAI_API_KEY` to the fake program (it prints its environment names
  to a file) and the `agent_env_applied` event names it without a value.
- [x] 2.6 Scrub and redaction. The store-time scrub in `adapter.rs`
  and the read-time addition in `RunRedactor::of` (design D2 items 2 and
  3). Test: a fake agent whose reply, stderr and stream output contain
  `fake-key-1234567`, with `NEARAI_API_KEY=fake-key-1234567` in its spawn
  environment; after the run, `grep -r fake-key-1234567` over the run
  directory finds nothing; the journaled reply holds
  `[redacted:NEARAI_API_KEY]`; with the variable then set to another
  value, `run_status` and `apb runs <id> --json` still show the marker; a
  key shorter than 8 characters is not replaced and the start warns.
- [x] 2.7 Node rule. `ValidationContext.profile_chains` and `.cloud`,
  filled in `for_registry`; `crates/apb-core/src/validate/cloud_models.rs`
  with V220 and V222, both warnings. Tests in
  `validate::cloud_models::tests`: V220 (severity warning) for an
  agent_task on the VL model, for a list node and for a judge emulation
  profile; a host-mode run of that playbook passes preparation; V222 for an unknown id; no finding for a claude
  profile; no finding when the context is empty; a `scope: global`
  profile ref is resolved.
- [x] 2.8 Supervisor and probe. The supervisor spawn and `apb doctor
  --probe-agent` pass `cloud` (design D2). Extend
  `agent_env_spawn_test::the_supervisor_spawn_gets_the_run_scoped_leading_arguments`
  with a NEAR model: the patch names the route.
- [x] 2.9 Gate the section: `cargo test -p apb-core --lib`, `cargo test
  -p apb-engine --test main cloud_gate_test`, `cargo test -p apb-engine
  --test main agent_env_spawn_test`, `cargo test -p apb-engine --lib
  invocation`, clippy.

## 3. Cost (design D3; spec cloud-model-cost)

- [x] 3.1 `AgentUsage.priced_from` with a lenient decode. Tests: a usage
  JSON without the field decodes; one with an object of another shape
  reads it as `None`; a priced usage round-trips.
- [x] 3.2 `stamp_catalog_price` in `adapter.rs` at both sites. Unit
  tests: 1,000,000 input and 100,000 output tokens on `Qwen/Qwen3.8-27B`
  cost 0.175; cache reads use the cached price, else the input price; a
  printed cost is kept and `priced_from` stays `None`; a row without an
  output price leaves the cost `None`.
- [x] 3.3 Basis. The cloud step in `cost_basis::resolve`. Tests: key
  present gives `Billed` with evidence `cloud:near-ai-cloud:NEARAI_API_KEY`;
  key absent falls through to the module and declared subscriptions as
  before; `deepseek-flash` is unaffected.
- [x] 3.4 End to end. An engine integration test with a fake dsh program
  that prints the dsh `--json` step events with a known `usage` (copy the
  shape from `crates/apb-core/src/agents/dsh/output.rs` tests): the
  journal's `attempt_finished.usage` has the computed `cost_usd`,
  `cost_basis: billed` and `priced_from`; `budget::spent` counts it; a
  run with `budget: { usd: 0.10 }` and a second node pauses on the
  budget gate when the first node's priced cost leaves too little;
  `apb stats` shows the spend.
- [x] 3.5 Gate the section: `cargo test -p apb-engine --lib budget`,
  `cargo test -p apb-engine --lib run_estimate`, the new integration
  test, clippy.

## 4. Privacy class (design D4; spec cloud-model-privacy)

- [x] 4.1 Resolved block. `resolved_chain` in `profile_tools.rs` and the
  `resolved` key of `profile_get`; `profile_list` gets `least_private`
  and `uniform`. Tests (`crates/apb-mcp/tests/suite/`): an all-`nearai`
  chain is uniform `confidential_tee`; a NEAR executor with an incognito
  fallback is not uniform, `least_private: incognito`, warning
  `profile_privacy_class_changes`; a chain with a claude tier has `null`
  in `classes` and `least_private: null`; tiers appear with their names.
  The HTTP route `GET /api/profiles/{name}` returns the same block (an
  `apb-server` test).
- [x] 4.2 Warnings. `profile_privacy_class_changes` in `profile_write`'s
  warnings and in `apb validate`'s profile lines; V221 in
  `validate/cloud_models.rs`. Tests as the scenarios of the spec.
- [x] 4.3 Attempt record. `AttemptStarted.cloud` written from the
  attempt's invocation. Tests: an old `attempt_started` line without the
  field decodes (extend the event decode tests in `event.rs`); a run
  whose executor is a NEAR `nearai` model and whose fallback is an
  incognito NEAR model, with a fake dsh that fails the first attempt with
  a quota error (the dsh error shape `apb_core::agents::dsh::output`
  classifies as quota), journals two attempts with their classes; the
  run view marks the second `privacy_class_left: true`; `apb runs <id>`
  prints it.
- [x] 4.4 Gate the section: `cargo test -p apb-mcp --test main`, `cargo
  test -p apb-engine --lib event`, the run-view test, clippy.

## 5. Decision provider (design D5; spec cloud-decision-provider)

- [x] 5.1 `catalog` on `ProviderDoc` and `ProviderSpec`, the defaults and
  the errors in `load_file`, `ProviderSpec.cloud` (the whole row). Tests in
  `decisions::tests`: the minimal provider of the spec loads with the
  NEAR base URL and `KeyRef::Env("NEARAI_API_KEY")`; `catalog` on `kind:
  systemone` is an error; an unknown catalog, a model the catalog lacks
  and `typesafe/jev-1.13` are errors naming the provider; the
  structured-output default follows the row's features; an explicit
  `json_schema` on a row without `structured_outputs` is an error.
- [x] 5.2 Price and class in the runner. `decision/mod.rs` pricing from
  the provider's row; `DecisionMade.privacy_class`. Tests with the
  `apb-decide` `testing` HTTP stub as the provider's base URL: 2,000
  input and 50 output tokens cost 0.0001475 with `cost_estimated: true`
  and `privacy_class: confidential_tee`; an old `decision_made` line
  without the field decodes; the TypeSafe list price still applies to
  `typesafe/jev-1.13` on `kind: systemone`; a run whose manifest holds
  the provider's row prices a decision after a catalog refresh with the
  old prices (resume test with a changed cache).
- [x] 5.3 Report and doctor show the class next to the provider; the
  doctor's probe of a `catalog` provider is a keyless `GET` (new branch
  in `decisions.rs` `probe`). Tests: the `decision/report` tests; a
  probe test against a local stub with `NEARAI_API_KEY=fake-key-1234567`
  set asserts no `Authorization` header arrives.
- [x] 5.4 Gate the section: `cargo test -p apb-core --lib decisions`,
  `cargo test -p apb-engine --lib decision`, clippy.

## 6. Docs, dashboard and the whole gate (design D6)

- [x] 6.1 `docs/CLOUD-MODELS.md`, `docs/AGENT-SETUP.md`, `docs/PROFILES.md`,
  `docs/DECISIONS.md` as design D6. No em dashes, no exclamation marks,
  no CJK, no personal machine settings.
- [x] 6.2 Decision record `docs/adr/0014-cloud-provider-knowledge-lives-in-its-catalog-module.md`
  (front matter and sections like 0013, "Checked by" naming the tests of
  1.6 and 2.3) and its line in `docs/adr/README.md`.
- [x] 6.3 `CLAUDE.md` and `AGENTS.md`, the same text in both: the
  `cloud_models` module in the apb-core paragraph and a "Cloud models"
  concept paragraph.
- [x] 6.4 Web: types in `web/src/lib/api/core.ts`, options and the
  resolved chain in `web/src/lib/profileedit.ts`, the editor in
  `web/src/pages/ProfileEdit.svelte`. Tests in `profileedit.test.ts`:
  cloud options keep their class and render after the DeepSeek list; the
  resolved panel lists executor, fallbacks and tiers and shows the
  warning for a mixed chain. `bun run test`, `bun run check`,
  `bun run build`. Plain CSS over the existing variables only.
- [x] 6.5 Generated TypeScript: `APB_WRITE_TS=1 cargo test -p apb-server
  --lib ts_contract`, then the test without the variable passes.
- [x] 6.6 Whole gate: `cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace`
  (or `cargo nextest run --workspace` when installed),
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (fix every violation after reading `code-ranker docs base <ID>`), and
  `scripts/third-party-notices.sh --check` (no dependency is added; it
  must stay clean).
- [x] 6.7 Final report: the codes used, the files changed, any open
  question you met and the default you kept, and the manual step left
  for a person (7.1).

## 7. Manual verification by a person (not for the coding agent)

- [ ] 7.1 With a real `NEARAI_API_KEY` and dsh 0.2.2-rc.1 or later, run a
  one-node playbook on `nearai/Qwen/Qwen3.8-27B@low` and one on
  `nearai/z-ai/glm-5.3-flash`: the step succeeds, the cost is priced,
  `attempt_started.cloud` carries the class. This is billed and needs the
  owner's go-ahead. Not done by the coding agent: not approved.

## Open questions (with the default this change uses)

1. Should the key variable be configurable? Default: no, it is fixed to
   `NEARAI_API_KEY` in `near_ai.rs`; a later change can add
   `cloud_models.near_ai_cloud.key_env` if someone needs another name.
2. Which reasoning efforts does NEAR accept per model? The catalog only
   says `reasoning`. Default: `low`, `medium`, `high` for a model with
   `reasoning`, spelled the same on the wire, and none otherwise; no
   `compat.thinkingFormat` is written. 7.1 checks it on two models.
3. Can `typesafe/jev-1.13` be reached on NEAR through a `/v1/systemone`
   route? Its output modality is `decisions` and NEAR documents no such
   path. Default: not supported; `catalog: near-ai-cloud` refuses it.
4. What does `is_ready: false` in the listing mean (27 rows on
   2026-11-09, including models that answer)? Default: ignored; the row
   is listed and offered by its features alone.
5. Is the gateway owner list for `incognito` complete? Default: the seven
   owners the listing shows with provider-prefixed ids on 2026-11-09; any
   new owner reads `unverified` until the list in `near_ai.rs` is
   extended by a pull request.
