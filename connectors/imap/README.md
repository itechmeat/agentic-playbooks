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

It lists mailbox folders, reads mail, marks messages read or unread, files a composed message as a draft, and saves one message's attachments into the run's own directory. It cannot delete a message, cannot move one between folders, and cannot send anything: `append_draft` files a message in the drafts folder with the `\Draft` flag and never delivers it, so sending stays the separate `smtp` connector.

Reading is silent: opening a folder to search it, and fetching a message body, both leave the unread state alone. A playbook can read your inbox without any trace appearing in your mail client. The only functions that change something on the server are `mark_read`, `mark_unread`, and `append_draft` (which adds a message to your drafts folder, nothing else), and each has to be granted to a node explicitly.

`save_attachments` writes a message's attachments under the run's `attachments/` directory: at most 20 files, 10 MiB per file and 25 MiB per call, each name sanitized and never overwriting an existing file. It reads the mailbox without changing it, works only inside a run, and what it returns is text written by people outside the run, so bindings that grant it should carry `untrusted: true`.

## Passwords and tokens

Three ways to authenticate, chosen by the account's `auth_method`:

- `password`: an app password, the default and the simplest.
- `xoauth2`: an access token minted by an external token helper you run yourself (`oama`, `mutt_oauth2`).
- `oauth2`: apb mints the access token itself from a refresh token that the `apb connector oauth imap` consent flow (run by you in a terminal, in a browser) obtained once. Providers that refuse app passwords, such as Outlook and Microsoft 365, work through this without an external helper. `INSTALL.md` walks an agent through it.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example per provider, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
