# google-calendar: installation instructions for an agent

You are setting up the apb `google-calendar` connector so playbooks can read the user's Google Calendar and create, change or delete events in it. Work through the steps in order. What you need from the user: the clicks in the Google Cloud console that create an OAuth client, its client secret, one consent that the user runs in their own terminal, and one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the client secret or the refresh token back to them, and do not put either in a commit, a log, a summary, or any file other than the secrets dotenv named below. Never run `apb connector oauth` yourself: it is the user's command, and it prints a secret meant for them alone.

Write nothing to the calendar during setup. The healthcheck in step 7 reads the primary calendar's entry and changes nothing. Do not create a test event: an event with guests mails them, a mail cannot be called back, and the user did not ask for one.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `google-calendar` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install google-calendar
```

This copies the embedded connector into `<config-dir>/connectors/google-calendar` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve google-calendar`. It is local: no network call, nothing published.

If it refuses because a differing `google-calendar` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: walk the user through the Google Cloud console

The user does this part with their own Google account at [console.cloud.google.com](https://console.cloud.google.com/); you cannot do it for them. Give them the sequence:

1. Create a project, or pick one of theirs.
2. Enable the Google Calendar API for it: APIs and Services, Library, Google Calendar API, Enable.
3. Configure the OAuth consent screen (in newer consoles, Google Auth Platform): user type External (Internal exists only inside a Google Workspace organization, and is fine there), an app name and a support e-mail. If the console has a Data Access page, add the two scopes the connector asks for there: `https://www.googleapis.com/auth/calendar.events` and `https://www.googleapis.com/auth/calendar.readonly`.
4. Decide the publishing status, and tell the user what it means before they choose. In Testing status they must add their own Google account as a test user (Audience, Test users), and Google issues refresh tokens that expire after 7 days: the account stops working a week after each consent, until step 6 is repeated. Published (In production), the token lasts until it is revoked; an app Google has not verified then shows a warning screen at consent, which the user may pass for an app they made for their own account.
5. Create an OAuth client: Credentials (or Clients), Create client, application type Desktop app, any name. Copy the client id, which ends in `.apps.googleusercontent.com` and is not a secret, and keep the client secret for step 4. A Desktop app client accepts the loopback redirect the consent in step 6 uses; another client type fails there with `redirect_uri_mismatch`.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/google-calendar.yaml`: a calendar the user wants available from every project.
- project, `<project>/.apb/connector-config/google-calendar.yaml`: a calendar that belongs to this project's routines.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: me
    default: true
    api_base: https://www.googleapis.com/calendar/v3
    token_url: https://oauth2.googleapis.com/token
    client_id: 1234567890-abc123.apps.googleusercontent.com
    client_secret: "{{env.GOOGLE_CALENDAR_CLIENT_SECRET}}"
    refresh_token: "{{env.GOOGLE_CALENDAR_REFRESH_TOKEN}}"
```

`api_base` and `token_url` are Google's fixed endpoints; write them exactly as above. `client_id` is the value from step 2. The `client_secret` and `refresh_token` fields must each hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

An optional field `granted_scopes` (space-separated) states the scopes the refresh token carries. Leave it out: apb then checks the scope list Google returns with each access token, which is always current.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`. A second Google account (work and personal) gets its own entry with its own refresh token variable; the same OAuth client can serve both.

## Step 4: prepare the secrets file, then ask for the client secret

```sh
apb connector env google-calendar --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, here `GOOGLE_CALENDAR_CLIENT_SECRET` and `GOOGLE_CALENDAR_REFRESH_TOKEN`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the client secret. Prefer that they fill the value of `GOOGLE_CALENDAR_CLIENT_SECRET` in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that they can reset the secret of the client in the Google Cloud console if that matters to them.

`GOOGLE_CALENDAR_REFRESH_TOKEN` stays empty for now: step 6 produces it.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve google-calendar --account <name>
```

Account trust pins the account's non-secret fields, which decide where the secrets get sent: `token_url` receives the client secret and the refresh token, `api_base` the access token. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that `token_url` is `https://oauth2.googleapis.com/token` and `api_base` is `https://www.googleapis.com/calendar/v3` before confirming.

Then check the picture so far:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `google-calendar` should be clean. This command makes no network call and does not tell an empty value from a real one: `GOOGLE_CALENDAR_REFRESH_TOKEN` has no value until step 6, so a clean report is necessary but not sufficient.

## Step 6: the consent, which the user runs

The consent needs a browser on this machine and the user at it: apb listens on `127.0.0.1` for Google's answer. The command is refused inside a run step and without a terminal, and it prints the refresh token once, for the user. So do not run it; ask the user to run it in their own terminal, from the project root when the account is a project account:

```sh
apb connector oauth google-calendar --account <name>
```

It prints a URL. The user opens it in a browser on the same machine, signs in with the Google account whose calendar the playbooks should use (in Testing status, one of the test users), and approves the access. The terminal then prints the refresh token alone on one line, the scopes Google granted, and where to store the token. The user pastes it into `GOOGLE_CALENDAR_REFRESH_TOKEN` in the secrets file themselves. apb keeps no copy.

Two options, when the user asks for them: `--scope <scope>`, repeatable, asks for other scopes than the two defaults (only `https://www.googleapis.com/auth/calendar.readonly` for a read-only setup, in which case every write function fails before any request, with a message naming the scope it needs); `--timeout-sec <n>` waits longer than the default 300 seconds.

What a failure means:

- `access_denied`: the user declined, or the account is not a test user of an app in Testing status.
- `redirect_uri_mismatch` on Google's page: the OAuth client is not of type Desktop app.
- "the provider returned no refresh token": Google still holds an earlier grant for this client. The user removes the app's access in their Google account (Security, third-party access), then runs the command again.
- a timeout: nobody completed the consent within the wait; run it again.

## Step 7: verify against the real API

The declared `healthcheck` is `verify`. It reads the primary calendar's entry and changes nothing; its `id` is the e-mail address of the Google account the token belongs to, which confirms the user consented with the intended account.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/google-calendar/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- `invalid_grant`: the refresh token was revoked, expired (7 days after the consent while the app is in Testing status), or pasted incompletely. The message names the command to run again; that is step 6.
- `invalid_client`: the client id or the client secret is wrong, or the client was deleted.
- a scope error naming `apb connector oauth ... --scope`: the consent granted fewer scopes than the function needs. Repeat step 6 with the scopes it names.
- `auth` with HTTP 403 right after a working token exchange: usually the Google Calendar API is not enabled in the project (step 2).
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 8: report

Tell the user, briefly:

- which account name you created, in which scope, and which Google account it reads (the healthcheck's `id`);
- which file holds the client secret and the refresh token, and their key names;
- the healthcheck result, and that nothing was written to the calendar;
- if the app stays in Testing status, that the refresh token expires 7 days after the consent and step 6 must then be repeated, and that publishing the app avoids this;
- that `create_event`, `update_event` and `delete_event` are irreversible: Google can e-mail every guest per `send_updates`, a mail cannot be called back, a deleted event cannot be restored through the connector, and a `human_review` gate in front of a node granted them cannot be decided automatically;
- that leaving `send_updates` out does not guarantee that no e-mail goes out, and that `none` can keep an event from reaching some guests;
- that a retried `create_event` never creates the event twice, while an `update_event` or `delete_event` whose answer was lost is refused on repeat until a person decides with `apb intent resolve`;
- that `query_free_busy` returns busy intervals, and the agent computes the free slots with the task's own rules;
- that a reading step is best granted `functions: read_only`, and every binding marked `untrusted: true`, since event text is written by other people.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
