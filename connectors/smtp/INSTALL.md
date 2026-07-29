# smtp: installation instructions for an agent

You are setting up the apb `smtp` connector so playbooks can send mail on the user's behalf. Work through the steps in order. The only thing you need from the user is an app password (or the relay's SMTP credentials), plus one confirmation about where the account should live. Everything else you determine yourself.

Report progress in the user's chat language. Do not print the password back to them, and do not put it in a commit, a log, a summary, or any file other than the secrets dotenv named below.

Send nothing during setup. The healthcheck in step 6 authenticates without transmitting a message, and that is the only live call this document asks you to make. Do not send a test message to prove it works; mail cannot be unsent, and the user did not ask for one.

## Step 0: check your ground

```sh
apb --version
apb connector list
```

`apb connector list` shows installed connectors and, below them, the embedded ones available to install. If `smtp` already appears in the installed group, skip to step 2 but still verify the account and trust.

Establish the global config directory, since several paths below live in it: `$APB_CONFIG_DIR` when set, else `$XDG_CONFIG_HOME/apb`, else `$HOME/.config/apb`. This document refers to it as `<config-dir>`.

## Step 1: install the connector

```sh
apb connector install smtp
```

This copies the embedded connector into `<config-dir>/connectors/smtp` and records trust for its tree digest in the same action, so you do not need a separate `apb connector approve smtp`. It is local: no network call, nothing published.

If it refuses because a differing `smtp` is already installed, do not reach for `--force` on your own. Report what is installed and ask the user whether to replace it.

## Step 2: work out the relay settings

Ask the user what they are sending through, since the two cases differ.

**A mailbox provider**, using an app password. Take the settings from this table rather than guessing:

| Provider | host | port | use_tls |
|---|---|---|---|
| Gmail, Google Workspace | `smtp.gmail.com` | `587` | `"true"` |
| Yandex Mail | `smtp.yandex.com` | `587` | `"true"` |
| iCloud | `smtp.mail.me.com` | `587` | `"true"` |
| Outlook, Microsoft 365 | see below | | |

**A transactional email service** (Postmark, SendGrid, Amazon SES, Mailgun, or similar). The host, port, and credentials all come from that service's dashboard. Ask the user to read them off it. Port 587 with STARTTLS is the usual choice. Do not invent a hostname for a service you are unsure about.

Outlook and Microsoft 365 have withdrawn password authentication for SMTP, and this connector has no OAuth path: its `verify` and `send_email` authenticate with a username and password or not at all. If that is the user's provider, say so plainly rather than configuring an account that will fail at login. A separate relay, or a transactional service, is the way through.

`use_tls` carries no engine-level default, so set it explicitly. Use `"true"` for STARTTLS on port 587, which is the common case. Only a trusted local relay with no encryption uses `"false"`, and then `username` and `password` are omitted entirely.

## Step 3: decide the scope, then write the account config

Two locations exist, and the difference matters:

- global, `<config-dir>/connector-config/smtp.yaml`: a personal sending address the user wants available from every project.
- project, `<project>/.apb/connector-config/smtp.yaml`: a sending identity that belongs to this project, for example a release bot.

Ask which one applies. Recommend project scope for anything that represents the project rather than the person. When both exist, the merged list is global plus project, and a project account replaces a global one of the same name.

Write the file with the settings from step 2. Use a short slug for `name`, and give the account `default: true` when it is the only one:

```yaml
accounts:
  - name: gmail
    default: true
    host: smtp.gmail.com
    port: "587"
    use_tls: "true"
    username: you@gmail.com
    from_email: you@gmail.com
    from_name: Playbook Bot
    password: "{{env.GMAIL_APP_PASSWORD}}"
```

`from_email` is required and is the address recipients will see. `from_name` is optional. With most mailbox providers `from_email` has to be the authenticated mailbox or one of its verified aliases: a mismatch is rejected by the provider at send time, or silently rewritten, so keep them the same unless the user has a verified alias in mind.

Name the env var after the account so several senders never collide: `GMAIL_APP_PASSWORD`, `POSTMARK_SMTP_TOKEN`. Ports and booleans are quoted strings, as above.

The `password` field must hold exactly one reference, either `{{env.VAR}}` or `{{cmd:<command>}}`. A literal secret in this file is a validation error and the call will be refused, so do not put one there even temporarily. This config file is non-secret by design and safe to commit.

If you are editing a file that already has accounts, add yours to the list and leave the others alone. At most one account in the merged list may carry `default: true`.

## Step 4: prepare the secrets file, then ask for the password

```sh
apb connector env smtp --write
```

Run this from the project root. It appends a `KEY=` template line for every unresolved env var to `<project>/.apb/secrets.env`, creates that file with owner-only permissions when it is absent, never duplicates a key that is already there, and makes sure `.gitignore` covers it. Values are left empty on purpose.

Now ask the user for the credential, and tell them how to get one:

- Gmail: 2-Step Verification has to be on before Google offers app passwords at all. Then Google Account, Security, App passwords. The result is 16 characters.
- Yandex Mail and iCloud: an app password from the provider's account settings.
- A transactional service: the SMTP token or password from its dashboard, which is usually shown once at creation.

Prefer that the user fills the value in themselves: give them the exact file path and the key name, and wait. That keeps the secret out of the conversation transcript entirely. If they hand it to you in chat instead, write it into that file without echoing it back, and tell them plainly that the transcript now contains it and that the credential can be revoked and reissued at the provider if that matters to them.

A global alternative exists at `<config-dir>/secrets.env`, resolved after the project file. Use it when the account is global and the user does not want a project-local secret. Only reach for a project `secrets.env` you created by hand if `apb connector env --write` was not used, and in that case verify `.gitignore` coverage yourself: an uncommitted secret is one `git add -A` away from a public repository.

The resolution order at call time is process environment, then the project dotenv, then the global dotenv.

## Step 5: approve the account

```sh
apb connector approve smtp --account <name>
```

Account trust pins the account's non-secret fields, which is what decides where a secret gets sent. It is deliberately separate from connector trust and is never bypassed by a run. The command prints the concrete field values it is approving; check that the host, username, and `from_email` are the ones you wrote before confirming.

Then verify the whole picture:

```sh
apb connector doctor
```

It reports manifest, config, env resolution, and trust status for every connector and account. Every check for `smtp` should be clean. This command makes no network call, so a clean report is necessary but not sufficient.

## Step 6: verify against the real relay

The live probe is the `verify` function: it connects, negotiates STARTTLS, authenticates, and sends no message. This is the only live call to make during setup.

`apb connector call` cannot be used here. It requires a run context (`APB_RUN_DIR` and `APB_NODE_ID`, both set by the engine), and fabricating one is not an acceptable substitute. Use the dashboard's healthcheck endpoint instead, which runs the same execution path outside a run.

Check whether a dashboard is up, and start one if not (the port is 7321 unless overridden by `port` in `<config-dir>/config.yaml`):

```sh
curl -s http://127.0.0.1:7321/api/health
apb dashboard --no-open    # only if the health check did not answer
```

The endpoint identifies the workspace by id, not by path. Read it from the project list, then probe:

```sh
curl -s http://127.0.0.1:7321/api/projects
curl -s -X POST "http://127.0.0.1:7321/api/connectors/smtp/healthcheck/<account>?workspace=<workspace-id>"
```

A 4xx answer means the workspace id or query string is wrong; once the workspace resolves, the answer is HTTP 200 with the outcome in the body's `ok` and `error` fields. A refusal or a failure is reported there, not as an HTTP status, so read the body.

Common failures and what they actually mean:

- `has no account <name>`: the account slug in the URL does not match the config, or you wrote the config into a scope this workspace does not see.
- authentication failed: with Gmail this is nearly always an ordinary account password where an app password is required, or 2-Step Verification not being enabled. With Outlook it is the withdrawn password authentication from step 2. It is not a connector problem.
- connection refused or a timeout: the wrong port, or an outbound block on 587 from this network.
- unresolved env var: the secrets file has the key but no value, or the key name in the config does not match the one in the dotenv.
- trust refused: step 5 was skipped, or the account fields changed after approval, which drops it.

Do not paper over a failure. Report the exact message and what it points at.

## Step 7: report

Tell the user, briefly:

- which account name you created, and in which scope;
- the `from_email` recipients will see;
- which file holds the secret and which key;
- the healthcheck result, and that nothing was sent;
- that `send_email` is not read-only and mail cannot be unsent, so it is worth granting only to the nodes that need it, with a `max_calls` cap on the grant when a loop could reach it;
- that this connector cannot read mail or see replies, and that `imap` is the companion connector for that.

Do not offer to run a playbook, and do not start one. Binding this connector to a node is a separate decision that belongs to the user.
