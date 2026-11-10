# google-drive: installation instructions for an agent

You are setting up the apb `google-drive` connector so playbooks can search and read the user's Google Drive. Work through the steps in order. You need two things from the user: the client secret of an OAuth client they create, and one consent they give themselves, in their own terminal and browser. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the client secret or the refresh token back to them, and do not put either in a commit, a log, a summary, or any file other than the secrets dotenv named below.

Never run `apb connector oauth` yourself. It is the moment a person grants access to their account, and apb refuses it inside a run step and without a terminal. You hand the user the command and wait.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `google-drive` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install google-drive
```

This copies the embedded connector into `<config-dir>/connectors/google-drive` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve google-drive`. It is local: no network call, nothing published.

If it refuses because a differing `google-drive` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: walk the user through the Google Cloud setup, and be honest about what it grants

Give the user this sequence in the [Google Cloud console](https://console.cloud.google.com/). Screen names move around; describe the goal of each step so they can find it.

1. Create a project, or pick an existing one they own.
2. Enable the Google Drive API for that project (APIs and Services, Library, Google Drive API, Enable).
3. Configure the OAuth consent screen (Google Auth Platform): an app name and a support e-mail; audience External for a personal Google account (Internal is only offered inside a Google Workspace organization); add their own Google account as a test user.
4. Create an OAuth client of type Desktop app, and copy its client id and client secret.

State these points plainly before the user approves anything:

- The connector asks for one scope, `https://www.googleapis.com/auth/drive.readonly`: read every file the account can open, including files shared with it and shared drives. No function of this connector writes, but the scope itself is wide, so the way to limit a playbook node is the grant's function list.
- Google classes `drive.readonly` as a restricted scope. An app the user creates for their own account works without verification; the consent then shows a warning that Google has not verified the app, and they continue through Advanced. That is expected for their own app.
- While the consent screen's publishing status is Testing, only the listed test users can sign in, and every refresh token expires after 7 days. For a setup that lasts, they can publish the app to production (it stays unverified, with the same warning) before the consent in step 6.

If the user already has a Desktop app client from setting up `google-calendar` or `gmail`, it can serve this connector too: enable the Drive API in the same project and reuse the client.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/google-drive.yaml`: one Google account the user wants available from every project.
- project, `<project>/.apb/connector-config/google-drive.yaml`: a Google account that belongs to this project.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: default
    default: true
    api_base: https://www.googleapis.com/drive/v3
    token_url: https://oauth2.googleapis.com/token
    client_id: 1234567890-abc123.apps.googleusercontent.com
    client_secret: "{{env.GOOGLE_CLIENT_SECRET}}"
    refresh_token: "{{env.GOOGLE_DRIVE_REFRESH_TOKEN}}"
```

`client_id` is not a secret and goes into this file as is. The `client_secret` and `refresh_token` fields must each hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

An optional `granted_scopes` field (space-separated) tells apb which scopes the refresh token carries, so a function whose scope is missing fails before any request. Leave it out unless the user wants it: without it, apb checks the scopes the token endpoint reports instead.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the client secret

```sh
apb connector env google-drive --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for `GOOGLE_CLIENT_SECRET`. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that they can reset the secret of the OAuth client in the Google Cloud console if that matters to them.

Leave `GOOGLE_DRIVE_REFRESH_TOKEN` empty for now: the consent in step 6 produces it.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve google-drive --account <name>
```

Account trust pins the account's non-secret fields (`api_base`, `token_url`, `client_id`, and `granted_scopes` when set), which is what decides where the secrets get sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that `api_base` and `token_url` are the Google ones you wrote before confirming. The consent in step 6 refuses an account that is not approved.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `google-drive` should be clean. It counts a key that is present in the secrets file as resolved even while its value is empty, so it cannot tell you that `GOOGLE_DRIVE_REFRESH_TOKEN` is still missing; step 6 fills it. This command makes no network call.

## Step 6: hand the consent to the user

Give the user this command to run in their own terminal, from the project root, and wait for them:

```sh
apb connector oauth google-drive --account <name>
```

What happens, so you can tell them in advance:

- The command prints a link to Google's consent page and waits up to 300 seconds (`--timeout-sec` changes that) for the browser to come back to `http://127.0.0.1:<port>/` on their machine.
- They open the link, sign in with the Google account whose Drive the playbooks should read, continue past the unverified-app warning through Advanced, and approve read access to Drive.
- The command prints the refresh token once, alone on one line, then the granted scopes and where to store it. apb keeps no copy.
- They put the token into `GOOGLE_DRIVE_REFRESH_TOKEN` in the secrets file of step 4 themselves. If they paste it to you instead, write it into that file without echoing it, and tell them the transcript now contains it.

Wait until they say it is stored. The healthcheck in step 7 is the first check that sees whether the token works.

If the command ends with "the provider returned no refresh token", the account already granted this client before: the user removes the app's access in their Google account settings (Security, third-party access) and runs the command again.

Each consent issues a new refresh token. Google keeps a limited number of live refresh tokens per account and client (about 100) and silently retires the oldest beyond that, so repeated consents are not free.

## Step 7: verify against the real API

The declared `healthcheck` is `verify`: it asks Google which account the token belongs to, with zero arguments, and reads nothing from Drive itself.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/google-drive/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body. On success the body names the Google account (`user.emailAddress`); check it is the one the user meant.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- `invalid_grant`: the refresh token was revoked or has expired. In Testing status that happens after 7 days; it also happens when the user removed the app's access or ran the consent so often that this token was retired. The user runs step 6 again and stores the new token.
- `invalid_client`: the `client_id` or the client secret is wrong, or they belong to different clients.
- `needs one of the OAuth scopes`: the token was granted without Drive read access. The user runs step 6 again and approves the Drive access on the consent page.
- `auth` with HTTP 403: usually the Google Drive API is not enabled in the project of the client, or was enabled only a minute ago. Enable it (step 2) and probe again.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 8: report

Tell the user, briefly:

- which account name you created, and in which scope, and which Google account the healthcheck named;
- which file holds the client secret and the refresh token, and which key names;
- the `verify` healthcheck result;
- that the token can read every file the account can open, shared drives included, and that no function of the connector writes, so the way to limit a node is the grant's function list;
- that while the consent screen stays in Testing the refresh token expires after 7 days, and how to renew it (step 6 again);
- that apb keeps no token on disk: the access token lives in the calling process's memory only;
- that Google files (Docs, Sheets, Slides) are read with `export_text` and stored text files with `read_text_file`, that an answer over 1 MiB fails `incomplete_response` instead of returning cut text, and that binary files and uploads are out of scope.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
