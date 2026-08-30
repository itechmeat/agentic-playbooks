# amocrm: installation instructions for an agent

You are setting up the apb `amocrm` connector. Work through the steps in order. The only things you need from the user are the account URL and a long-lived access token, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the access token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

No run is started anywhere in this document. Setting up a connector and binding it to a playbook node are separate decisions, and the second one belongs to the user.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `amocrm` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install amocrm
```

This copies the embedded connector into `<config-dir>/connectors/amocrm` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve amocrm`. It is local: no network call, nothing published.

If it refuses because a differing `amocrm` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: get the account URL and the token, and be honest about what they grant

Three account fields: `base_url` (required, non-secret), `drive_url` (optional, non-secret) and `access_token` (required, secret).

Ask which product the user is on first. `base_url` is the scheme and host only, no path suffix: `https://<subdomain>.amocrm.ru` for amoCRM, `https://<subdomain>.kommo.com` for Kommo. It is the same host they use in the browser. Every function is templated under `<base_url>/api/v4/...`, so a trailing path or a stray slash breaks every call at once.

The token is the long-lived token of an integration on that account. Walk the user through it: open amoMarket, create a private integration, grant it "Account data" and "Files access" (add "File deletion" only if they want `detach_files`), open the integration, go to the tab "Keys", press "Generate token", pick an expiry between one day and five years, and copy the value.

State this plainly before they generate anything. The value is shown once and cannot be read again, only revoked and regenerated. The token's reach is the integration's scope set, so a token without "Files access" fails every file function with 401 or 403 while the rest keeps working. A long-lived token carries no refresh token, so nothing rotates it for them: when the expiry they picked arrives, every call starts failing with 401 and a new token has to be generated. It can be revoked at any time from the integration's "Granted access" tab.

If the user reports that the integration section is unavailable on their account, tell them the two paths that exist: a private integration can require an application to amoCRM support on a non-technical account, while an external integration created directly in amoMarket issues the same kind of long-lived token without that step. This connector accepts either, since both produce a Bearer token.

Leave `drive_url` empty for now. Step 5 reads it from the account and comes back to it.

## Step 3: decide the scope, then write the account config and the secret

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/amocrm.yaml`: an identity the user wants available from every project.
- project, `<project>/.apb/connector-config/amocrm.yaml`: an identity that belongs to this project.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: default
    default: true
    base_url: https://example.amocrm.ru
    drive_url: https://drive-b.amocrm.ru
    access_token: "{{env.AMOCRM_ACCESS_TOKEN}}"
```

The `access_token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal token in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`. For a second amoCRM account, name it distinctly and set its `base_url` to that account's host; do not reuse an existing account name for it.

Then prepare the dotenv:

```sh
apb connector env amocrm --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Prefer the global `<config-dir>/secrets.env` for this connector. The token is an account credential rather than a project one, and a file outside the repository cannot be committed by accident. Add the `AMOCRM_ACCESS_TOKEN=` line there yourself if you take that path. The project `.apb/secrets.env` is gitignored and works too, but only reach for a project dotenv you created by hand after verifying that `.gitignore` covers it: an uncommitted secret is one `git add -A` away from a public repository.

Now ask the user for `AMOCRM_ACCESS_TOKEN`. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be revoked and regenerated in the integration's "Keys" tab if that matters to them.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 4: approve the account

```sh
apb connector approve amocrm --account <name>
```

Account trust pins the account's non-secret fields (`base_url` and `drive_url`), which is what decides where the token gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the token will be sent; check that `base_url` is the host the user named before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `amocrm` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 5: verify against the real API, then fill in drive_url

The declared `healthcheck` is `get_account`: it renders with zero required arguments and succeeds against any token the account has issued, so it doubles as the reachability probe and a meaningful live verification.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/amocrm/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

The healthcheck sends `get_account` with no arguments, which does not include `drive_url`. Ask for it on the same server, through the manual-call endpoint, which runs the identical execution path and resolves the token from the secrets file exactly as a run would. Never assemble the request yourself: the token lives in `secrets.env`, which the shell does not source, so a hand-written `Authorization` header would either send an empty Bearer or force the token onto the command line.

```sh
curl -sS -X POST "http://127.0.0.1:7321/api/connectors/amocrm/call?workspace=<workspace-id>" \
  -H 'Content-Type: application/json' \
  -d '{"function":"get_account","account":"<account>","args":{"with":"drive_url,is_api_filter_enabled"}}'
```

`account` may be omitted when the connector has a single account or one marked `default: true`. Like the healthcheck, this answers HTTP 200 whether the call succeeded or not. A success looks like `{"ok":true,"status":200,"body":{...},"truncated":false,"picked":true}`, where `picked` says the function's `response_pick` projection was applied, and `body` carries `drive_url` and `is_api_filter_enabled` among the account fields. A failure looks like `{"ok":false,"error":{"code":"...","message":"...","http_status":...}}`, with `retry_after_sec` added on a 429. Read the body, not the status line. Add `"dry_run":true` to render the request without executing it, which is also the only form that skips the trust gate; the live call above requires the approval from step 4.

Copy the returned `drive_url` into the account config from step 3, and note `is_api_filter_enabled` for the report: when it is false, the `filter_*` arguments on leads, contacts, companies and customers will not work until the account buys the filtering add-on. Skip this call entirely if the user does not want `list_files` and `get_file`.

Changing `drive_url` changes the account's non-secret fields, which drops account trust. Approve once more after the edit:

```sh
apb connector approve amocrm --account <name>
apb connector doctor
```

## Step 6: read the failure correctly

Do not paper over a failure. Report the exact message and what it points at.

| Symptom | What it actually means |
|---|---|
| HTTP 401 | The token expired (the expiry picked at generation time has passed), was revoked from "Granted access", or was mistyped. Regenerate it in the integration's "Keys" tab; there is no way to view an existing one. |
| HTTP 403 | The account has an IP allowlist that this machine is not on, the integration lacks the scope the function needs ("Files access" for the file functions, admin rights for `list_users`, `create_sources` and `set_customers_mode`), or amoCRM has rate-blocked the integration after repeated 429s. |
| HTTP 402 | A tariff gate, not a bug: customers, catalogs and products, webhooks and API filtering are each gated by the account's plan or an add-on. |
| HTTP 429 | The 7 requests per second per integration limit. It comes back as `rate_limited` with `retry_after_sec`; nothing retries automatically. |
| `has no account <name>` | The account slug does not match the config, or the config went into a scope this workspace does not see. |
| unresolved env var | The secrets file has the key but no value, or the key name in the config does not match the one in the dotenv. |
| trust refused | Step 4 was skipped, or the account fields changed after approval (adding `drive_url` does exactly that), which drops it. |

## Step 7: report

Tell the user, briefly:

- which account name you created, in which scope, and which host it targets;
- which file holds the token and which key name;
- the `get_account` healthcheck result, and the `drive_url` value if you read one;
- whether `is_api_filter_enabled` is true, since the `filter_*` arguments on leads, contacts, companies and customers depend on it;
- the expiry they picked for the token, and that a long-lived token has no refresh: every call starts failing with 401 on that date unless a new token is generated first;
- that the connector has 101 functions, 52 of them effectful, and that granting all of them costs roughly 17,000 tokens of node prompt, so `connectors/amocrm/README.md` lists seven ready-to-paste grant presets and they should pick one;
- that eight functions cannot be undone through the API (`decline_unsorted`, `delete_pipeline`, `delete_status`, `delete_custom_field`, `delete_transaction`, `set_customers_mode`, `enable_products`, `unsubscribe_webhook`) and belong only in a grant that specifically needs them;
- that API v4 has no DELETE for leads, contacts, companies, customers, tasks, notes, tags or catalog elements, so records a playbook creates stay in the account.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
