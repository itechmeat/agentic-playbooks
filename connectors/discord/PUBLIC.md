---
display_name: Discord
summary: List guild channels, read recent messages, and post or reply from a playbook.
tags: [discord, chat, bot, messages]
publisher: apb
---

The Discord connector covers channel listing, recent message history, and
sending or replying to messages over the Discord REST API (v10). Guild and
channel ids are call arguments, not account fields, so one bot account
serves every guild the bot has been invited to.

This connector uses REST only. Discord gateway events and the message-
content privileged intent are out of scope; reading recent history is done
by polling `get_messages` inside a node.

## Account setup

Two account fields: `api_base` (normally `https://discord.com/api/v10`) and
`token` (secret bot token).

```yaml
accounts:
  - name: default
    api_base: https://discord.com/api/v10
    token: "{{env.DISCORD_BOT_TOKEN}}"
```

### Create a bot token

1. Open the Discord Developer Portal
   (https://discord.com/developers/applications) and sign in.
2. Create a new application, then open its Bot tab.
3. Click Reset Token (or create the bot user if the application has none
   yet) and copy the token. Store it in `DISCORD_BOT_TOKEN` (or another
   env var referenced from the account). Treat the token as a secret;
   never commit it.

### Invite the bot and grant permissions

Invite the bot to each guild with an OAuth2 URL that includes the bot
scope and the permissions the playbook needs. At minimum:

| Permission | Used by |
|---|---|
| View Channels | `list_channels`, `get_messages`, `send_message`, `reply_to_message` |
| Read Message History | `get_messages` |
| Send Messages | `send_message`, `reply_to_message` |

REST reads return message content when the bot has Read Message History
in the channel. The gateway-only message-content intent does not apply to
REST; you do not need to enable that intent for this connector.

## Functions

- `get_me` (healthcheck): confirms the bot token and returns id, username,
  and bot.
- `list_channels`: lists channels in a guild (id, name, type, parent_id,
  position).
- `get_messages`: recent messages in a channel (id, content,
  author.username, timestamp). Optional `limit` (1-100) and `before`
  (message id) walk history backward; omit both on the first page.
- `send_message`: post a top-level text message (`content` required).
- `reply_to_message`: reply to an existing message with the same endpoint;
  body carries `content` plus a nested `message_reference.message_id`.

`send_message` and `reply_to_message` are separate functions so a playbook
grant can allow thread replies without allowing new top-level posts.

## Threads

Discord threads are channels. To read a thread, call `get_messages` with
the thread's channel id (not the parent channel id). Sending into a
thread uses the same id with `send_message` or `reply_to_message`.

## Pagination

`get_messages` paginates with Discord's `before` / `limit` message-id
query args. Read the oldest message id from the current page and pass it
as `before` on the next call. A query pair whose value is a single
absent placeholder is dropped, so the first page needs only `channel_id`.

## Rate limits

Discord rate limits are aggressive and per-route. Avoid tight polling
loops over `get_messages` or channel lists. Bound playbook calls with
`max_calls` grants so a runaway node cannot hammer a single route. The
engine maps HTTP 429 through the existing status table; this connector
does not special-case global rate-limit headers beyond that.

## Healthcheck

`get_me` probes `GET /users/@me` and reports the authenticated bot
identity.
