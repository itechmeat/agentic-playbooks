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

- [ ] 0.1 `git log --oneline -1` shows a012e4ae or a later main; note it.
- [ ] 0.2 Confirm the codes: `git grep -ohE '"V2[2-9][0-9]"' crates | sort -u`
  prints nothing. If V220 to V222 are taken, use the next free codes in
  V223 to V229, update `design.md` and the specs, and say so in the final
  report.
- [ ] 0.3 Confirm no module `cloud_models` exists in `crates/apb-core/src`
  and no `Command::Models` in `crates/apb-cli/src/main.rs`.
- [ ] 0.4 Confirm `HostSetup::scoped_args` has exactly one caller
  (`crates/apb-engine/src/adapter/spawn_env.rs` `with_scoped_args`):
  `git grep -n 'scoped_args(' crates`. If there are more, list them and
  change each in 2.3.

## 1. The catalog (design D0, D1; spec cloud-model-catalog)

- [ ] 1.1 Types. Create `crates/apb-core/src/cloud_models/mod.rs` with
  `CloudProvider`, `CloudModel`, `CloudFeatures`, `PrivacyClass` (lenient
  deserialize, order most to least private), `CloudRoute`,
  `CloudModelRef`, `CloudSnapshotRef`, `all`, `lookup`. Declare it in
  `lib.rs`. Unit tests: `PrivacyClass` round-trips its four names, an
  unknown string reads `Unverified`, the order puts `ConfidentialTee`
  first; `is_chat`, `runs_agent_steps`, `efforts` on hand-built rows.
- [ ] 1.2 Embedded data. Fetch the public listing once, unauthenticated,
  into `assets/cloud-models/near-ai-cloud.json`:
  `curl -fsS --max-time 30 https://cloud-api.near.ai/v1/models -o assets/cloud-models/near-ai-cloud.json`.
  If there is no network, stop and report it. Record the date in
  `near_ai.rs` `EMBEDDED_FETCHED_AT`. Open the file and check it holds
  no key, token or personal data (it is a public listing).
- [ ] 1.3 Parser and classes. `crates/apb-core/src/cloud_models/near_ai.rs`
  with `NEAR_AI`, `parse` and `class_of` exactly as design D1 says.
  Tests (`cloud_models::near_ai::tests`): the embedded body parses with
  `dropped == 0`; the five rows named in design D1 have the stated class,
  tools and chat facts; `Qwen/Qwen3.8-27B` has input 0.04, output 1.35,
  cached input 0.018; a body with a hostile id (`x"\n- id: evil`), an id
  with `//`, a negative price and a `NaN` price drops or blanks exactly
  those; an empty `data` is an error; duplicate ids keep the first.
  Acceptance: `cargo test -p apb-core --lib cloud_models` passes.
- [ ] 1.4 Snapshot, cache and refresh. `crates/apb-core/src/cloud_models/catalog.rs`
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
- [ ] 1.5 One check. `crates/apb-core/src/cloud_models/check.rs`
  (`CloudIssue`, `check`) with tests for each variant and `None` for
  `Qwen/Qwen3.8-27B@high`.
- [ ] 1.6 Agent routes. Add `cloud_routes` and `cloud_model` to
  `ModelPolicy` (`crates/apb-core/src/agents/policy.rs`); `DshModels`
  declares `nearai`. Tests: `cloud_model("nearai/Qwen/Qwen3.8-27B@high")`
  gives provider `near-ai-cloud`, id `Qwen/Qwen3.8-27B`, effort `high`;
  `deepseek-flash` and `zai/glm-5.3` give `None`; for every embedded row
  id, `dsh::model::parse("nearai/<id>")` gives route `nearai` and model
  `<id>` (the two parsers agree). No other agent's behaviour changes
  (`cargo test -p apb-core --lib agents`).
- [ ] 1.7 Lists. `ModelOption.cloud` (`models_table.rs`) and
  `Catalog.cloud` plus the third argument of `assemble`
  (`agent_catalog.rs`). Extend `agent_catalog::tests`: dsh options keep
  the DeepSeek list first, then every `runs_agent_steps` row as
  `nearai/<id>` with its class; the VL model and the embedding model are
  absent; claude and codex options are unchanged; the existing tests
  still pass with an empty cloud argument.
- [ ] 1.8 Model check. `ModelIssue::Cloud`, `ModelContext.cloud`, and the
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
- [ ] 1.9 Config. `GlobalConfig.cloud_models` (`config.rs`) with
  `deny_unknown_fields` inside; tests: absent section is the default,
  `auto_refresh: false` parses, an unknown key under it is an error.
- [ ] 1.10 Surfaces. `GET /api/models` `cloud` block and `POST
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
- [ ] 1.11 Gate the section: `cargo test -p apb-core --lib`, `cargo test
  -p apb-core --test main`, `cargo test -p apb-server --lib`,
  `cargo test -p apb --test main validate`, `cargo clippy --workspace
  --all-targets -- -D warnings`.

## 2. Execution on dsh (design D2; spec cloud-model-execution)

- [ ] 2.1 Manifest. `ResolvedInvocation.cloud` filled in
  `resolve_invocation`. Tests in `crates/apb-engine/src/invocation.rs`
  tests: a dsh pair on a NEAR id gets the row, a claude pair gets
  `None`; a manifest written with the field reads back equal; a manifest
  without the field (an older run) still reads.
- [ ] 2.2 Start refusals. `crates/apb-engine/src/gate/cloud.rs` called
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
- [ ] 2.3 Patch. Third parameter of `HostSetup::scoped_args`;
  `SpawnTarget.cloud` and `AgentTask.cloud`; `patch_body` and
  `patch_args` in `dsh/model.rs` as design D2. Unit tests: the body for
  `nearai/Qwen/Qwen3.8-27B@high` equals a golden string (both rows,
  JSON-quoted values, no price, no key value); a non-reasoning row
  writes `reasoningEfforts: false`; a missing context length leaves the
  key out; two different NEAR models give two files and neither is
  rewritten; `nearai/<id>` with `cloud: None` is a `ScopedEnvError`; the
  existing tests of `dsh::model` still pass unchanged for non-NEAR
  routes; `nearai/<id>` with `cloud: None` writes the model row alone
  (today's body), byte for byte. A resume test: a run directory whose
  manifest has no `cloud` field on a `nearai/` executor resumes and its
  spawn gets the model-only patch.
- [ ] 2.4 Composition check against a real dsh, skipped when `dsh` is
  not on `PATH`: write the golden patch to a temp dir and run
  `DSH_HOME=<empty temp dir> dsh --profile headless --patch <file>
  --dump-config`; assert exit 0 and that the output contains `provider:
  nearai`, `baseURL: https://cloud-api.near.ai/v1` and `apiKeyEnv:
  NEARAI_API_KEY`. This makes no model call. Mark it `#[ignore]` with a
  reason if the CI image has no dsh, and run it once locally.
- [ ] 2.5 Environment. `DshSetup::model_env` for `nearai`. Extend
  `crates/apb-engine/tests/suite/agent_env_spawn_test.rs`: a profile
  with `process_env: { policy: allowlist }` on a NEAR model passes
  `NEARAI_API_KEY` to the fake program (it prints its environment names
  to a file) and the `agent_env_applied` event names it without a value.
- [ ] 2.6 Scrub and redaction. The store-time scrub in `adapter.rs`
  and the read-time addition in `RunRedactor::of` (design D2 items 2 and
  3). Test: a fake agent whose reply, stderr and stream output contain
  `fake-key-1234567`, with `NEARAI_API_KEY=fake-key-1234567` in its spawn
  environment; after the run, `grep -r fake-key-1234567` over the run
  directory finds nothing; the journaled reply holds
  `[redacted:NEARAI_API_KEY]`; with the variable then set to another
  value, `run_status` and `apb runs <id> --json` still show the marker; a
  key shorter than 8 characters is not replaced and the start warns.
- [ ] 2.7 Node rule. `ValidationContext.profile_chains` and `.cloud`,
  filled in `for_registry`; `crates/apb-core/src/validate/cloud_models.rs`
  with V220 and V222, both warnings. Tests in
  `validate::cloud_models::tests`: V220 (severity warning) for an
  agent_task on the VL model, for a list node and for a judge emulation
  profile; a host-mode run of that playbook passes preparation; V222 for an unknown id; no finding for a claude
  profile; no finding when the context is empty; a `scope: global`
  profile ref is resolved.
- [ ] 2.8 Supervisor and probe. The supervisor spawn and `apb doctor
  --probe-agent` pass `cloud` (design D2). Extend
  `agent_env_spawn_test::the_supervisor_spawn_gets_the_run_scoped_leading_arguments`
  with a NEAR model: the patch names the route.
- [ ] 2.9 Gate the section: `cargo test -p apb-core --lib`, `cargo test
  -p apb-engine --test main cloud_gate_test`, `cargo test -p apb-engine
  --test main agent_env_spawn_test`, `cargo test -p apb-engine --lib
  invocation`, clippy.

## 3. Cost (design D3; spec cloud-model-cost)

- [ ] 3.1 `AgentUsage.priced_from` with a lenient decode. Tests: a usage
  JSON without the field decodes; one with an object of another shape
  reads it as `None`; a priced usage round-trips.
- [ ] 3.2 `stamp_catalog_price` in `adapter.rs` at both sites. Unit
  tests: 1,000,000 input and 100,000 output tokens on `Qwen/Qwen3.8-27B`
  cost 0.175; cache reads use the cached price, else the input price; a
  printed cost is kept and `priced_from` stays `None`; a row without an
  output price leaves the cost `None`.
- [ ] 3.3 Basis. The cloud step in `cost_basis::resolve`. Tests: key
  present gives `Billed` with evidence `cloud:near-ai-cloud:NEARAI_API_KEY`;
  key absent falls through to the module and declared subscriptions as
  before; `deepseek-flash` is unaffected.
- [ ] 3.4 End to end. An engine integration test with a fake dsh program
  that prints the dsh `--json` step events with a known `usage` (copy the
  shape from `crates/apb-core/src/agents/dsh/output.rs` tests): the
  journal's `attempt_finished.usage` has the computed `cost_usd`,
  `cost_basis: billed` and `priced_from`; `budget::spent` counts it; a
  run with `budget: { usd: 0.10 }` and a second node pauses on the
  budget gate when the first node's priced cost leaves too little;
  `apb stats` shows the spend.
- [ ] 3.5 Gate the section: `cargo test -p apb-engine --lib budget`,
  `cargo test -p apb-engine --lib run_estimate`, the new integration
  test, clippy.

## 4. Privacy class (design D4; spec cloud-model-privacy)

- [ ] 4.1 Resolved block. `resolved_chain` in `profile_tools.rs` and the
  `resolved` key of `profile_get`; `profile_list` gets `least_private`
  and `uniform`. Tests (`crates/apb-mcp/tests/suite/`): an all-`nearai`
  chain is uniform `confidential_tee`; a NEAR executor with an incognito
  fallback is not uniform, `least_private: incognito`, warning
  `profile_privacy_class_changes`; a chain with a claude tier has `null`
  in `classes` and `least_private: null`; tiers appear with their names.
  The HTTP route `GET /api/profiles/{name}` returns the same block (an
  `apb-server` test).
- [ ] 4.2 Warnings. `profile_privacy_class_changes` in `profile_write`'s
  warnings and in `apb validate`'s profile lines; V221 in
  `validate/cloud_models.rs`. Tests as the scenarios of the spec.
- [ ] 4.3 Attempt record. `AttemptStarted.cloud` written from the
  attempt's invocation. Tests: an old `attempt_started` line without the
  field decodes (extend the event decode tests in `event.rs`); a run
  whose executor is a NEAR `nearai` model and whose fallback is an
  incognito NEAR model, with a fake dsh that fails the first attempt with
  a quota error (the dsh error shape `apb_core::agents::dsh::output`
  classifies as quota), journals two attempts with their classes; the
  run view marks the second `privacy_class_left: true`; `apb runs <id>`
  prints it.
- [ ] 4.4 Gate the section: `cargo test -p apb-mcp --test main`, `cargo
  test -p apb-engine --lib event`, the run-view test, clippy.

## 5. Decision provider (design D5; spec cloud-decision-provider)

- [ ] 5.1 `catalog` on `ProviderDoc` and `ProviderSpec`, the defaults and
  the errors in `load_file`, `ProviderSpec.cloud` (the whole row). Tests in
  `decisions::tests`: the minimal provider of the spec loads with the
  NEAR base URL and `KeyRef::Env("NEARAI_API_KEY")`; `catalog` on `kind:
  systemone` is an error; an unknown catalog, a model the catalog lacks
  and `typesafe/jev-1.13` are errors naming the provider; the
  structured-output default follows the row's features; an explicit
  `json_schema` on a row without `structured_outputs` is an error.
- [ ] 5.2 Price and class in the runner. `decision/mod.rs` pricing from
  the provider's row; `DecisionMade.privacy_class`. Tests with the
  `apb-decide` `testing` HTTP stub as the provider's base URL: 2,000
  input and 50 output tokens cost 0.0001475 with `cost_estimated: true`
  and `privacy_class: confidential_tee`; an old `decision_made` line
  without the field decodes; the TypeSafe list price still applies to
  `typesafe/jev-1.13` on `kind: systemone`; a run whose manifest holds
  the provider's row prices a decision after a catalog refresh with the
  old prices (resume test with a changed cache).
- [ ] 5.3 Report and doctor show the class next to the provider; the
  doctor's probe of a `catalog` provider is a keyless `GET` (new branch
  in `decisions.rs` `probe`). Tests: the `decision/report` tests; a
  probe test against a local stub with `NEARAI_API_KEY=fake-key-1234567`
  set asserts no `Authorization` header arrives.
- [ ] 5.4 Gate the section: `cargo test -p apb-core --lib decisions`,
  `cargo test -p apb-engine --lib decision`, clippy.

## 6. Docs, dashboard and the whole gate (design D6)

- [ ] 6.1 `docs/CLOUD-MODELS.md`, `docs/AGENT-SETUP.md`, `docs/PROFILES.md`,
  `docs/DECISIONS.md` as design D6. No em dashes, no exclamation marks,
  no CJK, no personal machine settings.
- [ ] 6.2 Decision record `docs/adr/0014-cloud-provider-knowledge-lives-in-its-catalog-module.md`
  (front matter and sections like 0013, "Checked by" naming the tests of
  1.6 and 2.3) and its line in `docs/adr/README.md`.
- [ ] 6.3 `CLAUDE.md` and `AGENTS.md`, the same text in both: the
  `cloud_models` module in the apb-core paragraph and a "Cloud models"
  concept paragraph.
- [ ] 6.4 Web: types in `web/src/lib/api/core.ts`, options and the
  resolved chain in `web/src/lib/profileedit.ts`, the editor in
  `web/src/pages/ProfileEdit.svelte`. Tests in `profileedit.test.ts`:
  cloud options keep their class and render after the DeepSeek list; the
  resolved panel lists executor, fallbacks and tiers and shows the
  warning for a mixed chain. `bun run test`, `bun run check`,
  `bun run build`. Plain CSS over the existing variables only.
- [ ] 6.5 Generated TypeScript: `APB_WRITE_TS=1 cargo test -p apb-server
  --lib ts_contract`, then the test without the variable passes.
- [ ] 6.6 Whole gate: `cargo fmt --all -- --check`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test --workspace`
  (or `cargo nextest run --workspace` when installed),
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (fix every violation after reading `code-ranker docs base <ID>`), and
  `scripts/third-party-notices.sh --check` (no dependency is added; it
  must stay clean).
- [ ] 6.7 Final report: the codes used, the files changed, any open
  question you met and the default you kept, and the manual step left
  for a person (7.1).

## 7. Manual verification by a person (not for the coding agent)

- [ ] 7.1 With a real `NEARAI_API_KEY` and dsh 0.2.2-rc.1 or later, run a
  one-node playbook on `nearai/Qwen/Qwen3.8-27B@low` and one on
  `nearai/z-ai/glm-5.3-flash`: the step succeeds, the cost is priced,
  `attempt_started.cloud` carries the class. This is billed and needs the
  owner's go-ahead.

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

## 8. External data runs (design D8; spec run-external-data)

Independent of sections 1 to 5: it may be built before or after them.
The same environment, test-first and gate rules apply. Every refusal test
asserts the code and that nothing was written (no run directory, the store
file byte-identical). Every test that could touch a store under the home
directory (zcode's desktop index, Claude Code sessions) sets `HOME` to a
directory under `$TMPDIR` for the process it runs.

- [x] 8.1 Preconditions: `git grep -n 'external_data' crates web/src`
  prints nothing; `git grep -n 'RunOrigin::\|is_production()' crates`
  lists the readers D8 names. Any reader not named there is routed
  through `learns_from` (history) or `runs_follow_up` (its own follow-up)
  in 8.3 and listed in the final report.
- [x] 8.2 Manifest and journal. Tests in `manifest.rs`: an unflagged
  manifest serializes byte-identical to today's; a flagged production run
  writes `external_data: true`, `origin: { kind: external_data }` and the
  mode suffix `+external`; a flagged scenario run keeps `kind: scenario`;
  `base_mode` strips the suffix (and `+guard` with it); `is_empty` is
  false for a flagged manifest with no profiles; `amend` through
  `add_secret_params` keeps all three; `ExternalData.is_production()` is
  false and `runs_follow_up()` true; a fixture manifest with an unknown
  origin kind reads as `Unknown`; `execution_mode()` of a fixture whose
  mode carries an unknown suffix is refused (the downgrade guard: the
  same code path an older build runs). Test in `event.rs`: `run_started`
  without the field decodes as false and an unflagged one serializes
  without it.
- [x] 8.3 `crates/apb-engine/src/external_data.rs` with `is_flagged`,
  `learns_from`, `refuse_if_flagged`, `for_start`, `learnable_run_dirs`,
  `flagged_markers`; `StatsRun.learns`; every reader of D8 "Field home"
  item 6.2 switched to `learns_from`. Unit tests: each source alone
  (boolean only, origin only, mode suffix only, journal field only)
  flags, and for each such fixture `counts_as_history`,
  `production_dir` and the `previous_success` lookup skip the run (not
  only `is_flagged`); a manifest that does not read gives `learns_from`
  false and `refuse_if_flagged` an error; `learnable_run_dirs` over two
  unflagged runs followed by `cap + 10` flagged runs returns the two; a
  flagged run's follow-up is scheduled (extend the follow-up test in
  `follow_up/mod.rs`) and absent from `run_stats/follow_up`.
- [x] 8.4 Start: `RunOptions.external_data`; `for_start` on every surface
  before its remember handling; the empty recall arm in `prepare.rs` and
  in `gate/params.rs`; the child line in `scheduler/node.rs`; the
  `__drive-supervised` argv; `candidate_trial` and settlement guards.
  Integration tests in `crates/apb-engine/tests/suite/external_data_test.rs`
  (declared in `main.rs`): a flagged start ignores a remembered value
  (`run_provenance.remembered` empty) and a required param with only a
  remembered value is refused `param_needs_value` by the gate, as
  preparation would leave it empty; a child of a flagged run and a child
  of a flagged scenario run are flagged; a `continued_from` successor is
  flagged and an explicit `false` is refused `external_data_immutable`;
  a flagged start with a waiting candidate runs `current` with
  `candidate_skipped: external_data`; a flagged start naming the
  candidate version leaves `candidate_trial` false and the trial record
  byte-identical; a resume of a flagged run with `false` and of an
  unflagged run with `true` are both refused with the journal length
  unchanged, and a resume repeating the run's value is accepted; the
  two-run journal equality of the spec's last scenario on the fake agent.
- [x] 8.5 Learning stores, one failing test each before the guard, in the
  same suite file: known problems `observe` and `scan` leave the store
  absent or byte-identical; corrections leave the ledger unchanged for a
  flagged production run and a flagged scenario run; a cache-eligible
  node of a flagged run adds no file under `.apb/cache` and a later
  unflagged run misses; `forward_patch::create`,
  `judge_eval::example::save_from_run`, `run_values` and scenario capture
  refuse `external_data_run`; an in-run patch of a flagged run under
  `promote_supervisor_patches` leaves `current` unchanged, its version
  provenance names the run, and `move_current`, `apb version use` and an
  unflagged start naming it refuse `external_data_run`; the decisions
  report loader and `replay` over one flagged and one unflagged run read
  only the unflagged one (assert replay sends no request for the flagged
  run's records with the decide stub) and the report counts one skipped;
  a standalone decision about a flagged run writes a log line with
  `external_data: true` that the report skips; `zcode_ui_sync` with
  `HOME` under `$TMPDIR` inserts no row for a flagged run into that
  home's index; a flagged eval with a temp config dir writes nothing
  under `evals/results`; a flagged scenario run's index line holds only
  run id, scenario, level and the flag, `latest` skips it, and
  retention still removes its directory past `KEEP_RUNS`.
  Tests that need another suite's fixtures (the in-run patch in
  `migrate_test`, replay in `decision_test`, `zcode_ui_sync` in
  `zcode_adapter_test`) or crate-private code (candidate settlement and
  `previous_success` in `external_data/tests.rs`, the standalone log line
  in `decision/standalone.rs`, a flagged scenario run's child in
  `scenario/drive_tests.rs`) live there instead.
- [x] 8.6 History readers: `apb stats` (`run_stats`), the estimate
  (`run_estimate::history`), `grade::backfill`, the `run_retro`
  baseline, `previous_success` and candidate settlement each skip a
  flagged run: one test per reader over a fixture project with two
  unflagged and one flagged finished run. `previous_success` with an
  unreadable newest manifest still refuses `HistoryUnreadable`.
- [x] 8.7 Discovery and the save hook: `DiscoverRequest.exclude` in
  `apb_core::discover`, filled from `flagged_markers` by
  `apb_mcp::tools::discover` and the CLI; the same markers in the save
  hook (`apb_core::save_nudge`, `decision/save_nudge.rs`). Tests: a
  session file named after a flagged run's `attempt_finished.session` is
  never opened (assert through the walker's read report); a session
  whose tool input names a flagged run id inside the read part
  contributes no step, request or example and is counted under the
  skip reason `external_data`; an unflagged run's session is read as before; the
  save hook gives no nudge, caches no answer and asks no decision for a
  session naming a flagged run id.
- [x] 8.8 Surfaces: the field on `RunBody`, the scenario run body,
  `PlaybookRunArgs`, `PlaybookTrialArgs`, `PlaybookExecutePlanArgs`,
  `PlaybookScenarioRunArgs`, `RunResumeArgs`, `ResumeBody`, `apb run`
  (also with `--supervise`), `apb eval` (also `--level quick|smoke`,
  the CLI's scenario run), `apb resume`. One test per surface that the started run's manifest
  carries the flag (MCP in `crates/apb-mcp/src/server/tests`, HTTP in
  `crates/apb-server/tests/suite`, CLI in `crates/apb-cli/tests/suite`).
  A scenario run is prepared by an `apb __scenario-run` child, which a
  test binary of apb-mcp or apb-server cannot start, so MCP
  `playbook_scenario_run` and the HTTP scenario route are tested in
  `crates/apb-cli/tests/suite/external_data_surfaces_test.rs` through the
  shipped binary (`apb mcp`, `apb dashboard`).
  `remember` with the flag, and with an inheriting `continued_from` and
  no flag, is refused `external_data_remember_refused` on MCP, HTTP
  (409) and CLI before a run directory exists, with the memory file
  unchanged. `playbook_memory` `get` and `set` with `APB_RUN_ID` of a
  flagged run are refused `external_data_run`. Read-only MCP
  (`apb mcp --read-only`) still refuses every start tool: assert it.
- [x] 8.9 Visibility: `RunDetail.external_data`, `RunSummary.external_data`,
  `run_status` and `runs_list` (`tools/run.rs`, next to `unattended`),
  `apb runs <id>` and `apb runs` (`run.rs` `runs_cmd`), the report page;
  no surface shows the `+external` suffix in a mode. Tests: present and
  true on a flagged run, absent on an unflagged one. Regenerate
  `api.gen.ts` with `APB_WRITE_TS=1 cargo test -p apb-server --lib
  ts_contract`.
- [x] 8.10 Dashboard: the Run dialog checkbox; when checked, "Remember
  for this project" is disabled and remembered values the person did not
  edit are cleared; the run page shows the flag. Tests in the dialog's
  vitest file; `bun run test`, `bun run check`, `bun run build`. Plain
  CSS over the existing variables only.
- [x] 8.11 Docs as D8 "Docs": `docs/EXTERNAL-DATA.md`, `docs/GUARDRAILS.md`,
  `CLAUDE.md` and `AGENTS.md` with the same text. No em dashes, no
  exclamation marks, no CJK.
- [x] 8.12 Gate the section: `cargo test -p apb-engine --test main
  external_data_test`, `cargo test -p apb-engine --lib external_data`,
  `cargo test -p apb-engine --lib manifest`, the surface tests of 8.8 one
  filter each, clippy; then the whole gate of 6.6 once at the end.
