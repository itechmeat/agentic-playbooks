# atrip: installation instructions for an agent

You are setting up the apb `atrip` connector. Work through the steps in order. The only thing you need from the user is a credential pair, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the client secret back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `atrip` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install atrip
```

This copies the embedded connector into `<config-dir>/connectors/atrip` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve atrip`. It is local: no network call, nothing published.

If it refuses because a differing `atrip` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: get the credentials, and be honest about what they grant

Four account fields: `client_id`, `client_secret` (secret), `base_url`, and `search_base_url`. Only `search` goes to `search_base_url`; every other function goes to `base_url`.

Ask which environment the user wants first: sandbox or production. Recommend starting in sandbox regardless of the eventual target, since the declared healthcheck (`query_void_orders`) only proves reachability and credentials, and the first meaningful verification is a live call.

**Sandbox**: generate the client id and client secret yourself in the ATRIP portal, self-service, no approval needed. Both `base_url` and `search_base_url` are `https://sandbox.atriptech.com`.

**Production**: the credential pair and both base URLs are issued together inside the ATRIP portal, under My Profile then Company Information, once the account has passed UAT and a customer manager has switched it to LIVE status. Production uses two separate per-tenant base URLs, one for search and a different one for everything else; there is no shared production hostname to hardcode, and AtripTech's own documentation warns against reusing the sandbox base URL after go-live. Do not guess these values; ask the user to read them from their own portal.

State this plainly before the user generates anything: AtripTech's documentation says to keep both credential values server-side and never expose them in a client application. There is no scope selection for these credentials; the pair reaches everything the account is entitled to, so the only place to narrow access is the function grant on the node.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/atrip.yaml`: an identity the user wants available from every project.
- project, `<project>/.apb/connector-config/atrip.yaml`: an identity that belongs to this project.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: sandbox
    default: true
    client_id: "{{env.ATRIP_CLIENT_ID}}"
    client_secret: "{{env.ATRIP_CLIENT_SECRET}}"
    base_url: https://sandbox.atriptech.com
    search_base_url: https://sandbox.atriptech.com
```

The `client_secret` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. `client_id` is not marked secret and could be written literally, but referencing it from the same dotenv keeps both halves of the pair together and out of a committed file; either way is valid. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`. For a production account, name it distinctly (for example `production`) and set its `base_url`/`search_base_url` to the two per-tenant URLs from the portal; do not reuse the sandbox account name for it.

## Step 4: prepare the secrets file, then ask for the credentials

```sh
apb connector env atrip --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for `ATRIP_CLIENT_ID` and `ATRIP_CLIENT_SECRET`. Prefer that they fill the values in themselves: give them the exact file path and the key names, and wait. That keeps both values out of the conversation transcript entirely. If they hand either to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the credential pair can be regenerated in the ATRIP portal if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want project-local secrets. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve atrip --account <name>
```

Account trust pins the account's non-secret fields (`client_id`, `base_url`, `search_base_url`), which is what decides where the secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the client secret will be sent; check that both URLs are the ones you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `atrip` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The declared `healthcheck` (`query_void_orders`) is a zero-argument reachability probe: it proves the URL and credentials work, but against a tenant with no void records the vendor may answer with a business-level error in the body, which the probe still counts as reachable. The meaningful live verification here is a real, low-risk read-only call: `balance` with a `currency` argument (for example `USD`), which asks Atlas for the account balance and touches no booking or payment state.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's generic call endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/atrip/call?workspace=<workspace-id>" \
  -H 'content-type: application/json' \
  -d '{"function":"balance","account":"<account-name>","args":{"currency":"USD"}}'
```

A 4xx answer means the workspace id or request body is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body. A successful call returns `accountBalance.amount` and `accountBalance.currency`, projected by the function's `response_pick`; pass `"full": true` in the request body instead if you need the unprojected response for debugging.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the request does not match the config, or you wrote the config into a scope this workspace does not see.
- an HTTP-level 4xx or 5xx from Atlas itself: an infrastructure or gateway problem rather than a business error, since Atlas reports most business outcomes inside a 200-OK body.
- a body with a non-zero `status` and a `msg`: read the message, but per AtripTech's own documentation do not use `msg` alone to judge success or failure; the numeric `status` is authoritative, and for `balance` specifically the research behind this connector found no documented status field at all, so any response carrying `accountBalance` is the success signal.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, and in which scope, and whether it targets sandbox or production;
- which file holds the client id and client secret, and which two keys;
- the result of the `balance` verification call;
- that there is no scope selection on these credentials, so the way to limit a node is the grant's function list;
- that eight functions are effectful (`order`, `pay`, `void`, `refund`, `stop_ticket`, `regenerate_order`, `pnr_claim`, `post_booking_ancillary_order`) and deserve a narrow grant, with `pay` singled out as retry-unsafe because AtripTech documents no idempotency key for it.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
