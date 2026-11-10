# gmail: installation instructions for an agent

You are setting up the apb `gmail` connector. Work through the steps in
order. The only things you need from the user are a Google Cloud project
they control (or the client id and client secret of an existing one) and,
at the consent step, a browser. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the client
secret, the refresh token, or any access token back to them, and do not put
any of them in a commit, a log, a summary, or any file other than the
secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the
embedded ones available to install. If `gmail` already appears in the
installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in
it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else
`$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install gmail
```

This copies the embedded connector into `<config-dir>/connectors/gmail`
and records trust for its tree digest in the same action. It is local: no
network call, nothing published.

If it refuses because a differing `gmail` is already installed, do not
reach for `--force` on your own. Report what is installed and ask the user
whether to replace it.

## Step 2: create the Google OAuth client

The connector mints its access tokens at call time from a refresh token,
so the account needs an OAuth client of type Desktop app plus a refresh
token consented by a human. If the user already has a client that serves
the apb `google-calendar` or `google-drive` connector, reuse it: one
Google client may serve all three Google connectors, and one refresh token
can carry the scopes of all of them when `apb connector oauth` gets every
`--scope`. Skip to step 3 in that case.

Otherwise, in the Google Cloud console (console.cloud.google.com):

1. Pick or create the project.
2. Under APIs and Services, Library, enable the Gmail API.
3. Under APIs and Services, OAuth consent screen, configure the app. For
   an internal or testing app limited to the user's own account, the
   unverified-app warning during consent is expected; say so before the
   user clicks through it.
4. Under Credentials, Create credentials, OAuth client ID, Application
   type Desktop app. Copy the client id (it ends in
   `.apps.googleusercontent.com`) and the client secret.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/gmail.yaml`: an identity the user
  wants available from every project.
- project, `<project>/.apb/connector-config/gmail.yaml`: an identity that
  belongs to this project.

Ask which one applies. When both exist, the merged list is global plus
project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: default
    default: true
    api_base: https://gmail.googleapis.com/gmail/v1
    token_url: https://oauth2.googleapis.com/token
    client_id: "1234567890.apps.googleusercontent.com"
    client_secret: "{{env.GMAIL_CLIENT_SECRET}}"
    refresh_token: "{{env.GMAIL_REFRESH_TOKEN}}"
    email: agent@example.com
```

`email` is the mailbox the connector reads and the From address of every
draft; it must be the Google account that consents in step 4. Leave
`granted_scopes` out here; step 4 fills it in.

The `client_secret` and `refresh_token` fields must each hold exactly one
reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in
this file is a validation error and the call will be refused, so do not put
one there even temporarily. This config file is non-secret by design and
safe to commit.

If you are editing a file that already has accounts, add yours to the list
and leave the others alone. At most one account in the merged list may
carry `default: true`. For a second mailbox, name the account distinctly
and set its `email` to that address; do not reuse an existing account name
for it.

## Step 4: run the consent flow, then prepare the secrets file

```sh
apb connector env gmail --write
```

Run this from the project root. It appends a `KEY=` template line for every
unresolved env var to `<project>/.apb/secrets.env`, creates that file with
owner-only permissions when it is absent, never duplicates a key that is
already there, and makes sure `.gitignore` covers it. Values are left empty
on purpose.

Then run the OAuth consent loop. It prints an authorization URL, the user
opens it in a browser and consents, Google calls back on 127.0.0.1, and the
command prints the refresh token and the granted scopes.

```sh
apb connector oauth gmail
```

Without `--scope` it asks for the connector's `consent_scopes`, which here
is `https://www.googleapis.com/auth/gmail.modify` (reads, drafts, labels
and send in one scope). To serve several Google connectors with one refresh
token, pass every scope in one run:

```sh
apb connector oauth gmail \
  --scope https://www.googleapis.com/auth/gmail.modify \
  --scope https://www.googleapis.com/auth/calendar.events \
  --scope https://www.googleapis.com/auth/calendar.readonly \
  --scope https://www.googleapis.com/auth/drive.readonly
```

(Use the calendar and drive scopes only if those connectors are being set
up too; the same refresh token then goes into all three accounts.) Google
rotates refresh tokens: the printed `granted_scopes` line goes verbatim
into the account's `granted_scopes` field, so the engine can refuse a call
whose scope the account never granted before any request leaves.

Ask the user to fill the values into `<project>/.apb/secrets.env`:

- `GMAIL_CLIENT_SECRET`: the client secret from step 2,
- `GMAIL_REFRESH_TOKEN`: the refresh token the consent loop printed.

Prefer that they fill the values in themselves: give them the exact file
path and the key names, and wait. That keeps the secrets out of the
conversation transcript entirely. If they hand them over in chat instead,
write them into that file without echoing them back, and tell them plainly
that the transcript now contains them and that the refresh token can be
revoked from the Google account's third-party access page if that matters
to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after
the project file. Use it when the account is global and the user does not
want a project-local secret.

## Step 5: approve the account

```sh
apb connector approve gmail --account <name>
```

Account trust pins the account's non-secret fields (`api_base`,
`token_url`, `client_id`, `email`), which is what decides where a token
request and a draft go. It is deliberately separate from connector trust
and is never bypassed by a run. The command prints the concrete field
values it is approving; check that `api_base` and `email` are the ones you
wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every
connector and account. Every check for `gmail` should be clean. This
command makes no network call, so a clean report is necessary but not
sufficient.

## Step 6: verify against the real API

The declared `healthcheck` is `verify`, which reads the mailbox profile
(address and message count) and succeeds against any consented account.

`apb connector call` cannot be used here. It requires a run context
(`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating
one is not an acceptable substitute. Use the dashboard's healthcheck
endpoint instead, which runs the same execution path outside a run.

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
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/gmail/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the
workspace resolves, the answer is HTTP 200 with the outcome in the body's
`ok` and `error` fields. A refusal or a failure is reported there, not as
an HTTP status, so read the body.

Common failures and what they actually mean:

- 401 with `invalid_grant`: the refresh token was revoked, expired, or
  belongs to another Google account than `email`; run step 4 again.
- 403 with a `missing scope` message: the refresh token was consented for
  narrower scopes than the function needs; run step 4 again with the
  scopes the grant requires.
- unresolved env var: the secrets file has the key but no value, or the
  key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after
  approval, which drops it.

Do not paper over a failure. Report the exact message and what it points
at.

## Step 7: report

Tell the user, briefly:

- which account name you created, in which scope (global or project), and
  which mailbox (`email`) it reads;
- which file holds the client secret and the refresh token, and which key
  names;
- the `verify` healthcheck result;
- which grant preset the intended playbooks need (see the connector's
  README), and that `send_draft` is the one irreversible function, best
  put behind a `human_review` gate;
- that Google may rotate the refresh token; when a call warns about a
  rotated token, run step 4 again and update the stored value;
- that the access token is minted per call, kept in memory only, and
  redacted from results, the journal and logs.

Do not offer to run a playbook, and do not start one. Binding this
connector to a node is a separate decision that belongs to the user.
