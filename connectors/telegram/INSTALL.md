# telegram: installation instructions for an agent

You are setting up the apb `telegram` connector so playbooks can message the user through a bot. Work through the steps in order. The only thing you need from the user is a bot token, plus one action only they can perform: introducing the bot to the chat it should write to.

Report progress in the user's chat language. Do not print the token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

Send nothing during setup beyond the healthcheck in step 6, which asks Telegram who the bot is and writes to no chat. Do not send a test message to prove it works: it lands on someone's phone, and the user did not ask for one.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `telegram` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install telegram
```

This copies the embedded connector into `<config-dir>/connectors/telegram` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve telegram`. It is local: no network call, nothing published.

If it refuses because a differing `telegram` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: get the bot token

`api_base` is `https://api.telegram.org`. Override it only for a self-hosted Bot API server, and only if the user says they run one.

The token comes from [@BotFather](https://t.me/BotFather). Walk the user through it if they have no bot yet: open BotFather in Telegram, send `/newbot`, choose a display name and a username ending in `bot`, and the reply contains the token. An existing bot's token can be re-read or regenerated with `/mybots`.

Ask whether the bot already exists before sending the user off to create one. Note that regenerating a token invalidates the old one, so do not suggest it as a troubleshooting step for a bot that other tooling may be using.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/telegram.yaml`: a personal notification bot the user wants available from every project.
- project, `<project>/.apb/connector-config/telegram.yaml`: a bot that belongs to this project, for example one posting into a project group.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: notifier
    default: true
    api_base: https://api.telegram.org
    token: "{{env.TELEGRAM_BOT_TOKEN}}"
```

Name the env var after the account when several bots may coexist. The `token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the token

```sh
apb connector env telegram --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the token. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be regenerated in BotFather if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve telegram --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the bot token will be sent; check that `api_base` is the one you wrote before confirming, since a self-hosted Bot API server is the only reason it would differ.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `telegram` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `get_me` healthcheck: it asks Telegram to identify the bot and writes to no chat.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/telegram/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401 unauthorized: a mistyped token, or a token that was regenerated in BotFather after you stored it.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: tell the user the one thing left to them

A passing healthcheck does not mean the bot can message anyone. Telegram refuses a message to a chat the bot has never been introduced to, and only the user can fix that:

- private chat: open the bot in Telegram and send it any message once.
- group: add the bot to the group as a member.

Chat ids are call arguments, not account fields, so nothing about this lives in the config. The playbook that sends will need the chat id, which `get_updates` reports once a message has reached the bot.

## Step 8: report

Tell the user, briefly:

- which account name you created, and in which scope;
- which file holds the token and which key;
- the healthcheck result, and that no chat was written to;
- that they still have to introduce the bot to the target chat, and how;
- that there is no webhook support, so reacting to a reply means polling `get_updates` from a node;
- that `send_message` reaches a real device and cannot be unsent, so a `max_calls` cap on the grant is worth setting where a loop could reach it.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
