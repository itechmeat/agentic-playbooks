# sentry: installation instructions for an agent

You are setting up the apb `sentry` connector. Work through the steps in order. The only thing you need from the user is a token and their organization slug, plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `sentry` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install sentry
```

This copies the embedded connector into `<config-dir>/connectors/sentry` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve sentry`. It is local: no network call, nothing published.

If it refuses because a differing `sentry` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: settle the base URL, the org, and the token scopes

`base_url` is `https://sentry.io` on the hosted service, or the user's own host when self-hosted. Ask which one applies.

`org` is the organization slug, not its display name. It is the short segment in Sentry URLs, for example the `acme` in `https://sentry.io/organizations/acme/issues/`. Ask the user to read it off their own URL rather than guessing it from a company name.

The token is created in Sentry under Settings, then Auth Tokens. Ask which capabilities the playbooks need and request only those scopes:

- `project:read` and `event:read`: listing projects, listing and reading issues. Enough for a read-only triage playbook.
- `event:write`: additionally required by `update_issue`, which is how an issue is resolved or ignored.
- `project:releases`: additionally required by `create_release` and `create_deploy`.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/sentry.yaml`: one organization the user works with everywhere.
- project, `<project>/.apb/connector-config/sentry.yaml`: an organization tied to this project.

Ask which one applies. Recommend project scope, since a Sentry account is bound to one organization and organizations usually map to projects. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: acme
    default: true
    base_url: https://sentry.io
    org: acme
    token: "{{env.SENTRY_TOKEN}}"
```

The `token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the token

```sh
apb connector env sentry --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the token. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be revoked and reissued in Sentry if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve sentry --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that `base_url` and `org` are the ones you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `sentry` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `list_projects` healthcheck: authenticated, read-only, and scoped to the configured organization, which makes it a check of the org slug as well as the token.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/sentry/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401: an expired, revoked, or mistyped token.
- 404: the wrong organization slug, or a token issued in a different organization.
- 403 on a write while the healthcheck passes: a missing `event:write` or `project:releases` scope.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, in which scope, and against which organization;
- which file holds the token, which key, and which scopes it carries;
- the healthcheck result;
- that `list_issues` requires a `cursor` argument on every call, passed as an empty string on the first call: the call result's `link` field then carries the next cursor to pass on the following call;
- that `update_issue`, `create_release`, and `create_deploy` are visible to colleagues, so they belong in the grants of nodes that need them rather than everywhere;
- that alert rules, webhooks, and cross-service issue linking are out of scope for this connector and belong in playbook orchestration.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
