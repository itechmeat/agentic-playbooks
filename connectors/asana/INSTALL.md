# asana: installation instructions for an agent

You are setting up the apb `asana` connector. Work through the steps in order. The only thing you need from the user is a token, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `asana` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install asana
```

This copies the embedded connector into `<config-dir>/connectors/asana` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve asana`. It is local: no network call, nothing published.

If it refuses because a differing `asana` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: get the token, and be honest about what it grants

`api_base` is `https://app.asana.com/api/1.0`.

The token is a personal access token: in Asana, profile settings, then Apps, then Developer apps, then create a personal access token.

State this plainly to the user before they create it, because it is not obvious and it cannot be narrowed afterwards: an Asana personal access token acts as the user who created it, carrying that user's full permissions, and Asana provides no scope selection. Everything the user can see or change, a node holding this token can see or change. The only place that can be constrained is the apb side, by granting a node just the functions it needs.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/asana.yaml`: the user's own Asana identity, available from every project.
- project, `<project>/.apb/connector-config/asana.yaml`: an identity tied to this project.

Ask which one applies. Recommend global, since one token already reaches every workspace the user can see and workspace ids are call arguments rather than account fields. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: personal
    default: true
    api_base: https://app.asana.com/api/1.0
    token: "{{env.ASANA_TOKEN}}"
```

The `token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the token

```sh
apb connector env asana --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the token. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be revoked and reissued in Asana if that matters to them. Given how broad an Asana token is, that warning is worth making explicitly rather than in passing.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve asana --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the token will be sent; check that `api_base` is the one you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `asana` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `get_me` healthcheck: it asks Asana to identify the token's owner and changes nothing.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -s -X POST "http://127.0.0.1:7321/api/connectors/asana/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401: an expired, revoked, or mistyped token.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, and in which scope;
- which file holds the token and which key;
- the healthcheck result;
- that the token carries their full Asana permissions with no scope selection available, so the way to limit a node is the grant's function list;
- that workspace, project, section, and task gids are call arguments, so one account already reaches everything the token can see;
- that `list_workspaces`, `list_projects`, and `list_tasks` all require a `limit` argument on every call, and page through an optional `offset`: it comes back in the call result's `next_page.offset` and goes into the next call, omitted on the first;
- that `search_tasks` is a fuzzy typeahead over task names rather than a full-text search, so a playbook that needs a complete result set should list a project's tasks instead.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
