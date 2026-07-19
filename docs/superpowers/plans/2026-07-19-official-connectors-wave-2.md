# Official Connectors Wave 2 (asana, imap) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the `asana` and `imap` official connectors, the built-in `imap` function kind with guaranteed-silent mail reading, the typed/optional single-placeholder template rule, and the ETXTBSY flaky-test fix, as one PR releasing 0.5.0.

**Architecture:** The `imap` kind mirrors the wave 1 `smtp` kind exactly: a spec struct in `apb-core` `def.rs`, a blocking executor in `apb-engine` (`connector_imap.rs`) wired into the `connector_call.rs` `Prepared` dispatch, dry-run and `tests.yaml` support through the same offline render path. Asana is a manifest plus one small template-engine rule (typed/optional single placeholders) that strict APIs need. Spec: `docs/superpowers/specs/2026-07-19-official-connectors-wave-2-design.md` (sections referenced below as "spec N").

**Tech Stack:** Rust workspace edition 2024. New workspace deps: `imap` 2.x (protocol client, TLS-agnostic), `mail-parser` (MIME decoding), `rustls` + `rustls-platform-verifier` (TLS with platform trust roots; lettre already pulls rustls). No native-tls anywhere.

## Global Constraints

- Branch: `feat/official-connectors-wave-2`. One PR. Commit per task (or more); never push.
- Every commit: `git commit --signoff` and end the message with the trailer line `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`.
- No em-dashes (U+2014), no exclamation marks, no CJK in any code, docs, or user-facing strings.
- Gates per task before reporting DONE: `cargo fmt --all -- --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; tests of the touched crate(s) pass.
- Testing rules (docs/TESTING-GUIDELINES.md): integration tests live ONLY in the crate's single binary `tests/main.rs` + `tests/suite/<name>.rs` (add `mod <name>;` to main.rs); unit tests inline in src modules. Tempdirs only, no writes outside them. No real network in tests; local listeners on 127.0.0.1 ephemeral ports only. Executable stub files must be written with a create + write_all + sync_all sequence before exec (ETXTBSY).
- Secrets are never logged, returned, or cached. IMAP results never put subjects, addresses, or bodies into the event log (spec 3.4); error messages are scrubbed against resolved secret values (mirror `SmtpCall.redactions`).
- `apb_core::content::sha256_hex` already returns a `sha256:<hex>` prefixed string. Never wrap it in another `format!("sha256:{}")`.
- Machine-facing fields and all manifests/docs are English.
- Follow existing patterns: when this plan says "mirror X", open X first and copy its structure, naming, and error style.

---

### Task 1: Fix the ETXTBSY flaky test in secrets.rs

**Files:**
- Modify: `crates/apb-core/src/connector/secrets.rs` (test helper `write_stub`, around line 672)

**Interfaces:** none produced; test-only change.

`write_stub` writes an executable stub with `fs::write` and the tests exec it immediately; on Linux CI this intermittently fails with ETXTBSY (os error 26). PR `3fb1d94` fixed the same race for six engine helpers with a write + sync pattern; this helper was added later and missed it.

- [ ] **Step 1: Replace the write with a synced write**

```rust
    fn write_stub(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        // create + write_all + sync_all before exec: without the sync, an
        // immediate execve of the freshly written file can fail with ETXTBSY
        // on Linux (see engine tests/suite/common write_sync, PR #10).
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }
```

- [ ] **Step 2: Run the affected tests**

Run: `cargo test -p apb-core --lib resolve_cmd`
Expected: all `resolve_cmd_*` tests pass.

- [ ] **Step 3: Commit**

`test: sync cmd-secret stub scripts before exec to prevent ETXTBSY flake`

---

### Task 2: Core imap function kind and contract-test shape

**Files:**
- Modify: `crates/apb-core/src/connector/def.rs` (types, exactly-one-kind rule, validation)
- Modify: `crates/apb-core/src/connector/contract.rs` (`expect.imap`)

**Interfaces:**
- Produces (used by Tasks 4, 5, 7):
  - `def.rs`: `pub struct ImapConnection { pub host: String, pub port: String, pub use_tls: String, pub auth_method: String, pub username: String, pub password: String }` (all template strings, `deny_unknown_fields`, derive set identical to `SmtpConnection`).
  - `def.rs`: `pub enum ImapOp { Verify, ListFolders, Search, Fetch, SetFlags }` with `#[serde(rename_all = "snake_case")]`, plus `Copy` and an `pub fn as_str(self) -> &'static str` returning the snake_case name.
  - `def.rs`: `pub struct ImapSpec { pub connection: ImapConnection, pub op: ImapOp, #[serde(default)] pub params: BTreeMap<String, String> }` (`deny_unknown_fields`).
  - `def.rs`: `FunctionSpec` gains `#[serde(default)] pub imap: Option<ImapSpec>` and `pub fn is_imap(&self) -> bool`.
  - `contract.rs`: `pub struct ImapExpect { pub op: String, #[serde(default)] pub folder: Option<String>, #[serde(default)] pub params_contains: Option<BTreeMap<String, String>> }` (`deny_unknown_fields`); `Expectation` gains `#[serde(default)] pub imap: Option<ImapExpect>`; `ExpectKind` gains `Imap(&'a ImapExpect)`; `resolve` checks `imap` FIRST (before `envelope`).

Validation rules to implement in `def.rs` (mirror how `validate_smtp_function` and the exactly-one-kind check are structured today):

1. A function is exactly one of HTTP / mock / smtp / imap; extend the existing exactly-one-kind error to name `imap`.
2. An imap function must not set `query`, `body`, `headers`, or `response_pick` (mirror the smtp rules; response_pick per spec 3.1).
3. Per-op params (spec 3.3): allowed and required keys per op, any other key is an error, any missing required key is an error:
   - `verify`, `list_folders`: no params allowed;
   - `search`: required `folder`, `limit`; optional `unread_only`, `from_contains`, `subject_contains`, `since_days`;
   - `fetch`: required `folder`, `uid`;
   - `set_flags`: required `folder`, `uids`, `seen`.
4. Secret placement: `{{secret.*}}` is allowed in `imap.connection.password` and nowhere else in the imap block; every other connection field and every params value follows function-body rules (`account.*`/`args.*` only). Mirror the smtp secret-placement code path exactly.

- [ ] **Step 1: Write failing unit tests in def.rs `mod tests`**

Test names and assertions:
- `imap_function_parses_minimal`: a YAML function with `imap: { connection: {host: h, port: "993", use_tls: "true", auth_method: password, username: u, password: "{{secret.password}}"}, op: verify }` parses; `is_imap()` is true.
- `imap_and_mock_together_rejected`: a function with both `imap` and `mock` fails the exactly-one-kind rule.
- `imap_with_http_fields_rejected`: `imap` plus `query`/`body`/`headers`/`response_pick` each rejected (message names the offending field).
- `imap_op_unknown_param_rejected`: op `verify` with `params: { folder: X }` rejected.
- `imap_op_missing_required_param_rejected`: op `search` with only `folder` rejected (missing `limit`).
- `imap_secret_in_params_rejected`: `params: { folder: "{{secret.password}}" }` rejected.

- [ ] **Step 2: Run to verify failures**

Run: `cargo test -p apb-core --lib connector::def`
Expected: new tests fail to compile or fail (types missing).

- [ ] **Step 3: Implement the types and validation** (interfaces above; wire validation into the same function-level validate path smtp uses)

- [ ] **Step 4: Write failing contract.rs tests, then implement `ImapExpect`**

- `imap_expect_parses_and_resolves`: a case with `expect: { imap: { op: search, folder: INBOX, params_contains: { limit: "20" } } }` parses and resolves to `ExpectKind::Imap`.
- `imap_expect_unknown_key_rejected`: `imap: { op: x, bogus: 1 }` rejected.
- Update the doc comment at the top of contract.rs: shape discrimination is now `imap` -> imap, `envelope` -> smtp, `status`/`body` -> mock, otherwise HTTP.

- [ ] **Step 5: Run, gates, commit**

Run: `cargo test -p apb-core` then fmt + clippy gates.
Commit: `feat(core): imap function kind schema, validation, and contract expect shape`

---

### Task 3: Typed and optional single-placeholder rendering (spec 5.1)

**Files:**
- Modify: `crates/apb-core/src/connector/template.rs`
- Modify: `crates/apb-engine/src/connector_call.rs` (`render_query`, line ~1007)

**Interfaces:**
- Produces (used by Tasks 4 and 6):
  - `template.rs`: `pub fn single_args_placeholder(s: &str) -> Option<&str>` - `Some(field)` when the whole string is exactly `{{args.<field>}}` (optional surrounding whitespace inside the braces follows whatever `render_raw` tolerates today; when in doubt, exact match only), `None` otherwise. `{{args}}` bare returns `None` (handled separately at body top level).
  - Changed `render_body` semantics, in `render_body_walk`:
    - object entry whose value is a string with `single_args_placeholder` = `Some(f)`: if `ctx.args` has field `f`, insert its typed `serde_json::Value` clone; if absent, drop the entry;
    - array element, same rule: typed value or element dropped;
    - top-level body that is itself a single placeholder: typed value when present, `ConnectorError` naming the field when absent (a whole body must not silently vanish);
    - every other string keeps today's interpolation semantics (absent referenced arg stays an error).
- Changed `render_query`: a query value that is a single placeholder renders from the typed arg: absent arg drops the whole `key=value` pair; string renders verbatim then percent-encoded; number/bool render via their JSON text; array/object is a `config` CallError. Mixed-content values keep today's behavior.

- [ ] **Step 1: Write failing core tests in template.rs `mod tests`**

- `render_body_single_placeholder_is_typed`: body `{"data": {"completed": "{{args.completed}}", "projects": "{{args.projects}}"}}` with args `{"completed": true, "projects": ["12"]}` renders `completed` as boolean true and `projects` as the array.
- `render_body_drops_absent_optional_field`: body `{"data": {"name": "{{args.name}}", "notes": "{{args.notes}}"}}` with args `{"name": "x"}` renders `{"data": {"name": "x"}}` (no `notes` key).
- `render_body_drops_absent_array_element`: body `{"list": ["{{args.a}}", "{{args.b}}"]}` with args `{"a": 1}` renders `{"list": [1]}`.
- `render_body_top_level_single_placeholder_absent_is_error`.
- `render_body_mixed_content_absent_is_still_error`: `{"x": "v={{args.gone}}"}` with empty args errors.
- `render_body_whole_args_forward_unchanged`: existing `"{{args}}"` behavior still passes (extend the existing test only if needed).

- [ ] **Step 2: Run to verify failure** - `cargo test -p apb-core --lib connector::template`

- [ ] **Step 3: Implement in template.rs; run to green**

- [ ] **Step 4: Write failing engine tests for render_query, then implement**

Find where `render_http` unit tests live in the engine (search `render_query` / `render_http` tests; add alongside):
- `render_query_drops_absent_single_placeholder_pair`: function query `{"offset": "{{args.offset}}", "limit": "{{args.limit}}"}` with args `{"limit": 50}` renders `?limit=50` only.
- `render_query_typed_scalars`: `limit` integer 50 renders `limit=50`; bool renders `true`.
- `render_query_non_scalar_single_placeholder_is_config_error`: args `{"limit": [1]}` errors with code `config`.

- [ ] **Step 5: Full-crate runs, gates, commit**

Run: `cargo test -p apb-core && cargo test -p apb-engine`
Commit: `feat: typed and optional single-placeholder rendering in body and query templates`

---

### Task 4: Engine imap executor

**Files:**
- Create: `crates/apb-engine/src/connector_imap.rs`
- Modify: `crates/apb-engine/src/lib.rs` (module), `crates/apb-engine/src/connector_call.rs` (dispatch wiring), `Cargo.toml` (workspace deps) and `crates/apb-engine/Cargo.toml`
- Test: `crates/apb-engine/tests/suite/connector_imap.rs` (+ `mod connector_imap;` in `tests/main.rs`)

**Interfaces:**
- Consumes: `ImapSpec`/`ImapOp` from Task 2; `single_args_placeholder` from Task 3 (params rendering); `CallOk`, `CallError`, `CallErrorCode` from `connector_result.rs`.
- Produces (used by Task 5):
  - `pub enum ImapBuild { DryRun(serde_json::Value), Call(Box<ImapCall>) }`
  - `pub fn build(spec: &ImapSpec, ...) -> Result<ImapBuild, CallError>` with EXACTLY the same parameter shape as `connector_smtp::build` (open connector_smtp.rs first and mirror its signature, including how account fields, args, secrets, timeout, and dry_run arrive).
  - `impl ImapCall`: `pub(crate) fn endpoint(&self) -> String` returning `imap://<host>:<port>`; `pub(crate) fn event_extra(&self) -> (Option<String>, Option<u32>)` returning `(None, None)` (spec 3.4: no subjects in the event log); a send/execute method named the same as `SmtpCall`'s.

Build-time behavior:
- Render connection fields with the same helpers smtp uses; `use_tls` parsing mirrors smtp's (absent account field means default true). `auth_method` must render to `password` or `xoauth2`, anything else is a `config` error naming the account field.
- Render `params` values with function-body rules PLUS the Task 3 single-placeholder semantics: a params value that is a single `{{args.x}}` placeholder with `x` absent is dropped (this is how optional search params work); other values interpolate as strings.
- Parse and validate params into a typed plan (all failures are `invalid_args` naming the param):
  `enum ImapOpPlan { Verify, ListFolders, Search { folder: String, unread_only: bool, from_contains: Option<String>, subject_contains: Option<String>, since_days: Option<u32>, limit: u32 }, Fetch { folder: String, uid: u32 }, SetFlags { folder: String, uids: Vec<u32>, seen: bool } }`
  - `limit` 1..=100; `uid` u32; `uids` a non-empty comma-separated u32 list; `seen`/`unread_only` parse `true`/`false`; `since_days` u32 >= 1; `folder` non-empty.
  - Reject any control character (including CR, LF) in `folder`, `from_contains`, `subject_contains` as `invalid_args` (protocol injection guard, spec section 2).
- Dry-run: return `ImapBuild::DryRun` with `{ "ok": true, "dry_run": true, "imap": { "endpoint": "imap://host:port", "op": "<op>", "params": { ...rendered typed params... } } }`; never include the password; mirror the smtp dry-run envelope style.

Execute-time behavior (blocking, like `SmtpCall`; scrub every outgoing error message against redactions):
- Connect TCP with a connect timeout from `timeout_sec`; set read and write socket timeouts to the remaining budget. `use_tls: true` wraps the stream in rustls (`rustls-platform-verifier` roots); `use_tls: false` stays plaintext (test listener only). Feed the stream to `imap::Client::new`.
- Auth: `password` does LOGIN; `xoauth2` does AUTHENTICATE XOAUTH2 via the `imap` crate's `Authenticator` trait with the SASL payload `user=<username>\x01auth=Bearer <token>\x01\x01` (the crate base64-encodes; verify against its docs and the listener test).
- Ops (spec 3.3): read ops (`search`, `fetch`) open the mailbox with EXAMINE; `set_flags` uses SELECT; `verify`/`list_folders` open none. Message content is fetched ONLY with `BODY.PEEK[]`; the string `BODY[` without a preceding `PEEK` check must not appear in any FETCH the code composes.
  - `verify`: connect + auth + LOGOUT; body `{ "authenticated": true }`.
  - `list_folders`: LIST "" "*"; body `{ "folders": [{ "name", "attributes" }] }`.
  - `search`: compose criteria from the plan (`UNSEEN` when unread_only; `FROM "v"` / `SUBJECT "v"` with `"` and `\` backslash-escaped; `SINCE dd-Mon-yyyy` in UTC from since_days); `UID SEARCH`, take the highest `limit` UIDs, `UID FETCH (FLAGS ENVELOPE RFC822.SIZE INTERNALDATE)`; body `{ "folder", "total_matched", "messages": [{ "uid", "from", "to", "subject", "date", "seen", "size }] }` newest first (fields decoded from the envelope; absent envelope parts render as absent fields).
  - `fetch`: `UID FETCH <uid> (FLAGS BODY.PEEK[])`; parse bytes with `mail-parser`; body `{ "uid", "from", "to", "cc", "subject", "date", "seen", "text", "html", "attachments": [{ "filename", "mime", "size" }], "truncated" }`; `text`/`html` each capped at 262144 bytes with `truncated: true` when anything was cut; a uid with no message is `service`.
  - `set_flags`: `UID STORE <uid-set> +FLAGS (\Seen)` or `-FLAGS (\Seen)`; body `{ "folder", "updated": <count> }`.
- Error mapping (spec 3.4): DNS/connect/TLS -> `network`; LOGIN/AUTHENTICATE rejection -> `auth`; socket timeout or deadline -> `timeout`; NO/BAD (unknown folder, bad uid) -> `service` with the server text; pre-connect validation -> `invalid_args`.

Wiring in connector_call.rs: mirror the smtp arm in `build_prepared` (`if let Some(imap) = &function.imap { ... }` returning `Prepared::DryRun` or a new `Prepared::Imap { call: Box<ImapCall> }`), extend `Prepared::pre_auth_url`, `event_extra`, and `dispatch` accordingly (an imap call has no HTTP status, like smtp).

**Test fixture:** a scripted IMAP listener inside `tests/suite/connector_imap.rs`: std `TcpListener` on 127.0.0.1:0, one accepted connection per test, a thread that reads CRLF lines, records every received line into an `Arc<Mutex<Vec<String>>>`, and answers from a small per-test script keyed on the command word (greeting `* OK apb-test ready`, `CAPABILITY`, `LOGIN`, `AUTHENTICATE XOAUTH2` with a `+ ` continuation then reading the base64 line, `EXAMINE`/`SELECT` (`* 3 EXISTS`, `<tag> OK [READ-ONLY] done` / `OK done`), `UID SEARCH` (`* SEARCH 101 102 103`), `UID FETCH` (canned ENVELOPE or a `{N}` literal with a fixture RFC822 message), `UID STORE` (`* 1 FETCH (FLAGS (\Seen))`), `LOGOUT` (`* BYE`)). Tests run the executor through the same public path the smtp tests use (open `tests/suite/connector_smtp.rs` first and mirror its harness; `use_tls: false`).

- [ ] **Step 1: Add deps** - workspace `Cargo.toml`: `imap = { version = "2.4", default-features = false }`, `mail-parser = "0.11"`, `rustls = "0.23"`, `rustls-platform-verifier = "0.5"` (adjust to the latest compatible versions `cargo add` resolves); engine Cargo.toml gets `.workspace = true` entries.
- [ ] **Step 2: Write the listener fixture and the first failing test** `verify_ok_over_plaintext`.
- [ ] **Step 3: Implement build + verify op until green.**
- [ ] **Step 4: TDD the remaining ops, one test then implementation at a time:**
  - `login_rejected_maps_to_auth_error` (listener answers `<tag> NO [AUTHENTICATIONFAILED] nope`)
  - `xoauth2_sends_expected_sasl_payload` (decode the recorded base64; assert `user=u\x01auth=Bearer tok\x01\x01`)
  - `search_uses_examine_and_composes_criteria` (recorded lines contain `EXAMINE "INBOX"` (or unquoted, per crate), no `SELECT`; SEARCH line contains `UNSEEN`, `FROM`, `SINCE`; quotes in a `subject_contains` value arrive escaped)
  - `search_result_shape_and_order` (canned envelopes; newest first; `total_matched` = 3)
  - `crlf_in_search_values_is_invalid_args` (no connection attempted)
  - `fetch_uses_body_peek_and_parses_mime` (fixture multipart text+html message via literal; assert recorded FETCH contains `BODY.PEEK[]` and no bare `BODY[`; result has text, html, attachments list, `seen`)
  - `fetch_truncates_large_text_part` (unit test on the parse/cap helper with a >256 KiB part; `truncated: true`)
  - `set_flags_uses_select_and_store` (recorded `SELECT`, `UID STORE 101,102 +FLAGS (\Seen)`; and the `-FLAGS` variant)
  - `unknown_folder_no_maps_to_service`
  - `connection_refused_maps_to_network` (port from a dropped listener)
  - `stalled_server_maps_to_timeout` (listener accepts, sends greeting, then sleeps; `timeout_sec: 1`)
  - `dry_run_renders_without_connecting` (no listener at all; assert the dry-run JSON shape and that the password value appears nowhere in it)
  - `bad_params_are_invalid_args` (limit 0, limit 101, empty uids, non-numeric uid)
- [ ] **Step 5: Full runs and gates** - `cargo test -p apb-engine`, fmt, clippy.
- [ ] **Step 6: Commit** - `feat(engine): built-in imap function kind with silent-read guarantees`

---

### Task 5: expect.imap in the offline contract-test runner

**Files:**
- Modify: `crates/apb-engine/src/connector_test.rs`
- Test: inline `mod tests` of connector_test.rs or its existing test location (mirror how smtp expectation tests are placed today)

**Interfaces:**
- Consumes: `ExpectKind::Imap(&ImapExpect)` from Task 2; `connector_imap::build` dry-run from Task 4.

Runner behavior for an imap case: render the function through `connector_imap::build` with `dry_run: true`, secrets stubbed exactly like smtp cases; from the dry-run JSON take `imap.op` and `imap.params`; assert `expect.imap.op` equals the rendered op; when `expect.imap.folder` is present assert it equals `params.folder`; when `params_contains` is present assert each key renders to a value whose JSON text equals the expected string (subset match, mirroring `body_contains` semantics; compare typed values via their JSON string form so `limit: "20"` matches the number 20).

- [ ] **Step 1: Write failing tests** - a temp connector folder with an imap function and three cases: `imap_case_passes_on_match`, `imap_case_fails_on_wrong_op`, `imap_case_fails_on_missing_param` (params_contains key absent from render).
- [ ] **Step 2: Implement, run `cargo test -p apb-engine`, gates.**
- [ ] **Step 3: Commit** - `feat(engine): imap expectations in the offline connector test runner`

---

### Task 6: The asana connector

**Files:**
- Create: `connectors/asana/connector.yaml`, `connectors/asana/PUBLIC.md`, `connectors/asana/tests.yaml`
- Test: `crates/apb-engine/tests/suite/connector_asana.rs` (+ `mod` in tests/main.rs)

**Interfaces:**
- Consumes: Task 3 rendering rules. The manifest CI gate (`tests/suite/connector_manifest.rs` area) picks the folder up automatically; run it and satisfy it.

Manifest header: `name: asana`, `version: 0.1.0`, `healthcheck: get_me`, auth `kind: header`, `header: Authorization`, `value_template: "Bearer {{secret.token}}"`. Account fields: `api_base` (required; PUBLIC.md documents `https://app.asana.com/api/1.0`), `token` (required, secret).

Conventions for every function: descriptions imperative one-liners naming the Asana resource; every GET is `read_only: true`; strict `args_schema` (`required` exactly as listed below; optional args stay out of `required` and their templates rely on the Task 3 drop rule); at least one `examples` entry on `create_task`, `update_task`, and `search_tasks`; at least one tests.yaml case per function.

The full function set (spec 5; `o` = `{{args.<x>}}`):

| name | method, url (after `{{account.api_base}}`) | query | body `data` fields | required args | optional args | response_pick |
|---|---|---|---|---|---|---|
| `get_me` | GET `/users/me` | - | - | - | - | data.gid, data.name, data.email |
| `list_workspaces` | GET `/workspaces` | limit, offset | - | limit | offset | data.gid, data.name, next_page.offset |
| `list_projects` | GET `/projects` | workspace, limit, offset, opt_fields=`name,archived,permalink_url` | - | workspace, limit | offset | data.gid, data.name, data.archived, data.permalink_url, next_page.offset |
| `list_sections` | GET `/projects/{project}/sections` | opt_fields=`name` | - | project | - | data.gid, data.name |
| `list_tasks` | GET `/tasks` | project, limit, offset, completed_since, opt_fields=`name,completed,assignee.name,due_on,permalink_url` | - | project, limit | offset, completed_since | data.gid, data.name, data.completed, data.assignee.name, data.due_on, data.permalink_url, next_page.offset |
| `get_task` | GET `/tasks/{task}` | opt_fields=`name,notes,completed,assignee.name,due_on,permalink_url,projects.name,memberships.section.name,created_at,modified_at` | - | task | - | data.gid, data.name, data.notes, data.completed, data.assignee.name, data.due_on, data.permalink_url, data.projects.name, data.memberships.section.name, data.created_at, data.modified_at |
| `create_task` | POST `/tasks` | - | name, notes, projects: `["{{args.project}}"]`, assignee, due_on | project, name | notes, assignee, due_on | data.gid, data.name, data.permalink_url |
| `update_task` | PUT `/tasks/{task}` | - | name, notes, completed, assignee, due_on | task | name, notes, completed (boolean), assignee, due_on | data.gid, data.name, data.completed, data.permalink_url |
| `add_comment` | POST `/tasks/{task}/stories` | - | text | task, text | - | data.gid, data.created_at |
| `list_comments` | GET `/tasks/{task}/stories` | opt_fields=`text,created_by.name,created_at,resource_subtype` | - | task | - | data.gid, data.text, data.created_by.name, data.created_at, data.resource_subtype |
| `list_subtasks` | GET `/tasks/{task}/subtasks` | opt_fields=`name,completed,assignee.name,due_on` | - | task | - | data.gid, data.name, data.completed, data.assignee.name, data.due_on |
| `create_subtask` | POST `/tasks/{task}/subtasks` | - | name, notes, assignee, due_on | task, name | notes, assignee, due_on | data.gid, data.name, data.permalink_url |
| `add_task_to_section` | POST `/sections/{section}/addTask` | - | task | section, task | - | (none; the response body is empty) |
| `search_tasks` | GET `/workspaces/{workspace}/typeahead` | resource_type=`task` (literal), query, count, opt_fields=`name,completed,permalink_url` | - | workspace, query | count | data.gid, data.name, data.completed, data.permalink_url |

Types in args_schema: gids, offsets, names, notes, due_on (`YYYY-MM-DD`), completed_since are strings (`completed_since` description: `now for open tasks only, or an ISO 8601 timestamp`); `limit` and `count` are integers 1..100; `completed` is a boolean. Every body uses the explicit `data:` nesting; optional body fields and the optional query values are exactly one `{{args.x}}` placeholder each so the Task 3 rules apply. Reference body, create_task:

```yaml
    body:
      data:
        name: "{{args.name}}"
        notes: "{{args.notes}}"
        projects: ["{{args.project}}"]
        assignee: "{{args.assignee}}"
        due_on: "{{args.due_on}}"
```

tests.yaml: one case per function minimum; make the update_task case assert typed rendering (`args: { task: "1", completed: true }`, `body_contains: { data: { completed: true } }`) and add a second create_task case with optional args omitted asserting only required fields render (`body_contains: { data: { name: X } }`).

Engine render tests (`connector_asana.rs`, mirror the mock-HTTP harness used by the wave 1 e2e connector tests): `create_task_body_is_data_wrapped_and_typed` (capture the raw request body; assert exact JSON: no absent optional keys, projects is an array), `update_task_partial_body_drops_absent_fields` (body has only `data.completed`), `typeahead_renders_literal_resource_type_and_query`, `next_page_offset_survives_projection` (mock returns an Asana-shaped page; the picked result contains `next_page.offset`).

PUBLIC.md: what the connector does, the PAT walkthrough (Asana settings, Apps, Developer apps, create a personal access token), the `api_base` value, scopes note (PATs act as the user), pagination note (pass `next_page.offset` back as `offset`), and the `search_tasks` typeahead disclaimer (fuzzy name match, not full-text search).

- [ ] **Step 1: Write connector.yaml function by function; keep `apb connector test --dir connectors/asana` (or the equivalent cargo test) failing-then-green as you add tests.yaml cases.**
- [ ] **Step 2: Run the manifest CI gate** - `cargo test -p apb-engine connector_manifest` (adjust to the actual test name; it must now include asana).
- [ ] **Step 3: Write the four engine render tests; implement nothing new unless they expose a render bug (fix belongs in Task 3 code).**
- [ ] **Step 4: PUBLIC.md; full gates; commit** - `feat(connectors): official asana connector`

---

### Task 7: The imap connector

**Files:**
- Create: `connectors/imap/connector.yaml`, `connectors/imap/PUBLIC.md`, `connectors/imap/tests.yaml`

**Interfaces:** consumes Tasks 2, 4, 5. The manifest gate and `apb connector test` must pass for the folder.

Manifest header: `name: imap`, `version: 0.1.0`, `healthcheck: verify`, no connector-level `auth` (credentials live in the connection block, like smtp). Account fields: `host` (required), `port` (required), `use_tls` (optional; PUBLIC.md documents default true), `auth_method` (required; `password` or `xoauth2`), `username` (required), `password` (required, secret).

Shared connection block on every function (verbatim in each, the format has no shared-block feature):

```yaml
    imap:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        auth_method: "{{account.auth_method}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
```

Functions:

| name | op | params | required args | optional args | read_only |
|---|---|---|---|---|---|
| `verify` | verify | - | - | - | yes |
| `list_folders` | list_folders | - | - | - | yes |
| `search_messages` | search | folder, limit, unread_only, from_contains, subject_contains, since_days (each `"{{args.<x>}}"`) | folder, limit | unread_only, from_contains, subject_contains, since_days | yes |
| `get_message` | fetch | folder, uid | folder, uid | - | yes |
| `mark_read` | set_flags | folder, uids, seen: `"true"` (literal) | folder, uids | - | no |
| `mark_unread` | set_flags | folder, uids, seen: `"false"` (literal) | folder, uids | - | no |

args_schema types: folder a string; limit an integer 1..100; unread_only a boolean; since_days an integer minimum 1; uid an integer; uids a string (description: `comma-separated uid list, for example "101,102"`). `mark_read` and `mark_unread` are separate functions so a grant allowlist can permit one without the other (spec 4). Add `examples` to `search_messages` (unread from the last 7 days) and `mark_read`.

tests.yaml: one case per function; the search case asserts `expect: { imap: { op: search, folder: INBOX, params_contains: { limit: "20", unread_only: "true" } } }`; a second search case omits the optional params and asserts they are absent by NOT listing them (params_contains only checks presence, so also add the fetch and set_flags cases with exact folder assertions).

PUBLIC.md: the silent-read guarantee up front (reading never marks mail seen; the engine reads with BODY.PEEK and opens folders read-only; only mark_read/mark_unread change anything), then per-provider setup exactly as spec 4 lists (Gmail app password + XOAUTH2 note, Yandex app password + enable IMAP, Outlook XOAUTH2-only with a `{{cmd:...}}` token helper example, iCloud app-specific password), then the wave boundary (no delete, no move, no sending; sending is the smtp connector).

- [ ] **Step 1: connector.yaml + tests.yaml, gate green** (`connector_manifest` + the offline runner).
- [ ] **Step 2: PUBLIC.md; gates; commit** - `feat(connectors): official imap connector with silent-read semantics`

---

### Task 8: Demo playbook, docs, live smoke tests

**Files:**
- Create: `examples/playbooks/inbox-triage.yaml`
- Modify: `docs/CONNECTORS.md` (asana and imap sections), the demo-playbook CI test (find where sentry-triage.yaml and release-announce.yaml are validated; add inbox-triage)
- Modify/Create: the wave 1 live-smoke test file (search for `APB_LIVE_TEST_` under `crates/`; add `APB_LIVE_TEST_ASANA` and `APB_LIVE_TEST_IMAP` cases alongside)

**Interfaces:** consumes the shipped manifests from Tasks 6 and 7.

`inbox-triage.yaml` (open `examples/playbooks/sentry-triage.yaml` FIRST and mirror its structure, grants idiom, and prose style): search unread mail (`search_messages`), fetch selected messages (`get_message`), an agent node classifies which represent actionable work, create Asana tasks (`create_task`), mark processed messages read (`mark_read`). Grant allowlists name exactly those five functions (`mark_unread` deliberately absent) with `max_calls` bounds on each grant, mirroring the least-privilege style of the wave 1 demos.

docs/CONNECTORS.md: add asana and imap to whatever per-service setup structure wave 1 established (read the file first; keep its heading style and depth).

Live smoke tests, `#[ignore]` + env-gated like wave 1: asana calls `get_me` plus `list_workspaces`; imap calls `verify` plus `list_folders`. Connection settings come from env vars documented in the test header comment.

- [ ] **Step 1: Playbook + CI validation test green** (`cargo test -p apb-engine <demo test name>`).
- [ ] **Step 2: docs/CONNECTORS.md sections.**
- [ ] **Step 3: Live smoke tests compile and stay ignored** (`cargo test -p apb-engine -- --ignored --list` shows them).
- [ ] **Step 4: Gates; commit** - `docs: inbox-triage demo playbook, connector docs, live smoke hooks`

---

### Task 9: Version 0.5.0 and release notes

**Files:**
- Modify: root `Cargo.toml` (workspace version 0.4.0 -> 0.5.0) and every inter-crate version pin (`grep -rn "0.4.0" Cargo.toml crates/*/Cargo.toml` and update each `version = "0.4.0"` pin; mirror commit `728248d` / the 0.4.0 bump for the exact set), `Cargo.lock` via a build
- Create: `docs/release-notes/v0.5.0.md`

Release notes content (one paragraph = one line, no hard wraps; no AI-authorship markers; heading style copied from `docs/release-notes/v0.4.0.md`): title `apb 0.5.0: asana and mail connectors`; sections for the asana connector (task operations, PAT auth, typed request bodies, cursor pagination), the imap connector and the silent-read engine guarantee (BODY.PEEK plus read-only folder opens, XOAUTH2 via command-sourced tokens, per-provider setup), the template rule (typed and optional single placeholders), the flaky-test fix, and Known limitations (no delete or move, no attachment content, no STARTTLS, no OAuth flows inside apb, Asana premium search excluded).

- [ ] **Step 1: Bump versions; `cargo build` to refresh Cargo.lock; `cargo test --workspace`.**
- [ ] **Step 2: Write the release notes.**
- [ ] **Step 3: Gates; commit** - `chore: bump workspace version to 0.5.0 for the connectors wave 2 release`

---

## Final verification (controller)

After all tasks: `cargo metadata --format-version 1 >/dev/null && code-ranker check .`; `cargo clippy --release --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cd web && bun run test && bun run check` (web untouched, run anyway); whole-branch review; then wait for the owner's push approval.
