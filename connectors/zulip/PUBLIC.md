---
display_name: Zulip
summary: Read and post Zulip stream and direct messages from a playbook.
tags: [zulip, chat, messaging]
publisher: apb
---

The Zulip connector covers reading recent messages, listing streams and
topics, and posting stream and direct messages through the Zulip REST
API. It uses HTTP Basic authentication with the account email and API
key, and posts messages as `application/x-www-form-urlencoded` bodies,
matching Zulip's native write contract.

## Account setup

Three account fields: `api_base`, `email`, and `api_key` (secret).

```yaml
accounts:
  - name: default
    api_base: https://example.zulipchat.com/api/v1
    email: bot@example.zulipchat.com
    api_key: "{{env.ZULIP_API_KEY}}"
```

### Where to get the API key

For a personal (human) account: open Zulip settings, go to Account and
privacy, then Show API key, and copy the value. For a bot: open the bot
panel under Settings, find the bot, and copy its API key. The `email`
field is the account or bot email address shown in the same panel.

### api_base form

For Zulip Cloud the base is `https://<org>.zulipchat.com/api/v1`. For a
self-hosted server it is `https://<host>/api/v1` (or the equivalent
behind your reverse proxy). The connector only talks to the
`api_base` you configure.

### Basic auth note

Zulip authenticates every request with HTTP Basic auth, where the
username is the account email and the password is the API key (not a
password). The connector builds the `Authorization: Basic
base64(email:api_key)` header from the resolved account values, so the
API key never appears in a URL or query string.

## Stream and topic model

A Zulip conversation lives in a stream, and each stream is divided into
topics (analogous to threads). `send_stream_message` posts to a given
stream and topic; replying in a thread is the same call to the same
stream and topic, so there is no separate reply function.

## Reading messages

`get_messages` takes `anchor`, `num_before`, and `narrow`, all optional.
Omit `anchor` for newest-first; pass a numeric message id to page
around it. `narrow` is Zulip's JSON-encoded filter string: pass it
verbatim as a string whose content is a JSON array of operator/operand
objects, for example `[{"operator": "channel", "operand": "general"}]`.
The connector does not model `narrow` structurally; build the JSON
string in the playbook.

## Self-hosted server compatibility

The manifest sticks to long-stable endpoints (`/users/me`, `/streams`,
`/messages`, and `/users/me/<id>/topics`) so older self-hosted Zulip
servers work without a specific API version pin.

## Healthcheck

`get_me` confirms the API key resolves and reports the authenticated
user or bot identity.
