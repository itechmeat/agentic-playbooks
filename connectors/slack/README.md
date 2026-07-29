# slack: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating a Slack app, picking scopes, installing it to your workspace, editing two files, and approving trust. An agent can do all of it for you and will only stop to ask for the bot token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `slack` connector so playbooks can read and post in our Slack, then read `connectors/slack/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the bot token when you get there.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that asks Slack to identify the bot. What you get back is either a working account or a specific error.

## What you will be asked for

A bot token, the kind that starts with `xoxb-`. Getting one means creating an app: go to [api.slack.com/apps](https://api.slack.com/apps), create an app in your workspace, add bot token scopes under OAuth and Permissions, install the app to the workspace, and copy the Bot User OAuth Token.

The scopes to add depend on what playbooks should do: `channels:read` to list channels, `channels:history` to read messages and threads, and `chat:write` to post. Private channels need the `groups:` twins of the read scopes. The agent will tell you exactly which ones to tick based on what you want.

You will also need to invite the bot into each channel it should work in, with `/invite @your-app`. Only you can do that, and without it reading and posting both fail no matter how correct the setup is.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It lists channels, reads recent channel and thread messages, posts a message, and replies in a thread. That is the whole surface.

Posting a top-level message and replying in a thread are separate functions on purpose, so a playbook can be allowed to answer in a thread without being allowed to start new conversations in a channel.

It cannot send direct messages, cannot upload files, cannot react with emoji, cannot edit or delete messages, and cannot manage channels or users.

Worth knowing before you grant it: a message posted to a channel is seen by everyone in it and cannot be recalled. Give the grant a `max_calls` cap when a loop could reach it.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
