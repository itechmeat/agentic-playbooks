# smtp: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, generating an app password, and approving trust. An agent can do all of it for you and will only stop to ask for the app password, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `smtp` connector so playbooks can send mail from `you@gmail.com`, then read `connectors/smtp/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the app password when you get there.

Swap in your own address, and name your provider if it is not Gmail. The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that logs in to the relay without sending anything. What you get back is either a working account or a specific error.

If you also want playbooks to read mail, ask for the `imap` connector in the same prompt. The two are designed to be installed together for a read-and-reply workflow.

## What you will be asked for

One thing: an app password for the mailbox you are sending from. Not your account password. Providers issue a separate password for programs like this, and it can be revoked on its own without touching your login.

For Gmail: turn on 2-Step Verification first (Google will not offer app passwords until you do), then generate one under your Google Account security settings, App passwords. It is 16 characters.

If you are sending through a transactional email provider rather than a mailbox (Postmark, SendGrid, Amazon SES and the like), what you need is that service's SMTP credentials, which it issues in its own dashboard. There is no app password involved.

The password is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value. Nothing is sent anywhere except to the relay you configured.

## What this connector can and cannot do

Two functions, and that is the whole surface: `verify` logs in to the relay without sending, and `send_email` sends one message. Plain text, optional HTML alongside it, and comma-separated recipient lists for to, cc, and bcc.

It cannot read mail, list a mailbox, or see replies. Reading is the separate `imap` connector.

Worth knowing before you grant it: `send_email` is not read-only, and mail cannot be unsent. A playbook node that has this function can mail anyone the run puts in its recipient list. Grant it to the nodes that genuinely need to send, and consider giving a node a `max_calls` cap so a loop cannot turn into a hundred messages.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
