# zulip: installation instructions for an agent

You are setting up the apb `zulip` connector so playbooks can read and post in the user's Zulip organization. Work through the steps in order. The only thing you need from the user is an API key and the matching email address, plus one confirmation about where the account should live.

Report progress in the user's chat language. Do not print the API key back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

Post nothing during setup beyond the healthcheck in step 6, which identifies the account and writes to no stream. Do not post a test message to prove it works: a stream message is visible to every subscriber and cannot be recalled, and the user did not ask for one.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `zulip` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install zulip
```

This copies the embedded connector into `<config-dir>/connectors/zulip` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve zulip`. It is local: no network call, nothing published.

If it refuses because a differing `zulip` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: settle the API base, the identity, and the key

`api_base` is `https://<org>.zulipchat.com/api/v1` on Zulip Cloud, or `https://<host>/api/v1` self-hosted. The `/api/v1` suffix is part of the base, not something the connector appends. Ask the user for their Zulip address and build the base from it rather than guessing the org name.

Then ask whether playbooks should post as the user or as a bot, and explain the difference rather than deciding silently: a bot posts under its own name and can be subscribed to only the streams it needs, while the user's own account posts as them and reaches everything they can reach. Recommend a bot for anything recurring.

Where the credential comes from depends on that answer:

- the user's own account: Zulip settings, Account and privacy, then Show API key. The `email` field is the user's own address.
- a bot: the bot panel in organization settings, which shows the bot's email and its API key together. The `email` field is the bot's address, which usually ends in `-bot@...`.

The `email` and the key have to belong to the same identity: authentication is HTTP Basic with the email as the username and the key as the password, so a mismatched pair fails as a bad credential rather than as a configuration error.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/zulip.yaml`: one organization the user works with everywhere.
- project, `<project>/.apb/connector-config/zulip.yaml`: an organization or bot tied to this project.

Ask which one applies. Recommend project scope for a bot created for this project. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: bot
    default: true
    api_base: https://acme.zulipchat.com/api/v1
    email: playbooks-bot@acme.zulipchat.com
    api_key: "{{env.ZULIP_API_KEY}}"
```

The `api_key` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. The `email` is not secret and belongs in this file as a plain value. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the key

```sh
apb connector env zulip --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the API key. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the key can be regenerated in Zulip if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve zulip --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that `api_base` and `email` are the ones you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `zulip` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `get_me` healthcheck: it asks Zulip to identify the authenticated account and writes to no stream. Because authentication carries the email, this also confirms the email and key pair match.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/zulip/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401: a mistyped key, a regenerated key, or an email that does not belong to the key. All three look the same from here, so check the pair together.
- 404: an `api_base` missing the `/api/v1` suffix, or a wrong host.
- a stream error on a later call while the healthcheck passes: for a bot, not being subscribed to that stream.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: tell the user the one thing left to them

A passing healthcheck only proves the email and key pair is valid. Zulip refuses to post to or read a stream the bot or account is not subscribed to, and only the user can fix that: subscribe it to each stream the playbooks should read or post in, from the organization's stream settings.

Stream and topic are call arguments, not account fields, so nothing about this lives in the config.

## Step 8: report

Tell the user, briefly:

- which account name you created, in which scope, and which identity it posts as;
- which file holds the API key and which key name;
- the healthcheck result, and that no stream was posted to;
- that a bot needs to be subscribed to a stream before it can post there, which only they can arrange;
- that a conversation is a stream plus a topic, so replying in a thread is the same `send_stream_message` call to the same topic and there is no separate reply function;
- that `get_messages` takes optional `anchor` (a numeric message id to page around, omitted for newest), `num_before`, and `narrow` (Zulip's filter, passed verbatim as a string whose content is a JSON array of operator and operand objects);
- that a stream message is visible to every subscriber and cannot be recalled, which makes a `max_calls` cap worthwhile where a loop could reach it.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
