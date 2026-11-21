# Tasks: MCP tool connectors

Work in order, one task at a time. Before a section, read its part of
`design.md` (named in the section title) and the spec delta it names.
Tick each box (`- [x]`) as soon as the task is done, so an interrupted
run can resume from the first open box. Partial edits from an earlier
attempt may already be in the tree: review them, keep what is right,
finish the rest.

Every design choice is made in `design.md`. If something there does not
fit the code (a function was renamed, a line moved), find the site by
the name `design.md` gives, keep the decision, and note the difference
in the final report. Never invent a different behaviour.

For every behaviour: write the failing test first, run it and see it
fail, then write the code. A compile error is an acceptable first
failure only for a test that names a type or function that does not
exist yet; the transport, timeout and send-once tests must fail on a real
assertion before the code that makes them pass exists. So every section
builds in this order: types and changed signatures first (each step
compiles), then a skeleton whose functions do nothing where a test file
needs them to exist, then the red tests, then the behaviour one piece at
a time. A task names the tests that may pass before its code exists;
every other test it writes must be seen failing first.

Line numbers are those of main efae5822. eval-doubles merges before this
change; inbox-waits, start-rules and incremental-lists-and-session-fork
merge before or beside it. Re-verify every line on merged main and find
each site by the name `design.md` gives; design "Overlap with the
in-flight changes" says which symbols belong to which change.

This change is stdio only. The Streamable HTTP transport with
authorization and the listing surfaces (`apb connector tools`, the doctor
rows, `connectors_list`, the dashboard, the contract kind `mcp`) are the
next change, mcp-http-and-surfaces; build none of them here.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-mcp-tool-connectors-target TMPDIR=$HOME/.cache/apb-mcp-tool-connectors-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-mcp-tool-connectors-target` fails with
a read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-mcp-tool-connectors-target`
and `TMPDIR=/tmp/apb-mcp-tool-connectors-tmp` instead, and before each
build check that `df -h /tmp` shows more than 4G free; if not, stop and
report it. `/tmp` is a RAM disk shared with other sessions. Never use
`/dev/shm`, and never put a target dir, cache or temp data inside the
worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start, and never signal a process by pid or group number.
The owner authorized local commits on this branch; do not push and do
not open a pull request. Do not start subagents.
Never call an agent CLI (claude, codex, opencode, zcode, dsh) or a model:
nothing in this list needs one. The only network use is `cargo` fetching
crates and `bun install`.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-core --lib connector::mcp`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>` (for example `cargo test -p apb-engine --test main
   connector_mcp`). The CLI package is named `apb`: `cargo test -p apb
   --test main mcp_revision_test`.
3. `cargo test` takes one name filter per invocation: two modules are two
   commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;` (see
`crates/apb-engine/tests/suite/connector_call.rs` lines 1 to 60 for a
hand-built run snapshot, and `crates/apb-engine/tests/suite/common/mod.rs`
line 435 `spawn_script` for a scripted HTTP server). The new crate's
integration tests use the same layout: `crates/apb-mcp-client/tests/main.rs`
with `suite/`.

## 0. Preconditions

- [x] 0.1 Each symbol below exists in the named file (one `zg --rg -F
  <symbol> <file>` each; note the line; on main efae5822 they are at the
  line given). If one is missing, stop and report it.
  1. `pub struct ConnectorDoc` (558), `pub struct FunctionSpec` (367),
     `fn is_inbox` (480), `fn carries_notices` (502), `fn idempotency_view`
     (511), `fn from_yaml` (624), `fn validate_templates` (1293):
     `crates/apb-core/src/connector/def.rs`.
  2. `pub enum FunctionKind` (215), `fn native_transport` (227),
     `fn check_function` (348), `fn check_marker` (440):
     `crates/apb-core/src/connector/idempotency.rs`.
  3. `pub struct ConnectorFacts` (415), `fn of_manifest` (465):
     `crates/apb-core/src/connector/resolve.rs`.
  4. `pub mod marker;` (25): `crates/apb-core/src/connector/mod.rs`.
  5. `pub fn render_raw` (227), `pub struct RenderCtx` (32):
     `crates/apb-core/src/connector/template.rs`.
  6. `fn connector_sites` (44): `crates/apb-core/src/connector/sites.rs`.
  7. `mod loop_items;` (29), `mod node_io;` (30), `check_isolation(`
     call (481): `crates/apb-core/src/validate/mod.rs`;
     `fn check_isolation` (15): `crates/apb-core/src/validate/isolation.rs`;
     `fn check_connectors` (41): `crates/apb-core/src/validate/connectors.rs`.
  8. `pub fn guard_program_start` (145), `pub mod testing` (219),
     `pub fn in_read_only_child` (227):
     `crates/apb-core/src/process_mode.rs`.
  9. `pub fn atomic_write_private` (742), `pub fn lock_dir` (901),
     `pub fn sync_dir` (782): `crates/apb-core/src/fsutil.rs`;
     `pub fn read_regular_capped`: `crates/apb-core/src/fsutil/beneath.rs`.
  10. `pub fn sha256_hex` (419), `pub fn is_executable` (328):
      `crates/apb-core/src/content.rs`; `pub fn base_set` (11):
      `crates/apb-core/src/process_env/sets.rs`; `StepCaller` with
      `fn refusal` (37): `crates/apb-core/src/step_env.rs`.
  11. `enum PreparedCall` (307), `fn build_prepared` (696),
      `fn append_event` (1209), `struct EventMeta` (91), `mod target;`
      (31), `settle_unsent(` call (570):
      `crates/apb-engine/src/connector/call/mod.rs`.
  12. `pub(crate) struct LiveTarget` (121), `fn load` (133),
      `fn require_trust` (183): `crates/apb-engine/src/connector/call/target.rs`;
      `pub(crate) fn resolve_secrets` (35): `call/auth.rs`;
      `fn gate` (109), `fn settle_unsent` (444), `fn finish` (461):
      `call/send_once.rs`; `pub fn prepare_body_args` (42):
      `crates/apb-engine/src/connector/intent/stamp.rs`.
  13. `pub enum CallErrorCode` (17), `pub enum CallOk` (146),
      `pub(crate) struct Dispatched` with `warnings` (70):
      `crates/apb-engine/src/connector/result.rs`; `pub fn classify` (36):
      `crates/apb-engine/src/connector/effect.rs`.
  14. `ConnectorCall {` variant (1112), field `item` (1165):
      `crates/apb-engine/src/event.rs`; `const MAY_HAVE_ACTED` (28):
      `crates/apb-engine/src/grade/rules/consent.rs`.
  15. `pub(crate) struct ConnectorScan` (19), `fn scan_connectors` (42):
      `crates/apb-engine/src/gate/trust_scan.rs`; `fn check_connectors`
      (1290): `crates/apb-engine/src/gate/mod.rs`; `fn policy_actions`
      (217): `crates/apb-engine/src/next_action/start.rs`;
      `unapproved_accounts` (94): `crates/apb-engine/src/fit/trust.rs`.
  16. `fn approve_cmd` (345), `Approve {` (67):
      `crates/apb-cli/src/connector.rs`; `mod connector;` (19):
      `crates/apb-cli/src/main.rs`; `fn ask_typed` (136):
      `crates/apb-cli/src/consent.rs`; `fn sanitize_for_terminal` (85):
      `crates/apb-cli/src/util.rs`; `fn start_mcp` (22), `fn request`
      (80): `crates/apb-cli/tests/suite/mcp_cli_test.rs`.
  17. `rmcp = { version = "=3.9.0"` (18): `crates/apb-mcp/Cargo.toml`;
      `fn tier0_reaches_both_the_initialize_and_discover_channels`
      (2073): `crates/apb-mcp/src/server/tests.rs`.
  18. `fn real_home` (772), `pub fn config_dir_redirected` (803),
      `pub fn config_dir` (488): `crates/apb-core/src/config.rs`;
      `pub fn effective_for` (562), `fn checks_redirect` (144):
      `crates/apb-engine/src/gate/launch.rs`.
  19. `pub fn is_engine_site` (30), `pub fn site_label` (123),
      `check_site_node` (used at 130): `crates/apb-core/src/connector/sites.rs`.
  20. `fn decide` (187): `crates/apb-engine/src/connector/call/send_once.rs`;
      `fn effective_state_with` (495):
      `crates/apb-engine/src/connector/intent/ledger/mod.rs`;
      `fn execute_bounded` (149), `fn add_warnings` (205), `fn prepare`
      (434): `crates/apb-engine/src/connector/call/mod.rs`;
      `Remedy::Manual` in the `unapproved_connector_account` arm (266):
      `crates/apb-engine/src/next_action/start.rs`.
  21. `fn guard_refusal` (143), `pub fn healthcheck` (1001), `fn
      prepare_healthcheck` (1040), `pub fn play_call` (1073), `fn
      play_call_pinned` (1089), `fn prepare_play_call` (1146), `fn
      load_snapshot` (1183): `crates/apb-engine/src/connector/call/mod.rs`;
      `fn redact_value` (9): `call/response.rs`; `type ResolvedSecrets`
      (25): `call/auth.rs`; `pub struct ManifestConnector` (264), `fn
      connector` (740): `crates/apb-engine/src/manifest.rs`; `fn
      approve_connector_handler` (519): `crates/apb-server/src/routes/connectors/mod.rs`;
      `fn stop_process_tree` (133): `crates/apb-engine/src/proc.rs`. In
      rmcp 3.9.0 `src/transport/async_rw.rs`: `fn encode` (613) and the
      `send` of the transport (234).
- [x] 0.2 `rg -o '"V3[0-9][0-9]"' crates` prints nothing. If V300
  appears, stop and report it.
- [x] 0.3 None of these exists: `crates/apb-mcp-client/`,
  `crates/apb-core/src/connector/mcp/`, `crates/apb-engine/src/connector/mcp/`,
  `crates/apb-core/src/validate/mcp.rs`, `crates/apb-cli/src/connector_mcp.rs`.
  If one exists, it is a partial earlier attempt: review it against
  `design.md`.
- [x] 0.4 eval-doubles is merged and archived (its folder is under
  `openspec/changes/archive/`): `crates/apb-core/src/eval_doubles.rs` and
  `crates/apb-engine/src/eval/doubles.rs` exist, `PreparedCall` has a
  `Double` variant, `CallErrorCode` has `DoubleUnmatched`, and
  `build_prepared` answers a doubled function (`DoubleSite::prepare`)
  before `resolve_secrets`. If not, stop and report it. Note which of
  inbox-waits, start-rules and incremental-lists-and-session-fork are
  archived.
- [x] 0.5 Note the merged-main commit you start from and the highest
  validator code on it (`rg -o '"V[0-9]{3}"' crates/apb-core/src/validate | sort -u | tail -1`).
- [x] 0.6 `Cargo.lock` holds rmcp 3.9.0 and no other rmcp. In its source
  (`~/.cargo/registry/src/*/rmcp-3.9.0/`) check that each name of design
  "The rmcp client this design uses" exists at the line given:
  `ClientLifecycleMode` with `Initialize`, `Discover`, `Auto`;
  `DEFAULT_AUTO_DISCOVER_TIMEOUT`; `serve_with_lifecycle`;
  `call_tool_once`; `CallToolResponse`; `ProtocolVersion::V_2026_07_28`
  and `V_2025_11_25`; `peer_info`; the `(R, W)` `IntoTransport` impl and
  `DEFAULT_MAX_LINE_LENGTH` in `src/transport/async_rw.rs`; the feature
  `transport-async-rw`. If one is missing, stop and report it. Do not
  change the rmcp version.

## 1. apb mcp wire tests on rmcp 3.9.0 (design D6, spec mcp-server-revision)

No rmcp change in this section or any other: `crates/apb-mcp/Cargo.toml`
stays as it is.

- [x] 1.1 Create `crates/apb-cli/tests/suite/mcp_revision_test.rs` with a
  `start_mcp_raw(dir, env)` helper (like `start_mcp` in
  `mcp_cli_test.rs` line 22, without the handshake) and the test
  `the_2025_06_18_wire_is_unchanged` of design D6 item 3.1, with an
  `APB_RECORD_MCP_FIXTURE=1` switch that writes the normalized lines
  (the envelopes, the sorted tool names and the definitions of
  `playbook_catalog` and `run_status` only, never the whole tool list) to
  `crates/apb-cli/tests/fixtures/mcp/legacy_2025_06_18.jsonl` instead of
  comparing; on a name-list difference the test prints the added and
  removed names. Record it once with the switch, then run `cargo test -p
  apb --test main mcp_revision_test` without it: it passes. Note both
  runs in the report.
- [x] 1.2 Add the other three tests of design D6 item 3 to
  `mcp_revision_test.rs`: `the_2025_11_25_tool_list_has_no_cache_fields`,
  `a_2026_07_28_client_gets_discover_tier0_and_cache_hints` (the
  `prompts/list` answer is an empty `prompts` list, design D6 "What is
  already true" item 4), `a_2026_07_28_request_without_meta_is_refused`
  (`-32602`). They are expected to pass with no code change; if one
  fails, stop and report the answer you saw. Run `cargo test -p apb
  --test main mcp_revision_test`, `cargo test -p apb --test main
  mcp_cli_test` and `cargo test -p apb-mcp --lib
  server::tests::tier0_reaches_both_the_initialize_and_discover_channels`.

## 2. The client crate and its test server (design D0, D4 "The client")

- [x] 2.1 Create `crates/apb-mcp-client/` (`Cargo.toml` as design D0 "The
  new crate" item 1: `rmcp = "=3.9.0"`, features `client` and
  `transport-async-rw`, no `libc`; `src/lib.rs` with the module doc
  "Blocking MCP client sessions for apb's connector calls: one server
  conversation per session, run on its own thread"; `src/session.rs`,
  `src/stdio.rs` and `src/tap.rs` with the public types of design D4,
  `Ended`, `FailureKind::Refused` and `MAX_LINE_BYTES` included, and
  functions that return `Err(Failure { kind: FailureKind::Transport,
  message: "not built".into() })`, `close` answering `Ended::NotStarted`).
  Add it to the root `Cargo.toml` members (line 3) and as a dependency of
  `apb-engine` (design D0 item 3). `cargo build -p apb-mcp-client` and
  `cargo build -p apb-engine`.
- [x] 2.2 `src/testing.rs` behind the `testing` feature: `StubServer`,
  `StubEra`, `StubCall` and the embedded `STUB_SH` exactly as design D0
  "Test server" (with `pid.log`, `cwd.log`, `env.values`, `methods.log`,
  the paged `tools.json`, `StubCall::Oversize`, `StubCall::RawResult`
  with `call.raw`, and `ignore_eof.flag`). Unit tests in that file (they may
  pass at once; they test the stub, not apb):
  `the_stub_answers_discover_in_the_modern_era`,
  `the_stub_answers_initialize_in_the_legacy_era`,
  `the_stub_logs_calls_env_args_and_cwd`, `the_stub_serves_pages`
  (each drives `stub-mcp.sh` with `std::process::Command` and lines on
  stdin, and waits for its exit). Run `cargo test -p apb-mcp-client
  --features testing --lib testing`.

## 3. The manifest kind and its rules (design D1, spec mcp-connector-kind)

- [x] 3.1 Create `crates/apb-core/src/connector/mcp/mod.rs` with the types
  of design D1 "Manifest schema" and empty
  `validate_function_shape` and `validate_server_shape` returning
  `Ok(())`; `pub mod mcp;` in `connector/mod.rs` (design D1). Add
  `ConnectorDoc.mcp`, `FunctionSpec.mcp`, `is_mcp`,
  `is_untrusted_output`, the `carries_notices` line, and
  `FunctionKind::Mcp` with its `native_transport` arm. Fix every struct
  literal of `FunctionSpec` and `ConnectorDoc` in the workspace (`rg -n
  'FunctionSpec \{' crates`, `rg -n 'ConnectorDoc \{' crates`) with
  `mcp: None` (on efae5822 the only literal is
  `crates/apb-server/src/routes/connectors/view.rs` line 88; the other
  hits are signatures). `cargo build --workspace`.
- [x] 3.2 Tests first, in `connector/mcp/mod.rs` `#[cfg(test)] mod tests`
  (parse the YAML of design D1 "Example" and variants through
  `ConnectorDoc::from_yaml`): `a_stdio_connector_loads`,
  `an_mcp_function_with_an_http_field_is_refused`,
  `a_relative_command_is_refused`, `a_command_with_a_placeholder_is_refused`,
  `an_env_name_starting_with_apb_is_refused`,
  `a_secret_in_args_is_refused`, `a_secret_in_env_is_allowed`,
  `args_take_account_fields_only`, `a_write_without_idempotency_is_refused`,
  `key_is_refused_on_mcp`, `marker_and_none_are_accepted_on_mcp`,
  `a_marker_reconcile_function_must_be_a_read_only_mcp_function`,
  `an_mcp_identity_function_is_accepted`,
  `an_mcp_block_without_mcp_functions_is_refused`,
  `mcp_functions_without_a_block_are_refused`,
  `an_http_function_next_to_an_mcp_block_is_refused`,
  `auth_and_webhook_are_refused_with_an_mcp_block`,
  `two_writes_mapping_one_tool_are_refused`,
  `two_reads_mapping_one_tool_are_accepted`,
  `a_timeout_below_the_probe_is_refused_under_auto` (`timeout_sec: 10`,
  the message starts `mcp_timeout_too_small:`),
  `a_short_timeout_is_accepted_under_modern` (`timeout_sec: 2`),
  `launcher_name_knows_the_launchers`,
  `http_transport_is_refused_by_this_apb` (a manifest with only
  `transport: http` in its `mcp:` block gets the message of design D1
  rule 3.4.1), `mcp_functions_count_as_untrusted_output`,
  `mcp_functions_never_carry_notices`. Then the rules of design D1
  "Manifest rules" 1 to 5 in `def.rs`, `connector/mcp/mod.rs`,
  `idempotency.rs` and `validate_templates`. Run `cargo test -p apb-core
  --lib connector`.
- [x] 3.3 Tests first in `crates/apb-core/src/validate/mcp.rs`:
  `v300_warns_for_a_binding_without_functions`,
  `v300_is_silent_with_a_functions_list`,
  `v300_is_silent_with_read_only`, `v300_is_silent_without_facts`,
  `v301_warns_for_a_launcher_command`, `v301_is_silent_for_a_server_program`,
  `v302_refuses_an_mcp_baseline_read`, `v302_refuses_an_mcp_item_check`,
  `v302_refuses_an_mcp_follow_up`, `v302_is_silent_for_a_node_binding`; in
  `validate/isolation.rs` tests: `v113_names_the_mcp_reason` and
  `v113_keeps_its_message_for_an_http_connector`; in
  `validate/connectors.rs` tests: `v187_warns_for_an_unmarked_mcp_grant`.
  Then `ConnectorFacts.mcp_transport`, `ConnectorFacts.mcp_launcher` and
  the `untrusted_output_functions` change in `resolve.rs` (design D1
  "Validator"), `validate/mcp.rs`, the
  `mod mcp;` line, the call after the V187 line (421), and the
  `check_isolation` signature change. Run `cargo test -p apb-core --lib
  validate`.
- [x] 3.4 Tests first in `crates/apb-engine/src/connector/prompt.rs`:
  `an_mcp_function_shows_the_manifest_description_only` (a doc whose
  function description is "manifest text"; the block contains it and no
  other description) and `an_mcp_grant_adds_the_untrusted_paragraph_once`.
  Then the untrusted paragraph uses `is_untrusted_output` (line 89). Run
  `cargo test -p apb-engine --lib connector::prompt`.

## 4. Tool digests and the pin store (design D2, spec mcp-tool-pins)

- [x] 4.1 Tests first in `crates/apb-core/src/connector/mcp/digest.rs`:
  `key_order_does_not_change_the_digest`,
  `icons_and_meta_are_not_pinned`, `a_changed_description_changes_the_digest`,
  `a_member_no_list_names_is_pinned`,
  `an_absent_title_and_an_empty_title_differ`,
  `the_digest_starts_with_sha256`, `the_pin_digest_covers_program_and_tools`
  (each of the connector tree digest, the account digest, the program,
  the program digest and one tool digest changes it). Then
  `UNPINNED_MEMBERS`, `tool_digest`, `canonical_json`, `PIN_DIGEST_DOMAIN`
  and `pin_digest`. Run `cargo test -p apb-core --lib
  connector::mcp::digest`.
- [x] 4.2 Tests first in `crates/apb-core/src/connector/mcp/pins.rs` (each
  with a config dir under `$TMPDIR` through `isolate_test_config!`,
  `crates/apb-core/src/config.rs` line 732): `a_missing_store_is_empty`,
  `record_then_compare_is_equal`, `compare_answers_the_first_difference`
  (one case each for `Tree`, `Account`, `ProgramGone`, `Program`,
  `ToolUnpinned` and `Tool`, and a record that differs in two places answers the first
  in binding order), `compare_without_a_tool_digest_is_presence_only`,
  `compare_skips_a_program_not_read`, `a_record_under_another_tree_differs`,
  `record_replaces_the_whole_record`, `an_unparsable_store_is_an_error`,
  `a_linked_store_is_refused` (unix), `a_linked_config_dir_is_followed_once`
  (unix), `record_merges_with_a_concurrent_writer` (two threads, two
  accounts), `the_store_file_is_private` (mode 0600 on unix),
  `the_trust_store_is_untouched`,
  `a_record_over_the_cap_is_refused_and_leaves_the_file`
  (`mcp_pin_store_too_large`, the file byte for byte as before),
  `every_successful_record_loads_again` (records of near-limit listings
  added until one is refused: `load` reads after each that succeeded).
  Then `PinStore`, `ServerPin` (with `connector_digest` and
  `account_digest`), `PIN_STORE_CAP`, `PinError::TooLarge`, and `ServerPin::compare`, `ToolPin` (with
  `members`), `ProgramFact`, `LiveBinding`, `BindingDiff`, `PinError`,
  `server_key`, with
  the canonical config dir, the `Beneath` read, the lock that fails
  closed and `sync_dir`. Run `cargo test -p apb-core --lib
  connector::mcp::pins`.
- [x] 4.3 Tests first in `crates/apb-core/src/config.rs` tests:
  `account_entry_names_the_real_user` (unix: the home equals what
  `default_config_dir` is built from, the name is not empty). Then
  `account_entry() -> Option<AccountEntry { name, home }>` (one
  `getpwuid_r`, reading `pw_name` and `pw_dir`) with `real_home` built
  on it (design D4 "The server's environment"). Run `cargo test -p
  apb-core --lib config`.

## 5. The stdio session (design D4 "The client", spec mcp-stdio-transport)

- [x] 5.1 Tests first in `crates/apb-mcp-client/tests/suite/stdio_test.rs`
  (all `#[cfg(unix)]`, the stub of section 2):
  `a_modern_server_opens_modern`, `a_legacy_server_falls_back`,
  `a_silent_legacy_server_falls_back_after_the_probe` (`LegacySilent`;
  allow 15 s), `modern_mode_refuses_a_legacy_server` (`Unsupported`),
  `legacy_mode_skips_discover` (`methods()` has no `server/discover`),
  `list_tools_follows_cursors`, `list_tools_stops_at_a_repeated_cursor`,
  `list_tools_stops_at_the_page_limit`, `list_tools_stops_at_the_tool_limit`,
  `list_tools_stops_at_the_byte_limit` (pages that each fit the line
  limit and sum past `max_bytes`: `TooLarge`),
  `a_failed_handshake_reaps_a_server_that_ignores_end_of_input`
  (`set_ignore_eof(true)` and a modern server under `Lifecycle::Legacy`
  or an `initialize` answer that does not parse: `Failure.ended` is
  `Reaped { killed: true }` and `StubServer::is_alive(last_pid)` is false
  when `connect` returns),
  `a_handshake_failing_at_the_deadline_is_reaped_before_connect_returns`
  (`LegacySilent` with `set_ignore_eof(true)` and a 3 s deadline: `connect`
  answers `Timeout` after the close, `ended` is `Reaped { killed: true }`,
  `is_alive` is false at return, and the return comes before deadline
  plus `REPLY_CLEANUP_ALLOWANCE`),
  `call_tool_returns_the_result`,
  `an_is_error_result_is_complete_with_is_error`,
  `a_json_rpc_error_is_answered`, `an_exit_after_read_is_lost`,
  `a_hang_is_lost_with_timeout`, `input_required_is_reported`,
  `list_tools_keeps_members_the_struct_drops` (a tool with an
  `execution` member comes back with it),
  `a_listing_answered_at_once_is_captured_every_time` (a hundred
  sessions in a loop against the stub, which answers `tools/list` as soon
  as it reads the line; every `list_tools` answers the raw tools),
  `a_small_body_with_a_large_dropped_block_is_refused_by_the_line_limit`
  (`StubCall::RawResult` with a short text block and an image block whose
  data makes the line `MAX_LINE_BYTES + 1` bytes: `Lost` with a message
  starting `mcp_message_too_large:`), `an_oversize_answer_is_refused_by_name`,
  `an_oversize_listing_is_too_large`, `a_message_flood_is_refused_by_name`
  (a stub that writes 10 001 notification lines before answering),
  `the_deadline_bounds_the_discover_probe` (`LegacySilent`, a 3 s
  deadline: `Timeout` in under 5 s),
  `the_environment_is_exactly_the_given_one`,
  `stderr_never_reaches_the_answer`,
  `close_after_end_of_input_reaps_without_kill` (`Reaped { killed:
  false }`),
  `a_server_that_ignores_end_of_input_is_killed_and_reaped`
  (`set_ignore_eof(true)`: `Reaped { killed: true }` and
  `StubServer::is_alive(last_pid)` false), `a_second_close_is_not_started`,
  `a_read_only_process_starts_no_server` (through
  `apb_core::process_mode::testing::in_read_only_child`; `Refused`, no
  `starts.log`), `a_session_works_inside_a_tokio_runtime`
  (`#[tokio::test]`, open and call inside it),
  `a_spawn_failure_names_the_program_only`; and unit tests in
  `src/tap.rs` over an in-memory pipe:
  `the_id_is_registered_before_any_byte_is_forwarded` (the sink checks,
  on the first byte it receives, that the shared set already holds the
  request's id), `a_split_request_is_registered_once_whole` (a
  `tools/list` request written in three pieces: the sink receives
  nothing before the third and one whole line after it, and the set
  holds the id once), `a_non_listing_request_is_forwarded_unregistered`,
  `flush_forwards_pending_lines`. Then the session: the worker
  thread, the lifecycle mapping under the deadline, the guard and the
  spawn, the line tap (bounded reader at `MAX_LINE_BYTES`, the writer
  that registers before it forwards, and the raw `tools/list` capture) over
  the pipes as the rmcp transport, the raw list and the call mapping,
  `close` and `Drop`, as design D4 items 1 to 6. Run `cargo test -p apb-mcp-client --features
  testing --test main stdio_test`, then `rg -n
  'libc::kill|\.signal\(|graceful_shutdown|process_group|setsid'
  crates/apb-mcp-client` must print only the expected hits (since fix
  round 1, which gave the server its own process group): the group calls
  of `src/session.rs` `mod group` (`libc::killpg` of the server's own
  group) and the spawn's `process_group(0)`, the test-only `ps` helper
  `StubServer::process_group` in `src/testing.rs`, and the client tests
  that call it. Any other hit (a `setsid`, a `.signal(`, a
  `graceful_shutdown`, a `libc::kill` of a pid) is a defect. Fix round 1
  also made the end a group kill while the server is unreaped
  (`Reaped { killed, group }`) and the working directory a fresh private
  one; the tests above read `Reaped { killed: true, .. }`.

## 6. The call path, the pin check and the gate (design D4 "The engine side", D2 "Checking pins", "The run gate")

- [x] 6.1 Types first: `crates/apb-engine/src/connector/mcp/mod.rs`,
  `server.rs`, `map.rs`, `pin.rs` with the types and signatures of design
  D4 and D2 (`McpConnectInput` with `account_digest`, `check_pin`,
  `CheckedPin`, `connect`, `Connected`, `SessionHandle`, `McpCall`,
  `dry_run`, `end_warning`, `preview_start`, `resolve_program`,
  `ConfirmedProgram`, `list_live`,
  `record`) and bodies that return
  `Err(CallError::new(CallErrorCode::Config, "mcp: not built"))`;
  `send_once::preview` answering "go on" for every call;
  `pub(crate) mod target;` in `call/mod.rs`; `PreparedCall::Mcp` and its
  arms (with eval-doubles' tuple `dispatch`), `tool_digest`,
  `mcp_session`, `EventMeta.tool_digest`, `EventMeta.mcp_session`,
  `ConnectorCall.tool_digest` (every full
  construction gets `tool_digest: None`; on efae5822 they are
  `connector/call/mod.rs` 1211, `event.rs` 4271 and 4293,
  `review/scope.rs` 1257, `tests/suite/event_test.rs` 58 and
  `crates/apb-server/tests/suite/connectors_api_test.rs` 927; find them
  again with `rg -n 'ConnectorCall \{' crates` and skip the patterns that
  end in `..`), `CallOk::Mcp` and its arms in `result.rs`,
  `CallErrorCode::ToolError` after `DoubleUnmatched`. `cargo build
  --workspace`, then `cargo test -p apb-engine --test main
  journal_compat_test` (passes unchanged).
- [x] 6.2 Tests first: the unit test
  `connector::mcp::tests::an_unconfirmed_end_is_a_warning_never_a_stop_claim`,
  and in a new `crates/apb-engine/tests/suite/connector_mcp.rs` (a
  hand-built run snapshot as in `connector_call.rs`, the connector of
  design D1 "Example" with `command` set to the stub's absolute path,
  a pin record written through `PinStore::record` with the snapshot's
  `ManifestConnector.digest` as `connector_digest`, the stub's absolute
  path and file digest and the tool digests from `tool_digest`,
  an `APB_CONFIG_DIR` under `$TMPDIR` and `APB_TEST_DEFAULT_CONFIG_DIR`
  set to the same dir, so the redirect rule holds only in its own test,
  every mcp function `timeout_sec` of 15 or more under `protocol: auto`):
  `a_read_only_call_returns_structured_content`,
  `a_call_returns_text_parsed_as_json`, `a_call_returns_plain_text`,
  `non_text_blocks_are_counted_as_dropped`,
  `response_pick_projects_the_body`, `full_skips_the_projection`,
  `a_secret_echoed_by_the_tool_is_redacted`,
  `a_large_read_is_incomplete_response`,
  `the_server_sees_only_the_fixed_set_and_its_declared_env`,
  `a_declared_proxy_reaches_the_server`,
  `a_shadow_program_on_the_callers_path_is_not_started`,
  `a_changed_program_file_is_refused`,
  `a_changed_definition_refuses_the_old_pin` (the manifest's connector
  digest differs from the record's `connector_digest`, as after an
  `mcp.args` change and a new tree approval: `mcp_server_changed` naming
  the definition, no `starts.log`),
  `an_unpinned_server_is_refused`,
  `a_run_step_with_a_redirected_config_dir_is_refused`,
  `no_refusal_names_the_approve_command`,
  `a_changed_execution_member_refuses_the_next_call`,
  `the_server_runs_in_the_project_root` (`cwd_seen`; replaced in fix
  round 1 by `the_server_runs_in_a_fresh_private_directory`: a new empty
  private directory, never the project root),
  `the_server_shares_the_callers_process_group`
  (replaced in fix round 1 by `the_server_leads_its_own_process_group`:
  `StubServer::process_group(last_pid)` is the server's own pid and not
  the test process's group; `ps -o pgid=`, no `/proc`),
  `a_changed_pin_runs_no_secret_helper` (the account's token comes from a
  `{{cmd:...}}` helper that appends to a marker file; with the record's
  program digest, tree digest, account digest or tool pin changed, one
  case each, the call is refused `mcp_server_changed` or
  `mcp_tool_unpinned`, the marker file does not exist and there is no
  `starts.log`),
  `a_dry_run_starts_nothing_and_names_env_only`,
  `each_call_starts_its_own_server`,
  `arguments_that_are_not_an_object_are_refused` (`"x"`, `1`, `[1]` and
  `null`, with and without `args_schema`, live and dry-run: `invalid_args`,
  no `starts.log`, no `send_intent_recorded`),
  `a_pin_of_another_project_account_is_refused` (two project roots, each
  with its own `work` account of different fields; pinned in the first,
  a call in the second is `mcp_server_changed` naming the account),
  `a_reflected_secret_in_a_call_listing_is_refused`
  (`mcp_tools_reflect_a_secret`, no `tools/call`),
  `an_unpinned_tool_is_refused_before_the_call`,
  `a_description_changed_between_two_calls_is_refused` (assert the code
  prefix `mcp_tool_changed:` and the class, not the text after the
  semicolon), `no_refusal_carries_a_digest` (the texts of
  `mcp_server_changed` for each of tree, account, program gone and
  program changed, `mcp_tool_changed`, `mcp_tool_unpinned` and
  `mcp_server_unpinned`: each names what differs and holds no
  `sha256:` and no run of 16 or more hexadecimal characters), `a_reapproved_tool_goes_out_again`,
  `a_missing_tool_is_refused`, `an_unparsable_pin_store_refuses_the_call`,
  `annotations_never_change_what_a_call_may_do`,
  `the_journal_carries_the_tool_digest`,
  `the_journal_url_is_the_endpoint`, `max_calls_counts_mcp_calls`,
  `a_refused_pin_check_spends_no_max_calls`,
  `the_healthcheck_checks_the_pins_without_a_call` (`call::healthcheck`
  on the installed connector: ok with `tools_checked`, one `starts.log`
  line, `methods.log` holds `tools/list` and no `tools/call`, no
  `calls.log`, nothing written to the config dir),
  `the_healthcheck_refuses_a_changed_tool` (`mcp_tool_changed`, no
  `tools/call`), `the_healthcheck_of_an_unpinned_account_starts_nothing`
  (`mcp_server_unpinned`, no secret helper run, no `starts.log`),
  `the_healthcheck_of_a_connector_without_one_is_refused_as_on_main` (no
  `healthcheck` field: `declares no healthcheck`, no `starts.log`),
  `a_playground_call_of_an_mcp_function_is_refused_by_name` (`play_call`
  live, the account's token from a marker-writing `{{cmd:...}}` helper:
  `mcp_live_call_unsupported:`, the marker file does not exist, no
  `starts.log`),
  `a_playground_dry_run_of_an_mcp_function_renders`,
  `a_body_at_the_cap_in_the_worst_escaping_is_returned`
  (`StubCall::RawResult` with a `structuredContent` string whose
  serialized body is `BODY_CAP` bytes of two-byte characters, every one
  written as `\u00XX`, and the same value serialized in a text block with
  its escapes escaped again: the call answers the body complete),
  `an_mcp_grant_hands_the_agent_no_mcp_server` (a profile in the
  minimal environment; the claude stub of `tool_exposure_test.rs` line
  79 writes its argv; it holds `--strict-mcp-config` and no
  `--mcp-config` file names the connector; its doc line says
  `environment: full` is out of its reach),
  `an_mcp_function_is_answered_by_its_double` (design D5 "Eval doubles":
  an eval run config whose `connectors:` doubles answer
  `tracker-mcp.create_comment`; the call answers the double's body, the
  capture line names `create_comment`, the journaled call has no
  `tool_digest`, the stub's `starts.log` does not exist), and the unit
  test `connector::mcp::tests::max_line_bytes_covers_the_body_cap`
  (`MAX_LINE_BYTES == 7 * BODY_CAP + 64 * 1024`). Then
  `end_warning`, `server::stdio_server` with the fixed environment of
  design D4 "The server's environment", `check_pin` (the config dir
  rule, the store, the record, the binding of tree, account, program
  path and file digest, the tool's pin, all before `resolve_secrets`),
  then `connect` (after the secrets: the start, the raw listing, the
  reflected-secret walk, the tool digest compared),
  `McpCall::send` with the `map` of section 7 stubbed to the success row
  only, the live order of design D4 "The order of one call" in
  `prepare` (with `preview` still answering "go on"), branch 7a of
  `build_prepared` (the dry-run answer and the live refusal, after
  eval-doubles' step 5b, the mock and the inbox branches, and BEFORE
  secret resolution, so a refused live call runs no secret helper), the mcp branch of
  `healthcheck` and `mcp::pin::healthcheck` (design D4 "Live calls
  outside a run"), and the `run`, `execute_bounded` (the close after
  the settlement) and `append_event` changes. The
  double test passes with no code in either doubles module; if it does
  not, stop and report. Run `cargo test -p apb-engine --lib
  connector::mcp`, `cargo test -p apb-engine --test main connector_mcp`
  and `cargo test -p apb-engine --test main connector_call`.
- [x] 6.3 Tests first in `connector/mcp/pin.rs` `#[cfg(test)]` with the
  stub: `resolve_program_finds_a_bare_name_on_the_given_path`,
  `resolve_program_canonicalizes_a_link`, `list_live_marks_each_row`
  (one row of each status, a changed row with its changed members, and
  one unmapped tool), `list_live_needs_the_connector_approved_only`,
  `list_live_writes_nothing`, `list_live_binds_the_tree_digest`,
  `list_live_binds_the_account_digest`, `record_writes_the_listing_whole`,
  `a_reflected_secret_refuses_the_listing_before_hashing` (the token in a
  tool name, a description, an annotation value, an input schema member
  name and a nested string value, mapped and unmapped tools, one case
  each: `mcp_tools_reflect_a_secret`, no row and no digest in the
  answer, the error names no tool),
  `redact_display_rewrites_member_names` (the second line, over text
  given to it directly; fix round 1 replaced `redact_display` by the one
  walk `mcp::redact_tree` and the test is now
  `the_display_walk_rewrites_member_names`), `a_description_is_cut_after_the_redaction`,
  `list_live_runs_only_after_the_program_confirmation`,
  `list_live_refuses_an_account_changed_since_confirmation` and
  `list_live_refuses_a_tree_changed_since_confirmation` (the confirmed
  digests against a reloaded account and connector; no secret helper
  runs, no `starts.log`), `list_live_uses_the_fixed_timeout` (a
  healthcheck function with `timeout_sec: 2` does not shorten it),
  `record_writes_the_prepared_pins` (the record's member digests equal
  the prepared map's, built before the confirmation),
  `list_live_takes_only_the_confirmed_program` (a `ConfirmedProgram`
  whose path or file digest is not the file's is refused before
  `resolve_secrets`), `preview_start_renders_without_secrets` (fields
  rendered, every `{{secret.*}}` part masked with its source, no helper
  run).
  Then `resolve_program`, `list_live`, `redact_display` (now
  `mcp::redact_tree`) and `record`
  (design D2 "Capturing pins", "Reflected secrets"). Run `cargo test -p apb-engine --lib connector::mcp::pin`.
- [x] 6.4 Tests first in a new
  `crates/apb-engine/tests/suite/connector_mcp_gate.rs`:
  `a_start_with_an_unpinned_tool_is_refused`,
  `a_start_with_every_tool_pinned_is_admitted`,
  `the_gate_starts_no_server`,
  `another_project_account_of_the_same_name_is_unpinned` (two project
  roots, `mcp_tools_unpinned` in the second),
  `an_old_pin_under_a_reapproved_tree_is_unpinned` (the installed tree
  changed and approved, the record names the old tree digest:
  `mcp_tools_unpinned`), `the_refusal_offers_a_manual_remedy`
  (`next_action::start` actions: `Remedy::Manual`, no command line),
  `fit_lists_the_unpinned_tool`. Then
  `ConnectorScan.unpinned_mcp_tools`, the `check_connectors` return, the
  `policy_actions` arm, the fit row (design D2 "The run gate"). Run
  `cargo test -p apb-engine --test main connector_mcp_gate`, `cargo test
  -p apb-engine --lib next_action`, `cargo test -p apb-mcp --test main
  run_tools_test` and `cargo test -p apb-server --test main
  connectors_api_test`.

## 7. Outcomes and send once (design D3, spec mcp-send-once)

- [x] 7.1 Tests first in `connector/mcp/map.rs`:
  `every_answer_maps_as_the_table_says` (one assertion per row of the
  design D3 table, read-only and write, each of -32700, -32600, -32601,
  -32602, -32603, -32000 and -32099 its own row, every one `tool_error`
  and `WriteUnknown` for a write), `a_connect_failure_is_pre_dispatch`,
  `a_refused_spawn_is_permission`, `messages_are_redacted`,
  `the_body_rules_hold` (the four rules of design D3 "Body rules"); in
  `effect.rs` tests: `a_tool_error_write_is_unknown` and
  `a_tool_error_read_is_failed`; in
  `crates/apb-engine/tests/suite/grade_rules_test.rs`:
  `a_tool_error_call_may_have_acted`. Then `map::connect_error`,
  `map::answer`, `map::body`, the `classify` line, `as_str` of
  `ToolError`, and `"tool_error"` in `MAY_HAVE_ACTED`. Run `cargo test -p
  apb-engine --lib connector::mcp::map`, `cargo test -p apb-engine --lib
  connector::effect` and `cargo test -p apb-engine --test main
  grade_rules_test`.
- [x] 7.2 Tests first in a new `crates/apb-engine/tests/suite/connector_mcp_send.rs`
  (the snapshot of 6.2 with `create_comment`, `list_comments` and
  `whoami`; a run journal with an `attempt_started` so calls bind to an
  attempt, as `connector_play_call.rs` does):
  `a_confirmed_write_settles_confirmed`,
  `a_server_that_exits_after_reading_the_call_leaves_it_unknown`,
  `a_call_that_times_out_is_unknown`,
  `a_tool_error_on_a_write_is_unknown_and_a_marker_repeat_reconciles`,
  `a_tool_error_on_a_none_write_is_refused_on_repeat`,
  `on_unknown_resend_allows_the_repeat`,
  `a_request_error_after_acting_is_unknown` (a stub that appends the
  write to `calls.log` as done and then answers `-32602`: `tool_error`,
  `send_intent_settled.outcome == "unknown"`, a `none` repeat refused
  `unknown_effect` and `calls.log` keeps one line),
  `a_server_error_on_a_write_is_unknown` (`-32603`),
  `input_required_settles_unknown` (a `none` repeat is refused
  `unknown_effect`), `a_task_answer_is_unknown`,
  `a_server_that_cannot_start_records_no_intent`,
  `a_pin_refusal_records_no_intent`,
  `a_kill_between_connect_and_call_leaves_no_intent`,
  `the_settlement_is_journaled_before_the_close`,
  `a_forged_marker_in_the_arguments_is_neutralized`,
  `the_marker_rides_in_its_argument`,
  `an_already_sent_repeat_starts_no_server`,
  `a_spent_budget_starts_no_server`,
  `a_repeatable_write_is_untracked`. Every function there that uses
  `timeout_sec` under 15 sets `protocol: modern`. Then `preview` (design
  D4 "The order of one call" step 1) and the full `map` in
  `McpCall::send`. Run `cargo test -p apb-engine --test main
  connector_mcp_send` and `cargo test -p apb-engine --test main connector_mcp`.

## 8. The approve output, exposure and docs (design D5, D6 item 5, spec mcp-connector-surfaces)

- [x] 8.1 Tests first. Two places, since an MCP approval succeeds only
  at an interactive terminal and no test drives one:
  1. Unit tests in `crates/apb-cli/src/connector_mcp.rs` over
     `approve_mcp` with a scripted `Confirm` (it answers each typed
     question from a list and records the questions and everything
     printed before each; a config dir under `$TMPDIR`, the connector
     installed and approved, an account file, the stub; the account's
     token comes from a `{{cmd:...}}` helper that appends to a marker
     file, so a test sees whether any secret was resolved):
     `two_confirmations_pin_the_tools` (both answered with the connector
     name: the record holds the tree and account digests, the program
     path, its digest and the mapped tools; the program started once),
     `a_declined_program_confirmation_starts_nothing` (no marker line, no
     `starts.log`, nothing approved or pinned),
     `a_declined_tool_confirmation_pins_nothing` (the server was listed,
     nothing approved or pinned),
     `the_program_confirmation_shows_the_rendered_start` (an `env` of
     `PATH: "{{account.bin_dir}}:/usr/bin"`, `HTTPS_PROXY:
     "{{account.proxy}}"` and `TOKEN: "{{secret.token}}"`, an `args` entry
     `--base={{account.base_url}}`: before the first question the output
     holds the rendered `PATH` and `HTTPS_PROXY` values, the rendered arg,
     every non-secret field of the account, `TOKEN=[secret token from
     cmd]`, and the marker file does not exist yet),
     `the_program_confirmation_shows_each_secret_command` (the account's
     secret field taken from a `{{cmd:...}}` helper that appends to the
     marker file: before the first question the output holds
     `secret command: token: <that command line>`, sanitized, and the
     marker file does not exist),
     `the_confirmations_sanitize_definition_and_account_text` (an account
     field holding `ESC [ 2 J`: printed sanitized in both confirmations),
     `approve_prints_each_tools_title_and_input_schema` (a title and an
     input schema longer than the output limit: the title line, the
     schema line cut with ` [cut: <n> more characters]`, both sanitized),
     `approve_reports_a_missing_tool_and_pins_the_rest`,
     `approve_warns_for_a_launcher` (since fix round 1 the line says "an
  interpreter or a launcher"), `server_text_is_sanitized` (a
     description with an escape sequence prints without it),
     `approve_tree_prints_every_env_template` (the tree path: three `env:`
     lines with the unrendered templates, no account value, no marker
     line), `approve_tree_at_a_terminal_approves_and_pins_nothing` (the
     record in `mcp-tools.json` byte for byte what it was, no
     `starts.log`), `an_args_change_needs_the_account_approved_again`
     (tree and account approved through `approve_mcp`, `mcp.args`
     changed, the new tree approved: `gate::check_run` of a playbook
     granting the function is refused `mcp_tools_unpinned`; the account
     approval records the new `connector_digest`),
     `a_reflected_secret_refuses_the_approval` (the stub's `tools.json`
     holds the account's test token in a description, then as an input
     schema property name, then in a tool name: each time
     `mcp_tools_reflect_a_secret:`, `mcp-tools.json` byte for byte as
     before, nothing printed holds the token or a tool name, no account
     approval).
  2. Binary tests in a new `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs`
     (stdin not a terminal, as every integration test runs):
     `approve_without_a_terminal_is_refused` and
     `approve_tree_without_a_terminal_is_refused` (exit 2, the message
     starts `mcp_approve_needs_a_person:`, the output holds no
     `sha256:` and no tool or program text, no pin, no approval, no
     `starts.log`, no marker line),
     `a_retry_without_a_terminal_learns_nothing` (three runs in a row of
     the account path and two of the tree path: each output is the same
     refusal and holds no digest, so no run can supply anything to the
     next), `approve_in_a_run_step_is_refused` and
     `approve_tree_in_a_run_step_is_refused` (`APB_RUN_ID=x`),
     `an_escape_sequence_in_mcp_args_is_refused_at_install` (an `args`
     entry holding `ESC [ 2 J`: `mcp `args[0]` holds a control
     character`),
     `approve_of_an_http_connector_is_unchanged` (inside `APB_RUN_ID` and
     without a terminal it approves as on main).
  In `crates/apb-server/tests/suite/connectors_api_test.rs`:
  `the_dashboard_refuses_to_approve_an_mcp_connector` (with and without
  `account`: 403, the message starts `mcp_approve_needs_a_person:` and
  holds no digest, the trust store and `mcp-tools.json` unchanged) and
  `the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins` (the
  async handler answers ok, the stub got no `tools/call`). Then
  `crates/apb-cli/src/connector_mcp.rs` with `approve_mcp`, `Confirm`,
  `print_tree`, `print_program` and `print_pins` (design D2 "Capturing
  pins", D5 "The approve output"), `mod connector_mcp;` in
  `crates/apb-cli/src/main.rs`, the `approve_cmd` change on both paths
  (no new flag on `ConnectorAction::Approve`), `preview_start` in
  `connector/mcp/server.rs` and the refusal in
  `approve_connector_handler` (`crates/apb-server/src/routes/connectors/mod.rs`
  line 519). Keep the setup of these tests reusable: mcp-http-and-surfaces
  adds its tools and doctor tests here. Run `cargo test -p apb --bin apb
  connector_mcp`, `cargo test -p apb --test main connector_mcp_cli_test`
  and `cargo test -p apb-server --test main connectors_api_test`.
- [x] 8.2 Test in `crates/apb-engine/tests/suite/tool_exposure_test.rs`:
  `an_mcp_grant_adds_no_expected_server` (design D5 "Tool exposure"). It
  passes with no code change; if it does not, stop and report. Run
  `cargo test -p apb-engine --test main tool_exposure_test`.
- [x] 8.3 Docs: `docs/CONNECTORS.md` "MCP servers" (stdio only, with the
  three eval-doubles points of design D5, what the pin store guards
  against, and the end through the handle), the "Sending once" paragraph
  and the approve line of the CLI list; `docs/HOWTO-authoring.md`;
  `docs/ISOLATION.md` (design "Docs to update" items 1 to 3, the
  server's environment table, launchers, the person's approval and
  V301 and V302 included);
  `docs/HOST-INTEGRATION.md` line 18 and `docs/MCP.md` "Protocol
  compatibility and adoption plan" (design D6 item 5). Nothing in them
  claims a host was measured.
- [x] 8.4 Section check: `cargo test -p apb-core --lib connector`,
  `cargo test -p apb-core --lib validate`, `cargo test -p apb-mcp-client
  --features testing --test main stdio_test`, `cargo test -p apb-engine
  --test main connector_mcp`, `cargo test -p apb-engine --test main
  connector_mcp_send`, `cargo test -p apb-engine --test main
  connector_mcp_gate`, `cargo test -p apb --test main connector_mcp_cli_test`,
  `cargo test -p apb --test main mcp_revision_test` pass, and `git diff
  --stat` shows no change to `crates/apb-core/src/eval_doubles.rs`,
  `crates/apb-engine/src/eval/doubles.rs`,
  `crates/apb-core/src/validate/guarded.rs`,
  `crates/apb-engine/src/manifest.rs`,
  `crates/apb-engine/tests/fixtures/journal/known_events.jsonl` or
  `crates/apb-mcp/Cargo.toml`, and `Cargo.lock` still holds rmcp 3.9.0
  only. No run guard name is added (design review record, third pass
  item 3: an older apb fails every MCP call closed), so `guarded.rs`
  and the `GuardedFeature` counts stay as they are.
- [x] 8.5 Journal corpus row M1 in
  `crates/apb-cli/tests/suite/journal_corpus_test.rs` (design review
  record, third pass item 3), its playbook and connector inline and one
  paragraph in the module doc. The new binary installs the connector of
  design D1 "Example" with `command` the stub's absolute path, approves
  the tree through `TrustStore` and writes the account's pin record
  through `PinStore::record` with the stub's path and digests in the
  case's config dir (no binary approves an MCP connector without a
  terminal), and starts an inline
  playbook whose `human_review` gate comes before an `agent_task` node
  granted `tracker-mcp.list_comments`, with a stub agent
  (`APB_AGENT_CMD`, as rows D1 to D8) that runs `"$CORPUS_APB" connector
  call tracker-mcp list_comments --args '{}'`, writes its exit code and
  output to a file of the case and exits 0; the run waits at the gate
  and is saved. Each old binary (`v1.0.0-beta.1`, `v0.25.0`), on a fresh
  copy with `CORPUS_APB` set to itself: reads the run (exit 0); approves
  and resumes it, and the node runs, the recorded call exits non-zero
  with `snapshot is invalid` in its output, the stub's `starts.log` does
  not exist and the journal holds no `send_intent_recorded`; and refuses
  a new start of the playbook with `connector_unresolved`, again with no
  `starts.log`. The new binary's part runs without the old binaries.
  Run `cargo test -p apb --test main journal_corpus` without and, when
  `scripts/old-apb.sh` can build them, with `APB_OLD_BINARIES`.

## 9. Records

- [x] 9.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both
  (only CLAUDE.md's "Claude-specific" section has no twin): design "Docs to
  update" item 5.
- [x] 9.2 Write `docs/superpowers/specs/<implementation date>-mcp-tool-connectors-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] 9.3 Third-party notices (the new crate; rmcp is unchanged):
  `scripts/third-party-notices.sh`, then `--check`.
- [x] 9.4 Style over every changed doc: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![` and
  Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds
  nothing).

## 10. Report (written after section 11 passes)

- [x] 10.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Check that no stub you started still runs
  (`pgrep -f stub-mcp.sh`); a leftover is reported, not signalled by pid.
- [x] 10.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to 0.1
  to 0.6, both runs of 1.1, each gate of section 11 with its numbers,
  the user-visible changes for the release notes (design "Release
  notes"), the changed file count against design "Files" (77, ceiling
  80, at the time; 84 after fix round 1, design "Files") with every file
  outside that list named, the comment text
  for issue 57 and the three follow-up issue texts (design D6 item 6),
  and anything left open.

## 11. Gates

Run each once on the final tree, in this order, and record the numbers
(passed, failed, skipped, time) for the report. Fix every failure and
rerun the gate that failed until it is clean.

- [x] 11.1 `cargo fmt --all -- --check`
- [x] 11.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 11.3 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 11.4 `cargo test --workspace --doc`
- [x] 11.5 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; this change adds no mirrored type, so the generated
  files must be unchanged)
- [x] 11.6 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 11.7 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 11.8 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 11.9 `scripts/third-party-notices.sh --check`
- [x] 11.10 `openspec validate mcp-tool-connectors --strict`

## 12. Fix round 1 (design "Review record", "Fix round 1")

The owner decided every finding of the spec conformance review and the
security and test audit; each fix below is done and tested. The branch
was rebased onto origin/main 3bb424be with no conflicts first.

- [x] 12.1 The program that runs is the file that was hashed:
  `apb_mcp_client::ProgramFile` (one open, `O_NOFOLLOW | O_NONBLOCK`, the
  digest from that descriptor), `StartFrom::Descriptor` on Linux and
  `PathCheckedAgain` elsewhere, the engine's `mcp::program_file` for
  every start and digest; the swap, link and replacement tests.
- [x] 12.2 Interpreters and launchers in `MCP_LAUNCHERS` (30, versioned
  names), the V301 and approval wording, the manifest rule
  `mcp_env_loads_code` over `LOADER_ENV` and the `LD_` and `DYLD_`
  prefixes, the control-character check first in install errors.
- [x] 12.3 The server leads its own process group; the close kills the
  group while the server is unreaped and reports `GroupEnd`; `reap_late`;
  `PR_SET_PDEATHSIG` on Linux; a failed call closes before its
  settlement, the warning in front of the error; the group tests.
- [x] 12.4 A fresh private working directory, `PATH` with the program's
  directory last and `/opt/homebrew/bin` on macOS, the runs-as-you line
  of the approval.
- [x] 12.5 In the leaf module `connector/mcp/redact.rs`: `server_text`
  (redact whole, then cut to 500); the `isError`
  text never parsed, `redact_tree` for the listing check, its display
  and a tool's answer, the protocol version at most 64 characters, the
  approval screen's bounds and its summary above the question.
- [x] 12.6 `apb-server` takes the client with `testing` as a
  dev-dependency only, the `compile_error!` guard in the client, and
  `release_workflow_test::testing_features_stay_in_test_builds`.
- [x] 12.7 `send_once::gate(input, may_record)` and `NeedsStart`;
  `mcp_ready` cannot fail; `with_warning`;
  `send_once::tests::a_gate_that_may_not_record_records_nothing`.
- [x] 12.8 `CallFailure::NotSent` from the tap's `calls_handed`, settled
  `not_sent`; `MAX_SESSION_BYTES` and parsing only while a listing is
  awaited; `kill_child_helper` removed, every kill test run again in a
  child (`APB_MCP_SEND_KILL_CHILD`, `child_call`) with the `CallChild`
  and `Fifo` drop guards; the crash tests of the record and the hand-over;
  `a_changed_command_is_refused`; looser client timing bounds.
- [x] 12.9 The pin store load error as `store_unreadable` with its own
  remedy and fit row; `pins::program_status` and `compare` for the
  display comparisons; `PinStore::forget` on uninstall and revoke; an
  eval case with its own config dir refused live (documented); the end
  warning of an answer from the ledger; the `UNPINNED_MEMBERS` comment;
  the notices script without `cat -s`.
- [x] 12.10 Docs: the design, the public record, the spec deltas, this
  list, the proposal, `docs/CONNECTORS.md`, `docs/HOWTO-authoring.md`,
  `CLAUDE.md` and `AGENTS.md` brought in line with 12.1 to 12.9.
- [x] 12.11 Rerun gate 11.3 whole on the final tree of this round (7881 passed).

## 13. Closing (design "Review record", "Fix round 5" and "Rebased on main 0336dada")

- [x] 13.1 Fix round 5: the dropped-line count read once the session
  closed; `connector_mcp::a_line_written_after_the_answer_is_counted`
  and the stub option `set_call_suffix`.
- [x] 13.2 Rebased on main 0336dada (#292): V340 reconciled, a
  `{{params.*}}` placeholder refused in an `mcp:` block
  (`connector::mcp::tests::a_param_placeholder_is_refused_in_args_and_env`),
  `AGENTS.md` and `CLAUDE.md` mirrored with both sides' text.
- [x] 13.3 The change archived, each new capability with its Purpose in
  `openspec/specs`, `openspec validate --all --strict` passing, the public
  design record matching the final design.
- [x] 13.4 The gates of section 11 run whole on the final tree.
