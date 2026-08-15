# Atrip Connector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship an official `atrip` connector (folder `connectors/atrip/`) for AtripTech's Atlas flight-booking API, covering the full documented operation set with offline contract tests and the standard connector docs.

**Architecture:** Pure data change: one connector folder (connector.yaml, tests.yaml, PUBLIC.md, README.md, INSTALL.md) plus a subsection in docs/CONNECTORS.md. No Rust changes; the folder is picked up by the existing rust_embed folder scan. Template: `connectors/asana/`.

**Tech Stack:** apb connector YAML schema (`ConnectorDoc::from_yaml`, crates/apb-core/src/connector/def.rs), offline contract runner (`apb connector test`).

## Global Constraints

- **No git commits and no git staging in this work. None.** The deliverable is reviewed locally; the owner commits after explicit approval.
- No real network calls to any atriptech.com host and no real credentials anywhere; all testing is offline (`apb connector test`, dry-run rendering, mock cases).
- Secrets: never a literal value; `{{secret.client_secret}}` appears ONLY inside the `auth` block (validator-enforced); `{{env.*}}` references are documentation examples only.
- No em-dashes (U+2014), no exclamation marks, no CJK anywhere.
- Endpoint facts come from the research report at `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/atrip-api-research.md` — never invent an endpoint, parameter, or default not present there; anything the report marks UNVERIFIED stays out of connector.yaml and is mentioned only in README caveats.
- Do not touch `.apb/profiles/developer/profile.yaml` or anything outside `connectors/atrip/` and `docs/CONNECTORS.md`.

## Design decisions (settled, do not reopen)

- Connector name: `atrip`, version `0.1.0`.
- `account_fields`: `client_id` (required, non-secret), `client_secret` (required, secret), `base_url` (required, non-secret), `search_base_url` (required, non-secret). Rationale: production URLs are per-tenant and split search vs the rest; sandbox uses `https://sandbox.atriptech.com` for both.
- `auth`: `kind: header`, `header: x-atlas-client-id` is NOT the auth header; the auth block carries the secret: `header: x-atlas-client-secret`, `value_template: "{{secret.client_secret}}"`. The companion identifier header `x-atlas-client-id: "{{account.client_id}}"` goes into every function's `headers` map (account placeholders are legal there; secret ones are not).
- Common headers on every function: `x-atlas-client-id: "{{account.client_id}}"`, `Accept: "*/*"` (the vendor docs mandate this exact value and warn against `application/json`), `Content-Type: application/json`. `Accept-Encoding: gzip` is included ONLY if the implementer verifies the engine's ureq client decompresses gzip transparently (check Cargo features/ureq docs in the lockfile); otherwise omit it and note the omission in README.
- URLs: `{{account.search_base_url}}/<name>.do` for the search-family operations the research assigns to the search host; `{{account.base_url}}/<name>.do` for everything else. All operations are POST with a JSON body.
- No `error_when`: the API signals failure via body `status != 0`, which the equality-only `error_when` cannot express. The `status` and error-message fields must therefore survive any `response_pick` projection, and the status convention is documented in PUBLIC.md/README.md and every function description ("success means status 0 in the body").
- `read_only: false` (effectful) exactly for: the order-creating call, the payment call, void, refund, stop-ticket, regenerate-order, PNR claim, and the post-booking ancillary purchase (the 8 operations the research flags as booking/money/irreversible). Everything else is `read_only: true`.
- The payment operation has no idempotency key per the research: its description must carry an explicit retry-unsafe warning.
- `healthcheck`: set it only if a zero-argument (or fixed-args) read-only operation exists that would succeed without tenant-specific data; otherwise omit (it is optional). Verify how the healthcheck invokes the function (crates/apb-cli/src/connector.rs or def.rs) before deciding.
- `args_schema` detail level: full JSON Schema (typed properties, required arrays, enums where documented) for the operations the research documents in detail; for the rest, a permissive object schema restating only the parameters the research names, with `additionalProperties: true` and the description noting the schema is intentionally loose. Every function gets at least one `examples` entry that validates against its schema.
- `response_pick`: use only where the research documents the response shape well enough to name stable top-level fields, and always include the body `status` and message fields; otherwise omit (full body passthrough).
- `timeout_sec`: default 30 everywhere except the search-family calls; the research notes search latency, so give search calls 60.

---

### Task 1: connector.yaml + tests.yaml

**Files:**
- Create: `connectors/atrip/connector.yaml`
- Create: `connectors/atrip/tests.yaml`

**Interfaces:**
- Consumes: the connector schema (def.rs), the asana template (`connectors/asana/connector.yaml`, `connectors/asana/tests.yaml`), the research report (single source of endpoint truth).
- Produces: a connector that parses via `ConnectorDoc::from_yaml` and passes `apb connector test atrip`.

- [ ] **Step 1: read the inputs.** The research report in full; `connectors/asana/connector.yaml` and `tests.yaml` in full; the def.rs test module names listed in the survey (`headers_forbid_secret_and_auth_allow_account_and_args` and neighbors) if any rule is unclear. Survey report with schema details: `/private/tmp/claude-501/-Users-techmeat-www-projects-omniteamhq-agentic-playbooks/31c72b8b-8939-492e-a627-a2ae8b80bf24/scratchpad/apb-connector-survey.md`.
- [ ] **Step 2: write connector.yaml** per the settled decisions above, covering every operation the research verifies (28 at last count) with snake_case function names derived from the endpoint (`search.do` -> `search`, `pnr/claim.do` -> `pnr_claim`, etc), each with a one-to-two-sentence description that states what it does, the status-0 convention, and (for effectful ones) what it spends or destroys.
- [ ] **Step 3: write tests.yaml** mirroring asana's contract-case style: at minimum one rendering case per auth-relevant aspect (secret header injection, client-id header, base_url vs search_base_url routing, JSON body rendering with args), schema-validation cases (a valid and an invalid args case for the detailed schemas), and cases for the effectful operations asserting their rendered method/url/body.
- [ ] **Step 4: validate offline.** Find the exact offline loop in `crates/apb-cli/src/connector.rs` (`apb connector test` and whether it accepts a directory or needs `install --from-dir` first). Prefer the path that does not mutate the user's global config; if installation into the config dir is unavoidable, install, test, and then REMOVE the installed copy, stating that cleanup in the report. Run with the locally installed `apb` binary. Every case must pass. Also run a plain parse check if a cheaper one exists (`apb connector doctor` or the cargo test that loads official connectors: find and run the existing official-connectors parse test in the workspace, e.g. via `cargo test -p apb-core connector`).
- [ ] **Step 5: sanity greps.** No `{{secret.` outside the auth block; no literal credential-looking strings; no em-dash/exclamation/CJK; folder name and `name:` both `atrip`.

### Task 2: docs (PUBLIC.md, README.md, INSTALL.md, docs/CONNECTORS.md)

**Files:**
- Create: `connectors/atrip/PUBLIC.md`, `connectors/atrip/README.md`, `connectors/atrip/INSTALL.md`
- Modify: `docs/CONNECTORS.md` (new subsection in the official-connectors list, matching the existing entries' style)
- Modify: `crates/apb-core/src/connector/official.rs` (add `atrip` to the pinned official-connector name list so `every_official_connector_carries_the_full_file_set` and `setup_documents_name_their_own_connector` pass again; scope amendment after Task 1 discovered the pinned list)

**Interfaces:**
- Consumes: Task 1's connector.yaml (function list and account fields must match exactly), asana's doc files as templates, the research report for prose facts.
- Produces: the four standard doc surfaces.

- [ ] **Step 1:** PUBLIC.md with the same frontmatter shape as asana's: what the Atlas API is, the operation families (search, verify, order, pay, ticket, ancillaries, void, refund, PNR, misc queries), the status-0 convention, the effectful-operations warning (bookings and money movements, payment retry-unsafe), sandbox-first guidance.
- [ ] **Step 2:** README.md: obtaining credentials in the ATRIP portal, the four account fields with a sandbox example config block (both URLs = `https://sandbox.atriptech.com`, secret via `{{env.ATRIP_CLIENT_SECRET}}`), production note (two per-tenant URLs issued in the portal, no public production hostname), known caveats from the research (rate limits confirmed only for search 10 QPS and seat/luggage 60 QPM; webhook schema undocumented; payment has no idempotency key).
- [ ] **Step 3:** INSTALL.md: the standard agent runbook mirroring asana's (install, configure account, env var, approve, test), adapted to the four fields.
- [ ] **Step 4:** docs/CONNECTORS.md subsection: same length and structure as the asana entry.
- [ ] **Step 5:** consistency check: every function named in docs exists in connector.yaml and vice versa; account field names match; no em-dash/exclamation/CJK; no invented facts (spot-check three claims against the research report).

---

## Acceptance (coordinator-owned)

Per-task review, then a final review of the whole folder against the research report. Deliverable stays uncommitted; the owner decides on commit/PR.
