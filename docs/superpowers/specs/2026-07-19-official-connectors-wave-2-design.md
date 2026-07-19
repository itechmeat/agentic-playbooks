# Official connectors, wave 2: asana and imap

Date: 2026-07-19
Status: approved design, pending implementation plan
Depends on: 2026-07-18-connectors-design.md, 2026-07-19-official-connectors-design.md

## 1. Purpose and scope

Wave 1 delivered the connector infrastructure and four official connectors
(github, telegram, smtp, sentry). Wave 2 adds two more, chosen because they
unlock agent workflows around work management and mail:

- `asana` - task operations over the Asana REST API. Almost a pure
  manifest (header Bearer auth, nested JSON bodies, response projection,
  body-carried pagination cursors all exist since wave 1), plus one small
  template-engine extension that strict APIs need: typed and optional
  single-placeholder rendering (section 5.1).
- `imap` - silent mailbox access for Gmail, Yandex Mail, Outlook, iCloud,
  and any standard IMAP server. One unified connector, not one per provider:
  the protocol is identical and only the connection settings differ, which
  is exactly what account configs are for. This requires one engine
  extension, a built-in `imap` function kind, in the mold of wave 1's
  `smtp` kind.

The same PR also fixes the flaky pre-existing test
`connector::secrets::tests::resolve_cmd_empty_output_is_error` (section 9).

In scope:

- engine extension: the `imap` function kind (executor, validation, contract
  test support, dry-run);
- XOAUTH2 authentication for IMAP, with the access token delivered through
  the existing `{{cmd:...}}` command-sourced secret mechanism;
- the `asana` and `imap` connector manifests with PUBLIC.md and tests.yaml;
- a demo playbook exercising both connectors end to end;
- live smoke hooks, docs, release notes, version bump to 0.5.0;
- the ETXTBSY flaky-test fix in `secrets.rs`.

Out of scope (section 10): message deletion and moves, attachment content
download, STARTTLS for IMAP, OAuth flows inside apb, Asana webhooks and
premium search, IMAP IDLE push.

## 2. Decisions and their rationale

| Decision | Choice | Why |
|---|---|---|
| One mail connector or one per provider | One `imap` connector; PUBLIC.md documents per-provider setup (Gmail, Yandex, Outlook, iCloud) | The protocol is the same everywhere; provider differences are connection settings, which live in account configs. Per-provider connectors would be three copies of one manifest with different defaults |
| Outlook and OAuth | Engine supports SASL XOAUTH2; the access token comes from the account config via the existing `{{cmd:...}}` mechanism. apb implements no OAuth flow | Consumer Outlook and Microsoft 365 no longer accept password IMAP auth. Embedding browser consent flows, client registration, and refresh-token storage into a local-first runner is heavy and foreign; standard external token helpers already exist. This mirrors wave 1's `gh auth token` decision exactly |
| Sending mail | Not in `imap`; the wave 1 `smtp` connector already sends | One protocol per connector keeps trust surfaces honest. The Sent folder is an ordinary IMAP folder and is readable through `imap` |
| Silent reading | An engine invariant, not a manifest convention: message content is fetched only with `BODY.PEEK[]`, and read-only operations open the mailbox with EXAMINE (read-only select) | With EXAMINE the server itself refuses flag changes, so a read cannot set `\Seen` even by engine bug. Flag changes happen only through explicit non-read_only functions that use SELECT |
| Search input | Structured arguments (folder, unread_only, from_contains, subject_contains, since_days, limit); the engine composes the IMAP SEARCH command | Raw IMAP SEARCH syntax from agent args would be an injection channel into the protocol stream. Structured args keep the surface typed, schema-validated, and safe |
| IMAP TLS | Implicit TLS via rustls (the workspace TLS stack, shared with lettre); `use_tls: false` means plaintext for local test fixtures only | Implicit TLS on port 993 is what every targeted provider uses. STARTTLS support is deferred until a real provider needs it |
| MIME parsing | The `mail-parser` crate; return decoded text (and html when present), list attachments by name, mime type, and size only | Hand-rolling MIME decoding is a bug farm. Attachment bytes stay out of results by design: agent context is the wrong place for binary payloads |
| Asana auth | Personal Access Token, header Bearer | PATs are Asana's supported integration path for individual accounts and fit the local-first model; OAuth app registration adds nothing for a single-user runner |
| Asana task search | Workspace typeahead endpoint, not `/workspaces/{gid}/tasks/search` | The search API requires Asana Premium; typeahead works on every tier and covers the find-a-task-by-name agent need |
| Asana field selection | Fixed `opt_fields` baked into each function's query, projected by `response_pick` | Asana list responses are compact records (gid, name) by default; opt_fields requests exactly the fields the projection exposes, so one round trip returns useful data without kilobytes of unused payload |
| Asana request bodies | Explicit nested body templates plus a new single-placeholder rendering rule (section 5.1), not the wave 1 `body: "{{args}}"` whole-forward | Asana rejects unrecognized fields in `data`, so routing args must not leak into bodies; `completed` is a boolean and `projects` an array, which string interpolation would corrupt; partial `update_task` needs absent optional fields to disappear rather than render as empty strings |
| Flaky test | Fix `write_stub` in `secrets.rs` to sync the stub script before exec, same as engine `common::write_sync` from PR #10 | The ETXTBSY race is test-side only: production `resolve_cmd` executes user-installed helpers, not freshly written files. The minimal fix lands where the race lives |

## 3. The imap function kind

### 3.1 Manifest shape

A function is now exactly one of: HTTP (`method` + `url`), `mock`, `smtp`,
or `imap`. The `imap` block:

```yaml
functions:
  - name: search_messages
    description: List messages in a folder matching structured criteria
    read_only: true
    imap:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        auth_method: "{{account.auth_method}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
      op: search
      params:
        folder: "{{args.folder}}"
        unread_only: "{{args.unread_only}}"
        from_contains: "{{args.from_contains}}"
        subject_contains: "{{args.subject_contains}}"
        since_days: "{{args.since_days}}"
        limit: "{{args.limit}}"
    args_schema: { ... }
```

Rules:

- `op` is a closed enum: `verify`, `list_folders`, `search`, `fetch`,
  `set_flags`. Each op accepts a fixed param set (section 3.3); a param not
  belonging to the op is a validation error, as is a missing required param.
- Secret placement policy extends by exactly one location: `{{secret.*}}`
  is allowed in `auth`, `smtp.connection.password`, and
  `imap.connection.password`, nowhere else. `params` follows function-body
  rules (`account.*` and `args.*` only).
- `response_pick` on an `imap` function is a validation error, same as
  `mock` and `smtp`: the engine already returns a purpose-built envelope.
- An `imap` function must not carry `query`, `body`, or `headers` (HTTP-only
  fields), mirroring the existing smtp rule.
- `connection.auth_method` renders to `password` or `xoauth2`; any other
  value is a `config` error at call time (it comes from the account config,
  so it cannot be validated statically).

### 3.2 Connection and authentication

- `use_tls: true` (the documented default) opens a rustls TLS stream to
  `host:port`; certificate validation uses the platform trust roots. No
  native-tls dependency enters the workspace.
- `use_tls: false` is a plaintext TCP stream. It exists for the local test
  listener; PUBLIC.md never suggests it for real providers.
- `auth_method: password` performs LOGIN with username and password. This
  covers Gmail and Yandex app passwords, iCloud app-specific passwords, and
  self-hosted servers.
- `auth_method: xoauth2` performs `AUTHENTICATE XOAUTH2` with the SASL
  string `user=<username>\x01auth=Bearer <password>\x01\x01`, base64-encoded
  by the engine. The `password` field carries the OAuth access token; the
  recommended account config sources it with `{{cmd:...}}` from any standard
  token helper. apb never sees a refresh token or a client secret.
- The executor is blocking (the `imap` crate over a rustls stream) and runs
  under `spawn_blocking`, the same discipline as the smtp executor. Socket
  read and write timeouts are set from the function's `timeout_sec`, and the
  whole operation races an overall deadline of the same budget.

### 3.3 Operations

| op | params | mailbox open | returns |
|---|---|---|---|
| `verify` | none | none | `{ ok: true, body: { authenticated: true } }` after connect, TLS, auth, LOGOUT |
| `list_folders` | none | none | `{ folders: [{ name, attributes }] }` from LIST "" "*" |
| `search` | `folder` (required), `unread_only` (bool), `from_contains`, `subject_contains`, `since_days` (int), `limit` (required, 1-100) | EXAMINE | `{ folder, total_matched, messages: [{ uid, from, to, subject, date, seen, size }] }`, newest first, at most `limit` |
| `fetch` | `folder` (required), `uid` (required) | EXAMINE | `{ uid, from, to, cc, subject, date, seen, text, html, attachments: [{ filename, mime, size }], truncated }` |
| `set_flags` | `folder` (required), `uids` (required, non-empty array), `seen` (required bool) | SELECT | `{ folder, updated: <count> }` |

Details:

- `search` composes the SEARCH command from present params: `UNSEEN` when
  `unread_only` is true, `FROM "<value>"` and `SUBJECT "<value>"` with
  quoted-string escaping of `"` and `\`, `SINCE <date>` computed from
  `since_days`. Absent optional params contribute nothing. Envelope data
  comes from `UID FETCH (FLAGS ENVELOPE RFC822.SIZE INTERNALDATE)` on the
  last `limit` UIDs of the match set; ENVELOPE fetches never set `\Seen`
  on any server, and EXAMINE makes that structural.
- `fetch` uses `UID FETCH <uid> (FLAGS BODY.PEEK[])`. The engine never
  issues `BODY[]` without `.PEEK` anywhere in the codebase; the test
  listener asserts this (section 7). The fetched message is parsed with
  `mail-parser`; `text` is the decoded plain-text part (or the html part
  converted to nothing - when only html exists, `text` is absent and `html`
  carries it). `text` and `html` are each capped at 256 KiB with
  `truncated: true` set when the cap cut anything; the whole result stays
  under the existing 1 MiB result cap.
- `set_flags` runs `UID STORE <uids> +FLAGS (\Seen)` or `-FLAGS (\Seen)`
  depending on `seen`. Only the `\Seen` flag is exposed; this is a
  deliberate product boundary, not a technical one.
- `verify` is the natural healthcheck target and is `read_only: true`, as
  are `list_folders`, `search`, and `fetch`. `set_flags` is not.

### 3.4 Error mapping and logging

- DNS, connect, and TLS failures are `network`; LOGIN or AUTHENTICATE
  rejection is `auth`; the deadline is `timeout`; NO and BAD protocol
  responses (unknown folder, bad uid) are `service` with the server text in
  the message; parameter problems caught before connecting (empty folder,
  limit out of range, empty uids) are `invalid_args`.
- The event log records host, port, op, folder, and result counts (matched,
  fetched size class, updated) - never subjects, addresses, bodies, or
  credentials. This is stricter than smtp (which logs the subject the
  playbook itself composed): inbound mail content belongs to third parties
  and does not enter the log.
- `--dry-run` renders the connection block (password redacted), the op, and
  the composed params without connecting, consistent with smtp dry-run.

### 3.5 Contract tests

`tests.yaml` gains an `imap` expect shape, checked through the same
offline dry-run path:

```yaml
cases:
  - function: search_messages
    account: { host: imap.example.com, port: "993", auth_method: password, username: u }
    args: { folder: INBOX, unread_only: true, from_contains: "", subject_contains: "", since_days: 7, limit: 20 }
    expect:
      imap:
        op: search
        folder: INBOX
        params_contains: { unread_only: "true", since_days: "7" }
```

`expect.imap` checks `op`, `folder` when the op has one, and
`params_contains` as a subset match over the rendered params, mirroring
`body_contains`.

## 4. The imap connector manifest

Auth: none at the connector level (the `imap` connection block carries
credentials, same as smtp). Account fields: `host` (required), `port`
(required), `use_tls` (optional, default true in PUBLIC.md and the
scaffolded config), `auth_method` (required, `password` or `xoauth2`),
`username` (required), `password` (required, secret).

| Function | op | read_only |
|---|---|---|
| `verify` (healthcheck) | verify | yes |
| `list_folders` | list_folders | yes |
| `search_messages` | search | yes |
| `get_message` | fetch | yes |
| `mark_read` | set_flags (`seen: true`) | |
| `mark_unread` | set_flags (`seen: false`) | |

`mark_read` and `mark_unread` are separate functions rather than one
function with a bool so that a playbook grant allowlist can permit marking
processed mail as read without permitting the reverse.

PUBLIC.md documents per-provider setup:

- Gmail: `imap.gmail.com:993`, app password (requires 2FA), or XOAUTH2 via
  a token helper for accounts that cannot use app passwords.
- Yandex Mail: `imap.yandex.com:993`, app password, IMAP enabled in mailbox
  settings.
- Outlook / Microsoft 365: `outlook.office365.com:993`, XOAUTH2 only; the
  account config sources the token with `{{cmd:...}}` from a helper such as
  oama or mutt_oauth2. Password auth does not work and PUBLIC.md says so
  plainly.
- iCloud: `imap.mail.me.com:993`, app-specific password.

## 5. The asana connector manifest

### 5.1 Engine extension: typed and optional single placeholders

Wave 1 body templates render every string leaf by interpolation and treat a
referenced-but-absent arg as an error, and query values are always sent even
when they render empty. That fits GitHub (which ignores unknown and empty
fields) but not Asana, which rejects unrecognized `data` fields, types
`completed` as a boolean and `projects` as an array, and expects absent
optional fields to be absent. One rendering rule closes the gap:

- A body string leaf or a query value that consists of exactly one
  `{{args.<field>}}` placeholder (nothing else in the string) renders as
  the typed JSON value of that arg: booleans stay booleans, numbers stay
  numbers, arrays stay arrays. In a query value, a non-scalar arg is a
  `config` error.
- If the referenced arg is absent from the call args, the enclosing body
  object field, body array element, or query pair is dropped from the
  rendered request. `required` args are still enforced by `args_schema`
  before rendering, so dropping applies only to genuinely optional args.
- A top-level body that is a bare single placeholder still errors when the
  arg is absent (a whole body that silently vanishes is a bug, not an
  option), and the wave 1 whole-forward `body: "{{args}}"` form is
  unchanged.
- Mixed-content templates (`"prefix {{args.x}}"`) keep the wave 1
  semantics exactly: string interpolation, absent arg is an error.

Existing manifests are unaffected: their body and query args are all
`required`, so the drop rule never fires, and typed rendering of a string
arg produces the same string.

Auth: `header`, `Authorization: Bearer {{secret.token}}`. Account fields:
`api_base` (required, normally `https://app.asana.com/api/1.0`), `token`
(required, secret) - a Personal Access Token. PUBLIC.md walks through
creating one in the developer console.

Asana specifics the manifest encodes:

- Every write request wraps its payload as `{"data": {...}}`; the body
  template expresses this nesting directly (the format already supports
  nested body JSON).
- Every response wraps content in `data`; `response_pick` paths start with
  `data.` and map over arrays midway per wave 1 projection semantics.
- List endpoints take `limit` plus an opaque `offset` cursor; the next
  cursor arrives in the response body at `next_page.offset` and is included
  in `response_pick`, so the agent passes it back explicitly. This is the
  Sentry cursor pattern with a body-carried cursor instead of a Link
  header.
- List and get functions bake a fixed `opt_fields` value into their query
  so the response carries exactly the fields the projection exposes.

Functions (workspace, project, section, and task gids are args everywhere):

| Function | Method and path | read_only |
|---|---|---|
| `get_me` (healthcheck) | GET `/users/me` | yes |
| `list_workspaces` | GET `/workspaces` | yes |
| `list_projects` | GET `/projects` (workspace, limit, offset) | yes |
| `list_sections` | GET `/projects/{gid}/sections` | yes |
| `list_tasks` | GET `/tasks` (project, completed filter, limit, offset) | yes |
| `get_task` | GET `/tasks/{gid}` | yes |
| `create_task` | POST `/tasks` (name, notes, projects, assignee, due_on) | |
| `update_task` | PUT `/tasks/{gid}` (name, notes, completed, assignee, due_on) | |
| `add_comment` | POST `/tasks/{gid}/stories` (text) | |
| `list_comments` | GET `/tasks/{gid}/stories` | yes |
| `list_subtasks` | GET `/tasks/{gid}/subtasks` | yes |
| `create_subtask` | POST `/tasks/{gid}/subtasks` (name, notes, assignee, due_on) | |
| `add_task_to_section` | POST `/sections/{gid}/addTask` (task) | |
| `search_tasks` | GET `/workspaces/{gid}/typeahead` (resource_type=task, query, count) | yes |

`list_tasks` filters incomplete tasks with Asana's `completed_since=now`
idiom when the caller asks for open tasks only. Deliberately excluded:
webhooks and events, custom field CRUD, portfolios, goals, the premium
search API, attachments, batch API.

## 6. Demo playbook

`examples/playbooks/inbox-triage.yaml`: search unread mail
(`search_messages`), fetch the interesting ones (`get_message`), have the
agent classify which represent actionable work, create Asana tasks for
those (`create_task`), and mark the processed messages read (`mark_read`).
Grant allowlists include exactly the five functions named, `mark_unread` is
deliberately absent, and `max_calls` bounds the loop. CI validates the
playbook the same way wave 1 demo playbooks are validated (install
`--from-dir` into a temp config dir, fake accounts, `apb validate`).

## 7. Testing

Three tiers, extending the wave 1 harness:

1. **Manifest CI gate**: the existing test iterates every folder under
   `connectors/` and picks up `asana` and `imap` automatically (parse,
   digest, PUBLIC.md frontmatter, strict schemas, healthcheck exists and is
   read_only, examples validate, at least one tests.yaml case per
   function).
2. **Engine contract tests**:
   - A scripted local IMAP listener (plaintext TCP, canned responses),
     the same pattern as the wave 1 SMTP listener. It asserts on the exact
     client commands: EXAMINE (not SELECT) for read ops, SELECT only for
     `set_flags`, `BODY.PEEK[]` and never bare `BODY[]`, correctly escaped
     SEARCH criteria, the base64 XOAUTH2 string, STORE +FLAGS/-FLAGS
     forms. Error mapping cases: LOGIN NO is `auth`, unknown folder NO is
     `service`, a stalled listener is `timeout`, connection refused is
     `network`.
   - MIME parsing unit tests over fixture messages: multipart
     text+html, html-only, attachments listed without content, the 256 KiB
     truncation flag.
   - Asana render tests through the mock HTTP server pattern: the
     `{"data": ...}` body wrapper, typeahead query rendering, opt_fields
     presence, `next_page.offset` surviving the projection.
3. **Live smoke tests**: `#[ignore]`-marked, env-gated
   (`APB_LIVE_TEST_ASANA=1`, `APB_LIVE_TEST_IMAP=1`), calling each
   connector's healthcheck plus one read_only function against real
   accounts. Never run in CI.

## 8. Release

Workspace version bumps to 0.5.0 (including inter-crate pins), with
`docs/release-notes/v0.5.0.md` covering both connectors, the imap kind, and
the flaky-test fix. Connector versions: `asana 0.1.0`, `imap 0.1.0`.
`docs/CONNECTORS.md` gains per-service setup sections for both.

## 9. Flaky test fix

`connector::secrets::tests::resolve_cmd_empty_output_is_error` (and its
sibling stub-based tests) intermittently fail on Linux CI with ETXTBSY:
`write_stub` writes the executable stub with `fs::write` and `resolve_cmd`
execs it immediately, the exact race PR #10 fixed for six engine helpers.
The fix mirrors PR #10 on the test side: `write_stub` writes through
create + write_all + sync_all before setting the exec bit. Production code
is untouched; `resolve_cmd` executes user-installed helper binaries, which
are never freshly written by the same process.

## 10. Out of scope, wave 3 candidates

- Message deletion, moves between folders, and non-`\Seen` flags; the
  product boundary this wave is silent reading plus processed-marking.
- Attachment content download (needs an artifact story for binary data;
  the node cache artifacts from 0.4.0 may become the vehicle).
- STARTTLS for IMAP (port 143 servers); no targeted provider needs it.
- OAuth flows inside apb (consent, refresh, client registration); the
  token-helper boundary is deliberate.
- IMAP IDLE push and any wait-for-mail primitive; polling inside a node is
  the interim answer, same as Telegram `get_updates`.
- Asana webhooks, custom field CRUD, portfolios, goals, premium search,
  attachments, batch API.
- A shared pagination abstraction; Asana's body cursor is the second
  cursor shape (after Sentry's Link header), and a third connector with
  cursors is the trigger to generalize.

## 11. Delivery slicing

Suggested implementation order, each slice independently landable:

1. Flaky test fix in `secrets.rs` (independent of everything).
2. Core schema: the `imap` function block, op enum, param validation,
   secret placement extension, validator rules, the `expect.imap` contract
   test shape.
3. Template extension: typed and optional single placeholders in body and
   query rendering (section 5.1).
4. Engine: the imap executor (`connector_imap.rs`), TLS and auth, the five
   ops, error mapping, logging, dry-run; the local IMAP listener fixture
   and contract tests.
5. Contract test runner: `expect.imap` execution in the offline runner.
6. The `asana` manifest folder (manifest, PUBLIC.md, tests.yaml) and its
   render tests.
7. The `imap` manifest folder (manifest, PUBLIC.md, tests.yaml).
8. Demo playbook, docs/CONNECTORS.md sections, live smoke tests.
9. Version bump and release notes.

Slices 1, 2, and 3 are independent; 4 needs 2; 5 needs 4; 6 needs 3; 7
needs 2 through 5; 8 and 9 close the wave.
