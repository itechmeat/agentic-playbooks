---
display_name: Telegram
summary: Send and read messages through a Telegram bot.
tags: [telegram, messaging, notifications]
publisher: apb
---

The Telegram connector wraps a Bot API bot: send messages, edit them,
inspect a chat, poll for updates, and answer inline keyboard callbacks.
There is no webhook support in this wave; `get_updates` is the
pull-based way to react to replies inside a playbook node.

## Account setup

Create a bot with [@BotFather](https://t.me/BotFather) in Telegram
(`/newbot`), then store the token it gives you:

```yaml
accounts:
  - name: default
    api_base: https://api.telegram.org
    token: "{{env.TELEGRAM_BOT_TOKEN}}"
```

`api_base` is overridable for a self-hosted Bot API server; leave it as
`https://api.telegram.org` otherwise.

Before `send_message` works on a chat, the bot must already be a member
of it (added to a group, or the user has started a conversation with it
directly).

## Healthcheck

`get_me` confirms the token resolves to a real bot.
