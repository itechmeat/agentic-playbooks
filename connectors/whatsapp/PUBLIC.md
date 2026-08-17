---
display_name: WhatsApp
summary: Send text, template, and media messages and receive inbound WhatsApp messages through Meta's Cloud API.
tags: [whatsapp, messaging, meta]
publisher: apb
---

The WhatsApp connector covers Meta's WhatsApp Business Platform Cloud API:
sending free-form text, sending template messages, sending media messages by
hosted link or media id, marking an inbound message as read, template
management (list, create, delete), the WhatsApp Business Profile (get,
update), the account's phone numbers (list, get), media (get a short-lived
download URL, delete), and receiving inbound messages and status updates
through a webhook-backed inbox. 16 functions in total.

Sending and receiving are split by deployment. Sending works from any apb
install that holds a valid access token: it is an ordinary outbound REST
call. Receiving has no polling endpoint on Meta's side; inbound messages,
delivery receipts, and template status updates arrive only as webhook POSTs,
so receiving requires the server-mode plus webhook-ingest topology (a public
HTTPS callback URL that Meta is registered against) and is read back locally
through the inbox functions, never over the network. A send-only account can
omit the two webhook secrets.

A plain text message only sends inside the 24-hour customer-service window
opened by the recipient's last inbound message. Outside that window Meta
rejects the call with error 131047, and a template message is required to
re-open contact; the connector cannot enforce this by itself, since the
window is per-recipient runtime state on Meta's side and not visible to a
stateless call. Creating a template is asynchronous: the call returns the new
template with status PENDING, and Meta's review outcome arrives later,
observable only by re-listing templates or through a webhook. Deleting a
template or a media object is a hard, unrecoverable delete on both resources,
unlike this connector's twenty sibling, which soft-deletes CRM records.
Uploading binary media is out of scope, since Meta's upload endpoint is
multipart with a binary file part and this connector's schema expresses only
JSON and form-urlencoded bodies; media is sent by a hosted `link` or by a
media `id` obtained out of band.

## Account setup

Seven account fields: `base_url` (defaults to `https://graph.facebook.com`),
`graph_version` (e.g. `v23.0`), `phone_number_id`, `waba_id`, and three
secrets: `access_token` (the System User permanent token, required),
`app_secret`, and `verify_token` (the latter two required only for
receiving).

```yaml
accounts:
  - name: default
    base_url: https://graph.facebook.com
    graph_version: v23.0
    phone_number_id: "123456789012345"
    waba_id: "234567890123456"
    access_token: "{{env.WHATSAPP_ACCESS_TOKEN}}"
    app_secret: "{{env.WHATSAPP_APP_SECRET}}"
    verify_token: "{{env.WHATSAPP_VERIFY_TOKEN}}"
```

## Healthcheck

`get_phone_number` confirms the access token is valid and that
`phone_number_id` belongs to it: it renders with zero arguments (its Graph
API `fields` query is a fixed literal) and returns the account's
`verified_name` and `quality_rating`.
