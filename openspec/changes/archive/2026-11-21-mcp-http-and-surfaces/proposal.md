# Proposal: MCP over HTTP and on every surface

## Why

mcp-tool-connectors (merged as #293 on main f0340685, after eval-doubles
#291) lets a node use an MCP server's tools over stdio, pinned by a person
at a terminal and sent once. Three gaps remain, and that change names
each as the next change's work:

1. Remote servers are out of reach. Its manifest rule refuses
   `transport: http` ("mcp transport `http` is not supported by this
   apb", `validate_server_shape` in `crates/apb-core/src/connector/mcp/mod.rs`,
   manifest rule 15.3.4), and its connector rule refuses a
   connector-level `auth:` next to an `mcp:` block
   (`validate_mcp_connector` in `crates/apb-core/src/connector/def.rs`,
   rule 15.3). Many services offer
   their MCP server only over Streamable HTTP with a bearer token or OAuth.
2. A person sees a server's tools only inside an approval. `apb connector
   approve <c> --account <a>` prints them before its second typed
   confirmation; `ConnectorAction` (`crates/apb-cli/src/connector.rs`
   lines 27 to 133 on main f0340685) has no read-only listing, and the
   `mcp_tool_changed` refusal points only at a person's approval.
3. No other surface knows about MCP. `apb connector doctor` (`doctor_cmd`,
   `connector.rs` line 574) does not read the pin store; MCP
   `connectors_list` (`crates/apb-mcp/src/tools/meta.rs` line 22) names
   no tool and no pin; the dashboard's connector page
   (`get_connector_handler`, `crates/apb-server/src/routes/connectors/mod.rs`
   line 336) shows neither the server nor its pins; and a connector's
   offline contract cases (`crates/apb-engine/src/connector/contract_test.rs`,
   `eval_inbox` at line 772 and its siblings) cannot check an mcp
   function's tool and arguments.

Issue 57 also keeps one manual item that mcp-tool-connectors moved here
(its out of scope 14): the host matrix and the tier-0 re-measurement on
the 2026-07-28 revision, which need real host CLIs on the owner's login.

| Decision | Code example | Non-code example |
|---|---|---|
| D5 listing surfaces | `apb connector tools tracker-mcp --account work` lists each mapped tool with its status and exits 3 because `create_comment` changed since approval | the dashboard's connector page shows the CRM's approved tools and, on "List tools", marks `add_note` "changed since approval" |
| D7 Streamable HTTP | an issue tracker's hosted MCP server over HTTPS with the existing `oauth2_refresh` account (a pre-registered client, the consent through `apb connector oauth`) | a CRM's hosted MCP server over HTTPS with a static API token in an `Authorization` header |
| D8 host matrix | the procedure for Claude Code on both revisions, Codex and opencode | the same procedure for Hermes and the supervisor long-poll |

Examples for the whole change:

1. Code: an issue tracker's MCP server over HTTPS. The connector keeps its
   OAuth in the existing connector-level `auth:` block; `create_comment`
   declares `idempotency: marker`, so a retry after a dropped connection
   finds the comment by its marker. After the tracker updates the tool,
   `apb connector tools` exits 3 and names the changed row.

```yaml
name: tracker-mcp
version: 0.1.0
healthcheck: whoami
auth:
  kind: oauth2_refresh
  token_url: "{{account.token_url}}"
  authorize_url: "{{account.authorize_url}}"
  client_id_template: "{{account.client_id}}"
  refresh_token_template: "{{secret.refresh_token}}"
mcp:
  transport: http
  protocol: modern
  url: "https://{{account.server_host}}/mcp"
```

2. Non-code: a CRM's MCP server over HTTPS with `auth: { kind: header,
   header: Authorization, value_template: "Bearer {{secret.token}}" }`:
   `list_contacts` read only, `add_note` with `idempotency: marker`,
   `delete_contact` `irreversible` with `idempotency: none` behind a
   `human_review` gate. The dashboard's connector page shows "changed
   since approval" for a tool the CRM changed and says that a person
   approves it again at a terminal.

## What changes

1. Listing surfaces (capability `mcp-connector-listing`, design D5):
   `apb connector tools <c> [--account <a>] [--json]` runs the pin check
   of the healthcheck against an approved account and lists every mapped
   tool with its status, without digests, writing nothing (exit 0 when
   every row is `pinned`, 3 otherwise, 2 on a refusal); the
   `mcp_tool_changed` refusal also names that read-only command; `apb
   connector doctor` gets one pin row per MCP connector account and
   starts no server; MCP `connectors_list` names each function's tool,
   the accounts whose pins cover it and the transport, starting no
   server; the dashboard's connector detail carries an `mcp` block whose
   account rows name their project (two projects with an account of the
   same name are two rows, each with its own approval), a new route
   `POST /api/connectors/{name}/mcp-tools/{account}?workspace=<id>`
   lists the live rows of that project's account and writes nothing,
   taking only a signed-in session (the merged healthcheck route of an
   MCP connector gets the same rule), and a new section of the connector page shows
   both, rendering server text as text; `tests.yaml` cases gain the
   expectation kind `mcp`, checked offline.
2. Streamable HTTP (capability `mcp-http-transport`, design D7):
   `transport: http` with a `url` that starts with the literal `https://`
   (host, port and path from account fields), https only on every host
   (the loopback and `localhost` included; plaintext only in apb's own
   test builds for the test servers), the endpoint and the token URL
   checked before any secret is resolved, the stateless
   2026-07-28 revision only (`protocol: modern`), and the connector-level
   `auth:` block of kind `header` or `oauth2_refresh`, minted and
   consented by the existing code (`TokenSource`, `apb connector oauth`);
   an MCP connector's token request goes through an agent with no proxy,
   and a 401 while connecting takes one refresh whose new tokens replace
   the bearer and join the redaction set before the one reconnect,
   through the same code the HTTP connectors run after a mint.
   Every answer, a JSON body or a whole event stream, passes the same
   strict reader and classifier as a stdio line before rmcp sees any of
   it, a call counts as handed over before its POST opens a connection,
   and the pin binds the endpoint and the token URL through an endpoint
   digest recomputed on every call before any secret. Header
   annotations (`x-mcp-header`) are not supported over HTTP: such a
   listing fails closed, the client sends only the protocol headers
   `mcp-protocol-version`, `mcp-method` and `mcp-name` from rmcp, and an
   `auth.header` naming a reserved header is the validator error V303.
   Every rule of mcp-tool-connectors holds unchanged; design D7
   maps each one to HTTP.
3. The host matrix of issue 57 (design D8): `docs/HOST-INTEGRATION.md`
   gets the exact procedure and an empty results table; the owner runs it
   after merge. The implementer runs no host CLI.

## Out of scope (future work)

1. MCP authorization discovery: Protected Resource Metadata (RFC 9728),
   authorization server metadata (RFC 8414, OpenID discovery), the
   `resource` parameter (RFC 8707) in the token request, the `iss` check
   (RFC 9207), issuer-keyed credentials and an MCP-specific consent. The
   authorization server is the one the approved definition and account
   name; an author can already put `resource` on the authorization request
   through `authorize_params` (`crates/apb-core/src/connector/def.rs` lines
   65 to 69). A server that requires `resource` on the refresh is not
   reachable yet.
2. Client ID Metadata Documents and dynamic client registration. The
   client id is pre-registered, as for every `oauth2_refresh` connector.
3. Servers on a revision older than 2026-07-28 over HTTP (`protocol: auto`
   or `legacy` with `transport: http`): they need the `Mcp-Session-Id`
   session, its `DELETE` and the server's `GET` event stream.
4. A proxy or an extra certificate authority for the HTTP transport, and
   certificate pinning. The transport and an MCP connector's token
   request ignore the calling process's proxy variables and use the TLS
   roots every apb HTTP call uses. The HTTP and IMAP connectors' token
   request keeps honouring them as on main (design "Pre-existing on
   main").
5. The `basic`, `query` and `path` auth kinds on an MCP connector.
6. Digests on the listing surfaces; they stay in the terminal confirmations
   of `apb connector approve` (mcp-tool-connectors, seventh pass).
7. A dashboard or MCP route that pins tools, and any surface that prints
   the approve command. Approval stays a person's act at a terminal.
8. Reusing an HTTP connection across calls. Each `apb connector call` is
   its own process and opens its own connection.
9. A connector broker for isolated nodes (V113 stays).
10. Running the host matrix. It needs host CLIs and model calls on the
    owner's login; issue 57 stays open until the owner fills the table.
11. Tool header annotations (`x-mcp-header`, the `Mcp-Param-*` request
    headers) over HTTP; a server that advertises one is refused.
12. Binding stdio `args` and `env` to a digest recomputed in a run (they
    render from the run snapshot today; design "Pre-existing on main"
    item 4), for the change that freezes a run's snapshot.

## Decisions taken in this design

1. Authorization reuses the connector-level `auth:` block and its code; no
   `mcp.auth`, no discovery, no new consent.
2. HTTP speaks the stateless 2026-07-28 revision only.
3. The listing surfaces run the healthcheck's order (`list_approved`,
   beside the unchanged healthcheck), never the approval listing, so
   nothing starts for an account nobody approved.
4. No surface shows a digest or the approve command; both stay in the
   person's terminal confirmations.
5. Every error of a write after `tools/call` is handed over settles
   `unknown` (a 401 or 403, any status, a JSON-RPC error, a broken
   stream, a timeout), through mcp-tool-connectors' one classifier change
   in `effect.rs`; this change adds no second one.
6. An http MCP endpoint is `https` only, so the peer is authenticated by
   web PKI on every call; the approval pins the URL (the endpoint and the
   token URL, through the endpoint digest) and the account, and is a
   person's act at an interactive terminal only.
7. One strict reader decides every HTTP answer, a body or a whole event
   stream, before any of it is acted on, with the same line tap functions
   the stdio transport uses; nothing is dropped over HTTP, and neither the
   transport nor an MCP connector's token request uses the calling
   process's proxy.
8. The listing surfaces serialize an explicit public shape
   (`ListingView`), never the internal listing.
9. One validator code, V303 (a reserved `auth.header` of an http MCP
   connector); no event, no field, no run guard, no manifest mark, no new
   state file.
10. The dashboard's MCP listing route and the MCP healthcheck route take
    only a signed-in session, as a publish consent does.

## Impact

1. Crates: `apb-mcp-client` (the rmcp feature
   `transport-streamable-http-client` without reqwest, `ureq`, `http.rs`
   with apb's own `StreamableHttpClient` over the line tap's strict
   reader, classifier and write gate, `Server::Http`,
   `FailureKind::Auth`); `apb-core` (`McpServerSpec.url`, the http rules,
   `url_scheme_ok`, `RESERVED_HTTP_HEADERS`, `digest::endpoint_digest`,
   the contract kind `mcp`, V303 in `validate/mcp.rs` over
   `ConnectorFacts.mcp_reserved_header`);
   `apb-engine` (the http arms of `connector/mcp/`, `TokenProxy` and
   `adopt_minted` in `oauth/token.rs`, shared by the HTTP connector path,
   `list_approved`, `http_target` (the endpoint and token URL checked
   before any secret), `ListingView`, `eval_mcp`); `apb-mcp`
   (`connectors_list` fields); `apb` CLI (`apb connector tools`, the
   doctor rows, the http approval); `apb-server` (the `mcp` block, the
   listing route); `web` (the connector page's MCP section).
2. Bundled content: none.
3. Journal: no new event and no new field. An HTTP call is a
   `connector_call` with mcp-tool-connectors' `tool_digest` and the
   endpoint `mcp+<url>#<tool>`.
4. New state: none.
5. Behaviour changes for release notes: design "Release notes".
6. Built on main f0340685 (1.0.0-beta.7, with #287 to #293). Builds on
   mcp-tool-connectors as merged (the client crate and its line tap, the
   kind, the pins, `check_pin`, `check_tools`, `open`, `print_pins`,
   `approve_mcp`); start-rules, inbox-waits,
   incremental-lists-and-session-fork and one-point-oh-fixes merge before
   or beside it (design "Overlap with the in-flight changes").
7. Validator codes: V303 (the family block V300 to V319: V300 to V302
   are mcp-tool-connectors', V304 to V319 stay reserved).
8. 53 changed files by design "Files", at most 56 (target at most about
   80).
9. The places where the merged code contradicted a decision of this
   design are decided in design "Settled decisions".
