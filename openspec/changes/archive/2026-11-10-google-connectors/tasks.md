# Tasks: Google connectors, OAuth refresh and bounded paging

Work in order, one section at a time. Before a section, read its part of
`design.md` and the spec delta it names. Tick each box (`- [x]`) as soon as
it is done, so an interrupted run can resume from the first open box.
Partial edits from an earlier attempt may already be in the tree: review
them, keep what is right, finish the rest.

For every behaviour: write the failing test first, run it and see it fail,
then write the code. For a test that names a field or a module that does
not exist yet, a compile error is the expected first failure.

Integration tests live in `crates/<crate>/tests/suite/*.rs` and run in one
binary, `main`: declare every new file in `crates/<crate>/tests/main.rs`
with `#[path = "suite/<file>.rs"] mod <file>;` next to its neighbours.

The order matters: D1 (OAuth) and D2 (paging) land and pass their tests
before any connector that uses them; the format helpers (D3 key settings,
D4 `mime` and `decode_base64url`, D5 `content_types`, D8
`untrusted_output`) land before the connector manifests that declare them.

## Environment

Set this in every shell before any cargo command:

```sh
export CARGO_BUILD_JOBS=4 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_TARGET_DIR=$HOME/.cache/apb-google-connectors-target TMPDIR=$HOME/.cache/apb-google-connectors-tmp
mkdir -p "$TMPDIR"
```

If writing under `$HOME/.cache/apb-google-connectors-target` fails with a
read-only file system, use `CARGO_TARGET_DIR=/tmp/apb-google-connectors-target`
and `TMPDIR=/tmp/apb-google-connectors-tmp` instead, and before each build
check that `df -h /tmp` shows more than 4G free; if not, stop and report
it. `/tmp` is a RAM disk shared with other sessions. Never use `/dev/shm`,
and never put a target dir, cache or temp data inside the worktree.

Before the first web command run `(cd web && bun install --frozen-lockfile)`
if `web/node_modules/.bin/vite` is missing; it needs network access. If it
fails, skip the web steps and report it.

Never touch the real `~/.config/apb`; tests that need a config dir get one
under `$TMPDIR`. Never contact a real Google or Brave endpoint: every test
here is offline (the live smoke tests are `#[ignore]` and env-gated, and
you do not run them). Never stop a process you did not start. Do not
commit, push or open a pull request: the git directory is read-only for
you. Do not start subagents.

During a section run only the tests of the crates it touches
(`cargo test -p <crate> --lib <module>`,
`cargo test -p <crate> --test main <module>`), one filter per invocation.
The wider gates run in section 17.

## 1. OAuth: schema and pure helpers (D1, spec `oauth-refresh-auth`)

- [x] 1.1 Tests in `crates/apb-core/src/connector/def.rs` tests: an
  `oauth2_refresh` block round-trips through YAML with and without the
  optional fields; a literal `refresh_token_template` is refused; a
  `client_secret_template` of `"x{{secret.s}}"` is refused; `{{args.x}}`
  in `token_url` is refused; `{{secret.s}}` in `granted_scopes_template`
  is refused; `scopes` on an HTTP function of a `header` connector is
  refused; `scopes` with whitespace or a duplicate is refused; `scopes`
  on an `oauth2_refresh` HTTP function loads.
- [x] 1.2 Implement the `AuthSpec::Oauth2Refresh` variant, `FunctionSpec.scopes`,
  `sole_secret` (extracted from `validate_webhook_templates` and used by
  both), `auth_templates`, rule 11 in `from_yaml` and its doc comment.
- [x] 1.3 Tests in a new `crates/apb-core/src/connector/oauth.rs`
  (declare `pub mod oauth;` in `connector/mod.rs`): `pkce_challenge` of
  the RFC 7636 Appendix B verifier `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk`
  is `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM`; `new_verifier` is 43
  chars of the base64url alphabet and two calls differ; `check_scopes`
  any-of, empty required, none granted; `scope_set` drops empty tokens;
  `parse_token_response` without `access_token` errors without echoing
  the body; `token_error_code`; `authorize_url` parameter order and
  encoding; `parse_callback` decodes `%2F` and `+`.
- [x] 1.4 Implement them.
- [x] 1.5 `crates/apb-core/src/token_shape.rs`: tests then the
  `ya29\.`, `1//0`, `GOCSPX-` prefixes (design D1 "Scrubbing token
  shapes").
- [x] 1.6 `cargo test -p apb-core --lib connector` and
  `cargo test -p apb-core --lib token_shape` pass.

## 2. OAuth: token source and the HTTP call (D1)

- [x] 2.1 Extend `crates/apb-engine/tests/suite/common/mod.rs`: a
  `Reply::RespondWith(u16, Vec<(String, String)>, String)` variant of the
  scripted server that sends the given headers (Link, Content-Type), and a
  way to read every captured request whole (method, path, headers, body).
- [x] 2.2 New `crates/apb-engine/tests/suite/connector_oauth.rs` (seed a
  run like `connector_slack.rs` does, with an inline `oauth2_refresh`
  connector whose `token_url` and `api_base` point at one scripted
  server). Tests, failing first:
  `refresh_then_call_sends_the_bearer_token`;
  `a_401_refreshes_once_and_retries_once`;
  `a_second_401_is_final`;
  `a_403_is_not_retried`;
  `invalid_grant_names_the_oauth_command`;
  `a_token_error_description_is_never_echoed`;
  `account_scopes_missing_fail_before_any_request` (zero requests);
  `token_scope_missing_fails_before_the_service_request` (one token request);
  `any_one_scope_is_enough`;
  `a_rotated_refresh_token_warns_and_is_redacted`;
  `an_echoed_access_token_is_redacted`;
  `tokens_never_reach_the_journal` (grep `events.jsonl` and every file
  under the run dir and the config dir for both token values);
  `one_call_one_event_one_budget_unit_after_a_retry`;
  `two_accounts_get_two_tokens_in_one_process`;
  `dry_run_contacts_no_token_endpoint`;
  `a_token_timeout_settles_a_none_send_not_sent` (no `connector_call`
  event, a repeat with the same intent is sent);
  `a_token_status_is_never_the_http_status`;
  `a_refresh_failure_after_a_401_keeps_the_401` (`http_status: 401`,
  a `mode: none` send settles `not_sent`);
  `a_stalled_token_endpoint_fails_within_the_deadline`;
  `execute_within_bounds_token_minting`;
  `a_stalled_second_page_fails_within_one_deadline`;
  `a_token_echoed_in_the_link_header_is_redacted`;
  `a_token_echoed_in_the_retry_response_is_redacted`.
- [x] 2.3 Implement `crates/apb-engine/src/connector/oauth/mod.rs` and
  `token.rs` (`TokenSource`, the cache, the refresh request, the
  `TokenError` mapping), declare `pub mod oauth;` in `connector/mod.rs`.
- [x] 2.4 The one call deadline (design D1 "One deadline per call") on
  `HttpCall` and `ImapCall`, replacing per-request `timeout_sec`; run
  `cargo test -p apb-engine --test main connector_call` and
  `connector_imap` before going on.
- [x] 2.5 Wire it into `build_prepared` (minting and scopes before
  dispatch), `HttpCall` (`send`, the 401 refresh and retry, `auth_header`,
  redactions over body, `link`, warnings and errors) and the `Dispatched`
  struct with `warnings` through `run`, `execute_bounded` and
  `answer_live` (design D1 "The HTTP call").
- [x] 2.6 `cargo test -p apb-engine --test main connector_oauth`,
  `cargo test -p apb-engine --test main connector_call` and
  `cargo test -p apb-engine --lib connector` pass.

## 3. OAuth: IMAP and the consent command (D1)

- [x] 3.1 `crates/apb-engine/tests/suite/connector_imap.rs`: update every
  `build(...)` call with `&ImapExtras::default()`; new test
  `oauth2_method_mints_and_sends_the_token_over_xoauth2` (token endpoint
  on a `spawn_script` server, IMAP on the test listener; the recorded
  AUTHENTICATE payload decodes to `user=...\x01auth=Bearer <minted>`), and
  `oauth2_without_an_auth_block_is_a_config_error`,
  `oauth2_rotation_warns_through_the_imap_result`,
  `oauth2_token_echoed_in_a_server_no_is_redacted`.
- [x] 3.2 Implement `ImapExtras`, `AuthMethod::OAuth2`, `ImapSecret`, the
  `render_connection` change and the call site in `call/mod.rs`.
- [x] 3.3 `connectors/imap/connector.yaml`: the optional OAuth fields and
  the auth block (design D1 "IMAP over the same source"), `password`
  `required: false`, version 0.2.0; `tests.yaml` keeps passing
  (`cargo run -p apb -- connector test --dir connectors/imap`).
- [x] 3.4 Unit tests in `crates/apb-engine/src/connector/oauth/consent.rs`:
  `finish` over a real loopback request from a test thread
  (`GET /?code=c1&state=<state>`), with the token endpoint on a scripted
  server: the exchange body holds `grant_type=authorization_code`,
  `code=c1`, `code_verifier` and the same `redirect_uri`, and the result
  holds the refresh token; a wrong `state` fails and makes no exchange; a
  request to `/favicon.ico` gets 404 and the wait goes on; `error=access_denied`
  ends the flow; no `refresh_token` in the answer is the error of design D1;
  the timeout ends the flow; a client that sends half a request line and
  stays connected does not hold the flow past a 2 s timeout; headers over
  8 KiB are dropped and the wait goes on; a target with two `code` params
  ends the flow; `plan` succeeds for a trusted account whose
  `refresh_token` env var has no value, and for one whose `refresh_token`
  is a `{{cmd:...}}` writing a marker file (the file does not appear).
- [x] 3.5 Implement `consent.rs` (`plan`, `start`, `finish`) and
  `auth::resolve_named_secrets`.
- [x] 3.6 Tests in `crates/apb-cli/tests/suite/connector_oauth_test.rs`:
  with `APB_RUN_ID` set the command exits 2 and prints the refusal; with
  `APB_HOST_TASK` set likewise; without a terminal (the test harness has
  none) it exits 2 with the terminal message.
- [x] 3.7 Implement `crates/apb-cli/src/connector_oauth.rs` and the
  `Oauth` action; `apb connector show` prints the auth kind and per
  function scopes.
- [x] 3.8 `cargo test -p apb-engine --test main connector_imap`,
  `cargo test -p apb-engine --lib oauth` and
  `cargo test -p apb-cli --test main connector_oauth_test` pass.

## 4. Paging: schema (D2, spec `bounded-pagination`)

- [x] 4.1 Tests in a new `crates/apb-core/src/connector/paginate.rs`
  (declare it in `connector/mod.rs`), through `ConnectorDoc::from_yaml`:
  each rule 1 to 6 of design D2 refused with its own message; a valid
  link block and a valid token block load; `max_pages` defaults to 5;
  `items_optional` defaults to false and round-trips.
- [x] 4.2 Implement `Paginate`, `PageNext`, `validate`, the
  `FunctionSpec.paginate` field and the call from `from_yaml`.
- [x] 4.3 Tests in `crates/apb-core/src/connector/contract.rs`: a case
  with `responses`, `requests`, `result_contains` and `error_code` parses;
  a `ScriptedResponse` without `status` reads 200; an unknown key in it is
  refused. Implement the fields and `ExpectKind::Http`'s new members.
- [x] 4.4 `cargo test -p apb-core --lib connector` passes.

## 5. Paging: the walk, the result, the journal (D2)

- [x] 5.1 Unit tests in `crates/apb-engine/src/connector/call/paginate.rs`
  over scripted closures (the design D2 "Wrong implementations" 8 to 10
  among them: `items_optional` absent field and empty middle page; the
  byte allowance passed to `fetch` and a page cut by it failing with no
  items; `WalkStats` on a page 2 status error, a page 2 transport error
  and a cut page 2; the prefix on all three): Link parsing (quoted, unquoted, `rel="next last"`,
  several entries, none); same-origin check (default ports, host case);
  two pages merge; `items: ""`; the last page's body around the items;
  stop at `max_pages` with `more: true`; stop at `PAGES_BYTE_CAP`; an
  empty or null token stops; a page without an item array fails; a page
  2 error carries the prefix; the token re-render sets the arg.
- [x] 5.2 Create `call/shape.rs` (`RawResponse`, `shape`,
  `check_content_type`), give `read_body` its `cap` argument and the
  bytes-read return, move the response half of `send_raw` into `shape`
  with no behaviour change (run
  `cargo test -p apb-engine --test main connector_call` before going on),
  then implement `paginate.rs` and the `HttpCall` changes.
- [x] 5.3 Decode test in `crates/apb-engine/src/event.rs` tests: an old
  `connector_call` line without `pages` reads as `None`; a call without
  paging serializes no `pages` key. Add the field and fix every
  constructor the compiler lists with `pages: None`.
- [x] 5.4 `CallOk::Http.pages`, `to_success_json`, the dry-run
  `paginate` block, the prompt tag.
- [x] 5.5 E2e in a new `crates/apb-engine/tests/suite/connector_paginate.rs`
  (an inline connector, scripted server with Link headers):
  `link_pages_are_followed_and_merged`, `a_link_to_another_host_is_not_followed`,
  `a_failed_second_page_fails_the_call`, `max_calls_counts_a_paged_read_once`
  (`max_calls: 1`, three pages), `the_event_records_the_page_count`,
  `the_token_cache_mints_once_for_all_pages` (an `oauth2_refresh`
  connector: one token request for three pages),
  `a_failed_page_journals_pages_and_the_last_status`,
  `real_bytes_count_against_the_total` (whitespace-padded JSON pages).
- [x] 5.6 `contract_test.rs` `eval_http`: run scripted responses through
  `walk`/`shape` and the pick; unit tests there for each new assertion and
  for a left-over or missing response.
- [x] 5.7 `cargo test -p apb-engine --lib connector`,
  `cargo test -p apb-engine --test main connector_paginate` and
  `cargo test -p apb-engine --test main connector_call` pass.

## 6. Paging in github and asana (D2)

- [x] 6.1 `connectors/github/tests.yaml`: the new cases of design D2
  "Official connectors" 1, failing first (`cargo run -p apb -- connector
  test --dir connectors/github`).
- [x] 6.2 `connectors/github/connector.yaml`: `paginate` and `per_page`
  on `list_issues` and `list_pulls`, version 0.3.0. Update the version
  assertions in `crates/apb-core/src/connector/official.rs` tests (lines
  127 and 199) and any other test pinning `0.2.1` (`rg -n '0\.2\.1' crates`).
- [x] 6.3 `connectors/asana/tests.yaml` then `connector.yaml` (design D2
  "Official connectors" 2), version 0.2.0.
- [x] 6.4 `cargo test -p apb-cli --test main official_connectors_gate` and
  `cargo test -p apb-core --lib official` pass.

## 7. Outside text marked at the source (D8, spec `untrusted-connector-output`)

- [x] 7.1 Confirm V187 is free: `rg -n '"V187"' crates` finds nothing.
- [x] 7.2 Tests in a new `crates/apb-core/tests/suite/validate_untrusted_output_test.rs`
  (connector facts handed in through `ValidationContext.connectors`, as
  `validate_untrusted_test.rs` sets up its context): V187 is a warning
  (`Severity::Warning`) for a binding granting a marked function without
  `untrusted: true` through `functions: all`, through `functions:
  read_only` and through a list; none with the marker; none for a list
  of unmarked functions only; none for `read_only` when every marked
  function is a write; none for a list node's item check (`<node>#check`)
  that reads a marked function; none for a baseline check reading one;
  none when the connector is unknown or does not load.
- [x] 7.3 Implement `FunctionSpec.untrusted_output`,
  `ConnectorFacts.untrusted_output_functions`, `check_untrusted_output`
  over authored node bindings only (its own grant expansion;
  `decisions::granted` unchanged) and its call in `validate/mod.rs`.
- [x] 7.4 Prompt test in `prompt.rs` tests: a granted `untrusted_output`
  function adds the paragraph once; an inbox grant still gets only the
  inbox paragraph. Implement.
- [x] 7.5 Mark imap `search_messages` and `get_message`
  `untrusted_output: true` (the imap version is already 0.2.0 from 3.3).
- [x] 7.6 `cargo test -p apb-core --test main validate_untrusted_output_test`
  and `cargo test -p apb-engine --lib prompt` pass.

## 8. Format helpers: composer, `mime`, `decode_base64url`, `content_types` (D4, D5)

- [x] 8.1 Move the SMTP composer into `crates/apb-engine/src/connector/compose.rs`
  (design D4 "Composer shared with SMTP") with no behaviour change; run
  `cargo test -p apb-engine --test main connector_smtp` and the smtp unit
  tests before going on.
- [x] 8.2 Unit tests in `compose.rs`: `in_reply_to` and `references`
  headers; `keep_bcc` keeps `Bcc:`; the default drops it; the Message-ID
  domain comes from `from_email`; the preview leaves absent fields out.
  Add `in_reply_to` and `references` to `SmtpMessage` and its template
  check, then implement.
- [x] 8.3 Manifest tests in `def.rs`: `mime` rules of design D4 (each
  refused case, one valid case). Implement `MimeBody` and the rules.
- [x] 8.4 Render tests in `crates/apb-engine/src/connector/call/tests.rs`:
  `render_http` puts the preview at `message.raw`; the live build puts
  URL-safe base64 there (decode it and find `Subject:`). Implement.
- [x] 8.5 Unit tests of `decode_paths` in `call/shape.rs` (UTF-8, padded
  and unpadded, not base64, binary, the 256 KiB cap and `_truncated`,
  array mapping) and manifest tests for `decode_base64url` on a non-HTTP
  function. Implement.
- [x] 8.6 Unit tests of `check_content_type` and an e2e in a new
  `connector_content_types.rs`: a 2 MiB `image/png` 2xx fails `service`,
  not `incomplete_response`; a missing Content-Type fails; `text/plain`
  passes. A contract-runner unit test in `contract_test.rs` scripts the
  same three headers and gets the same three verdicts. Manifest test:
  upper case or `;` in an entry is refused. Implement.
- [x] 8.7 Official gate (`crates/apb-cli/tests/suite/official_connectors_gate.rs`):
  the `content_types` exemption of design D5, JSON included; a gate unit
  check (a small helper the gate calls, tested in the same file) that a
  function like `read_text_file` passes and a read-only function with
  neither `content_types` nor `response_pick` fails.
- [x] 8.8 `cargo test -p apb-engine --lib connector`,
  `cargo test -p apb-engine --test main connector_smtp` and
  `cargo test -p apb-core --lib connector` pass.

## 9. Send-once key settings (D3)

- [x] 9.1 Tests in `crates/apb-core/src/connector/idempotency.rs`:
  `encoding` and `conflict` parse on `key` and are refused on other
  modes. Implement.
- [x] 9.2 Tests in `crates/apb-engine/src/connector/intent/stamp.rs`:
  the two encoding vectors of design D3; the preview is not encoded.
  Implement `encode_key` and its use in `apply_key`.
- [x] 9.3 E2e in `crates/apb-engine/tests/suite/send_intent_test.rs`:
  a key function with `conflict: confirms` whose first send is cut and
  whose replay gets 409 settles `confirmed` with source `key_conflict`,
  answers `already_sent` with `ref` the key, and exits ok; the same
  without `conflict` stays `unknown`. Implement `Tracked` and `finish`
  changes and the `settle_send` flag.
- [x] 9.4 `cargo test -p apb-engine --test main send_intent_test` and
  `cargo test -p apb-engine --lib intent` pass.

## 10. `google-calendar` (D3, spec `google-calendar-connector`)

- [x] 10.1 `connectors/google-calendar/tests.yaml` (design D3) and
  `connector.yaml` (version 0.1.0); `cargo run -p apb -- connector test
  --dir connectors/google-calendar` passes.
- [x] 10.2 `README.md` (heading `# google-calendar: setup for humans`,
  the agent prompt naming `connectors/google-calendar/INSTALL.md`, what
  it can and cannot do, why free slots are left to the agent, the
  irreversible `delete_event`, `send_updates`), `INSTALL.md` (heading
  `# google-calendar: installation instructions for an agent`; steps as
  `connectors/twenty/INSTALL.md`: install; create a Google Cloud project,
  enable the Calendar API, configure the OAuth consent screen (External,
  add yourself as a test user, or publish it: an app in Testing status
  gets refresh tokens that expire after 7 days), create an OAuth client of
  type Desktop app; write the account config; put `client_secret` into the
  secrets file (offer the person to fill it in themselves); approve the
  connector and the account; run `apb connector oauth google-calendar`
  in the person's terminal (never by the agent) and have the person store
  the printed refresh token; healthcheck; report), `PUBLIC.md`
  (frontmatter `display_name`, `summary`, `tags`, `publisher: apb`).
- [x] 10.3 E2e `crates/apb-engine/tests/suite/connector_google_calendar.rs`
  (design D3), reading the checked-in manifest like `connector_slack.rs`,
  including `a_lost_update_answer_is_refused_on_repeat` (design D3 "Wrong
  implementations" 4).
- [x] 10.3a In `crates/apb-core/tests/suite/` next to the existing V73
  tests: a node granted `google-calendar.create_event`, and one granted
  `update_event`, after a `human_review` gate with `auto_decide` gets V73
  (facts from the checked-in manifest through `ConnectorFacts::of_manifest`).
- [x] 10.4 `cargo test -p apb-engine --test main connector_google_calendar`
  passes.

## 11. `gmail` (D4, spec `gmail-connector`)

- [x] 11.1 `connectors/gmail/tests.yaml` (including the exact-URL
  `label_id` case and the empty-search case of design D4) and
  `connector.yaml` (design D4), version 0.1.0; the contract test passes.
- [x] 11.2 `README.md` with the four grant presets and their measured
  prompt sizes (write a throwaway test or use `apb connector show`, then
  record the numbers; do not commit the throwaway), `INSTALL.md` (as
  10.2 for the Gmail API, plus: a shared Google client may serve all three
  Google connectors, and one refresh token can carry the scopes of all of
  them when `apb connector oauth` gets every `--scope`), `PUBLIC.md`.
- [x] 11.3 E2e `crates/apb-engine/tests/suite/connector_gmail.rs`
  (design D4).
- [x] 11.4 `cargo test -p apb-engine --test main connector_gmail` passes.

## 12. `google-drive` (D5, spec `google-drive-connector`)

- [x] 12.1 `connectors/google-drive/tests.yaml` and `connector.yaml`
  (design D5), version 0.1.0; the contract test passes.
- [x] 12.2 `README.md` (export formats, the 1 MiB read cap, no upload,
  the restricted `drive.readonly` scope), `INSTALL.md`, `PUBLIC.md`.
- [x] 12.3 E2e `crates/apb-engine/tests/suite/connector_google_drive.rs`:
  `export_text` returns the scripted text; `read_text_file` refuses an
  `application/octet-stream` answer; `search_files` follows the token.
- [x] 12.4 `cargo test -p apb-engine --test main connector_google_drive`
  passes.

## 13. `web-search` (D6, spec `web-search-connector`)

- [x] 13.1 `connectors/web-search/tests.yaml` and `connector.yaml`
  (design D6), version 0.1.0; the contract test passes.
- [x] 13.2 `README.md` (the key from the Brave Search API dashboard,
  plans and the 1 request per second free limit, the paid probe, no page
  fetching), `INSTALL.md`, `PUBLIC.md`.
- [x] 13.3 E2e `crates/apb-engine/tests/suite/connector_web_search.rs`:
  the `X-Subscription-Token` header carries the key; the result is
  projected; the key echoed in a body is redacted.

## 14. IMAP drafts and attachments (D7, spec `mail-drafts-attachments`)

- [x] 14.1 Manifest tests in `def.rs`: `message` required for
  `append_draft` and refused elsewhere; the params of both new ops;
  `people` on an `append_draft` function loads, on `get_message` it does
  not. Implement the schema.
- [x] 14.2 Move `fetch_body_json`'s parsing to `connector/mime_read.rs`
  with the three new header fields; the existing imap tests pass
  unchanged except for the added fields.
- [x] 14.3 Unit tests of `safe_file_name` in a new `connector/files.rs`
  (path parts, controls, reserved characters, dots, empty, the 120-byte
  cut with the extension kept, a multi-byte name). Implement.
- [x] 14.3a `crates/apb-core/src/fsutil/beneath.rs`: tests first
  (`create_exclusive` gives `AlreadyExists` and keeps the old bytes; a
  link at the name is not followed;
  `create_exclusive_refuses_a_directory_swapped_for_a_link` through
  `tests::plant_between`), then `Beneath::create_exclusive` in both the
  unix and the non-unix impl. `cargo test -p apb-core --lib beneath`.
- [x] 14.4 The IMAP test server changes (APPEND, the `\Drafts` LIST
  line, the partial-section `BODY[]<0>` FETCH answer, a script setting
  for the advertised `RFC822.SIZE`) and every test of design D7 "Tests",
  failing first: the drafts, overwrite, outside-a-run, file cap, call
  cap, twenty-files, advertised-size and three symlink tests. Implement
  `append_draft` and `save_attachments` (all writes through `Beneath`),
  `CallMode.files`, `ImapExtras.files`.
- [x] 14.5 `connectors/imap/connector.yaml`: `append_draft`,
  `save_attachments`, `from_email`, `from_name`; `tests.yaml` cases for
  both (the imap expectation gains `message` for the preview:
  `ImapExpect.message: Option<Envelope>`-like subset of `from`, `to`,
  `subject`); README and INSTALL (drafts, attachments, `oauth2`); the
  contract test passes.
- [x] 14.6 `cargo test -p apb-engine --test main connector_imap` and
  `cargo test -p apb-engine --lib connector` pass.

## 15. The official set, the gate and the live smoke tests

- [x] 15.1 `crates/apb-core/src/connector/official.rs`
  `every_official_connector_carries_the_full_file_set`: the pinned name
  list gains `gmail`, `google-calendar`, `google-drive`, `web-search` in
  sorted order. Run `cargo test -p apb-core --lib official`.
- [x] 15.2 `crates/apb-cli/tests/suite/live_smoke_test.rs`: generalize
  `run_live_probe` to a list of `(field, env var, value)` secrets (keep
  the existing callers working through a one-entry list), and add four
  `#[ignore]` tests gated by `APB_LIVE_TEST_GOOGLE_CALENDAR`,
  `APB_LIVE_TEST_GMAIL`, `APB_LIVE_TEST_GOOGLE_DRIVE`,
  `APB_LIVE_TEST_WEB_SEARCH` (credentials from `GOOGLE_CLIENT_ID`,
  `GOOGLE_CLIENT_SECRET`, `GOOGLE_REFRESH_TOKEN`, `BRAVE_API_KEY`):
  healthcheck plus `list_calendars`, `list_labels`, `search_files`,
  `search`. Do not run them.
- [x] 15.3 `cargo test -p apb-cli --test main official_connectors_gate`
  passes (it runs every connector's contract tests).

## 16. Docs

- [x] 16.1 `docs/CONNECTORS.md`: "Secrets" and the new "OAuth accounts"
  section (design D1 "Surfaces and docs"); "Binding a connector to a
  node" (D8 paragraph); "Paging" (D2); "What a call returns" (`pages`,
  `more`, `warnings`); "Sending once" (`encoding`, `conflict`, the
  `key_conflict` settlement); "Contract tests" (the four new fields);
  "The `apb connector` CLI" (the `oauth` line); "Official connectors":
  "Fifteen" becomes "Nineteen", the name list, the `github`, `asana` and
  `imap` entries (paging, `oauth2`, drafts, attachments, the downgrade
  note), and new entries `google-calendar`, `gmail`, `google-drive`,
  `web-search` in the style of the existing ones; "Coverage note" keeps
  its claim (write paths are contract-tested only).
- [x] 16.2 `docs/HOWTO-authoring.md`: V187 in the untrusted-input code
  table; the format reference of connector manifests if it lists
  function fields (`rg -n 'response_pick' docs/HOWTO-authoring.md`), with
  `scopes`, `paginate`, `mime`, `decode_base64url`, `content_types`,
  `untrusted_output`.
- [x] 16.3 `apb connector init`'s template (`apb-cli/src/connector.rs`
  around line 1330) needs no change; confirm and say so in the report.

## 17. Records and gates

- [x] 17.1 `CLAUDE.md` and `AGENTS.md`, the same shared text in both:
  in the `apb-core` module list, `connector/oauth.rs` (the OAuth pure
  helpers: token response, scopes, PKCE, authorization URL, callback) and
  `connector/paginate.rs` (the paging block and its manifest rules); in
  the `apb-engine` list, `connector/oauth/` (`token` the in-memory token
  source and its refresh, `consent` the loopback consent of `apb connector
  oauth`), `connector/call/paginate.rs` (the bounded page walk),
  `connector/call/shape.rs` (one response shaped: status, error_when,
  decoded text), `connector/compose.rs` (the e-mail composer SMTP, IMAP
  drafts and `mime` bodies share), `connector/mime_read.rs` (a raw message
  as JSON), `connector/files.rs` (safe file names of saved attachments);
  in the `apb-cli` paragraph, `connector_oauth.rs` is `apb connector
  oauth`. In "Concepts that span files", one short paragraph "OAuth
  connector accounts": the access token lives in process memory only,
  refresh before expiry and once after a 401, scopes any-of checked
  before the request, a rotated refresh token is never stored, consent
  only from a person's terminal.
- [x] 17.2 Write `docs/superpowers/specs/2026-11-09-google-connectors-design.md`:
  the content of `proposal.md` and `design.md` as one public record, with
  no mention of `openspec/` and no reference to any outside article,
  author or project.
- [x] 17.3 Style over every changed doc, connector file and bundled file:
  no U+2013 or U+2014 (`rg -nP '[\x{2013}\x{2014}]' <files>` finds
  nothing) and no exclamation mark in prose (`rg -n '[A-Za-z]!( |$)'
  <files>`; `!=`, `<!--` and `![` do not count). No CJK.
- [x] 17.4 The full gates once, each with its numbers in the report:
  `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`;
  `cargo nextest run --workspace` (or `cargo test --workspace` when
  nextest is missing; say which);
  `cargo test --workspace --doc`;
  `APB_WRITE_TS=1 cargo test -p apb-server --lib ts_contract`, then the
  same without the variable (expected: no drift, since no mirrored type
  changed; report if it regenerated anything);
  `cd web && bun install --frozen-lockfile && bun run check && bun run test && bun run build`;
  `scripts/third-party-notices.sh --check` (no new dependency is
  expected; if one was added, regenerate and say why);
  `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.
  Fix every failure and every code-ranker violation (read
  `code-ranker docs base <ID>` first).
- [x] 17.5 Remove the `$TMPDIR` contents you created. Leave
  `$CARGO_TARGET_DIR` in place if it is under `$HOME/.cache`; remove it if
  it is under `/tmp`. Stop only the processes you started.
- [x] 17.6 Final report: per section what was built, decisions you took
  where `design.md` left room, each gate with numbers, the measured
  prompt sizes of the gmail presets, the user-visible changes for the
  release notes (proposal "Impact" 5), the changed file count, and
  anything left open.
