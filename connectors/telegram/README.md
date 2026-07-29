# telegram: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means creating a bot, editing two files, and approving trust. An agent can do all of it for you and will only stop to ask for the bot token, which is the one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `telegram` connector so playbooks can message me, then read `connectors/telegram/INSTALL.md` under the apb config directory (by default `~/.config/apb`) and follow it to the end. Ask me for the bot token when you get there.

The agent installs the connector, writes the account config, prepares the secrets file, approves trust, and runs a live healthcheck that asks Telegram who the bot is. What you get back is either a working account or a specific error.

## What you will be asked for

A bot token. Telegram bots are created by talking to [@BotFather](https://t.me/BotFather): send it `/newbot`, pick a name and a username, and it replies with a token that looks like `123456789:AAF...`. That token is the bot.

You will also need to add the bot to whatever chat it should write to, and that part only you can do. A bot cannot message a person or a group it has never been introduced to: for a private chat, open the bot and send it any message once; for a group, add it as a member. Without that step `send_message` fails no matter how correct the setup is.

The token is written to a local file with owner-only permissions, and the account config next to it stores only a reference to it, never the value.

## What this connector can and cannot do

It sends messages, edits messages it sent, reads a chat's metadata, polls for new updates, and answers inline keyboard callbacks. That is the whole surface.

There is no webhook support: `get_updates` is the pull-based way for a playbook to notice a reply. A node that wants to react to an answer polls for it rather than being called back.

It cannot read arbitrary chat history, cannot join chats on its own, and cannot delete messages.

Worth knowing before you grant it: `send_message` reaches a real person's phone, and messages cannot be unsent once delivered. Give the grant a `max_calls` cap when a loop could reach it, so a retry storm does not turn into a hundred notifications.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the account fields and a config example, and `docs/CONNECTORS.md` in the apb repository covers accounts, secrets, and trust in general. `INSTALL.md` is written for an agent but the steps read fine as a checklist.
