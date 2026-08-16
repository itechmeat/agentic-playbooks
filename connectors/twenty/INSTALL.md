# twenty: installation instructions for an agent

You are setting up the apb `twenty` connector. Work through the steps in order. The only thing you need from the user is a base URL and an API key, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the API key back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `twenty` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install twenty
```

This copies the embedded connector into `<config-dir>/connectors/twenty` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve twenty`. It is local: no network call, nothing published.

If it refuses because a differing `twenty` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: get the base URL and API key, and be honest about what they grant

Two account fields: `base_url` (the app origin, no path suffix) and `api_key` (secret).

Ask which instance the user wants first: self-hosted or the cloud product. For self-hosted, `base_url` is their own host, for example `https://crm.example.com`; there is no separate `api.` subdomain. For the cloud product, `base_url` is `https://api.twenty.com`.

The API key is created in Twenty under Settings, API and Webhooks (some versions label this section Playground instead), then Create key. The key value is shown only once, so tell the user to copy it immediately; there is no way to view it again afterward, only to revoke and create a new one. State this plainly before the user generates anything: a key is scoped to the workspace it was created in, and its permissions follow the role assigned to it under Settings, Members, Roles (Assignment tab); Twenty has no separate scope selection beyond that role. Every key carries a mandatory expiry set at creation time - there is no "never expires" option, so the user should plan to rotate it before it lapses.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/twenty.yaml`: an identity the user wants available from every project.
- project, `<project>/.apb/connector-config/twenty.yaml`: an identity that belongs to this project.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: default
    default: true
    base_url: https://crm.example.com
    api_key: "{{env.TWENTY_API_KEY}}"
```

The `api_key` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`. For a second workspace (a different self-hosted instance, or cloud alongside self-hosted), name it distinctly and set its `base_url` to that instance's origin; do not reuse an existing account name for it.

## Step 4: prepare the secrets file, then ask for the API key

```sh
apb connector env twenty --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for `TWENTY_API_KEY`. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the key can be revoked and a new one created in Twenty (Settings, API and Webhooks) if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve twenty --account <name>
```

Account trust pins the account's non-secret fields (`base_url`), which is what decides where the API key gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the key will be sent; check that `base_url` is the one you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `twenty` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The declared `healthcheck` is `list_companies`: it renders with zero required arguments and succeeds against any key that can read companies, so it doubles as both the reachability probe and a meaningful live verification.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/twenty/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 403 with `messages` mentioning missing authentication: the `Authorization` header did not reach Twenty, usually a `base_url` typo.
- 401 with `messages` mentioning an invalid token: an expired, revoked, or mistyped API key.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, and in which scope, and which instance (self-hosted host or `api.twenty.com`) it targets;
- which file holds the API key and which key name;
- the `list_companies` healthcheck result;
- that the key's permissions follow the role it was created under, with no separate scope selection, so the way to limit a node is the grant's function list;
- that every key carries a mandatory expiry, so the account will need a new key before it lapses;
- that every record delete (the five typed deletes and the generic `delete_record`) is a soft delete (`soft_delete=true` is always sent) and can be undone with `restore_record`, that `delete_webhook` is the one exception and is a hard, unrecoverable delete, and that the 23 effectful functions overall (typed and generic create/update/delete, `create_note_target`, `create_task_target`, `create_webhook`, `delete_webhook`) still write into data other people see and deserve a narrow grant.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
