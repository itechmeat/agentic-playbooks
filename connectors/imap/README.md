# imap: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, generating an app password, and approving trust. An agent can do all of it for you and will only stop to ask for the app password, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `imap` connector for my Gmail account `you@gmail.com`, then read `connectors/imap/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the app password when you get there.

Swap in your own address, and name your provider if it is not Gmail (Yandex Mail, Outlook, Microsoft 365, iCloud, or any other IMAP host). The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that logs in without opening a mailbox. What you get back is either a working account or a specific error.

## What you will be asked for

One thing: an app password for the mailbox. Not your account password. Providers issue a separate password for programs like this, and it can be revoked on its own without touching your login.

For Gmail: turn on 2-Step Verification first (Google will not offer app passwords until you do), then generate one under your Google Account security settings, App passwords. It is 16 characters. Yandex Mail wants IMAP access enabled in its web settings first. iCloud issues one from your Apple ID account page. Outlook and Microsoft 365 do not accept passwords at all and need an OAuth token helper instead, which the agent will explain if that is your provider.

The password is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value. Nothing is sent anywhere except to your own mail provider.

## What this connector can and cannot do

It lists mailbox folders, reads mail, and marks messages read or unread. That is the whole surface. It cannot delete a message, cannot move one between folders, and cannot send anything. Sending is the separate `smtp` connector.

Reading is silent: opening a folder to search it, and fetching a message body, both leave the unread state alone. A playbook can read your inbox without any trace appearing in your mail client. The only functions that change something on the server are `mark_read` and `mark_unread`, and each has to be granted to a node explicitly.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example per provider, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
