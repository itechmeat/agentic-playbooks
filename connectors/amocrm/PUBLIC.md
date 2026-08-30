---
display_name: amoCRM
summary: Leads, contacts, companies, customers, pipelines, tasks, notes, custom fields, catalogs, events, webhooks and files in amoCRM and Kommo over REST API v4.
tags: [amocrm, kommo, crm, sales]
publisher: apb
---

The amoCRM connector covers REST API v4 of amoCRM (`amocrm.ru`) and its
international twin Kommo (`kommo.com`), which share the same API under a
per-account host: account and dictionaries (users, roles, event types, loss
reasons), leads and the unsorted inbox, pipelines and statuses, contacts and
companies, customers with their statuses, segments and transactions, tasks,
notes, tags, entity links, the event feed, custom field definitions and
groups, catalogs and products, webhook subscriptions, salesbots, lead
sources, chat templates, talks, subscriptions, short links, call logging,
and the Files API (list, read, attach, detach). 101 functions in total, 52
of them effectful. Wherever amoCRM's routes are symmetric across families,
one function takes an `entity_type` of `leads`, `contacts`, `companies` or
`customers` instead of four near-identical functions, which is how notes,
tags, links, custom fields, custom field groups, entity files, subscriptions
and batch updates are exposed.

Several things are deliberately absent. API v4 has no DELETE for leads,
contacts, companies, customers, tasks, notes, tags or catalog elements, so
this connector has none either: the eight irreversible functions it does
carry (`decline_unsorted`, `delete_pipeline`, `delete_status`,
`delete_custom_field`, `delete_transaction`, `set_customers_mode`,
`enable_products`, `unsubscribe_webhook`) are the whole destructive surface.
Webhook management is included (`list_webhooks`, `subscribe_webhook`,
`unsubscribe_webhook`) so an external receiver can be wired up, but
receiving amoCRM webhooks through apb's own ingest listener is not: amoCRM
posts form-encoded payloads without a signature, and the listener requires a
JSON body and an HMAC signature. File upload is out of scope (it is a
chunked binary session, and the connector format has no multipart or
raw-bytes body), so are the Chats API on `amojo.amocrm.ru` (a separate host
with its own HMAC-SHA1 signature scheme) and OAuth2 authorization-code flow
with refresh rotation (refresh tokens are single-use with a three-month TTL
and apb stores no tokens). Filters take exactly one value each, because a
query map cannot repeat a key.

## Account setup

Three account fields: `base_url` (scheme and host only, no trailing path,
either `https://<subdomain>.amocrm.ru` or `https://<subdomain>.kommo.com`),
`drive_url` (optional, the account-specific Files API host, used only by
`list_files` and `get_file`), and `access_token` (secret).

```yaml
accounts:
  - name: default
    base_url: https://example.amocrm.ru
    drive_url: https://drive-b.amocrm.ru
    access_token: "{{env.AMOCRM_ACCESS_TOKEN}}"
```

The token is the long-lived token of an integration on that account: open
the integration in amoMarket, tab "Keys", "Generate token", pick an expiry
between one day and five years, and copy the value, which is shown only
once. Long-lived tokens carry no refresh token, so nothing has to rotate
them between runs, and they can be revoked from the "Granted access" tab.
Read `drive_url` once from `get_account` with `with=drive_url` and copy it
into the account.

## Healthcheck

`get_account` confirms the token and the base URL: it renders with zero
arguments and succeeds against any token the account has issued. Called with
`with=drive_url,is_api_filter_enabled` it also reports the Files API host to
put in `drive_url` and whether the paid filtering add-on is on, which decides
whether the `filter_*` arguments work on leads, contacts, companies and
customers.
