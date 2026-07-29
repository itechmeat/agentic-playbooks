# zulip: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, finding your API key, and approving trust. An agent can do all of it for you and will only stop to ask for the API key, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `zulip` connector for our Zulip, then read `connectors/zulip/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the API key when you get there.

Tell it your Zulip address, cloud or self-hosted. The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that asks Zulip who you are. What you get back is either a working account or a specific error.

## What you will be asked for

Two things: the email address of the account or bot that will post, and its API key.

For your own account, the key is in Zulip settings under Account and privacy, behind Show API key. For a bot, it is in the bot panel next to the bot you created. Which one to use is a real choice: a bot posts under its own name and can be given narrow stream access, while your own account posts as you.

The API key is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It reads recent messages, lists streams and their topics, posts to a stream topic, and sends direct messages. That is the whole surface.

Zulip organizes conversation as streams divided into topics, and a thread is just a topic. So there is no separate reply function: replying means posting to the same stream and topic again.

It cannot edit or delete messages, cannot react with emoji, cannot upload files, and cannot manage streams, users, or subscriptions.

Worth knowing before you grant it: a stream message is visible to everyone subscribed to that stream and cannot be recalled. Give the grant a `max_calls` cap when a loop could reach it.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
