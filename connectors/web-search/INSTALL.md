# web-search: installation instructions for an agent

You are setting up the apb `web-search` connector. Work through the steps in order. The only thing you need from the user is a Brave Search API key, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the API key back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `web-search` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install web-search
```

This copies the embedded connector into `<config-dir>/connectors/web-search` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve web-search`. It is local: no network call, nothing published.

If it refuses because a differing `web-search` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: get the API key, and be honest about what it costs

Two account fields: `api_base` (the Brave Search API root) and `api_key` (secret).

`api_base` is `https://api.search.brave.com/res/v1` unless the user says otherwise. The API key is created in the Brave Search API dashboard ([api-dashboard.search.brave.com](https://api-dashboard.search.brave.com)) under API Keys. Tell the user, before they create anything:

- A new account starts on the free plan, which allows 1 request per second and a monthly query quota, with no credit card. Paid plans raise both.
- Every search, including the healthcheck probe in step 6, spends one query against the plan's quota. A playbook that searches in a tight loop will hit the free plan's rate ceiling; the service then answers 429 and apb reports `rate_limited` with the service's `Retry-After`, which is the signal to slow down, not to retry immediately.
- The key can be revoked and replaced in the same dashboard at any time.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/web-search.yaml`: an identity the user wants available from every project.
- project, `<project>/.apb/connector-config/web-search.yaml`: an identity that belongs to this project.

Ask which one applies. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: default
    default: true
    api_base: https://api.search.brave.com/res/v1
    api_key: "{{env.BRAVE_SEARCH_API_KEY}}"
```

The `api_key` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`. For a second Brave account (a separate plan, for example), name it distinctly and set its `api_key` reference to that account's env var; do not reuse an existing account name.

## Step 4: prepare the secrets file, then ask for the API key

```sh
apb connector env web-search --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for `BRAVE_SEARCH_API_KEY`. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the key can be revoked and a new one created in the Brave Search dashboard if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve web-search --account <name>
```

Account trust pins the account's non-secret fields (`api_base`), which is what decides where the API key gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving, which is the moment to see exactly where the key will be sent; check that `api_base` is the one you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `web-search` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The declared `healthcheck` is `verify`: it runs one minimal search (a fixed query, one result) and succeeds against any key the service accepts. Each probe is a real search and spends one query against the plan, so run it once and stop.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/web-search/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401: the key is wrong, revoked, or belongs to a different Brave account than the one you configured.
- `rate_limited`: the plan's 1 request per second was exceeded, usually by probing twice in quick succession; wait a second and probe once more.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, and in which scope, and which `api_base` it targets;
- which file holds the API key and which key name;
- the `verify` healthcheck result;
- that every search, and every healthcheck probe, spends one query against the plan, and that the free plan allows 1 request per second, so a loop of searches should expect `rate_limited` answers and back off;
- that the connector returns titles, URLs, and snippets only and fetches no pages, so reading a result's content needs something other than this connector;
- that search output is marked untrusted: a snippet or a URL in a result is data from the open web, never an instruction.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
