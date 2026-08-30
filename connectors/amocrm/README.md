# amocrm: setup for humans

## The short way: let an agent do it

Setting this connector up by hand means editing two files, generating a
token in amoCRM, and approving trust. An agent can do all of it for you and
will only stop to ask for the account subdomain and the token, which is the
one thing it cannot obtain on your behalf.

Paste a prompt like this to a coding agent that has `apb` available:

> Install the apb `amocrm` connector for my account, then read
> `connectors/amocrm/INSTALL.md` under the apb config directory (by default
> `~/.config/apb`) and follow it to the end. Ask me for the account URL and
> the access token when you get there.

The agent installs the connector, writes the account config, prepares the
secrets file, approves trust, and probes the account with `get_account`.
What you get back is either a working account or a specific error.

## What you will be asked for

An account URL and a long-lived access token.

The account URL is the scheme and host only, no path suffix:
`https://<subdomain>.amocrm.ru` for amoCRM or
`https://<subdomain>.kommo.com` for Kommo. It is the same host you use in
the browser. Every function is templated under `<base_url>/api/v4/...`, so a
trailing path or a stray slash breaks every call at once.

The token comes from an integration on that account. Open amoMarket, create
a private integration, grant it "Account data" and "Files access", open the
integration, go to the tab "Keys", press "Generate token", and pick an
expiry, which can be anything from one day to five years. "Files access" is
enough for every file function in this connector, `detach_files` included;
"File deletion" is never needed, because no drive-level delete is exposed.
The value is shown once and cannot be read again afterwards, only revoked
and regenerated, so copy it immediately. A long-lived token has no refresh
token and needs no rotation logic; it can be revoked at any time from the
integration's "Granted access" tab.

There is a third, optional field, `drive_url`: the account-specific Files
API host, for example `https://drive-b.amocrm.ru`. Only `list_files` and
`get_file` use it. Read it once by calling `get_account` with
`with=drive_url` and copy the value into the account config. Leave it out
if you do not need those two functions.

## Token

Creating a private integration on a non-technical amoCRM account can require
an application to amoCRM support, a waiver signed with company documents,
before the integration section becomes available. The "external integration"
created directly in amoMarket issues the same kind of long-lived token
without that step, and amoCRM documents long-lived tokens for external and
private integrations alike. Either path hands you a Bearer token, and this
connector accepts both: it only ever sends `Authorization: Bearer <token>`,
where the token is whatever the account's `access_token` field resolves to.

## What this connector can and cannot do

It covers the account and its dictionaries (users, roles, event types, loss
reasons), leads and the unsorted inbox, pipelines and statuses, contacts and
companies, customers with their statuses, segments and transactions, tasks,
notes, tags, entity links, the event feed, custom field definitions and
groups, catalogs and products, webhook subscriptions, salesbots, lead
sources, chat templates, talks, subscriptions, short links, call logging,
and the Files API (list, read, attach, detach). 101 functions in total, 52
of them effectful and 49 marked `read_only`.

Wherever amoCRM's routes are symmetric, one function takes an `entity_type`
of `leads`, `contacts`, `companies` or `customers` rather than four
near-identical functions. That collapse covers notes, tags, links, custom
fields, custom field groups, entity files and batch updates. Subscriptions
collapse the same way but over two families only, `leads` and `customers`,
which is all amoCRM documents for that route.

These API facts hold across the whole connector and are stated here rather
than repeated in every function description:

| Fact | What it means for a playbook |
|---|---|
| Rate limit: 7 requests per second per integration | A 429 comes back as `rate_limited` with `retry_after_sec`. Nothing retries automatically, so bound loops with `max_calls` and back off yourself. |
| An empty result is HTTP 204 with no body | Not an empty array. A list function that finds nothing yields nothing, not `[]`. |
| Paging is HAL | Items sit under `_embedded.<collection>`, the next page under `_links.next.href`, `page` starts at 1 and `limit` caps at 250 (100 for `list_events`, 50 for `list_chat_templates`, and custom field lists return at most 50 per page whatever `limit` says). |
| `filter[...]` may need the paid filtering add-on | On leads, contacts, companies and customers. `get_account` with `with=is_api_filter_enabled` says whether it is on. |
| Every filter takes exactly one value | `query` is a map and cannot repeat a key, so `filter[id][0]`, `filter[id][1]` cannot be expressed. Sweeping several ids or several event types means one call each. |
| Batch writes take an array | 50 records per call is the vendor recommendation and 250 the hard cap; `create_leads_complex` and `create_sources` cap at 50, `create_catalogs` at 10. Each record may carry a `request_id` that is echoed back, which is how a 400 maps to the record that caused it. |
| v4 has no DELETE for core entities | Leads, contacts, companies, customers, tasks, notes, tags and catalog elements cannot be deleted through the API at all. Do not go looking for it. |
| Tariff gates answer HTTP 402 | Customers, catalogs and products, webhooks and API filtering are each gated by the account's tariff or an add-on. A 402 is a billing answer, not a bug in the call. |

A few things are out of scope on purpose:

- Receiving amoCRM webhooks through apb's ingest listener. amoCRM posts
  `application/x-www-form-urlencoded` without a signature; the listener
  requires a JSON body and an HMAC signature. Managing subscriptions
  (`list_webhooks`, `subscribe_webhook`, `unsubscribe_webhook`) is in scope,
  so an external receiver can be wired up today.
- File upload. It is a chunked binary Drive session, and the connector
  format has no multipart or raw-bytes body. Files already in the account
  can be listed, read, attached and detached.
- The Chats API on `amojo.amocrm.ru`, a separate host with its own
  HMAC-SHA1 signature plus `Content-MD5` and `Date` headers that no auth
  kind expresses. `POST /contacts/chats` goes with it, since it needs chat
  ids from that API.
- OAuth2 authorization-code flow and refresh rotation. Refresh tokens are
  single-use with a three-month TTL and apb stores no tokens; the long-lived
  token is the documented path for this kind of integration.
- Creating unsorted entries (`/unsorted/forms`, `/unsorted/sip`), customer
  status and segment writes, segment custom fields, widget install and
  uninstall, website buttons, and the chat template review flow. They belong
  to form, telephony and widget integrations rather than to agent workflows.

## Grant presets

Every granted function is rendered into the node prompt in full: its
description, its argument schema and one example. Granting the whole
connector costs 69,667 characters of prompt (roughly 17,000 tokens at four
characters per token), which is a lot of context to spend before the node
has done anything. Granting the `sales-write` preset below costs 19,251
characters (roughly 4,800 tokens). Both numbers were measured by rendering
the instruction block against this manifest.

So grant a preset, not the connector. Copy one of these into the node's
`connectors` list:

```yaml
connectors:
  - name: amocrm
    functions: [get_account, list_pipelines, list_statuses, list_leads, get_lead, list_contacts, get_contact, list_companies, get_company, list_tasks, list_notes, list_tags, list_users]
```

That is `sales-read`: everything a node needs to answer questions about the
pipeline without touching it.

```yaml
connectors:
  - name: amocrm
    functions: [get_account, list_pipelines, list_statuses, list_leads, get_lead, list_contacts, get_contact, list_companies, get_company, list_tasks, list_notes, list_tags, list_users, create_leads, create_leads_complex, update_lead, create_contacts, update_contact, create_companies, update_company, create_notes, create_tasks, complete_task, create_tags, link_entities]
```

`sales-write`: `sales-read` plus the everyday write path. It creates and
moves records but deletes nothing and changes no account settings.

```yaml
connectors:
  - name: amocrm
    functions: [list_unsorted, get_unsorted_summary, accept_unsorted, decline_unsorted, list_talks, close_talk]
```

`inbox`: triage of the unsorted queue and of open chat talks.
`decline_unsorted` is irreversible.

```yaml
connectors:
  - name: amocrm
    functions: [list_pipelines, create_pipelines, update_pipeline, list_statuses, create_statuses, update_status, list_custom_fields, create_custom_fields, update_custom_field, list_custom_field_groups, list_users, list_roles, list_webhooks, subscribe_webhook, unsubscribe_webhook]
```

`setup-admin`: shaping the account itself. Give this to a setup node, not to
a node that runs on every lead.

```yaml
connectors:
  - name: amocrm
    functions: [list_catalogs, get_catalog, create_catalogs, update_catalog, list_catalog_elements, get_catalog_element, create_catalog_elements, update_catalog_element, get_products_settings, enable_products]
```

`catalog`: catalogs and products. Add `list_catalog_custom_fields` and
`create_catalog_custom_fields` when the node has to map catalog field ids.
`enable_products` is irreversible, and the whole family answers 402 on a
tariff without catalogs.

```yaml
connectors:
  - name: amocrm
    functions: [list_customers, get_customer, create_customers, update_customer, list_customer_statuses, list_customer_segments, list_transactions, add_transactions, delete_transaction, set_customers_mode]
```

`customers`: repeat sales. `delete_transaction` cannot be undone and
`set_customers_mode` reshapes the whole account; the family answers 402 on a
tariff without customers.

```yaml
connectors:
  - name: amocrm
    functions: [list_files, get_file, list_entity_files, attach_files, detach_files, list_file_links]
```

`files`: attachments. `list_files` and `get_file` need the `drive_url`
account field, and the whole family needs "Files access" on the token, which
is the only file scope it needs.

`functions: read_only` is the built-in coarse split. It grants all 49
read-only functions at once, which is broader and more expensive than
`sales-read` but needs no list to maintain.

## Irreversible functions worth restricting

Eight functions carry the `EFFECTFUL, IRREVERSIBLE` marker:
`decline_unsorted`, `delete_pipeline`, `delete_status`,
`delete_custom_field`, `delete_transaction`, `set_customers_mode`,
`enable_products` and `unsubscribe_webhook`. Six of them cannot be undone
through the API at all: deleting a status moves its leads to the first stage
of the pipeline, deleting a custom field takes its values with it, and
`enable_products` cannot be switched back. The last two can be redone rather
than undone, and are on the list for their account-wide reach:
`set_customers_mode` can be switched again, but it recomputes how every
customer's stages are derived, and `unsubscribe_webhook` can be
resubscribed, but every event fired in between is lost. A ninth function is
worth the same treatment without being on that list: `detach_files` leaves
the drive file intact, so it is not irreversible, but it does strip the
attachment off the record. Keep those eight, and `detach_files` with them,
out of any grant that does not specifically need them.

## Doing it by hand

If you would rather not involve an agent, `PUBLIC.md` in this folder has the
account fields and a config example, and `docs/CONNECTORS.md` in the apb
repository covers accounts, secrets, and trust in general. `INSTALL.md` is
written for an agent but the steps read fine as a checklist.
