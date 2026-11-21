# Tasks: MCP over HTTP and on every surface

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
exist yet; every other test must fail on a real assertion before the code
that makes it pass exists. So every section builds in this order: types
and changed signatures first (each step compiles), then a skeleton whose
functions do nothing where a test file needs them to exist, then the red
tests, then the behaviour one piece at a time. A task names the tests
that may pass before its code exists; every other test it writes must be
seen failing first.

Lines `design.md` gives are those of main f0340685, where
mcp-tool-connectors (#293) and eval-doubles (#291) are merged;
start-rules, inbox-waits, incremental-lists-and-session-fork and
one-point-oh-fixes merge before or beside this change. Find each site by
the name `design.md` gives and re-verify its line on the main you start
from. Tasks name symbols and files, never lines. `design.md` "Settled
decisions" holds the decisions the design review settled; build them as
written.

## Size

53 changed files by design "Files", at most 56 (target at most about
80). Sections 1 to 6 are the listing surfaces over stdio connectors
(about 20 files), sections 7 to 9 the HTTP transport (about 20), section
10 the docs and records.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-mcp-http-and-surfaces-target TMPDIR=$HOME/.cache/apb-mcp-http-and-surfaces-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-mcp-http-and-surfaces-target` fails with
a read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-mcp-http-and-surfaces-target`
and `TMPDIR=/tmp/apb-mcp-http-and-surfaces-tmp` instead, and before each
build check that `df -h /tmp` shows more than 4G free; if not, stop and
report it. `/tmp` is a RAM disk shared with other sessions. Never use
`/dev/shm`, and never put a target dir, cache or temp data inside the
worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR` (the existing test helpers do this). Never stop a process
you did not start. Do not commit, push or open a pull request. Do not
start subagents. Do not run any host or agent CLI (D8 is a written
procedure only). Never send anything to a server or a model without the
owner's go: the only network use allowed without asking is `cargo`
fetching crates and `bun install`. Every HTTP test server binds
`127.0.0.1`.

Search code with `zg --rg -F <symbol> <path>` first; verify an edit with
plain `rg`.

Inside a task run only the tests it names. Commands:

1. A unit test module: `cargo test -p <crate> --lib <module path>`
   (for example `cargo test -p apb-core --lib connector::mcp`).
2. An integration test file: `cargo test -p <crate> --test main <file
   module>`. The CLI package is named `apb`: `cargo test -p apb --test
   main connector_mcp_cli_test`. The client crate's tests need its
   feature: `cargo test -p apb-mcp-client --features testing --test main
   http_test`.
3. `cargo test` takes one name filter per invocation: two modules are two
   commands.

A new integration test file `crates/<crate>/tests/suite/<name>.rs` is
declared in `crates/<crate>/tests/main.rs`, in alphabetical place, as

```rust
#[path = "suite/<name>.rs"]
mod <name>;
```

and uses the shared helpers through `use crate::common;`
(`crates/apb-engine/tests/suite/common/mod.rs` line 435 `spawn_script`
for a scripted HTTP server that records each request's text).

## 0. Preconditions

Each item is checked by name (`zg --rg -F <symbol> <file>`, then the hit
read); no line number is given here, since lines move within this
change. If a symbol is missing or has another shape, stop and report it.

- [x] 0.1 mcp-tool-connectors is merged (#293) and archived
  (`openspec/changes/archive/2026-11-21-mcp-tool-connectors/`). These
  exist as named:
  1. `crates/apb-mcp-client/src/session.rs`: `pub enum Server` with
     `Stdio` and no `Http`; `pub enum FailureKind` with `Refused` and
     `ProgramChanged` and no `Auth`; `pub enum Ended` with `NotStarted`;
     `pub enum CallFailure` with `NotSent`, `Answered`, `Lost`; `fn
     same_listing`; `ClientLifecycleMode::Discover` in `Conn::open`;
     `set_response_cache_config`.
  2. `crates/apb-mcp-client/src/tap.rs`: `pub const MAX_LINE_BYTES`, `pub
     const MAX_SESSION_BYTES`, `pub(crate) const MAX_LINES`,
     `MAX_DROPPED_LINES`, `pub(crate) struct LineTap` with `fn
     classify_line`, `fn observe`, `pub(crate) fn seal`, `fn forward`,
     `fn refuse_incoming`, `pub(crate) fn take_last_listing`,
     `pub(crate) fn dropped`; `struct WriteGate` with `calls_handed` and
     `sealed`.
  3. `crates/apb-mcp-client/src/frame.rs`: `pub(crate) fn classify`,
     `pub(crate) fn incoming`, `pub(crate) const UTF8_BOM`.
  4. `crates/apb-mcp-client/src/stdio.rs`: `pub enum EnvValue`; and
     `crates/apb-mcp-client/Cargo.toml`: `rmcp = { version = "=3.9.0"`
     with `"client", "transport-async-rw"`.
  5. `crates/apb-core/src/connector/mcp/mod.rs`: `pub struct
     McpServerSpec` (no `url`; its doc sentence "mcp-http-and-surfaces
     adds the http fields `url` and `auth` here"), `pub fn
     validate_server_shape` with the text "mcp transport `http` is not
     supported by this apb", the test `http_transport_is_refused_by_this_apb`.
  6. `crates/apb-core/src/connector/mcp/pins.rs`: `pub struct PinStore`,
     `pub struct ServerPin`, `pub enum ProgramFact` with `NotRead`, `Gone`,
     `Digest`, `pub struct LiveBinding`, `pub fn compare`, `pub fn
     program_status`; `crates/apb-core/src/connector/mcp/digest.rs`: `pub
     fn pin_digest`, `pub fn tool_digest`.
  7. `crates/apb-core/src/connector/def.rs`: `fn validate_mcp_connector`
     with "has an `mcp:` block and must not set", `fn
     mcp_function_shape` (with `scopes` among the HTTP fields), `pub fn
     validate_templates`, `pub fn sole_secret`, `pub enum AuthSpec`.
  8. `crates/apb-engine/src/connector/mcp/mod.rs`: `pub(crate) fn
     check_pin`, `pub(crate) fn check_tools`, `pub(crate) struct
     CheckedPin` with `fn compare` and `fn refusal` (its
     `mcp_tool_changed` text), `pub(crate) fn open`, `pub(crate) struct
     Opened` with `fn verify`, `pub(crate) struct Start`, `pub(crate) fn
     open_program`, `pub(crate) fn connect`, `pub(crate) struct
     Connected`, `pub(crate) struct SessionHandle` with `close_noted`,
     `pub(crate) struct McpCall` with `fn endpoint`, `pub(crate) fn
     dry_run`, `pub(crate) fn end_warning`, `pub(crate) fn dropped_note`,
     `pub mod pin`.
  9. `crates/apb-engine/src/connector/mcp/server.rs`: `pub struct
     StartPreview`, `pub fn preview_start`, `pub(crate) fn stdio_server`,
     `fn config_error`; `crates/apb-engine/src/connector/mcp/map.rs`: `fn
     composed`, `pub(crate) fn connect_error`, `pub(crate) fn answer`, `fn
     not_sent`, `fn lost`.
  10. `crates/apb-engine/src/connector/mcp/pin.rs`: `pub struct ToolRow`,
      `pub struct ProgramFacts`, `pub struct ToolListing` with `cleanup`
      and the private `pins`, `pub struct ConfirmedProgram`, `pub fn
      resolve_program`, `pub fn list_live`, `fn cleanup_note`, `fn
      build_listing`, `pub fn record`, `pub(crate) fn healthcheck`, the
      private `fn check_live`.
  11. `crates/apb-cli/src/connector_mcp.rs`: `pub(crate) fn approve_mcp`,
      `pub(crate) trait Confirm`, `pub(crate) fn refusal`,
      `pub(crate) fn print_program`, `fn server_lines`, `pub(crate) fn
      print_pins`, `fn approve_account` (its `pin::resolve_program`
      call).
  12. Tests: `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs` (its
      `Fx`, `assert_untouched`, `approve_without_a_terminal_is_refused`),
      `crates/apb-engine/tests/suite/connector_mcp.rs` (`Fx::pin`,
      `no_refusal_carries_a_digest`),
      `crates/apb-server/tests/suite/connectors_api_test.rs`
      (`install_mcp`,
      `the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins`).
- [x] 0.2 These symbols of main exist as named: `enum ConnectorAction`,
  `Test`, `fn connector_cmd`, `fn approve_cmd`, `fn doctor_cmd`
  (`crates/apb-cli/src/connector.rs`); `pub fn connectors_list`
  (`crates/apb-mcp/src/tools/meta.rs`) and its `ReadOnlyUse::Full` row
  (`crates/apb-mcp/src/server/read_only.rs`); `get_connector_handler`,
  `healthcheck_connector_handler`, `pub(crate) fn merged_accounts`,
  `pub(crate) fn account_scope` (`crates/apb-server/src/routes/connectors/mod.rs`);
  the route `/api/connectors/{name}/healthcheck/{account}`
  (`crates/apb-server/src/lib.rs`); `pub(crate) fn resolve_root`,
  `pub(crate) fn enumerate_workspaces` (`crates/apb-server/src/state.rs`);
  `pub enum ExpectKind`, `pub fn resolve` (`crates/apb-core/src/connector/contract.rs`);
  `fn json_subset`, `fn eval_inbox`,
  `kind_mismatch_between_function_and_expectation_fails`
  (`crates/apb-engine/src/connector/contract_test.rs`); `fn
  check_token_url`, `pub(crate) fn render`, `pub(crate) fn access_token`,
  `pub(crate) fn refresh_now`, `fn refresh_request`, `fn cache_key`
  (`crates/apb-engine/src/connector/oauth/token.rs`); the two callers of
  `TokenSource::render` (`crates/apb-engine/src/connector/call/mod.rs`
  with the first mint's `ROTATION_WARNING` and `Redaction::new` pushes
  right after it; `crates/apb-engine/src/connector/imap.rs`); in
  `crates/apb-engine/src/connector/call/http.rs` `fn send_raw` (its
  `max_redirects(0)` agent), the 401 `refresh_now` retry in `fn send`,
  `fn register_redaction`, `fn auth_header`; `pub(crate) fn validate_url`
  (`crates/apb-engine/src/connector/call/render.rs`); `CallErrorCode::Auth`
  and `CallErrorCode::ToolError` (`crates/apb-engine/src/connector/result.rs`);
  `fn guard_refusal`, `fn settle_send`, `fn answer_live`, `fn
  build_prepared` (`crates/apb-engine/src/connector/call/mod.rs`); `pub
  fn refusal` (`crates/apb-core/src/step_env.rs`); `ExitCode::from(3)`
  (`crates/apb-cli/src/health.rs`); `pub fn classify` with
  `CallErrorCode::ToolError` in its `undetermined` expression
  (`crates/apb-engine/src/connector/effect.rs`; if it is not there, stop
  and report it: this change relies on it and adds no other);
  `pub(super) fn preview`, `pub(super) fn gate`, `SendGate::NeedsStart`,
  `pub(super) fn finish` (`crates/apb-engine/src/connector/call/send_once.rs`);
  `pub fn resolve_reference` (`crates/apb-core/src/connector/secrets.rs`);
  `pub fn render_raw`, `pub struct RenderCtx`
  (`crates/apb-core/src/connector/template.rs`); `pub fn
  validate_accounts`, `pub fn account_digest`, `pub fn load_merged`,
  `pub fn cmd_refs` (`crates/apb-core/src/connector/config.rs`); `pub
  fn plan`, `fn exchange` (`crates/apb-engine/src/connector/oauth/consent.rs`);
  the testing gate `#[cfg(all(debug_assertions, any(test, feature =
  "testing")))]` (`crates/apb-core/src/billing.rs`) and the release
  refusal of the feature (`compile_error!` in `crates/apb-core/src/lib.rs`);
  `pub fn spawn_http` and `pub fn spawn_script`
  (`crates/apb-engine/tests/suite/common/mod.rs`); `pub(crate) fn
  sanitize_for_terminal` (`crates/apb-cli/src/util.rs`);
  `fetchConnector`, `runConnectorHealthcheck` (`web/src/lib/api/connectors.ts`);
  `safeExternalUrl` (`web/src/pages/ConnectorView.svelte`).
- [x] 0.3 `Cargo.lock` holds rmcp 3.9.0 only and ureq 3.5.2. In
  `~/.cargo/registry/src/*/rmcp-3.9.0/` check each name of design "The
  rmcp client this design uses": the feature
  `transport-streamable-http-client`, `trait StreamableHttpClient` with
  `post_message`, `delete_session`, `get_stream`, `enum
  StreamableHttpPostResponse` with `Accepted` and `Json`,
  `StreamableHttpClientTransport::with_client`,
  `StreamableHttpClientTransportConfig::with_uri`,
  `max_concurrent_requests`, `reinit_on_expired_session`. In
  `~/.cargo/registry/src/*/ureq-3.5.2/` check `pub fn proxy(mut self, v:
  Option<Proxy>)` on the config builder. If one is missing, stop and
  report it.
- [x] 0.4 None of these exists: `crates/apb-mcp-client/src/http.rs`, a
  `url` field of `McpServerSpec`, `fn list_approved`, `fn check_server`,
  `fn listing_view`, `fn endpoint_digest`, `fn http_program`, `Tools` in
  `ConnectorAction`, `mcp_tools_handler`, `McpExpect`,
  `web/src/lib/McpToolsSection.svelte`,
  `crates/apb-engine/tests/suite/connector_mcp_http.rs`. If one exists,
  it is a partial earlier attempt: review it against `design.md`.
- [x] 0.5 One validator code is added, V303: note the output of `rg -o
  --no-filename '"V3[0-9][0-9]"' crates | sort -u` (V300, V301, V302 and
  V340 on f0340685) now and check it again in 10.2 (then V303 added,
  nothing else). Note the merged-main
  commit you start from. Note `rg -n 'McpServerSpec \{' crates` (only
  the definition on f0340685).

## 1. `list_approved` and `listing_view` (design D5 "`list_approved`", spec mcp-connector-listing)

- [x] 1.1 Types first: `ListingView` (with `cleanup`), `ServerView`,
  `RowView` and a `listing_view` skeleton in
  `crates/apb-engine/src/connector/mcp/pin.rs`. Test first in its tests
  module: `listing_view_keeps_out_every_unlisted_field` (a `ToolListing`
  built through the module's own fixture, whose serialized form carries
  `connector_digest`, `account_digest`, `pin_digest`,
  `program.program_digest` and a row `digest`: the view's key sets at the
  top, in `server` and in each row are exactly the named fields of
  `ListingView`, `ServerView` and `RowView`, no member of the listing
  outside them appears at any level, the text holds no `sha256:`). Then
  `listing_view`, starting with the exhaustive destructuring (no `..`) of
  `ToolListing`, `ProgramFacts` and `ToolRow` that design D5 names. Run
  `cargo test -p apb-engine --lib connector::mcp::pin`.
- [x] 1.2 `check_server` and `CheckedPin::compare_server` in
  `crates/apb-engine/src/connector/mcp/mod.rs` (`check_tools` becomes
  `check_server` plus its unchanged per-tool loop), then `list_approved`
  in `pin.rs` over `check_server`, `open`, `cleanup_note` and
  `build_listing`. `pin::healthcheck` and its private `check_live` are
  not changed. Tests first in `pin.rs` tests:
  `list_approved_marks_each_row` (a pinned, a changed, an unpinned and a
  missing tool, through the module's stub fixture),
  `list_approved_of_an_unpinned_account_starts_nothing` (no record:
  `mcp_server_unpinned`, the stub's start count 0, the helper's marker
  absent), `list_approved_writes_nothing` (the config files
  byte-identical, as `list_live_writes_nothing` checks). The existing
  healthcheck tests are the guard (they pass before and after): run
  `cargo test -p apb-engine --lib connector::mcp`, `cargo test -p
  apb-engine --test main connector_mcp` and `cargo test -p apb-server
  --test main connectors_api_test`.

## 2. `apb connector tools`, the refusal text and the doctor rows (design D5, spec mcp-connector-listing)

- [x] 2.1 Tests first in `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs`
  (its `Fx` setup: stub, connector, account; the approval written with
  `PinStore::record` of a `ServerPin` built from `digest::tool_digest`
  and `digest::pin_digest`, as `connector_mcp.rs` `Fx::pin` and
  `connectors_api_test.rs` `install_mcp` do, since no test can pass the
  terminal confirmation):
  `tools_exits_0_when_all_pinned`,
  `tools_lists_each_row_and_exits_3_when_changed` (after `set_tools`
  appends one word to a description: the row says `changed`, exit 3, the
  output holds no `apb connector approve`),
  `tools_json_carries_no_digest` (`--json`: rows present, no `sha256:`,
  no run of 16 or more hexadecimal characters),
  `tools_of_an_unapproved_account_starts_nothing` (no record: exit 2,
  `mcp_server_unpinned`, `assert_untouched`, a `{{cmd:...}}` helper's
  marker file absent),
  `tools_in_a_run_step_is_refused` (`APB_RUN_ID` set: exit 2,
  `assert_untouched`),
  `tools_on_a_connector_without_mcp_exits_2`,
  (`a_changed_tool_refusal_names_the_listing` lives in
  `crates/apb-engine/tests/suite/connector_mcp.rs`, which has the run
  fixture and `no_refusal_carries_a_digest`: one call of the changed
  tool inside a run: the error holds `apb connector tools`, no `apb
  connector approve`, no digest),
  `doctor_shows_pin_rows` (one `ok` row for the approved account, one
  `warn` row for an account without a record, no `apb connector approve`
  in the output), `doctor_starts_no_server` (`assert_untouched`).
  In `crates/apb-cli/src/connector_mcp.rs` tests:
  `print_pins_is_unchanged_by_the_digest_flag` (may pass before the
  code: it pins `print_pins`'s lines over the module's fixture listing).
  Then `ConnectorAction::Tools`, `tools_cmd` and the `digests` parameter
  of `server_lines` in `crates/apb-cli/src/connector_mcp.rs`, the
  dispatch in `connector_cmd`, the doctor rows in `doctor_cmd`, and the
  `mcp_tool_changed` text in `CheckedPin::refusal`
  (`crates/apb-engine/src/connector/mcp/mod.rs`). Run `cargo test -p apb
  --test main connector_mcp_cli_test`, `cargo test -p apb --bin apb
  connector_mcp` (the approval tests of mcp-tool-connectors, unchanged,
  still green) and `cargo test -p apb-engine --test main connector_mcp`
  (`no_refusal_carries_a_digest` and the new
  `a_changed_tool_refusal_names_the_listing` green). For an
  `oauth2_refresh` connector `tools_cmd` prints the rotation note of
  design D5 "`apb connector tools`" item 4.

## 3. MCP `connectors_list` (design D5 "MCP `connectors_list`", spec mcp-connector-listing)

- [x] 3.1 Tests first in `crates/apb-mcp/tests/suite/read_tools_test.rs`:
  `connectors_list_names_mcp_tools_without_starting_a_server` (the
  `mcp` block, `mcp_tool`, `approved_accounts` holding the approved
  account only, no digest, the stub's `starts()` empty) and
  `connectors_list_reports_a_pin_store_error` (an unparsable
  `mcp-tools.json`: `pins_error` set, the listing still answers). Then the
  fields in `crates/apb-mcp/src/tools/meta.rs`. Run `cargo test -p apb-mcp
  --test main read_tools_test`.

## 4. The HTTP API (design D5 "Dashboard" items 1 and 2, spec mcp-connector-listing)

- [x] 4.1 Tests first in `crates/apb-server/tests/suite/connectors_api_test.rs`:
  `the_connector_detail_carries_the_mcp_block` (transport, command, env
  names, the account row with `approved: true`, no digest),
  `the_mcp_tools_route_lists_live_rows` (`ok: true`, rows with status,
  no digest), `the_mcp_tools_route_writes_no_pin` (`mcp-tools.json`
  byte-identical before and after), and
  `the_mcp_tools_route_refuses_an_unapproved_account` (`ok: false`,
  `mcp_server_unpinned`, the stub's `starts()` empty), and
  `two_projects_with_the_same_account_name_are_two_rows` (design D5
  wrong implementation 9: two registered projects each with an account
  `work` of different fields, only the first approved; the machine-wide
  detail holds two `work` rows with their `workspace` and `root_label`,
  `approved` true for the first and false for the second; the route with
  the first's `workspace` lists that project's server, which sees the
  request while the second's sees none; with the second's `workspace` it
  is refused `mcp_server_unpinned` and nothing starts; without
  `workspace` it answers 400 and nothing starts),
  `the_mcp_tools_route_refuses_a_keyless_dashboard_and_a_bearer` and
  `the_mcp_healthcheck_route_refuses_a_keyless_dashboard_and_a_bearer`
  (each 403, the stub's `starts()` empty; the other route tests and the
  existing MCP healthcheck test run on a signed-in session). Then the
  session-only rule of design D5 "Dashboard" items 2 and 6 (the listing
  route, and the merged healthcheck handler for a connector with
  `mcp:`), the `mcp` field of
  `get_connector_handler` with its per-project account rows (not through
  `merged_accounts`), `mcp_tools_handler` with its strict `workspace`
  resolution over `pin::list_approved` and its route in
  `crates/apb-server/src/lib.rs`, after the healthcheck route.
  The tests pin through `install_mcp`'s `PinStore::record`, as
  `the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins` does. Run `cargo test -p apb-server --test
  main connectors_api_test`.

## 5. The dashboard (design D5 "Dashboard" items 3 to 5, spec mcp-connector-listing)

- [x] 5.1 Tests first: `web/src/lib/mcpTools.test.ts` (`statusLabel
  names every status`, `needsApproval is true for every status but
  pinned`) and `web/src/lib/McpToolsSection.test.ts` (`shows the rows and
  no approve command`, `a description is rendered as text`, both with
  `render` from `svelte/server` as `web/src/pages/Login.test.ts` does,
  and `each account row names its project`: two rows `work` with labels
  `alpha` and `beta` both shown, each "List tools" bound to its own
  `workspace`). Then the types (`McpAccountView` with `workspace` and
  `root_label`) and `listMcpTools` in `web/src/lib/api/connectors.ts`,
  `web/src/lib/mcpTools.ts`, `web/src/lib/McpToolsSection.svelte` and its
  mount in `web/src/pages/ConnectorView.svelte`. Plain CSS over the
  existing CSS variables in the component's `<style>` block; no new style
  dependency. Run `cd web && bun run test && bun run check`.

## 6. Contract tests (design D5 "Contract tests", spec mcp-connector-listing)

- [x] 6.1 Tests first: in `crates/apb-core/src/connector/contract.rs`
  tests `an_mcp_expectation_resolves`; in
  `crates/apb-engine/src/connector/contract_test.rs` tests
  `an_mcp_case_checks_tool_and_arguments`,
  `an_mcp_case_shows_the_marker_preview`, `a_tool_mismatch_fails`,
  `an_mcp_case_against_an_http_function_fails`. Then `McpExpect`,
  `Expectation.mcp`, `ExpectKind::Mcp`, the `resolve` arm, the dispatch
  arm and `eval_mcp`. Run `cargo test -p apb-core --lib
  connector::contract` and `cargo test -p apb-engine --lib
  connector::contract_test`.
- [x] 6.2 Section check: `cargo test -p apb --test main
  connector_mcp_cli_test`, `cargo test -p apb-mcp --test main
  read_tools_test`, `cargo test -p apb-server --test main
  connectors_api_test`, `cargo test -p apb-engine --lib
  connector::contract_test`, `cargo test -p apb-engine --test main
  connector_mcp` pass.

## 7. The HTTP manifest (design D7 "Manifest", spec mcp-http-transport)

- [x] 7.1 Types first: `McpServerSpec.url` and the corrected doc
  sentences of `McpServerSpec` and `McpTransportKind::Http` (design D7
  "Manifest"); `rg -n 'McpServerSpec \{' crates` finds no struct literal
  on f0340685, so add `url: None` only where it finds one on your base.
  Then tests in `crates/apb-core/src/connector/mcp/mod.rs` tests:
  `an_http_connector_loads` (header auth),
  `an_http_connector_with_oauth2_refresh_loads`,
  `an_http_block_with_args_is_refused`, `a_stdio_block_with_url_is_refused`,
  `http_needs_protocol_modern`, `a_secret_in_the_url_is_refused`,
  `the_url_needs_a_literal_scheme`,
  `a_plaintext_url_is_refused` (`http://crm.example/mcp`,
  `http://localhost:8080/mcp`, `http://127.0.0.1:8080/mcp`,
  `{{account.server_url}}`: `https_only` refuses all four, `url_scheme_ok`
  admits only the third, through its test-build arm),
  `a_control_character_in_the_url_is_refused`,
  `http_takes_header_or_oauth2_refresh_only` (`query`, `path`, `basic`
  refused, each naming the kind), `stdio_still_refuses_auth` (the
  stdio text "has a stdio `mcp:` block and must not set `auth`"; the
  existing `auth_and_webhook_are_refused_with_an_mcp_block` keeps its
  `webhook` case and moves its `auth` case to this test). In
  `crates/apb-core/src/validate/mcp.rs` tests:
  `a_reserved_auth_header_is_v303` and `an_ordinary_auth_header_is_not_v303`
  (design D7 "Manifest" rule 7). In
  `crates/apb-core/src/connector/mcp/digest.rs` tests:
  `the_endpoint_digest_covers_the_token_url` (against a base of an
  endpoint and a token URL: a changed endpoint, a changed token URL and
  no token URL each give a different digest; the same inputs give the
  same digest). Remove `http_transport_is_refused_by_this_apb` and its
  refusal. Then `https_only`, `url_scheme_ok` (the test-build arm under
  the existing gate of design D7 "The https rule"; no new feature,
  variable or switch), `endpoint_digest`, the rules in
  `validate_server_shape`, the connector-level rule in
  `validate_mcp_connector` and `validate_templates` (`def.rs`) with the
  rule-15 doc of `ConnectorDoc::from_yaml`, `RESERVED_HTTP_HEADERS` with
  `reserved_http_header`, `ConnectorFacts.mcp_reserved_header`
  (`resolve.rs`) and V303. Run `cargo test -p apb-core --lib
  connector::mcp`, `cargo test -p apb-core --lib connector::def` and
  `cargo test -p apb-core --lib validate::mcp`.

## 8. The HTTP client (design D7 "The client", spec mcp-http-transport)

- [x] 8.1 Types and skeleton: the rmcp feature and `ureq` in
  `crates/apb-mcp-client/Cargo.toml`, `src/http.rs` with `HttpServer`,
  `HttpFault` and `UreqClient` (methods answering `HttpFault::Transport`),
  `Server::Http`, `FailureKind::Auth`, the `Ended::NotStarted` doc, and
  in `src/tap.rs` the four entries `classify_message`, `start_call`,
  `read_frame` and `read_body` over the existing private functions, with
  their two `TapState` counters; it compiles and every stdio test passes
  (`cargo test -p apb-mcp-client --features testing --lib` and `--test
  main stdio_test`).
- [x] 8.2 Tests first in `crates/apb-mcp-client/src/tap.rs` tests:
  `a_frame_read_over_http_is_captured_as_a_line_is` (the same listing
  answer through `read_frame` and through the reader: the same captured
  result), `start_call_after_the_seal_starts_nothing` (`false`, count
  unchanged). Then in `crates/apb-mcp-client/tests/suite/http_test.rs`
  (new, declared in `tests/main.rs`) over a scripted HTTP server in the
  test file (std `TcpListener` on `127.0.0.1`, one canned answer per POST
  by JSON-RPC method, every request's headers and body recorded):
  `a_modern_http_server_lists_and_calls`,
  `the_raw_tools_list_is_captured` (a tool with an `execution` member:
  `list_tools` returns it),
  `the_headers_and_bearer_are_sent` (`Accept`, `Content-Type`, the
  protocol version header, `Mcp-Method`, the server header, `Authorization:
  Bearer <token>`),
  `the_callers_proxy_is_ignored`, `a_redirect_is_not_followed`,
  `a_401_while_connecting_is_auth`,
  `a_401_on_the_call_is_lost_auth`,
  `a_json_rpc_error_is_answered_rejected`,
  `an_event_stream_answer_is_read` (two notifications then the response),
  `an_event_stream_past_the_message_bound_is_too_large`,
  `a_large_answer_is_too_large`,
  `a_session_id_from_the_server_is_refused`,
  `close_of_an_http_session_is_not_started`,
  `an_http_session_works_inside_a_tokio_runtime`,
  the strict-reader tests of design D7 wrong implementations 15 to 18 and
  21: `a_byte_order_mark_body_never_reaches_rmcp`,
  `a_duplicated_member_in_a_body_is_refused`,
  `garbage_after_the_json_body_is_refused`,
  `a_bad_event_before_the_answer_hands_on_nothing`,
  `an_unknown_event_field_is_refused`,
  `a_priming_event_without_data_is_skipped`,
  `the_field_value_loses_one_leading_space`,
  `the_stream_is_closed_after_the_answer`,
  `a_media_type_with_parameters_is_read`,
  `an_accepted_request_is_a_transport_fault`,
  `nothing_is_posted_after_the_seal`,
  `a_tool_with_a_header_annotation_fails_the_http_listing_closed`,
  `an_unknown_rmcp_header_is_refused`,
  `a_reserved_server_header_is_refused`,
  `a_refused_connection_after_the_call_started_is_lost`,
  `a_call_after_the_seal_is_not_sent_and_never_posted`,
  `no_fault_echoes_the_server`, `a_plain_text_body_ends_the_session`.
  Each of these must fail with its rule removed (the reader skipped, the
  stream handed on event by event, the count moved after the connect, a
  fault naming the content type, the drop rule applied), checked once by
  hand and noted in the report. The proxy test runs its body in a child
  process of the test binary (design D7 wrong implementation 4), never
  setting a proxy variable in the test process. Then the behaviour of `UreqClient`, its
  event-stream reader and the session's http arms. Run `cargo test -p
  apb-mcp-client --features testing --test main http_test`, `cargo test
  -p apb-mcp-client --features testing --lib` and `cargo test -p
  apb-mcp-client --features testing --test main stdio_test` (unchanged,
  still green).

## 9. The HTTP call path (design D7 "The engine", "Settlement of an HTTP write", "The approval over HTTP", "The four orders over HTTP", spec mcp-http-transport)

- [x] 9.1 Tests first: in
  `crates/apb-engine/src/connector/mcp/server.rs` tests
  `http_target_refuses_plaintext_and_a_query` (a rendered
  `http://localhost:<port>/mcp`, an endpoint with a query, a plaintext
  token URL: each `config`),
  `resolve_endpoint_renders_account_fields`,
  `preview_start_masks_the_header_secret`,
  `http_target_digest_is_the_endpoint_digest`,
  `http_target_refuses_whitespace_in_a_rendered_url`; in
  `crates/apb-engine/src/connector/mcp/map.rs` tests
  `a_lost_auth_is_tool_error_for_a_write_and_auth_for_a_read` and
  `no_http_failure_of_a_write_classifies_failed` (every row of design D7
  "Settlement of an HTTP write" through `map::answer` and then
  `effect::classify(.., false)` with `http_status: None`: `WriteUnknown`
  each) and `a_not_sent_http_call_is_not_sent_only_before_the_post` (the
  `NotSent` row stays `not_sent`; a `Lost(Transport)` of a refused
  connection is `network`); in a new tests module of
  `crates/apb-engine/src/connector/oauth/token.rs`
  `a_no_proxy_source_sets_no_proxy` (with `HTTPS_PROXY` and `HTTP_PROXY`
  set, the agent config a `TokenProxy::None` source builds has no proxy)
  and `adopt_minted_registers_both_tokens_and_warns_on_rotation` (may
  pass before the move: it pins the behaviour the move keeps). Then
  `resolve_endpoint`, `http_target` (with its endpoint digest),
  `http_server`, `HttpPreview` and the http `preview_start`, and the `map`
  row; then `TokenProxy`, the `proxy`
  parameter of `TokenSource::render` with `TokenProxy::Caller` at its two
  existing callers (`call/mod.rs`, `imap.rs`), and `adopt_minted` with
  the code of `http.rs` and `call/mod.rs` moved into it (design D7 "The
  engine" item 2). `effect.rs` and `send_once.rs` stay unchanged. Run
  `cargo test -p apb-engine --lib connector::mcp`, `cargo test -p
  apb-engine --lib connector::oauth` and, unchanged and still green,
  `cargo test -p apb-engine --test main connector_oauth`, `cargo test -p
  apb-engine --test main connector_call` and `cargo test -p apb-engine
  --test main connector_imap` (the HTTP, OAuth and IMAP paths whose code
  moved or gained the argument).
- [x] 9.2 Tests first in a new `crates/apb-engine/tests/suite/connector_mcp_http.rs`
  (declared in `crates/apb-engine/tests/main.rs`; `common::spawn_script`
  for the MCP endpoint and the token endpoint; the pin record written with
  `PinStore::record` of a `ServerPin` whose `program` is the rendered
  endpoint and `program_digest` its `digest::endpoint_digest`, as
  `connector_mcp.rs` `Fx::pin` builds its record):
  `a_header_server_gets_its_header`,
  `an_oauth_call_mints_and_sends_the_bearer`,
  `a_401_at_connect_refreshes_once` (two token requests, two discover
  POSTs, then the call; the token endpoint answers A, then B with a
  rotated refresh token: the first discover POST carries `Authorization:
  Bearer A`, the second `Bearer B`, and no request after the refresh
  carries A),
  `a_listing_that_echoes_the_refreshed_token_is_refused` (the first
  discover answers 401, the refresh returns B and a rotated refresh
  token, and the `tools/list` after the reconnect echoes B in a
  description; a second case echoes the rotated refresh token: each
  refused `mcp_tools_reflect_a_secret`, no `tools/call`, neither value in
  the journal or the result),
  `the_token_request_ignores_the_callers_proxy` (`HTTPS_PROXY` and
  `HTTP_PROXY` pointed at a listener that records connections, set as
  `http_test.rs::the_callers_proxy_is_ignored` sets them; an
  `oauth2_refresh` call mints, connects and calls: the token endpoint saw
  its request, the listener saw no connection),
  `a_401_on_the_call_settles_unknown` (a `none` write: the journal's
  `send_intent_settled.outcome` is `unknown`, a repeat of the same intent
  is refused `unknown_effect`, the server saw one `tools/call`),
  `every_failure_of_an_http_write_after_the_hand_over_settles_unknown`
  (one marker write per row of design D7 "Settlement of an HTTP write":
  401, 403, 500 with a JSON-RPC error body, 500 with a plain body, an
  event stream cut before the answer, the deadline; each asserts the
  journal's `send_intent_settled.outcome == "unknown"`, not only the
  `connector_call.outcome`),
  `a_changed_account_url_sends_nothing` (the account's `server_host`
  edited after approval),
  `a_url_with_a_query_is_refused`,
  `a_refused_endpoint_starts_no_secret_helper` and
  `a_refused_token_url_starts_no_secret_helper` (the account's secret is a
  `{{cmd:...}}` helper that writes a marker file; the record written by
  `pin::record` for the definition and account digests, as the file's
  setup does; the endpoint refused through a query in an account field,
  the token URL through a plaintext `http://localhost:<port>/token`; a
  call and a `pin::list_approved` each refused `config`, the marker file
  absent, the scripted servers saw nothing),
  `a_changed_execution_member_refuses_the_next_http_call` (the scripted
  server saw zero `tools/call` requests after the changed listing),
  `the_token_never_reaches_the_journal_or_the_result`,
  `a_listing_that_reflects_the_access_token_is_refused`,
  `the_endpoint_is_journaled_without_secrets` (`connector_call.url` is
  `mcp+http://127.0.0.1:<port>/mcp#<tool>` and `tool_digest` set),
  `a_dry_run_of_an_http_function_sends_nothing`,
  `a_duplicated_tool_member_never_matches_a_pin`,
  `a_connection_refused_on_the_call_settles_unknown`,
  `an_endpoint_digest_that_differs_refuses_the_call` (the record's
  `program_digest` edited: `mcp_server_changed`, neither scripted
  endpoint saw a request),
  `the_healthcheck_of_an_http_connector_checks_the_pins` (the healthcheck
  over the http arm: ok with `tools_checked`, no `tools/call`),
  `an_http_check_opens_no_program_file` (design D7 "The engine" item
  3.1). The proxy tests run their bodies in a child process (design D7
  wrong implementation 4). Then the
  http arms of `check_tools` (and so `check_pin` and `check_server`),
  `open` (`open_http`, `adopt_minted`, the one 401 refresh, the token
  URL check of the source), `connect`
  (`Connected.endpoint`) and `dry_run`. Run `cargo
  test -p apb-engine --test main connector_mcp_http` and `cargo test -p
  apb-engine --test main connector_mcp`.
- [x] 9.3 The approval of an http server. Test first in
  `crates/apb-cli/src/connector_mcp.rs` tests (scripted `Confirm`):
  `the_http_program_confirmation_shows_the_endpoint_and_token_url` (the
  first question declined: the output holds the endpoint, the token
  endpoint, the client id and the masked header, and the scripted token
  endpoint and MCP endpoint saw no request). In
  `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs`:
  `approve_of_an_http_mcp_connector_without_a_terminal_is_refused` (and
  inside a run step: `mcp_approve_needs_a_person`, `mcp-tools.json`
  absent, the scripted endpoints saw nothing; may pass before the code,
  since the terminal check comes first, and guards that it stays first).
  In `crates/apb-engine/src/connector/mcp/pin.rs` tests:
  `list_live_of_an_http_server_refuses_a_changed_endpoint` (the account
  edited after the confirmation to another https host:
  `mcp_approve_account_changed`, nothing sent). Then `pin::http_program`,
  its call in `approve_account`, the http arms of `print_program`,
  `list_live` and `record`, and the doc comments of `ServerPin` in
  `crates/apb-core/src/connector/mcp/pins.rs`. Run `cargo test -p apb
  --bin apb connector_mcp`, `cargo test -p apb --test main
  connector_mcp_cli_test` and `cargo test -p apb-engine --lib
  connector::mcp::pin`.
- [x] 9.4 The http line of the dashboard. Test first: extend
  `the_connector_detail_carries_the_mcp_block` with an http connector
  (`url` template and `auth` kind, no `command`). Then the http fields of
  the `mcp` block and of `McpToolsSection`. Run `cargo test -p apb-server
  --test main connectors_api_test` and `cd web && bun run test`.
- [x] 9.5 Third-party notices: `scripts/third-party-notices.sh`, then
  `scripts/third-party-notices.sh --check` (rmcp's `client-side-sse`
  adds crates). Name every crate `Cargo.lock` gained in the report.
- [x] 9.6 Section check: `cargo test -p apb-core --lib connector::mcp`,
  `cargo test -p apb-mcp-client --features testing --test main
  http_test`, `cargo test -p apb-engine --test main connector_mcp_http`,
  `cargo test -p apb-engine --test main connector_mcp`, `cargo test -p
  apb-engine --lib connector::oauth`, `cargo test -p apb --bin apb
  connector_mcp` pass.

## 10. Docs and records (design "Docs to update", D8)

- [x] 10.1 `docs/CONNECTORS.md`: "MCP servers" (the http part and the
  listing surfaces), "OAuth accounts", "Contract tests", "The `apb
  connector` CLI" (design "Docs to update" item 1).
- [x] 10.2 `docs/HOST-INTEGRATION.md`: the D8 section and the pointer
  from line 18's paragraph. No host CLI is run; the results table stays
  empty. Check again: `rg -o --no-filename '"V3[0-9][0-9]"' crates |
  sort -u` prints what 0.5 noted, and `git diff --stat` shows no change to
  `crates/apb-engine/src/event.rs`, `manifest.rs`, `guarded.rs`,
  `crates/apb-core/src/validate/` (but `validate/mcp.rs`, V303),
  `oauth/consent.rs`,
  `crates/apb-core/src/connector/oauth.rs`,
  `crates/apb-cli/src/connector_oauth.rs`,
  `connector/effect.rs`, `call/send_once.rs`, `call/target.rs`,
  `crates/apb-mcp-client/src/frame.rs`, `stdio.rs` or `testing.rs`, and
  the diff of
  `oauth/token.rs`, `call/http.rs`, `call/mod.rs` and `connector/imap.rs`
  holds only `TokenProxy`, the `proxy` parameter and `adopt_minted`
  (design "Unchanged from main" items 2 and 6); `rg -n
  'tls_config|TlsConfig|disable_verification|key_log' crates/apb-mcp-client
  crates/apb-engine/src/connector/mcp` finds nothing; and no surface
  serializes `ToolListing` whole (`rg -n 'to_value\(&?listing\)|json!\(listing\)'
  crates/apb-cli crates/apb-server crates/apb-engine/src/connector/mcp`
  finds nothing); and the healthcheck's body (`pin::healthcheck` and the
  private `check_live`) is unchanged but for the `&mut` redaction list
  `open` takes.
- [x] 10.3 `CLAUDE.md` and `AGENTS.md`, the same shared text in both
  (only CLAUDE.md's "Claude-specific" section has no twin): design "Docs
  to update" item 3.
- [x] 10.4 Write `docs/superpowers/specs/<implementation date>-mcp-http-and-surfaces-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] 10.5 Style over every changed doc and web file: no U+2013 or U+2014
  (`rg -nP '[\x{2013}\x{2014}]' <files>` finds nothing), no exclamation
  mark in prose (`rg -n '[A-Za-z]!( |$)' <files>`; `!=`, `<!--`, `![` and
  Rust macros such as `writeln!(` do not count), no CJK
  (`rg -nP '\p{Han}|\p{Hiragana}|\p{Katakana}|\p{Hangul}' <files>` finds
  nothing).

## 11. Gates

The repository's twelve gates (CLAUDE.md "Required gates" and "Commands",
and the jobs of `.github/workflows/ci.yml` and `test-gate.yml`). Run each
once on the final tree, in this order, and record the numbers (passed,
failed, skipped, time) in the report. Fix every failure and rerun the
gate that failed until it is clean.

- [x] 11.1 `cargo fmt --all -- --check`
- [x] 11.2 `cargo clippy --workspace --all-targets -- -D warnings`
- [x] 11.3 `cargo deny check` (licenses, advisories, bans; the CI lint
  job's `cargo-deny-action`)
- [x] 11.4 `scripts/third-party-notices.sh --check` (after 9.5
  regenerated it)
- [x] 11.5 `cargo nextest run --workspace` (when `cargo nextest` is
  missing: `cargo test --workspace`; say which ran)
- [x] 11.6 `cargo test --workspace --doc`
- [x] 11.7 `cargo nextest run -p apb --test main journal_corpus` (the CI
  journal corpus job; without `APB_OLD_BINARIES` each old-binary case
  prints one line and passes; say whether old binaries were present)
- [x] 11.8 `cargo test -p apb-server --lib ts_contract` (without
  `APB_WRITE_TS`; this change adds no mirrored type, so the generated
  files must be unchanged)
- [x] 11.9 `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`
- [x] 11.10 `cargo metadata --format-version 1 >/dev/null && code-ranker check .`
  (for a violation read `code-ranker docs base <ID>` first, fix, rerun
  until clean)
- [x] 11.11 `cargo clippy --release` (fix every error, clean warnings as
  far as practical)
- [x] 11.12 `openspec validate mcp-http-and-surfaces --strict`

## 12. Close

- [x] 12.1 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it
  if it is under `/tmp`. Stop only the processes you started (check
  `pgrep -f stub-mcp.sh` and the test HTTP servers).
- [x] 12.2 Final report: per section what was built, every place where
  the code differed from `design.md` and what you did, the answers to 0.1
  to 0.5, each gate of section 11 with its numbers, the crates
  `Cargo.lock` gained, the user-visible changes for the release notes
  (design "Release notes"), the changed file count against design
  "Files" (53, at most 56) with every file outside that list named, the
  rule-removal checks of 8.2, and anything left open.

## 13. Fix round 1 (design "Review record", "Fix round 1")

- [x] 13.1 Rebase onto main 3248c77f ("One point oh fixes" #294); the
  test conflict in `connectors_api_test.rs` resolved over #294's `keyed`
  and `sign_in`; `AGENTS.md` and `CLAUDE.md` mirrored; V303 still free.
- [x] 13.2 C1: the caller's `DecisionChannel` into
  `connector::call::healthcheck` and `pin::list_approved`, decided by
  `pin::refuse_caller` (`person_only_refusal`) on the one loaded
  definition (`LiveTarget::load_picking`); the routes map the refusal to
  403. Test and bite: design item 2.
- [x] 13.3 C2, R1: `Opened.warnings` in the healthcheck's `warnings` and
  the approval listing's note (`listing_note`). Tests and bites: design
  item 3.
- [x] 13.4 R2: the 2xx JSON arm runs `answers` after the strict reader.
  Test and bite: design item 4.
- [x] 13.5 R3: the proxy child's marker file. Bite: design item 5.
- [x] 13.6 C3, R6: the scripted server's `stop()` joins its accept thread
  and waits for every accepted request; no sleep-based proof. Bites:
  design item 6.
- [x] 13.7 R4, R5, R7: the web section under `web/src/lib/components/`
  with `ui/button` and `ui/table`, named rows, live regions, the refused
  state. Tests and bites: design item 7.
- [x] 13.8 The twelve gates of section 11 plus
  `check-forbidden.sh origin/main`, on the rebased tree.

## 14. Fix round 2 (design "Review record", "Fix round 2")

- [x] 14.1 K1: the token cache entry keeps the rotated refresh token
  sealed and a hit answers it (`rotated`, `new_refresh`) for every user
  of the cache (MCP, HTTP, IMAP); a non-rotating 401 refresh keeps the
  earlier rotation. Tests and bite: design item 1.
- [x] 14.2 K2: `CallError.warnings` and `CallError::carrying`;
  `open_http` over `open_http_steps`, `Opened::fail` and
  `Opened::carrying` attach the open's warnings to every post-mint
  error; the CLI and the listing route print them. Tests and bite:
  design item 2.
- [x] 14.3 K3: `body_fault`, the one body-error mapper of both readers.
  Tests and bite: design item 3.
- [x] 14.4 The twelve gates of section 11 plus
  `check-forbidden.sh origin/main`.

## 15. Fix round 3 (design "Review record", "Fix round 3")

- [x] 15.1 M1, M3, M4: `adopt_minted` warns once per call; an insert
  drops expired cache entries; an `invalid_grant` refresh drops its
  entry; `forget` ends the access token and keeps the carried rotation.
  Tests: design items 1, 3 and 4.
- [x] 15.2 M2: `call::healthcheck_outcome` and
  `HealthcheckOutcome::CallerRefused`, the route's 403 decided on the
  type; `pin::caller_refusal_in` removed. Design item 2.
- [x] 15.3 M5, M6: the passed deadline and the request build before
  `start_call`; `last_fault` cleared per post. Tests: design items 5
  and 6.
- [x] 15.4 M7: `mcp-session-id` and `last-event-id` reserved, docs and
  the spec delta updated. Test: design item 7.
- [x] 15.5 M8: `Reply::ThenClose` in the scripted server; no sleep.

## 16. Closing

- [x] 16.1 The change archived, each new capability with its Purpose in
  `openspec/specs`, `openspec validate --all --strict` passing, the public
  design record matching the final design.
- [x] 16.2 The gates of section 11 run whole on the final tree.
