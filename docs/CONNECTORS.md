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

## Upgrades

An installed official connector is a copy of the one embedded in the `apb` binary. When the dashboard starts (an upgrade restarts it), every installed copy that is exactly what an earlier `apb` installed, meaning its tree digest is trusted with origin `bundled`, is replaced by the embedded version, trust included. A copy with local changes is left alone: `apb connector list`, MCP `connectors_list` and the dashboard report it with `update_available` (the built-in version), and `apb connector install <name> --force` or the dashboard's "Update to the built-in version" replaces it.

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

`{{cmd:...}}` is allowed only in the global file. The project file is part of the
repository, and a command in it would run as you at the next call or healthcheck
of a connector you already trust, so a project account that sources any field
from a command makes the connector's config fail to load, naming the account and
field. Use `{{env.VAR}}` in a project file, or move the account to the global file.

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

Inside `apb` a resolved value is a sealed type: it prints as `***` in any log,
error or debug output, cannot be written to a state file, and is compared in
constant time. The value is read out only where it must leave the process: the
rendered auth of the outgoing request, the SMTP or IMAP login, and the webhook
signature check. Text that comes back from a call (a response body, an error
message) has every resolved value replaced with `[redacted:<VAR>]`, or
`[redacted:cmd:<field>]` for a command-sourced one: as resolved and in its
percent-encoded forms (a query or form value, a path segment). A Basic
credential echoed back as the base64 of `user:password` becomes
`[redacted:basic]`. Other transformations (a hash, a different encoding) are
not recognized.

With an `oauth2_refresh` connector (see [OAuth accounts](#oauth-accounts))
the credential the service sees is an access token minted from the
account's refresh token at call time. The access token lives in process
memory only: it is never written to a file, the journal or a prompt, and
it is redacted from everything a call returns like any other resolved
value.

A connector call from a node runs as a child of the node's agent, so it
inherits the agent's environment. Under a profile's `process_env: allowlist`
(PROFILES.md "Process environment") that environment holds only the allowed
names. `{{env.VAR}}` secrets are unaffected (the connector names are scrubbed
from every agent anyway, and the dotenv files are read by `apb` itself), and
`APB_CONFIG_DIR` and the run context always arrive. A `{{cmd:...}}` helper,
however, runs with that same environment: a password manager or keyring helper
that needs `PASSWORD_STORE_DIR`, `GNUPGHOME`, `OP_SESSION_*` or
`DBUS_SESSION_BUS_ADDRESS` gets them only when the profile's `allow` names them.
`apb doctor --run <id>` points at this when such a node fails.

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

## OAuth accounts

Four official connectors authenticate with OAuth2 refresh (`oauth2_refresh`
in the manifest): `gmail`, `google-calendar`, `google-drive` and `imap`.
Their auth block names the token endpoint and where each piece of the
refresh request comes from:

```yaml
auth:
  kind: oauth2_refresh
  token_url: "{{account.token_url}}"
  client_id_template: "{{account.client_id}}"
  client_secret_template: "{{secret.client_secret}}"
  refresh_token_template: "{{secret.refresh_token}}"
```

`client_id` is not a secret and is a plain account field;
`client_secret` and `refresh_token` are `secret: true` account fields, so
each is one `{{env.VAR}}` or `{{cmd:...}}` reference like any other
secret. At call time apb sends `grant_type=refresh_token` to the token
endpoint, keeps the access token it gets back in process memory until
shortly before it expires, and mints a fresh one after that. Nothing
about the token reaches a file, the journal or a prompt.

Scopes: every function of such a connector declares the OAuth scopes it
needs, and a call goes out only when the token grants any one of them.
The account's `granted_scopes` field (the scopes the refresh token was
consented for, when your account records them) decides first; without it
the `scope` the token endpoint answers with does; with neither there is
no check. A missing scope fails the call as `auth` before any request
reaches the service, with the command to consent again.

A token that has gone stale is refreshed before the request. A 401 from
the service refreshes the token once and retries once: a 401 means the
service did not act on the attempt, so the retry cannot repeat an
effect. The whole exchange stays one call: one `connector_call` event,
one `max_calls` unit, and the call's single `timeout_sec` covers both
requests.

Some providers hand out a new refresh token together with a new access
token and retire the old one soon after. apb stores no tokens, so it
cannot keep the new one for you: the result carries a `warnings` entry
when this happens, again on every later call that reuses the access
token minted then (the stored refresh token is still the old one), and
on an error that follows the mint as well; the fix is to keep the newest refresh token
yourself, usually by making the account's `refresh_token` field a
`{{cmd:...}}` helper that reads it from your password manager and is
rotated there. Until the store holds the newest one, the previous value
keeps working only as long as the provider keeps it.

The refresh token itself comes from a one-time consent flow a person
runs:

```text
apb connector oauth <connector> [--account <a>] [--scope <s>]...
```

apb binds a short listener on 127.0.0.1, prints the provider's
authorization URL (PKCE, the connector's default scopes unless `--scope`
says otherwise) and waits for the browser to call back. It exchanges the
code at the token endpoint and prints the refresh token once, alone on
one line, with the granted scopes and where to store it. Nothing is
written to disk. The command is refused from inside a run (only a person
grants access to an account) and without a terminal, for the same
reason.

Google's limits: while your project's OAuth consent screen is in Testing
status, the refresh tokens it issues expire after 7 days, and the
account stops working a week after each consent until you publish the
app (an unverified app shows a warning screen you can continue past when
it is your own). Google also keeps at most 100 live refresh tokens per
account and client pair and retires the oldest one without warning when
the limit is hit. On the `imap` connector the same machinery arrives as
`auth_method: oauth2`: its account's `oauth_*` fields form the same kind
of auth block, so a provider that refuses app passwords needs no external
token helper.

An MCP connector over HTTP uses the same `oauth2_refresh` block and the
same `apb connector oauth` consent; like any connector it needs a
`healthcheck` function for the consent to load it. The authorization
server is the one the definition and the account name (no discovery); an
author who needs `resource` on the authorization request puts it in
`authorize_params`. After storing the printed refresh token, a person
approves the account and its tools again when the secret reference
changed. Its token request ignores the calling process's proxy variables.
Every listing of such an account (`apb connector tools`, the dashboard's
"List tools", the approval's listing) and its healthcheck mint an access
token; a rotated refresh token is not stored, so listing can invalidate
the stored one, and each of them carries the rotation warning.

## Approving trust

A foreign `connector.yaml` (URL templates plus secrets) is as dangerous as
foreign code, and the account config decides where a token is sent. Both are
digest-pinned in the trust store, so a run refuses until you approve them, and
any later edit drops that approval:

```sh
apb connector approve <name>                    # approve the connector tree digest
apb connector approve <name> --account <acct>   # approve one account's non-secret fields
```

Approving a connector of any kind is refused inside a run step before
anything is read. Over HTTP a connector is approved only through a signed-in
dashboard session, and an MCP connector never over HTTP (see "MCP servers");
whoever holds a server key can sign in, so this guards against a script or a
run step posting by mistake, not against a key holder.

Approving an account shows the concrete field values so you see exactly where
secrets will be sent. A secret read from a command is shown with its command
line wherever an account is approved from, because approving the account lets
`apb` run that command: `apb connector show` and `approve --account` (`cmd`),
the dashboard's account row, the MCP run gate's `unapproved_connector_account`
refusal (`fields` and `cmd`), MCP `connectors_list` (`account_commands`), and
`apb connector doctor` (a `secret command` row per such field). `apb connector
doctor` reports the trust status of every connector and account (approved,
changed since approval, or never approved) alongside manifest, config, and env
checks.

For a connector with an `mcp:` block both approvals are a person's act at an
interactive terminal, and the account approval also pins the server's program
and tools; see [MCP servers](#mcp-servers).

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
      max_calls: 50                   # optional call budget, per executor attempt
      on_unknown: refuse              # optional: refuse (default) or resend, see "Sending once"
    - name: github
      functions: read_only            # shorthand: every read_only function
```

`functions: read_only` grants exactly the functions the manifest marks
`read_only: true`, resolved at run start and frozen in the manifest grant, so a
later connector edit cannot widen a running grant. A function whose call
cannot be taken back (a merge, a deletion, a payment, a publication) can be
flagged `irreversible: true` in the manifest: a node granted it after a
`human_review` gate refuses that gate's `auto_decide` (V73), and a granted
function that is merely not `read_only` makes V73 warn. `max_calls` is a safety budget
against a looping agent, not a rate limiter; exceeding it returns a `permission`
error.

The budget is counted **per executor attempt**, not per run and not per node
visit: a retry or a fallback step starts a fresh count, because an executor that
died mid-node is not the looping agent the budget exists to stop. Size the
number with that multiplier in mind. The worst case for a single visit to the
node is `attempts x max_calls`, and `attempts` means every spawn, not every
`max_retries`: each step of the fallback chain is worth `max_retries + 1`
attempts plus the infrastructure-retry backoff steps, two more by default. An
infrastructure retry (an executor that failed to start or died on transport)
does not advance the node retry counter, but it is a fresh spawn, so it does
get a fresh count. An interactive node in `reprompt` or `resume` mode
multiplies it further: each question and answer round re-invokes the agent, so
each round is a new attempt with a fresh count even when nothing failed. Under
`interaction: live` it does not, because one long-lived attempt spans every
round. For a node kind that runs no executor (a script node) there are no
attempts, so the count restarts on each visit to the node instead.

The binding is part of the playbook YAML and is covered by the playbook digest,
so grants need no separate approval.

A binding whose `accounts` allowlist is empty has no account to select, so every
call fails at account selection. The connectors block in the agent's prompt
marks such a binding as not callable (no accounts configured, calls will fail
until an account is added, use the fallback path) while still listing its
functions, so the agent routes around it instead of retrying doomed calls.

Some functions are marked `untrusted_output` in the manifest: their results
are text written by people outside the run (mail, events, files, search
results), which is a prompt-injection surface like any other outside text.
Mark such a binding `untrusted: true` so the untrusted-input checks (V179 to
V182) see that text for what it is; a binding that grants a marked function
without the mark draws validator warning V187. With the mark, the node's
prompt also says that such text is data, never instructions (see the
authoring guide, "Untrusted input").

### Baseline reads (the `@baseline` site)

A playbook's `baseline_check` may read a connector without any node binding
it (see "baseline_check" in `docs/HOWTO-authoring.md`):

```yaml
baseline_check:
  checks:
    - id: unowned
      connector: { name: twenty, account: default, function: list_opportunities,
                   args: { filter: "ownerId[is]:NULL", limit: 1 } }
      observe:
        - { id: count, type: number, path: totalCount, expect: { equals: 0 } }
```

Each such check becomes a grant for the reserved site `@baseline` (a node
may not use an id that starts with `@`, V68): exactly that function, on that account, frozen in
the run manifest like a node's grant and covered by the playbook digest.
The function must be `read_only` and must not be an inbox function: the
validator refuses anything else when it can see the installed connector
(V63), and the run start refuses it again when it resolves the grants. The
engine calls it itself, in process, before the first node that can act and
at the finish; each call is journaled as an ordinary `connector_call` with
`node_id: "@baseline"`. A call that fails, a response not read whole, or
one that does not satisfy the check's `complete_when` reads unknown, never
as an empty or zero result. The call is bounded by the smaller of the
check's `timeout_seconds` and the function's `timeout_sec`. Only the engine
calls under `@baseline`: `apb connector call` refuses an `APB_NODE_ID` that
starts with `@`.

### Notifications (the `@notify` site)

A run that notifies a person about its transitions (see `docs/NOTIFY.md`)
sends through the person's own target, not through a node's grant: one
function of one approved account, checked when the subscription is set and
again at every send against the connector and account digests pinned then.
Each notification is journaled as an ordinary `connector_call` with
`node_id: "@notify"` and `origin: "notify"`, and goes through the send ledger
under the site `@notify` with the label `notify:<transition>.<seq>`. The run
outcome leaves those sends out of the run's own sends; `apb intent list`
shows them. Like `@baseline`, the site is reserved for the engine: a node may
not use an id that starts with `@` (V68).

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
  event_type_path: entry.0.changes.0.field  # optional dot path naming the event's type
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

The block may also name the event's type, by a request header
(`event_type_header: X-GitHub-Event`) or by a dot path into the delivery body
(`event_type_path: entry.0.changes.0.field`); at most one of the two, both
literals. The ingest listener stores each verified delivery's type on its
inbox event, cleaned to 1 to 100 characters of letters, digits and
`. _ : / -`, and an event stored before the field existed reads as untyped.
A connector that names a type by header needs no field in the body, which is
how the github connector types its deliveries. A header type is not covered
by the signature when the provider signs the body only (GitHub's
`X-Hub-Signature-256` does): someone who holds a signed delivery could replay
its body under another type once the dedupe window passed, so a reader that
acts on a type alone should also check the body fields it relies on with
`where`.

The block and the inbox functions require each other: a manifest with one and
not the other does not load. `verify_token` and `signature.secret` are the
only fields outside `auth` besides the smtp and imap passwords where
`{{secret.*}}` is allowed, and both must name an account field declared
`secret: true`. Everything else in the block is a literal. The block is part
of the connector folder, so editing it changes the connector digest and drops
its recorded trust, which is what stops a shared config from quietly
redirecting or weakening verification for anything a run does. The inbound
listener is guarded by the signature itself rather than by that digest: it
reads the manifest directly, because hashing a connector folder per delivery
would put unbounded work on a route strangers can call.

The three ops: `read` returns
`{events: [{seq, received_at, type, body}], cursor, truncated}` without moving
anything (`type` present only on a typed event), `ack` takes `up_to_seq` and
moves a named consumer's cursor forward
only, and `peek_depth` returns `{pending}`. Delivery is at-least-once with an
explicit acknowledgement, because a reader that stops mid-thought must not
lose what it was holding. A read takes an optional `consumer` (default
`default`) and `limit` (default 50, capped at 500); different consumers keep
independent cursors over the same events.

A read may also filter. `types` keeps only events whose stored type is named
(exact, case-sensitive; an untyped event never passes a `types` filter), and
`where` is a list of `{path, equals}` conditions over the delivery body,
compared like a list's `where`: the value at the dot path equals `equals`
(a string, number, bool or null), a missing path never equals anything, and
an object or array never matches. With a filter, the events before the first
match that do not match are acknowledged for this consumer only (its cursor
moves past them; other consumers are untouched), a non-matching event after a
match the reader has not acknowledged yet stays pending, and the envelope
gains `skipped`, the count of pending events the read did not return because
they did not match. A filter never hides a gap: the gap is computed before
any cursor moved, and in a run it is in the run journal before the cursor
moves past it. When the journal cannot take it, the read moves no cursor and
still returns the gap, and the next read tries again; a person's live call
outside a run has no journal, so there the gap is only in the result.

Only a caller that may move the cursor anyway moves it through a filtered
read: a node whose grant also covers the connector's `ack` function, or a
person's live call outside a run. A node granted read-only functions only
(`functions: read_only`), every engine site (an item check, a baseline or
follow-up read, the reconcile of a send) and a read-only process read with no
acknowledgement: the skips are still counted in `skipped` and the gap is
still reported, and the next read sees the skipped events again. So
`inbox_read` stays a read-only function whatever its arguments.

A read is also capped at 1 MiB of events, the same ceiling an HTTP response
body has. Past it the newest events are left for the next read and
`truncated` is `true`: the oldest pending events are the ones a consumer has
to see to make progress, so it processes what it got, acks that `seq`, and
reads on.

Received events are stored per connector and per account under
`<config-dir>/connector-inbox/<connector>/<account>/`, at mode 0600, outside
any run: messages arrive between runs and are not lost because nothing was
executing. A per-account cap of 50 MB or 30 days, whichever hits first, keeps
the store bounded; acknowledged events are dropped first, and only the size
cap ever drops an unacknowledged one.

When the size cap drops events a consumer has not acknowledged yet, its next
read says so. The result carries `gap: {from, through}`, the inclusive range
of sequence numbers that were stored and lost before this consumer saw them,
at the top level next to `ok` (outside `body`, so a `response_pick`
projection cannot remove it; a read without a loss has no `gap` key). The run
journal records the loss once per distinct gap as an `inbox_gap` event, and
`run_report`, `run_status`, `apb runs <id>` and the dashboard run page list
it, so a run never reads as having processed its inbox completely when it did
not. Events a consumer acknowledged before they were dropped are not a loss
for that consumer. A consumer that starts reading late sees the events
dropped before its first read as a gap too, since it never saw them.

An inbound delivery never starts a run. A playbook consumes the inbox by
polling it, an `inbox_read` call inside a loop or behind a wait node, and
acknowledging what it processed.

**Inbox content is untrusted.** It is written by whoever can reach the
callback URL, which is the first apb input not authored by the operator. The
node prompt says so to the agent, and the dashboard marks it when it renders
it. Neither stops a message from steering the agent that reads it. Mark the
inbox-reading node's connector binding `untrusted: true` (see the authoring
guide, "Untrusted input"): the validator then refuses an agent that renders
that text and can send outward unless the author accepts the risk with
`accept_untrusted`, and every attempt gets a prompt section that says the
text is data, never instructions.

A grant limits only what the node can do through apb's connector calls: give an
inbox-reading node the narrowest `functions:` allowlist and a `max_calls` budget
it can live with, and never let the same node hold a write-capable grant it
would not want a stranger to steer. A grant does not confine the agent itself.
apb starts coding agents in their autonomous modes, in your working directory,
with your environment minus the connector secrets, so an agent that follows
injected instructions can still use its own tools: run commands, edit files,
call other CLIs you are logged in to. Treat a node that reads the inbox as a
node that runs untrusted input: keep its prompt's job narrow, keep its
profile on the default `minimal` environment, and run such playbooks where that
agent's reach is acceptable.

Two validator rules cover the playbook side. **V42**: a node grants inbox
functions of a connector with no webhook block, so nothing could ever be
delivered. **V43**: a node grants them on an account that does not define the
fields the webhook block references, so a delivery could not be verified.

Accepted deliveries are capped per account at 600 appends in a rolling 60
second window; beyond the cap a delivery is dropped with a 200 (so the
provider stops retrying) and counted in a persisted per-account dropped
counter, visible in `apb connector doctor` and the dashboard's inbox panel.
The panel's event list shows each typed event's `type`, the value a
`types` filter matches, as plain text.
Deliveries only ever resolve against accounts defined in the global
`connector-config`, never a project-scoped account, because the hook path
carries no workspace segment.

To run the listener, see docs/DEPLOYMENT.md. `apb connector doctor` prints
the exact callback URL per account, the pending depth, and whether the local
listener answers.

## MCP servers

A connector can map the tools of a server of the Model Context Protocol
instead of HTTP endpoints. apb starts the server and calls its tools itself,
through `apb connector call`, exactly as it makes an HTTP call: the grant,
`max_calls`, the trust approvals, secret scrubbing, the `connector_call`
journal entry and the send ledger all apply. The agent never gets the
server (below). A server is a program apb starts (`transport: stdio`) or
an HTTPS endpoint apb posts to (`transport: http`, Streamable HTTP of the
2026-07-28 revision; see "A server over HTTP" below).

### The manifest

A connector-level `mcp:` block says how to start the server, and each
function names the tool it calls with `mcp: { tool: <name> }`:

```yaml
name: code-search
version: 0.1.0
mcp:
  transport: stdio
  command: code-search-mcp            # an absolute path or a bare program name
  args: ["--root", "{{account.repo_dir}}"]
account_fields:
  - { name: repo_dir, required: true }
functions:
  - { name: search, description: Search the code, read_only: true, mcp: { tool: search } }
  - { name: read_file, description: Read one file, read_only: true, mcp: { tool: read_file } }
```

The `mcp:` block:

- `transport`: `stdio` (below) or `http` (see "A server over HTTP").
- `command`: a literal program, an absolute path or a bare name, with no
  placeholder. A bare name is looked up once, when a person approves an
  account (below), never at call time.
- `args`: argument templates; each may use `{{account.*}}` only. A
  `{{secret.*}}` here would land in the process list and is refused.
- `env`: variable name to template. Names are upper case letters, digits
  and `_` and must not start with `APB_`. Values may use `{{account.*}}`
  and `{{secret.*}}`; this is the only place a secret reaches the server.
  A name that changes what code a process loads is refused when the
  manifest loads (`mcp_env_loads_code`), since the approval of the
  program does not cover the code such a variable pulls in: every name
  starting `LD_` or `DYLD_`, and `NODE_OPTIONS`, `NODE_PATH`,
  `PYTHONPATH`, `PYTHONSTARTUP`, `PYTHONHOME`, `PYTHONUSERBASE`,
  `PYTHONINSPECT`, `RUBYOPT`, `RUBYLIB`, `GEM_HOME`, `GEM_PATH`,
  `PERL5OPT`, `PERL5LIB`, `PERLLIB`, `PERL5DB`, `BASH_ENV`, `ENV`,
  `ZDOTDIR`, `CLASSPATH`, `JAVA_TOOL_OPTIONS`, `_JAVA_OPTIONS`,
  `JDK_JAVA_OPTIONS`, `GCONV_PATH` and `LUA_PATH`.
- `protocol`: how the conversation opens. `auto` (the default) asks
  `server/discover` first and falls back to `initialize` for a server of an
  older revision; `modern` speaks 2026-07-28 only (an older server is an
  error); `legacy` sends `initialize` only.

`{{args.*}}` is refused anywhere in the block: the call's arguments go to
the tool, not to the program. No control character may appear in
`command`, an `args` entry or an `env` name or value.

A connector with an `mcp:` block holds only `mcp` and `mock` functions, and
an `mcp:` block exactly when it has an `mcp` function. It sets no
`webhook` or `error_when`, and a stdio one no `auth` ("connector `<c>` has
a stdio `mcp:` block and must not set `auth`"). An mcp function takes the fields every function
has (`name`, `description`, `args_schema`, `examples`, `read_only`,
`irreversible`, `idempotency`, `response_pick`, `timeout_sec`,
`untrusted_output`) and none of the HTTP fields (`query`, `headers`, `body`,
`body_form`, `text_args`, `scopes`, `paginate`, `mime`,
`decode_base64url`, `content_types`). The tool name is 1 to 128 of `A-Z`,
`a-z`, `0-9`, `_`, `-` and `.`, exactly as the server lists it. Two
functions may map the same tool (a narrower `args_schema` for one use) only
when both are `read_only`. A function that is not `read_only` must declare
`idempotency` (see "Sending once over MCP" below).

What a function may do is the author's word, never the server's:
`read_only`, `irreversible` and `idempotency` are declared per function as
for HTTP, and the server's annotations (`readOnlyHint`, `destructiveHint`,
`idempotentHint`) are shown to a person at approval and read by nothing
else. An `irreversible` mcp function reaches V73 and the unattended rules
like any other connector function.

The timeout rule of `protocol: auto`: a server that ignores
`server/discover` is asked `initialize` only after a fixed 10 s probe, and
the call's `timeout_sec` bounds the probe too. So under `auto` every mcp
function needs a `timeout_sec` of at least 15, else the manifest is refused
with `mcp_timeout_too_small`. Set `protocol: modern` or `legacy` when the
server's revision is known; both skip the probe and take any timeout.

A second example, outside code: a local notes server that adds a note sent
once by a marker and deletes a note only behind a person's review.

```yaml
name: notes-mcp
version: 0.1.0
mcp:
  transport: stdio
  command: /opt/notes-mcp/bin/notes-mcp
  args: ["--folder", "{{account.folder}}"]
account_fields:
  - { name: folder, required: true }
identity: { function: whoami, path: name }
functions:
  - { name: whoami, description: The user the server writes as, read_only: true, mcp: { tool: whoami } }
  - name: list_notes
    description: Notes changed since a time
    read_only: true
    mcp: { tool: list_notes }
    args_schema: { type: object, properties: { since: { type: string } } }
  - name: add_note
    description: Add a note
    mcp: { tool: add_note }
    args_schema:
      type: object
      properties: { title: { type: string }, body: { type: string } }
      required: [title, body]
    idempotency:
      mode: marker
      arg: body
      format: html_comment
      reconcile: { function: list_notes, since_arg: since, items: notes, text: body, author: author, item_ref: id }
  - name: delete_note
    description: Delete one note
    irreversible: true
    mcp: { tool: delete_note }
    args_schema: { type: object, properties: { id: { type: string } }, required: [id] }
    idempotency: { mode: none }
```

A retry of `add_note` whose answer was lost finds the note by its marker
instead of adding a second one. Grant `delete_note` only to a node that
runs after a `human_review` gate; V73 refuses that gate's `auto_decide`.

### A server over HTTP

A hosted MCP server is reached over Streamable HTTP: one POST per
message to one endpoint, in the stateless 2026-07-28 revision only. Its
authorization is the connector-level `auth:` block every HTTP connector
uses, of kind `header` or `oauth2_refresh`:

```yaml
name: crm-mcp
version: 0.1.0
healthcheck: whoami
mcp:
  transport: http
  url: "https://{{account.server_host}}/mcp"
  protocol: modern
auth:
  kind: header
  header: X-Api-Key
  value_template: "{{secret.token}}"
account_fields:
  - { name: server_host, required: true }
  - { name: token, required: true, secret: true }
identity: { function: whoami, path: id }
functions:
  - { name: whoami, description: The user the server acts as, read_only: true, mcp: { tool: whoami } }
  - name: add_note
    description: Add a note to a contact
    mcp: { tool: add_note }
    args_schema: { type: object, properties: { contact: { type: string }, body: { type: string } }, required: [contact, body] }
    idempotency: { mode: none }
```

The rules of an http block:

- `url` is required and `command`, `args` and `env` are refused. Its
  literal text before the first placeholder starts with `https://`, so
  the scheme is the definition's and no account field can choose it; the
  host, port and path may hold `{{account.*}}` placeholders only. A
  plaintext `http://` endpoint is refused on every host, the loopback and
  `localhost` included: the endpoint receives a secret on every call, and
  its peer is authenticated by web PKI on every call (certificate
  verification is always on). `url: "{{account.server_url}}"` is refused
  for that reason.
- `protocol: modern` is required.
- `auth` takes `header` or `oauth2_refresh` only. `query` and `path`
  would put the credential in the URL, which the MCP authorization rules
  forbid; `basic` is not supported yet. A `header` auth may not name a
  header the transport sets itself (`mcp-protocol-version`, `mcp-method`,
  `mcp-name`, `content-type`, `accept`, `host`, `content-length`,
  `transfer-encoding`, `mcp-session-id`, `last-event-id`, and `authorization` beside a minted token): V303.

On every call, listing and approval, apb renders the endpoint (and, for
`oauth2_refresh`, the token URL) from the account's non-secret fields and
checks it before any secret is resolved: https only, no userinfo, no query
and no fragment, no control character or whitespace in what an account
field put there. A refused endpoint or token URL runs no `{{cmd:...}}`
helper and sends nothing. The request carries no proxy the calling process
names and follows no redirect; the token request of an MCP connector
ignores the caller's proxy variables too. Headers come only from the
definition and the account, plus the protocol headers the transport sets.

How a pin binds an HTTP server, in plain words: the approval records the
connector definition, the account, the rendered endpoint and token URL
(as one endpoint digest) and every tool. A call compares the same four
before anything leaves; an account that now names another host, or a
record whose endpoint differs, is refused `mcp_server_changed`. The pin
covers the URL, not an address: a name that resolves somewhere else gets
nothing unless that peer holds the name's certificate.

With `oauth2_refresh` the access token is minted after the binding passed
and sent as `Authorization: Bearer`. A 401 while connecting or listing
renews the token once and reconnects once with the new token (the old one
is never sent again); both tokens are scrubbed from everything apb keeps.
A 401 or 403 on the `tools/call` itself is never retried: for a write
whether the tool acted is not known, so it settles `unknown`.

Every answer, a JSON body or each event of an event stream, is read whole
by the same strict reader as a stdio line before anything of it is used:
one JSON-RPC object, no member named twice, no byte order mark, nothing
around it. A body or event that is not a frame ends the session; over
HTTP no line is ever dropped as log text. An event stream is read up to
the event that answers the request and the connection is then closed. A
server that answers with a session id, or whose tools carry `x-mcp-header`
annotations, is refused (header annotations are not supported over HTTP
yet). Every failure of a write after the call was handed over (a status,
a broken stream, a refused frame, the deadline) settles `unknown`; only a
request apb never started is `not_sent`.

Not supported yet: servers on an older revision over HTTP (sessions),
authorization discovery and the `resource` parameter on the token request,
dynamic client registration, a proxy or an extra certificate authority or
certificate pinning, the `basic`, `query` and `path` auth kinds, header
annotations, and reusing a connection across calls.

### One server per call

Every call starts its own server process from the program the approval
pinned, opens the conversation, lists the tools, checks the pins, sends
one `tools/call` and closes the server again. Nothing is kept between
calls, so a server that crashed costs one call, never the run. The
working directory is a fresh, empty, private directory (mode 0700) that
apb makes under the temporary directory for this one start and removes,
with everything in it, once the server has ended; it is never the project
root. The server's stderr is not read. One deadline, the function's
`timeout_sec`, covers the start, the handshake, the listing and the call.

The server runs as you, unsandboxed: it can read and change what you can,
including apb's own files and the secrets of other connectors. The
private working directory only keeps a relative path from landing in
your project. Approve only a server you would run yourself. Running MCP
servers under apb's isolation sandbox is future work.

apb starts the server as the leader of its own process group. To end it,
apb closes the server's stdin and waits up to 3 s for it to exit; then,
while the server is still unreaped (so its group id cannot have been
reused), it kills the server's whole process group, reaps the server
within 2 s and looks up to 1 s for the group to be empty. So a worker the
server forked in its group ends with it. A process that left the group
(its own session or group) is not chased; when the group is not seen
empty, the call carries the warning "the MCP server was reaped, but its
process group could not be shown empty: a process it started may still
run and act". apb signals no other process. When the calling process
itself is killed, apb's close does not run. On Linux the server is then
killed with it (it is started with a parent-death signal), though a
worker it forked is not. On other platforms the server ends only when it
exits on the end of its stdin, so a killed call whose server ignores the
end of its input leaves that server running. A cleanup of such survivors
is future work. Nothing in the journal or the send ledger depends on the
server ending.

A single message from the server is limited to a little over 7 MiB, one
session to four such messages in all, and one listing to 20 pages, 500
tools and 2 MiB; past a limit the call fails with
`mcp_message_too_large`.

### The server's environment

The server does not inherit the environment of whoever calls it. apb
clears it and builds a new one from a fixed source:

| Variable | Value | Source |
|---|---|---|
| `HOME` | the user's home directory | the system account database of the user apb runs as |
| `USER`, `LOGNAME` | the user's name | the same account entry |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME` | `<home>/.config`, `<home>/.local/share`, `<home>/.local/state`, `<home>/.cache` | the home above and the XDG defaults, never the caller's values |
| `PATH` | `/usr/local/bin:/usr/bin:/bin`, then `/opt/homebrew/bin` on macOS, then `<program dir>` | fixed: the system directories first, so a name the server looks up is never found first in the program's own directory; on macOS the Homebrew directory, so a `#!/usr/bin/env node` server finds its interpreter; the pinned program's own directory last |
| each name of `mcp.env` | its rendered template | the approved definition and the approved account; a declared name overrides a row above |
| everything else of the calling process (`PATH`, `HOME`, `XDG_*`, `LANG`, `TZ`, `TMPDIR`, the proxy variables in either case, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`, `APB_*`, an agent's token) | dropped | none |

So a caller cannot point the pinned program at another configuration or
route its traffic through a proxy of its choosing. A server that needs a
proxy, a certificate bundle, a locale or a temp dir gets it only by name in
`mcp.env`, with its value from the definition or an account field:

```yaml
mcp:
  transport: stdio
  command: /opt/tracker-mcp/bin/tracker-mcp
  env:
    HTTPS_PROXY: "{{account.proxy}}"
    SSL_CERT_FILE: "{{account.ca_bundle}}"
    TRACKER_TOKEN: "{{secret.token}}"
```

Where the account database gives no home (no entry for the user, or a
system that is not unix), the call is refused before anything starts.

### Approving the program and the tools

The connector tree and each account are approved by digest as for every
connector, and an MCP connector adds two more facts: the program that
receives the account's secrets, and the tools it may be asked to run. For a
connector with an `mcp:` block, every approval is a person's act at an
interactive terminal:

```sh
apb connector approve code-search                  # the tree: one typed confirmation
apb connector approve code-search --account work   # the program, then the tools: two typed confirmations
```

Without `--account`, apb prints the tree digest and the `mcp:` block as
written (transport, command, each `args` template, every `env` name with its
template, `protocol`; placeholders stay unrendered, so no secret or account
value is read) and asks you to type the connector name. It starts nothing
and touches no pin. A tree approval alone never makes an old pin usable:
the output lists every account whose tools need a new approval.

With `--account`, in this order:

1. The first confirmation, the program. apb resolves `command` once, a
   bare name through your own `PATH`, to an absolute canonical path, and
   hashes the file. It prints the command, the path, the file's digest, a
   warning when the command is an interpreter or a launcher (below), the
   line "this program runs as you, unsandboxed: it can read and change
   what you can, including apb's own files and the secrets of other
   connectors", every argument rendered
   with the account's fields, every `env` override rendered the same way
   (a part that comes from a secret shows as `[secret <field> from env
   <VAR>]` or `[secret <field> from cmd]`, never its value), every
   non-secret field of the account, and for each secret read from a command
   the command line that will run. Nothing has been resolved or started
   yet, so an account field that sets a proxy, a `PATH` or a certificate
   file is seen before it takes effect. You type the connector name to go
   on; anything else ends the command with nothing resolved, started or
   written.
2. apb reloads the connector and the account and compares both digests
   with the ones you confirmed, then opens the program file once and
   compares its digest with the one you confirmed. A change refuses the
   approval (`mcp_approve_definition_changed`,
   `mcp_approve_account_changed`, `mcp_server_changed`) with nothing
   resolved or started.
3. Only then are the account's secrets resolved and the file opened in
   step 2 started (below, "The program that runs"), under a fixed 30 s,
   and every tool listed.
4. The second confirmation, the tools. apb prints the era of the
   conversation, the program again, and one line per function: the tool,
   its status against the existing pin (`pinned`, `unpinned`, `changed` or
   `missing`), the full digest of its definition and its description;
   for a changed tool the pinned and the new description side by side and
   the names of the members that changed; then each tool's title, input
   schema and annotations (shown, not used). Server tools no function maps
   are listed by name; they can never be called. Each field of server
   text is cut to its own length, and all of it together to 20 000
   characters, with one line saying how many lines were left out. Right
   above the question apb prints again the program digest, the count of
   tools in each status and the pin digest over the whole binding, so no
   server text can scroll them away. When the server started for this
   listing could not be shown ended, a warning above them says that a
   process started for the listing may still be running with the
   account's secrets. You type the connector name again.
5. apb records the account approval and then one pin record for the
   connector and account.

Every text in both confirmations, from the definition, the account, the
file system or the server, is sanitized for the terminal, and long values
are cut with a marker. The protocol version a server reports is kept only
when it is one apb supports; any other ends the conversation with
`mcp_protocol_unsupported`, which does not repeat what the server sent. A
tool the server does not list is reported as a
warning and not pinned; its function is refused (`mcp_tool_missing`) until
the server lists it and you approve again.

There is no other way to approve an MCP connector. Inside a run step the
approval is refused first, as for every connector kind. Without an
interactive terminal, over MCP and from the dashboard, a signed-in session
included, it is refused `mcp_approve_needs_a_person`. Neither refusal prints
a digest, program or tool, so a caller cannot learn a value to replay. No
flag names an expected digest. Approval without a terminal (for example in
provisioning scripts) is future work. Connectors of other kinds keep their
approval as described in "Approving trust".

A tool's digest covers its whole definition as the server sent it, read
off the wire (name, title, description, input and output schema,
annotations and any member a later protocol revision adds), except `icons`
and `_meta`, which a server may change per request. A pin record binds,
as one unit, the approved tree digest, the approved account digest, the
program's path and file digest, and the digest of every mapped tool. So a
changed definition (its `command`, `args`, `env` or any other field), a
project account that replaces a global one of the same name, a program
file that changed, or a tool whose definition changed each refuse every
call until a person approves the account again. Each approval of an
account replaces its record whole, and the last approval of an
account name on this machine is the one that counts.

A server could echo one of the account's secrets back into a tool's
definition. A listing whose raw tool definitions hold any secret of the
account, in a name, a description, an annotation or any member, is refused
`mcp_tools_reflect_a_secret` before anything is hashed, shown or stored;
the refusal names no tool. On top of that, every server text the listing
shows or stores passes the account's redactions.

### Every call checks the pins

Before a call starts anything or resolves any secret, apb checks the pin
store, the record for the connector and account, the run's tree and
account digests against it, the program file at the pinned path and its
digest, and that the function's tool is pinned. Only then does it resolve
the secrets (a `{{cmd:...}}` helper runs only for a call whose pins
passed), start exactly the program file it checked (never a name looked
up in the caller's `PATH`; see "The program that runs" above), list the tools and compare the live digest of the tool
with its pin. This happens on every call, not once per run: a server that
changes a description between two calls of one attempt is refused on the
second. All of it comes before the send intent is recorded and before
anything is sent.

The run gate also refuses a start whose granted mcp functions have no pin
for their connector, account and tree (`mcp_tools_unpinned`, with a manual
next action); it starts no server and hashes no file. `acknowledge_untrusted`
does not bypass it. A doubled eval run is no exception (below).

The pins live in `<config-dir>/mcp-tools.json`, machine-wide like
`trust.json`, one record per connector account. The file is read without
following a link below the config dir, written atomically under a lock
(mode 0600), and refused rather than read as empty when it does not parse,
so an edit never silently resets it. A record that would push the file
past its 4 MiB read cap is refused (`mcp_pin_store_too_large`). A call made
from a run step reads pins only from the default config dir: with the
config dir redirected it is refused `mcp_config_dir_redirected`.

What the pin store guards against: a server or a program file that
changes after a person approved it; a caller that picks the program or its
environment; and an agent or a mistake reaching pins through apb's own
surfaces (no MCP tool and no HTTP route writes a pin, and the approval
takes a person at a terminal). "Approved at a terminal" has two limits.
The pin store is an unsigned file that any process of the same user can
write, exactly as `trust.json`, so an approval is no protection against a
run step of the same user that has a shell; for an agent with a shell,
isolation (`docs/ISOLATION.md`) is the answer. And the terminal check is
`isatty` plus `/dev/tty`: an agent with a pseudo-terminal that clears
`APB_RUN_ID` passes it, as it passes every typed confirmation of apb.

Pins go with the connector and its approval. Uninstalling a connector
(the dashboard's uninstall of an installed connector) removes its pin
records (a reinstall asks a person to approve
its tools again at a terminal; the trust record stays, as before), and
revoking the approval of a connector or of a connector account (`apb
trust revoke`, MCP `trust_revoke`, the dashboard) removes the pin records
of that connector or that account. Both are best effort: a pin store that
does not read keeps them, and a kept pin still binds the tree digest. An
account has no removal command; the pin of an account you deleted binds
an account digest nothing matches, and the next approval of an account of
that name replaces it.

The program that runs is the file that was hashed. apb opens the program
file once, without following a link and without waiting on a FIFO,
accepts only a regular file, reads its digest from that open file and
keeps it open until the start:

- On Linux the server is started from that open file
  (`/proc/self/fd/<n>`), so a file renamed over the path, a new file or a
  swapped link after the check changes nothing: the file that was hashed
  runs. For a script with an interpreter line, the interpreter reads the
  hashed file the same way, and the script's own name (`$0`,
  `sys.argv[0]`, `__file__`) is `/proc/self/fd/<n>`, a link that resolves
  to the approved file (or to `<path> (deleted)` when it was replaced). A
  script that finds its files next to its own name resolves that link
  first: node does it itself, and a shell script can use `readlink
  /proc/$$/fd/<n>`.
- On other platforms (macOS) apb opens the path again without following
  a link and hashes it again right before the start; a different digest
  refuses the start (`mcp_server_changed`) and nothing starts. The short
  window between that second hash and the start remains there.
- On no platform is a file rewritten in place (the same file, changed
  between the check and the start) covered; only a replacement is.

The program pin covers the program file only, not the libraries it
loads.

Interpreters and launchers. When `command` is a launcher (`npx`, `pnpx`,
`bunx`, `uvx`, `pipx`, `uv`, `npm`, `pnpm`, `yarn`, `docker`, `podman`,
`env`) or an interpreter (`node`, `nodejs`, `deno`, `bun`, `python`,
`python3`, `sh`, `bash`, `zsh`, `dash`, `ksh`, `fish`, `ruby`, `perl`,
`php`, `java`, `lua`, `pwsh`, and a versioned name such as `python3.12`),
the approval covers that program file only, not the script or package it
runs. The script, the package or image it fetches or picks at each start,
and the modules it loads are not covered and can change with nothing
pinned changing; the tool pins still refuse a changed tool list, but a
server that changes what it does and not its tools is not caught. Each
confirmation prints "warning: <name> is an interpreter or a launcher;
this approval covers the <name> file only, not the script or package it
runs", and every binding of such a connector draws validator warning
V301. Install the server and name its program in `command` instead.

Refusals name what differs and never a digest or a command to run; the
message starts with the code, while `error.code` stays the coarse class:

| Code | Class | When |
|---|---|---|
| `mcp_server_unpinned` | `permission` | no pin record for the connector and account |
| `mcp_server_changed` | `permission` | the definition, the account, or the program file changed, or the file is gone, a link or not a regular file |
| `mcp_tool_unpinned` | `permission` | the record has no pin for the function's tool |
| `mcp_tool_changed` | `permission` | the tool's definition differs from its pin |
| `mcp_tool_missing` | `config` | the server no longer lists the mapped tool |
| `mcp_tools_reflect_a_secret` | `permission` | a tool definition holds a secret of the account |
| `mcp_config_dir_redirected` | `permission` | a run step's config dir is redirected |
| `mcp_message_too_large` | `service` | a message or a listing over its limit (before the call is sent) |
| `mcp_live_call_unsupported` | `config` | a live call outside a run (below) |
| `mcp_tools_unpinned` | run gate | a run start whose granted mcp functions are not pinned; when the pin store does not read, the refusal carries `store_unreadable` with its path and the next action is to move it aside and approve the tools again at a terminal |
| `mcp_approve_needs_a_person` | approve, exit 2 (dashboard 403) | an approval anywhere but an interactive terminal |
| `mcp_approve_definition_changed`, `mcp_approve_account_changed` | approve, exit 2 | the connector or the account changed after the program confirmation |
| `mcp_env_loads_code` | install and load | an `mcp.env` name that changes what code a process loads |

### Listing an approved server's tools

```text
apb connector tools <connector> [--account <a>] [--json]
```

starts the approved server with the account's secrets, in the
healthcheck's order (the pins are checked before any secret), lists its
tools and prints each mapped tool with its status: `pinned` (approved),
`unpinned` (not approved), `changed` (changed since approval) or `missing`
(gone from the server). It pins nothing, writes nothing and shows no
digest. Exit 0 when every row is approved, 3 when any is not, 2 on a
refusal. It is refused inside a run step. A refusal `mcp_tool_changed`
names this command.

`apb connector doctor` adds one row per account of an MCP connector,
`ok` when every tool is approved for the installed definition and the
account, else `warn` naming the tools; it starts no server. MCP
`connectors_list` names each mcp function's tool and the accounts that
approved it. The dashboard's connector page has an "MCP server" section:
the transport, the command or URL template, the auth kind, one row per
project and account with each function's approval, and a "List tools"
button that lists the live rows in that row's project. It approves
nothing and shows no digest. The listing and the healthcheck of an MCP
connector start the server with the account's secrets, so the dashboard
takes them only from a signed-in session: a dashboard without keys and a
bearer key are refused (403, `mcp_needs_a_person`), the rule a connector
approval follows. A dashboard without keys offers no "List tools" button
and says to sign in or run `apb connector tools` at a terminal.

Every listing of an `oauth2_refresh` account mints an access token in a
fresh process. With a provider that rotates refresh tokens the new one is
not stored (the listing carries the rotation warning), so listing can
invalidate the stored refresh token; keep the newest one with a
`{{cmd:...}}` helper.

### What an MCP call returns

An MCP call answers `{ "ok": true, "body": ... }` with no HTTP `status`.
The body is the tool's structured content when it sent one (any JSON value,
`null` included); otherwise its text blocks joined with a newline, parsed
as JSON when they parse and `{ "text": "<joined text>" }` when not (an
answer with no text block is `{ "text": "" }`). Image, audio and resource
blocks are left out and counted in `dropped_content`. The account's
secrets are redacted from the body (in every string, every member name
and every number), then a `response_pick` projection applies (`picked:
true`). Every text the server provides in an error (a tool's error text,
a JSON-RPC error message, a failure message) is redacted whole and then
cut to 500 characters with a marker; a tool's error text is kept as
text, never parsed. A body over the 1 MiB cap fails a `read_only`
function `incomplete_response`; a write succeeds with a `null` body and
`response_complete: false`, as an HTTP write with a cut body does. The
call's arguments must be a JSON object; a scalar, an array or `null` is
refused `invalid_args` before anything starts.

The journaled `connector_call` carries the url `mcp+stdio://<connector>/<tool>`
and `tool_digest`, the digest of the tool definition the call was checked
against.

How each answer counts (what decides the send ledger is whether the
`tools/call` request was handed to the server):

- A failure before the request is handed over (the start, the handshake,
  the listing, a pin refusal) records no send intent, journals no call and
  spends no `max_calls` unit. The send intent is recorded only after the
  server started and its pins were checked. A failure after the intent was
  recorded and before the hand-over settles the send `not_sent`, and so
  does a call no byte of which reached the server (the server already
  gone): "the call never reached the MCP server; nothing was sent". apb
  says so only after it stopped every further write to the server, so a
  call that waited in a queue when its deadline passed can never go out
  later; a call apb cannot prove unwritten settles `unknown`. apb
  reads every message before it writes it to the server and refuses one
  it cannot read rather than send it unread; the arguments of an MCP
  call may be nested at most 64 levels and be at most 1 MiB as JSON,
  else the call is refused `invalid_args` before the server starts.
  Every line the server sends that starts with `{` must be exactly one
  JSON-RPC 2.0 message (no member named twice, nothing around it but one
  carriage return); apb ends the session on any other such line, and on a
  line that starts with a byte order mark (`mcp_message_invalid`), and
  passes nothing of it on. A line that does not start with `{` (log text,
  a blank line) is dropped unread; the call's warnings say how many, never
  what they said, and more than 1000 of them end the session.
- A complete answer that is not an error is a success; a write settles
  `confirmed`.
- An answer with `isError: true`, a task handle, an `input_required` answer
  (the server wants input apb cannot give), and a JSON-RPC error of any code
  to a write are the outcome `tool_error`: the tool may have acted before it
  failed or asked, so a write settles `unknown`. A server error code is the
  server's own word, received after the request went out, so no code counts
  as "nothing happened".
- A JSON-RPC error to a `read_only` function is a failed read: `service`
  for -32700 and -32600, `config` for -32601, `invalid_args` for -32602,
  and `tool_error` for any other code.
- No answer: a timeout is `timeout`, a broken pipe, a server that exited or
  an answer over the message limit is `network`; a write settles `unknown`.

A call that failed closes its server before its result is journaled and
its send settled, so a warning from the close (a process group not shown
empty) is at the front of the settlement's detail, where `apb intent list`
and `apb intent resolve` show it.

### Sending once over MCP

The protocol has no idempotency key, so a write declares `marker`, `none`
or `repeatable`, and `key` is refused. A `marker` send needs the
connector's `identity` and a `reconcile` function that is a read-only mcp
function of the same connector; the marker rides in the tool's arguments,
and a repeat of an unknown send lists recent items through the reconcile
function, whose `items`, `text`, `author` and `item_ref` paths read the
body shape above. A `none` send that ends unknown waits for a person (or
the binding's `on_unknown: resend`). See [Sending once](#sending-once).

### Where an MCP connector cannot be used

- An isolated node: V113 refuses any connector on a node with `isolation`,
  and its message names MCP connectors, whose server would need the
  credentials the sandbox withholds. A broker is future work.
- An engine site: V302 (error) refuses an MCP connector at the baseline
  check, the follow-up check and a list item check (`<node>#check`). Those
  reads run from the drive or a detached waker, for which this version
  defines no server environment, working directory or stop path.
- A notification target: an mcp function never carries notifications, so
  `apb notify` refuses it.
- Outside a run. A live call of an mcp function outside a run (the
  dashboard's playground, a pinned notice) is refused
  `mcp_live_call_unsupported` before any secret is resolved; a `--dry-run`
  still answers, with the pinned program (or the `command` when nothing is
  pinned), the rendered arguments, the `env` names, the tool and the
  prepared arguments, starting nothing; for an http server it answers the
  checked endpoint and the auth kind instead of the program, sending
  nothing. The dashboard's healthcheck of an
  MCP connector checks the pins without a tool call: it starts the pinned
  server, lists its tools, compares every mcp function's tool with its pin
  and closes the server, journaling and writing nothing (`{ "ok": true,
  "mcp": { "era": ..., "tools_checked": n } }`). Like any connector, it
  needs a `healthcheck` function, whose `timeout_sec` is its deadline.

A binding that grants an MCP connector without a `functions` list grants
every tool the manifest maps, destructive ones included, and draws V300.
Every mcp function counts as `untrusted_output`: its server writes every
byte of the answer, so bind the connector with `untrusted: true` (V187).

### The agent never gets the server

The agent sees the functions as the manifest describes them: description,
`args_schema` and examples are the author's, never the server's. It calls
them through `apb connector call` like any other function, and apb adds no
server to any agent's MCP configuration. In a profile's default `minimal`
environment claude runs with `--strict-mcp-config`, so no MCP server but
apb's own reaches it. Under `environment: full` claude loads the person's
own MCP configuration, as before; apb adds nothing there either, but cannot
say which servers the person configured.

### Eval doubles

An eval case doubles an mcp function exactly as an HTTP one, by connector
and function name (see `docs/EVALS.md`); the double answers before any
secret is resolved and no server starts. Four points differ:

1. The double's `answer` is `{ status, body }` as for any function, but the
   agent reads `body` where a live call returns the body shape above, so
   write `body` in that shape (the structured content, or
   `{ "text": ... }`). `status` means nothing to the server.
2. The journaled call of a doubled function has the url `eval-double` and
   no `tool_digest`: no tool was checked.
3. The run gate's pin check applies to an eval run as to any run: a
   doubled eval needs its tools approved once, against the real server, as
   it needs its connector and account approved.
4. An eval case that runs under the config dir it was handed calls no MCP
   server live: a live mcp call from a run step whose config dir is
   redirected is refused `mcp_config_dir_redirected`, even with the pins
   in that dir. Give such a case's MCP calls a double (`connectors:`).

### Older apb

An apb older than this change cannot load a connector manifest with an
`mcp:` block: it refuses to start such a run, and in a run it resumes,
every call of an mcp function fails before anything starts.

## What a call returns

A call answers with one JSON document. On success it carries `ok: true`, the
HTTP `status`, and the `body` (parsed when the service sent JSON, a string
otherwise). A body over the 1 MiB read cap fails a `read_only` function as
`incomplete_response` (see below); on any other function the body is cut at
the cap, `truncated` is `true`, and the result says the response was not
complete. When the function declares a `response_pick` projection, `picked`
is `true` and `body` holds only the projected fields; `--full` skips the
projection and returns the raw body. A paging `Link` header from the service
surfaces verbatim as `link`. A paginated read (see
[Paging](#paging)) adds `pages` (how many pages it fetched) and `more`
(whether the listing went on) at the top level. A call whose token endpoint
returned a new refresh token adds `warnings` (an array of one-line strings)
at the top level, on success and on failure alike; see
[OAuth accounts](#oauth-accounts).

On failure it carries `ok: false` and an `error` object with a `code`, a
human-readable `message`, and, when they apply, `http_status` and
`retry_after_sec`. The codes: `config` (a broken or drifted connector setup),
`permission` (no grant, no account, or the `max_calls` budget spent),
`invalid_args`, `auth` (the service rejected the credentials), `not_found`,
`rate_limited` (with `retry_after_sec` when the service gave one), `service`
(any other non-2xx, a redirect, or an `error_when` reclassification inside a
2xx), `network`, `timeout`, and `incomplete_response`. An MCP call adds
`tool_error` (see [MCP servers](#mcp-servers)). A function that is sent
once (see [Sending once](#sending-once)) adds two refusals: `unknown_effect`
and `in_flight`. Any call refuses with `attempt_closed` when it does not
belong to the node's current attempt: that attempt has ended, or the call
carries another attempt's `APB_ATTEMPT_TOKEN` (an agent left behind by an
earlier attempt). A send also needs the token itself: a call that names no
attempt may read and write, but not send once in an attempt's name.

Besides an `error_when` reclassification, `incomplete_response` is the
failure a 2xx can produce on a `read_only` function. A response body that was cut at the read cap, or whose
read failed partway through, must not pass as complete data: the call fails
with `reason` set to `body_cap` or `read_error` (plus `body_cap_bytes` and
the `io_error_kind` where they apply: `unexpected_eof`, `connection_reset`,
`connection_aborted`, `broken_pipe`, `timed_out`, `invalid_data`,
`interrupted` or `other`), no partial body is returned, and the
run journal records the outcome as `incomplete_response` with
`response_complete: false`. The fix is to narrow the request (page, filter,
or use a smaller page size) and call again; nothing retries such a call
automatically.

For a function that is not read-only the call stays a success, because its
effect already happened. The result then returns the body that was read and
carries `effect: "committed"` and `response_complete: false` (plus the
`reason`) at the top level of the result JSON, where a `response_pick`
projection cannot remove them; a complete response carries no such keys. A
cut body is not always `truncated`: one whose read failed partway is
`truncated: false` with `response_complete: false`, so check
`response_complete`. The run journal records `response_complete` for every
HTTP call that answered 2xx (and for an `incomplete_response` failure), so a
playbook report can tell a whole response from a cut one.

A function that declares `content_types` fails `service` on a 2xx whose
media type matches no entry, without reading the body. The error keeps the
2xx `http_status`; on a function that is not read-only it also carries
`reason: content_type` and the journal records `response_complete: false`,
because the service accepted the write: a send of such a function is
settled as sent and is not repeated.

## Paging

A list function can follow a service's own pagination within one call, so
the agent does not re-issue the call per page and track the cursor itself.
The manifest declares it per function:

```yaml
    paginate:
      items: messages                # dot path of the item array in each page; "" is the body itself
      next: { token_path: nextPageToken, token_arg: page_token }
      max_pages: 5                   # default 5, at most 20
      items_optional: true           # an absent item array reads as an empty page (a search with no hits)
```

Two ways to find the next page: a body token (`token_path` is where the
page carries it, `token_arg` the string argument it is passed back in,
with the request re-rendered exactly like the first one so every other
query pair stays the same), or the `Link` header's `rel="next"` URL
(`next: { link_header: true }`). A Link URL is followed only within the
scheme, host and port of the first request; a Link that points elsewhere
fails the call instead of being sent there.

A failed page fails the whole call, with a `page {n} of the listing`
prefix on the message from page 2 on: a partial list is never returned as
success. The bounds are declared: `max_pages` pages (default 5, at most
20) and 4 MiB of response bodies across the pages of one call. Past
either bound the call returns what it has and `more: true` says the
listing went on.

The result is one document: the pages' items concatenated in order, with
`pages` and `more` at the top level next to `ok`. The body around the
items is the last page's, so a cursor the service put there (a
`nextPageToken`, an `offset`) is the one that continues the listing:
pass it back in the function's page-token argument when `more` is `true`.

The walk is one call: one `connector_call` event whose `pages` field
records how many were fetched (the failed page included), one `max_calls`
unit whatever the page count, and one `timeout_sec` that has to cover
every page, which is why the paging functions of `google-calendar` and
`google-drive` declare 60.

Paginated reads are contract-tested offline with scripted responses; see
"Contract tests" under [Sending once](#sending-once).

## Sending once

A step that sends a message, posts a comment or opens an issue can be run
again: a retry after a failure, an infrastructure retry after a network
error, a fallback step, a deadline continuation, a resume after a pause or a
crash, a supervisor retry. When the earlier attempt's send went out but its
answer was lost, a plain repeat sends a second copy. A function that declares
`idempotency` is protected against that: apb records every send of it in the
run journal before the request leaves, settles what became of it afterwards,
and answers a repeat of the same send from that record.

### What makes two sends the same send

A send is identified by the node, the connector function, the account and a
label. The label is the `--intent` name the agent passes:

```text
apb connector call telegram send_message --intent release-1.4 --args '{"chat_id":"-100","text":"1.4 is out"}'
```

An intent name is 1 to 64 characters of `a-z`, `0-9`, `.`, `_` and `-`,
starting with a letter or a digit. Without `--intent` the label is a hash of
the arguments, so a repeat that is word for word the same is still
recognised, but a reworded one is not: tell agents to pass a stable name and
the same name on every attempt. A list item's send without `--intent` is
the exception: its label is the fixed `item:send`, so one item sends a
function once (see "Sending once in list mode" below). The connector block of the agent's prompt
marks such functions `send-once` and says so.

What counts is the node's effect scope: everything since the node last
succeeded. Every kind of repeat listed above falls inside it, also across a
failure route back into the same node. A loop visit after a success starts a
new scope, so a send that belongs to each iteration is allowed again. A new
run (`continue_from`, a fresh start) has its own journal and does not see the
earlier run's sends.

### What a repeat gets

| The earlier send | A repeat with the same intent |
| --- | --- |
| went through (`confirmed`) | `{ok: true, already_sent: true, intent, sent_in_attempt, ref}`, with no request and no `max_calls` charge, also once the budget is spent |
| did not go out (`not_sent`: refused by the service, or recorded and never sent) | a new send |
| is being sent right now by another call | the `in_flight` refusal |
| may or may not have gone out (`unknown`: a timeout, a cut connection, a 5xx, a sender that died) | depends on the mode, below |

Whether an unknown send can be repeated safely depends on what the service
offers, declared per function:

```yaml
  - name: send_message
    method: POST
    url: "{{account.api_base}}/channels/{{args.channel_id}}/messages"
    body: { content: "{{args.content}}", enforce_nonce: true }
    idempotency:
      mode: key                 # key | marker | none | repeatable
      field: nonce              # or `header: Idempotency-Key`, exactly one
      window_sec: 120           # how long the service honours the key
      result_ref: id            # optional: the created item's id or link
      encoding: base32hex       # optional: map the key onto 0-9/a-v, see below
      conflict: confirms        # optional: a 409 to a repeat proves the first send, see below
```

- `key`: the service deduplicates by a key apb puts into a header or a
  top-level body field. Within `window_sec` of the first send (the repeat's
  own `timeout_sec` included, so its answer still comes inside it), a repeat sends
  again with the same key and the service answers with the one item it
  already has (settled `confirmed` by `key_replay`). After the window the
  send is treated like `none`.
- `marker`: apb appends a hidden marker to a text argument
  (`format: html_comment`, an HTML comment invisible in rendered Markdown)
  and, on a repeat, looks for it among the account's own recent items before
  deciding. Found means the earlier send went through: the repeat gets its
  reference and nothing is sent. Not found, or a lookup that cannot see the
  whole list, leaves the send unknown, and a `marker` send is never resent
  automatically. The lookup is declared with `reconcile` (a `read_only`
  function of the same connector, the arguments copied from the send, a
  `since_arg` filled with the intent's time minus 300 s, and the dot paths of
  the list, the text, the author and the item reference) plus a
  connector-level `identity` (a `read_only` function and the dot path of the
  account's own name), so a marker another account posted never counts.
- `none`: the service offers no way to tell. A repeat of an unknown send is
  refused with `unknown_effect` until a person decides, unless the binding
  says `on_unknown: resend`.
- `repeatable`: a repeat changes nothing (attaching a label that is already
  there, removing one that is gone), so there is nothing to protect. The
  call is not recorded in the send ledger and behaves exactly like a
  function with no declaration: a call whose answer was lost is retried, and
  nothing waits for a person. Unlike an undeclared write, a list step may be
  granted it (V88). Declare it only when a second call really leaves the
  same state as the first; a message or a comment is never repeatable.

Two `key` options exist for services that need them, and both are refused
on any other mode. `encoding: base32hex` maps the key (the intent's nonce,
24 characters of `a-z` and `2-7`) character for character onto `0-9a-v`,
for a service whose ids allow only that alphabet. `conflict: confirms`
says the key is an id the client chose (an event id apb generates, a mail
message apb composes): a 409 to a repeat then proves the first request
created that id, the send settles `confirmed` with source `key_conflict`,
and the repeat is answered `already_sent` with the key as its `ref`.
Without `conflict`, a 409 stays unknown, like any other outcome nobody
can tell.

`idempotency` is allowed only on HTTP, mock, smtp, imap and mcp functions
that are not `read_only` (an smtp or imap function may declare only `mode:
none`, since neither protocol has a key or a marker to reconcile with; an
mcp function, below, anything but `key`), and
the
manifest is refused at load time when a `key` field is empty, is also an
argument, or sits in a body that does not always render as a JSON object
(an object, or `{{args}}` with an object `args_schema`), a header collides, a
`marker` argument is not a string used in the body or has the `literal` text
mode, the `reconcile` function is missing, not a `read_only` HTTP function,
or not given its required arguments, or `identity` is missing, not
`read_only` or takes required arguments. The JSON and form body of every
HTTP write has the word `apb-intent` rewritten, so no marker apb looks for
(it reads only the items' text) can be forged through apb. An agent
posting directly under the same account, outside apb, could still plant one;
agents get connector tokens scrubbed from their environment, which makes
that unlikely but is not a guarantee.

`--dry-run` never carries a real key or marker: the preview shows
`(set at send)` for a key and `[apb adds a hidden send marker here]` for a
marker.

An mcp function (see [MCP servers](#mcp-servers)) that is not `read_only`
must declare `marker`, `none` or `repeatable`; `key` is refused, since the
protocol has no idempotency key. A `marker` send's `reconcile` function is
a `read_only` mcp function of the same connector, and the marker rides in
the tool's arguments, neutralized and appended as for HTTP. What counts as
unknown is wider than for HTTP: once the `tools/call` request was handed to
the server, an answer with `isError: true`, an `input_required` answer, a
task handle and a JSON-RPC error of any code are the outcome `tool_error`
and settle the send `unknown`, as do a timeout and a broken connection.
Only a failure apb knows came before the hand-over settles `not_sent`.

### `on_unknown`: a binding's choice for unknown sends

```yaml
connectors:
  - { name: telegram, functions: [send_message], on_unknown: resend }
```

`refuse` (the default) keeps an unknown send refused until a person decides.
`resend` lets a repeat send it again, recorded as a resend of the earlier
intent, for a message where a rare duplicate is better than a missing one.
That includes a send that is still under way but whose sender stopped
answering: a pending intent reads as unknown once its call's timeout plus
30 s has passed (or its process is gone), so a slow request that is still
in flight past that deadline can be resent, and both may arrive. It
applies to `none` sends and to `key` sends after their window, never to
`marker` sends. The validator refuses `on_unknown` on a binding none of whose
functions declares `none` or `key` (V69; a `repeatable` function does not
count, since it is retried anyway), and `resend` on a binding that
grants an `irreversible` function (V84).

`resend` accepts a possible duplicate, and it does not settle the first
send: a resend is a new send with its own nonce and proves nothing about
the one before it. The first send stays unknown, so the run still needs a
look and a person resolves that first send (see "A person's decision"
below). The run outcome and the `resolve_intent` next action say so: "a
resend went out (si-...); whether this first send also went out is not
known".

### Sending once in list mode

A list node (`list:`, HOWTO-authoring "List processing") runs its step once
per item and repeats items by design, so every write function it is granted
must declare `idempotency` (V88): a send-once mode, or `repeatable` for a
call a repeat leaves unchanged. The send ledger keys each item's sends by
the node and the item, so one item's send never answers another's. Without
`--intent` an item's send carries the fixed label `item:send`: one send per
function per item, also when a rerun words the payload differently. An item
that sends one function more than once names each send with its own
`--intent`. A call that names an item with no running attempt of its own
is refused, so a made-up `APB_ITEM_ID` can neither send nor reset a spent
`max_calls`. When an
item's step fails with a send of unknown outcome, a `key` or `marker` send
gets one free reconcile run of that item; a `none` send leaves the item
needing a look (`unknown_send`), and no retry runs it again until a person
resolves the send, also under `on_unknown: resend` (once the item has
settled, no later round, visit or retry runs it again on its own while that
send's outcome is unknown).
An item whose step resent such a send itself (`on_unknown: resend`) and
then passed reads as passed on its chain of sends, while its first send
stays open for a person: its row says "earlier send unresolved" (the
`unresolved_send` field of the item in `run_status`, `run_report`, the
run detail of the HTTP API and `apb runs <id> --items`, and the dashboard's
item table), and `run_status` names it beside the items that need a look.
The declarations on `asana.create_task` and
`imap.mark_read` exist so those functions can be used in list steps, but
they apply to every binding: outside a list too, a repeat of a send whose
outcome is unknown is refused unless the binding says `on_unknown: resend`.

### A person's decision

An unknown send is listed in the run outcome as a gap
(`call outcome unknown`) that names the intent and the command, and the run
reads as needing a look (`unknown sends`). A replay repeats a send with its
nonce, so a replay that went out settles it, and one that did not go out
does not, so the send stays listed. A resend is a separate send: each
unknown one stays listed on its own, also after a later resend went out,
since both may have acted. Check the service, then record what you
found, naming the intent by its `si-...` id (not the `--intent` label):

```text
apb intent list <run>                          # each send's id (si-...), label and state
apb intent resolve <run> <si-id> --sent        # it went out: a repeat answers already_sent
apb intent resolve <run> <si-id> --not-sent    # it did not: a repeat sends it again
```

`--note` keeps what you checked with the decision. Over MCP the same is
`run_intent_resolve`; an agent asks the user first and passes on only their
answer. The CLI refuses a decision from inside a run (`APB_RUN_ID` or
`APB_HOST_TASK` set in its environment). The MCP tool cannot tell a
person's session from a host subagent that shares the same server, so there
the rule is an instruction, as for `review_decide`: a subagent never calls
it and reports the send as a finding instead, and a person resolves it. A send whose outcome is already known, or that is still in flight, is
not decided again. A send confirmed by apb's lookup or by a person is listed
under the run's done lines.

### Contract tests

A `tests.yaml` case can render as a send of an intent and check a lookup
offline:

```yaml
  - function: comment_issue
    args: { owner: acme, repo: site, number: 42, body: "Looking into this." }
    intent: { run: r-1, node: triage, attempt: 2, nonce: abcdefghijklmnopqrstuvwx }
    expect:
      reconcile:
        identity: { login: triage-bot }       # the identity function's raw answer
        response:                             # the reconcile function's raw answer
          - user: { login: triage-bot }
            html_url: "https://github.com/acme/site/issues/42#issuecomment-2"
            body: "Looking into this.\n\n<!-- apb-intent v1 run=r-1 node=triage attempt=2 nonce=abcdefghijklmnopqrstuvwx -->"
        outcome: found                        # found | absent | inconclusive
        item_ref: "https://github.com/acme/site/issues/42#issuecomment-2"
```

`intent` makes the case render with the key or the marker that intent
carries, so `body_contains` and `headers` can check it. `expect.reconcile`
runs the lookup's matching against the given answers (`next_page: true`
says the listing had a further page) and checks its conclusion, `outcome`:
the item is `found`, `absent` from a listing read whole, or the lookup is
`inconclusive` (no item list where the connector says it is, or a further
page), and the found item's reference.

A case can also script the HTTP answers a call reads offline, which is
how the paging functions are tested. Four fields in `expect` cover it:

- `responses`: the answers the runner gives the rendered request and each
  next page, in order (`status`, default 200, `headers`, `body`; the
  content type is `application/json` unless a header says otherwise);
- `requests`: every URL the call requested, first page included;
- `result_contains`: a subset match over the result JSON (`ok`, `body`,
  `pages`, `more`);
- `error_code`: the expected error code instead of a success.

```yaml
  - function: list_issues
    args: { owner: acme, repo: site, state: open, labels: "", page: 1, per_page: 50 }
    expect:
      responses:
        - headers: { Link: '<https://api.github.com/repos/acme/site/issues?page=2&per_page=50>; rel="next"' }
          body: [ { number: 1 }, { number: 2 } ]
        - body: [ { number: 3 } ]
      requests:
        - "https://api.github.com/repos/acme/site/issues?labels=&page=1&per_page=50&state=open"
        - "https://api.github.com/repos/acme/site/issues?labels=&page=2&per_page=50&state=open"
      result_contains:
        ok: true
        pages: 2
        more: false
        body: [ { number: 1 }, { number: 2 }, { number: 3 } ]
```

A response left unused or missing, a request that differs, a result that
does not contain the expected subset, or a differing `error_code` fails
the case. The same content-type guard the live path applies runs over each
scripted response too, so a case and a live call refuse the same headers.

An mcp function's case takes the expectation kind `mcp`: the tool the
function maps and a subset of the arguments its call would send, the
marker preview included. The case renders through the same path a
`--dry-run` call takes; no server starts, no secret is resolved and
nothing is sent.

```yaml
  - function: add_note
    args: { contact: "c-17", body: "Called back." }
    expect:
      mcp:
        tool: add_note
        arguments_contain: { contact: "c-17" }
```

## The `apb connector` CLI

```text
apb connector list              installed connectors, trust and config status
apb connector show <name>       manifest summary and per-account status
apb connector call <name> <fn>  the agent-facing call channel (--account, --args, --dry-run, --intent)
apb connector approve <name>    approve a connector or (--account) an account digest; an MCP connector only by a person at a terminal, --account also pins its program and tools
apb connector oauth <name>      one-time OAuth consent for an account; a person at a terminal; prints the refresh token once
apb connector tools <name>      an approved MCP server's tools and their status (--account, --json); exit 3 when any is not approved
apb connector doctor            check every connector: manifest, config, env, trust, healthcheck
apb connector env [<name>]      unresolved env var names as ready-to-paste KEY= lines
apb connector init <name>       scaffold a new connector folder from a template
```

`apb connector call` needs a run context (`APB_RUN_DIR` and `APB_NODE_ID`, set by
the engine when a node executes a call). Outside a run, `--dry-run` renders a
call without executing it, against the live connector and account config (no
secret is resolved, so no approval is needed); probe an account with the
dashboard healthcheck. For an MCP connector the healthcheck checks the pins
without a tool call (it starts the pinned server, lists and compares its
tools, and closes it), and the dashboard's playground refuses a live call of
an mcp function (`mcp_live_call_unsupported`).

Approving an MCP connector, with or without `--account`, is a person's act
at an interactive terminal only: there is no flag and no form without a
terminal (provisioning scripts are future work), it is refused inside a run
step, and the dashboard does not approve MCP connectors. `approve
--account` also pins the connector's program and tools (see
[MCP servers](#mcp-servers)).
`--args -` reads the JSON arguments from stdin.

## Agent-written text (`text_args`)

The text a connector sends is usually written by an agent, and some services
act on text, not only show it: a line that starts with a slash command can
close or merge something, and a reference or mention in a title can link or
ping people. A function declares how each such string argument is prepared,
with the optional `text_args` map from argument name to mode:

```yaml
  - name: comment_issue
    method: POST
    url: "{{account.api_base}}/projects/{{args.project}}/issues/{{args.iid}}/notes"
    body:
      body: "{{args.body}}"
    text_args:
      body: markdown_no_commands
    args_schema:
      type: object
      properties:
        body: { type: string }
```

The engine applies the mode to the argument's value before it is placed into
`body` or `body_form`, so no template can send the raw value around it. An
argument with a mode may appear only in the body: one also used in `url`,
`query` or `headers`, where no mode is applied, fails the manifest at load
time. There are three modes:

| Mode | Use it for | What is sent |
| --- | --- | --- |
| `markdown` | Text where formatting and commands are intended, such as a comment on a service that runs commands only through a separate function | The value exactly as given. Declaring it documents the intent. |
| `markdown_no_commands` | Multi-line Markdown on a service that runs slash commands found in the text (comments, descriptions) | The value as written, except that a `/` at the start of any line (after optional spaces or tabs; a lone carriage return ends a line too) followed by a letter, digit or underscore gets a backslash, so the service shows the command as text. A `/` in the middle of a line is unchanged. Lines inside fenced code blocks are escaped too, since a fence is not a documented guarantee that a command there is ignored. |
| `literal` | A one-line value from untrusted text, such as a title | One line: whitespace runs collapse to one space and the ends are trimmed. Every Markdown-significant character gets a backslash escape (never an HTML entity): `` \ ` * _ [ ] < > \| ~ # & @ ! $ % ``. At the start of the value a `-`, `+`, `=`, an ordered-list marker (`1.`, `2)`) or a slash command is escaped too. Bare `http://` and `https://` URLs are kept as typed, so they still link. |

Only HTTP functions may declare `text_args`, and every key must be a string
property of `args_schema`; an unknown mode or any other key fails the manifest
at load time. Arguments without a declared mode are sent as given, as before.
Pick `markdown_no_commands` for comment and description bodies of a service
that executes commands from text, `literal` for titles and other single-line
fields that the service renders as Markdown, and `markdown` where commands are
meant to work. A field the service shows as plain text, or stores as given and
shows with its backslashes, needs no mode: an escape there would only show up
as a stray backslash, and a value read back and sent again would gain one more
each time.

## Text people read (`people:`)

A function that sends something a person reads (a message, a mail, a pull
request description) declares which of its string arguments those are:

```yaml
  - name: send_message
    people: { to: chat_id, text: text }
    idempotency: { mode: none, result_ref: result.message_id }
```

`text` is required and names the body; `subject` (a mail subject, a chat
topic, a pull request title) and `to` (the recipient) are optional. Each must
name a string property of `args_schema`, no argument may fill two roles,
and the block is refused on a function that sends nothing (`read_only`, an
inbox or an imap read).

The block does two things:

- A node granted such a function gets apb's writing brief for people in its
  prompt (lead with the conclusion, say who acts next, state facts and mark
  what is not verified, no internal terms), and its connector block names
  the arguments people read, one line per function. See "Writing for people"
  in `docs/HOWTO-authoring.md`.
- A function whose block names `to`, that declares `idempotency`, and whose
  `args_schema` requires no argument the block does not name can carry run
  notifications (`docs/NOTIFY.md`): apb fills `to` with the person's
  recipient, `text` with the notice and `subject` with its subject line, and
  nothing else. A `text` argument with the `literal` text mode gets the
  notice as one line.

The official connectors declare it on `telegram.send_message`,
`slack.send_message` (`reply_in_thread` has `text` only), `smtp.send_email`,
`zulip.send_stream_message`, `zulip.send_direct_message` and
`github.create_pull` (`subject` and `text` only).

## Official connectors

Nineteen official connectors ship inside the `apb` binary and install with
`apb connector install <name>`: `amocrm`, `asana`, `atrip`, `discord`,
`github`, `gitlab`, `gmail`, `google-calendar`, `google-drive`, `imap`,
`sentry`, `slack`, `smtp`, `telegram`, `twenty`, `web-search`, `whatsapp`,
`youtrack`, `zulip`. Installing
from the binary records trust for the
connector's tree digest in the same action, since the bytes are already
part of the binary you are running; `apb connector install --from-dir
<path>` (the development loop for this repository, `connectors/<name>/`)
keeps the normal approve flow.

### github

Account fields: `api_base` (`https://api.github.com`, or your GHES API
base), `token` (secret), and `webhook_secret` (secret, optional - required
only on an account that receives webhook deliveries). The connector's
`webhook` block verifies `X-Hub-Signature-256` against that secret and types
each delivery by the `X-GitHub-Event` header (`issues`, `pull_request`,
`push`, `workflow_run` and the rest); the three inbox functions
(`inbox_read`, `inbox_ack`, `inbox_peek_depth`) read the stored deliveries,
and `inbox_read` takes `types` and `where`. Register the hook URL
`/hooks/github/<account>` with a `webhook_secret` set to receive. Prefer `token: "{{cmd:gh auth token}}"` when
`gh auth login` has already run; otherwise `{{env.GITHUB_TOKEN}}` with a
personal access token: classic PATs need `repo` (or `public_repo` for
public repositories); fine-grained PATs need repository access with
Actions write permission for `dispatch_workflow`. Healthcheck:
`get_rate_limit`. `create_issue` and `comment_issue` are sent once in
`marker` mode: the lookup reads the token's account (`get_authenticated_user`,
`GET /user`) and the recent issues (`list_recent_issues`) or comments
(`list_issue_comments`). A GitHub App installation token cannot read `/user`,
so with one an unknown send stays unknown until a person decides.
`create_release` declares `none`: a duplicate tag is refused by GitHub, but
nothing reads back which intent created a release. `add_labels` and
`remove_label` declare `repeatable`: GitHub has no key or marker for a
label, and repeating either changes nothing on the issue, so they are not
tracked as sends (a lost answer is retried, as for any undeclared call), and
a list step may be granted them (V88).

`repeatable` arrived with github 0.2.1. An apb older than that mode cannot
read a manifest that declares it (its parser denies an unknown mode), and it
skips the connector instead of restoring its own copy. After going back to
such a binary, put its copy back with `apb connector install github --force`
(run with that older binary); this apb reads both the 0.2.0 and the 0.2.1
manifest.

`list_issues` and `list_pulls` follow GitHub's `Link` pages within one
call, up to 5 pages of up to 100 items each (the optional `per_page`
argument): the result concatenates the pages and carries `pages` and
`more` (see [Paging](#paging)). Paging arrived with github 0.3.0. An apb
older than this change cannot read the new manifest (its parser denies
the `paginate` key), and it skips the connector instead of restoring its
own copy; after going back to such a binary, put its copy back with
`apb connector install github --force` (run with that older binary).

### telegram

Account fields: `api_base` (`https://api.telegram.org`, overridable for
a self-hosted Bot API server) and `token` (secret) - the token
[@BotFather](https://t.me/BotFather) gives you for a new bot. The bot
must already be a member of a chat before `send_message` reaches it.
Healthcheck: `get_me`. `send_message` declares `idempotency: none`: the Bot
API has no idempotency key and a bot cannot read chat history, so an unknown
send is refused until a person decides, or resent under `on_unknown: resend`.

### smtp

Account fields: `host`, `port`, `from_email` (all required), and
`username`, `password` (secret), `from_name`, `use_tls` (all optional).
Set `use_tls` explicitly (there is no engine-level default for account
fields): `true` for STARTTLS on port 587, the common case. Healthcheck:
`verify` (connects, negotiates STARTTLS, authenticates, sends nothing).
`send_email` declares `idempotency: none`: SMTP has no idempotency key, so an
email whose answer was lost is refused on a repeat until a person decides, or
resent under `on_unknown: resend`.

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
call, omitting it on the first call. `list_tasks` also follows
`next_page.offset` pages within one call, up to 5 pages: the result
concatenates the pages and carries `pages` and `more` (see
[Paging](#paging)), and its `next_page` is the last page's. That, and
the manifest key it needs, arrived with asana 0.2.0: an apb older than
this change cannot read the new manifest, and after going back you
reinstall with that binary (`apb connector install asana --force`).
`search_tasks` is a fuzzy typeahead
match against task names, not a full-text search; use `list_tasks` with
a project filter when a complete, predictable result set matters more.
`create_task` declares `idempotency: { mode: none, result_ref: data.gid }`:
Asana takes no idempotency key, so a task whose answer was lost is refused
on a repeat until a person decides (the task may exist), or created again
under `on_unknown: resend`; a confirmed send keeps the created task's gid
as its reference. Healthcheck: `get_me`.

### imap

Account fields: `host`, `port`, `auth_method` (`password`, `xoauth2` or
`oauth2`), `username` (all required), `password` (secret, required only
for `auth_method: password`), `use_tls` (optional, default `true`),
`from_email` and `from_name` (optional, the From address and name of a
composed draft), and the optional `oauth_*` fields that
`auth_method: oauth2` reads (see [OAuth accounts](#oauth-accounts): apb
mints the access token itself from a refresh token obtained once through
`apb connector oauth imap`). One connector serves any IMAP provider,
since the protocol is identical everywhere and only the connection
settings differ. `search_messages`, `get_message` and
`save_attachments` open the folder read-only with `EXAMINE` and fetch
content with `BODY.PEEK[]`, so reading a message never marks it seen;
only `mark_read` and `mark_unread` change anything on the server, and
each is a separate, independently grantable function. `mark_read`
declares `idempotency: { mode: none }` (`mark_unread` declares no
`idempotency`), so a list step may be granted it (V88); a `mark_read`
whose answer was lost is refused on a repeat until a person decides, or
sent again under `on_unknown: resend`, which suits it since marking a
message read twice does no harm. `use_tls` defaults to `true` when omitted; only set it to
`false` for a local plaintext test fixture, never for a real provider.
Gmail (`imap.gmail.com`, port `993`) needs 2-Step Verification enabled
before an app password can be generated; a Google Workspace account
with app passwords disabled by policy instead uses `auth_method:
oauth2`. Outlook and Microsoft 365 (`outlook.office365.com`, port `993`)
do not accept passwords at all: use `auth_method: oauth2`, or an
external token helper such as `oama` or `mutt_oauth2` with
`auth_method: xoauth2`. Yandex Mail
(`imap.yandex.com`, port `993`) needs IMAP access enabled in the Yandex
Mail web settings before an app password can be generated. iCloud
(`imap.mail.me.com`, port `993`) uses an app-specific password from the
Apple ID account page. No message deletion, no move between folders,
and no sending: this connector reads, marks read/unread, files drafts
(`append_draft` composes a message from `from_email` and the usual
address and body arguments and appends it to the drafts folder, marked
`\Draft`, never sent), and saves a message's attachments into the run's
attachments directory (`save_attachments`: at most 20 files, 10 MiB per
file and 25 MiB per call, inside a run only, never overwriting). Its
results are text people wrote, so `search_messages`, `get_message` and
`save_attachments` are marked `untrusted_output`. It is still meant to
be installed alongside `smtp` (or `gmail`) for a read-and-reply
workflow. Healthcheck: `verify` (connects, negotiates TLS,
authenticates, without opening or reading any mailbox). Drafts,
attachments and `oauth2` arrived with imap 0.2.0: an apb older than this
change cannot read the new manifest, and after going back you reinstall
with that binary (`apb connector install imap --force`).

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
`remove_labels` are comma-separated deltas). Agent-written issue and merge
request comment bodies and descriptions are sent as `markdown_no_commands`
(see `text_args` above), so that text can never run a quick action. Titles
and release names and descriptions are sent as written: GitLab runs no quick
action from them, and it stores a title as given, so a `literal` escape would
stay in it as a visible backslash. `trigger_pipeline`
starts CI on a branch or tag (optional `variables` is an array of
`{key, value}` objects); grant it only when a playbook is meant to
start pipelines, not just observe them. `create_release` is sent once in
`marker` mode: the lookup reads the token's account (`get_user`) and the
20 newest releases by creation time (`list_releases`); a release not among
them leaves the send unknown for a person, and apb never sends it again on
its own.
Healthcheck: `get_user`.

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
name; find it with `list_projects`. Comment and description text is declared `markdown` and sent as
written, since YouTrack runs no command from it; `apply_command` runs YouTrack's
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
`get_me`. `send_stream_message` and `send_direct_message` declare
`idempotency: none`: Zulip takes no idempotency key, so a send whose outcome
is not known is refused on a repeat until a person decides, or resent under
`on_unknown: resend`. Before, such a send was simply sent again.

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
Healthcheck: `get_me`. `send_message` and `reply_to_message` are sent once in
`key` mode: every message carries a `nonce` with `enforce_nonce: true`, and
within 120 s a repeat sends the same nonce, so Discord returns the one
message. Both post with `allowed_mentions: { parse: [] }`, so an
`@everyone` or a role mention in agent-written text never pings anyone.

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
`send_message` declares `idempotency: none`: Slack takes no idempotency key,
so a send whose outcome is not known is refused on a repeat until a person
decides, or resent under `on_unknown: resend`. Before, such a send was simply
sent again.

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

### whatsapp

Account fields: `base_url` (`https://graph.facebook.com`), `graph_version`
(an account field rather than a hardcoded segment because Meta supports each
Graph API version for about two years; documented current value `v23.0`),
`phone_number_id`, `waba_id` (all required, non-secret), `access_token`
(required, secret), and `app_secret`, `verify_token` (both secret, optional -
required only on an account that receives). Create the access token as a
System User permanent token under the Meta Business Suite with the
`whatsapp_business_messaging` and `whatsapp_business_management`
permissions, set to Never expire; `phone_number_id` and `waba_id` are both
visible on the Meta app's WhatsApp > API Setup page. Sending works from any
install with a token; receiving has no polling endpoint on Meta's side and
needs the server-mode plus webhook-ingest topology, a document-level
`webhook` block (`challenge: meta_hub`, `hmac_sha256_hex` signature over
`X-Hub-Signature-256`) whose `verify_token` and `app_secret` are the two
account fields above. Each delivery is typed by the change's field
(`entry.0.changes.0.field`: `messages` for inbound messages, reactions and
delivery or read statuses alike, or another subscribed field such as
`message_template_status_update`), and `inbox_read` takes `types` and
`where`; a status update is told apart with `where` on
`entry.0.changes.0.value.statuses.0.status`. Covers send (`send_text`, `send_template`,
`send_media`, `mark_read`), template management (`list_templates`,
`create_template`, `delete_template`), business profile
(`get_business_profile`, `update_business_profile`), phone numbers
(`list_phone_numbers`, `get_phone_number`), media (`get_media_url`,
`delete_media`), and receiving (`inbox_read`, `inbox_ack`,
`inbox_peek_depth`). 16 functions in total. A plain `send_text` only
succeeds inside the 24-hour customer-service window opened by the
recipient's last inbound message; outside that window Meta rejects it with
error 131047 and `send_template` is required instead to re-initiate contact,
a rule the connector cannot enforce since it is per-recipient runtime state
on Meta's side. `delete_template` and `delete_media` are HARD,
unrecoverable deletes, unlike this connector family's twenty sibling, where
every delete is reversible. `send_media` is a full-body passthrough (the
media object's JSON key varies with the message type), so a grant of it can
in practice send any message type, not only media. Uploading binary media
is out of scope: Meta's upload endpoint is multipart/form-data with a
binary file part, which this connector's schema cannot express, so media is
sent by a hosted `link` or by a media `id` obtained out of band.
`send_text` and `send_template` declare `idempotency: none`: the Cloud API
takes no idempotency key, so an unknown send is refused on a repeat until a
person decides, or resent under `on_unknown: resend`. The
`healthcheck` is `get_phone_number`, which takes no arguments (its `fields`
query is a fixed literal so the no-arg probe stays self-sufficient) and
proves the token is valid and that `phone_number_id` belongs to it.

### amocrm

Account fields: `base_url` (scheme and host only, no path suffix, either
`https://<subdomain>.amocrm.ru` or `https://<subdomain>.kommo.com`, since
amoCRM and Kommo are the same API on different domains), `drive_url`
(optional, non-secret, the account-specific Files API host such as
`https://drive-b.amocrm.ru`, read once from `get_account` with
`with=drive_url` and used only by `list_files` and `get_file`), and
`access_token` (secret). The token is the long-lived token of an integration
on that account: open the integration in amoMarket, tab "Keys", "Generate
token", pick an expiry between one day and five years, and copy the value,
which is shown only once. Long-lived tokens carry no refresh token, so
nothing rotates them and a lapsed expiry simply starts answering 401; they
are revocable from the integration's "Granted access" tab. A private
integration can require an application to amoCRM support on a non-technical
account, while an external integration created in amoMarket issues the same
kind of token without that step; the connector accepts either, since both
are Bearer tokens. Covers account and dictionaries (users, roles, event
types, loss reasons), leads and the unsorted inbox, pipelines and statuses,
contacts and companies, customers with statuses, segments and transactions,
tasks, notes, tags, entity links, the event feed, custom field definitions
and groups, catalogs and products, webhook subscriptions, salesbots, lead
sources, chat templates, talks, subscriptions, short links, call logging,
and the Files API (list, read, attach, detach). 101 functions in total, 49
of them `read_only` and 52 effectful. Wherever amoCRM's routes are
symmetric, one function takes an `entity_type` of `leads`, `contacts`,
`companies` or `customers` instead of four near-identical functions, which
is how notes, tags, links, custom fields, custom field groups, entity files
and batch updates are exposed; subscriptions collapse the same way over the
two families amoCRM documents for that route, `leads` and `customers`. Eight
functions carry the `EFFECTFUL, IRREVERSIBLE` marker: `decline_unsorted`,
`delete_pipeline`, `delete_status`, `delete_custom_field`,
`delete_transaction`, `set_customers_mode`, `enable_products` and
`unsubscribe_webhook`, the last two of them for their account-wide reach
rather than because they cannot be undone; API v4 has no DELETE at all for
leads, contacts, companies, customers, tasks, notes, tags or catalog
elements, so the connector exposes none. Shared API facts that apply
everywhere: 7 requests per second per integration (429 comes back as
`rate_limited` with `retry_after_sec`, nothing retries automatically), an
empty result is HTTP 204 with no body rather than an empty array, HAL paging
with items under `_embedded.<collection>` and the cursor under
`_links.next.href`, `filter[...]` on leads, contacts, companies and
customers may need the paid filtering add-on (`get_account` with
`with=is_api_filter_enabled` says whether it is on), each filter takes
exactly one value because a query map cannot repeat a key, and customers,
catalogs, webhooks and filtering are tariff-gated behind HTTP 402. Out of
scope for this connector's 0.1 surface: receiving amoCRM webhooks through
the ingest listener (amoCRM posts form-encoded payloads without a signature,
while the listener requires a JSON body and an HMAC signature, so webhook
management is exposed but reception is not), file upload (a chunked binary
Drive session, which the format's body kinds cannot express), the Chats API
on `amojo.amocrm.ru` (a separate host with its own HMAC-SHA1 signature
scheme), and OAuth2 authorization-code flow with refresh rotation (refresh
tokens are single-use with a three-month TTL and apb stores no tokens).
Because every granted function is rendered into the node prompt in full,
granting the whole connector costs about 70,000 characters of context;
`connectors/amocrm/README.md` carries seven ready-to-paste grant presets
(`sales-read`, `sales-write`, `inbox`, `setup-admin`, `catalog`,
`customers`, `files`) and the measured prompt sizes. Request shapes are
verified against the amoCRM API v4 documentation and the offline contract
tests, not yet against a live account. The `healthcheck` is `get_account`,
which renders with zero arguments and succeeds against any token the account
has issued.

### gmail

Account fields: `api_base` (`https://gmail.googleapis.com/gmail/v1`),
`token_url` (`https://oauth2.googleapis.com/token`), `client_id`,
`client_secret` (secret), `refresh_token` (secret), `email` (required,
the From address of composed drafts), and `granted_scopes` (optional).
Authenticate with OAuth2: `apb connector oauth gmail` runs the consent in
a person's terminal and prints the refresh token once (see
[OAuth accounts](#oauth-accounts)). Reads: `verify` (the healthcheck),
`list_labels`, `search_messages` (Gmail's query language in `q`; follows
`nextPageToken` pages, and a search with no hits reads as an empty page),
`get_message` (headers and body parts, text parts decoded; a binary part
is marked instead of decoded) and `get_thread`; message content is text
people wrote, so `get_message` and `get_thread` are marked
`untrusted_output`. Writes: `create_draft` (apb composes the RFC 5322
message itself and the dry-run shows the composed header preview; a reply
in a thread needs `thread_id`, `in_reply_to`, `references` and the same
subject), `send_draft` (the one irreversible function: the message leaves
the mailbox and cannot be recalled), and `add_labels`, `remove_labels`
and `archive` (`repeatable`). Grant presets (`read`, `draft`, `triage`,
`send`) with measured prompt sizes are in `connectors/gmail/README.md`;
`connectors/gmail/INSTALL.md` is the setup runbook.

### google-calendar

Account fields: `api_base` (`https://www.googleapis.com/calendar/v3`),
`token_url` (`https://oauth2.googleapis.com/token`), `client_id`,
`client_secret` (secret), `refresh_token` (secret), and `granted_scopes`
(optional). Authenticate with OAuth2 like `gmail` above
(`apb connector oauth google-calendar`). Reads: `verify` (the
healthcheck), `list_calendars` and `list_events` (both follow
`nextPageToken` pages), `get_event`, and `query_free_busy` (busy
intervals of up to 50 calendars; the free slots are the gaps between
them, which the agent computes with the task's own rules). Writes:
`create_event` (sent once in `key` mode: apb sets the event id itself,
and a 409 to a repeat proves the first request created it),
`update_event` and `delete_event` (both `mode: none`, since a repeat can
mail every guest again); all three are `irreversible` because a mailed
invitation, change or cancellation cannot be called back. Guests are
e-mailed per the `send_updates` argument; an omitted one does not
guarantee that no mail goes out, and `none` can keep the event from
reaching some guests. Setup is in `connectors/google-calendar/README.md`
and `connectors/google-calendar/INSTALL.md`.

### google-drive

Account fields: `api_base` (`https://www.googleapis.com/drive/v3`),
`token_url` (`https://oauth2.googleapis.com/token`), `client_id`,
`client_secret` (secret), `refresh_token` (secret), and `granted_scopes`
(optional). Authenticate with OAuth2 like `gmail` above
(`apb connector oauth google-drive`). Reads, all marked
`untrusted_output` (file contents and names are text people wrote):
`verify` (the healthcheck), `search_files` (Drive's query language in
`q`; follows `nextPageToken` pages), `get_file_metadata`, `export_text`
(Docs, Sheets and Slides to text: Docs as `text/plain` or
`text/markdown`, Sheets as `text/csv`, first sheet only, Slides as
`text/plain`) and `read_text_file` (a stored text file, JSON parsed; a
Google Docs file has no stored content and fails here, use
`export_text`). apb reads at most 1 MiB of one answer, so a longer file
or export fails `incomplete_response`; Google's own export limit is
10 MB. Upload is out of scope (no binary request body). `drive.readonly`
is a restricted scope: an app you use for your own account works
unverified, past Google's warning screen. Setup is in
`connectors/google-drive/README.md` and `connectors/google-drive/INSTALL.md`.

### web-search

Account fields: `api_base` (`https://api.search.brave.com/res/v1`) and
`api_key` (secret), the Brave Search API subscription key, sent in its
dedicated header so it never appears in a URL. One function, `search`:
titles, URLs, descriptions and ages, with `count` (1 to 20), `offset` (a
page index, 0 to 9), `freshness` (`pd`, `pw`, `pm`, `py`, or a date
range), `country`, `search_lang` and `safesearch`;
`query.more_results_available` in the result says whether another page
exists, and the agent fetches it by raising `offset` (there is no paging
block). Nothing fetches a result page. The free plan allows one request
per second and answers 429 past it (`rate_limited` with the service's
`retry_after_sec`); every search spends one query against the plan's
quota, the healthcheck `verify` included. Setup is in
`connectors/web-search/README.md` and `connectors/web-search/INSTALL.md`.

### Demo playbooks

`examples/playbooks/sentry-triage.yaml` and
`examples/playbooks/release-announce.yaml` exercise the github,
telegram, smtp, and sentry connectors end to end;
`examples/playbooks/inbox-triage.yaml` exercises imap and asana, and
`examples/playbooks/release-heartbeat.yaml` exercises gitlab,
youtrack, and slack (checking the latest pipeline, posting a summary,
and filing an issue on failure; `trigger_pipeline` is deliberately
absent from its grants). `examples/playbooks/whatsapp-inbox.yaml`
exercises whatsapp: it sends a greeting, then reads one batch of
pending inbox events, drafts and sends a reply for each, and
acknowledges the batch; its `read_inbox` node prompt marks inbox
content as untrusted external input. That pass is single-shot by
design (a cycle back to `read_inbox` needs a bounding condition node
or a `max_traversals` edge); a real deployment wraps the poll in such
a loop, or simply runs this playbook on a schedule. All five double as
reference examples for grant allowlists and `max_calls`. They validate
in CI against fake accounts and are not run against real services
there; run them manually once your own accounts are configured and
approved.

### Coverage note

Write functions (issue creation, merges, releases, sends) are verified
by the offline contract tests in each connector's `tests.yaml`; the
env-gated live smoke tests exercise each connector's healthcheck plus
one read-only function against the real service. Write paths are not
called against real services by any automated test.
