# github: installation instructions for an agent

You are setting up the apb `github` connector. Work through the steps in order. The only thing you need from the user is a token, and one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the token back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `github` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install github
```

This copies the embedded connector into `<config-dir>/connectors/github` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve github`. It is local: no network call, nothing published.

If it refuses because a differing `github` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: settle the API base and the credential

`api_base` is `https://api.github.com` for github.com. On GitHub Enterprise Server it is that instance's own REST API base, which GitHub's convention puts at `https://<host>/api/v3`. Ask which one applies rather than assuming github.com, and have the user confirm the GHES base off their own instance rather than taking that convention on faith.

For the credential, check the cheapest option first:

```sh
gh auth status
```

If `gh` is authenticated, prefer `token: "{{cmd:gh auth token}}"`. The token is then resolved at call time from the CLI session, nothing is stored in a file, and the user has nothing to paste. Note that the command string becomes part of the account digest, so changing it later drops account trust and needs a fresh approval.

Otherwise the user needs a personal access token, and the required permissions depend on the token type:

- classic token: `repo`, or `public_repo` when only public repositories are in play.
- fine-grained token: access to the specific repositories, plus Actions write permission if `dispatch_workflow` will be used.

Tell the user which one you are asking for and why. Do not ask for broader permissions than the playbooks actually need.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/github.yaml`: the user's own GitHub identity, available from every project.
- project, `<project>/.apb/connector-config/github.yaml`: an identity that belongs to this project, for example a bot account.

Ask which one applies. Recommend global for the user's own account. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

```yaml
accounts:
  - name: personal
    default: true
    api_base: https://api.github.com
    token: "{{cmd:gh auth token}}"
```

Or with a stored token:

```yaml
accounts:
  - name: personal
    default: true
    api_base: https://api.github.com
    token: "{{env.GITHUB_TOKEN}}"
```

The `token` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the token

Skip this step entirely when the account uses `{{cmd:gh auth token}}`.

```sh
apb connector env github --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the token. Prefer that they fill the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the token can be revoked and reissued on GitHub if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve github --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that `api_base` is the one you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `github` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real API

The live probe is the `get_rate_limit` healthcheck: authenticated, cheap, and it changes nothing.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -s -X POST "http://127.0.0.1:7321/api/connectors/github/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- 401: an expired, revoked, or mistyped token.
- 404 on a repository call that the healthcheck did not catch: a fine-grained token without access to that repository. The healthcheck passes because rate limit needs no repository access.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, and in which scope;
- whether the credential is a stored token or the `gh` CLI session, and where it lives in the stored case;
- the healthcheck result;
- that a fine-grained token can pass the healthcheck and still be denied on a specific repository, so the first real call is the true test;
- that `merge_pull`, `create_release`, and `dispatch_workflow` are not read-only and not easily undone, so they belong only in the grants of nodes that need them, with a `max_calls` cap where a loop could reach them.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
