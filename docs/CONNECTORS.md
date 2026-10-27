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
secrets will be sent. A secret read from a command is shown with its command
line wherever an account is approved from, because approving the account lets
`apb` run that command: `apb connector show` and `approve --account` (`cmd`),
the dashboard's account row, the MCP run gate's `unapproved_connector_account`
refusal (`fields` and `cmd`), MCP `connectors_list` (`account_commands`), and
`apb connector doctor` (a `secret command` row per such field). `apb connector
doctor` reports the trust status of every connector and account (approved,
changed since approval, or never approved) alongside manifest, config, and env
checks.

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
may not use that id, V68): exactly that function, on that account, frozen in
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
redirecting or weakening verification for anything a run does. The inbound
listener is guarded by the signature itself rather than by that digest: it
reads the manifest directly, because hashing a connector folder per delivery
would put unbounded work on a route strangers can call.

The three ops: `read` returns
`{events: [{seq, received_at, body}], cursor, truncated}` without moving
anything, `ack` takes `up_to_seq` and moves a named consumer's cursor forward
only, and `peek_depth` returns `{pending}`. Delivery is at-least-once with an
explicit acknowledgement, because a reader that stops mid-thought must not
lose what it was holding. A read takes an optional `consumer` (default
`default`) and `limit` (default 50, capped at 500); different consumers keep
independent cursors over the same events.

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
it. Neither stops a message from steering the agent that reads it.

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
Deliveries only ever resolve against accounts defined in the global
`connector-config`, never a project-scoped account, because the hook path
carries no workspace segment.

To run the listener, see docs/DEPLOYMENT.md. `apb connector doctor` prints
the exact callback URL per account, the pending depth, and whether the local
listener answers.

## What a call returns

A call answers with one JSON document. On success it carries `ok: true`, the
HTTP `status`, and the `body` (parsed when the service sent JSON, a string
otherwise). A body over the 1 MiB read cap fails a `read_only` function as
`incomplete_response` (see below); on any other function the body is cut at
the cap, `truncated` is `true`, and the result says the response was not
complete. When the function declares a `response_pick` projection, `picked`
is `true` and `body` holds only the projected fields; `--full` skips the
projection and returns the raw body. A paging `Link` header from the service
surfaces verbatim as `link`.

On failure it carries `ok: false` and an `error` object with a `code`, a
human-readable `message`, and, when they apply, `http_status` and
`retry_after_sec`. The codes: `config` (a broken or drifted connector setup),
`permission` (no grant, no account, or the `max_calls` budget spent),
`invalid_args`, `auth` (the service rejected the credentials), `not_found`,
`rate_limited` (with `retry_after_sec` when the service gave one), `service`
(any other non-2xx, a redirect, or an `error_when` reclassification inside a
2xx), `network`, `timeout`, and `incomplete_response`. A function that is sent
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
the same name on every attempt. The connector block of the agent's prompt
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
      mode: key                 # key | marker | none
      field: nonce              # or `header: Idempotency-Key`, exactly one
      window_sec: 120           # how long the service honours the key
      result_ref: id            # optional: the created item's id or link
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

`idempotency` is allowed only on HTTP, mock and smtp functions that are not
`read_only` (an smtp function may declare only `mode: none`), and the
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
functions declares `none` or `key` (V69), and `resend` on a binding that
grants an `irreversible` function (V84).

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

## The `apb connector` CLI

```text
apb connector list              installed connectors, trust and config status
apb connector show <name>       manifest summary and per-account status
apb connector call <name> <fn>  the agent-facing call channel (--account, --args, --dry-run, --intent)
apb connector approve <name>    approve a connector or (--account) an account digest
apb connector doctor            check every connector: manifest, config, env, trust, healthcheck
apb connector env [<name>]      unresolved env var names as ready-to-paste KEY= lines
apb connector init <name>       scaffold a new connector folder from a template
```

`apb connector call` needs a run context (`APB_RUN_DIR` and `APB_NODE_ID`, set by
the engine when a node executes a call). Outside a run, `--dry-run` renders a
call without executing it, against the live connector and account config (no
secret is resolved, so no approval is needed); probe an account with the
dashboard healthcheck.
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

## Official connectors

Fifteen official connectors ship inside the `apb` binary and install with
`apb connector install <name>`: `github`, `telegram`, `smtp`, `sentry`,
`asana`, `imap`, `gitlab`, `youtrack`, `zulip`, `discord`, `slack`, `atrip`,
`twenty`, `whatsapp`, `amocrm`. Installing
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
`get_rate_limit`. `create_issue` and `comment_issue` are sent once in
`marker` mode: the lookup reads the token's account (`get_authenticated_user`,
`GET /user`) and the recent issues (`list_recent_issues`) or comments
(`list_issue_comments`). A GitHub App installation token cannot read `/user`,
so with one an unknown send stays unknown until a person decides.
`create_release` declares `none`: a duplicate tag is refused by GitHub, but
nothing reads back which intent created a release.

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
`remove_labels` are comma-separated deltas). Agent-written issue and merge
request comment bodies and descriptions are sent as `markdown_no_commands`
(see `text_args` above), so that text can never run a quick action. Titles
and release names and descriptions are sent as written: GitLab runs no quick
action from them, and it stores a title as given, so a `literal` escape would
stay in it as a visible backslash. `trigger_pipeline`
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
account fields above. Covers send (`send_text`, `send_template`,
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
