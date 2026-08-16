# Connectors

A connector links an `agent_task` node and its agent to an external service (a
task tracker, a messenger, mail, and similar). It gives the node a named set of
callable functions, similar in spirit to MCP but scoped to one service and
granted per node. Secrets never leave the `apb` process: functions are
declarative HTTP defined in `connector.yaml` and executed by `apb`, so a
connector cannot run arbitrary code and a token is only ever referenced by name.

Full design: [superpowers/specs/2026-07-18-connectors-design.md](superpowers/specs/2026-07-18-connectors-design.md).

## Installing a connector

A connector is a folder; installation is copying it and removal is deleting it:

- global scope: `<config-dir>/connectors/<name>/`

with:

- `connector.yaml` - the machine part, the only file the engine reads at run
  time (auth block, `account_fields`, and `functions`);
- `PUBLIC.md` - the storefront (YAML frontmatter plus a markdown body), rendered
  by the dashboard, never read at run time;
- `README.md` - the human setup page (see below);
- `INSTALL.md` - the agent setup runbook (see below);
- `tests.yaml` - the offline contract cases run by `apb connector test <name>`;
- `skills/` - reserved and covered by the digest, not delivered to prompts yet.

The folder name is the connector name and must pass the same slug rule as
profiles and skills (`[a-z0-9][a-z0-9-]*`, at most 64 chars). Scaffold a fresh
one with `apb connector init <name>`.

## README.md and INSTALL.md

Every official connector carries two setup documents, and a new connector is expected to do the same. They exist because configuring a connector by hand is several steps in several places (an account config, a dotenv, two trust approvals, a live probe), and the person doing it should not have to hold that sequence in their head.

`README.md` is for the human, and it opens by saying that the shortest path is to hand the job to an agent, with a ready-to-paste prompt that names the connector and points at `INSTALL.md`. The rest is only what the person has to decide or supply themselves: which credential is needed and how to create it, what the connector can and cannot do, and which functions are irreversible enough to be worth restricting in a grant.

`INSTALL.md` is the runbook an agent follows, written as ordered steps ending in a report. It covers installing the connector, the service-specific settings and credential (with concrete hosts, scopes, and where in the service's UI a token is created), the choice between a global and a project account, the secrets dotenv, both trust approvals, the live healthcheck, and what each common failure actually means. Its standing rules are that a secret is never echoed, logged, or committed, that the user is offered the option of filling the dotenv value in themselves so the credential never enters the transcript, and that a run is never started as part of setup.

Both files ship inside the connector folder, so `apb connector install <name>` materializes them next to the manifest at `<config-dir>/connectors/<name>/INSTALL.md`. That is what makes the README's prompt work: an agent installs the connector, then reads its runbook from disk.

Neither file is read at run time, and neither is used by the dashboard, which renders `PUBLIC.md`. They are covered by the connector's tree digest like every other file in the folder, so editing one drops connector trust until it is approved again.

## Configuring accounts

An account tells the connector where to send a call and which secret to use. The
config files are non-secret and safe to commit and share:

- global: `<config-dir>/connector-config/<connector>.yaml`
- project: `<project>/.apb/connector-config/<connector>.yaml`

```yaml
accounts:
  - name: project-board          # slug, unique within the merged list
    default: true                # used when a grant has several accounts and no --account
    base_url: https://client.example.net
    token: "{{env.PROJECT_BOARD_TOKEN}}"
```

The merged list is global accounts plus project accounts; a project account with
the same name replaces the global one, all others are additive. A `secret: true`
field must be exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`
(a command whose stdout is the secret, resolved at call time, e.g.
`token: "{{cmd:gh auth token}}"`); a literal secret in a config file is a
validation error. At most one `default: true` per merged list.

## Secrets

An `{{env.VAR}}` reference resolves at call time, in order: the process
environment, then the project `<project>/.apb/secrets.env`, then the global
`<config-dir>/secrets.env`. Dotenv files are `KEY=value` lines read only by
`apb`. A `{{cmd:<command>}}` reference instead runs the command (shell-words
argv, no shell, 10 second timeout) at call and healthcheck time and uses its
trimmed stdout as the value; the command string is part of the account digest,
so changing it requires re-approval. Secret values never appear in the run
manifest, the event log, CLI output, or generated prompts, and the connector
env names are scrubbed from every spawned agent's environment.

List the variables an account still needs (names only, never values) with:

```sh
apb connector env <name>          # print the KEY= template lines to stdout
apb connector env <name> --write  # write them into .apb/secrets.env instead
```

Plain `env` prints the missing `KEY=` lines. With `--write`, `apb` appends those
lines to the project `<project>/.apb/secrets.env`, creating the file at mode 0600
when it does not exist, preserving anything already there, and never duplicating a
key the file already lists; it then makes sure `.gitignore` covers
`.apb/secrets.env`. Either way the values stay empty for you to fill in by hand;
`apb` never writes a secret value.

## Approving trust

A foreign `connector.yaml` (URL templates plus secrets) is as dangerous as
foreign code, and the account config decides where a token is sent. Both are
digest-pinned in the trust store, so a run refuses until you approve them, and
any later edit drops that approval:

```sh
apb connector approve <name>                    # approve the connector tree digest
apb connector approve <name> --account <acct>   # approve one account's non-secret fields
```

Approving an account shows the concrete field values so you see exactly where
secrets will be sent. `apb connector doctor` reports the trust status of every
connector and account (approved, changed since approval, or never approved)
alongside manifest, config, and env checks.

## Binding a connector to a node

An `agent_task` node gets an optional `connectors` list, the same two-form
pattern as `skills`:

```yaml
- id: triage
  type: agent_task
  profile: dev
  connectors:
    - mock-tracker                    # everything allowed
    - name: telegram
      accounts: [team-bot]            # optional allowlist; absent = all
      functions: [send_message]       # optional allowlist; absent = all
      max_calls: 50                   # optional per-node call budget (across retries)
    - name: github
      functions: read_only            # shorthand: every read_only function
```

`functions: read_only` grants exactly the functions the manifest marks
`read_only: true`, resolved at run start and frozen in the manifest grant, so a
later connector edit cannot widen a running grant. `max_calls` is a safety budget
against a looping agent, not a rate limiter; exceeding it returns a `permission`
error. The binding is part of the playbook YAML and is covered by the playbook
digest, so grants need no separate approval.

A binding whose `accounts` allowlist is empty has no account to select, so every
call fails at account selection. The connectors block in the agent's prompt
marks such a binding as not callable (no accounts configured, calls will fail
until an account is added, use the fallback path) while still listing its
functions, so the agent routes around it instead of retrying doomed calls.

## Receiving events (webhooks and the inbox)

Some services never answer a poll: they push. A connector that receives
declares a document-level `webhook:` block saying how a delivery is
authenticated, plus one or more `inbox` functions saying how a playbook reads
what arrived.

```yaml
webhook:
  challenge: meta_hub                     # optional, only dialect in v1
  verify_token: "{{secret.verify_token}}" # required when challenge is set
  signature:
    scheme: hmac_sha256_hex               # only scheme in v1
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: entry.0.id                 # optional dot path to the provider's own id
functions:
  - name: inbox_read
    description: Read pending inbound events without consuming them.
    read_only: true
    response_pick: [events, cursor]
    inbox: { op: read }
  - name: inbox_ack
    description: Advance the consumer cursor after processing.
    inbox: { op: ack }
```

The block and the inbox functions require each other: a manifest with one and
not the other does not load. `verify_token` and `signature.secret` are the
only fields outside `auth` besides the smtp and imap passwords where
`{{secret.*}}` is allowed, and both must name an account field declared
`secret: true`. Everything else in the block is a literal. The block is part
of the connector folder, so editing it changes the connector digest and drops
its recorded trust, which is what stops a shared config from quietly
redirecting or weakening verification.

The three ops: `read` returns `{events: [{seq, received_at, body}], cursor}`
without moving anything, `ack` takes `up_to_seq` and moves a named consumer's
cursor forward only, and `peek_depth` returns `{pending}`. Delivery is
at-least-once with an explicit acknowledgement, because a reader that stops
mid-thought must not lose what it was holding. A read takes an optional
`consumer` (default `default`) and `limit` (default 50, capped at 500);
different consumers keep independent cursors over the same events.

Received events are stored per connector and per account under
`<config-dir>/connector-inbox/<connector>/<account>/`, at mode 0600, outside
any run: messages arrive between runs and are not lost because nothing was
executing. A per-account cap of 50 MB or 30 days, whichever hits first, keeps
the store bounded; acknowledged events are dropped first, and only the size
cap ever drops an unacknowledged one.

An inbound delivery never starts a run. A playbook consumes the inbox by
polling it, an `inbox_read` call inside a loop or behind a wait node, and
acknowledging what it processed.

**Inbox content is untrusted.** It is written by whoever can reach the
callback URL, which is the first apb input not authored by the operator. The
node prompt says so to the agent, and the dashboard marks it when it renders
it, but the real protection is the grant: give an inbox-reading node the
narrowest `functions:` allowlist and a `max_calls` budget it can live with,
and never let the same node hold a write-capable grant it would not want a
stranger to steer.

Two validator rules cover the playbook side. **V42**: a node grants inbox
functions of a connector with no webhook block, so nothing could ever be
delivered. **V43**: a node grants them on an account that does not define the
fields the webhook block references, so a delivery could not be verified.

Accepted deliveries are capped per account at 600 appends in a rolling 60
second window; beyond the cap a delivery is dropped with a 200 (so the
provider stops retrying) and counted in a persisted per-account dropped
counter, visible in `apb connector doctor` and the dashboard's inbox panel.
Deliveries only ever resolve against accounts defined in the global
`connector-config`, never a project-scoped account, because the hook path
carries no workspace segment.

To run the listener, see docs/DEPLOYMENT.md. `apb connector doctor` prints
the exact callback URL per account, the pending depth, and whether the local
listener answers.

## The `apb connector` CLI

```text
apb connector list              installed connectors, trust and config status
apb connector show <name>       manifest summary and per-account status
apb connector call <name> <fn>  the agent-facing call channel (--account, --args, --dry-run)
apb connector approve <name>    approve a connector or (--account) an account digest
apb connector doctor            check every connector: manifest, config, env, trust, healthcheck
apb connector env [<name>]      unresolved env var names as ready-to-paste KEY= lines
apb connector init <name>       scaffold a new connector folder from a template
```

`apb connector call` needs a run context (`APB_RUN_DIR` and `APB_NODE_ID`, set by
the engine when a node executes a call); outside a run use `--dry-run` to render
a call without executing it, or the dashboard healthcheck to probe an account.
`--args -` reads the JSON arguments from stdin.

## Official connectors

Thirteen official connectors ship inside the `apb` binary and install with
`apb connector install <name>`: `github`, `telegram`, `smtp`, `sentry`,
`asana`, `imap`, `gitlab`, `youtrack`, `zulip`, `discord`, `slack`, `atrip`,
`twenty`. Installing
from the binary records trust for the
connector's tree digest in the same action, since the bytes are already
part of the binary you are running; `apb connector install --from-dir
<path>` (the development loop for this repository, `connectors/<name>/`)
keeps the normal approve flow.

### github

Account fields: `api_base` (`https://api.github.com`, or your GHES API
base) and `token` (secret). Prefer `token: "{{cmd:gh auth token}}"` when
`gh auth login` has already run; otherwise `{{env.GITHUB_TOKEN}}` with a
personal access token: classic PATs need `repo` (or `public_repo` for
public repositories); fine-grained PATs need repository access with
Actions write permission for `dispatch_workflow`. Healthcheck:
`get_rate_limit`.

### telegram

Account fields: `api_base` (`https://api.telegram.org`, overridable for
a self-hosted Bot API server) and `token` (secret) - the token
[@BotFather](https://t.me/BotFather) gives you for a new bot. The bot
must already be a member of a chat before `send_message` reaches it.
Healthcheck: `get_me`.

### smtp

Account fields: `host`, `port`, `from_email` (all required), and
`username`, `password` (secret), `from_name`, `use_tls` (all optional).
Set `use_tls` explicitly (there is no engine-level default for account
fields): `true` for STARTTLS on port 587, the common case. Healthcheck:
`verify` (connects, negotiates STARTTLS, authenticates, sends nothing).

### sentry

Account fields: `base_url` (`https://sentry.io`, or self-hosted),
`org` (the organization slug), and `token` (secret). Create the token at
Settings > Auth Tokens with scopes `project:read`, `event:read`,
`event:write` for issue functions and `project:releases` for
`create_release`/`create_deploy`. `list_issues` paginates through the
call result's `link` field: pass the cursor it returns back into the
next call's `cursor` argument. Healthcheck: `list_projects`.

### asana

Account fields: `api_base` (`https://app.asana.com/api/1.0`) and
`token` (secret). Workspace, project, section, and task gids are call
arguments, not account fields, so one account serves every workspace
the token can reach. Create the token as a personal access token: in
Asana, open your profile settings, go to Apps, then Developer apps, and
create a new personal access token; it acts as the user who created it,
with that user's full permissions, and there is no separate scope to
select. `list_workspaces`, `list_projects`, and `list_tasks` take an
optional `offset` argument: read the next page's offset from the call
result's `next_page.offset` field and pass it back on the following
call, omitting it on the first call. `search_tasks` is a fuzzy typeahead
match against task names, not a full-text search; use `list_tasks` with
a project filter when a complete, predictable result set matters more.
Healthcheck: `get_me`.

### imap

Account fields: `host`, `port`, `auth_method` (`password` or `xoauth2`),
`username` (all required), `password` (secret), and `use_tls` (optional,
default `true`). One connector serves any IMAP provider, since the
protocol is identical everywhere and only the connection settings
differ. `search_messages` and `get_message` open the folder read-only
with `EXAMINE` and fetch content with `BODY.PEEK[]`, so reading a
message never marks it seen; only `mark_read` and `mark_unread` change
anything on the server, and each is a separate, independently grantable
function. `use_tls` defaults to `true` when omitted; only set it to
`false` for a local plaintext test fixture, never for a real provider.
Gmail (`imap.gmail.com`, port `993`) needs 2-Step Verification enabled
before an app password can be generated; a Google Workspace account
with app passwords disabled by policy instead uses `auth_method:
xoauth2` with an access token sourced via `{{cmd:...}}`. Outlook and
Microsoft 365 (`outlook.office365.com`, port `993`) only accept
`auth_method: xoauth2`, sourced from an external token helper such as
`oama` or `mutt_oauth2` with the same `{{cmd:...}}` mechanism; `apb`
does not implement an OAuth consent flow itself. Yandex Mail
(`imap.yandex.com`, port `993`) needs IMAP access enabled in the Yandex
Mail web settings before an app password can be generated. iCloud
(`imap.mail.me.com`, port `993`) uses an app-specific password from the
Apple ID account page. No message deletion, no move between folders,
and no sending: this connector only reads and marks read/unread, and is
meant to be installed alongside `smtp` for a read-and-reply workflow.
Healthcheck: `verify` (connects, negotiates TLS, authenticates, without
opening or reading any mailbox).

### gitlab

Account fields: `api_base` (`https://gitlab.com/api/v4`, or your
self-hosted instance's API base ending in `/api/v4`) and `token`
(secret). Create a personal access token under Preferences > Access
tokens (user settings, not project or group tokens): scope `api`
covers the full connector surface; `read_api` is enough for the
read-only subset. Every project-scoped function takes a `project`
argument, either the numeric id or the `group/project` path with a
literal slash; the engine percent-encodes the substituted path value,
so never pre-encode the slash yourself. List functions follow GitLab
page pagination via optional `page` and `per_page` (max 100)
arguments; omit both for the first page. Label edits go through
`update_issue` (`labels` replaces the full set, `add_labels` and
`remove_labels` are comma-separated deltas). `trigger_pipeline`
starts CI on a branch or tag (optional `variables` is an array of
`{key, value}` objects); grant it only when a playbook is meant to
start pipelines, not just observe them. Healthcheck: `get_user`.

### youtrack

Account fields: `api_base` (`https://<org>.youtrack.cloud/api` on
YouTrack Cloud, `https://<host>/api` self-hosted; the `/api` suffix
is required) and `token` (secret). Create the token as a permanent
access token: open your profile, go to Account Security, then Access
Tokens; the token acts as the user who created it, with that user's
full permissions, and there is no separate scope to select. Read
functions bake a literal `fields=` projection matching their
`response_pick`, so a response carries exactly the projected fields.
`search_issues` uses YouTrack's native query syntax in its `query`
argument (`state: Fixed`, `project: DEMO`, `for: me #Unresolved`)
and pages with optional `$skip` and `$top` arguments. `create_issue`
takes the project database id (for example `0-0`), not the short
name; find it with `list_projects`. `apply_command` runs YouTrack's
command syntax, which can change almost anything on an issue (state,
tags, assignment, priority, custom fields), so restrict it in grant
allowlists accordingly. Healthcheck: `get_me`.

### zulip

Account fields: `api_base` (`https://<org>.zulipchat.com/api/v1` on
Zulip Cloud, `https://<host>/api/v1` self-hosted), `email`, and
`api_key` (secret). Find the API key in Zulip settings under Account
and privacy > Show API key for a personal account, or in the bot
panel for a bot; `email` is the matching account or bot address.
Every request authenticates with HTTP Basic auth (the email as the
username, the API key as the password), and write functions post
`application/x-www-form-urlencoded` bodies via the manifest-level
`body_form` field, matching Zulip's native write contract. A Zulip
conversation lives in a stream divided into topics:
`send_stream_message` posts to a given stream and topic, and
replying in a thread is the same call to the same topic, so there is
no separate reply function. `get_messages` takes optional `anchor`
(a numeric message id to page around; omit for newest), `num_before`,
and `narrow` (Zulip's filter, passed verbatim as a string whose
content is a JSON array of operator/operand objects). Healthcheck:
`get_me`.

### discord

Account fields: `api_base` (`https://discord.com/api/v10`) and
`token` (secret), a bot token from the Discord Developer Portal:
create an application, open its Bot tab, and reset or create the
token there. Invite the bot to each guild with an OAuth2 URL that
includes the bot scope plus the permissions the playbook needs: View
Channels for everything, Read Message History for `get_messages`,
Send Messages for `send_message` and `reply_to_message`. The
connector is REST-only, so the gateway-only message-content
privileged intent is not needed. Guild and channel ids are call
arguments, not account fields, so one bot account serves every guild
the bot has been invited to; threads are channels, so read or post
into a thread by passing the thread's channel id. `get_messages`
pages backward with optional `limit` (1-100) and `before` (a message
id) arguments. `send_message` and `reply_to_message` are separate
functions so a grant can allow thread replies without allowing new
top-level posts. Discord rate limits are aggressive and per-route:
avoid tight polling loops and bound calls with `max_calls` grants.
Healthcheck: `get_me`.

### slack

Account fields: `api_base` (`https://slack.com/api`) and `token`
(secret), a bot token (`xoxb-...`): create an app at
[api.slack.com/apps](https://api.slack.com/apps), add bot token scopes
under OAuth and Permissions, install the app to the workspace, and
copy the Bot User OAuth Token. Scopes: `channels:read` for
`list_channels`, `channels:history` for `get_messages` and
`get_thread` (plus the `groups:*` twins for private channels), and
`chat:write` for `send_message` and `reply_in_thread`. Scopes are
granular, so a missing one fails per function (as `missing_scope`),
not at the healthcheck; reinstall the app after adding a scope. The
bot must be invited to a channel (`/invite @your-app`) before reading
or posting there. Slack reports failures as HTTP 200 with
`"ok": false`; the manifest's `error_when` block reclassifies such a
response into a service error carrying Slack's `error` string, so
retries and fallbacks react to it. Channel ids are call arguments;
list and history functions page with a body-carried cursor (pass
`response_metadata.next_cursor` back as `cursor`). `send_message` and
`reply_in_thread` are separate functions so a grant can allow thread
replies without allowing new top-level posts. Healthcheck:
`auth_test` (a POST by Slack API convention, mutates nothing).

### atrip

Account fields: `client_id`, `client_secret` (secret), `base_url`, and
`search_base_url` (all required). The four fields split search traffic
from everything else: `search` goes to `search_base_url`, every other
function goes to `base_url`. Sandbox uses
`https://sandbox.atriptech.com` for both; production issues two separate
per-tenant URLs (and a separate credential pair) inside the ATRIP portal
under My Profile then Company Information, only after UAT approval and
once a customer manager switches the account to LIVE status - there is no
shared production hostname to hardcode. Most functions signal outcome
through a `status` field inside a 200-OK JSON body rather than through
HTTP status; several calls (`verify`, `pay`, `void`, `refund`) use their
own status range or field rather than a plain 0/nonzero pair, so each
function's description states what success means for that call. Eight
functions are effectful: `order` (creates a booking), `pay` (charges
money), `void` (cancels a booking and starts a refund), `refund` (moves
money back to the customer), `stop_ticket` (irreversibly halts ticket
issuance), `regenerate_order` and `pnr_claim` (each creates a new order),
and `post_booking_ancillary_order` (charges money for a baggage add-on).
`pay` carries no documented idempotency key, so it must never be retried
blindly: back off and check `query_order_details` before trying again.
The `healthcheck` is `query_void_orders`, a no-required-arguments read-only
status poll; a business-level error in its body still proves reachability
and credentials, and the fuller verification is a real read-only call
(for example `balance`).

### twenty

Account fields: `base_url` (the app origin, no path suffix - your own host
for a self-hosted instance, `https://api.twenty.com` for the cloud product)
and `api_key` (secret). Create the key in Twenty under Settings, API and
Webhooks (some versions label this section Playground instead), then Create
key; the value is shown only once, it is scoped to the workspace it was
created in, its permissions follow the role assigned to it under Settings,
Members, Roles (Assignment tab), and every key carries a mandatory expiry
with no "never expires" option. Covers typed CRUD for the five core objects
(companies, people, opportunities, notes, tasks), the noteTargets/taskTargets
join objects, duplicate detection for companies and people, generic record
access for every other object including custom ones (addressed by camelCase
plural REST name, discoverable with `list_objects`), and webhook management.
41 functions in total. Every record delete (the five typed deletes and the
generic `delete_record`) always sends the fixed query `soft_delete=true`
rather than Twenty's own hard-destroy default, so a delete here sets
`deletedAt` and can be undone with `restore_record`; `delete_webhook` is the
one exception, a hard delete with no restore. 23 functions are effectful:
the five typed create/update/delete triples, `create_note_target`,
`create_task_target`, the generic
`create_record`/`update_record`/`delete_record`/`restore_record`, and
`create_webhook`/`delete_webhook`. `depth` accepts only `0` or `1`; error
bodies carry a `messages` array rather than a single `message` string; a
missing auth header returns 403 while an invalid key returns 401; `limit`
defaults to 60 and caps at 200. Batch endpoints, `groupBy`, merge, attachment
binary upload, GraphQL, and API-key management are out of scope for this
connector's 0.1 surface. The `healthcheck` is `list_companies`, which
renders with zero arguments and succeeds against any key that can read
companies.

### Demo playbooks

`examples/playbooks/sentry-triage.yaml` and
`examples/playbooks/release-announce.yaml` exercise the github,
telegram, smtp, and sentry connectors end to end;
`examples/playbooks/inbox-triage.yaml` exercises imap and asana, and
`examples/playbooks/release-heartbeat.yaml` exercises gitlab,
youtrack, and slack (checking the latest pipeline, posting a summary,
and filing an issue on failure; `trigger_pipeline` is deliberately
absent from its grants). All four double as reference examples for
grant allowlists and `max_calls`. They validate in CI against fake
accounts and are not run against real services there; run them
manually once your own accounts are configured and approved.

### Coverage note

Write functions (issue creation, merges, releases, sends) are verified
by the offline contract tests in each connector's `tests.yaml`; the
env-gated live smoke tests exercise each connector's healthcheck plus
one read-only function against the real service. Write paths are not
called against real services by any automated test.
