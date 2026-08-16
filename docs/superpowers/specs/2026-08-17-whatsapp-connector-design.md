# WhatsApp connector: Meta Cloud API, send plus webhook inbox

Date: 2026-08-17. Status: approved for implementation planning.

## Motivation

WhatsApp is the driver behind the webhook-ingest feature. Meta's WhatsApp Business Platform (Cloud API) has no polling endpoint for inbound messages, delivery receipts or template status: they arrive only as webhook POSTs. With server mode (authenticated deployment) and the generic webhook ingest subsystem now landed, apb can finally ship a WhatsApp connector that both sends over REST and receives through the ingest listener. This is the fourteenth official connector and the first to declare a `webhook` block and `inbox` functions.

Deployment context: the receiving half only works when apb runs in the server-mode topology (domain, reverse proxy, ingest listener reachable at a public HTTPS callback URL that Meta is registered against). The sending half works from any apb install with a token. Both facts are documented, not assumed.

## Authentication

- Account fields: `base_url` (default `https://graph.facebook.com`), `graph_version` (e.g. `v23.0`, an account field because Meta supports each version about two years and it will need bumping without a connector change), `phone_number_id`, `waba_id`, and two secrets: `access_token` (the System User permanent token) and, for the webhook block, `app_secret` and `verify_token`.
- Auth block: `Authorization: Bearer {{secret.access_token}}` on every REST call. No OAuth flow, no refresh; the operator pastes a never-expiring System User token.
- The webhook block references `verify_token` and `app_secret` (both `secret: true`) per the landed webhook schema.

## Outbound functions (ordinary HTTP)

Base path is `{{account.base_url}}/{{account.graph_version}}`.

- `send_text`, `send_template`, `send_media`, `mark_read`: `POST /{{account.phone_number_id}}/messages` with the typed body per Meta's envelope (`messaging_product: whatsapp`, `to`, `type`, ...). `send_text` uses `body: "{{args.message_body}}"` (the full typed body object passed by the caller, so no field leakage), same pattern as the twenty connector's typed bodies.
- Template management on the WABA id: `list_templates` (`GET /{{account.waba_id}}/message_templates`, read_only, response_pick over name/status/category/language), `create_template` (`POST`), `delete_template` (`DELETE ?name=`).
- Business profile: `get_business_profile` (`GET /{{account.phone_number_id}}/whatsapp_business_profile`, read_only), `update_business_profile` (`POST`).
- Phone numbers: `list_phone_numbers` (`GET /{{account.waba_id}}/phone_numbers`, read_only), `get_phone_number` (`GET /{{account.phone_number_id}}`, read_only, the healthcheck).
- Media: `upload_media` (`POST /{{account.phone_number_id}}/media`), `get_media_url` (`GET /{{media_id}}`, read_only), `delete_media` (`DELETE /{{media_id}}`).
- Healthcheck: `get_phone_number`, extracting `verified_name` and `quality_rating` via response_pick. Read-only, no side effects, proves both token validity and that the configured phone_number_id belongs to it.

Every read_only function carries a non-empty `response_pick` (the official-connector gate requires it).

## Webhook block and inbox functions

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

- `dedupe_path` targets the inbound message id in Meta's webhook envelope; a delivery whose payload has no message id (a status-only update) falls back to the SHA-256 of the raw body, which is correct because status updates are distinct payloads.
- `inbox_read` (read_only, response_pick over the event envelope) and `inbox_ack` per the landed inbox function kind. A demo playbook shows the poll loop: `inbox_read` in a node, process, `inbox_ack`.
- The GET challenge (`meta_hub`) answers Meta's subscription handshake; the operator registers the printed callback URL (from `apb connector doctor`) in the Meta app console once.

## Documented limitations

- Receiving requires the server-mode + ingest deployment; a local-only apb can send but not receive. The connector docs state this in the first paragraph.
- The 24-hour customer-service window: a plain `send_text` outside the window fails with Meta error 131047; a template is required to re-initiate. The connector cannot enforce this (it is a per-recipient runtime state), so the docs flag it and `send_template` exists for exactly this case.
- `create_template` is asynchronous: it returns a PENDING status; approval arrives later and is observable only by re-listing templates or via a webhook. The description says so.
- DELETE on media and templates is a hard delete (no soft-delete concept on these Meta resources), unlike the twenty connector's soft-delete stance. The docs are explicit.

## Live verification

Offline: the standard `tests.yaml` contract cases (typed bodies, path substitution, response_pick shapes, the webhook block parsing, the challenge and signature over Meta's documented sample payload). These need no credentials and run in CI.

Live smoke (requires operator-supplied credentials, deferred until the operator provides a Meta WABA test number, app secret and a System User token, exactly as the twenty connector used a disposable demo): healthcheck, send_text to a verified test recipient, list_templates, get_business_profile, and, if a public callback is reachable, the webhook challenge plus one inbound message landing in the inbox and being read then acked. The connector ships on the strength of offline contract tests plus web-verified API facts; live smoke is a confirmation pass when credentials exist, not a merge blocker.

## Files

- `connectors/whatsapp/{connector.yaml,tests.yaml,PUBLIC.md,README.md,INSTALL.md}` (the five-file official gate).
- `crates/apb-core/src/connector/official.rs`: add `whatsapp` to the pinned name list (fourteenth connector).
- `docs/CONNECTORS.md`: a whatsapp subsection, connector count bump.
- An INSTALL runbook covering the Meta app setup: System User permanent token, phone_number_id, waba_id, app secret, verify token, and callback-URL registration against the ingest public base URL.
- A demo playbook under `examples/` showing send plus the inbox poll loop.

## Out of scope

Interactive/list messages, flows, reactions, typing indicators, the Resumable Upload API for large media, and message-triggered runs (the inbox is polled by a node, per the ingest design). These are additive later if wanted.
