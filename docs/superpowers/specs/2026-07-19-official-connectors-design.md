# Official connectors, wave 1

Date: 2026-07-19
Status: draft
Depends on: 2026-07-18-connectors-design.md

## 1. Purpose and scope

The connector infrastructure shipped in 0.2.0 with only a test fixture. This
story delivers the first four official connectors - `github`, `telegram`,
`smtp`, `sentry` - as product-ready manifests developed inside this
repository, plus the engine extensions they require. There is no marketplace
yet and connector development must not wait for one: the connectors live in a
top-level `connectors/` folder, ship embedded inside the apb binary, and
install into the global scope with one command. When a marketplace appears it
becomes a second install source; the folder format does not change.

The four services were chosen deliberately as different shapes: GitHub is a
large REST surface with CLI-managed credentials, Telegram is a small bot API
with path-embedded auth, SMTP is not HTTP at all, Sentry is a Bearer-token
REST API with cursor pagination. Building them exercises the connector
format, the trust flow, and the call pipeline from four different directions.

In scope:

- repository layout, embedded distribution, `apb connector install`;
- engine extension: command-sourced secrets (`{{cmd:...}}`);
- engine extension: `smtp` function kind;
- engine extension: `path` auth kind with the `{{auth}}` URL placeholder;
- minor format additions: per-function `headers`, a default `User-Agent`,
  per-function `examples` for the agent instruction block;
- response shaping: `response_pick` projections with a `--full` escape;
- declarative connector contract tests (`tests.yaml`, `apb connector test`);
- the four connector manifests with PUBLIC.md storefronts;
- demo playbooks exercising the connectors end to end;
- a dashboard playground for calling connector functions manually;
- validation, tests, and live smoke hooks for all of the above.

Out of scope (section 10): webhooks and triggers of any kind, GitHub App
auth, GraphQL-only operations, multipart uploads, Sentry alert rules, the
script escape hatch, wait-for-external-event primitives.

Reference material: the superplane integrations for the same four services
(`superplanehq/superplane`, commit `35bc83b`, `pkg/integrations/`) were
studied as prior art. Their action lists informed the function sets below;
their architecture (server-side webhook infrastructure, GitHub App manifest
flow, imperative Go per integration) is deliberately not copied because apb
is a local-first runner with a declarative, no-code-execution connector
model.

## 2. Decisions and their rationale

| Decision | Choice | Why |
|---|---|---|
| Where official connectors live | Top-level `connectors/<name>/` in this repo, embedded into the binary via rust-embed | Fast iteration while the format is young (engine and manifests change in one PR); one release artifact - whoever has the binary has the matching connectors; the folder format is exactly the installed format, so the future marketplace migration is moving folders |
| Installation | `apb connector install <name>` materializes the embedded folder into `<config_dir>/connectors/`; `--from-dir <path>` installs from a working copy | Install stays an explicit user action; embedded install can seed trust (below); `--from-dir` keeps the edit-install-run loop during connector development |
| Trust on embedded install | Install from the binary records the trust approval in the same action; `--from-dir` does not | The embedded content is part of the binary the user already executes; a separate approve of bytes shipped inside the trusted binary is ritual without security. Foreign folders keep the full approve flow |
| GitHub auth | Declarative HTTP with header Bearer auth; the token comes from the account config, which may now reference a command: `token: "{{cmd:gh auth token}}"` | Keys-free UX (gh already holds a token on developer machines) without breaking the no-code-execution rule for manifests: the command lives only in the user-owned, digest-pinned account config, never in the marketplace-distributable manifest. The mechanism is generic (glab, `op read`, vault) |
| Telegram auth | New `path` auth kind: the token renders into a URL path segment via a reserved `{{auth}}` placeholder | The Bot API accepts the token only in the URL path (`/bot<token>/method`); no existing auth kind can express that, and `{{secret.*}}` in `url` is forbidden by design. `path` auth keeps the secret confined to the auth block and out of logs, same as `query` auth |
| SMTP | New built-in `smtp` function kind executed natively by the engine (lettre crate) | SMTP is not HTTP; the script escape hatch would be a far heavier trust story. A built-in kind keeps manifests declarative and code-free, is bounded (one protocol, one operation shape), and proves the format extends beyond HTTP without opening arbitrary execution |
| Sentry pagination | Cursor is an explicit function argument; the agent passes the cursor from the previous response | Keeps functions stateless single requests. A generic pagination engine capability is deferred until more than one connector needs it |
| Function granularity | Owner, repo, project and similar routing values are call args, not account fields | One account per service credential serves many repos and projects; pinning them per account would multiply accounts without a security gain (the token scope is the real boundary) |
| Response size control | `response_pick` projection applied by default, full body on `--full` | Raw service responses are dominated by fields agents never need (a GitHub issue is kilobytes of JSON for half a dozen useful fields); trimming by default protects agent context and cost, while the `--full` flag keeps deep debugging possible without a second function |
| Connector correctness gate | Declarative `tests.yaml` in the connector folder, run offline by `apb connector test` | Deepens the CI gate from "parses" to "renders the right requests"; becomes the submission requirement when the marketplace arrives, so official connectors pioneer the format |

## 3. Repository layout and distribution

```
connectors/
  github/
    connector.yaml
    PUBLIC.md
    tests.yaml
  telegram/
    connector.yaml
    PUBLIC.md
    tests.yaml
  smtp/
    connector.yaml
    PUBLIC.md
    tests.yaml
  sentry/
    connector.yaml
    PUBLIC.md
    tests.yaml
```

The folder sits at the repository top level, next to `crates/` and `web/`,
because it is a product artifact, not Rust source. It is embedded into
`apb-core` with rust-embed (the same pattern as `assets/models.yaml`), so
every crate above core can enumerate and read the official set.

CLI:

- `apb connector install <name>` - copies the embedded folder into
  `<config_dir>/connectors/<name>/` and records the connector trust approval
  for its tree digest in the same action. Refuses if the target folder exists
  and differs from what would be installed (`--force` overwrites); a
  same-digest reinstall is a no-op.
- `apb connector install --from-dir <path>` - installs from any folder on
  disk (validated first: manifest parses, digest computes). Does NOT record
  trust: the normal approve flow applies. This is both the developer loop for
  this repository (`--from-dir ./connectors/github`) and the general
  sideloading path for third-party connectors.
- `apb connector list` gains an "available" section listing embedded
  connectors that are not installed yet, with versions.
- `apb connector test [<name> | --dir <path>]` - runs the declarative
  contract tests of an installed connector, an embedded one, or a folder on
  disk (section 4.6). Fully offline; exit code reflects the outcome.

`tests.yaml` sits inside the connector folder and is covered by the tree
digest like every other file - editing tests drops trust, which is correct:
the tests document what the user approved the connector to do.

Version discipline: each connector carries its own semver in
`connector.yaml`, bumped in the PR that changes the folder. The embedded set
is whatever the built binary contains, so a binary release implicitly
publishes the connector versions present at build time. `install` prints the
installed version; installing over an older version with `--force` is the
upgrade path (trust re-records for the new digest).

## 4. Engine extensions

### 4.1 Command-sourced secrets

Account config secret fields currently accept exactly one `{{env.VAR}}`
reference. They now alternatively accept exactly one command reference:

```yaml
accounts:
  - name: personal
    token: "{{cmd:gh auth token}}"
```

Semantics:

- The text after `cmd:` is parsed into argv with shell-words rules (quoted
  arguments supported). No shell is involved; the binary is resolved via
  `PATH`. Pipes, redirection, and substitution are not supported and not
  wanted - a wrapper script is the answer for complex cases.
- Executed at secret resolution time (call, healthcheck, doctor) with a 10
  second timeout. Stdout with trailing whitespace trimmed is the secret
  value. A non-zero exit, a timeout, or empty stdout is a `config` error
  naming the account and field; the error message includes a trimmed stderr
  excerpt (stderr of credential helpers carries diagnostics, not secrets).
- Resolution order for a secret field is unchanged conceptually: the field
  holds exactly one reference, either `{{env.VAR}}` or `{{cmd:...}}`; the
  validator rejects anything else, including mixed content.
- The resolved value lives only in the memory of the resolving apb process,
  exactly like env-sourced secrets, and is covered by the interim literal
  redaction.
- Env scrubbing is unaffected: command-sourced secrets add no env names. A
  scrubbed agent child can still run `gh auth token` itself only if the
  user's own tooling allows it; apb neither helps nor hinders that (see the
  threat model of the base spec - same-OS-user processes are outside apb's
  boundary).

Trust: the account digest MUST cover secret field references (the literal
`{{env.VAR}}` / `{{cmd:...}}` strings, never resolved values). Otherwise an
edit to a shared project config could silently swap `gh auth token` for a
malicious command without dropping account trust - exactly the attack class
account pinning exists to stop. If the current canonical serialization
excludes secret references, this story extends it; existing approved
accounts then drop trust once and need a one-time re-approve, which is
acceptable and honest.

### 4.2 The smtp function kind

A function is now exactly one of: HTTP (`method` + `url`), `mock`, or
`smtp`. The `smtp` block:

```yaml
functions:
  - name: send_email
    description: Send an email over SMTP
    smtp:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
      message:
        from_email: "{{account.from_email}}"
        from_name: "{{account.from_name}}"
        to: "{{args.to}}"
        cc: "{{args.cc}}"
        bcc: "{{args.bcc}}"
        subject: "{{args.subject}}"
        body_text: "{{args.body_text}}"
        body_html: "{{args.body_html}}"
    args_schema: { ... }
  - name: verify
    description: Probe the SMTP connection without sending
    read_only: true
    smtp:
      connection: { ... same shape ... }
      verify: true
```

Rules:

- Secret placement policy extends by exactly one location: `{{secret.*}}` is
  allowed in `auth` and in `smtp.connection.password`, nowhere else. The
  `message` block follows function-body rules (`account.*` and `args.*`
  only).
- A `verify: true` function carries only `connection` and performs
  connect, EHLO, STARTTLS when enabled, AUTH when credentials are present,
  QUIT - no message. It is the natural `healthcheck` target.
- Execution uses the lettre crate: STARTTLS with TLS 1.2 minimum,
  multipart/alternative built by the library when both `body_text` and
  `body_html` are present, single part when only one is. No hand-rolled MIME
  or HTML stripping (superplane hand-builds these in Go; lettre makes that
  unnecessary).
- `to`, `cc`, `bcc` accept a comma-separated list; every address is parsed
  and validated before connecting; a bad address is `invalid_args`.
- Error mapping: connection or DNS failure is `network`; SMTP auth rejection
  is `auth`; the per-function timeout is `timeout`; any other SMTP protocol
  rejection is `service` with the SMTP reply code in the message. Success
  returns `{ ok: true, body: { accepted: [...], from, subject } }`.
- `--dry-run` renders the message envelope and headers without connecting,
  consistent with HTTP dry-run.
- The event log records host, port, subject, and recipient count - never
  message bodies and never credentials.

### 4.3 The path auth kind

```yaml
auth:
  kind: path
  value_template: "bot{{secret.token}}"
```

A connector with `path` auth must use the reserved `{{auth}}` placeholder in
every HTTP function `url`, exactly once:

```yaml
url: "https://api.telegram.org/{{auth}}/sendMessage"
```

Rules:

- `{{auth}}` is valid only in `url`, only once per URL, and only when the
  connector's auth kind is `path`; every other placement is a validation
  error. With `path` auth, an HTTP function without `{{auth}}` is also an
  error (a call that silently goes out unauthenticated is a bug, not a
  feature).
- The rendered value is inserted as one path segment, percent-encoded per
  RFC 3986 pchar rules, which keep `:` literal - Telegram tokens contain a
  colon and must survive rendering verbatim.
- Logging: the recorded pre-auth URL keeps the literal `{{auth}}`
  placeholder unrendered, so the token never reaches the event log - the
  same guarantee `query` auth already has.
- Mock functions and dry-run never render auth, unchanged.

### 4.4 Minor format additions

- `FunctionSpec` gains an optional `headers` map (string to template
  string). Values may use `{{account.*}}` and `{{args.*}}`; `{{secret.*}}`
  stays forbidden outside `auth`. Needed for GitHub's
  `X-GitHub-Api-Version` and `Accept` headers; generally useful.
- The HTTP executor always sends `User-Agent: apb/<version>` unless the
  function's `headers` override it. GitHub rejects requests without a
  User-Agent, and reqwest does not set one by default; this must be verified
  against the current executor and added if absent.
- The call result gains an optional `link` field carrying the response
  `Link` header verbatim when the service sends one. Cursor pagination
  (Sentry, and GitHub beyond `page` counting) is expressed only in that
  header, so without surfacing it the agent cannot obtain the next cursor.
  No other response headers are exposed.
- `FunctionSpec` gains an optional `examples` list; each entry is
  `{ args: <object>, note: <string> }`. Examples render into the generated
  agent instruction block after the function description, and the validator
  checks every example's `args` against the function's `args_schema`, so an
  example cannot drift from the schema silently. One or two examples per
  non-obvious function (Telegram `parse_mode`, GitHub label arrays) is the
  intended dose.

### 4.5 Response shaping: response_pick and --full

An HTTP function may declare a projection over the response body:

```yaml
- name: list_issues
  ...
  response_pick: [number, title, state, html_url, user.login, labels.name]
```

Rules:

- Paths are dot-separated field chains. Applied to an object, a path keeps
  that field; applied to an array (at the top level or midway through a
  chain), the projection maps over the elements. Missing paths are silently
  absent - services add and remove fields, and a projection must not turn
  that into an error.
- When `response_pick` is present the call result body is the projection,
  and the result carries `"picked": true` so the agent knows it is looking
  at a subset.
- `apb connector call --full` skips the projection and returns the complete
  body (still under the existing 1 MiB cap). The generated instruction block
  documents the flag as the debugging escape: normal calls stay light, and
  the agent reaches for `--full` only when a problem needs the whole
  payload. A `--full` call is an ordinary call in every other respect
  (grants, budget, events).
- The event log keeps recording the raw truncated body regardless of
  `response_pick`, so run forensics never depend on what a manifest chose to
  expose to the agent.
- `response_pick` on `mock` and `smtp` functions is a validation error: the
  first returns authored payloads, the second returns a fixed envelope.

All four official connectors declare `response_pick` on every list-shaped
and get-shaped HTTP function; the picked sets are chosen while writing the
contract tests, function by function.

### 4.6 Declarative contract tests: tests.yaml

`tests.yaml` holds offline cases asserting what a function renders, executed
through the same code path as `--dry-run` (secrets stubbed, no network):

```yaml
cases:
  - function: create_issue
    account: { api_base: https://api.github.com }
    args: { owner: acme, repo: site, title: "Broken build" }
    expect:
      method: POST
      url: https://api.github.com/repos/acme/site/issues
      body_contains: { title: "Broken build" }
  - function: send_email
    account: { host: smtp.example.com, port: "587", from_email: a@b.c }
    args: { to: x@y.z, subject: Hi, body_text: Test }
    expect:
      envelope: { from: a@b.c, to: [x@y.z], subject: Hi }
```

- `account` supplies fake non-secret field values; secret fields resolve to
  a fixed stub (`test-secret`), never to real configuration.
- `expect` for HTTP functions checks method, the fully rendered pre-auth
  URL, optional `headers`, and `body_contains` (subset match, so tests stay
  robust to field additions). For `smtp` functions it checks the envelope;
  for `mock` functions the canned status and body.
- `apb connector test` runs every case; the CI manifest gate (section 8)
  runs it for all embedded connectors and additionally requires at least one
  case per function in official connectors.

## 5. The four connectors

Common conventions: every GET-shaped function is `read_only: true`; every
connector declares a cheap read-only `healthcheck`; `args_schema` is strict
(`required` lists populated, no permissive empty schemas); descriptions are
written for the agent instruction block - imperative, one line, naming the
service resource.

### 5.1 github

Auth: `header`, `Authorization: Bearer {{secret.token}}`. Account fields:
`api_base` (required, `https://api.github.com` for github.com, another value
for GHES), `token` (required, secret). The recommended account config uses
`token: "{{cmd:gh auth token}}"`; `{{env.GITHUB_TOKEN}}` is the fallback for
machines without gh. Headers on every function: `Accept:
application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`.

Functions (owner, repo, and numbers are args everywhere):

| Function | Method and path | read_only |
|---|---|---|
| `get_rate_limit` (healthcheck) | GET `/rate_limit` | yes |
| `list_issues` | GET `/repos/{o}/{r}/issues` (state, labels, page) | yes |
| `get_issue` | GET `/repos/{o}/{r}/issues/{n}` | yes |
| `create_issue` | POST `/repos/{o}/{r}/issues` | |
| `update_issue` | PATCH `/repos/{o}/{r}/issues/{n}` | |
| `comment_issue` | POST `/repos/{o}/{r}/issues/{n}/comments` | |
| `add_labels` | POST `/repos/{o}/{r}/issues/{n}/labels` | |
| `remove_label` | DELETE `/repos/{o}/{r}/issues/{n}/labels/{name}` | |
| `add_assignees` | POST `/repos/{o}/{r}/issues/{n}/assignees` | |
| `list_pulls` | GET `/repos/{o}/{r}/pulls` (state, base, page) | yes |
| `get_pull` | GET `/repos/{o}/{r}/pulls/{n}` | yes |
| `create_pull` | POST `/repos/{o}/{r}/pulls` | |
| `merge_pull` | PUT `/repos/{o}/{r}/pulls/{n}/merge` | |
| `request_reviewers` | POST `/repos/{o}/{r}/pulls/{n}/requested_reviewers` | |
| `create_review` | POST `/repos/{o}/{r}/pulls/{n}/reviews` | |
| `create_release` | POST `/repos/{o}/{r}/releases` | |
| `get_release_by_tag` | GET `/repos/{o}/{r}/releases/tags/{tag}` | yes |
| `dispatch_workflow` | POST `/repos/{o}/{r}/actions/workflows/{file}/dispatches` | |
| `list_workflow_runs` | GET `/repos/{o}/{r}/actions/workflows/{file}/runs` (page) | yes |
| `list_check_runs` | GET `/repos/{o}/{r}/commits/{ref}/check-runs` | yes |
| `get_combined_status` | GET `/repos/{o}/{r}/commits/{ref}/status` | yes |

Deliberately excluded: `mark_pull_ready_for_review` (GraphQL-only endpoint;
the format is REST-only), reactions, deployments (rarely agent-driven), all
webhooks.

### 5.2 telegram

Auth: `path`, `value_template: "bot{{secret.token}}"`. Account fields:
`api_base` (required, normally `https://api.telegram.org`; overridable for
self-hosted Bot API servers), `token` (required, secret) - the BotFather
token.

| Function | Method and path | read_only |
|---|---|---|
| `get_me` (healthcheck) | GET `{api_base}/{{auth}}/getMe` | yes |
| `send_message` | POST `{api_base}/{{auth}}/sendMessage` (chat_id, text, parse_mode) | |
| `edit_message_text` | POST `{api_base}/{{auth}}/editMessageText` | |
| `get_chat` | POST `{api_base}/{{auth}}/getChat` | yes |
| `get_updates` | POST `{api_base}/{{auth}}/getUpdates` (offset, timeout) | yes |
| `answer_callback_query` | POST `{api_base}/{{auth}}/answerCallbackQuery` | |

`get_updates` long-polls within the request; its `timeout_sec` is 75 with
the long-poll `timeout` arg capped at 60 by `args_schema`, so the HTTP
timeout always outlives the poll. This gives playbooks a pull-based way to
react to replies without any webhook infrastructure. `send_document` and
other uploads are excluded (multipart is out of scope this wave).

### 5.3 smtp

Auth: none (the `smtp` connection block carries credentials). Account
fields: `host` (required), `port` (required), `username` (optional),
`password` (optional, secret), `from_email` (required), `from_name`
(optional), `use_tls` (optional, default true expressed in PUBLIC.md and the
scaffolded config, since account fields carry no schema defaults).

Functions: `verify` (healthcheck, `smtp` with `verify: true`, read_only) and
`send_email` (section 4.2). One operation is the honest surface of SMTP -
superplane's SMTP integration exposes exactly one action too.

### 5.4 sentry

Auth: `header`, `Authorization: Bearer {{secret.token}}`. Account fields:
`base_url` (required, `https://sentry.io` or self-hosted), `org` (required,
the organization slug), `token` (required, secret) - a user auth token with
`project:read`, `event:read`, `issue:write` (data functions) and
`project:releases` (release and deploy functions); PUBLIC.md spells the
scopes out.

| Function | Method and path | read_only |
|---|---|---|
| `list_projects` (healthcheck) | GET `/api/0/organizations/{org}/projects/` | yes |
| `list_issues` | GET `/api/0/organizations/{org}/issues/` (query, project, cursor) | yes |
| `get_issue` | GET `/api/0/issues/{issue_id}/` | yes |
| `update_issue` | PUT `/api/0/issues/{issue_id}/` (status, assignedTo) | |
| `create_release` | POST `/api/0/organizations/{org}/releases/` | |
| `create_deploy` | POST `/api/0/organizations/{org}/releases/{version}/deploys/` | |

Pagination: `list_issues` takes an optional `cursor` arg; the next cursor
comes from the response `Link` header surfaced through the call result's
`link` field (section 4.4), and the agent passes it explicitly. Excluded: the whole alert-rule CRUD
domain (the majority of superplane's Sentry complexity, rare in agent
playbooks), webhooks, the internal-integration reconciliation machinery,
`LinkGitHubIssue` (cross-connector orchestration belongs in playbooks, not
inside a connector).

## 6. Demo playbooks

Two official example playbooks live in `examples/playbooks/` and exercise
the connectors end to end. They are simultaneously integration tests of the
whole chain, living documentation of connector binding, and seed content for
the future marketplace:

- `sentry-triage.yaml` - pull fresh Sentry issues (`list_issues`), have the
  agent assess them, open GitHub issues for the real ones (`create_issue`),
  and post a summary to a Telegram chat (`send_message`).
- `release-announce.yaml` - create a GitHub release (`create_release`), then
  announce it by email (`send_email`) and Telegram (`send_message`).

Both use grant allowlists and `max_calls` the way the documentation
recommends, so they double as the reference for least-privilege binding. CI
validates them: a test installs the repo connectors `--from-dir` into a
temporary config dir, configures fake accounts, and runs `apb validate` on
each playbook, so a manifest change that breaks a demo playbook fails the
build. They are not executed against real services in CI.

## 7. Dashboard playground

The connector detail page gains a playground panel: pick a function, get a
form generated from its `args_schema`, pick an account, and call it with a
dry-run/real toggle. The result renders structured (status, body or error
code, `link`, `picked`). Real calls go through the standard call path with
the same trust gating as the healthcheck probe: an unapproved connector or
account refuses with `permission`. Dry-run works without secrets and shows
the rendered request. This is the primary manual tool for shaking down the
four connectors without authoring a playbook first.

The server side is one endpoint (`POST /api/connectors/:name/call`) wrapping
the same execution the healthcheck button already uses, extended with args
and the dry-run flag.

## 8. Testing

Three tiers:

1. **Manifest CI gate**: a test iterates every folder under `connectors/`,
   asserts the manifest parses, the name matches the folder, the tree digest
   computes, PUBLIC.md frontmatter parses, every `args_schema` is a valid
   JSON Schema object with non-empty `required` where args exist, every
   healthcheck target exists and is `read_only` or mock, every example
   validates against its schema, and every function has at least one
   `tests.yaml` case, all of which pass (section 4.6). This is the gate that
   keeps official connectors always installable and correct.
2. **Engine contract tests**: the new kinds run against local ephemeral
   servers, extending the existing `mock-tracker` e2e pattern - an HTTP
   server asserting on the rendered requests of representative github,
   telegram, and sentry functions (auth header, path-auth segment, custom
   headers, percent-encoding, error mapping), and a local SMTP test listener
   asserting on EHLO, STARTTLS negotiation, AUTH, and the MIME structure of
   `send_email`. Command-sourced secrets are tested with a stub executable
   on PATH (success, non-zero exit, timeout, empty output).
3. **Live smoke tests**: `#[ignore]`-marked tests behind env flags
   (`APB_LIVE_TEST_GITHUB=1` and similar) that call each service's
   healthcheck plus one read_only function with real accounts. Never run in
   CI; exist for manual pre-release verification and for developing against
   real services.

## 9. Security notes

- Embedded install seeds trust because the connector bytes are part of the
  executing binary; anything installed `--from-dir` or hand-copied keeps the
  full approve flow. The trust store format does not change.
- Command-sourced secret references join the account digest (section 4.1);
  this is the one place this story touches trust semantics and it is a
  strictly tightening change.
- `path` auth keeps the token out of the recorded URL by logging the
  unrendered placeholder; `smtp` logs envelope metadata only. Both new kinds
  preserve the invariant that secrets appear in no manifest, log, prompt, or
  CLI output.
- The four official manifests are themselves subject to the standard
  validation: secrets confined to `auth` (and `smtp.connection.password`),
  strict schemas, no unknown fields.

## 10. Out of scope, wave 2 candidates

- Webhooks, triggers, and any inbound event delivery (superplane's biggest
  surface; needs a server story apb does not have yet).
- A generic wait-for-external-event engine primitive (what superplane's
  Telegram `WaitForButtonClick` really is); `get_updates` polling inside a
  node is the interim answer.
- GitHub App auth, JWT signing, installation tokens.
- GraphQL functions, multipart and file upload bodies.
- Generic pagination strategies; revisit when a third connector needs
  cursors.
- Sentry alert-rule CRUD.
- The script escape hatch for arbitrary protocols; `smtp` as a built-in kind
  deliberately postpones it.
- Marketplace distribution; this story's install command is designed to gain
  a remote source without changing the folder format.
- Engine-side auto-retry of `read_only` functions on `network` and
  `rate_limited` outcomes (respecting `retry_after_sec`); today the agent
  decides, which is correct but spends agent turns on mechanical retries.
- `apb connector update` - detect installed-versus-embedded version drift
  after a binary upgrade and reinstall in one step; in this wave
  `connector list` showing both versions is the interim answer.

## 11. Delivery slicing

Suggested implementation order, each slice independently landable:

1. Format and engine: `headers` map, default User-Agent, `path` auth,
   `{{auth}}` placeholder, `link` result field, `response_pick` with
   `--full`, `examples` (core schema + engine rendering + tests).
2. Command-sourced secrets, including the account-digest extension.
3. The `smtp` function kind.
4. `connectors/` folder embedding, `install` command, trust seeding,
   `connector test` runner, CI manifest gate.
5. The four manifests plus PUBLIC.md and tests.yaml, contract tests, live
   smoke tests, demo playbooks, docs (`docs/CONNECTORS.md` section on
   official connectors and per-service setup).
6. Dashboard playground (server endpoint plus web UI).

Slices 1 through 3 are independent of each other; 4 needs nothing from 1-3;
5 needs 1-4; 6 needs 5 only for manual verification, its code depends on
none of the manifests.
