# discord: installation instructions for an agent

You are setting up the apb `discord` connector so playbooks can read and post in the user's Discord server. Work through the steps in order. The only thing you need from the user is a bot token, plus one action only they can perform: inviting the bot to the server with the right permissions.

Report progress in the user's chat language. Do not print the token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

Post nothing during setup beyond the healthcheck in step 6, which identifies the bot and writes to no channel. Do not post a test message to prove it works: a channel post is visible to everyone in it and cannot be recalled, and the user did not ask for one.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `discord` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install discord
```

This copies the embedded connector into `<config-dir>/connectors/discord` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve discord`. It is local: no network call, nothing published.

If it refuses because a differing `discord` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: walk the user through the bot and its invite

`api_base` is `https://discord.com/api/v10`.

The credential is a bot token, which requires an application. Give the user the sequence: open the Discord Developer Portal, create an application, open its Bot tab, and create or reset the token there. Resetting invalidates the previous token, so do not suggest a reset for a bot other tooling may be using.

Then the bot has to be invited to the server with an OAuth2 URL carrying the bot scope plus the permissions the playbooks need:

- View Channels: required for everything.
- Read Message History: `get_messages`.
- Send Messages: `send_message` and `reply_to_message`.

Ask what the playbooks should do and name only the permissions that follow. Tell the user that the message-content privileged intent is not needed here: this connector is REST-only, and that intent governs the gateway.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/discord.yaml`: one bot the user uses everywhere.
- project, `<project>/.apb/connector-config/discord.yaml`: a bot tied to this project.

Ask which one applies. Recommend project scope when the bot was created for this project. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: bot
    default: true
    api_base: https://discord.com/api/v10
    token: "{{env.DISCORD_BOT_TOKEN}}"
```

The `token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the token

```sh
apb connector env discord --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the bot token. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be reset in the Developer Portal if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve discord --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the bot token will be sent; check that `api_base` is the one you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `discord` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `get_me` healthcheck: it asks Discord to identify the bot and writes to no channel.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -s -X POST "http://127.0.0.1:7321/api/connectors/discord/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401: a mistyped token, or one that was reset in the Developer Portal after you stored it.
- 403 on a channel call while the healthcheck passes: the bot was invited without the permission that call needs, or not invited to that server at all.
- 429: rate limited. Discord throttles per route and aggressively; this is a signal about call volume, not about the setup.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: tell the user the one thing left to them

A passing healthcheck only proves the token is valid. The bot still has to be invited to the server, with the permissions from step 2, before any channel call works. Only the user can do that, through the OAuth2 invite URL.

Guild and channel ids are call arguments, not account fields, so nothing about this lives in the config.

## Step 8: report

Tell the user, briefly:

- which account name you created, and in which scope;
- which file holds the token and which key;
- the healthcheck result, and that no channel was posted to;
- that they still have to invite the bot with the permissions the playbooks need;
- that threads are channels, so reading or posting in a thread means passing the thread's channel id;
- that `get_messages` pages backward with optional `limit` (1 to 100) and `before` (a message id);
- that Discord rate limits per route and aggressively, so tight polling loops should be avoided and grants should carry a `max_calls` cap;
- that `send_message` and `reply_to_message` are separate functions, so a grant can allow replies without allowing new top-level posts, and that a channel post cannot be recalled.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
