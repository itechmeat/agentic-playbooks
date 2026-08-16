# Twenty Connector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an official `twenty` connector (folder `connectors/twenty/`) for the Twenty open-source CRM REST API, covering CRUD on the five core CRM objects plus generic record access for custom objects, with offline contract tests, the standard connector docs, and a live smoke test against the owner's disposable demo instance.

**Architecture:** Data-only connector folder (connector.yaml, tests.yaml, PUBLIC.md, README.md, INSTALL.md) picked up by the existing rust_embed folder scan, plus the one-line pin in `crates/apb-core/src/connector/official.rs` and a subsection in `docs/CONNECTORS.md`. Template: `connectors/asana/` (structure) and `connectors/github/` (bare `{{args}}` body passthrough precedent).

**Tech Stack:** apb connector YAML schema (`ConnectorDoc::from_yaml`, crates/apb-core/src/connector/def.rs), offline contract runner (`apb connector test`), live probe reports in the scratchpad.

## Global Constraints

- **No git commits and no git staging in this work. None.** The deliverable is reviewed locally; the owner commits after explicit approval.
- Endpoint facts come ONLY from the two research reports (they agree; the live probe wins on any conflict since it ran against the real target version v2.30.0):
  - `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/twenty-api-research.md` (docs plus source reading)
  - `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/twenty-live-probe.md` (empirical, Twenty v2.30.0)
  - Saved OpenAPI specs in the same directory: `twenty-openapi-core.json`, `metadata_openapi.json` (grep these for enum literals such as order_by directions and the `is` operator values before documenting them; anything not found there or in the reports stays out of connector.yaml and is at most a README caveat).
- Never invent an endpoint, parameter, or default not present in those sources.
- Secrets: never a literal credential anywhere in the repo; `{{secret.api_key}}` appears ONLY inside the `auth` block; `{{env.*}}` references are documentation examples only. The live demo credentials live in `.../scratchpad/twenty-demo.env` (TWENTY_BASE_URL, TWENTY_API_KEY) — Task 3 only; the key value must never be printed, logged, or written into any file under the repo.
- Live network calls to the demo instance are allowed ONLY in Task 3 (the owner sanctioned them; the instance is disposable). Tasks 1 and 2 are fully offline.
- No em-dashes (U+2014), no exclamation marks, no CJK anywhere.
- Do not touch `.apb/profiles/` or anything outside `connectors/twenty/`, `docs/CONNECTORS.md`, and the single pin line in `crates/apb-core/src/connector/official.rs`.
- The official-connectors gate (crates/apb-cli/tests/suite/official_connectors_gate.rs) is binding: every `read_only: true` HTTP function MUST have a non-empty `response_pick`, and a `healthcheck` MUST be declared naming a read_only-or-mock function. Run the whole `cargo test --workspace` (not per-crate) before calling Task 2 done.

## Design decisions (settled, do not reopen)

- Connector name: `twenty`, version `0.1.0`.
- `account_fields`: `base_url` (required, non-secret; the app origin of the self-hosted instance, no path suffix, e.g. `https://crm.example.com`; cloud tenants use `https://api.twenty.com`) and `api_key` (required, secret). Nothing else.
- `auth`: `kind: header`, `header: Authorization`, `value_template: "Bearer {{secret.api_key}}"`.
- No `error_when`: Twenty signals failure via HTTP status codes (400/401/403/404/409/422), which the engine already maps. Error bodies carry `messages` as an array; the docs must mention this shape.
- All URLs are `{{account.base_url}}/rest/...`. No trailing-slash games; `base_url` is documented as scheme plus host only.
- `healthcheck: list_companies` (read-only, renders with zero args, succeeds with any key that can read companies).
- **41 functions.** Exact list, grouped:
  1. Typed CRUD for the five core objects `companies`, `people`, `opportunities`, `notes`, `tasks` (5 x 5 = 25). Singular names for get/create/update/delete (`get_company`, `create_person`, ...), plural for list (`list_companies`):
     - `list_<plural>` GET `/rest/<plural>` read_only, query map `{filter: "{{args.filter}}", order_by: "{{args.order_by}}", limit: "{{args.limit}}", starting_after: "{{args.starting_after}}", ending_before: "{{args.ending_before}}", depth: "{{args.depth}}"}` (all optional args; the engine drops absent single-placeholder pairs), `response_pick: [data.<plural>, totalCount, pageInfo.hasNextPage, pageInfo.endCursor]`.
     - `get_<singular>` GET `/rest/<plural>/{{args.id}}` read_only, optional `depth` query arg, `response_pick: [data.<singular>]`.
     - `create_<singular>` POST `/rest/<plural>`, `body: "{{args.record}}"`, args `{record: object}` with per-object required subfields (company: `name` string; person: `name` object `{firstName, lastName}`; opportunity/note/task: per the reports), `additionalProperties: true` inside `record` so custom fields pass through.
     - `update_<singular>` PATCH `/rest/<plural>/{{args.id}}`, `body: "{{args.record}}"`, args `{id, record}`; partial-update semantics documented. The `record` must NOT be sent with bare `{{args}}` (the id would leak into the body and Twenty rejects unexpected shapes).
     - `delete_<singular>` DELETE `/rest/<plural>/{{args.id}}` with FIXED query `soft_delete: "true"` (literal, not an arg). The connector deliberately never exposes Twenty's default hard destroy; the description says the deletion is a recoverable soft delete and names `restore_record` as the undo.
  2. Link objects (4): `create_note_target` POST `/rest/noteTargets` and `create_task_target` POST `/rest/taskTargets` (body `"{{args.record}}"`, record carries `noteId`/`taskId` plus exactly one of `personId`, `companyId`, `opportunityId`); `list_note_targets` / `list_task_targets` GET with the same optional list query args, read_only, `response_pick: [data.noteTargets|data.taskTargets, totalCount, pageInfo.hasNextPage, pageInfo.endCursor]`.
  3. Duplicates (2): `find_duplicate_companies` / `find_duplicate_people` POST `/rest/<plural>/duplicates`, read_only (logically a search), body `{data: "{{args.data}}", ids: "{{args.ids}}"}` (both optional single-placeholder entries so either wrapper key can be sent alone, per the probe: the endpoint requires `data` or `ids`, never a bare array), `response_pick: [data]`.
  4. Generic record access for every other object including custom ones (6): `list_records` GET `/rest/{{args.object}}`, `get_record` GET `/rest/{{args.object}}/{{args.id}}`, `create_record` POST `/rest/{{args.object}}` body `"{{args.record}}"`, `update_record` PATCH `/rest/{{args.object}}/{{args.id}}` body `"{{args.record}}"`, `delete_record` DELETE `/rest/{{args.object}}/{{args.id}}` fixed `soft_delete: "true"`, `restore_record` PATCH `/rest/restore/{{args.object}}/{{args.id}}` (empty body: omit `body` entirely; verify the contract runner accepts a bodyless PATCH, else `body: {}`). `object` is the camelCase plural REST name (`workspaceMembers`, `attachments`, a custom object's `namePlural`). list/get read_only with `response_pick: [data, totalCount, pageInfo.hasNextPage, pageInfo.endCursor]` and `[data]` respectively; loose schemas with descriptions saying the looseness is intentional.
  5. Webhooks (3): `list_webhooks` GET `/rest/webhooks` read_only `response_pick: [data]`; `create_webhook` POST `/rest/webhooks` body `{targetUrl: "{{args.target_url}}", operations: "{{args.operations}}", description: "{{args.description}}"}` (operations is an array like `["person.created", "*.updated"]` per the reports; no signing-secret arg: args land in event logs, so the docs direct users to set webhook secrets in the Twenty UI); `delete_webhook` DELETE `/rest/webhooks/{{args.id}}`.
  6. Metadata (1): `list_objects` GET `/rest/metadata/objects`, read_only, `response_pick: [data.id, data.nameSingular, data.namePlural, data.labelPlural, data.isCustom]` (the metadata envelope is a bare `data` array; the pick maps over elements and strips the huge per-object `fields` payload). Description notes it needs a key whose role has data-model settings permission.
- `read_only: true` exactly for: the 5 `list_<plural>`, the 5 `get_<singular>`, `list_note_targets`, `list_task_targets`, both duplicates functions, `list_records`, `get_record`, `list_webhooks`, `list_objects` (18 functions). Everything else (23) is effectful.
- args_schema: typed for the typed CRUD (list args: `filter` string, `order_by` string, `limit` integer 1..200, `starting_after`/`ending_before` string, `depth` integer enum [0,1]; ids: string with uuid format note in description). `record` objects: required keys per the reports, `additionalProperties: true`. Every function gets at least one `examples` entry that validates against its schema.
- Function descriptions must carry the load-bearing API facts: filter syntax sketch on the list functions (`field[op]:value`, comma is AND, `or(...)`, operators from the verified list), soft-delete semantics on deletes, composite-field shapes on creates (person `name`, `emails.primaryEmail`, company `domainName` as a LINKS object, currency fields in micros), the `data.create<Singular>` response envelope on creates, cursor pagination on lists (`pageInfo.endCursor` feeds `starting_after`).
- `timeout_sec`: default 30 everywhere (no long-latency endpoints in scope).
- Out of scope for 0.1 (documented as such in README, never as functions): batch endpoints (`/rest/batch/*`), groupBy, merge, attachment binary upload (token flow unverified), GraphQL, api-key management. The `favorites` object does not exist on Twenty 2.7+ and must not be mentioned as available.

---

### Task 1: connector.yaml + tests.yaml

**Files:**
- Create: `connectors/twenty/connector.yaml`
- Create: `connectors/twenty/tests.yaml`

**Interfaces:**
- Consumes: the two research reports and saved OpenAPI specs (endpoint truth), `connectors/asana/connector.yaml` + `tests.yaml` (structure template), `connectors/github/connector.yaml` (body passthrough precedent), def.rs validation rules.
- Produces: a connector that parses via `ConnectorDoc::from_yaml` and passes `apb connector test --dir connectors/twenty` (or the equivalent offline path the CLI exposes; find it in `crates/apb-cli/src/connector.rs` and prefer the invocation that does not mutate the user's global config).

- [ ] **Step 1: read the inputs.** Both research reports in full; asana connector.yaml and tests.yaml in full; the github `create_issue`/`update_issue` functions for the body-passthrough shape; `render_body` semantics in `crates/apb-core/src/connector/template.rs` (single `{{args.field}}` placeholder renders the typed value; absent optional entries drop). Grep the saved OpenAPI core spec for the order_by direction literals and the `is` operator accepted values; use only what you find.
- [ ] **Step 2: write connector.yaml** exactly per the settled decisions above: header comment (what the API is, the `/rest` path convention, the soft-delete stance, snake_case arg convention where args map to camelCase JSON), auth block, two account fields, healthcheck, then the 41 functions in the groups and order given. Every read_only function has the specified non-empty response_pick.
- [ ] **Step 3: write tests.yaml**: at minimum one case per function (41+), plus targeted cases for: the Authorization header rendering (secret injection), soft_delete=true present on every delete, filter/order_by/limit query assembly on a list (URL-encoded expectations), optional-arg dropping (a list call with no args renders no query pairs), update body containing ONLY the record fields (no id leakage), duplicates body with `data` only and with `ids` only, generic functions with `object` path substitution, restore_record URL shape, webhook create body. Mirror asana's `expect: {method, url, body_contains}` case style.
- [ ] **Step 4: validate offline.** Run the parse and the full offline suite with the locally installed `apb` binary; every case must pass. Also run `cargo test -p apb-core connector` (or the existing official-connectors parse test) if it picks up the folder without the official.rs pin; note if it does not (the pin lands in Task 2).
- [ ] **Step 5: sanity greps.** No `{{secret.` outside the auth block; no literal credential-looking strings; no em-dash/exclamation/CJK; folder name and `name:` both `twenty`; no mention of favorites/batch/merge/groupBy as available functions.

### Task 2: docs and the official pin

**Files:**
- Create: `connectors/twenty/PUBLIC.md`, `connectors/twenty/README.md`, `connectors/twenty/INSTALL.md`
- Modify: `docs/CONNECTORS.md` (new `### twenty` subsection matching the existing entries' style; bump the connector count wording from twelve to thirteen everywhere it appears)
- Modify: `crates/apb-core/src/connector/official.rs` (add `twenty` to the pinned official-connector name list)

**Interfaces:**
- Consumes: Task 1's connector.yaml (function list and account fields must match exactly), atrip and asana doc files as templates, the research reports for prose facts.
- Produces: the four standard doc surfaces plus the pin; `cargo test --workspace` green including official_connectors_gate.

- [ ] **Step 1:** PUBLIC.md, same frontmatter shape as asana/atrip: what Twenty is, the function families (typed CRUD, links, duplicates, generic records incl. custom objects, webhooks, metadata), the soft-delete-only stance, auth (API key under Settings, API and Webhooks; keys always carry an expiry), self-hosted base_url guidance.
- [ ] **Step 2:** README.md: creating an API key (Settings, API and Webhooks, key shown once, role-scoped, mandatory expiry), the two account fields with a config example (`api_key: "{{env.TWENTY_API_KEY}}"`), capability summary, the effectful-function list with the soft-delete/restore story, rate limits (defaults 100 requests/second and 100 requests/minute windows, env-overridable on self-hosted; mutation cap 100 records), caveats: `depth` only 0 or 1, error bodies use a `messages` array, missing auth header returns 403 while an invalid key returns 401, `limit` max 200 default 60, batch/merge/groupBy/attachment-upload out of scope, custom objects served by the generic record functions using the object's `namePlural`.
- [ ] **Step 3:** INSTALL.md: the standard agent runbook mirroring asana/atrip, adapted to the two fields and the `list_companies` healthcheck.
- [ ] **Step 4:** docs/CONNECTORS.md subsection: same length and structure as the atrip entry; name the healthcheck and the soft-delete stance.
- [ ] **Step 5:** the official.rs pin line, then run the FULL `cargo test --workspace` and fix anything the official-connectors gate reports (file-set completeness, setup-doc self-naming, response_pick and healthcheck rules).
- [ ] **Step 6:** consistency check: every function named in docs exists in connector.yaml and vice versa; account field names match; no em-dash/exclamation/CJK; spot-check three prose claims against the research reports.

### Task 3: live smoke test against the demo instance

**Files:**
- Create: `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/twenty-smoke-report.md` (report only; nothing under the repo)

**Interfaces:**
- Consumes: the finished connector folder, the demo credentials env file (path in Global Constraints), the apb CLI.
- Produces: a pass/fail smoke report; a clean demo instance (every record the smoke test created is deleted); the local apb config restored (installed copy and account config removed).

- [ ] **Step 1:** install the connector from the working tree (`apb connector install twenty --from-dir connectors/twenty` or the exact form the CLI exposes), write a minimal account config pointing at the demo `base_url` with `api_key: "{{env.TWENTY_API_KEY}}"`, export the env vars by sourcing the env file (never echo them), approve connector and account trust.
- [ ] **Step 2:** run `apb connector doctor` for the healthcheck, then live calls covering at least: `list_companies` (with a filter and limit), `get_company` (id from the list), `create_company` / `update_company` / `delete_company` cycle with an `apb-smoke-` named record, `create_person` (composite name), `find_duplicate_companies` (data wrapper), `list_records` with `object: workspaceMembers`, `list_objects`, `list_webhooks`. Verify each response projects through response_pick as designed (compare with `--full` where useful).
- [ ] **Step 3:** cleanup: delete every record the smoke test created and verify with a filter sweep (`totalCount: 0` for the smoke prefix); remove the installed connector copy, the account config, and the trust entries; state each cleanup action in the report.
- [ ] **Step 4:** write the smoke report: each call, rendered request (key redacted), status, verdict; any mismatch between design assumptions and live behavior is a finding for the coordinator, not something to silently patch.

---

## Acceptance (coordinator-owned)

Per-task review, then a final whole-deliverable review against the research reports, then the Task 3 smoke findings adjudicated. Format/lint gates (`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, code-ranker) run before the deliverable is presented. Deliverable stays uncommitted; the owner decides on commit/PR/release.
