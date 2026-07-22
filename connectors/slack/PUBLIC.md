---
display_name: Slack
summary: Read Slack channels and threads and post messages from a playbook.
tags: [slack, chat, messaging]
publisher: apb
---

The Slack connector covers listing channels, reading recent channel and
thread messages, and posting messages and thread replies through the
Slack Web API with a bot token. Slack reports API failures as HTTP 200
with `"ok": false` in the body; the connector declares an `error_when`
rule so such a response becomes a real service error (carrying Slack's
`error` string, for example `missing_scope` or `channel_not_found`)
that retries, fallbacks, and gates react to.

## Account setup

Two account fields: `api_base` (normally `https://slack.com/api`) and
`token` (secret).

```yaml
accounts:
  - name: default
    api_base: https://slack.com/api
    token: "{{env.SLACK_BOT_TOKEN}}"
```

### Creating the bot token

1. Open [api.slack.com/apps](https://api.slack.com/apps) and create an
   app (from scratch) in your workspace.
2. Under OAuth and Permissions, add the bot token scopes the playbook
   needs (see below), then install the app to the workspace.
3. Copy the Bot User OAuth Token (it starts with `xoxb-`) into your
   secrets, for example `.apb/secrets.env` as `SLACK_BOT_TOKEN=...`.

### Required bot token scopes

- `channels:read` for `list_channels` (add `groups:read` to list
  private channels the bot is in).
- `channels:history` for `get_messages` and `get_thread` on public
  channels (`groups:history` for private ones).
- `chat:write` for `send_message` and `reply_in_thread`.

Slack scopes are granular: a token missing a scope fails per function,
not at the healthcheck, and the error surfaces as `missing_scope`
through the connector's `error_when` mapping. After adding a scope,
reinstall the app to the workspace for the token to pick it up.

### Channel membership

The bot must be a member of a channel before it can read history or
post there: invite it with `/invite @your-app` in the channel. A call
against a channel the bot is not in fails with `not_in_channel`.

## Channels, threads, and pagination

Channel ids (`C...`) are call arguments, not account fields; find them
with `list_channels` or from a channel's URL. A message whose result
carries `thread_ts` belongs to a thread; read the thread with
`get_thread` (passing the parent's `ts`) and reply with
`reply_in_thread`. `send_message` and `reply_in_thread` are separate
functions so a grant can allow thread replies without allowing new
top-level posts.

List and history functions page with a body-carried cursor: pass the
`response_metadata.next_cursor` value from one call's result as the
`cursor` argument of the next; an empty `next_cursor` means the last
page.

## Healthcheck

`auth_test` calls `auth.test`, confirming the token resolves and
reporting the workspace and bot identity. It is a POST by Slack API
convention but mutates nothing.
