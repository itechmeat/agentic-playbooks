# Official connectors, wave 3: slack, zulip, gitlab, youtrack, discord

Date: 2026-07-22
Status: approved design, pending implementation plan
Depends on: 2026-07-18-connectors-design.md, 2026-07-19-official-connectors-design.md, 2026-07-19-official-connectors-wave-2-design.md
Source: issue #35 (Create new connectors), brainstormed with the owner on 2026-07-22

## 1. Problem

Waves 1 and 2 delivered the connector infrastructure and six official
connectors (github, telegram, smtp, sentry, asana, imap). Issue #35 asks for
five more official connectors: Slack, Zulip, GitLab, YouTrack, and Discord.
All five are HTTP REST APIs, so no new function kind (in the mold of `smtp`
or `imap`) is required. Two services expose API styles the current HTTP
engine cannot express:

- Zulip requires `application/x-www-form-urlencoded` POST bodies; the engine
  renders JSON bodies only.
- Slack reports errors as HTTP 200 with `"ok": false` in the response body;
  the engine maps errors by HTTP status, so a Slack API failure would look
  like success to retry, fallback, and gate machinery.

## 2. Goals

- Five new official connector manifests: `slack`, `zulip`, `gitlab`,
  `youtrack`, `discord`, each with connector.yaml, PUBLIC.md, and tests.yaml,
  picked up automatically by the manifest CI gate.
- Read plus write scope for the three messaging connectors (channel listing,
  reading recent messages, sending, replying in thread), per owner decision.
- Full github-connector parity for gitlab (issues, merge requests, labels,
  releases, pipeline status and pipeline triggering), and a broad youtrack
  surface (issue search and CRUD, comments, command-driven tagging), per
  owner decision.
- Two small generic engine extensions: `body_form` (form-encoded request
  bodies) and `error_when` (declarative body-error mapping), reusable by
  future connectors.
- The established quality bar: three-tier testing, a demo playbook,
  docs/CONNECTORS.md sections, release notes.

## 3. Non-goals

- No OAuth flows inside apb (consent, refresh, client registration). Tokens
  come from account configs, optionally via the existing `{{cmd:...}}`
  helper mechanism. This repeats the wave 1 and wave 2 boundary.
- No websocket or gateway transports. Discord gateway events, Slack Socket
  Mode, and Zulip real-time event queues are out; reading recent history by
  polling inside a node is the interim answer, same as telegram
  `get_updates`.
- No file or attachment upload and download in any of the five connectors
  (the binary artifact story is still open, see wave 2 section 10).
- No admin surfaces: no user or channel management, no workspace settings,
  no webhook registration.
- No Slack Events API subscriptions and no slash-command hosting; apb is a
  client, not a server, toward these services.
- No shared pagination abstraction yet. Wave 3 adds cursor shapes but each
  stays a plain manifest idiom; generalizing remains a future decision.

## 4. Chosen approach and rationale

Approach A from the brainstorm: two generic engine extensions plus five pure
manifests.

### 4.1 Engine extension: `body_form`

A new optional HTTP-function field `body_form`, a string-to-template map that
renders to an `application/x-www-form-urlencoded` request body.

- Mutually exclusive with `body`; declaring both is a validation error.
- Allowed only on methods that carry bodies (POST, PUT, PATCH, DELETE),
  mirroring the existing `body` rule.
- Values follow function-body template rules: `account.*` and `args.*` only,
  never `secret.*`.
- The wave 2 single-placeholder semantics apply: a value that is exactly one
  `{{args.<field>}}` placeholder renders the typed arg (booleans and numbers
  become their canonical string forms, since form encoding is stringly), and
  when the referenced optional arg is absent the whole key-value pair is
  dropped from the encoded body. Mixed-content templates keep string
  interpolation with absent-arg-is-error.
- The engine sets `content-type: application/x-www-form-urlencoded` and
  percent-encodes keys and values.
- Contract tests gain `expect.body_form_contains`, a subset match over the
  decoded pairs, mirroring `body_contains`.

Rationale: Zulip's write endpoints accept form bodies only. Smuggling write
parameters through the query string is undocumented behavior and puts long
message bodies into URLs and logs; a form body block is small, declarative,
and reusable by any future form-style API.

### 4.2 Engine extension: `error_when`

A new optional connector-level block that declares when a 2xx response is
actually an error:

```yaml
error_when:
  path: ok
  equals: false
  message_path: error
```

- Applies to every HTTP function of the connector. After a 2xx response, the
  engine evaluates `path` against the parsed JSON body; when the value at
  `path` equals `equals`, the call maps to a `service` error whose message
  is the string at `message_path` (or a fixed fallback when absent).
- Non-2xx statuses keep the existing status-table mapping; `error_when` only
  reclassifies false successes.
- `response_pick` is not applied to a response reclassified as an error; the
  error envelope carries the extracted message instead.
- Mock, smtp, and imap functions ignore the block (validation warns if a
  connector has no HTTP functions but declares it).
- Declared connector-level rather than per-function because Slack's
  convention is uniform across its API. A per-function override is deferred
  until a real API needs mixed styles.

Rationale: without this, a Slack failure (`ok: false`) counts as node
success, which silently breaks retries, fallbacks, and policy gates. The
predicate is declarative, engine-verified, and applies to any API with the
envelope-error style.

### 4.3 Why not the alternatives

- Approach B (no engine changes): exposing Slack's `ok` and `error` through
  `response_pick` leaves the engine believing failed calls succeeded, and
  Zulip writes through query parameters are an undocumented path with
  secrets-adjacent data in URLs. Rejected on correctness.
- Approach C (built-in `slack` and `zulip` function kinds like smtp/imap):
  these are plain HTTP APIs; a built-in kind sacrifices manifest
  declarativeness and raises maintenance cost with no capability gain.
  Built-in kinds are for non-HTTP protocols.

## 5. Connector manifests

Decisions the brainstorm resolved from API documentation research (recorded
here so implementation does not re-litigate them):

- Auth styles all fit the existing `AuthSpec` variants: header auth for
  slack (`Bearer`), gitlab (`PRIVATE-TOKEN`), youtrack (`Bearer`), discord
  (`Bot` prefix); basic auth (email plus API key) for zulip. No auth
  extension is needed.
- Healthchecks: slack `auth.test`, zulip `GET /users/me`, gitlab
  `GET /user`, youtrack `GET /users/me`, discord `GET /users/@me`. All are
  read-only in effect; slack's `auth.test` is a POST by API convention but
  mutates nothing and is marked `read_only: true`.
- Pagination differs per service and stays a manifest idiom: slack uses a
  body-carried cursor (`response_metadata.next_cursor`, the asana pattern),
  gitlab uses `page`/`per_page` (the github pattern), youtrack uses
  `$skip`/`$top`, zulip uses `anchor`/`num_before`, discord uses
  `before`/`limit` message ids.
- youtrack requests bake explicit `fields=` values per function, projected
  by `response_pick`, the same discipline as asana `opt_fields`.

### 5.1 slack

Auth: header, `Authorization: Bearer {{secret.token}}` (a bot token).
Account fields: `api_base` (required, normally `https://slack.com/api`),
`token` (required, secret). Connector declares the `error_when` block from
section 4.2.

| Function | Method and path | read_only |
|---|---|---|
| `auth_test` (healthcheck) | POST `/auth.test` | yes |
| `list_channels` | GET `/conversations.list` (types, cursor, limit) | yes |
| `get_messages` | GET `/conversations.history` (channel, cursor, limit) | yes |
| `get_thread` | GET `/conversations.replies` (channel, ts, cursor, limit) | yes |
| `send_message` | POST `/chat.postMessage` (channel, text) | |
| `reply_in_thread` | POST `/chat.postMessage` (channel, thread_ts, text) | |

`send_message` and `reply_in_thread` are separate functions over the same
endpoint so a playbook grant can allow thread replies without allowing new
top-level posts, the same granularity precedent as imap `mark_read` and
`mark_unread`. PUBLIC.md documents the required bot scopes
(`channels:read`, `channels:history`, `chat:write`) and that the bot must be
invited to a channel before reading or posting.

### 5.2 zulip

Auth: basic, username `{{account.email}}`, password `{{secret.api_key}}`.
Account fields: `api_base` (required, `https://<org>.zulipchat.com/api/v1`
or a self-hosted equivalent), `email` (required), `api_key` (required,
secret). Write functions use `body_form` (section 4.1).

| Function | Method and path | read_only |
|---|---|---|
| `get_me` (healthcheck) | GET `/users/me` | yes |
| `list_streams` | GET `/streams` | yes |
| `list_topics` | GET `/users/me/{stream_id}/topics` | yes |
| `get_messages` | GET `/messages` (anchor, num_before, narrow) | yes |
| `send_stream_message` | POST `/messages` (type=stream, to, topic, content) | |
| `send_direct_message` | POST `/messages` (type=direct, to, content) | |

A thread reply in Zulip is a stream message to the same topic, so
`send_stream_message` covers it. `narrow` is passed as Zulip's JSON-encoded
string argument; the manifest documents the format in the arg description
and examples rather than modeling it structurally.

### 5.3 discord

Auth: header, `Authorization: Bot {{secret.token}}`. Account fields:
`api_base` (required, normally `https://discord.com/api/v10`), `token`
(required, secret).

| Function | Method and path | read_only |
|---|---|---|
| `get_me` (healthcheck) | GET `/users/@me` | yes |
| `list_channels` | GET `/guilds/{guild_id}/channels` | yes |
| `get_messages` | GET `/channels/{channel_id}/messages` (limit, before) | yes |
| `send_message` | POST `/channels/{channel_id}/messages` (content) | |
| `reply_to_message` | POST `/channels/{channel_id}/messages` (content, message_reference) | |

Discord threads are channels, so reading a thread is `get_messages` with
the thread's channel id; PUBLIC.md explains this. The REST API returns
message content when the bot has the Read Message History permission; the
gateway-only message-content intent does not apply to REST reads, and
PUBLIC.md spells out the required bot permissions.

### 5.4 gitlab

Auth: header, `PRIVATE-TOKEN: {{secret.token}}` (a personal access token).
Account fields: `api_base` (required, normally
`https://gitlab.com/api/v4`, self-hosted supported by construction), `token`
(required, secret). Project is an arg everywhere (numeric id or URL-encoded
`group%2Fproject` path, documented in PUBLIC.md).

Full parity with the github connector core, mapped to GitLab idioms:

| Function | Method and path | read_only |
|---|---|---|
| `get_user` (healthcheck) | GET `/user` | yes |
| `list_issues` | GET `/projects/{id}/issues` (state, labels, page) | yes |
| `get_issue` | GET `/projects/{id}/issues/{iid}` | yes |
| `create_issue` | POST `/projects/{id}/issues` (title, description, labels) | |
| `update_issue` | PUT `/projects/{id}/issues/{iid}` (title, description, state_event, labels, add_labels, remove_labels, assignee_ids) | |
| `comment_issue` | POST `/projects/{id}/issues/{iid}/notes` (body) | |
| `list_merge_requests` | GET `/projects/{id}/merge_requests` (state, page) | yes |
| `get_merge_request` | GET `/projects/{id}/merge_requests/{iid}` | yes |
| `create_merge_request` | POST `/projects/{id}/merge_requests` (source_branch, target_branch, title, description) | |
| `merge_merge_request` | PUT `/projects/{id}/merge_requests/{iid}/merge` | |
| `approve_merge_request` | POST `/projects/{id}/merge_requests/{iid}/approve` | |
| `comment_merge_request` | POST `/projects/{id}/merge_requests/{iid}/notes` (body) | |
| `create_release` | POST `/projects/{id}/releases` (tag_name, name, description) | |
| `get_release_by_tag` | GET `/projects/{id}/releases/{tag_name}` | yes |
| `list_pipelines` | GET `/projects/{id}/pipelines` (ref, status, page) | yes |
| `get_pipeline` | GET `/projects/{id}/pipelines/{pipeline_id}` | yes |
| `list_pipeline_jobs` | GET `/projects/{id}/pipelines/{pipeline_id}/jobs` | yes |
| `trigger_pipeline` | POST `/projects/{id}/pipeline` (ref, variables) | |

GitLab label editing lives on `update_issue` (`add_labels`,
`remove_labels`), not separate endpoints as on GitHub; the manifest follows
the native API rather than imitating github function names. `trigger_pipeline`
is a write function gated like any other by playbook grant allowlists.

### 5.5 youtrack

Auth: header, `Authorization: Bearer {{secret.token}}` (a permanent token).
Account fields: `api_base` (required, `https://<org>.youtrack.cloud/api` or
self-hosted `/api`), `token` (required, secret). Every function bakes an
explicit `fields=` query value matching its `response_pick`.

| Function | Method and path | read_only |
|---|---|---|
| `get_me` (healthcheck) | GET `/users/me` | yes |
| `list_projects` | GET `/admin/projects` | yes |
| `search_issues` | GET `/issues` (query, $skip, $top) | yes |
| `get_issue` | GET `/issues/{id}` | yes |
| `create_issue` | POST `/issues` (project id, summary, description) | |
| `update_issue` | POST `/issues/{id}` (summary, description) | |
| `list_comments` | GET `/issues/{id}/comments` | yes |
| `add_comment` | POST `/issues/{id}/comments` (text) | |
| `apply_command` | POST `/commands` (query, issue ids) | |

`apply_command` is YouTrack's native mechanism for state changes, tagging,
and field updates through command syntax (for example `state Fixed tag
regression`); it is the idiomatic write path and covers what dedicated
endpoints would otherwise need one function each for. PUBLIC.md documents
the command syntax with examples. `search_issues` uses YouTrack query
syntax, documented in the arg description, same approach as sentry's
`query` arg.

## 6. Demo playbook

`examples/playbooks/release-heartbeat.yaml`: check the latest gitlab
pipeline on the default branch (`list_pipelines`, `get_pipeline`), have the
agent summarize the state, post the summary to a messaging target through
one of the messaging connectors, and on a failed pipeline file a youtrack
issue (`create_issue`). Grant allowlists name exactly the functions used;
`trigger_pipeline` is deliberately absent. CI validates it the same way
prior demo playbooks are validated (install `--from-dir` into a temp config
dir, fake accounts, `apb validate`).

## 7. Testing

Three tiers, extending the existing harness:

1. **Manifest CI gate**: the existing test iterates every folder under
   `connectors/` and picks up all five automatically (parse, digest,
   PUBLIC.md frontmatter, strict schemas, healthcheck exists and is
   read-only, examples validate, at least one tests.yaml case per function).
2. **Engine contract tests**:
   - `body_form` rendering: percent-encoding, typed single placeholders,
     optional-pair dropping, the mutual-exclusion validation error, the
     `expect.body_form_contains` runner support.
   - `error_when`: a mock 200 body with `ok: false` maps to a `service`
     error with the extracted message; `ok: true` passes through; non-2xx
     statuses keep the status-table mapping; the block is ignored for mock
     functions.
   - Render tests per manifest through the mock HTTP server pattern: slack
     cursor propagation through `response_pick`, gitlab `PRIVATE-TOKEN`
     header and page pagination, youtrack `fields=` presence, zulip form
     bodies and basic auth, discord `Bot` prefix.
3. **Live smoke tests**: `#[ignore]`-marked, env-gated
   (`APB_LIVE_TEST_SLACK=1` and so on per service), calling each
   healthcheck plus one read-only function against real accounts. Never run
   in CI.

## 8. Risks

- Slack granular bot scopes: a token missing a scope fails per function,
  not at healthcheck. Mitigation: PUBLIC.md lists the exact scopes per
  function group, and `error_when` surfaces Slack's `missing_scope` error
  text verbatim.
- Discord rate limits are aggressive and per-route. The existing retry and
  timeout machinery applies; PUBLIC.md warns about tight loops, and
  `max_calls` grants bound them. Global rate-limit handling beyond the
  status-table mapping (429) is not special-cased this wave.
- `trigger_pipeline` and `merge_merge_request` are consequential writes.
  They are ordinary grant-gated functions; the demo playbook models the
  restraint pattern by omitting them.
- YouTrack command syntax is powerful enough to change almost anything on
  an issue. `apply_command` is one function, so a grant that includes it
  allows all commands; PUBLIC.md states this plainly so playbook authors
  can decide.
- Zulip self-hosted servers may lag API versions. The manifest sticks to
  long-stable endpoints (`/messages`, `/streams`, `/users/me`).
- Five manifests in one wave is the largest batch so far. The manifest CI
  gate keeps per-connector quality mechanical, and the two engine
  extensions are independent slices that can land first.

## 9. Release

Workspace version bumps to the next free minor at implementation time
(including inter-crate pins), with release notes under
`docs/release-notes/` covering the five connectors and the two engine
extensions. Connector versions: all `0.1.0`. `docs/CONNECTORS.md` gains a
setup section per service. A pending backlog item (publishing apb to
crates.io) is decided during release planning and is out of scope here.

## 10. Acceptance criteria

- `apb` validates and runs playbooks calling every function listed in
  section 5 against mock servers; the manifest CI gate passes for all five
  connectors.
- A Zulip write renders an `application/x-www-form-urlencoded` body with
  correct encoding; declaring both `body` and `body_form` is a validation
  error.
- A Slack-style 200 response with `ok: false` produces a `service` error
  carrying the body's error message; retries and fallbacks trigger on it.
- Healthchecks of all five connectors succeed against real accounts in the
  env-gated live smoke tests (run manually).
- The demo playbook validates in CI.
- PUBLIC.md for each connector documents token creation, required scopes or
  permissions, and per-provider setup where relevant.
- fmt, clippy, code-ranker, and the full test suite are clean.

## 11. Delivery slicing

Each slice independently landable:

1. Engine: `body_form` (schema, validation, rendering, contract-test shape).
2. Engine: `error_when` (schema, validation, mapping, tests).
3. `gitlab` manifest folder and render tests (no engine dependency).
4. `youtrack` manifest folder and render tests (no engine dependency).
5. `discord` manifest folder and render tests (no engine dependency).
6. `slack` manifest folder and render tests (needs 2).
7. `zulip` manifest folder and render tests (needs 1).
8. Demo playbook, docs/CONNECTORS.md sections, live smoke tests.
9. Version bump and release notes.

Slices 1 through 5 are mutually independent; 6 needs 2; 7 needs 1; 8 and 9
close the wave.
