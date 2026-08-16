# whatsapp: installation instructions for an agent

You are setting up the apb `whatsapp` connector. Work through the steps in
order. What you need from the user is the phone number id, WABA id, and
access token from a Meta app they control, plus (only if they want to
receive messages) an app secret and a verify token, and one confirmation
about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the access token,
app secret, or verify token back to the user in chat, and do not put any of
them in a commit, a log, a summary, or any file other than the secrets
dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the
embedded ones available to install. If `whatsapp` already appears in the
installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in
it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else
`$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install whatsapp
```

This copies the embedded connector into `<config-dir>/connectors/whatsapp`
and records trust for its tree digest in the same action, so you do not need
a separate `apb connector approve whatsapp`. It is local: no network call,
nothing published.

If it refuses because a differing `whatsapp` is already installed, do not
reach for `--force` on your own. Report what is installed and ask the user
whether to replace it.

## Step 2: gather the fields, and be honest about what the token grants

Seven account fields, three secret. `base_url` defaults to
`https://graph.facebook.com` and normally does not need to be asked for.
`graph_version` defaults in practice to the current Graph API version
(`v23.0` at the time of writing); ask only if the user has a reason to pin a
different one, since Meta supports each version for about two years before
retiring it. The fields you actually need from the user are `phone_number_id`,
`waba_id`, `access_token`, and, only for receiving, `app_secret` and
`verify_token`.

Ask the user to open their Meta app (Meta for Developers, with the WhatsApp
product added) and get:

- `phone_number_id` and `waba_id` from WhatsApp > API Setup in the app
  console.
- `access_token`: a System User permanent token created in Meta Business
  Suite, granted the `whatsapp_business_messaging` and
  `whatsapp_business_management` permissions, and set to Never expire. State
  this plainly before the user generates anything: an expiring token turns
  every send into a silent authentication failure once it lapses, and there
  is no separate scope selection beyond the two permissions granted, so a
  token that can send can also manage templates and the business profile;
  the way to limit a node is the connector grant's function list, not the
  token itself.
- if the user wants to receive messages: `app_secret` (Meta app > Settings >
  Basic) and a `verify_token` of the user's own choosing, any string they
  pick. Do not generate this value yourself unless the user asks you to; it
  is theirs to remember, since they will paste it into the Meta console
  later in this runbook.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/whatsapp.yaml`: an identity the
  user wants available from every project.
- project, `<project>/.apb/connector-config/whatsapp.yaml`: an identity that
  belongs to this project.

Ask which one applies. When both exist, the merged list is global plus
project, and a project account replaces a global one of the same name.

If the user wants to receive messages, prefer the global location: a hook
URL carries no workspace segment, so only a globally configured account can
ever receive a delivery (an account defined only in a project's
connector-config is invisible to the ingest listener, and step 6's runbook
covers how to confirm this with `apb connector doctor`).

```yaml
accounts:
  - name: default
    default: true
    base_url: https://graph.facebook.com
    graph_version: v23.0
    phone_number_id: "123456789012345"
    waba_id: "234567890123456"
    access_token: "{{env.WHATSAPP_ACCESS_TOKEN}}"
    app_secret: "{{env.WHATSAPP_APP_SECRET}}"
    verify_token: "{{env.WHATSAPP_VERIFY_TOKEN}}"
```

The `access_token`, `app_secret`, and `verify_token` fields must each hold
exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal
secret in this file is a validation error and the call will be refused, so
do not put one there even temporarily. Omit `app_secret` and `verify_token`
entirely on a send-only account. This config file is non-secret by design
and safe to commit.

If you are editing a file that already has accounts, add yours to the list
and leave the others alone. At most one account in the merged list may carry
`default: true`. For a second WhatsApp Business Account or phone number, name
it distinctly; do not reuse an existing account name for it.

## Step 4: prepare the secrets file, then ask for the values

```sh
apb connector env whatsapp --write
```

Run this from the project root. It appends a `KEY=` template line for every
unresolved env var to `<project>/.apb/secrets.env`, creates that file with
owner-only permissions when it is absent, never duplicates a key that is
already there, and makes sure `.gitignore` covers it. Values are left empty
on purpose.

Now ask the user for `WHATSAPP_ACCESS_TOKEN`, and, if they want to receive
messages, `WHATSAPP_APP_SECRET` and `WHATSAPP_VERIFY_TOKEN`. Prefer that they
fill the values in themselves: give them the exact file path and the key
names, and wait. That keeps the secrets out of the conversation transcript
entirely. If they hand a value to you in chat instead, write it into that
file without echoing it back, and tell them plainly that the transcript now
contains it and that the access token can be revoked and a new one created
in Meta Business Suite if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the
project file. Use it when the account is global and the user does not want a
project-local secret. Only reach for a project `secrets.env` you created by
hand if `apb connector env --write` was not used, and in that case verify
`.gitignore` coverage yourself: an uncommitted secret is one `git add -A`
away from a public repository.

The resolution order at call time is process environment, then the project
dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve whatsapp --account <name>
```

Account trust pins the account's non-secret fields (`base_url`,
`graph_version`, `phone_number_id`, `waba_id`), which is what decides where
the access token gets sent and which resources it addresses. It is
deliberately separate from connector trust and is never bypassed by a run.
The command prints the concrete field values it is approving, which is the
moment to see exactly where the token will be sent; check that `base_url`,
`phone_number_id`, and `waba_id` are the ones you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every
connector and account. Every check for `whatsapp` should be clean at this
point except the ingest-related callback checks, which stay unresolved until
step 7 if the user wants to receive. This command makes no network call, so
a clean report is necessary but not sufficient.

## Step 6: verify sending against the real API

The declared `healthcheck` is `get_phone_number`: it renders with zero
arguments (its `fields` query is a fixed literal) and returns the account's
`verified_name` and `quality_rating`, so it doubles as both the reachability
probe and a meaningful live verification that `access_token` is valid and
that `phone_number_id` actually belongs to it.

`apb connector call` cannot be used here. It requires a run context
(`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating
one is not an acceptable substitute. Use the dashboard's healthcheck endpoint
instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321
unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the
project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/whatsapp/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the
workspace resolves, the answer is HTTP 200 with the outcome in the body's
`ok` and `error` fields. A refusal or a failure is reported there, not as an
HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the
  config, or you wrote the config into a scope this workspace does not see.
- 401 with an OAuth error about the token: an expired, revoked, or mistyped
  access token, or a token missing the `whatsapp_business_management`
  permission needed to read the phone number's own record.
- unresolved env var: the secrets file has the key but no value, or the key
  name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after
  approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

If the user only wants to send, stop here and go to step 8.

## Step 7: the receiving-half runbook

Skip this step entirely for a send-only account.

Enable the ingest listener in the global config
(`<config-dir>/config.yaml`):

```yaml
ingest:
  enabled: true
  bind: "127.0.0.1"
  port: 7322
  public_base_url: https://hooks.example.com
```

`public_base_url` must be the public HTTPS host a reverse proxy fronts the
ingest listener with; `docs/DEPLOYMENT.md` in the apb repository covers the
proxy setup in full. `apb dashboard` co-starts the listener when `enabled` is
true; on a machine that runs no dashboard, run `apb ingest` instead.

Re-run `apb connector doctor` and read the callback line it prints for this
account:

```sh
apb connector doctor
```

A clean check reports `register this URL with the provider: <url>`; the URL
has the shape `<public_base_url>/hooks/whatsapp/<account>`. A warning instead
of that line means one of two things: `ingest.public_base_url` is not set
yet, or this account exists only in a project connector-config and is
therefore unaddressable (move it to the global connector-config, per step 3).

Take the printed URL and the `verify_token` value from step 2 into the Meta
app console: WhatsApp > Configuration > Webhooks, set the Callback URL and
Verify Token fields to those two values, and save. Meta immediately issues a
GET request against the callback URL with `hub.mode=subscribe`,
`hub.verify_token`, and `hub.challenge` query parameters; apb answers by
echoing `hub.challenge` back when the token matches, which is what makes the
save in the Meta console succeed. If the save fails, re-check that
`verify_token` in the account config matches exactly what was typed into the
console field, with no surrounding whitespace.

Once saved, subscribe the app to the `messages` field under the same
Webhooks page (the field list also offers message status and template
status events; subscribe to those too if the demo playbook or the user's own
playbook is meant to observe them, but `messages` alone is enough for the
inbox to receive inbound text).

Confirm receiving end to end only if the user can send a WhatsApp message to
the configured `phone_number_id` from a phone: ask them to send one, then
poll the inbox with `apb connector call` from inside a real run, or ask the
user to check back after a playbook using `inbox_read` runs. Do not fabricate
a run context to call `inbox_read` outside one.

## Step 8: report

Tell the user, briefly:

- which account name you created, and in which scope, and which
  `phone_number_id` it targets;
- which file holds the secrets and which key names;
- the `get_phone_number` healthcheck result;
- that the access token's permissions follow the two System User scopes it
  was granted, with no separate scope selection, so the way to limit a node
  is the connector grant's function list;
- if receiving was set up: the callback URL registered, and that a delivery
  arriving while the ingest listener is down is lost after Meta's own retry
  window elapses, with no way to ask for redelivery later;
- that `delete_template` and `delete_media` are hard, unrecoverable deletes
  with no restore, and that a plain `send_text` outside the 24-hour
  customer-service window fails with error 131047 and needs `send_template`
  instead.

Do not offer to run a playbook, and do not start one. Binding this connector
to a node is a separate decision that belongs to the user.
