# WhatsApp Connector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the fourteenth official connector, `whatsapp` (folder `connectors/whatsapp/`), for Meta's WhatsApp Business Platform (Cloud API). It sends over REST (text, template, media, mark-read, template management, business profile, phone numbers, media URL/delete) and, as the first connector to declare a `webhook` block and `inbox` functions, receives inbound messages and status updates through the landed webhook-ingest listener. Offline contract tests are the merge gate; live smoke against a real Meta WABA is deferred and operator-credentialed.

**Architecture:** Data-only connector folder (connector.yaml, tests.yaml, PUBLIC.md, README.md, INSTALL.md) picked up by the existing rust_embed folder scan in `crates/apb-core/src/connector/official.rs`, plus the one-line pin in that file's official-name list, a `### whatsapp` subsection and count bump in `docs/CONNECTORS.md`, and a demo playbook under `examples/playbooks/`. No engine or schema code changes: the webhook block, the `inbox` function kind, the `meta_hub` challenge, and the `hmac_sha256_hex` signature scheme all landed with the webhook-ingest feature (`crates/apb-core/src/connector/def.rs` WebhookSpec/SignatureSpec/InboxSpec, `crates/apb-core/src/connector/webhook.rs`). Structural templates: `connectors/twenty/` (five-file shape, typed bodies via `{{args.*}}`, response_pick, healthcheck, docs) and `crates/apb-engine/tests/fixtures/connectors/echo-hooks/` (the webhook block plus inbox functions, and the `inbox` tests.yaml expectation shape).

**Tech Stack:** apb connector YAML schema (`ConnectorDoc::from_yaml`, def.rs), the offline contract runner (`apb connector test --dir`), the landed webhook/inbox validation and execution.

**Spec:** `docs/superpowers/specs/2026-08-17-whatsapp-connector-design.md` (approved). Depends on the landed `docs/superpowers/specs/2026-08-16-webhook-ingest-design.md`.

## Global Constraints (one line each)

- No em-dashes (U+2014) and no exclamation marks in docs or user-facing strings; no CJK anywhere.
- Every `read_only: true` function carries a non-empty `response_pick` (the official-connector gate requires it).
- The five-file official gate: `connector.yaml`, `tests.yaml`, `PUBLIC.md`, `README.md`, `INSTALL.md` all present; `README.md` and `INSTALL.md` first line starts with `# whatsapp:`; `README.md` body contains the literal `connectors/whatsapp/INSTALL.md`.
- Add `whatsapp` to the pinned official name list in `crates/apb-core/src/connector/official.rs` (the sorted vec in `every_official_connector_carries_the_full_file_set`), inserted between `twenty` and `youtrack`.
- Connector secrets are referenced only as `{{secret.*}}` and env-backed; NEVER a literal token in any repo file. `{{secret.access_token}}` appears only inside `auth`; `{{secret.verify_token}}` and `{{secret.app_secret}}` appear only inside the `webhook` block; `{{env.*}}` names are documentation examples only.
- The webhook block is the third deliberate `{{secret.*}}` exemption to the auth-only rule (after the smtp and imap connection passwords), already landed and enforced in `def.rs`.
- Commits are the owner's call: `git commit --signoff` with the trailer `Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>`. Do not commit or stage in this work; deliver locally and let the owner commit after approval.
- Gates before the deliverable is presented: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p apb-core -p apb-cli`, and `code-ranker check .` (warm the cache first: `cargo metadata --format-version 1 >/dev/null`).
- Contract tests are fully offline and need no credentials.
- Live smoke is deferred and needs operator-supplied Meta credentials (a WABA test number, System User token, app secret, verify token); it is a confirmation pass when credentials exist, not a merge blocker.

## Design decisions (settled, do not reopen)

- Name `whatsapp`, version `0.1.0`. `auth`: `kind: header`, `header: Authorization`, `value_template: "Bearer {{secret.access_token}}"`.
- `account_fields` (six): `base_url` (required, non-secret; documented value `https://graph.facebook.com`), `graph_version` (required, non-secret; documented example `v23.0`, a field because Meta supports each version ~2 years and it bumps without a connector change; `v23.0` is CONFIRMED in-window as of Aug 2026, released 2025-05-29 and expiring 2027-10-08 with the latest at v26.0, so it ships with runway, cite https://developers.facebook.com/docs/graph-api/changelog/versions/ ), `phone_number_id` (required, non-secret), `waba_id` (required, non-secret), `access_token` (required, secret), `app_secret` (secret, **not** required), `verify_token` (secret, **not** required). RESOLUTION: `app_secret`/`verify_token` are `required: false` so a send-only account (no server-mode ingest) stays valid; the webhook block still references them so a receiving account defines them, and validator V43 catches a node granting inbox functions on an account that omits them.
- Base path for every HTTP function is `{{account.base_url}}/{{account.graph_version}}`.
- `healthcheck: get_phone_number` (read_only, side-effect free; proves token validity and that the configured `phone_number_id` belongs to it, extracting `verified_name` and `quality_rating`).
- No `error_when`: Meta signals failure via HTTP status plus an `error.code` body; the engine's status mapping is sufficient for 0.1, and the error-code taxonomy (131047 out-of-window, 132001 template not approved, 130429 throughput, 190 expired token) is documented in prose, not modeled.
- **16 functions.** Concrete surface below. Every read_only function has a non-empty response_pick; every function has an `args_schema` (JSON Schema object) and at least one `examples` entry that validates against it (both gate requirements).

### Outbound functions (13), method / path (under `{{account.base_url}}/{{account.graph_version}}`) / body / response_pick / read_only

- `send_text` (effectful): POST `/{{account.phone_number_id}}/messages`; body `{"messaging_product":"whatsapp","recipient_type":"individual","to":"{{args.to}}","type":"text","text":"{{args.text}}"}`; args `{to: string req, text: object req {body: string req, preview_url: boolean opt}}`. Connector fixes the envelope so the grant boundary is real (a `send_text` grant cannot send a template). No response_pick (effectful; full body returned, `messages[].id` is the sent id).
- `send_template` (effectful): POST `/{{account.phone_number_id}}/messages`; body `{"messaging_product":"whatsapp","recipient_type":"individual","to":"{{args.to}}","type":"template","template":"{{args.template}}"}`; args `{to: string req, template: object req {name: string req, language: object req {code: string req}, components: array opt}}`. The function to (re-)open contact outside the 24h window.
- `send_media` (effectful): POST `/{{account.phone_number_id}}/messages`; body `"{{args.message_body}}"` (full Messages-API envelope, forwarded verbatim, the twenty `{{args.record}}` precedent). args `{message_body: { type: object, additionalProperties: true } req}`. RESOLUTION: media is the one passthrough send because the media object's JSON key must equal the top-level `type` (`image`/`video`/`audio`/`document`/`sticker`) and a static template cannot express a dynamic key; the description states the envelope must carry `messaging_product`, `to`, `type`, and the type-named media object with either `{id}` or `{link}`, and that `recipient_type` (`individual`) is an optional common envelope field. NAMING (item 6): the body arg is `message_body`, not the spec draft's `media` arg, deliberately renamed for clarity; use `message_body` consistently in connector.yaml, tests.yaml, and docs, and do not revert it. Because the envelope is caller-supplied, a `send_media` grant can send ANY message type (text, template, media alike); the function description AND the README limitations must state this plainly (the twenty "intentionally loose passthrough" honesty pattern).
- `mark_read` (effectful): POST `/{{account.phone_number_id}}/messages`; body `{"messaging_product":"whatsapp","status":"read","message_id":"{{args.message_id}}"}`; args `{message_id: string req}`. Fully typed by the connector.
- `list_templates` (read_only): GET `/{{account.waba_id}}/message_templates`; query `{fields:"{{args.fields}}", limit:"{{args.limit}}", after:"{{args.after}}"}` (all optional, dropped when absent); args `{fields: string opt, limit: integer opt, after: string opt}`; response_pick `[data.name, data.status, data.category, data.language, data.id, paging.cursors.after]` (response_pick maps `data.<field>` over the array, the `list_objects` precedent).
- `create_template` (effectful): POST `/{{account.waba_id}}/message_templates`; body `"{{args.template}}"`; args `{ template: { type: object, properties: {name, category, language, components}, required: [name, category, language, components], additionalProperties: true } req }` (the `additionalProperties: true` lives INSIDE the `template` property, the twenty `create_company` `record` precedent, never at the schema top level). Description: asynchronous, returns a PENDING status; approval arrives later, observable only by re-listing or via a webhook.
- `delete_template` (effectful): DELETE `/{{account.waba_id}}/message_templates`; query `{name:"{{args.name}}", hsm_id:"{{args.hsm_id}}"}`; args `{name: string req, hsm_id: string opt}`. Description: `name` alone is a HARD delete of all language variants of that name, unlike twenty's soft-delete stance; there is no restore. `hsm_id` is a real current Meta parameter (deletes a single language variant, used together with `name`), confirmed against https://developers.facebook.com/documentation/business-messaging/whatsapp/templates/template-management ; the bulk `hsm_ids` parameter also exists but is left out of the 0.1 surface.
- `get_business_profile` (read_only): GET `/{{account.phone_number_id}}/whatsapp_business_profile`; query `{fields:"{{args.fields}}"}` (optional); args `{fields: string opt}`; response_pick `[data.about, data.address, data.description, data.email, data.websites, data.vertical, data.profile_picture_url]`.
- `update_business_profile` (effectful): POST `/{{account.phone_number_id}}/whatsapp_business_profile`; body `"{{args.profile}}"`; args `{ profile: { type: object, additionalProperties: true } req }` (`additionalProperties: true` nested inside the `profile` property, not top level). Description: `profile` must carry `messaging_product: "whatsapp"` plus any of `about`/`address`/`description`/`email`/`websites`/`vertical`.
- `list_phone_numbers` (read_only): GET `/{{account.waba_id}}/phone_numbers`; query `{fields:"{{args.fields}}", limit:"{{args.limit}}", after:"{{args.after}}"}` (optional); args `{fields: string opt, limit: integer opt, after: string opt}`; response_pick `[data.verified_name, data.display_phone_number, data.id, data.quality_rating, paging.cursors.after]`.
- `get_phone_number` (read_only, the healthcheck): GET `/{{account.phone_number_id}}`; query `{ fields: "verified_name,quality_rating,display_phone_number,code_verification_status" }` (a FIXED LITERAL, not `{{args.fields}}`, the twenty `soft_delete:"true"` literal precedent); args `{}` (empty object, `required: []`); response_pick `[verified_name, quality_rating, display_phone_number, code_verification_status, id]`. RATIONALE (item 1, GLM Important): the dashboard probes the healthcheck with NO args; if the `fields` query were optional it would drop, Meta would return only default fields, and the promised `response_pick` could come back empty while the call still reports "ok". Baking the literal `fields` makes the no-arg probe self-sufficient. Keep `response_pick` unchanged. The other reads (`get_business_profile`, `list_templates`, `list_phone_numbers`) keep the optional `{{args.fields}}` because they are agent-driven and the caller passes fields deliberately.
- `get_media_url` (read_only): GET `/{{args.media_id}}`; query `{phone_number_id:"{{account.phone_number_id}}"}`; args `{media_id: string req}`; response_pick `[url, mime_type, file_size, sha256, id]`. Description: the returned `url` is short-lived (~5 min) and the binary download still needs the bearer token.
- `delete_media` (effectful): DELETE `/{{args.media_id}}`; query `{phone_number_id:"{{account.phone_number_id}}"}`; args `{media_id: string req}`. Description: a HARD delete; no restore.

RESOLUTION (dropped): `upload_media` (`POST /{{phone_number_id}}/media`) is NOT included. It is multipart/form-data with a binary file part, which the connector schema cannot express (only JSON `body` or form-urlencoded `body_form`, no multipart file upload). Media is sent by hosted `link` (Meta caches it ~10 min) or by a media `id` obtained out of band. README documents this limitation. The Resumable Upload API for large media is already out of scope per the spec.

### Webhook block and inbox functions (3)

Document-level block, verbatim from the spec (references `verify_token`/`app_secret`, both `secret: true` account fields):

```yaml
webhook:
  challenge: meta_hub
  verify_token: "{{secret.verify_token}}"
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: entry.0.changes.0.value.messages.0.id
```

- `inbox_read` (read_only): `inbox: { op: read }`; args `{consumer: string opt, limit: integer opt}`; response_pick `[events, cursor, truncated]`.
- `inbox_ack` (effectful): `inbox: { op: ack }`; args `{consumer: string opt, up_to_seq: integer req}`.
- `inbox_peek_depth` (read_only): `inbox: { op: peek_depth }`; args `{consumer: string opt}`; response_pick `[pending]`. The natural probe for the receiving half.

`dedupe_path` targets the inbound message id; a status-only delivery (no message id at that path) falls back to the SHA-256 of the raw body, which is correct because a status update is a distinct payload. The path `entry.0.changes.0.value.messages.0.id` is CONFIRMED against Meta's real messages webhook envelope (`entry[0].changes[0].value.messages[0].id`), cite https://developers.facebook.com/documentation/business-messaging/whatsapp/webhooks/reference/messages ; keep it as written. The block and the inbox functions require each other (`from_yaml` enforces both directions), so they land together.

---

### Task 1: connector.yaml

**Files:** Create `connectors/whatsapp/connector.yaml`.

**Interfaces:** Consumes the research doc (verified endpoints/envelopes at `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/whatsapp-cloud-api-research.md`), `connectors/twenty/connector.yaml` (structure, typed body precedent), `crates/apb-engine/tests/fixtures/connectors/echo-hooks/connector.yaml` (webhook block + inbox functions), def.rs validation rules. Produces a manifest that parses via `ConnectorDoc::from_yaml` and passes `apb connector test --dir connectors/whatsapp`.

- [ ] **Step 1: read inputs.** The research doc in full; twenty connector.yaml for the typed-body and response_pick idiom; echo-hooks connector.yaml for the webhook block and the three inbox function shapes; `crates/apb-core/src/connector/template.rs` `render_body` semantics (a single `{{args.field}}` placeholder renders the typed value; absent optional query/entries drop).
- [ ] **Step 2: write connector.yaml** exactly per the settled surface above: a header comment (Cloud API only, base path convention `{{base_url}}/{{graph_version}}`, the 24h window and template rule, the hard-delete stance on templates/media vs twenty's soft delete, the send/receive split), the auth block, the six account fields, `healthcheck: get_phone_number`, the document-level `webhook` block, then the 13 outbound plus 3 inbox functions in that order. Every read_only function has its specified non-empty response_pick; every function has an args_schema and at least one example. For the passthrough functions (`send_media` `message_body`, `create_template` `template`, `update_business_profile` `profile`), put `additionalProperties: true` INSIDE the passthrough property's object schema, e.g. `message_body: { type: object, additionalProperties: true }` and `template: { type: object, properties: {...}, required: [...], additionalProperties: true }`, matching twenty's `create_company` `record` precedent, never at the schema top level.
- [ ] **Step 3: validate offline.** Parse and run the offline suite with the locally built `apb connector test --dir connectors/whatsapp` once tests.yaml exists (Task 2); if running Task 1 alone, at minimum assert `ConnectorDoc::from_yaml` succeeds and the webhook/inbox mutual-requirement passes.
- [ ] **Step 4: sanity greps.** `{{secret.access_token}}` only in `auth`; `{{secret.verify_token}}`/`{{secret.app_secret}}` only in the webhook block; no literal token-shaped strings; no em-dash/exclamation/CJK; folder name and `name:` both `whatsapp`; no `upload_media` function.

### Task 2: tests.yaml offline contract cases

**Files:** Create `connectors/whatsapp/tests.yaml`. Optionally add one focused parse assertion to `crates/apb-core/src/connector/official.rs` tests (webhook block resolves) if not already covered by the folder scan.

**Interfaces:** Consumes Task 1's connector.yaml and `connectors/twenty/tests.yaml` (HTTP `expect: {method, url, body_contains}` style) plus `crates/apb-engine/tests/fixtures/connectors/echo-hooks/tests.yaml` (the `inbox` expectation with `seed`/`events`/`cursor`/`acked`/`acked_up_to`/`pending`). Produces a suite where the gate finds a case for every one of the 16 functions and every case passes offline.

- [ ] **Step 1:** one case per function (16 total), each supplying `account: { base_url: "https://graph.facebook.com", graph_version: "v23.0", phone_number_id: "100000000000000", waba_id: "200000000000000" }` (no secrets: the runner renders url/method/body without resolving `{{secret.*}}`, exactly as twenty's cases do; the Authorization header is therefore not asserted).
- [ ] **Step 2: outbound cases assert:** `send_text`/`send_template`/`mark_read` POST to `.../v23.0/100000000000000/messages` with `body_contains` for the fixed envelope (`messaging_product: whatsapp`, `to`, `type`); `send_media` POST with `body_contains` over the forwarded `message_body`; `list_templates`/`create_template`/`delete_template` path-substitute `waba_id` (`.../v23.0/200000000000000/message_templates`, `delete_template` asserting `?name=...`); `get_business_profile`/`update_business_profile` path-substitute `phone_number_id`; `get_phone_number` (no args) path-substitutes `phone_number_id` AND carries the fixed literal `fields=verified_name,quality_rating,display_phone_number,code_verification_status` query (URL-encoded, so assert the encoded form the runner emits); `get_media_url`/`delete_media` path-substitute `{{args.media_id}}` and carry `?phone_number_id=100000000000000`; `list_phone_numbers` path-substitutes `waba_id`.
- [ ] **Step 3: inbox cases** mirror echo-hooks: `inbox_read` with a seeded envelope asserting `events` and `cursor` (and a `limit` case, and an `acked` case advancing the cursor), `inbox_ack` asserting `acked_up_to`, `inbox_peek_depth` asserting `pending`.
- [ ] **Step 4: RESOLUTION on webhook signature/challenge.** The `tests.yaml` contract runner supports only the Http/Smtp/Mock/Imap/Inbox expectation kinds (`crates/apb-core/src/connector/contract.rs` `ExpectKind`); it has no signature or challenge expectation. The `meta_hub` challenge echo and the `hmac_sha256_hex` signature over Meta's documented sample payload are already covered by the provider-agnostic unit vectors in `crates/apb-core/src/connector/webhook.rs` and the axum-level ingest tests in apb-server, both landed with webhook-ingest. Do NOT attempt to add signature/challenge cases to `tests.yaml`. The connector's webhook-block correctness is proven by the manifest parsing (`ConnectorDoc::from_yaml` runs `validate_webhook_shape` + `validate_webhook_templates`) inside the official gate; add nothing beyond that unless a focused apb-core assertion that the embedded `whatsapp` webhook block resolves (challenge `meta_hub`, header `X-Hub-Signature-256`, prefix `sha256=`, dedupe_path present) is judged worthwhile.
- [ ] **Step 5:** run `apb connector test --dir connectors/whatsapp`; every case passes.

### Task 3: docs (PUBLIC.md, README.md, INSTALL.md)

**Files:** Create `connectors/whatsapp/PUBLIC.md`, `connectors/whatsapp/README.md`, `connectors/whatsapp/INSTALL.md`.

**Interfaces:** Consumes Task 1's function list (must match exactly) and `connectors/twenty/{PUBLIC,README,INSTALL}.md` as templates. Produces the three doc surfaces with the exact first-line formats the gate requires.

- [ ] **Step 1: PUBLIC.md** frontmatter (`display_name: WhatsApp`, `summary:` one line, `tags: [whatsapp, messaging, meta]`, `publisher: apb`) then a body: what the connector covers (send text/template/media, mark-read, template management, business profile, phone numbers, media URL/delete, plus receiving via the webhook inbox), the send/receive split (receiving requires the server-mode + ingest topology; sending works from any install with a token), the 24h customer-service window and the template requirement, asynchronous template review (PENDING), hard delete on templates and media, and that binary media upload is out of scope (send by link or id). 16 functions.
- [ ] **Step 2: README.md** first line exactly `# whatsapp: setup for humans`. The short-way agent prompt naming the connector and pointing at `connectors/whatsapp/INSTALL.md` (gate requires this literal string in the body). Then: the Meta setup a human must do (a System User permanent token with `whatsapp_business_messaging` + `whatsapp_business_management`, set to Never expire; the `phone_number_id`; the `waba_id`; the app secret; a self-chosen verify token; the callback URL), the six account fields with a config example (`access_token: "{{env.WHATSAPP_ACCESS_TOKEN}}"`), the effectful vs read_only split, and the limitations: outside the 24h window a plain `send_text` fails with error 131047 and a template is required (the connector cannot enforce a per-recipient runtime state); `create_template` is asynchronous; template and media deletes are hard; receiving needs server-mode + ingest and deliveries that arrive while the listener is down are lost after the provider's retry window; no binary media upload. Include one sentence (item 5) stating that a `send_media` grant can send any message type, not only media, because its body envelope is caller-supplied, so scope that grant like any full-send capability (the twenty intentionally-loose-passthrough honesty). In the README receiving section, add one sentence (item 7) that Meta may batch several `entry[]`/`changes[]` into a single POST, so an agent reading an inbox event should walk `entry[].changes[].value.messages[]` rather than assume one message per event; dedupe still holds because each message keeps its own id (or, for a status-only payload, the identical-body hash).
- [ ] **Step 3: INSTALL.md** first line exactly `# whatsapp: installation instructions for an agent`. The standard ordered agent runbook adapted from twenty: check ground, `apb connector install whatsapp`, gather the six fields and be honest about what the token grants, choose account scope and write the config, prepare the secrets dotenv (`WHATSAPP_ACCESS_TOKEN`, and for receiving `WHATSAPP_APP_SECRET` + `WHATSAPP_VERIFY_TOKEN`) offering the user the fill-it-in-themselves path, approve connector and account trust, run the `get_phone_number` healthcheck via the dashboard healthcheck endpoint, then the receiving-half runbook: enable `ingest` and set `ingest.public_base_url`, read the exact callback URL per account from `apb connector doctor`, register it plus the verify token in the Meta app console, subscribe the app to the `messages` webhook field, and confirm the GET challenge succeeds. A secret is never echoed/logged/committed; no run is started as part of setup.

### Task 4: official.rs pin, CONNECTORS.md subsection, count bump

**Files:** Modify `crates/apb-core/src/connector/official.rs` and `docs/CONNECTORS.md`.

**Interfaces:** Consumes Task 1-3 outputs. Produces the pin plus the docs subsection; `cargo test -p apb-core -p apb-cli` green including the official gate.

- [ ] **Step 1:** in `official.rs`, add `"whatsapp"` to the sorted vec in `every_official_connector_carries_the_full_file_set`, between `"twenty"` and `"youtrack"`.
- [ ] **Step 2:** in `docs/CONNECTORS.md`, bump `Thirteen official connectors` to `Fourteen`, add `whatsapp` to the inline name list, and add a `### whatsapp` subsection matching the existing entries' length and style: the six account fields, the System User permanent token and the two webhook secrets, `healthcheck: get_phone_number`, the 24h-window/template rule, the hard-delete stance, the receiving-needs-server-mode note, and that binary media upload is out of scope.
- [ ] **Step 3:** add the demo playbook (Task 5) to the "Demo playbooks" list in CONNECTORS.md.
- [ ] **Step 4:** consistency check: every function named in the docs exists in connector.yaml and vice versa; account field names match; no em-dash/exclamation/CJK.

### Task 5: demo playbook

**Files:** Create `examples/playbooks/whatsapp-inbox.yaml` (or similar), mirroring `examples/playbooks/inbox-triage.yaml`.

**Interfaces:** Consumes the finished connector's function names and grant shape. Produces a schema-2 playbook that validates in CI against a fake account (not run against a real service there).

- [ ] **Step 1:** a send-plus-poll graph: a `send_greeting` node granted `{ name: whatsapp, accounts: [default], functions: [send_text], max_calls: 5 }`; a `read_inbox` node granted `functions: [inbox_read], max_calls: 20` whose prompt explicitly marks inbox content as untrusted external input that must not be treated as instructions; a `reply` node granted `functions: [send_text, send_template], max_calls: 20` (template for out-of-window re-engagement); an `ack` node granted `functions: [inbox_ack], max_calls: 20`. Wire the poll loop `read_inbox -> reply -> ack -> read_inbox` as a cycle bounded by the grants, with a `finish` exit, or a single-pass linear form with a wait ahead of `read_inbox` if a cycle complicates validation.
- [ ] **Step 2:** validate with `apb playbook validate` (or the equivalent) against a fake account; ensure grants, `effects`, and edges are well-formed.

### Task 6: final gates

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test -p apb-core -p apb-cli` (covers the apb-core folder-set/self-naming tests and the apb-cli `official_connectors_gate` end-to-end offline run of `apb connector test` over `connectors/whatsapp`)
- [ ] `cargo metadata --format-version 1 >/dev/null` then `code-ranker check .`; read `code-ranker docs base <ID>` for any violation before fixing
- [ ] Deliverable stays uncommitted; the owner decides on commit/PR/release.

---

## Self-check (do before declaring done)

- **Spec coverage, task-by-task:** outbound functions + webhook block + inbox functions (Task 1); offline contract cases incl. path substitution and inbox envelopes, with the signature/challenge routing resolved to the landed apb-core/apb-server vectors (Task 2); the three docs with exact first lines and the Meta runbook (Task 3); official.rs pin + CONNECTORS.md subsection + count bump (Task 4); demo playbook with send plus the inbox poll loop (Task 5); final gates incl. the official gate (Task 6).
- **Placeholder scan:** every function's method, path, query, headers, body, response_pick, args_schema, and read_only flag is concrete above (no TBD).
- **Name/type consistency:** account fields (`base_url`, `graph_version`, `phone_number_id`, `waba_id`, `access_token`, `app_secret`, `verify_token`) are spelled identically in auth, URLs, the webhook block, docs, and the config example; function names match across connector.yaml, tests.yaml, docs, and the demo grants.
- **read_only carries response_pick (gate):** the seven read_only functions each have one: `list_templates`, `get_business_profile`, `list_phone_numbers`, `get_phone_number`, `get_media_url`, `inbox_read`, `inbox_peek_depth`. The healthcheck `get_phone_number` is read_only.
