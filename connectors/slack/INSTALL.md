# slack: installation instructions for an agent

You are setting up the apb `slack` connector so playbooks can read and post in the user's Slack workspace. Work through the steps in order. The only thing you need from the user is a bot token, plus one action only they can perform: inviting the bot into the channels it should work in.

Report progress in the user's chat language. Do not print the token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

Post nothing during setup beyond the healthcheck in step 6, which identifies the bot and writes to no channel. Do not post a test message to prove it works: a channel post is visible to everyone in it and cannot be recalled, and the user did not ask for one.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `slack` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install slack
```

This copies the embedded connector into `<config-dir>/connectors/slack` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve slack`. It is local: no network call, nothing published.

If it refuses because a differing `slack` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: walk the user through the app and its scopes

`api_base` is `https://slack.com/api`.

The credential is a bot token (`xoxb-...`), which requires a Slack app. Give the user the sequence: open [api.slack.com/apps](https://api.slack.com/apps), create an app in their workspace, open OAuth and Permissions, add bot token scopes, install the app to the workspace, then copy the Bot User OAuth Token.

Ask what the playbooks should do, then name only the scopes that follow from the answer:

- `channels:read`: `list_channels`.
- `channels:history`: `get_messages` and `get_thread`.
- `chat:write`: `send_message` and `reply_in_thread`.
- the `groups:` twins of the read scopes (`groups:read`, `groups:history`): the same operations in private channels.

Two properties of Slack scopes matter here and are worth telling the user up front. Scopes are granular, so a missing one does not fail the healthcheck: the connector authenticates fine and then a specific function fails with `missing_scope`. And adding a scope later requires reinstalling the app to the workspace before it takes effect.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/slack.yaml`: one workspace the user works with everywhere.
- project, `<project>/.apb/connector-config/slack.yaml`: a workspace or bot tied to this project.

Ask which one applies. Recommend project scope when the app was created for this project's notifications. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: workspace
    default: true
    api_base: https://slack.com/api
    token: "{{env.SLACK_BOT_TOKEN}}"
```

The `token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the token

```sh
apb connector env slack --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the bot token. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be rotated in the app's OAuth settings if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve slack --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the bot token will be sent; check that `api_base` is the one you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `slack` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `auth_test` healthcheck. It is a POST by Slack's own API convention and mutates nothing: it reports which workspace and bot identity the token belongs to.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -s -X POST "http://127.0.0.1:7321/api/connectors/slack/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Note that Slack itself also answers HTTP 200 on failure, with `"ok": false` in its body. The connector's manifest declares an `error_when` rule that turns such a response into a real service error carrying Slack's own `error` string, so a failure surfaces as a failure and retries and fallbacks react to it. That means the error text you see is Slack's, and it is worth quoting verbatim.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- `invalid_auth`: a mistyped or rotated token.
- `not_authed`: an empty token, which usually means the env var resolved to nothing.
- `missing_scope`: the token works but lacks the scope for that function. The healthcheck will not catch this. Add the scope and reinstall the app.
- `not_in_channel`: the bot has not been invited to the channel.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: tell the user the one thing left to them

A passing healthcheck does not mean the bot can read or post anywhere. Slack refuses both in a channel the bot is not a member of, and only the user can fix that: run `/invite @your-app` in each channel the playbooks should work in.

Channel ids are call arguments, not account fields, so nothing about this lives in the config.

## Step 8: report

Tell the user, briefly:

- which account name you created, and in which scope;
- which file holds the token and which key, and which scopes the app has;
- the healthcheck result, and that no channel was posted to;
- that they still have to invite the bot into each channel with `/invite @your-app`;
- that a missing scope shows up only when a specific function is called, not at the healthcheck, and that adding one requires reinstalling the app;
- that list and history functions page with a cursor carried in the body: pass `response_metadata.next_cursor` back as the next call's `cursor`;
- that `send_message` and `reply_in_thread` are separate functions, so a grant can allow thread replies without allowing new top-level posts, and that a channel post cannot be recalled, which makes a `max_calls` cap worthwhile where a loop could reach it.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
