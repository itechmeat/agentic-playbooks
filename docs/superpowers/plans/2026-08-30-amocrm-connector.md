# amoCRM Connector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `connectors/amocrm/`, the fifteenth official connector, covering amoCRM REST API v4 (and Kommo) with about 100 functions over a long-lived Bearer token, with a low per-node prompt cost.

**Architecture:** A pure connector folder (`connector.yaml`, `tests.yaml`, `PUBLIC.md`, `README.md`, `INSTALL.md`, empty `skills/`) embedded into the binary by rust-embed; the only Rust change is adding the name to the pinned list test in `crates/apb-core/src/connector/official.rs`. Functions are grouped by resource family; symmetric amoCRM routes are collapsed with an `entity_type` enum argument. Docs carry grant presets so playbooks grant 5 to 15 functions, not the whole surface.

**Tech Stack:** APB connector YAML (JSON Schema `args_schema`, `response_pick`, `query` templates), `apb connector test --dir`, the CI gate test `every_official_connector_folder_is_complete`, Rust toolchain for the gate only.

**Spec:** `docs/superpowers/specs/2026-08-30-amocrm-connector-design.md`

## Global Constraints

- No em-dashes (U+2014), no exclamation marks, no CJK in any file. Machine fields English.
- Connector name `amocrm`; function and argument names snake_case, `[a-z0-9_]`, at most 64 chars.
- `connector.yaml` is `deny_unknown_fields`: only `name, version, healthcheck, auth, error_when, webhook, account_fields, functions` at top level; per function only `name, description, read_only, deprecated, method, url, query, headers, body, body_form, args_schema, examples, response_pick, timeout_sec` (or `mock`).
- `{{secret.*}}` only inside `auth`. `{{args.X}}` in a `url` must be listed in `args_schema.required`. Every `read_only: true` function must have a non-empty `response_pick`. Every function needs at least one `tests.yaml` case. Every example must validate against its `args_schema`.
- Descriptions at most three lines of prose; no per-function repetition of shared API facts (they live in the header comment and README).
- Effectful functions start their description with `EFFECTFUL.`; irreversible ones with `EFFECTFUL, IRREVERSIBLE.`
- Query values that are exactly `{{args.x}}` are dropped when the arg is absent; never build mixed templates like `"{{args.a}},{{args.b}}"` for optional args.
- `/` inside a substituted `{{args.x}}` is percent-encoded to `%2F`: never pass path fragments through an arg.
- Commit only after the owner approves; `git commit --signoff`; never push without approval. Never write the live token into any file other than the global `secrets.env`, never into chat summaries, commits or issues.
- Gates before "ready": `apb connector test --dir connectors/amocrm`, `cargo test -p apb --test main every_official_connector_folder_is_complete`, `cargo test -p apb-core official`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo metadata --format-version 1 >/dev/null && code-ranker check .`.

## Shared YAML building blocks

Every task below reuses these fragments verbatim. Names are fixed; later tasks rely on them.

**Header, auth, account fields** (Task 1 writes them):

```yaml
name: amocrm
version: 0.1.0
# amoCRM (amocrm.ru) and its international twin Kommo (kommo.com) expose the
# same REST API v4 under https://<subdomain>.<tld>/api/v4/... base_url is
# scheme plus host only, no trailing path; the account-specific Files API
# host (drive_url) comes from get_account with=drive_url and is a second,
# optional account field used only by list_files and get_file.
#
# Shared API facts, stated once here and in README.md rather than in every
# function: the rate limit is 7 requests per second per integration (HTTP
# 429 with Retry-After, no automatic retry in apb); an empty list result is
# HTTP 204 with no body, not an empty array; list responses are HAL, the
# items sit under _embedded.<collection> and the next page under
# _links.next.href; page starts at 1 and limit caps at 250 (100 for
# events, 50 for chat templates); filter[...] on leads, contacts, companies
# and customers may need the paid filtering add-on (get_account with
# is_api_filter_enabled says whether it is on); batch writes take an array
# of records, 50 recommended and 250 hard cap, each record may carry a
# request_id that is echoed back; v4 has no DELETE for leads, contacts,
# companies, customers, tasks, notes, tags or catalog elements.
#
# Argument naming: call arguments are snake_case. Single-value filters are
# exposed as filter_<field> (rendered filter[<field>]) and ranges as
# filter_<field>_from / filter_<field>_to (rendered filter[<field>][from|to]);
# a filter can take exactly one value because a query map cannot repeat a
# key. Record bodies (record, records) are forwarded verbatim in amoCRM's
# own JSON shape: custom_fields_values, _embedded, tags_to_add,
# tags_to_delete, responsible_user_id, and so on.
#
# entity_type is the enum [leads, contacts, companies, customers] wherever
# amoCRM's routes are symmetric (notes, tags, links, custom fields, files,
# batch updates); one function replaces four.
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.access_token}}"
account_fields:
  - name: base_url
    required: true
  - name: drive_url
    required: false
  - name: access_token
    required: true
    secret: true
healthcheck: get_account
functions:
```

**Reusable schema fragments** (copy inline; YAML anchors are not used because the manifest is compared by digest and anchors hurt readability):

```yaml
# entity_type property
entity_type: { type: string, enum: [leads, contacts, companies, customers] }
# paging
page: { type: integer, minimum: 1 }
limit: { type: integer, minimum: 1, maximum: 250 }
# ordering (entity lists): see the ordering rendering rule below
order_created_at: { type: string, enum: [asc, desc] }
order_updated_at: { type: string, enum: [asc, desc] }
order_id: { type: string, enum: [asc, desc] }
# batch records
records:
  type: array
  minItems: 1
  maxItems: 250
  items: { type: object, additionalProperties: true }
# single record
record: { type: object, additionalProperties: true }
```

**Ordering rendering rule:** amoCRM takes `order[created_at]=asc`. Because the query key is literal and cannot embed an arg, expose three fixed keys and let the agent set one:

```yaml
    query:
      order[created_at]: "{{args.order_created_at}}"
      order[updated_at]: "{{args.order_updated_at}}"
      order[id]: "{{args.order_id}}"
```
with properties `order_created_at`, `order_updated_at`, `order_id`, each `{ type: string, enum: [asc, desc] }`. This replaces the `order_by`/`order_dir` pair mentioned in the spec; the spec's intent (sort by one of the three fields) is preserved. For custom fields the keys are `order[sort]` and `order[id]` with `order_sort`, `order_id`.

**Standard list `response_pick`:** `[_embedded.<collection>, _page, _links.next.href]` where `<collection>` is the HAL key of that family (`leads`, `contacts`, `notes`, `custom_fields`, and so on). For `entity_type` functions the collection key does not depend on the entity type (`notes`, `tags`, `links`, `custom_fields`, `files`), so one fixed pick works.

**tests.yaml account stub for every case:** `account: { base_url: "https://example.amocrm.ru", drive_url: "https://drive-b.amocrm.ru" }`.

---

### Task 1: Scaffold, registration, healthcheck

**Files:**
- Create: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`, `connectors/amocrm/PUBLIC.md`, `connectors/amocrm/README.md`, `connectors/amocrm/INSTALL.md`, `connectors/amocrm/skills/.gitkeep`
- Modify: `crates/apb-core/src/connector/official.rs:169-170` (pinned list), `.gitignore`

**Interfaces:**
- Produces: the manifest header from "Shared YAML building blocks" and the function `get_account(with)`; later tasks append functions under `functions:` and cases under `cases:`.

- [ ] **Step 1: Check the gate fails on the missing connector**

Run: `cargo test -p apb-core official -- every_official_connector_carries_the_full_file_set`
Expected: PASS today (14 names). After Step 2 it must fail with "the embedded official connector set changed" until Step 4 fixes the list. Record that.

- [ ] **Step 2: Write the manifest header and the healthcheck function**

`connectors/amocrm/connector.yaml`: the header block from "Shared YAML building blocks", then:

```yaml
  # -- Account and dictionaries -------------------------------------------

  - name: get_account
    description: >-
      Account settings and feature flags. with is a comma list from
      amojo_id, users_groups, task_types, version, entity_names,
      datetime_settings, drive_url, is_api_filter_enabled, invoices_settings.
    read_only: true
    method: GET
    url: "{{account.base_url}}/api/v4/account"
    query:
      with: "{{args.with}}"
    args_schema:
      type: object
      properties:
        with: { type: string, description: "comma-joined include list, e.g. drive_url,is_api_filter_enabled,task_types" }
      required: []
    examples:
      - args: { with: "drive_url,is_api_filter_enabled,task_types" }
        note: "drive_url is the Files API host to copy into the drive_url account field; is_api_filter_enabled says whether filter_* arguments will work on leads, contacts, companies and customers."
    response_pick: [id, name, subdomain, currency, country, drive_url, is_api_filter_enabled, _embedded]
```

- [ ] **Step 3: Write tests.yaml with the first case**

```yaml
cases:
  # -- Account ----------------------------------------------------------------

  - function: get_account
    account: { base_url: "https://example.amocrm.ru", drive_url: "https://drive-b.amocrm.ru" }
    args: {}
    expect:
      method: GET
      url: "https://example.amocrm.ru/api/v4/account"

  - function: get_account
    account: { base_url: "https://example.amocrm.ru", drive_url: "https://drive-b.amocrm.ru" }
    args: { with: "drive_url,task_types" }
    expect:
      method: GET
      url: "https://example.amocrm.ru/api/v4/account?with=drive_url%2Ctask_types"
```

- [ ] **Step 4: Write minimal PUBLIC.md, README.md, INSTALL.md**

`PUBLIC.md`:
```markdown
---
display_name: amoCRM
summary: Leads, contacts, companies, customers, pipelines, tasks, notes, custom fields, catalogs, events, webhooks and files in amoCRM and Kommo over REST API v4.
tags: [amocrm, kommo, crm, sales]
publisher: apb
---

Body is completed in Task 7.
```

`README.md` must start with `# amocrm: setup for humans` and contain the literal path `connectors/amocrm/INSTALL.md`. `INSTALL.md` must start with `# amocrm: installation instructions for an agent`. Both get one placeholder paragraph "Completed in Task 7." which Task 7 replaces (the gate does not read the bodies).

- [ ] **Step 5: Register the connector and protect the secrets file**

`crates/apb-core/src/connector/official.rs`: in the `vec![...]` inside `every_official_connector_carries_the_full_file_set`, insert `"amocrm",` between `"asana",` and `"atrip",` (alphabetical). Run `cargo fmt --all`.

`.gitignore`: append after `.apb/workdir.lock`:
```
.apb/secrets.env
```

- [ ] **Step 6: Run the offline test and the gates**

Run: `cargo build -p apb && ./target/debug/apb connector test --dir connectors/amocrm`
Expected: 2 cases pass.
Run: `cargo test -p apb-core official` and `cargo test -p apb --test main every_official_connector_folder_is_complete`
Expected: PASS.

- [ ] **Step 7: Stop for owner approval before committing.** Proposed message: `feat(connectors): scaffold the amocrm connector with account healthcheck`.

---

### Task 2: Dictionaries, leads, unsorted, pipelines, statuses

**Files:**
- Modify: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`

**Interfaces:**
- Consumes: header and `get_account` from Task 1.
- Produces: functions `list_users, get_user, list_roles, list_event_types, list_loss_reasons, list_leads, get_lead, create_leads, update_lead, update_leads, create_leads_complex, list_unsorted, get_unsorted_summary, accept_unsorted, decline_unsorted, list_pipelines, get_pipeline, create_pipelines, update_pipeline, delete_pipeline, list_statuses, create_statuses, update_status, delete_status`.

- [ ] **Step 1: Add the failing test cases first**

Append to `tests.yaml` one case per function (two for `list_leads`). Exact expectations:

| function | args | expect |
|---|---|---|
| list_users | `{ with: "role,group", limit: 50 }` | GET `.../api/v4/users?limit=50&with=role%2Cgroup` |
| get_user | `{ id: 123 }` | GET `.../api/v4/users/123` |
| list_roles | `{}` | GET `.../api/v4/roles` |
| list_event_types | `{ language_code: "ru" }` | GET `.../api/v4/events/types?language_code=ru` |
| list_loss_reasons | `{}` | GET `.../api/v4/leads/loss_reasons` |
| list_leads | `{}` | GET `.../api/v4/leads` |
| list_leads | `{ filter_pipeline_id: 100, filter_status_id: 200, filter_updated_from: 1700000000, limit: 10, order_updated_at: "desc", with: "contacts" }` | GET `.../api/v4/leads?filter%5Bstatuses%5D%5B0%5D%5Bpipeline_id%5D=100&filter%5Bstatuses%5D%5B0%5D%5Bstatus_id%5D=200&filter%5Bupdated_at%5D%5Bfrom%5D=1700000000&limit=10&order%5Bupdated_at%5D=desc&with=contacts` |
| get_lead | `{ id: 5, with: "contacts,loss_reason" }` | GET `.../api/v4/leads/5?with=contacts%2Closs_reason` |
| create_leads | `{ records: [{ name: "apb-test-lead", price: 100 }] }` | POST `.../api/v4/leads`, body_contains is not usable on a top-level array, so assert `method` and `url` only |
| update_lead | `{ id: 5, record: { status_id: 142 } }` | PATCH `.../api/v4/leads/5`, body_contains `{ status_id: 142 }` |
| update_leads | `{ records: [{ id: 5, price: 200 }] }` | PATCH `.../api/v4/leads` |
| create_leads_complex | `{ records: [{ name: "apb-test", _embedded: { contacts: [{ first_name: "A" }] } }] }` | POST `.../api/v4/leads/complex` |
| list_unsorted | `{ filter_category: "forms", limit: 20 }` | GET `.../api/v4/leads/unsorted?filter%5Bcategory%5D=forms&limit=20` |
| get_unsorted_summary | `{ filter_pipeline_id: 100 }` | GET `.../api/v4/leads/unsorted/summary?filter%5Bpipeline_id%5D=100` |
| accept_unsorted | `{ uid: "abc", user_id: 7, status_id: 200 }` | POST `.../api/v4/leads/unsorted/abc/accept`, body_contains `{ user_id: 7, status_id: 200 }` |
| decline_unsorted | `{ uid: "abc" }` | DELETE `.../api/v4/leads/unsorted/abc/decline` |
| list_pipelines | `{}` | GET `.../api/v4/leads/pipelines` |
| get_pipeline | `{ id: 100 }` | GET `.../api/v4/leads/pipelines/100` |
| create_pipelines | `{ records: [{ name: "apb-test-pipeline", sort: 10, is_main: false }] }` | POST `.../api/v4/leads/pipelines` |
| update_pipeline | `{ id: 100, record: { name: "renamed" } }` | PATCH `.../api/v4/leads/pipelines/100`, body_contains `{ name: "renamed" }` |
| delete_pipeline | `{ id: 100 }` | DELETE `.../api/v4/leads/pipelines/100` |
| list_statuses | `{ pipeline_id: 100, with: "descriptions" }` | GET `.../api/v4/leads/pipelines/100/statuses?with=descriptions` |
| create_statuses | `{ pipeline_id: 100, records: [{ name: "apb-test-status", sort: 50, color: "#fffeb2" }] }` | POST `.../api/v4/leads/pipelines/100/statuses` |
| update_status | `{ pipeline_id: 100, id: 200, record: { name: "renamed" } }` | PATCH `.../api/v4/leads/pipelines/100/statuses/200`, body_contains `{ name: "renamed" }` |
| delete_status | `{ pipeline_id: 100, id: 200 }` | DELETE `.../api/v4/leads/pipelines/100/statuses/200` |

Note: the query string is emitted in BTreeMap key order (alphabetical by literal key), which is why `filter[...]` precedes `limit`, `order[...]` and `with`.

- [ ] **Step 2: Run to verify failure**

Run: `./target/debug/apb connector test --dir connectors/amocrm`
Expected: fails with unknown function names.

- [ ] **Step 3: Implement the functions**

Full YAML for the two patterns every other function follows (a list with filters, and a batch write); the remaining functions are the same shapes with the method, url, query and args from the table above.

```yaml
  - name: list_leads
    description: >-
      List leads. filter_status_id needs filter_pipeline_id (rendered as one
      filter[statuses][0] pair). with: catalog_elements, is_price_modified_by_robot,
      loss_reason, contacts, only_deleted, source_id.
    read_only: true
    method: GET
    url: "{{account.base_url}}/api/v4/leads"
    query:
      with: "{{args.with}}"
      query: "{{args.query}}"
      page: "{{args.page}}"
      limit: "{{args.limit}}"
      order[created_at]: "{{args.order_created_at}}"
      order[updated_at]: "{{args.order_updated_at}}"
      order[id]: "{{args.order_id}}"
      filter[id]: "{{args.filter_id}}"
      filter[name]: "{{args.filter_name}}"
      filter[pipeline_id]: "{{args.filter_pipeline_id_only}}"
      filter[statuses][0][pipeline_id]: "{{args.filter_pipeline_id}}"
      filter[statuses][0][status_id]: "{{args.filter_status_id}}"
      filter[responsible_user_id]: "{{args.filter_responsible_user_id}}"
      filter[created_at][from]: "{{args.filter_created_from}}"
      filter[created_at][to]: "{{args.filter_created_to}}"
      filter[updated_at][from]: "{{args.filter_updated_from}}"
      filter[updated_at][to]: "{{args.filter_updated_to}}"
      filter[closed_at][from]: "{{args.filter_closed_from}}"
      filter[closed_at][to]: "{{args.filter_closed_to}}"
      filter[price][from]: "{{args.filter_price_from}}"
      filter[price][to]: "{{args.filter_price_to}}"
    args_schema:
      type: object
      properties:
        with: { type: string }
        query: { type: string, description: "full-text search over filled fields" }
        page: { type: integer, minimum: 1 }
        limit: { type: integer, minimum: 1, maximum: 250 }
        order_created_at: { type: string, enum: [asc, desc] }
        order_updated_at: { type: string, enum: [asc, desc] }
        order_id: { type: string, enum: [asc, desc] }
        filter_id: { type: integer }
        filter_name: { type: string }
        filter_pipeline_id_only: { type: integer, description: "filter[pipeline_id] without a status; use filter_pipeline_id plus filter_status_id to filter by stage" }
        filter_pipeline_id: { type: integer, description: "pipeline of the stage filter; pair with filter_status_id" }
        filter_status_id: { type: integer, description: "stage id; 142 won, 143 lost" }
        filter_responsible_user_id: { type: integer }
        filter_created_from: { type: integer, description: "unix seconds" }
        filter_created_to: { type: integer }
        filter_updated_from: { type: integer }
        filter_updated_to: { type: integer }
        filter_closed_from: { type: integer }
        filter_closed_to: { type: integer }
        filter_price_from: { type: integer }
        filter_price_to: { type: integer }
      required: []
      dependentRequired:
        filter_status_id: [filter_pipeline_id]
    examples:
      - args: { filter_pipeline_id: 100, filter_status_id: 200, limit: 50, order_updated_at: "desc" }
        note: "every argument is optional and an omitted one is dropped from the query; an empty page is HTTP 204 with no body."
    response_pick: [_embedded.leads, _page, _links.next.href]
```

```yaml
  - name: create_leads
    description: >-
      EFFECTFUL. Create leads in one batch. Each record is amoCRM's lead
      model (name, price, pipeline_id, status_id, responsible_user_id,
      custom_fields_values, _embedded.tags/contacts/companies, request_id).
    method: POST
    url: "{{account.base_url}}/api/v4/leads"
    body: "{{args.records}}"
    args_schema:
      type: object
      properties:
        records:
          type: array
          minItems: 1
          maxItems: 250
          items: { type: object, additionalProperties: true }
      required: [records]
    examples:
      - args: { records: [{ name: "Website order 1042", price: 15000, pipeline_id: 100, status_id: 200, request_id: "r1" }] }
        note: "50 records per call is the vendor recommendation; request_id is echoed in _embedded.leads[].request_id."
    response_pick: [_embedded.leads]
```

Rules for the rest: `get_*` functions have `required: [id]` and pick the whole body (`response_pick: [id, name, _embedded]` for leads; for users `[id, name, email, rights.is_admin, rights.is_active]`; for pipelines `[id, name, sort, is_main, _embedded.statuses]`). Delete functions: `description: "EFFECTFUL, IRREVERSIBLE. ..."`, no body, `required: [id]` (plus `pipeline_id` for statuses). `accept_unsorted` body is `{ user_id: "{{args.user_id}}", status_id: "{{args.status_id}}" }`, `required: [uid]`. `create_leads_complex` description notes: one contact and one company per lead, at most 50 leads, participates in duplicate control. `update_lead` uses `body: "{{args.record}}"`, `required: [id, record]`. `list_unsorted` filters: `filter[uid]`, `filter[category]` (enum `[chats, forms, sip, mail]`), `filter[pipeline_id]`. `get_unsorted_summary` filters: `filter[uid]`, `filter[pipeline_id]`, `filter[created_at][from]`, `filter[created_at][to]`. `list_statuses` query `with` (enum-free string, documented as `descriptions`). `list_loss_reasons` description states it is documented only by Kommo and verified live.

- [ ] **Step 4: Run tests and the gate**

Run: `./target/debug/apb connector test --dir connectors/amocrm` then `cargo test -p apb --test main every_official_connector_folder_is_complete`
Expected: PASS.

- [ ] **Step 5: Stop for owner approval before committing.** Message: `feat(connectors): amocrm leads, unsorted, pipelines and dictionaries`.

---

### Task 3: Contacts, companies, batch updates, customers, tasks

**Files:**
- Modify: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`

**Interfaces:**
- Produces: `list_contacts, get_contact, create_contacts, update_contact, list_companies, get_company, create_companies, update_company, update_entities, list_customers, get_customer, create_customers, update_customer, list_customer_statuses, list_customer_segments, list_transactions, add_transactions, delete_transaction, set_customers_mode, list_tasks, get_task, create_tasks, update_task, complete_task`.

- [ ] **Step 1: Add the failing test cases**

| function | args | expect |
|---|---|---|
| list_contacts | `{ query: "ivanov", limit: 5 }` | GET `.../api/v4/contacts?limit=5&query=ivanov` |
| get_contact | `{ id: 9, with: "leads" }` | GET `.../api/v4/contacts/9?with=leads` |
| create_contacts | `{ records: [{ first_name: "apb-test", last_name: "Contact" }] }` | POST `.../api/v4/contacts` |
| update_contact | `{ id: 9, record: { name: "Renamed" } }` | PATCH `.../api/v4/contacts/9`, body_contains `{ name: "Renamed" }` |
| list_companies | `{ filter_name: "Acme" }` | GET `.../api/v4/companies?filter%5Bname%5D=Acme` |
| get_company | `{ id: 3 }` | GET `.../api/v4/companies/3` |
| create_companies | `{ records: [{ name: "apb-test-company" }] }` | POST `.../api/v4/companies` |
| update_company | `{ id: 3, record: { name: "Acme LLC" } }` | PATCH `.../api/v4/companies/3`, body_contains `{ name: "Acme LLC" }` |
| update_entities | `{ entity_type: "contacts", records: [{ id: 9, name: "X" }] }` | PATCH `.../api/v4/contacts` |
| update_entities | `{ entity_type: "companies", records: [{ id: 3, name: "Y" }] }` | PATCH `.../api/v4/companies` |
| list_customers | `{ filter_status_id: 5, limit: 10 }` | GET `.../api/v4/customers?filter%5Bstatus_id%5D=5&limit=10` |
| get_customer | `{ id: 11 }` | GET `.../api/v4/customers/11` |
| create_customers | `{ records: [{ name: "apb-test-customer", next_date: 1700000000 }] }` | POST `.../api/v4/customers` |
| update_customer | `{ id: 11, record: { next_price: 500 } }` | PATCH `.../api/v4/customers/11`, body_contains `{ next_price: 500 }` |
| list_customer_statuses | `{}` | GET `.../api/v4/customers/statuses` |
| list_customer_segments | `{}` | GET `.../api/v4/customers/segments` |
| list_transactions | `{ customer_id: 11 }` | GET `.../api/v4/customers/11/transactions` |
| add_transactions | `{ customer_id: 11, records: [{ price: 500, comment: "apb-test" }] }` | POST `.../api/v4/customers/11/transactions` |
| delete_transaction | `{ id: 77 }` | DELETE `.../api/v4/customers/transactions/77` |
| set_customers_mode | `{ mode: "segments" }` | PATCH `.../api/v4/customers/mode`, body_contains `{ mode: "segments" }` |
| list_tasks | `{ filter_is_completed: 0, filter_responsible_user_id: 7, limit: 20 }` | GET `.../api/v4/tasks?filter%5Bis_completed%5D=0&filter%5Bresponsible_user_id%5D=7&limit=20` |
| get_task | `{ id: 40 }` | GET `.../api/v4/tasks/40` |
| create_tasks | `{ records: [{ text: "apb-test call", complete_till: 1700003600, entity_id: 5, entity_type: "leads", task_type_id: 1 }] }` | POST `.../api/v4/tasks` |
| update_task | `{ id: 40, record: { text: "changed" } }` | PATCH `.../api/v4/tasks/40`, body_contains `{ text: "changed" }` |
| complete_task | `{ id: 40, result_text: "Done" }` | PATCH `.../api/v4/tasks/40`, body_contains `{ is_completed: true, result: { text: "Done" } }` |

- [ ] **Step 2: Run to verify failure.** `./target/debug/apb connector test --dir connectors/amocrm` fails on unknown functions.

- [ ] **Step 3: Implement**

`list_contacts` and `list_companies` copy `list_leads` minus the price, closed_at and statuses filters, plus `filter[pipeline_id]` dropped; `with` values documented: contacts `catalog_elements, leads, customers`; companies `catalog_elements, leads, customers, contacts`. `response_pick: [_embedded.contacts, _page, _links.next.href]` and `[_embedded.companies, ...]`.

`update_entities`:
```yaml
  - name: update_entities
    description: >-
      EFFECTFUL. Batch update leads, contacts, companies or customers; each
      record carries id plus the fields to change (tags_to_add and
      tags_to_delete attach or detach tags by id or name).
    method: PATCH
    url: "{{account.base_url}}/api/v4/{{args.entity_type}}"
    body: "{{args.records}}"
    args_schema:
      type: object
      properties:
        entity_type: { type: string, enum: [leads, contacts, companies, customers] }
        records:
          type: array
          minItems: 1
          maxItems: 250
          items:
            type: object
            properties:
              id: { type: integer }
            required: [id]
            additionalProperties: true
      required: [entity_type, records]
    examples:
      - args: { entity_type: "leads", records: [{ id: 5, tags_to_add: [{ name: "vip" }] }] }
        note: "the only tag attach path in v4; there is no separate endpoint."
    response_pick: [_embedded]
```

`list_customers` filters: `filter[id]`, `filter[name]`, `filter[responsible_user_id]`, `filter[status_id]`, `filter[next_date][from|to]`, `filter[created_at][from|to]`, `filter[updated_at][from|to]`, `filter[next_price][from|to]`; `with` documented as `catalog_elements, contacts, companies`. `list_transactions` url `{{account.base_url}}/api/v4/customers/{{args.customer_id}}/transactions`, `required: [customer_id]`, query `filter[id]`, `page`, `limit`. `set_customers_mode` body `{ mode: "{{args.mode}}", is_enabled: "{{args.is_enabled}}" }`, `mode` enum `[segments, periodicity]`, `is_enabled` boolean optional, description `EFFECTFUL, IRREVERSIBLE. Enable or switch the customers mode; admin only, changes account behaviour.` Customers functions mention in one clause: `HTTP 402 when the tariff has no customers`.

`list_tasks` query: `filter[responsible_user_id]`, `filter[is_completed]` (integer enum `[0, 1]`), `filter[task_type]`, `filter[entity_type]` (enum `[leads, contacts, companies, customers]`), `filter[entity_id]`, `filter[id]`, `filter[updated_at][from]`, `filter[updated_at][to]`, `page`, `limit`, `order[created_at]`, `order[complete_till]`, `order[id]`. `complete_task`:
```yaml
  - name: complete_task
    description: >-
      EFFECTFUL. Mark a task completed with a result text (PATCH is_completed
      plus result.text).
    method: PATCH
    url: "{{account.base_url}}/api/v4/tasks/{{args.id}}"
    body:
      is_completed: true
      result:
        text: "{{args.result_text}}"
    args_schema:
      type: object
      properties:
        id: { type: integer }
        result_text: { type: string }
      required: [id, result_text]
    examples:
      - args: { id: 40, result_text: "Called, agreed on a demo" }
        note: "task_type ids come from get_account with=task_types; 1 is call, 2 is meeting."
    response_pick: [id, is_completed, result]
```

- [ ] **Step 4: Run tests and the gate.** Expected: PASS.
- [ ] **Step 5: Stop for owner approval before committing.** Message: `feat(connectors): amocrm contacts, companies, customers and tasks`.

---

### Task 4: Notes, tags, links, events

**Files:**
- Modify: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`

**Interfaces:**
- Produces: `list_notes, get_note, create_notes, update_note, pin_note, unpin_note, list_tags, create_tags, list_links, link_entities, unlink_entities, list_events, get_event`.

- [ ] **Step 1: Add the failing test cases**

| function | args | expect |
|---|---|---|
| list_notes | `{ entity_type: "leads", filter_entity_id: 5, filter_note_type: "common" }` | GET `.../api/v4/leads/notes?filter%5Bentity_id%5D=5&filter%5Bnote_type%5D=common` |
| list_notes | `{ entity_type: "contacts", limit: 10 }` | GET `.../api/v4/contacts/notes?limit=10` |
| get_note | `{ entity_type: "leads", id: 88 }` | GET `.../api/v4/leads/notes/88` |
| create_notes | `{ entity_type: "leads", records: [{ entity_id: 5, note_type: "common", params: { text: "apb-test note" } }] }` | POST `.../api/v4/leads/notes` |
| update_note | `{ entity_type: "leads", id: 88, record: { params: { text: "edited" } } }` | PATCH `.../api/v4/leads/notes/88`, body_contains `{ params: { text: "edited" } }` |
| pin_note | `{ entity_type: "leads", id: 88 }` | POST `.../api/v4/leads/notes/88/pin` |
| unpin_note | `{ entity_type: "leads", id: 88 }` | POST `.../api/v4/leads/notes/88/unpin` |
| list_tags | `{ entity_type: "contacts", query: "vip" }` | GET `.../api/v4/contacts/tags?query=vip` |
| create_tags | `{ entity_type: "leads", records: [{ name: "apb-test-tag", color: "EBEBEB" }] }` | POST `.../api/v4/leads/tags` |
| list_links | `{ entity_type: "leads", entity_id: 5, filter_to_entity_type: "contacts" }` | GET `.../api/v4/leads/5/links?filter%5Bto_entity_type%5D=contacts` |
| link_entities | `{ entity_type: "leads", entity_id: 5, records: [{ to_entity_id: 9, to_entity_type: "contacts", metadata: { is_main: true } }] }` | POST `.../api/v4/leads/5/link` |
| unlink_entities | `{ entity_type: "leads", entity_id: 5, records: [{ to_entity_id: 9, to_entity_type: "contacts" }] }` | POST `.../api/v4/leads/5/unlink` |
| list_events | `{ filter_entity: "lead", filter_entity_id: 5, filter_type: "lead_status_changed", limit: 50 }` | GET `.../api/v4/events?filter%5Bentity%5D=lead&filter%5Bentity_id%5D=5&filter%5Btype%5D=lead_status_changed&limit=50` |
| get_event | `{ id: "01pz58t6p04ymgsgfbmfyfy1mf" }` | GET `.../api/v4/events/01pz58t6p04ymgsgfbmfyfy1mf` |

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

`create_notes` description lists the note types once: `note_type: common, call_in, call_out, service_message, extended_service_message, message_cashier, geolocation, sms_in, sms_out, attachment; params per type (common: text; call_in/call_out: uniq, duration, source, link, phone, call_responsible; service_message: service, text; sms_in/sms_out: text, phone).` Items schema: `properties: { entity_id: integer, note_type: string enum of the ten, params: object }`, `required: [entity_id, note_type]`. `list_notes` query: `filter[entity_id]`, `filter[note_type]`, `filter[id]`, `filter[updated_at][from]`, `filter[updated_at][to]`, `page`, `limit`, `order[updated_at]`, `order[id]`; `response_pick: [_embedded.notes, _page, _links.next.href]`. `list_tags` query `query`, `filter[name]`, `filter[id]`, `page`, `limit`; pick `[_embedded.tags, _page, _links.next.href]`. `create_tags` items `{ name: string (required), color: string }`. Links: `to_entity_type` enum `[leads, contacts, companies, customers, catalog_elements]`; `metadata` object with `is_main`, `quantity`, `catalog_id`, `price_id` documented in the description; `list_links` query `filter[to_entity_id]`, `filter[to_entity_type]`, `filter[to_catalog_id]`, pick `[_embedded.links]`. `list_events`: `limit` max 100; query `filter[entity]` (enum `[lead, contact, company, customer, task]`), `filter[entity_id]`, `filter[type]`, `filter[created_by]`, `filter[created_at][from]`, `filter[created_at][to]`, `with`, `page`, `limit`; description says `filter_entity_id requires filter_entity` and names the most useful types in one line: `lead_added, lead_status_changed, lead_linked, task_added, task_completed, incoming_chat_message, outgoing_chat_message, custom_field_value_changed, entity_responsible_changed; list_event_types returns the full set.` Use `dependentRequired: { filter_entity_id: [filter_entity] }`. `get_event` id is `{ type: string }`, pick `[id, type, entity_id, entity_type, created_by, created_at, value_after, value_before]`.

- [ ] **Step 4: Run tests and the gate.** Expected: PASS.
- [ ] **Step 5: Stop for owner approval before committing.** Message: `feat(connectors): amocrm notes, tags, links and events`.

---

### Task 5: Custom fields, catalogs, products

**Files:**
- Modify: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`

**Interfaces:**
- Produces: `list_custom_fields, get_custom_field, create_custom_fields, update_custom_field, delete_custom_field, list_custom_field_groups, create_custom_field_groups, list_catalog_custom_fields, create_catalog_custom_fields, list_catalogs, get_catalog, create_catalogs, update_catalog, list_catalog_elements, get_catalog_element, create_catalog_elements, update_catalog_element, get_products_settings, enable_products`.

- [ ] **Step 1: Add the failing test cases**

| function | args | expect |
|---|---|---|
| list_custom_fields | `{ entity_type: "leads", filter_type: "select", limit: 50 }` | GET `.../api/v4/leads/custom_fields?filter%5Btype%5D%5B0%5D=select&limit=50` |
| list_custom_fields | `{ entity_type: "contacts" }` | GET `.../api/v4/contacts/custom_fields` |
| get_custom_field | `{ entity_type: "leads", id: 300 }` | GET `.../api/v4/leads/custom_fields/300` |
| create_custom_fields | `{ entity_type: "leads", records: [{ name: "apb-test-field", type: "text" }] }` | POST `.../api/v4/leads/custom_fields` |
| update_custom_field | `{ entity_type: "leads", id: 300, record: { name: "renamed" } }` | PATCH `.../api/v4/leads/custom_fields/300`, body_contains `{ name: "renamed" }` |
| delete_custom_field | `{ entity_type: "leads", id: 300 }` | DELETE `.../api/v4/leads/custom_fields/300` |
| list_custom_field_groups | `{ entity_type: "contacts" }` | GET `.../api/v4/contacts/custom_fields/groups` |
| create_custom_field_groups | `{ entity_type: "leads", records: [{ name: "apb-test-group", sort: 10 }] }` | POST `.../api/v4/leads/custom_fields/groups` |
| list_catalog_custom_fields | `{ catalog_id: 500 }` | GET `.../api/v4/catalogs/500/custom_fields` |
| create_catalog_custom_fields | `{ catalog_id: 500, records: [{ name: "Weight", type: "numeric" }] }` | POST `.../api/v4/catalogs/500/custom_fields` |
| list_catalogs | `{}` | GET `.../api/v4/catalogs` |
| get_catalog | `{ id: 500 }` | GET `.../api/v4/catalogs/500` |
| create_catalogs | `{ records: [{ name: "apb-test-catalog", type: "regular", can_add_elements: true }] }` | POST `.../api/v4/catalogs` |
| update_catalog | `{ id: 500, record: { name: "renamed" } }` | PATCH `.../api/v4/catalogs/500`, body_contains `{ name: "renamed" }` |
| list_catalog_elements | `{ catalog_id: 500, query: "sku-1", limit: 20 }` | GET `.../api/v4/catalogs/500/elements?limit=20&query=sku-1` |
| get_catalog_element | `{ catalog_id: 500, id: 9001 }` | GET `.../api/v4/catalogs/500/elements/9001` |
| create_catalog_elements | `{ catalog_id: 500, records: [{ name: "apb-test-item" }] }` | POST `.../api/v4/catalogs/500/elements` |
| update_catalog_element | `{ catalog_id: 500, id: 9001, record: { name: "renamed" } }` | PATCH `.../api/v4/catalogs/500/elements/9001`, body_contains `{ name: "renamed" }` |
| get_products_settings | `{}` | GET `.../api/v2/products_settings` |
| enable_products | `{}` | POST `.../api/v2/products_settings/`, body_contains `{ enabled: true }` |

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

`list_custom_fields` query: `filter[type][0]: "{{args.filter_type}}"`, `page`, `limit`, `order[sort]`, `order[id]`; description: `Custom field definitions for an entity type; the API returns at most 50 per page regardless of limit. field types: text, numeric, checkbox, select, multiselect, multitext, date, url, textarea, radiobutton, streetaddress, smart_address, birthday, legal_entity, date_time, price, category, items, tracking_data, linked_entity, chained_list, monetary, file.` `filter_type` is a string with that enum. Pick `[_embedded.custom_fields, _page, _links.next.href]`. `create_custom_fields` items require `name` and `type`; description mentions `enums: [{value, sort}]` for select types and `group_id`. `delete_custom_field` is `EFFECTFUL, IRREVERSIBLE. Deletes the field and every value stored in it.` Catalog fields: same shapes with `url: "{{account.base_url}}/api/v4/catalogs/{{args.catalog_id}}/custom_fields"`, `required: [catalog_id]`.

Catalogs: `create_catalogs` items `{ name (required), type: enum [regular, invoices], can_add_elements, can_show_in_cards, can_link_multiple, sort }`; description: `at most 10 catalogs per account; products are the catalog with type products, created by enable_products, not here.` `list_catalog_elements` query `query`, `filter[id]`, `page`, `limit`, `with` (documented `invoice_link`); pick `[_embedded.elements, _page, _links.next.href]`. Elements items: `{ name (required), custom_fields_values }`. `get_products_settings` url `{{account.base_url}}/api/v2/products_settings`, pick `[is_enabled, catalog_id]`. `enable_products` url `{{account.base_url}}/api/v2/products_settings/` with body `{ enabled: true }`, `args_schema: { type: object, properties: {}, required: [] }`, description `EFFECTFUL, IRREVERSIBLE. Turns on the products feature and creates the products catalog; returns its catalog_id.`

- [ ] **Step 4: Run tests and the gate.** Expected: PASS.
- [ ] **Step 5: Stop for owner approval before committing.** Message: `feat(connectors): amocrm custom fields, catalogs and products`.

---

### Task 6: Administration, automation, files

**Files:**
- Modify: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`

**Interfaces:**
- Produces: `list_webhooks, subscribe_webhook, unsubscribe_webhook, list_bots, run_bot, stop_bot, list_sources, create_sources, list_chat_templates, list_talks, close_talk, list_subscriptions, create_short_links, add_calls, list_files, get_file, list_entity_files, attach_files, detach_files, list_file_links`.

- [ ] **Step 1: Add the failing test cases**

| function | args | expect |
|---|---|---|
| list_webhooks | `{ filter_destination: "https://hooks.example.com/amo" }` | GET `.../api/v4/webhooks?filter%5Bdestination%5D=https%3A%2F%2Fhooks.example.com%2Famo` |
| subscribe_webhook | `{ destination: "https://hooks.example.com/amo", settings: ["add_lead", "status_lead"], sort: 10 }` | POST `.../api/v4/webhooks`, body_contains `{ destination: "https://hooks.example.com/amo", settings: ["add_lead", "status_lead"] }` |
| unsubscribe_webhook | `{ destination: "https://hooks.example.com/amo" }` | DELETE `.../api/v4/webhooks`, body_contains `{ destination: "https://hooks.example.com/amo" }` |
| list_bots | `{ limit: 50 }` | GET `.../api/v4/bots?limit=50` |
| run_bot | `{ bot_id: 12, entity_type: 2, entity_id: 5 }` | POST `.../api/v4/bots/12/run`, body_contains `{ entity_type: 2, entity_id: 5 }` |
| stop_bot | `{ bot_id: 12, entity_type: 2, entity_id: 5 }` | POST `.../api/v4/bots/12/stop`, body_contains `{ entity_type: 2, entity_id: 5 }` |
| list_sources | `{}` | GET `.../api/v4/sources` |
| create_sources | `{ records: [{ name: "apb-test-source", external_id: "apb-1", pipeline_id: 100 }] }` | POST `.../api/v4/sources` |
| list_chat_templates | `{ limit: 20 }` | GET `.../api/v4/chats/templates?limit=20` |
| list_talks | `{ filter_contact_id: 9, filter_only_in_work: 1 }` | GET `.../api/v4/talks?filter%5Bcontact_id%5D=9&filter%5Bonly_in_work%5D=1` |
| close_talk | `{ id: 555, force_close: true }` | POST `.../api/v4/talks/555/close`, body_contains `{ force_close: true }` |
| list_subscriptions | `{ entity_type: "leads", entity_id: 5 }` | GET `.../api/v4/leads/5/subscriptions` |
| create_short_links | `{ records: [{ url: "https://example.com/offer", metadata: { entity_type: "contacts", entity_id: 9 } }] }` | POST `.../api/v4/short_links` |
| add_calls | `{ records: [{ direction: "inbound", duration: 60, source: "apb", phone: "+79990000000", call_result: "ok" }] }` | POST `.../api/v4/calls` |
| list_files | `{ filter_term: "contract", limit: 20 }` | GET `https://drive-b.amocrm.ru/v1.0/files?filter%5Bterm%5D=contract&limit=20` |
| get_file | `{ uuid: "0bd7d1a8-7d36-4f6b-9c2a-000000000001" }` | GET `https://drive-b.amocrm.ru/v1.0/files/0bd7d1a8-7d36-4f6b-9c2a-000000000001` |
| list_entity_files | `{ entity_type: "leads", entity_id: 5, limit: 20 }` | GET `.../api/v4/leads/5/files?limit=20` |
| attach_files | `{ entity_type: "leads", entity_id: 5, records: [{ file_uuid: "0bd7d1a8-7d36-4f6b-9c2a-000000000001" }] }` | PUT `.../api/v4/leads/5/files` |
| detach_files | `{ entity_type: "leads", entity_id: 5, records: [{ file_uuid: "0bd7d1a8-7d36-4f6b-9c2a-000000000001" }] }` | DELETE `.../api/v4/leads/5/files` |
| list_file_links | `{ uuid: "0bd7d1a8-7d36-4f6b-9c2a-000000000001" }` | GET `.../api/v4/files/0bd7d1a8-7d36-4f6b-9c2a-000000000001/links` |

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

`subscribe_webhook` body `{ destination: "{{args.destination}}", settings: "{{args.settings}}", sort: "{{args.sort}}" }`; `settings` is `{ type: array, minItems: 1, items: { type: string, enum: [add_lead, update_lead, delete_lead, restore_lead, status_lead, responsible_lead, add_contact, update_contact, delete_contact, restore_contact, responsible_contact, add_company, update_company, delete_company, restore_company, responsible_company, add_customer, update_customer, delete_customer, responsible_customer, add_task, update_task, delete_task, responsible_task, add_talk, update_talk, note_lead, note_contact, note_company, note_customer, add_message, add_outgoing_message, add_chat_template_review] } }`. Description: `EFFECTFUL. Subscribe a URL to account events (re-posting the same destination replaces its settings). Deliveries are form-encoded and unsigned, so they cannot be received by the apb ingest listener; point destination at your own receiver. At most 100 hooks per account.` `unsubscribe_webhook` is a DELETE with body `{ destination }`, `EFFECTFUL, IRREVERSIBLE`.

`run_bot` and `stop_bot` body `{ entity_type: "{{args.entity_type}}", entity_id: "{{args.entity_id}}" }` where `entity_type` is `{ type: integer, enum: [1, 2, 3, 12], description: "1 contact, 2 lead, 3 company, 12 customer" }`; `required: [bot_id, entity_type, entity_id]`. `list_bots` query `page`, `limit`, `with`; pick `[_embedded.bots, _page, _links.next.href]`.

`create_sources` items `{ name (required), external_id (required), pipeline_id, default }`, `maxItems: 50`, description notes it is admin-only and needs an integration widget with `lead_sources` location for the sources to show. `list_talks` query `filter[contact_id]`, `filter[entity_id]`, `filter[entity_type]`, `filter[only_in_work]` (integer enum `[0, 1]`), `filter[talk_id]`, `page`, `limit`. `close_talk` body `{ force_close: "{{args.force_close}}" }`, boolean default described (`true skips the NPS bot`), `required: [id]`. `list_subscriptions` `entity_type` enum `[leads, customers]`. `create_short_links` items `{ url (required), metadata: { entity_type: enum [contacts], entity_id } }`, `maxItems: 250`. `add_calls` items require `direction` (enum `[inbound, outbound]`), `duration`, `source`, `phone`; description: `EFFECTFUL. Log calls; the phone is matched to a contact or company by its last 10 digits and an unmatched number is silently dropped. call_status: 1 left message, 2 call back later, 3 not available, 4 conversation, 5 wrong number, 6 no answer, 7 busy.`

Files: `list_files` url `{{account.drive_url}}/v1.0/files`, query `filter[name]`, `filter[term]`, `filter[deleted]`, `filter[source_id]`, `limit`, `offset`; description begins `Requires the drive_url account field (get_account with=drive_url) and the Files access scope on the token.` Pick `[_embedded.files, _links.next.href]`. `get_file` url `{{account.drive_url}}/v1.0/files/{{args.uuid}}`, pick `[uuid, name, size, type, versions, _links]`. `list_entity_files` query `limit`, `before_id`; pick `[_embedded.files, _links.next.href]`. `attach_files` method PUT, body `"{{args.records}}"`, items `{ file_uuid (required) }`; `detach_files` method DELETE, same body shape, `EFFECTFUL`. `list_file_links` pick `[_embedded.entities]`.

- [ ] **Step 4: Run tests and the gate.** Expected: PASS. Also run `./target/debug/apb connector call --dir connectors/amocrm list_files --dry-run --args '{}'` with an account stub lacking `drive_url` if the CLI supports a dry account, and record the error text for README; if not, note that a missing `drive_url` surfaces as a render error at call time.

- [ ] **Step 5: Stop for owner approval before committing.** Message: `feat(connectors): amocrm webhooks, bots, sources, talks, calls and files`.

---

### Task 7: Documentation, grant presets, prompt-size check

**Files:**
- Modify: `connectors/amocrm/PUBLIC.md`, `connectors/amocrm/README.md`, `connectors/amocrm/INSTALL.md`, `docs/CONNECTORS.md:269-280` (intro count and list) and a new `### amocrm` section after `### twenty` (before `### whatsapp` or alphabetically, follow the file's existing order which is chronological: append after `### whatsapp`).
- Create: `docs/release-notes/v0.21.0.md`

**Interfaces:**
- Consumes: the final function list from Tasks 1 to 6 (count them with `grep -c '^  - name:' connectors/amocrm/connector.yaml`).

- [ ] **Step 1: PUBLIC.md body**

Replace the placeholder paragraph with: one paragraph on coverage (families and the exact function count), one on what is deliberately absent (no DELETE for core entities in v4; webhook management is included but receiving is not; file upload, Chats API and OAuth2 are out of scope), an `## Account setup` section with the YAML:

```yaml
accounts:
  - name: default
    base_url: https://example.amocrm.ru
    drive_url: https://drive-b.amocrm.ru
    access_token: "{{env.AMOCRM_ACCESS_TOKEN}}"
```

and a `## Healthcheck` section naming `get_account`.

- [ ] **Step 2: README.md**

Follow `connectors/twenty/README.md` structure: `# amocrm: setup for humans`; "The short way" with the paste prompt naming `connectors/amocrm/INSTALL.md`; "What you will be asked for" (base_url is `https://<subdomain>.amocrm.ru` or `https://<subdomain>.kommo.com`; the long-lived token: amoMarket, create a private integration, grant "Account data" and "Files access", open it, tab "Keys", "Generate token", pick an expiry up to five years, the value is shown once; `drive_url` from `get_account`); "What this connector can and cannot do" with the shared API facts table (rate limit, 204, paging, filter add-on, batch caps, no deletes, tariff gates for customers, catalogs, webhooks, filtering); "Grant presets" with the seven lists from the spec written as ready-to-paste YAML:

```yaml
connectors:
  - name: amocrm
    functions: [get_account, list_pipelines, list_statuses, list_leads, get_lead, list_contacts, get_contact, list_companies, get_company, list_tasks, list_notes, list_tags, list_users]
```

for `sales-read`, then `sales-write`, `inbox`, `setup-admin`, `catalog`, `customers`, `files` exactly as enumerated in the spec section "Grant presets", and a closing sentence that `functions: read_only` is the coarse built-in split. Finish with "Irreversible functions worth restricting": `delete_pipeline, delete_status, delete_custom_field, delete_transaction, set_customers_mode, enable_products, unsubscribe_webhook, detach_files`.

- [ ] **Step 3: INSTALL.md**

Follow `connectors/twenty/INSTALL.md` step structure: Step 0 ground check; Step 1 `apb connector install amocrm`; Step 2 credentials (the same UI path as README, with the rule that the token is never echoed and the user may fill the dotenv themselves); Step 3 global vs project account, config YAML as in PUBLIC.md, `AMOCRM_ACCESS_TOKEN` in `<config-dir>/secrets.env` (project `.apb/secrets.env` is gitignored but the global file is preferred); Step 4 `apb connector approve amocrm --account <name>`; Step 5 healthcheck `apb connector call amocrm get_account --args '{"with":"drive_url,is_api_filter_enabled"}'` and copying `drive_url` back into the account, then a second approve; Step 6 failure table (401 token expired or revoked; 403 IP allowlist or rate block; 402 tariff; 429 rate limit); Step 7 report. State plainly that no run is started during setup.

- [ ] **Step 4: docs/CONNECTORS.md**

Change "Fourteen official connectors" to "Fifteen official connectors" and append `amocrm` to the inline list. Add a `### amocrm` section in the same single-paragraph style as `### twenty`, including the sentence `Request shapes are verified against the amoCRM API v4 documentation and the offline contract tests, not yet against a live account.`: account fields, where the token comes from, coverage with the function count, the `entity_type` collapse, the effectful function count (count `EFFECTFUL` occurrences), the irreversible list, the out-of-scope list with reasons, the grant presets pointer to README, and `get_account` as healthcheck.

- [ ] **Step 5: Release note**

`docs/release-notes/v0.21.0.md`, same voice as `v0.20.2.md`: title `# apb 0.21.0`, one-line summary, a section on the connector (what it covers, the long-lived token choice, the presets as the answer to prompt size), a short section on the limits (webhook receive, upload, multi-value filters) and that these are tracked as follow-ups, and one plain sentence that the connector is documentation-verified and awaits its first live account. Reference the PR number placeholder as `(#NNN)` only if the number is known at write time; otherwise omit the reference and add it at release time.

- [ ] **Step 6: Measure the prompt block**

Write a throwaway playbook in the scratchpad with one `agent_task` node granting `amocrm` with the `sales-write` preset and one granting all functions; render the instruction block through the engine (use `apb playbook validate` plus the existing prompt test helper `instruction_block` in `crates/apb-engine/src/connector/prompt.rs` via a temporary `#[test]` that prints `block.len()`, then delete the test). Record characters and the divide-by-four token estimate in the README's "Grant presets" section: the target is under 20,000 characters for `sales-write` and under 80,000 for the whole connector. If the whole connector exceeds the target, shorten descriptions before shipping.

- [ ] **Step 7: Lint the prose**

Run: `grep -nP "\x{2014}|!" connectors/amocrm/*.md connectors/amocrm/*.yaml docs/release-notes/v0.21.0.md` and the new `docs/CONNECTORS.md` section.
Expected: no output.

- [ ] **Step 8: Full gate**

Run: `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p apb-core official && cargo test -p apb --test main every_official_connector_folder_is_complete && cargo metadata --format-version 1 >/dev/null && code-ranker check .`
Expected: all clean.

- [ ] **Step 9: Stop for owner approval before committing.** Message: `docs(connectors): amocrm setup docs, grant presets and release note`.

---

### Task 8 (DEFERRED until a real client account exists): Live verification

**Files:**
- Modify: `connectors/amocrm/connector.yaml`, `connectors/amocrm/tests.yaml`, `connectors/amocrm/README.md` (only where the live payload disagrees with the documented shape)
- Create (scratchpad only, not in the repo): `<scratchpad>/amocrm-live-report.md`

**Interfaces:**
- Consumes: a real client's `base_url` and long-lived token (owner decision 2026-08-30: no trial-account application will be filed, so this task does not block the release). Token goes to `<config-dir>/secrets.env` as `AMOCRM_ACCESS_TOKEN=...`; account config at `<config-dir>/connector-config/amocrm.yaml` as in PUBLIC.md.

- [ ] **Step 1: Install from the folder and approve**

```sh
./target/debug/apb connector install --from-dir connectors/amocrm
./target/debug/apb connector approve amocrm
./target/debug/apb connector approve amocrm --account default
```

- [ ] **Step 2: Healthcheck and drive_url**

`./target/debug/apb connector call amocrm get_account --args '{"with":"drive_url,is_api_filter_enabled,task_types"}'`. Copy `drive_url` into the account config, re-approve the account. Record `is_api_filter_enabled`.

- [ ] **Step 3: Every read_only function once**

For each `read_only` function call it with minimal args (`--full` on the first call of each family to see the raw HAL body), and confirm every `response_pick` path exists. Where the trial tariff denies a family (402), record it and skip. Fix `response_pick` paths that are wrong; each fix gets a matching change in the function's description if the documented shape was wrong.

- [ ] **Step 4: Write scenario (names prefixed `apb-test-`)**

Order: `create_contacts` -> `create_companies` -> `create_leads_complex` -> `create_notes` (common) -> `create_tasks` -> `complete_task` -> `update_lead` (move to another stage in the same pipeline) -> `create_tags` and attach through `update_entities` with `tags_to_add` -> `link_entities` (lead to company) -> `list_links` -> `create_short_links` -> `list_events` for the lead -> `subscribe_webhook` to `https://example.invalid/apb-test` -> `list_webhooks` -> `unsubscribe_webhook`. Optional if the owner allowed it: `create_statuses` then `delete_status`; `create_custom_fields` then `delete_custom_field`. Capture each response's shape; anything the schema rejected or amoCRM rejected with 400 is a connector bug to fix in place, with a new `tests.yaml` case that pins the corrected request.

- [ ] **Step 5: Files, catalogs, customers if available**

`list_files`, `list_entity_files` on the test lead, `attach_files` only if the drive already has a file (no upload path exists). `list_catalogs`; if a products catalog exists, `list_catalog_elements`. `list_customers` only if customers mode is on; do not call `set_customers_mode` or `enable_products` on the trial account unless the owner asks.

- [ ] **Step 6: Report**

Write `<scratchpad>/amocrm-live-report.md`: table of functions called, status, fixes made, entities left in the account for manual cleanup (ids). No token, no raw auth headers. Re-run `apb connector test --dir connectors/amocrm` and the gate after fixes.

- [ ] **Step 7: Stop for owner approval before committing.** Message: `fix(connectors): amocrm live-verified response shapes` (only if fixes were needed).

---

## Self-review notes

- Spec coverage: every family in the spec inventory maps to Tasks 2 to 6; the custom fields split (spec update of 2026-08-30) is in Task 5; presets, docs and release note in Task 7; live plan in Task 8; registration and gitignore in Task 1. The spec's `order_by`/`order_dir` pair is implemented as three fixed `order[...]` keys because the query map cannot embed an argument in a key; the spec's intent is unchanged.
- Function count: 1 + 24 + 24 + 13 + 19 + 20 = 101 functions including `get_account`. This exceeds the spec's "approximately 72" because the spec counted some families loosely; the count is still legal for the format. Task 7 measures the prompt block and shortens descriptions if the whole-connector grant exceeds 80,000 characters. If the owner prefers a smaller surface, the candidates to drop are `list_sources, create_sources, list_chat_templates, list_subscriptions, get_unsorted_summary, list_customer_segments, get_event, list_file_links`.
- Names used consistently across tasks: `entity_type`, `records`, `record`, `id`, `catalog_id`, `pipeline_id`, `customer_id`, `entity_id`, `uuid`, `filter_*_from/to`, `order_created_at/order_updated_at/order_id`.
