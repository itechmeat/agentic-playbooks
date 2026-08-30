# amoCRM connector: REST API v4 over a long-lived token

Date: 2026-08-30. Status: approved by the owner; live verification deferred (see Testing).

## Motivation

amoCRM (and its international twin Kommo) is a widely used sales CRM with a large, uniform REST API v4. The connector gives playbooks dense coverage of that API (leads, contacts, companies, customers, pipelines, tasks, notes, tags, custom fields, catalogs and products, events, users, webhooks management, bots, sources, talks, files) while keeping the per-node prompt cost low. It is the fifteenth official connector and the second CRM after twenty.

The design goal stated by the owner: cover as much of the API as APB can express, but do not overload the agent's starting context; the connector should signal what is possible without front-loading every schema.

## Constraints from the connector format (verified in the codebase)

- One `auth` block per connector, four kinds (`header`, `query`, `basic`, `path`). No OAuth2, no token refresh, by design (`docs/CONNECTORS.md`, wave-2 spec decision table).
- Every granted function is rendered into the node prompt in full: description, compact `args_schema` JSON, one example. There is no summary layer, no lazy schema lookup, no function groups. The only levers are the per-node `functions:` allowlist and the `functions: read_only` shorthand (`crates/apb-engine/src/connector/prompt.rs`).
- `query:` is a map: duplicate keys are impossible, so amoCRM multi-value filters (`filter[id][0]=1&filter[id][1]=2`) cannot be expressed. Bracketed single keys work (percent-encoded on the wire, decoded by the server).
- A value that is exactly `{{args.x}}` drops the query pair when the arg is absent; mixed templates error on an absent arg.
- No multipart bodies. No automatic pagination, retry or rate limiting: a 429 reaches the agent as `rate_limited` with `retry_after_sec`.
- Webhook ingest requires an HMAC signature and a JSON body. amoCRM posts form-encoded payloads without a signature.
- CI gate (`crates/apb-cli/tests/suite/official_connectors_gate.rs`): every function needs an `args_schema` of type object, every `{{args.X}}` in a `url` must be `required`, every `read_only` HTTP function needs `response_pick`, every function needs at least one `tests.yaml` case, examples must validate, `PUBLIC.md` needs `display_name` and `summary`.

## Scope decisions (owner approved)

- Coverage tier C: core CRM, administrative layer, and the Files API (read, attach, detach; no upload).
- Authentication: long-lived token only (option 1). No OAuth2 helper, not even as documentation in v1.
- `base_url` is an account field, so Kommo (`https://<sub>.kommo.com`) works without a connector change.

## Out of scope for v1 (with reasons)

| Item | Reason |
|---|---|
| Receiving amoCRM webhooks through the ingest listener | amoCRM delivers `application/x-www-form-urlencoded` without an HMAC signature; the listener mandates a signature and a JSON body. Needs engine work, tracked separately. Webhook management (subscribe, list, unsubscribe) is in scope so external receivers can be wired. |
| File upload (Drive sessions) | Chunked binary upload; the format has no multipart or raw-bytes body. |
| Chats API (`amojo.amocrm.ru`) | Separate host with its own HMAC-SHA1 `X-Signature` plus `Content-MD5` and `Date` headers; no auth kind expresses it. Also excluded: `POST /contacts/chats` linking, since it depends on chat ids from that API. |
| OAuth2 authorization-code and refresh rotation | Refresh tokens are single-use with 3-month TTL; APB has no token storage. The long-lived token (TTL up to 5 years) is the documented path for private integrations. |
| Multi-value filters | `query` is a map. Each filter argument takes one value. Documented per function. |
| Deleting core entities (leads, contacts, companies, tasks, notes, tags, catalog elements) | v4 has no DELETE for them. Documented in README so agents do not look for it. |
| Widgets install/uninstall, website buttons (CRM Plugin), chat template review flow, Kommo AI, salesbot continue handlers, live-call `POST /api/v2/events/` | Require a widget archive, a moderated integration, or are UI-plugin plumbing; no value for a headless runner. |

## Files

```
connectors/amocrm/
  connector.yaml
  tests.yaml
  PUBLIC.md
  README.md
  INSTALL.md
  skills/          (empty, reserved)
```

Changes outside the folder: add `amocrm` to the pinned official list in `crates/apb-core/src/connector/official.rs`; a new section in `docs/CONNECTORS.md`; `docs/release-notes/v0.21.0.md`; add `.apb/secrets.env` to `.gitignore` (the `.apb/` directory is committed and the file is currently not excluded).

## Account and auth

```yaml
name: amocrm
version: 0.1.0
healthcheck: get_account
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.access_token}}"
account_fields:
  - name: base_url        # https://<subdomain>.amocrm.ru or https://<subdomain>.kommo.com, no trailing path
    required: true
  - name: drive_url       # from GET /api/v4/account?with=drive_url, e.g. https://drive-b.amocrm.ru; only file functions use it
    required: false
  - name: access_token    # long-lived token from the private integration, tab "Keys"
    required: true
    secret: true
```

Operator setup (README, INSTALL): create a private integration in amoMarket, grant "Account data" and "Files access" (and "File deletion" only if `delete_files` is wanted), open the integration, tab "Keys", generate a long-lived token, store it as `{{env.AMOCRM_ACCESS_TOKEN}}` in the global `secrets.env`. Run `get_account` with `with: drive_url` once and copy `drive_url` into the account config. The token is shown once; a lost token is regenerated, not recovered.

## Conventions

- Arguments are snake_case; record bodies (`record`, `records`) are forwarded verbatim in amoCRM's own JSON shape (`custom_fields_values`, `_embedded`, `tags_to_add`, `request_id`). Every argument description names the vendor field when the names differ.
- `entity_type` is an enum `[leads, contacts, companies, customers]` wherever amoCRM's routes are symmetric (notes, tags, links, custom fields, custom field groups, entity files, subscriptions). One function replaces four.
- Batch write functions take `records` (array, 1 to 50 items, amoCRM recommends 50 and hard-caps 250). Single-record update functions take `id` plus `record`.
- List functions expose `page`, `limit` (max 250; 100 for events; 50 for chat templates), `order_by` plus `order_dir` where the API supports `order[...]`, `with` as a comma-joined string, `query` where supported, and a bounded set of single-value `filter_*` arguments rendered as `filter[<name>]` or `filter[<name>][from|to]`.
- Effectful functions say EFFECTFUL in the first line of the description; irreversible ones (deletes, `set_customers_mode`, `enable_products`) say IRREVERSIBLE.
- Descriptions are at most three lines. Shared API facts live once: in the `connector.yaml` header comment and README, not repeated per function. Those facts: 7 requests per second per integration (429 with Retry-After, no automatic retry); empty result is HTTP 204 with no body, not an empty list; `filter[...]` on leads, contacts, companies and customers may require the paid filtering add-on (check `get_account` with `is_api_filter_enabled`); HAL pagination via `_links.next` and `_page`; all v4 writes return the created or updated entities under `_embedded`.
- `response_pick` on read functions projects `_embedded.<collection>`, `_page`, `_links.next.href` (and `_total_items` where present) so the agent sees the page cursor without the HAL noise.
- `args_schema` omits `description` on self-evident fields (`id`, `page`, `limit`) to keep the prompt block short.

## Function inventory

R marks `read_only: true`. Paths are relative to `{{account.base_url}}/api/v4` unless noted. About 100 functions (the per-family tables below are authoritative).

### Account and dictionaries (6)

| Function | Method, path | Notes |
|---|---|---|
| R `get_account` | GET `/account` | `with` (comma list: `amojo_id,users_groups,task_types,version,entity_names,datetime_settings,drive_url,is_api_filter_enabled`). Healthcheck. |
| R `list_users` | GET `/users` | `with`, `page`, `limit`. Admin token only. |
| R `get_user` | GET `/users/{id}` | `with` |
| R `list_roles` | GET `/roles` | `with=users`, `page`, `limit` |
| R `list_event_types` | GET `/events/types` | `language_code` |
| R `list_loss_reasons` | GET `/leads/loss_reasons` | Documented only by Kommo; verified live. |

### Leads and unsorted (10)

| Function | Method, path | Notes |
|---|---|---|
| R `list_leads` | GET `/leads` | `with`, `query`, `page`, `limit`, `order_by` in `[created_at, updated_at, id]`, `order_dir`, `filter_id`, `filter_name`, `filter_pipeline_id`, `filter_status_id` (rendered as `filter[statuses][0][status_id]` and requires `filter_pipeline_id`, rendered as `filter[statuses][0][pipeline_id]`), `filter_responsible_user_id`, `filter_created_from`, `filter_created_to`, `filter_updated_from`, `filter_updated_to`, `filter_price_from`, `filter_price_to`. |
| R `get_lead` | GET `/leads/{id}` | `with` |
| `create_leads` | POST `/leads` | `records` |
| `update_lead` | PATCH `/leads/{id}` | `record` |
| `update_leads` | PATCH `/leads` | `records`, each with `id` |
| `create_leads_complex` | POST `/leads/complex` | `records`; one contact and one company per lead, at most 50. |
| R `list_unsorted` | GET `/leads/unsorted` | `page`, `limit`, `filter_uid`, `filter_category` in `[chats, forms, sip, mail]`, `filter_pipeline_id` |
| R `get_unsorted_summary` | GET `/leads/unsorted/summary` | `filter_pipeline_id`, `filter_created_from`, `filter_created_to` |
| `accept_unsorted` | POST `/leads/unsorted/{uid}/accept` | `user_id`, `status_id` optional |
| `decline_unsorted` | DELETE `/leads/unsorted/{uid}/decline` | |

Creating unsorted entries (`/unsorted/forms`, `/unsorted/sip`) is deferred: they are for form and telephony integrations, not agent workflows. Note this in README.

### Pipelines and statuses (9)

R `list_pipelines`, R `get_pipeline`, `create_pipelines` (records), `update_pipeline` (id, record), `delete_pipeline` (id; IRREVERSIBLE, refused by amoCRM when the pipeline holds leads), R `list_statuses` (pipeline_id, `with=descriptions`), `create_statuses` (pipeline_id, records), `update_status` (pipeline_id, id, record), `delete_status` (pipeline_id, id; leads move to the first stage). README documents the system statuses 142 (won) and 143 (lost).

### Contacts and companies (8)

R `list_contacts`, R `get_contact`, `create_contacts`, `update_contact`; R `list_companies`, R `get_company`, `create_companies`, `update_company`. Lists share the leads filter set minus price and status. Batch update by `records` is covered through the generic `update_entities(entity_type, records)` below to avoid four near-identical functions.

### Generic multi-entity writes (1)

`update_entities` PATCH `/{entity_type}` with `records` (each carries `id`). Covers batch updates for leads, contacts, companies and customers; `update_leads` stays as the explicit, most common alias. Tag attach and detach are done through this function or the single-entity updates with `tags_to_add` and `tags_to_delete`, as amoCRM has no separate tag-attach endpoint.

### Customers (10)

R `list_customers` (filters: id, name, responsible_user_id, status_id, next_date range, created and updated ranges), R `get_customer`, `create_customers`, `update_customer`, R `list_customer_statuses`, R `list_customer_segments`, R `list_transactions` (`customer_id` optional; `filter_id`), `add_transactions` (customer_id, records), `delete_transaction` (id; IRREVERSIBLE), `set_customers_mode` (mode; IRREVERSIBLE in practice, admin). Customer statuses and segments create, update and delete are deferred to keep the count down; README says so.

### Tasks (5)

R `list_tasks` (`filter_responsible_user_id`, `filter_is_completed` 0 or 1, `filter_task_type`, `filter_entity_type`, `filter_entity_id`, `filter_updated_from`, `filter_updated_to`, `page`, `limit`, `order_by`, `order_dir`), R `get_task`, `create_tasks` (records), `update_task` (id, record), `complete_task` (id, result_text; renders PATCH `/tasks/{id}` with `is_completed: true` and `result.text`).

### Notes, tags, links, events (12)

| Function | Method, path | Notes |
|---|---|---|
| R `list_notes` | GET `/{entity_type}/notes` | `filter_entity_id`, `filter_note_type`, `filter_updated_from`, `filter_updated_to`, `page`, `limit`, `order_by`, `order_dir` |
| R `get_note` | GET `/{entity_type}/notes/{id}` | |
| `create_notes` | POST `/{entity_type}/notes` | `records`, each with `entity_id`, `note_type`, `params`. Description lists the 10 note types once. |
| `update_note` | PATCH `/{entity_type}/notes/{id}` | `record` |
| `pin_note` | POST `/{entity_type}/notes/{id}/pin` | |
| `unpin_note` | POST `/{entity_type}/notes/{id}/unpin` | |
| R `list_tags` | GET `/{entity_type}/tags` | `query`, `filter_name`, `filter_id`, `page`, `limit` |
| `create_tags` | POST `/{entity_type}/tags` | `records` (name, color) |
| R `list_links` | GET `/{entity_type}/{entity_id}/links` | `filter_to_entity_type`, `filter_to_entity_id`, `filter_to_catalog_id` |
| `link_entities` | POST `/{entity_type}/{entity_id}/link` | `records` of `{to_entity_id, to_entity_type, metadata}` |
| `unlink_entities` | POST `/{entity_type}/{entity_id}/unlink` | same shape |
| R `list_events` | GET `/events` | `filter_entity` (single value from `[lead, contact, company, customer, task]`), `filter_entity_id`, `filter_type`, `filter_created_by`, `filter_created_from`, `filter_created_to`, `with`, `page`, `limit` max 100 |
| R `get_event` | GET `/events/{id}` | id is a ULID string |

### Custom fields (9)

R `list_custom_fields` (entity_type, `filter_type`, `page`, `limit`, `order_by` in `[sort, id]`, `order_dir`), R `get_custom_field` (entity_type, id), `create_custom_fields` (entity_type, records), `update_custom_field` (entity_type, id, record), `delete_custom_field` (entity_type, id; IRREVERSIBLE), R `list_custom_field_groups` (entity_type), `create_custom_field_groups` (entity_type, records), R `list_catalog_custom_fields` (catalog_id), `create_catalog_custom_fields` (catalog_id, records).

Implementation note: the engine percent-encodes `/` inside a substituted `{{args.x}}` (`encode_component` in `crates/apb-engine/src/connector/call/encode.rs`), so one function cannot switch between `/{entity_type}/custom_fields` and `/catalogs/{id}/custom_fields`. Catalog fields therefore get their own two functions; segment custom fields (`customers/segments/custom_fields`) are deferred and noted in README.

### Catalogs and products (9)

R `list_catalogs`, R `get_catalog`, `create_catalogs`, `update_catalog`, R `list_catalog_elements` (catalog_id, `query`, `filter_id`, `page`, `limit`), R `get_catalog_element`, `create_catalog_elements`, `update_catalog_element`, R `get_products_settings` (GET `/api/v2/products_settings`, absolute path outside v4), `enable_products` (POST `/api/v2/products_settings/`, IRREVERSIBLE).

### Administration and automation (12)

R `list_webhooks` (`filter_destination`), `subscribe_webhook` (destination, settings array of event names, sort; the description lists the event names once), `unsubscribe_webhook` (destination); R `list_bots` (`with=favorite`, page, limit), `run_bot` (bot_id, body with `entity_id`, `entity_type`), `stop_bot` (bot_id, body); R `list_sources` (`filter_external_id`), `create_sources` (records, max 50); R `list_chat_templates` (page, limit max 50); R `list_talks` (`filter_contact_id`, `filter_entity_id`, `filter_entity_type`, `filter_only_in_work`, page, limit), `close_talk` (id, force_close); R `list_subscriptions` (entity_type in `[leads, customers]`, entity_id); `create_short_links` (records of `{url, metadata: {entity_type: contacts, entity_id}}`); `add_calls` (records; description states the phone-matching rule and that unmatched numbers are silently dropped).

### Files (6, host `{{account.drive_url}}`)

R `list_files` (GET `{{account.drive_url}}/v1.0/files`, `filter_name`, `filter_term`, `filter_deleted`, `limit`), R `get_file` (`/v1.0/files/{uuid}`), R `list_entity_files` (GET `/api/v4/{entity_type}/{entity_id}/files`, `limit`, `before_id` cursor), `attach_files` (PUT `/api/v4/{entity_type}/{entity_id}/files`, records of `{file_uuid}`), `detach_files` (DELETE same path), R `list_file_links` (GET `/api/v4/files/{file_uuid}/links`). All require the "Files access" scope on the token; a missing `drive_url` account field makes the two drive-host functions fail at render time with a clear message.

## Grant presets (documented in README and CONNECTORS.md)

Playbook authors copy one preset into the node's `connectors[].functions` list instead of granting the whole connector:

- `sales-read`: `get_account, list_pipelines, list_statuses, list_leads, get_lead, list_contacts, get_contact, list_companies, get_company, list_tasks, list_notes, list_tags, list_users`.
- `sales-write`: `sales-read` plus `create_leads, create_leads_complex, update_lead, create_contacts, update_contact, create_companies, update_company, create_notes, create_tasks, complete_task, create_tags, link_entities`.
- `inbox`: `list_unsorted, get_unsorted_summary, accept_unsorted, decline_unsorted, list_talks, close_talk`.
- `setup-admin`: `list_pipelines, create_pipelines, update_pipeline, list_statuses, create_statuses, update_status, list_custom_fields, create_custom_fields, update_custom_field, list_custom_field_groups, list_users, list_roles, list_webhooks, subscribe_webhook, unsubscribe_webhook`.
- `catalog`: the nine catalog and product functions.
- `customers`: the ten customer functions.
- `files`: the six file functions.
- `functions: read_only` remains the built-in coarse split.

Measured target: a `sales-write` grant should render under 5,000 tokens of prompt block; the full connector under 20,000. Both are checked once during implementation by rendering the instruction block with a throwaway playbook and counting characters.

## Error handling

- 401 or 403: `auth`. README explains the usual causes: expired long-lived token, token without the Files scope, IP allowlist, rate block after repeated 429.
- 402: `service`; customers, catalogs, webhooks and filtering are tariff-gated. The README table maps each family to the tariff or add-on it needs.
- 204 on lists: the engine returns an empty body; `response_pick` yields nothing. Descriptions of list functions say "an empty page is HTTP 204 with no body".
- 400 with `validation-errors`: surfaced verbatim; `request_id` in each record lets the agent map errors to records in a batch.
- 429: `rate_limited` with `retry_after_sec`; playbooks bound loops with `max_calls`.

## Testing

Offline (CI gate):

- One `tests.yaml` case per function, asserting method, exact URL (with percent-encoded brackets) and `body_contains` for writes. `entity_type` functions get two cases (`leads` and `contacts`). Filter functions get one case with a range filter and one with the optional filters absent, proving the drop-when-absent behavior.
- `apb connector test --dir connectors/amocrm`, then `cargo test -p apb --test <suite> official_connectors_gate`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `code-ranker check .`.

Live verification is DEFERRED (owner decision, 2026-08-30): a trial amoCRM account cannot create a private integration without a legal application with personal documents, and the owner will not file one. The connector ships verified against the documentation and the offline contract only; `docs/CONNECTORS.md` and the release note state this plainly. The live plan below runs when a real client account with an integration token becomes available:

1. `apb connector call amocrm get_account --args '{"with":"drive_url,is_api_filter_enabled,task_types"}'`; record `drive_url` into the account config.
2. Every `read_only` function once, checking that `response_pick` paths exist in the real payload (HAL shapes differ per family) and that 204 is handled.
3. Write scenario, all names prefixed `apb-test-`: `create_contacts` -> `create_companies` -> `create_leads_complex` -> `create_notes` (common) -> `create_tasks` -> `complete_task` -> `update_lead` (status to a different stage) -> `create_tags` and attach through `update_lead` -> `link_entities` -> `create_short_links` -> `list_events` for the lead -> `subscribe_webhook` to a throwaway URL and `unsubscribe_webhook`. Optionally `create_statuses` and `create_custom_fields` followed by their deletes. Entities without DELETE remain in the account and are listed in the report for manual cleanup.
4. Catalog, customers and files functions are exercised only if the trial tariff enables them; otherwise the 402 mapping is confirmed and noted.
5. Every mismatch between the documented and the real payload is fixed in the connector before the PR.

## Rollout

- Version `0.21.0`, release note `docs/release-notes/v0.21.0.md`.
- PR after owner approval only; DCO signoff on the commit.
- Follow-up issues to file (apb feedback loop): (a) connector-side function groups or a short prompt mode so large connectors do not need hand-written presets; (b) form-encoded, unsigned or IP-allowlisted webhook ingest mode, which amoCRM needs; (c) multi-value query keys.
