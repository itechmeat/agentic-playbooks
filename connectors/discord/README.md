# discord: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating a Discord application, generating a bot token, inviting the bot with the right permissions, editing two files, and approving trust. An agent can do all of it for you and will only stop to ask for the bot token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `discord` connector so playbooks can read and post in our Discord, then read `connectors/discord/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the bot token when you get there.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that asks Discord to identify the bot. What you get back is either a working account or a specific error.

## What you will be asked for

A bot token. Getting one means creating an application: open the Discord Developer Portal, create an application, open its Bot tab, and create or reset the token there.

You then invite the bot to your server with an OAuth2 URL that includes the bot scope plus the permissions playbooks need: View Channels for everything, Read Message History to read messages, and Send Messages to post. Only you can invite it, and without that step nothing works no matter how correct the setup is.

Nothing here needs the privileged message-content intent. This connector talks to Discord's REST API rather than its gateway, and that intent only governs the gateway.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It lists a server's channels, reads recent messages in a channel, posts a message, and replies to a specific message. That is the whole surface.

Threads are channels in Discord, so reading or posting in a thread is the same call with the thread's channel id.

It cannot send direct messages, cannot manage channels, roles, or members, cannot react with emoji, and cannot edit or delete messages.

Two things worth knowing before you grant it. A message posted to a channel is seen by everyone in it and cannot be recalled. And Discord's rate limits are aggressive and applied per route, so a playbook that polls a channel in a tight loop will start getting throttled: give the grant a `max_calls` cap.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
