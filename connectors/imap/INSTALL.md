# imap: installation instructions for an agent

You are setting up the apb `imap` connector for a user's mailbox. Work through the steps in order. The only thing you need from the user is an app password, and one confirmation about where the account should live. An `oauth2` account instead needs a client id and client secret plus the user's consent in a browser (step 4). Everything else you determine yourself.

Report progress in the user's chat language. Do not print the app password back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `imap` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install imap
```

This copies the embedded connector into `<config-dir>/connectors/imap` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve imap`. It is local: no network call, nothing published.

If it refuses because a differing `imap` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: work out the provider settings

Ask the user for their email address if you do not already have it, and for the provider if the address does not make it obvious. Then take the connection settings from this table rather than guessing.

| Provider | host | port | auth_method |
|---|---|---|---|
| Gmail, Google Workspace | `imap.gmail.com` | `993` | `password` |
| Yandex Mail | `imap.yandex.com` | `993` | `password` |
| iCloud | `imap.mail.me.com` | `993` | `password` |
| Outlook, Microsoft 365 | `outlook.office365.com` | `993` | `xoauth2` |

For any other provider, ask the user for the IMAP host, or find it in the provider's own documentation. Do not invent a hostname.

`use_tls` is `"true"` for every real provider. Set it explicitly; only a local plaintext test fixture ever uses `"false"`.

Two cases do not use a password at all:

- Outlook and Microsoft 365, which have stopped accepting passwords.
- A Google Workspace account whose administrator disabled app passwords by policy.

For either case set `auth_method: oauth2`: apb mints the access token itself from a refresh token obtained once through `apb connector oauth imap`, its own loopback consent flow. Follow step 4 after the account config. If the user prefers an external token helper they already run, `auth_method: xoauth2` with a token such as `oama` or `mutt_oauth2` referenced as `{{cmd:...}}` still works, and installing and authorizing that helper is a separate setup step you cannot complete for them. Do not attempt a password account as a workaround: it will fail at authentication and the failure will look like a bad password.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/imap.yaml`: a personal mailbox the user wants available from every project.
- project, `<project>/.apb/connector-config/imap.yaml`: a mailbox that belongs to this project, for example a shared support address.

Ask which one applies. Recommend global for a personal address. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

Write the file with the settings from step 2. Use a short slug for `name` (`gmail`, `support`, `work`), and give the account `default: true` when it is the only one:

```yaml
accounts:
  - name: gmail
    default: true
    host: imap.gmail.com
    port: "993"
    use_tls: "true"
    auth_method: password
    username: you@gmail.com
    password: "{{env.GMAIL_APP_PASSWORD}}"
```

Name the env var after the account so several mailboxes never collide: `GMAIL_APP_PASSWORD`, `SUPPORT_IMAP_PASSWORD`. Ports and booleans are quoted strings, as above.

The `password` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: an OAuth2 account, when step 2 chose `auth_method: oauth2`

A password account skips this step. An `oauth2` account authenticates with an access token apb mints at call time from a refresh token, over the connector's `oauth2_refresh` auth block. The token never sits in the account config; apb itself stores nothing.

You need an OAuth client from the provider. Ask the user for the client id and client secret, and have them create one if they have none: a Google Cloud OAuth client of type desktop for Gmail and Google Workspace (desktop allows the loopback redirect apb binds, `http://127.0.0.1` on a port it picks), or an Azure app registration with a mobile-and-desktop redirect for Outlook and Microsoft 365. Note the IMAP scopes the provider documents; apb asks for the ones the account names in `oauth_scopes`, so take them from the provider's IMAP page rather than guessing.

Extend the account config from step 3:

```yaml
    auth_method: oauth2
    oauth_token_url: https://oauth2.googleapis.com/token
    oauth_authorize_url: https://accounts.google.com/o/oauth2/v2/auth
    oauth_client_id: <client id>
    oauth_scopes: https://mail.google.com/
```

The URL and scope values above are the Gmail ones; use the provider's own endpoints and scopes for any other host. `oauth_client_secret` and `oauth_refresh_token` are secrets and belong in the secrets file, which the next step prepares.

Then run the consent flow, in a terminal, with the user present:

```sh
apb connector oauth imap --account <name>
```

It prints a URL; the user opens it, approves the access, and the provider calls back to the loopback port apb is listening on. The command prints the refresh token once. Ask the user to paste it into the secrets file's `oauth_refresh_token=` themselves, or write it for them without echoing it back, and tell them plainly that the transcript now contains it and that rerunning the consent flow revokes and reissues it. After the flow, continue with step 5 to finish the secrets file and approve the account.

## Step 5: prepare the secrets file, then ask for the password

```sh
apb connector env imap --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the app password, and tell them how to get one. An `oauth2` account needs no app password: `oauth_refresh_token` came from step 4's consent, and `oauth_client_secret` is the only value still missing, copied from the OAuth client the user created.

- Gmail: 2-Step Verification has to be on before Google offers app passwords at all. Then Google Account, Security, App passwords. The result is 16 characters.
- Yandex Mail: enable IMAP access in the Mail web settings first, then generate an app password.
- iCloud: App-Specific Passwords on the Apple ID account page.

Prefer that the user fills the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the password can be revoked and reissued at the provider if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 6: approve the account

```sh
apb connector approve imap --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that the host and username are the ones you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `imap` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 7: verify against the real server

The live probe is the `verify` function: it connects, negotiates TLS, authenticates, and opens no mailbox. It is the safest possible first call.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -sS -w '\nHTTP %{http_code}\n' -X POST "http://127.0.0.1:7321/api/connectors/imap/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- authentication failed: with Gmail this is nearly always an ordinary account password where an app password is required, or 2-Step Verification not being enabled. It is not a connector problem.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 6 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 8: report

Tell the user, briefly:

- which account name you created, and in which scope;
- which file holds the secret and which key;
- the healthcheck result;
- that reading mail never marks it read, and that `mark_read` and `mark_unread` are separate functions a node has to be granted;
- that `append_draft` files a message in the drafts folder and never sends it, and that `save_attachments` writes a message's attachments into the run's own directory only, capped, never overwriting a file;
- that this connector cannot send mail, and that `smtp` is the companion connector if they want to reply from a playbook.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
