# Design: MCP over HTTP and on every surface

Read `proposal.md` first. Every claim about main names its file, its
symbol and, where a line helps, its line on main f0340685 (1.0.0-beta.7,
after #291 eval doubles, #292 secret params are never rendered and #293
MCP tool connectors). mcp-tool-connectors is merged (#293) and archived
(`openspec/changes/archive/2026-11-21-mcp-tool-connectors/`, public record
`docs/superpowers/specs/2026-11-20-mcp-tool-connectors-design.md`); a
claim about it names its symbol on main and the section of its archived
design, whose "Wrong implementations", "Cut" sections and "Review record"
(fix rounds 1 to 5 and the pull request review) bind this change. If a
line moved, find the site by the name given next to it, never by the
number alone. Every rule of the Model Context Protocol this design relies
on is cited with its page of the 2026-07-28 revision. Rebased on main
3248c77f ("One point oh fixes" #294) in fix round 1; the line numbers
stay those of f0340685 ("Review record", "Fix round 1").

Three decisions, built in this order: D5 (the listing surfaces over stdio
connectors: `apb connector tools`, the doctor rows, the MCP
`connectors_list` fields, the dashboard section and the contract
expectation kind `mcp`), D7 (Streamable HTTP, using the existing
connector-level authorization), D8 (the written procedure of issue 57's
host matrix). D5 and D7 are both part of this change; D5 comes first
because its `list_approved` is the path D7's HTTP servers are listed
through.
The labels D5 and D7 are the 1.0 plan's, so a reader of
mcp-tool-connectors finds them under the names that change uses.

Nothing in this document is left to the implementer. Where a choice was
open it is decided here and the reason is given; tasks.md only orders the
work.

## What this change builds on (mcp-tool-connectors, on main)

These exist on main f0340685 and are used as they are unless a section
below names a change to one of them (tasks.md 0.1 checks each by name):

1. `crates/apb-mcp-client` (`src/lib.rs`, `session.rs`, `stdio.rs`,
   `tap.rs`, `frame.rs`, `testing.rs`): `Session::{connect, era,
   dropped_lines, list_tools, call_tool, close}`, `Server::Stdio(StdioServer)`
   (no `Http` variant), `EnvValue::{Plain, Secret}`, `ProgramFile`,
   `StartFrom`, `Lifecycle`, `Era` (only a constant of
   `SUPPORTED_VERSIONS`, never a server string),
   `ToolAnswer::{Complete, InputRequired, Task}`,
   `FailureKind::{Spawn, Timeout, Transport, Rejected, Unsupported,
   TooLarge, Refused, ProgramChanged}`, `Failure { kind, message, ended }`,
   `Ended::{Reaped { killed, group }, Unconfirmed, NotStarted}`,
   `GroupEnd`, `CallFailure::{NotSent, Answered, Lost}`,
   `REPLY_CLEANUP_ALLOWANCE`, `MAX_LINE_BYTES`, `MAX_SESSION_BYTES`,
   `check_arguments` with `MAX_ARGUMENT_DEPTH` and `MAX_ARGUMENT_BYTES`.
   The line tap of `src/tap.rs`: `LineTap` with its private
   `classify_line` (every outgoing line read by `frame::classify` before
   any byte is forwarded, a `tools/list` id registered first), `observe`
   (every incoming line read by `frame::incoming`, the strict reader, before
   any byte reaches rmcp; the raw `result` kept only from a whole
   successful response envelope; two frames for one watched id fail the
   listing closed), `seal` and the `WriteGate` (a `tools/call` counted as
   handed over once any byte of it was accepted, nothing forwarded after
   the seal), `take_last_listing`, the drop rule for stdout log lines
   (`MAX_DROPPED_LINES`), the counters `MAX_LINES` (10 000 messages) and
   `MAX_SESSION_BYTES`. `session.rs` `same_listing` (the raw listing must
   read as rmcp's typed one, else `mcp_listing_mismatch`) and rmcp's
   response cache turned off. `testing::StubServer`. rmcp `=3.9.0` with
   `default-features = false` and `features = ["client",
   "transport-async-rw"]`, and the `compile_error!` guard of the `testing`
   feature in `src/lib.rs`.
2. `apb_core::connector::mcp` (`crates/apb-core/src/connector/mcp/`):
   `McpServerSpec { transport, command, args, env, protocol }` (its doc
   still says "mcp-http-and-surfaces adds the http fields `url` and `auth`
   here"; this change adds `url` only and corrects that sentence),
   `McpTransportKind::{Stdio, Http}`, `McpProtocol`, `McpFunctionSpec {
   tool }`, `FunctionFacts`, `validate_function_shape` (an mcp function
   sets no HTTP field, `scopes` included), `validate_server_shape`
   (manifest rule 15.3.4, refusing `transport: http` first: "mcp transport
   `http` is not supported by this apb"),
   `digest::{tool_digest, canonical_json, pin_digest, UNPINNED_MEMBERS}`,
   `pins::{PinStore, ServerPin, ToolPin, ProgramFact::{NotRead, Gone,
   Digest}, LiveBinding, BindingDiff, PinError, program_status}` over
   `<config-dir>/mcp-tools.json`; `FunctionSpec::{is_mcp,
   is_untrusted_output}`, `ConnectorFacts.mcp_transport`, V300 to V302;
   in `crates/apb-core/src/connector/def.rs` manifest rule 15 (the doc of
   `ConnectorDoc::from_yaml`) and `validate_mcp_connector` (rule 15.3:
   "connector `{c}` has an `mcp:` block and must not set `{field}`" for
   `auth`, `webhook` and `error_when`).
3. `apb_engine::connector::mcp` (`crates/apb-engine/src/connector/mcp/`):
   in `mod.rs` `McpConnectInput`, `require_call_arguments`, `dry_run(doc,
   function, connector, account_name, account_fields, args, stamp)`,
   `check_pin`, `check_tools` (the healthcheck's check of every mapped
   tool), `CheckedPin` with its private `compare` and `refusal`, `open`,
   `Opened` with `verify`, `Start`, `OpenError`, `open_program`,
   `connect(checked, secrets, redactions: Vec<Redaction>)`, `Connected`
   (its `endpoint` `mcp+stdio://<connector>/<tool>`), `SessionHandle`
   with `close` and `close_noted`, `McpCall::{endpoint, tool_digest,
   send}`, `end_warning`, `dropped_note`, `with_end`, `with_warning`,
   `redacted`, `reflects_secret`, `reflected_secret`, `program_file`,
   `live_call_unsupported`, `MCP_PIN_CHECK_TIMEOUT` and the listing limits;
   `server::{StartPreview, preview_start, stdio_server, server_path,
   render_args}`; `map::{composed, connect_error, answer, body}` (every
   `CallError` of the kind composed and then redacted whole before it is
   bounded); `redact::{server_text, server_name, redact_tree}`;
   `pin` (already `pub mod pin`): `ToolRow`, `ProgramFacts`, `ToolListing`
   (with `cleanup`), `ConfirmedProgram`, `resolve_program`, `list_live`,
   `record`, `healthcheck` and its private `check_live`; `PreparedCall::Mcp`,
   `CallOk::Mcp`, `CallErrorCode::ToolError`, `connector_call.tool_digest`,
   and the refusals `mcp_server_unpinned`, `mcp_server_changed`,
   `mcp_tool_missing`, `mcp_tool_unpinned`, `mcp_tool_changed`,
   `mcp_tools_reflect_a_secret`, `mcp_config_dir_redirected`,
   `mcp_live_call_unsupported` and the gate's `mcp_tools_unpinned`.
4. The CLI: `crates/apb-cli/src/connector_mcp.rs` with `approve_mcp`,
   `Confirm`, `Terminal`, `refusal`, `NEEDS_A_PERSON`, `print_tree`,
   `print_program(facts, binding, preview)`, `print_pins(listing:
   &ToolListing)` and the row printer it uses, `server_lines(rows,
   unmapped)` with `ServerLines` (each row's identity always shown, server
   text within `SERVER_TEXT_TOTAL`); the test file
   `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs` and its `Fx`
   setup (a global config dir, the connector installed from a folder and
   approved, an account file, the stub, `assert_untouched` over
   `starts.log`).
5. The tests `crates/apb-engine/tests/suite/connector_mcp.rs` (its `Fx`
   pins through `PinStore::record` of a hand-built `ServerPin`;
   `no_refusal_carries_a_digest`), `connector_mcp_send.rs`,
   `connector_mcp_gate.rs`, and
   `crates/apb-server/tests/suite/connectors_api_test.rs`
   `the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins` and
   `the_dashboard_refuses_to_approve_an_mcp_connector` (its `install_mcp`
   helper pins the same way).
6. Its four orders (its D4 "The four orders"): the approval, a call in a
   run, the pin-check healthcheck, a refused live call outside a run.
   This change adds no fifth order: the listing surfaces take the
   healthcheck's order, and an HTTP server takes the same four with the
   arguments D7 names.
7. Its binding rules, carried into every HTTP arm of this change
   (archived design "Wrong implementations" and fix rounds 1 to 5): one
   strict reader decides every incoming byte before anything acts on it;
   nothing is forwarded or handed on that the reader did not understand;
   a call counts as handed over from its first byte, and every outcome
   after that is unknown for a write; every server text is redacted whole
   by the one function before any cut; the pin store holds one binding per
   connector and account; approval only at an interactive terminal; the
   dropped-line rule and its count note, never the text.

## Protocol facts this design relies on

| Rule | Where |
|---|---|
| Streamable HTTP: one POST per JSON-RPC message to one endpoint; `Accept` lists `application/json` and `text/event-stream`; the answer is one JSON object or an event stream that ends with the response | https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http |
| The 2026-07-28 core is stateless: no session id, no `initialize`; `server/discover` instead | https://modelcontextprotocol.io/specification/2026-07-28/basic/index ("Statelessness"); changelog major changes 1 and 9 |
| Tokens go in the `Authorization` header, never in the query string | https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization ("Token Requirements") |
| Authorization applies to HTTP transports; a client MAY use a pre-registered client | authorization page; https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/client-registration ("Pre-registration") |
| Discovery (RFC 9728, RFC 8414), `resource` (RFC 8707) and `iss` (RFC 9207) are what a full authorization client does; this change does not (proposal, out of scope 1) | authorization page, "Overview", "Resource Parameter Implementation", "Authorization Response Validation" |
| Tool annotations are untrusted unless from trusted servers | https://modelcontextprotocol.io/specification/2026-07-28/server/tools ("Tool") |

## The rmcp client this design uses (3.9.0, unchanged pin)

rmcp stays `=3.9.0` (mcp-tool-connectors decision 7; `Cargo.lock` on main
holds 3.9.0). Read in the local registry (`~/.cargo/registry/src/*/rmcp-3.9.0/`):

| API | Where in rmcp 3.9.0 |
|---|---|
| Feature `transport-streamable-http-client` = `client-side-sse` + `transport-worker`, no HTTP library; `client-side-sse` pulls `sse-stream`, `http`, `bytes` | `Cargo.toml` lines 83 to 88 and 151 to 154 |
| `trait StreamableHttpClient: Clone + Send + 'static` with `post_message(uri, message: ClientJsonRpcMessage, session_id, auth_header, custom_headers)`, `delete_session`, `get_stream` | `src/transport/streamable_http_client.rs` lines 474 to 560 |
| `enum StreamableHttpPostResponse { Accepted, Json(ServerJsonRpcMessage, Option<String>), Sse(..) }` | same file, lines 327 to 331 |
| `StreamableHttpClientTransport::with_client(client, config)` | same file, line 2098 |
| `StreamableHttpClientTransportConfig { uri, max_concurrent_requests, allow_stateless, auth_header, custom_headers, reinit_on_expired_session, .. }`, `with_uri`, `allow_stateless` true by default | same file, lines 2105 to 2145 and 2241 |
| The worker adds the protocol version and the standard request headers (`Mcp-Method`, `Mcp-Name`, `Mcp-Param-*`) to `custom_headers` itself; an `Mcp-Param-<name>` header carries a `tools/call` argument value whose property the cached tool schema annotates with `x-mcp-header` | same file, lines 44 to 70 (`build_request_headers`, `mcp_headers::standard_request_headers`); `src/transport/common/mcp_headers.rs` lines 270 to 316 |
| On every `Json` answer of a listing the worker keeps only the tools whose `x-mcp-header` annotations validate (`retain`), so its typed listing can differ from the raw one | same file, lines 94 to 116 (`cache_tools_from_response`) |
| A modern (discover) startup keeps no session id | same file, lines 1211 to 1224 |

Not used: the `transport-streamable-http-client-reqwest` feature (its
client decodes every answer into rmcp's typed messages, so the raw
`tools/list` JSON the tool digest needs would be lost, mcp-tool-connectors
D2 "The tool digest"; and it adds a second HTTP stack), the `auth` feature
(`auth_challenge()` exists only behind it, line 305; apb keeps one OAuth
implementation), and `max_json_response_size` (it does not exist in 3.9.0;
apb's own client caps the read). apb implements `StreamableHttpClient`
itself over `ureq` (D7 "The client").

## How the decisions of mcp-tool-connectors map to HTTP

Everything that change decided holds for an HTTP server; this table says
what each rule compares or does when the server is a URL instead of a
program.

| Rule of mcp-tool-connectors | Over stdio | Over HTTP |
|---|---|---|
| One pin binding, compared by `ServerPin::compare` before any secret is resolved or anything leaves | tree digest, account digest, program path and file digest (`ProgramFact::Digest` of the file `check_pin` opened), tool digests | tree digest, account digest, endpoint digest, tool digests. The endpoint is rendered from the definition's `url` template (in the tree) and the account's non-secret fields, which are literal values (`config::validate_accounts`, `crates/apb-core/src/connector/config.rs` lines 154 to 213: only `secret: true` fields take `{{env.VAR}}` or `{{cmd:...}}`) and are covered by `config::account_digest` (line 231). On the live paths (healthcheck, listing, approval) both digests are computed from the installed tree and the live account, but a run's call renders from its snapshot (`runs/<id>/connectors/<c>.yaml` and the manifest's account fields), which neither stored digest is recomputed over (`call/target.rs`: "drift protection is the snapshot itself"), so the two digests alone do not bind the URL a call reaches. The binding therefore names the server itself the way the merged binding names a program: the record keeps the rendered endpoint in `program` and, in `program_digest`, `digest::endpoint_digest` over the rendered endpoint and, for `oauth2_refresh`, the rendered token URL (D7 "The engine" item 1.2), recomputed from the rendered endpoint and token URL on every call, listing and approval before any secret and compared through the existing `ProgramFact::Digest` arm (a difference is `mcp_server_changed`). No new field of `ServerPin` and no change to `compare`. What the pin covers is the URL (scheme, host, port and path) of the endpoint and of the token endpoint, and the account; it does not name the peer. The peer is authenticated on every call by web PKI: both URLs are `https` only (D7 "The https rule"), and the agent keeps ureq's default certificate verification (rustls with its built-in web PKI roots, ureq 3.5.2 `src/tls/mod.rs` line 375; no TLS option is set, as for the agent of `crates/apb-engine/src/connector/call/http.rs` lines 326 to 333), so a process that takes over the port or the name of an approved server without the name's certificate gets no request with a secret and no request whose answer a pin is compared with. Each call opens its own connection (proposal, out of scope 8), so the check runs per call. No certificate pin (proposal, out of scope 4) |
| The program is fixed at approval | absolute path and sha256 of the file | the endpoint: `https` only, plaintext `http` refused on every host, the loopback and `localhost` included (D7 "The https rule"); no userinfo (`validate_url`, `crates/apb-engine/src/connector/call/render.rs` line 218), no query and no fragment |
| The server's environment comes from a fixed source | `env_clear`, the fixed set, the declared `mcp.env` | nothing of the calling process reaches the request: no proxy (`proxy(None)`; ureq's default reads `HTTPS_PROXY` and the like, ureq 3.5.2 `src/config.rs` line 1073), no redirect (`max_redirects(0)`), headers only from the definition and the account plus the three protocol headers rmcp names from the classified message (`mcp-protocol-version`, `mcp-method`, `mcp-name`); any other header name rmcp hands over is refused, a listing whose tool definitions carry `x-mcp-header` annotations fails closed, so no argument value is ever promoted into a header (D7 "The client" items 3.4 and 3.11), and a definition's `auth.header` may not name a reserved header (V303, D7 "Manifest" rule 7); the `oauth2_refresh` token request of an MCP connector goes through an agent with no proxy too (`TokenProxy::None`, D7 "The engine" item 2) |
| Approval only at an interactive terminal, two typed confirmations, the first before any secret | path, file digest, rendered args, env overrides, fields, secret commands | the same `approve_mcp` path, its terminal check (`connector_mcp::refusal`, `NEEDS_A_PERSON`) and its run-step refusal unchanged: the endpoint, the endpoint digest, the auth kind, the token endpoint and client id (`oauth2_refresh`) or the header name with its masked template (`header`), the fields, the secret commands; the question names the endpoint and the token endpoint |
| Secrets resolved only after the binding check | `resolve_secrets` after `check_pin` | the same, and inside the check `server::http_target` renders and checks the endpoint and the token URL from the account's non-secret fields before the binding is compared, so a refused endpoint or token URL starts no `{{cmd:...}}` helper (`apb_core::connector::secrets::resolve_reference`, `crates/apb-core/src/connector/secrets.rs` line 167); the token request (`TokenSource`) runs after `resolve_secrets`, so the refresh token never leaves before the binding and both URLs passed (D7 "The four orders over HTTP") |
| A listing that reflects a secret is refused before hashing | the account's resolved secrets | the same set plus the minted access token and a rotated refresh token, which the http arm of `open` pushes into the call's `redactions` before the listing, and after a 401 refresh the new access token and any newly rotated refresh token too, pushed before the reconnect (one helper, `oauth::adopt_minted`, shared with the 401 retry of `HttpCall::send`, `call/http.rs` lines 181 to 187, and the first mint of `call/mod.rs` lines 1333 to 1338) |
| One strict reader decides before anything acts (fix rounds 3 and 4) | every incoming line through `LineTap::observe` (`frame::incoming`) before any byte reaches rmcp; every outgoing line through `classify_line` (`frame::classify`) before any byte leaves | the same two functions of the same `LineTap`: every outgoing message is serialized and classified before its POST; every HTTP answer, a JSON body or each event of an event stream (framed by apb's own strict event-stream reader, D7 "The client" item 3.6), is read whole and passed through `observe` before any message of that answer reaches rmcp; a part neither reader accepts ends the session `mcp_message_invalid:` and nothing of that answer is handed on |
| The dropped-line rule and its count note | a whole stdout line not starting with `{` is dropped unread and counted (at most `MAX_DROPPED_LINES`); the note carries the count only | no line of an HTTP answer is dropped: there is no log channel in an HTTP body, so anything that is not a frame ends the session; `Session::dropped_lines` stays 0 for an HTTP session and `dropped_note` adds nothing |
| Limits compose | line cap `MAX_LINE_BYTES`, `MAX_LINES` 10 000 messages, `MAX_SESSION_BYTES`, 20 pages, 500 tools, 2 MiB, store cap 4 MiB | the body of one HTTP answer is read up to `MAX_LINE_BYTES`; every answer's bytes count toward the same `MAX_SESSION_BYTES`; every JSON message of an answer counts toward the same 10 000; the listing limits and the store cap unchanged |
| A call counts as handed over from its first byte (fix rounds 1 and 2) | `WriteGate.calls_handed` counts a `tools/call` line once the pipe took a byte; `seal` stops every later write; `NotSent` only when the count did not move | the same gate: the `tools/call` POST is counted as handed over, under the gate's lock, before apb opens its connection, and a sealed gate starts no POST; so `NotSent` is answered only for a POST apb never started, and every failure from the connection attempt on is `Lost` |
| Every error of a write after the hand-over settles `unknown` | from `call_tool` on, through mcp-tool-connectors' one classifier change (`effect.rs` `undetermined` holds `tool_error`, line 51) | from the hand-over on, through the same function and the same change: a 401 or 403, any other status, a JSON-RPC error, a broken stream, a refused frame, a timeout, an over-cap body (D7 "Settlement of an HTTP write"); only failures before the hand-over (the token request, a 401 on `server/discover` or `tools/list`, a POST never started) are pre-dispatch |
| Server text redacted whole before any cut (fix rounds 1 and 3) | `map::composed`, `redact::server_text`, `server_name`, `redact_tree` | the same functions for every text an HTTP failure carries; a client fault message holds no server-chosen string at all (no body text, no content type, no session id, no header value), only apb's words and the status number, which `map::composed` redacts with the rest |
| One server per call, closed after the settlement; never "stopped" without proof | `close` reaps the child | there is no process: `close` seals the gate, drops the transport and answers `Ended::NotStarted`; a POST still on its blocking thread ends at its own deadline; nothing claims a server stopped |
| No live MCP call outside a run; the healthcheck checks pins without a call | yes | the same; the listing surfaces use the healthcheck's path |
| No new event, field, run guard or manifest mark | yes | the same; an apb that has mcp-tool-connectors but not this change refuses the definition (`transport: http`, and a connector-level `auth` beside `mcp:`) and fails every call closed, as an older apb does for any MCP connector (its third pass, item 3) |

## D5. The listing surfaces

How a person sees an MCP connector's tools and pins without approving
anything. None of these surfaces writes a pin, prints a digest, or names
the approve command (mcp-tool-connectors D2 wrong implementation 17 and
its seventh pass: refusals carry no digest, full digests appear only in
the terminal confirmations). Pinning stays `apb connector approve`.

### `list_approved`: the healthcheck's order with rows

The draft built the live listing on `pin::list_live`. That function takes
a `ConfirmedProgram` and starts a program only after the person's first
typed confirmation (mcp-tool-connectors D2 "Capturing pins" item 5); a
surface that called it without one would start an unconfirmed program
with the account's secrets. The listing surfaces instead take the
healthcheck's order (its "The four orders" list 3), which starts only the
pinned program of an approved account.

The healthcheck already runs that order on main: `pin::healthcheck(root,
connector, account: &str) -> (Value, bool)` over the private
`pin::check_live(root, connector, account) -> Result<Value, CallError>`
(`crates/apb-engine/src/connector/mcp/pin.rs`), which takes the
`healthcheck` function, calls `check_tools` for every mapped tool,
resolves the secrets, `open`s the server and `verify`s each tool, refusing
on the first difference. The name `check_live` is taken, and the
healthcheck refuses where the listing must show rows, so the listing is a
new function next to it and the healthcheck is left as it is, but for
passing its redaction list to `open` as `&mut` (its tests are the guard; "Settled decisions" item 1).
Both share `check_tools`'s binding check, `open` (which gains the http arm
of D7) and the reflected-secret walk. In `pin.rs`:

```rust
/// The live listing of an approved account, for `apb connector tools` and
/// the dashboard. The healthcheck's order: the read-only guard
/// (`call::guard_refusal`, `crates/apb-engine/src/connector/call/mod.rs`
/// line 152), `LiveTarget::load` with the first mcp function in manifest
/// order, as `list_live` does (the single or default account when
/// `account` is `None`), `require_trust`, then `check_server`
/// (`mod.rs`, below): the config dir rule, the pin store, the record, and
/// `ServerPin::compare` of the tree digest, the account digest and the
/// program fact (the file digest for stdio, the endpoint digest for http,
/// D7 "The engine" item 1.2) with no tool required to be pinned: a `Tree`,
/// `Account`, `ProgramGone` or `Program` difference refuses
/// `mcp_server_changed` and no record refuses `mcp_server_unpinned`, both
/// before any secret is resolved or anything starts. For an http server
/// `server::http_target` runs inside that check, before the comparison
/// (D7 "The four orders over HTTP", list 3). Then `resolve_secrets`, `open`
/// (D7 for an http server), the raw listing within its limits, the close
/// (`SessionHandle::close`, its `Ended` read into `ToolListing.cleanup` by
/// the same `cleanup_note` `list_live` uses), the reflected-secret walk
/// over every listed tool, and `build_listing` with a `ConfirmedProgram`
/// built from the record (one `ToolRow` per mapped function with the
/// statuses `pinned`, `changed`, `unpinned`, `missing`, against the
/// record). Closes the session on every path. Makes no `tools/call`,
/// journals nothing, writes nothing.
pub fn list_approved(root: &Path, connector: &str, account: Option<&str>, deadline: Instant) -> Result<ToolListing, CallError>;

/// The public shape of a live listing: what `apb connector tools --json`
/// prints and the dashboard route answers. Named fields only; it is never
/// `ToolListing` serialized and filtered.
#[derive(Debug, Clone, Serialize)]
pub struct ListingView<'a> {
    pub connector: &'a str,
    pub account: &'a str,
    pub era: &'a str,
    /// `ToolListing.cleanup`: plain words when the server started for the
    /// listing could not be shown ended. Shown, because it warns that a
    /// process may still hold the account's secrets.
    pub cleanup: Option<&'a str>,
    pub server: ServerView<'a>,
    pub rows: Vec<RowView<'a>>,
    pub unmapped: &'a [String],
}

/// `ProgramFacts` without `program_digest`.
#[derive(Debug, Clone, Serialize)]
pub struct ServerView<'a> {
    pub command: &'a str,
    /// The program path (stdio) or the endpoint (http).
    pub program: &'a str,
    pub launcher: Option<&'a str>,
    pub status: &'static str,
}

/// `ToolRow` without `digest`.
#[derive(Debug, Clone, Serialize)]
pub struct RowView<'a> {
    pub function: &'a str,
    pub tool: &'a str,
    pub status: &'static str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub pinned_description: Option<&'a str>,
    pub members: &'a [String],
    pub annotations: Option<&'a serde_json::Value>,
    pub input_schema: Option<&'a serde_json::Value>,
}

/// Builds the view field by field. Its body starts with an exhaustive
/// destructuring, without `..`, of `ToolListing` (its public fields; the
/// private `pins` is bound to `_` inside `pin.rs`), `ProgramFacts` and
/// each `ToolRow`, binding every digest member to `_`: a field a later
/// change adds to one of those structs is a compile error here until that
/// change decides whether a person's surface shows it (and adds it to the
/// view and to the web types) or binds it to `_`. Digests appear only in
/// the terminal confirmations of an approval.
pub fn listing_view(listing: &ToolListing) -> ListingView<'_>;
```

`check_server` is the part of `check_tools` before its per-tool loop,
split out in `connector/mcp/mod.rs`: `check_tools` becomes `check_server`
followed by its unchanged `compare(tool, None)` for every tool, so the
healthcheck and the call keep their behaviour. `check_server` compares
through a new private `CheckedPin::compare_server`, which calls
`ServerPin::compare` with an empty tool name and reads
`Err(BindingDiff::ToolUnpinned)` as passing (in `compare` the tree,
account and program arms come before the tool arm, so every server-level
difference still refuses); no change to `apb_core`'s `compare`.

### `apb connector tools` (new)

`ConnectorAction` (`crates/apb-cli/src/connector.rs` lines 27 to 133)
gains, after `Test` (line 109):

```rust
    /// List the tools an approved MCP connector's server offers to one
    /// account, with each tool's status against its approval. Starts the
    /// approved server with the account's secrets; writes nothing.
    Tools {
        name: String,
        /// Explicit account name; omit to use the single or default one
        #[arg(long)]
        account: Option<String>,
        /// Print the listing as JSON
        #[arg(long)]
        json: bool,
    },
```

dispatched in `connector_cmd` (line 135) to `tools_cmd(root, &name,
account.as_deref(), json) -> ExitCode` in `crates/apb-cli/src/connector_mcp.rs`:

1. Inside a run step it is refused first:
   `StepCaller::from_env().refusal("listing an MCP connector's tools")`
   (`crates/apb-core/src/step_env.rs` line 37), exit 2. A step reads its
   granted functions through its node's binding; this command would start
   any approved server with its account's secrets, outside any grant.
2. A connector without `mcp:`: "connector `{c}` has no MCP server", exit 2.
3. `pin::list_approved(root, name, account, Instant::now() +
   MCP_PIN_CHECK_TIMEOUT)`. A `CallError` prints its message on stderr
   (already redacted), exit 2.
4. Text: `connector <c> account <a> (<era>)`, `server: <program>` (the
   program path for stdio, the endpoint for HTTP), then the row lines
   through the printer `print_pins` already uses, `server_lines(rows,
   unmapped)` (`crates/apb-cli/src/connector_mcp.rs`), which gains a last
   parameter `digests: bool`: `print_pins` passes `true` and keeps its
   output byte for byte, `tools_cmd` passes `false` and each row's identity
   is `function  tool  status` with no digest column. The text budget
   (`SERVER_TEXT_TOTAL`, each row's identity always shown) and the
   left-out line apply as in `print_pins`. Every text passes the same
   sanitizing as in `print_pins` (`shown`, `cut_marked`,
   `sanitize_for_terminal`, `crates/apb-cli/src/util.rs` line 85). A
   `cleanup` note is printed as `print_pins` prints it (`warning: ...`).
   When a row is not `pinned`, a last line: `a person approves changed
   tools again at a terminal (docs/CONNECTORS.md, MCP servers)`. For a
   connector whose `auth` is `oauth2_refresh`, a note line before the
   rows: `note: listing mints an access token; a provider that rotates
   refresh tokens may invalidate the stored one (docs/CONNECTORS.md, OAuth
   accounts)`, and the minted listing's `ROTATION_WARNING`, when the
   provider rotated, as a `warning:` line (a rotated refresh token is
   never stored, `token.rs` `ROTATION_WARNING`).
5. `--json`: `listing_view` (the `ListingView` shape) printed with
   `print_json`.
6. Exit 0 when every row is `pinned`, 3 when any row is `unpinned`,
   `changed` or `missing` (the "look here" code of `apb health check`,
   `crates/apb-cli/src/health.rs` line 169).

### The refusal names the listing

The `BindingDiff::Tool` arm of `CheckedPin::refusal`
(`crates/apb-engine/src/connector/mcp/mod.rs`; on main its text is
"mcp_tool_changed: tool `{tool}` of `{c}` changed since it was approved
for account `{a}`; a person approves it again at a terminal, which shows
the new definition (docs/CONNECTORS.md, MCP servers)") becomes:
`"mcp_tool_changed: tool `{t}` of `{c}` changed since it was approved for
account `{a}`; `apb connector tools {c} --account {a}` lists what
changed, and a person approves it again at a terminal, which shows the
new definition (docs/CONNECTORS.md, MCP servers)"`. That change allowed
exactly this addition; the code prefix, the class `Permission`, the
absence of any digest and of the approve command stay. Its test
`no_refusal_carries_a_digest` passes unchanged.

### `apb connector doctor`

`doctor_cmd` (`connector.rs` line 574), inside its loop over installed
connectors, for a connector whose `loaded.doc.mcp` is `Some`: `PinStore::load`
once per doctor run (a load error is one `fail` row `mcp tool pins` with
the `PinError` text), the merged accounts (`config::load_merged(root,
name)`), and for each account one row `connector `<c>`: mcp tools (<a>)`:
for every mcp function `record.compare(&LiveBinding { connector_digest:
&loaded.digest, account_digest: &config::account_digest(a), program:
ProgramFact::NotRead, tool, tool_digest: None })`. All pass: `ok`, "every
tool approved for this definition and account". No record, or any
difference: `warn`, "not approved for this definition and account:
<tools>; a person approves them at a terminal (docs/CONNECTORS.md, MCP
servers)". No server is started, no secret resolved, no file hashed:
reachability is `apb connector tools`'s, because the doctor runs often and
starting a server is running code.

### MCP `connectors_list`

`crates/apb-mcp/src/tools/meta.rs` `connectors_list` (line 22):
`PinStore::load` once per call; on an error the answer gains a top-level
`"pins_error": <fixed words>` (`PIN_STORE_UNREADABLE`, never the
reader's diagnostic) and every connector reads as having no pins. Each
connector object with `mcp:` gains `"mcp": { "transport": "stdio" |
"http", "protocol": "auto" | "modern" | "legacy" }`; each function object
of an mcp function gains `"mcp_tool": <tool>` and `"approved_accounts":
[<account names whose record passes compare for that tool, NotRead,
tool_digest None>]`. No digest, no server started; the tool stays allowed
in read-only mode (`crates/apb-mcp/src/server/read_only.rs` line 51).

### Dashboard

1. `GET /api/connectors/{name}` (`get_connector_handler`,
   `crates/apb-server/src/routes/connectors/mod.rs` line 336) adds, for a
   connector with `mcp:`, `"mcp": { "transport", "protocol", "command"
   (stdio), "url" (http: the template as written), "auth" (http: the
   connector-level kind, or `"none"`), "env": [<names>] (stdio), "accounts":
   [{ "workspace", "root_label", "account", "approved_at_ms" | null,
   "functions": [{ "function", "tool", "approved": bool, "description" }]
   }], "pins_error"? }`, from the manifest and the pin store through
   `ServerPin::compare` as the doctor does. `description` is the pin's,
   redacted at approval. No digest. The account rows carry their project:
   they are not built from `merged_accounts` (`mod.rs` line 55), which
   keeps the first project for each account name, but per project, one
   row per account of `config::load_merged(root, name)` for every
   `(workspace id, name, root)` of `enumerate_workspaces` (`state.rs` line
   181) that `connector_roots` covers, with `workspace` the workspace id
   and `root_label` the project name (both `""` on a dashboard bound to
   one project, `state.root`), and each row compared against its own
   account digest. Two projects with an account `work` are two rows, each
   with its own approval; an account of the global store that
   `load_merged` folds into every project is one row per project, labelled
   by the project a call would resolve it in. A project whose account
   config does not parse is skipped when several are listed and is the
   error with one, as `merged_accounts` does. The other users of
   `merged_accounts` (the summary list, `account_scope`, the existing
   account listing of the detail) are unchanged.
2. New `POST /api/connectors/{name}/mcp-tools/{account}`, registered in
   `crates/apb-server/src/lib.rs` after the healthcheck route (lines 213
   to 216, unchanged on f0340685), handler `mcp_tools_handler` in the same module, taking the
   row's project as `?workspace=<id>`: on a dashboard bound to one project
   (`state.root`) that root; otherwise `workspace` is required (without it
   the route answers 400, "name the project of the account") and resolves
   strictly through `resolve_root` (`state.rs` line 123, an unknown id is
   404), and the account is looked up in that project's
   `config::load_merged` (404 when it is not there), the
   `workspace.is_some()` arm of `account_scope` (`mod.rs` lines 98 to 111);
   the machine-wide arm over `merged_accounts` is never taken. The route
   takes only a signed-in session, decided in the engine (fix round 1):
   the request's `DecisionChannel` (`RequestChannel::of`) is passed to
   `list_approved`, which loads the connector once
   (`LiveTarget::load_picking`, picking the first mcp function from that
   same load) and asks `pin::refuse_caller` on that definition before
   the trust read, the pins and any secret. `refuse_caller` is
   `DecisionChannel::person_only_refusal`, the helper #294's connector
   approval route uses, so one rule decides both: a keyless dashboard
   (`DashboardKeyless`) and a bearer key (`HttpBearer`) are refused
   `mcp_needs_a_person: listing an MCP server's tools starts the server
   with the account's secrets, which only a person's channel takes
   (<refusal>); sign in to the dashboard, or run `apb connector tools` at
   a terminal`, which the route answers 403 as `{ "error":
   "mcp_needs_a_person", "message" }` (`pin::is_caller_refusal`). A
   definition that does not load fails closed (a config error, nothing
   started). The decision is never made on one load and executed on
   another, so a definition absent at one read and restored at the next
   cannot pass a caller the rule refuses. `apb connector tools` passes
   `DecisionChannel::Terminal` after its own run-step refusal, the rule
   #294's CLI approval follows. The reason: a local process
   (a run step's agent included) reaches a keyless dashboard without an
   `Origin`, and this route would start any approved server with its
   account's secrets, run its `{{cmd:...}}` helpers or mint a token
   outside any grant, which `apb connector tools` refuses a run step.
   Tests: `connectors_api_test.rs::the_mcp_tools_route_refuses_a_keyless_dashboard_and_a_bearer`
   (both 403, the stub's `starts()` empty); the route's other tests run
   on a signed-in session. Then
   `tokio::task::spawn_blocking` over
   `apb_engine::connector::mcp::pin::list_approved(&root, &name,
   Some(&account), channel, Instant::now() + MCP_PIN_CHECK_TIMEOUT)`, answering
   `{ "ok": true, "listing": listing_view(..) }` (the `ListingView`
   shape, nothing else of `ToolListing`) or `{ "ok": false, "error":
   <CallError::to_json> }`. It writes nothing. The `mcp` block of item 1
   is built from named fields too (`json!` with the keys listed there),
   and MCP `connectors_list` adds only the named keys of its section; no
   surface serializes `ToolListing`, `ServerPin` or `ToolPin` whole.
3. `web/src/lib/api/connectors.ts` (hand-written, like the rest of the
   file; `fetchConnector` line 102, `runConnectorHealthcheck` line 134):
   types `McpConnectorView`, `McpAccountView`, `McpFunctionView`,
   `McpToolRow` (the fields of `RowView`), `McpToolListing` (the fields of
   `ListingView`, its `server` the fields of `ServerView`), the optional
   `mcp` field of the connector detail, and `listMcpTools(name, account,
   workspace)`. `McpAccountView` carries `workspace` and `root_label`, and
   the section passes each row's own `workspace` (left out when `""`),
   never the page's.
4. `web/src/lib/mcpTools.ts` (new, pure): `statusLabel(status)` (`pinned`
   "approved", `unpinned` "not approved", `changed` "changed since
   approval", `missing` "gone from the server"),
   `needsApproval(listing): boolean`, and (fix round 1)
   `listButtonLabel(account, rootLabel)` ("List tools of <account>[ in
   <project>]"), `LISTING_REFUSED` ("Sign in to the dashboard to list
   tools, or run `apb connector tools` at a terminal.") and
   `listingFailure(e)`, which reads a 403 as the refused state.
5. `web/src/lib/components/McpToolsSection.svelte` (new, beside the
   page's other panels; props `connector`, `mcp`,
   `workspace`), mounted by `web/src/pages/ConnectorView.svelte` when the
   detail carries `mcp`: a section "MCP server" with the transport line,
   the command or URL template, the auth kind, the env names, a table per
   account row of functions, tools and approval, headed by the account
   name and, when `root_label` is not empty, its project, a "List tools"
   button per account row calling `listMcpTools` with that row's
   `workspace`, the live rows with `statusLabel`, the
   annotations under the caption "shown, not used", and, when
   `needsApproval`, the sentence "A person approves these tools again at
   a terminal (docs/CONNECTORS.md, MCP servers)." with no command. The
   button is the project's `ui/button` and the tables its `ui/table`, as
   on the rest of `ConnectorView.svelte`, with plain CSS over the
   existing CSS variables in the component's `<style>` block; no new
   style dependency. Each row's button has the accessible name
   `listButtonLabel`, so rows read apart; each row's result sits in a
   container with `aria-live="polite"`, `aria-busy` while it lists and
   the label "Live listing of <account>[ in <project>]". A 403 is its
   own refused state with the fixed `LISTING_REFUSED` words, never the
   server text, and when the page knows it is not signed in (the auth
   status read says no key is required, a dashboard without keys) no
   button is offered and the same words stand in its place. Every server text (descriptions, titles,
   annotations, input schemas, tool names) is rendered as text (`{value}`),
   never `{@html}`, like the untrusted connector metadata the page already
   guards (`ConnectorView.svelte` lines 322 to 324, `safeExternalUrl`).
6. The healthcheck of an MCP connector from the dashboard is
   mcp-tool-connectors' route (`healthcheck_connector_handler`), which
   starts the approved program with the account's secrets, so it had the
   same gap. This change fixes the merged route: the handler passes the
   request's `DecisionChannel` to `connector::call::healthcheck`, which
   loads the connector once (`LiveTarget::load_picking` with its
   `healthcheck` function) and, when that definition has `mcp:`, takes
   the MCP branch with the same loaded target, where `pin::healthcheck`
   asks `pin::refuse_caller` first, before the trust read and any secret
   (item 2's rule; fix round 1, so the branch is never selected on one
   load and executed on another). The route answers a refusal 403 on
   the typed outcome `call::HealthcheckOutcome::CallerRefused` of
   `call::healthcheck_outcome`, never on message text (fix round 3), and the
   healthcheck of every other connector is unchanged (its live play
   path, on the same one load, refuses a live mcp function
   `mcp_live_call_unsupported` before any secret). Test:
   `connectors_api_test.rs::the_mcp_healthcheck_route_refuses_a_keyless_dashboard_and_a_bearer`
   (both 403, the stub's `starts()` empty), with the existing
   `the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins` moved
   onto a signed-in session.

### Contract tests

`crates/apb-core/src/connector/contract.rs`: a new expectation shape

```rust
/// The mcp expectation: the tool the function maps and a subset of the
/// arguments the call would send (the marker preview included).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpExpect {
    pub tool: String,
    #[serde(default)]
    pub arguments_contain: Option<Value>,
}
```

`Expectation` gains `#[serde(default)] pub mcp: Option<McpExpect>`;
`ExpectKind` (line 263) gains `Mcp(&'a McpExpect)`; `Expectation::resolve`
returns it right after the `inbox` check. In
`crates/apb-engine/src/connector/contract_test.rs` the dispatch (the
`match kind` at lines 88 to 127) gains `ExpectKind::Mcp(expected) => eval_mcp(doc, function,
&case.account, &args, expected)`, next to `eval_inbox` (line 772):

1. `function.mcp` is `None`: "function `{f}` is not an mcp function but
   the case expects an mcp call".
2. `function.mcp.tool != expected.tool`: "tool mismatch: the case expects
   `{e}`, the function maps `{t}`".
3. `mcp::dry_run(doc, function, &doc.name, "contract", &case.account,
   &args, Some(&Stamp::Preview))` (its signature on main: `doc, function,
   connector, account_name, account_fields, args, stamp`), the same render
   path a `--dry-run` call takes; its `arguments` must contain
   `arguments_contain` by `json_subset` (line 728). No server starts, no
   secret is resolved, nothing is sent; `dry_run` only reads the pin
   store for the program it displays (`PinStore::load().ok()`), which a
   contract case never needs.

A function of another kind against an `mcp` expectation fails through the
first rule; the existing mismatch test
(`kind_mismatch_between_function_and_expectation_fails`, line 1312) is
unchanged.

### Wrong implementations (D5)

1. A listing surface over `list_live`, which needs the person's program
   confirmation. Caught by
   `connector_mcp_cli_test.rs::tools_of_an_unapproved_account_starts_nothing`
   (no record: exit 2, `mcp_server_unpinned`, no `starts.log`, the
   account's `{{cmd:...}}` helper's marker file absent).
2. A dashboard route that pins tools. Caught by
   `connectors_api_test.rs::the_mcp_tools_route_writes_no_pin`
   (`mcp-tools.json` byte-identical before and after).
3. A digest on a listing surface. Caught by
   `connector_mcp_cli_test.rs::tools_json_carries_no_digest`,
   `read_tools_test.rs::connectors_list_names_mcp_tools_without_starting_a_server`
   and `connectors_api_test.rs::the_connector_detail_carries_the_mcp_block`
   (no `sha256:` and no run of 16 or more hexadecimal characters in the
   output).
4. Naming the approve command on a surface. Caught by
   `connector_mcp_cli_test.rs::tools_lists_each_row_and_exits_3_when_changed`,
   `doctor_shows_pin_rows` and `McpToolsSection.test.ts` `shows the rows
   and no approve command` (no `apb connector approve` in any output).
5. Rendering the server's description with `{@html}`. Caught by
   `McpToolsSection.test.ts` `a description is rendered as text` (`render`
   from `svelte/server`, as `web/src/pages/Login.test.ts` line 2 does, with
   a description `<img src=x onerror=1>`: the body holds `&lt;img` and no
   `<img`).
6. `apb connector doctor` or `connectors_list` starting a server. Caught
   by `connector_mcp_cli_test.rs::doctor_starts_no_server` and the
   `read_tools_test.rs` test above (no `starts.log`).
7. Listing from a run step. Caught by
   `connector_mcp_cli_test.rs::tools_in_a_run_step_is_refused`.
8. A surface view that passes the internal listing through (`ToolListing`
   serialized whole and filtered), so a field a later change adds reaches
   the CLI, MCP or the dashboard without a decision. Caught at compile
   time by the exhaustive destructuring in `listing_view`, and by
   `pin::tests::listing_view_keeps_out_every_unlisted_field`: a
   `ToolListing` whose serialized form carries `connector_digest`,
   `account_digest`, `pin_digest`, `program.program_digest` and a row
   `digest` (the members the view does not list); the view's key sets are
   exactly the named fields of `ListingView`, `ServerView` and `RowView`,
   none of the listing's unlisted members appears at any level, and the
   text holds no `sha256:`.
9. Building the machine-wide MCP account rows through `merged_accounts`,
   so a second project's account of the same name disappears, or listing
   live tools in a project the row does not name. Caught by
   `connectors_api_test.rs::two_projects_with_the_same_account_name_are_two_rows`
   (two registered projects each with an account `work` of different
   fields, only the first approved: the machine-wide detail holds two
   `work` rows with their workspace ids and labels, `approved` true for
   the first and false for the second; the route with the first's
   `workspace` lists that project's server, which sees the request while
   the second's server sees none; with the second's `workspace` it is
   refused `mcp_server_unpinned` and nothing starts; without `workspace`
   it answers 400 and nothing starts) and `McpToolsSection.test.ts`
   `each account row names its project`.

## D7. Streamable HTTP

mcp-tool-connectors refuses `transport: http` in `validate_server_shape`
and refuses a connector-level `auth` next to an `mcp:` block. This
decision removes both refusals for HTTP and adds the URL, the client and
the call path arms, reusing the existing connector authorization.

### Manifest

`McpServerSpec` (`crates/apb-core/src/connector/mcp/mod.rs`) gains, after
`env`:

```rust
    /// Http only, required. Starts with the literal `https://`; host,
    /// port and path may hold `{{account.*}}` placeholders only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
```

Every struct literal of `McpServerSpec` gets `url: None` (tasks.md 7.1
finds them with `rg -n 'McpServerSpec \{' crates`). The doc of
`McpTransportKind::Http` loses its refusal sentence ("This apb refuses it
in `validate_server_shape`"), and the struct's doc sentence
"mcp-http-and-surfaces adds the http fields `url` and `auth` here" becomes
"`url` is the http transport's field; an http connector's authorization is
the connector-level `auth:` block": no `mcp.auth` field exists (proposal,
decision 1).

Rules, in `validate_server_shape` (manifest rule 15.3.4) and the
connector-level rules of `validate_mcp_connector` (rule 15.3,
`crates/apb-core/src/connector/def.rs`; rule 15 is listed in the doc of
`ConnectorDoc::from_yaml`, lines 651 to 657, which gains the http
sentence):

1. The first-place refusal of `transport: http` and its test
   `http_transport_is_refused_by_this_apb` are removed.
2. `transport: stdio` requires `command` and refuses `url`; `transport:
   http` requires `url` and refuses `command`, `args` and `env`: "mcp
   transport `{t}` requires `{field}`", "mcp transport `{t}` must not set
   `{field}`".
3. `transport: http` requires `protocol: modern`: "mcp transport `http`
   needs `protocol: modern`: this apb speaks only the stateless
   2026-07-28 revision over HTTP". The `mcp_timeout_too_small` rule (D1
   rule 3.4.4 there) applies under `auto` only, so it never reaches HTTP.
4. `url`, as written, passes `mcp::url_scheme_ok` (D7 "The https rule"):
   its literal text before the first `{{` starts with `https://` (ASCII
   case-insensitive), so the scheme is the definition's and no account
   field can choose it; and it holds no control character: "mcp url must
   start with the literal https://; a server that receives a header
   secret or a token is reached over https only" and "mcp `url` holds a
   control character". `url: "{{account.server_url}}"` is refused for
   this reason; the examples write `https://{{account.server_host}}/mcp`,
   and the HTTP tests `http://127.0.0.1:{{account.port}}/mcp` (the
   test-build arm).

### The https rule

An http MCP endpoint receives a secret (a header secret or a bearer
token) on every call it authenticates, and its `tools/list` answer is
what the pins are compared with, so its peer must be authenticated on
every call. The rule is one function in
`crates/apb-core/src/connector/mcp/mod.rs`:

```rust
/// Whether `url` (a template's literal prefix, or a rendered endpoint or
/// token URL) may be reached by an http MCP connector: it starts with
/// `https://`. In apb's own test builds a plaintext `http://127.0.0.1:`
/// prefix passes too, for the scripted servers of the HTTP tests, which
/// bind `127.0.0.1:0` (`common::spawn_http`,
/// `crates/apb-engine/tests/suite/common/mod.rs` line 198, and
/// `common::spawn_script`, line 435, as every connector HTTP test on main
/// reaches its local server). That arm is
/// `#[cfg(all(debug_assertions, any(test, feature = "testing")))]`, the
/// existing gate of apb-core's `testing` feature (`billing.rs` line 69,
/// `config.rs` line 490), which `lib.rs` line 8 refuses in any build
/// without debug assertions; no new feature, variable or switch. No
/// release or dist binary accepts plaintext, on the loopback or by the
/// name `localhost` included.
pub fn url_scheme_ok(url: &str) -> bool;

/// The release rule alone: `url` starts with `https://`. `url_scheme_ok`
/// is this plus the test-build arm.
fn https_only(url: &str) -> bool;
```

It is applied twice: by the manifest rule above (item 4, on the literal
prefix, so a plaintext definition never loads), and at call time by
`server::http_target` on the rendered endpoint and, for `oauth2_refresh`,
the rendered token URL (the token endpoint receives the refresh token
and the client secret; main's `check_token_url`, `token.rs` lines 481
to 503, which still admits plaintext on `127.0.0.1` and `localhost`,
stays as it is for HTTP connectors and is noted under "Pre-existing on
main"). The client crate does not check the scheme: it reaches what the
engine checked, and its own tests (`http_test.rs`) speak plaintext to a
local server. TLS is ureq's default with certificate verification on;
no code of this change calls a TLS option of the agent (the final diff
review: no `tls_config`, `TlsConfig`, `disable_verification` or
`key_log` under `crates/apb-mcp-client/` or `connector/mcp/`). Names,
rebinding and private addresses: no address filtering. The pin binds the
URL, not an address; web PKI on every fresh connection means a name that
resolves somewhere else gets nothing unless that peer holds the name's
certificate; an internal https host is allowed because a person approved
its URL at a terminal.
5. The connector-level rule 15.3 (`validate_mcp_connector`, whose text
   on main is "connector `{c}` has an `mcp:` block and must not set
   `{field}`"): `webhook` and `error_when` stay refused for both
   transports with that text; `auth` stays refused for stdio ("connector
   `{c}` has a stdio `mcp:` block and must not set `auth`") and, for
   http, is allowed with kind `header` or `oauth2_refresh` only
   (`AuthSpec`, `def.rs` lines 26 to 71): "connector `{c}` reaches its MCP
   server over http and takes `auth` of kind `header` or `oauth2_refresh`
   only, not `{kind}`". `query` and `path` would put the credential in the
   URL, which the MCP authorization rules forbid; `basic` is future work.
6. `validate_templates` (`def.rs` line 1423): `mcp.url` takes
   `{{account.*}}` only (`{{secret.*}}` and `{{args.*}}` refused, messages
   following the existing ones of that function). The `auth` block keeps
   main's template rules (`token_url` and `client_id_template`
   `{{account.*}}` only, the secret templates one `{{secret.*}}` each,
   `sole_secret`, `def.rs` line 1800). The function-level rule stays: an
   mcp function sets no `scopes` (`mcp_function_shape`, an HTTP field), so
   no scope check runs before an MCP mint.
7. Reserved headers, V303 (error), in `crates/apb-core/src/validate/mcp.rs`
   next to V300 to V302: an http MCP connector whose `auth: { kind:
   header }` names a reserved header (ASCII case-insensitive) is refused
   at every binding site: "node `{site}` binds MCP connector `{c}`, whose
   auth header `{h}` is one the HTTP transport sets itself; name another
   header". The reserved set is one constant in
   `crates/apb-core/src/connector/mcp/mod.rs`, `RESERVED_HTTP_HEADERS`:
   `mcp-protocol-version`, `mcp-method`, `mcp-name`, `content-type`,
   `accept`, `host`, `content-length`, `transfer-encoding`,
   `mcp-session-id` and `last-event-id`, with `authorization` reserved too when the connector's auth is
   `oauth2_refresh` (`reserved_http_header(name, oauth: bool)`; with one
   `auth` block per connector a `header` kind and `oauth2_refresh` never
   meet, so this arm guards a future second header source). The
   validator reads it through a new `ConnectorFacts.mcp_reserved_header:
   Option<String>` (`crates/apb-core/src/connector/resolve.rs`, `None`
   unless the rule holds). The client refuses the same set again at
   request time (D7 "The client" item 3.4), so a connector reached
   without a playbook (`apb connector tools`, the approval) fails closed
   too. Tests: `validate::mcp::tests::a_reserved_auth_header_is_v303` (each
   of `Content-Type`, `mcp-method` and `HOST`) and
   `an_ordinary_auth_header_is_not_v303` (`Authorization` with kind
   `header`, `X-Api-Key`).

`ConnectorFacts.mcp_transport` already carries the transport's name;
V300 to V302 and V113 apply to an HTTP connector unchanged (V113's
message already names "a remote server").

### The client (`crates/apb-mcp-client`)

The binding rule of mcp-tool-connectors' fix rounds 3 and 4 holds over
HTTP unchanged: rmcp never receives anything the tap did not read
completely, and nothing reaches the server that the tap did not classify
first. Over stdio rmcp reads bytes, so the tap sits on the pipes. Over
HTTP rmcp's Streamable HTTP worker hands typed messages to a
`StreamableHttpClient` and takes typed answers back, so apb's client is
that `StreamableHttpClient`, and it calls the same `LineTap` functions on
the bytes it sends and receives: one strict reader, one classifier, one
write gate, one listing capture, for both transports. A second reader, a
second capture or a second count is a wrong implementation (D7 items 15
to 18).

1. `Cargo.toml`: rmcp features `["client", "transport-async-rw",
   "transport-streamable-http-client"]` (no reqwest, no `auth`), and
   `ureq.workspace = true` (root `Cargo.toml` line 32, already used by
   `apb-core`, `apb-engine` and `apb-decide`).
2. `src/http.rs` (new; `mod http;` in `lib.rs`, `HttpServer` re-exported
   next to `StdioServer`):

```rust
/// An HTTP server to reach. No process is started.
pub struct HttpServer {
    /// The rendered endpoint (https; the engine checked it with
    /// `mcp::url_scheme_ok`).
    pub url: String,
    /// Extra request headers from the definition and the account; a
    /// secret value is exposed only into the request.
    pub headers: Vec<(String, EnvValue)>,
    /// An access token sent as `Authorization: Bearer <token>`.
    pub bearer: Option<apb_core::secret::Secret>,
}
```

   `Server` (`src/session.rs`) gains `Http(HttpServer)`.
3. `UreqClient` (private, in `http.rs`) implements rmcp's
   `StreamableHttpClient`. It holds the server's headers and bearer, the
   session's deadline, the session's `LineTap` (the same value
   `Session.tap` holds, so `dropped_lines`, `take_last_listing`, `seal`
   and the counts read one state), and a `last_fault:
   Arc<Mutex<Option<HttpFault>>>` the session reads (the pattern of the
   tap's `too_large` and `invalid`). `tap.rs` gains four `pub(crate)`
   entries over its existing private ones, used by `http.rs` only:
   `LineTap::classify_message(&self, bytes: &[u8]) -> std::io::Result<bool>`
   (`classify_line`), `LineTap::start_call(&self) -> bool` (under the
   `WriteGate` lock: `false` when sealed, else `calls_handed` plus one and
   `true`), and `LineTap::read_frame(&self, bytes: &[u8]) ->
   Result<(), String>` (one frame counted toward `MAX_LINES` in a counter
   `TapState` gains for it, since an HTTP session has no `TapReader` whose
   own counter would hold it; then `observe`; a refusal goes through
   `refuse_incoming`, as the reader's does), with `LineTap::read_body(&self,
   len: usize) -> Result<(), String>` counting each answer's bytes toward
   `MAX_SESSION_BYTES` in a second such counter. No rule of
   `frame.rs` changes. `post_message(uri, message, session_id,
   _auth_header, custom_headers)`:
   0. Once the gate is sealed, every message is answered
      `HttpFault::NotSent` and no connection is opened, a notification or
      a control message (rmcp's `notifications/cancelled` when a request
      future is dropped at apb's deadline, `control_posts`,
      `streamable_http_client.rs` line 1292) included: the stdio
      `WriteGate` rule "nothing forwarded after the seal". Test:
      `http_test.rs::nothing_is_posted_after_the_seal` (a call timed out
      at the deadline: the scripted server saw no request after the
      seal).
   1. A `session_id` is refused (`HttpFault::Transport`, "the server
      started a session; this apb speaks the stateless revision only").
      The id is never echoed.
   2. The message is serialized (`serde_json::to_vec`) and passed to
      `classify_message` before anything else: a `tools/list` id is
      registered before the request is built, and a message the tap
      cannot classify is refused (`mcp_request_unclassified:`) and seals
      the gate; nothing of it is sent. Its length is held to
      `MAX_LINE_BYTES` (`mcp_request_too_large:`), as the writer does.
   3. For a `tools/call` request, `start_call` runs right before the
      request is built: a sealed gate answers `HttpFault::NotSent` and no
      connection is opened; otherwise the call is counted as handed over
      from here, before apb opens the connection, so every failure after
      it is `Lost` (fix round 2's rule: only a request apb never started
      is `NotSent`).
   4. The request: `POST uri`, `Content-Type: application/json`, `Accept:
      application/json, text/event-stream`, the `custom_headers` entries
      whose names are `mcp-protocol-version`, `mcp-method` or `mcp-name`
      (ASCII case-insensitive), every server header (a secret through
      `Secret::expose_secret`, here and nowhere else), and
      `Authorization: Bearer <bearer>` when set. Any other header name
      rmcp hands over (an `Mcp-Param-*` header included) is refused:
      `HttpFault::Invalid` with `mcp_header_unsupported: the protocol
      library asked for a request header this client does not send`, the
      name never echoed. The names are checked right after step 2 and
      before step 3, and a refusal seals the gate, so nothing is sent and
      a refused `tools/call` is `NotSent` (it cannot happen once item 11
      held, since no tool schema with an annotation reaches rmcp). A
      server header whose name is in `RESERVED_HTTP_HEADERS` (D7
      "Manifest" rule 7) is refused the same way at the same point. The body is the bytes step 2 classified, byte for byte. Test:
      `http_test.rs::an_unknown_rmcp_header_is_refused` (the client fed a
      `custom_headers` map holding `mcp-param-region` and one holding
      `x-other`: each refused, the scripted server saw nothing) and
      `a_reserved_server_header_is_refused`.
   5. Sent in `tokio::task::spawn_blocking` with a ureq agent built like
      `HttpCall::send_raw`'s (`crates/apb-engine/src/connector/call/http.rs`
      lines 326 to 333: `max_redirects(0)`, `timeout_global(remaining)`,
      `http_status_as_error(false)`) plus `proxy(None)`. TLS is ureq's
      default, as for every apb HTTP call.
   6. The body is read before anything of it is used, up to
      `MAX_LINE_BYTES + 1`; longer is `HttpFault::TooLarge`
      (`mcp_message_too_large: an HTTP answer passed {MAX_LINE_BYTES}
      bytes`), and `read_body` counts its length toward
      `MAX_SESSION_BYTES` (past it: `mcp_message_too_large: the server sent
      more than {MAX_SESSION_BYTES} bytes`, as the reader words it). The
      caps count decoded bytes: ureq's default features (kept, root
      `Cargo.toml`) send `Accept-Encoding: gzip` and decode before the
      body is read, so a compressed answer is held to the same bounds
      once decoded. A JSON body is read to its end; an event stream is
      read as item 9 says.
   7. Status 202 or 204 with an empty body: `Accepted` when the message is
      a notification; for a request it is `HttpFault::Transport` ("the
      MCP server accepted a request without answering it"), so rmcp never
      waits to the deadline for an answer that cannot come. 401 or 403:
      `HttpFault::Auth { status }`. Any other status outside 2xx: when the
      body is a JSON-RPC response for the request's id by item 8, it is
      answered as item 8 (so a JSON-RPC error reaches rmcp as `McpError`);
      else `HttpFault::Status(status)`. Test:
      `http_test.rs::an_accepted_request_is_a_transport_fault`.
   8. The content type is the media type of `Content-Type` before the
      first `;`, trimmed, compared ASCII case-insensitive, its parameters
      ignored (`application/json; charset=utf-8` is `application/json`);
      never a substring match. `application/json`: the body is one frame:
      `read_frame` on the body bytes (the strict reader: no byte order
      mark, valid UTF-8, one JSON object with no member named twice,
      nothing around it, a JSON-RPC 2.0 frame with a well-typed id; the
      listing capture under its envelope and two-frames rules). Only
      after `read_frame` accepted it, and only when it answers the
      request (the response or error for the request's id, the same
      `answers` check as a non-2xx body and an event stream's answering
      frame; fix round 1), is the same bytes'
      `ServerJsonRpcMessage` decoded and answered `Json(message, None)`;
      a server request or another id's response is
      `HttpFault::Invalid` (`mcp_message_invalid:`) with nothing handed
      to rmcp, so a server request over HTTP is never answered: no path
      hands rmcp a server request (an event stream drops every frame but
      the answer, item 9, and the `GET` stream is refused), so rmcp never
      POSTs an answer to one. Test:
      `http_test.rs::a_json_body_that_is_not_the_answer_is_never_handed_on`
      (a `ping` request and another id's response, `mcp_message_invalid`,
      no POST after the `tools/call`);
      a decode failure is `HttpFault::Invalid` (`mcp_message_invalid:`).
      Test: `http_test.rs::a_media_type_with_parameters_is_read`
      (`application/json; charset=utf-8` and `Text/Event-Stream`
      answered; `application/jsonx` refused).
   9. `text/event-stream`: apb's own event-stream reader, in `http.rs`,
      reads the stream event by event and frames each event before any
      frame of it is used: lines end at LF or CR LF (a lone CR is
      refused); a line starting with `:` is a comment and ignored; a line
      `name:value` is the field `name` with `value`, one leading space
      after the colon stripped and no other; a line with no colon is the
      field named by the whole line with an empty value; the fields
      `data`, `event` (absent or `message` only), `id` and `retry` (both
      ignored) are read; any other field or a non-UTF-8 byte is refused.
      A blank line ends an event. An event with no `data` field, or whose
      joined `data` is empty (the priming event a server sends first so
      a client can resume), dispatches nothing and is skipped, its bytes
      still counted toward the caps. Each other event's `data` lines are
      joined with `\n` and the result is one frame passed to
      `read_frame`. Reading stops once the event carrying the response
      or error for the request's id is whole, through its terminating
      blank line: the connection is then closed and anything after it is
      never read, so a server that keeps the stream open after its
      answer no longer makes the call wait to the deadline. The stream
      ending (or the caps passed) before that event is whole is
      `HttpFault::Transport` ("the event stream ended without the
      answer"). That frame is decoded as in item 8; every earlier
      accepted frame (notifications, server requests) was read and
      counted and is not handed on, since apb declares no client
      capability. apb never hands rmcp an `Sse` answer. A refusal
      anywhere before the answer is whole hands nothing of the stream to
      rmcp and ends the session, the rule mcp-tool-connectors' pull
      request review kept for a refused line in one read (its thread 9).
   10. Any other content type: `HttpFault::Transport` ("the MCP server
       answered with a content type this client does not read"), the
       value never echoed.
   11. Header annotations are not supported over HTTP in this change. A
       `tools/list` answer accepted by `read_frame` whose `result.tools`
       holds, in any tool's definition at any depth, a member named
       `x-mcp-header` is refused before it is decoded or handed on:
       `HttpFault::Invalid` with `mcp_header_annotation_unsupported: a
       tool of this server declares x-mcp-header annotations, which this
       apb does not support over HTTP`, ending the session. So rmcp's
       worker never caches an annotated schema, never `retain`s a tool
       out of its typed listing (no `mcp_listing_mismatch` from two
       readers disagreeing), and never promotes an argument value into a
       `Mcp-Param-*` header. The engine maps it as every
       `mcp_message_invalid:`-class fault before the hand-over (a
       `transport`-coded refusal of the listing, approval or call). Test:
       `http_test.rs::a_tool_with_a_header_annotation_fails_the_http_listing_closed`
       (one valid and one invalid annotation, each refused, no
       `tools/call`).
   Every fault is written to `last_fault` before it is returned. No
   fault message carries a byte of the body, a header value or the
   content type. `delete_session` answers `Ok(())` (no session exists);
   `get_stream` answers `HttpFault::Transport` ("no event stream in the
   stateless revision"; rmcp opens one only for a legacy session or to
   resume an `Sse` answer, and apb has neither).
4. `Session::connect(Server::Http(s), Lifecycle::Modern, ..)`: the worker
   thread builds `StreamableHttpClientTransport::with_client(UreqClient,
   StreamableHttpClientTransportConfig::with_uri(url))` with
   `max_concurrent_requests(1)` and `reinit_on_expired_session: false`,
   and serves it with `ClientLifecycleMode::Discover` as for stdio
   (`Conn::open`), with the same version check against
   `SUPPORTED_VERSIONS`, the response cache turned off and `same_listing`
   on every listing. Any other `Lifecycle` with an HTTP server is
   `Unsupported` (the manifest rule makes it unreachable). A deadline
   already passed opens nothing. `guard_program_start` is not called
   (nothing starts); the call path's `guard_connector_call` is unchanged.
5. `FailureKind` gains `/// The server refused the authorization (HTTP
   401 or 403). Auth { status: u16 }`. Mapping before the hand-over
   (connect and `list_tools`): a `last_fault` of `Auth` is `Auth`,
   `TooLarge` is `TooLarge`, `Invalid` is `Transport` with the
   `mcp_message_invalid:` text, `Status` and `Transport` are `Transport`
   naming the status, an elapsed deadline is `Timeout`.
6. `call_tool` keeps its stdio rules (mcp-tool-connectors D4 item 5 and
   fix rounds 1 and 2): `NotSent` only when the gate's count did not move
   (the POST was never started: a closed session, a sealed gate, a
   message the tap refused); a JSON-RPC error answer stays
   `Answered(Rejected { code })`; every other failure is `Lost`, with kind
   `Auth { status }` for a 401 or 403 and `Transport` (the status, the
   `mcp_message_invalid:` or the `mcp_message_too_large:` text in the
   message) or `Timeout` otherwise. On a timeout the session seals and
   reads the count, as for stdio, so a POST rmcp still holds is never
   started later.
7. `close` for an HTTP session seals the gate, cancels the rmcp service,
   drops the transport and answers `Ended::NotStarted`, whose doc becomes
   "No process was started (a spawn failure, an HTTP server) or `close`
   already ran". A POST still running on its blocking thread ends at its
   own `timeout_global`, the call's deadline. `Session::dropped_lines` is
   0 for an HTTP session: nothing is dropped over HTTP.

### The engine (`crates/apb-engine/src/connector/mcp/`)

1. `server.rs`:
   1. `resolve_endpoint(spec: &McpServerSpec, fields: &BTreeMap<String,
      String>) -> Result<String, CallError>`: `render_raw`
      (`crates/apb-core/src/connector/template.rs` line 227) of `url` with
      a `RenderCtx` (line 32) of the account fields and no secret, then
      `validate_url` (`crates/apb-engine/src/connector/call/render.rs`
      line 218), then no `?` or `#` ("mcp url must not carry a query or a
      fragment"), then `mcp::url_scheme_ok` ("mcp url must use https").
      All `Config`, through `config_error` (`server.rs`), as
      `render_args` words its own.
   2. `http_target(doc: &ConnectorDoc, fields: &BTreeMap<String, String>)
      -> Result<HttpTarget, CallError>` with `pub(crate) struct HttpTarget
      { endpoint: String, token_url: Option<String>, digest: String }`:
      `resolve_endpoint`, then for `auth: oauth2_refresh` the `token_url`
      template rendered the same way (`{{account.*}}` only, `def.rs`
      `validate_templates`), then `validate_url` and `mcp::url_scheme_ok`
      ("the token_url of an MCP connector must use https"), then `digest =
      apb_core::connector::mcp::digest::endpoint_digest(&endpoint,
      token_url.as_deref())`. Both rendered URLs are refused `config`
      when they hold a control character or whitespace ("mcp url holds a
      control character or whitespace", "the token_url of an MCP
      connector holds a control character or whitespace"), since an
      account field can hold one and manifest rule 4 checks only the
      template: what the approval prints is what is reached (test
      `server::tests::http_target_refuses_whitespace_in_a_rendered_url`).
      Reads no secret, runs no helper, sends nothing. Every order of D7
      "The four orders over HTTP" calls it before `resolve_secrets`; the
      http arm of `open` takes the `HttpTarget` it returned and renders
      nothing again but for the token URL `TokenSource::render` renders
      itself (`token.rs` lines 154 and 155): `open_http` then checks that
      the token URL the source holds equals `HttpTarget.token_url` and
      refuses `mcp_server_changed` before any token request otherwise
      (the same `render_raw` over the same fields gives the same text;
      the check makes that a rule rather than an assumption).
   3. `http_server(endpoint, headers, bearer) -> HttpServer`.
   4. `preview_start` gains its http form (over `http_target`):
      `StartPreview` (`server.rs` line 23, today `args`, `env`, `fields`,
      `secret_commands`) gains `http: Option<HttpPreview>` with
      `HttpPreview { endpoint, endpoint_digest, auth, token_url,
      client_id, header }`: the rendered endpoint, its digest, the auth
      kind, for `oauth2_refresh` the rendered `token_url` and client id,
      for `header` the header name with its template, secret parts masked
      as `[secret <field> from <source>]`, plus `config::cmd_refs` in
      `secret_commands` as for stdio. No secret is resolved, no helper
      runs.
   In `crates/apb-core/src/connector/mcp/digest.rs`, next to `pin_digest`:
   `pub fn endpoint_digest(endpoint: &str, token_url: Option<&str>) ->
   String`, `sha256:` over the length-prefixed tag `mcp-http-endpoint/1`,
   the endpoint and the token URL (empty when `None`), the encoding
   `pin_digest` uses. It is what an http record keeps in `program_digest`.
2. `crates/apb-engine/src/connector/oauth/token.rs`. The URL rule is not
   changed: the MCP rule is `mcp::url_scheme_ok`, applied by `http_target`
   before any secret, and `TokenSource::render` still runs
   `check_token_url` (line 481) on a URL that already passed the stricter
   rule. Two additions:
   1. `pub(crate) enum TokenProxy { Caller, None }`. `TokenSource::render`
      (line 125) gains a last parameter `proxy: TokenProxy`, kept in a new
      field of `TokenSource`; `refresh_request` (line 279) adds
      `.proxy(None)` to its agent's config builder (lines 291 to 296) for
      `TokenProxy::None` and leaves the builder as it is for
      `TokenProxy::Caller`. The two existing callers,
      `crates/apb-engine/src/connector/call/mod.rs` line 1314 and
      `crates/apb-engine/src/connector/imap.rs` line 1020, pass
      `TokenProxy::Caller`, so the HTTP and IMAP connectors behave as on
      main (their proxy stays "Pre-existing on main" item 1); the MCP
      `open` passes `TokenProxy::None`, so an MCP connector's refresh
      token never goes to a proxy the calling process names. The cache
      key (`cache_key`, line 47) is unchanged (the proxy is not part of
      what a token is for).
   2. `pub(crate) fn adopt_minted(minted: &Minted, register: &mut dyn
      FnMut(Secret, &'static str), warnings: &mut Vec<String>) -> Secret`:
      pushes `ROTATION_WARNING` when `minted.rotated`, registers
      `minted.access` as `oauth:access_token` and `minted.new_refresh`,
      when set, as `oauth:refresh_token`, and returns the access token.
      It is the code `HttpCall::send` runs after its 401 refresh
      (`crates/apb-engine/src/connector/call/http.rs` lines 181 to 187,
      right after its `refresh_now` at line 178, `register_redaction`,
      line 97) and `call/mod.rs` runs after the first mint (lines 1333 to
      1338), moved into one function; both call it with their own
      `register` (`register_redaction`, and a push of `Redaction::new`
      into the `redactions` list) and keep their behaviour, so the HTTP
      and MCP paths share one code path for a fresh token. The MCP arm
      pushes into the call's `redactions: Vec<Redaction>`, the list
      `connect` already takes.
3. `mod.rs`:
   1. `check_tools` (and `check_server`, D5) for an http server: no
      program file is opened; `server::http_target(doc, fields)` runs
      first, before the comparison and before any secret, and the
      `CheckedPin` holds its `HttpTarget` (a new field `http:
      Option<HttpTarget>`, `program: None`) with `program_digest:
      Some(target.digest)`, so the unchanged `CheckedPin::compare` passes
      `ProgramFact::Digest(&endpoint digest)` and a record whose endpoint
      digest differs refuses `mcp_server_changed` ("the server endpoint
      changed" in `refusal`'s `Program` arm wording for http). Everything
      else is unchanged (the config dir rule, the store, the record, the
      tree and account digests, the tool's pin presence, the `home`
      lookup). How `CheckedPin` treats the two transports, stated as a
      rule: the transport is read from the installed definition
      (`spec.transport`), never from the record; for `Http` no program
      file is opened at all (no `program_file(Path::new(&record.program))`,
      whose `program` holds the endpoint), and the program fact is
      `ProgramFact::Digest` of the endpoint digest `http_target`
      computed; for `Stdio` the merged code is unchanged, so a program
      file that is missing is still `ProgramFact::Gone` and refused
      `mcp_server_changed`. `Gone` is never an http answer, and an http
      check never reads `NotRead`. Test:
      `connector_mcp_http.rs::an_http_check_opens_no_program_file` (an
      approved http account whose record's `program` is the endpoint
      URL, which names no file: the call passes the check and is
      answered; and, in `connector_mcp.rs`, the existing stdio case of a
      removed program file stays `mcp_server_changed`).
   2. `open` for an http server, after `resolve_secrets`, with the
      `HttpTarget` already checked (a new private `open_http` beside
      `open_program`, which stays stdio only): the authorization of the
      connector-level `auth`: `header` renders `value_template` with the
      secrets (as `HttpCall::auth_header` does, `call/http.rs` lines 495
      to 507) into one `(header, EnvValue::Secret)`; `oauth2_refresh`
      builds `TokenSource::render(doc.auth.as_ref(), fields, secrets,
      connector, account, TokenProxy::None)` (`token.rs` line 125) and
      `access_token(deadline, &redactions)`, then `adopt_minted` (item 2),
      which pushes the access token and any rotated refresh token into
      the call's redactions (the list of the reflected-secret walk and of
      every redacted text the call returns, `map::composed` included) and
      the `ROTATION_WARNING` when rotated; a `TokenError` is its
      `CallError`, before the intent. `open`'s `redactions` parameter
      becomes `&mut Vec<Redaction>` for this, and `connect`, the
      healthcheck and `list_approved` pass their own list. Then
      `server::http_server(endpoint, headers, Some(access))` and
      `Session::connect(Server::Http(..), Lifecycle::Modern, ..)`, then
      `list_tools` within the listing limits, as `open_program` does.
      A `Failure { kind: Auth { status: 401 } }` while connecting (on
      `server/discover` or on `tools/list`) with `oauth2_refresh` is
      retried once, the rule of `call/http.rs` lines 173 to 210 applied
      before the hand-over, in this order: (a) `TokenSource::refresh_now(deadline,
      &redactions)` (`token.rs` line 234), through the same no-proxy
      agent; (b) `adopt_minted` of its answer, so the new access token B
      and any newly rotated refresh token are in the redaction list before
      anything is sent with them; (c) the `HttpServer` value is rebuilt
      with `server::http_server(endpoint, headers, Some(B))`, the old one,
      with its bearer A, dropped (the bearer lives in the server value, so
      a reconnect over the old value would send A again); (d)
      `Session::connect` once more within the same deadline. A
      `TokenError` at (a) leaves the 401 standing: `auth`, its message
      grown by "renewing the token failed: ..." as `call/http.rs` lines
      194 to 206 do. A second `Auth`, or a 403, is `auth`. The retry
      exists only before the `tools/call` hand-over: a 401 on `tools/call`
      is never retried, and for a write it is `Lost(Auth)`, `tool_error`,
      settled `unknown` ("Settlement of an HTTP write"). Then `verify`
      (the reflected-secret walk over the redaction list that holds every
      minted token, A, B and any rotated refresh token, and the tool
      digest), as for stdio.
   3. `connect` sets `Connected.endpoint` for http to
      `mcp+<endpoint>#<tool>` (for stdio it stays
      `mcp+stdio://<connector>/<tool>`); the endpoint has no query, so no
      account-specific parameter is journaled. `McpCall::endpoint` returns
      it as today.
   4. `dry_run` for http: `{ "ok": true, "dry_run": true, "connector",
      "account", "transport": "http", "url": <rendered endpoint>, "auth":
      <kind or "none">, "tool": <tool>, "arguments": <prepared arguments>
      }`, the endpoint rendered through `http_target` (a refused endpoint
      is the dry run's `config` error). No secret, no token, no request.
4. `map.rs`: `connect_error` maps `Auth { status }` to `CallErrorCode::Auth`
   (`crates/apb-engine/src/connector/result.rs` line 21), "the MCP server
   refused the authorization (HTTP {status})", through `map::composed`
   like every message of the kind; `answer` gains one row:
   `Lost(Auth { status })` of a write is `tool_error`, "the MCP server
   answered the call with HTTP {status} after it was handed over; whether
   the tool acted is not known"; of a `read_only` function it is `auth`.
   `NotSent` keeps its row (`not_sent`: `service`, settled `not_sent`),
   reachable over HTTP only for a POST apb never started. Every other
   HTTP failure of `tools/call` reaches an existing row of the `answer`
   table. The settlement is the next section.

### Settlement of an HTTP write

The one classifier change is mcp-tool-connectors' (its D3 "Mapping", the
paragraph after the `ToolError` variant), on main: in `effect::classify`
(`crates/apb-engine/src/connector/effect.rs` lines 36 to 66), the
`undetermined` expression (lines 49 to 52) holds `|| call.outcome ==
CallErrorCode::ToolError.as_str()` (line 51). This change makes no change
to `effect.rs`, `send_once.rs` or the ledger: every failure of an HTTP
write after the hand-over maps, in the one `map::answer` both transports
use, to an outcome that expression already counts as undetermined, and
the MCP `CallError` carries `http_status: None` (the status is in its
message only), so `classify` decides on the outcome alone for stdio and
HTTP alike.

| HTTP failure of a write after the hand-over | Client answer | `map::answer` outcome | `undetermined` term (effect.rs lines 49 to 52) |
|---|---|---|---|
| 401 or 403 | `Lost(Auth { status })` | `tool_error` | `ToolError` (mcp-tool-connectors) |
| any other status outside 2xx whose body is a JSON-RPC error for the request | `Answered(Rejected { code })` | `tool_error` (mcp-tool-connectors' row, any code) | `ToolError` |
| any other status outside 2xx without one | `Lost(Transport)` | `network` | `Network` (main) |
| a refused connection after the POST was started, a reset, a broken event stream, a stream that ends without the answer, a frame the strict reader refuses, a session id, an unknown content type, an answer over the line cap, the session byte cap or the message bound | `Lost(Transport)` | `network` | `Network` (main) |
| the deadline | `Lost(Timeout)` | `timeout` | `Timeout` (main) |
| `isError`, a task, `input_required` | `Complete` / `Task` / `InputRequired` | `tool_error` (mcp-tool-connectors' rows) | `ToolError` |

The one `NotSent` an HTTP write can see is a POST apb never started (a
closed session, a sealed gate, a message the tap refused before
`start_call`): `map::answer`'s `not_sent` row, `service`, settled
`not_sent`, as for stdio, and correct, because no byte left.

The path, the same for both transports: `McpCall::send` returns
`map::answer`'s `CallError`; the call pipeline journals the
`connector_call` with `outcome` from `CallError.code` and `http_status`
from `err.http_status.or(http_status)` (`crates/apb-engine/src/connector/call/mod.rs`
lines 356 to 362), both `None` for MCP; `settle_send` (same file, line
252) builds `effect::CallFacts { outcome, http_status, response_complete }`
from those journaled fields and calls `send_once::finish`
(`call/send_once.rs` line 536), whose match (lines 549 to 559) maps
`classify(facts, false)` = `WriteUnknown` to `ledger::UNKNOWN`; only
`Failed` reaches `ledger::NOT_SENT`. No HTTP row above can produce `auth`,
`service`, `config` or `invalid_args` for a write, the outcomes `classify`
reads as `Failed` (`effect.rs` line 64). Before the hand-over nothing is
settled (mcp-tool-connectors D3 "When a call counts as handed over",
items 1 and 2), so a 401 at `server/discover` or `tools/list`
(`connect_error`, `auth`) is never a settlement.

### The approval over HTTP

1. `pin.rs`, the approval (mcp-tool-connectors "The four orders" list 1)
   for an http server:
   1. `resolve_program(command, path)` stays stdio only. `pin.rs` gains
      `pub fn http_program(doc: &ConnectorDoc, fields: &BTreeMap<String,
      String>) -> Result<(String, String), CallError>` over
      `server::http_target`, answering the endpoint and its endpoint
      digest; `approve_account` (`crates/apb-cli/src/connector_mcp.rs`)
      calls it in place of `resolve_program` for an http connector and
      builds `ProgramFacts { command: <url template>, program: <endpoint>,
      program_digest: <endpoint digest>, launcher: None, status }`,
      `status` from `pins::program_status` (display).
   2. `print_program` for http prints the binding line, `endpoint: <url>`,
      `endpoint digest: <digest> (<status>)`, `auth: <kind>`, `token
      endpoint: <url>` and `client id: <id>` (`oauth2_refresh`) or
      `header: <name>=<masked template>` (`header`) from
      `StartPreview.http`, the `field:` lines and the `secret command:`
      lines, all through the same `shown` sanitizing; no launcher warning
      and no `RUNS_AS_YOU` line (nothing runs on this machine). The first
      question is `Send the secrets of account `<a>` to <endpoint>` plus `
      and its token endpoint <token_url>` for `oauth2_refresh`, then `?
      Type the connector name to confirm:`.
   3. `list_live` for http, with `ConfirmedProgram { program: <endpoint>,
      program_digest: <endpoint digest>, .. }`: it rechecks the tree and
      account digests against the confirmation, then runs `http_target`
      on the reloaded account and compares its digest with
      `confirmed.program_digest` (the http form of its program file check,
      refused `mcp_server_changed` the same way), all before any secret;
      then `resolve_secrets`, `open` (the token, the connect, the listing),
      the close, the walk and `build_listing`, as above (D7 "The four
      orders over HTTP", list 1).
   4. `record` writes `ServerPin { command: <url template>, program:
      <endpoint>, program_digest: <endpoint digest>, .. }`; `pin_digest`
      covers them as for stdio. The doc comments of `ServerPin.command`,
      `program` and `program_digest` (`crates/apb-core/src/connector/mcp/pins.rs`)
      gain the http meaning; no field and no rule of `compare` changes.
2. The terminal rule is the merged one, unchanged: `approve_cmd`
   (`crates/apb-cli/src/connector.rs` line 345) sends every connector
   with `mcp:` through `connector_mcp::refusal` and `approve_mcp` with the
   `Terminal`, so an http MCP connector is approved only at an interactive
   terminal and never inside a run step, and the dashboard refuses it
   `mcp_approve_needs_a_person` (`crates/apb-server/src/routes/connectors/mod.rs`,
   the `loaded.doc.mcp.is_some()` arm of the approve handler).
3. The consent: `apb connector oauth` (`consent::plan`,
   `crates/apb-engine/src/connector/oauth/consent.rs` line 51) needs the
   connector's `healthcheck` (lines 63 to 69, only to load the live
   target) and reads the connector-level `oauth2_refresh` block (lines 73
   to 92); an http MCP connector that declares both gets the existing
   consent unchanged. The person stores the printed refresh token, then
   approves the account and its tools again (`approve_cmd`), since a new
   secret reference changes the account digest only when its
   `{{env.VAR}}` or `{{cmd:...}}` text changes. The consent's code
   exchange keeps main's agent and `check_token_url` ("Pre-existing on
   main" item 3; "Settled decisions" item 4).

### The four orders over HTTP

mcp-tool-connectors' "The four orders" (its D4, as merged), with the
arguments of an HTTP server. The order of the lists and of their steps is
that change's; this change adds one step, `http_target` (marked "new"),
always after everything that needs no URL, always before the binding is
compared and always before `resolve_secrets`, and no fifth order. In
every list a refusal before `resolve_secrets` resolves no secret, runs no
`{{cmd:...}}` helper and sends no request.

1. Approval of an account (`apb connector approve <c> --account <a>`):
   1. Refuse inside a run step, and without an interactive terminal
      (`mcp_approve_needs_a_person`, no digest printed).
   2. Load the installed connector (its tree digest) and the merged
      account (its digest); refuse an unapproved tree.
   3. `pin::http_program` over `http_target` (new): the endpoint and, for
      `oauth2_refresh`, the token URL rendered from the account's
      non-secret fields and checked (`mcp::url_scheme_ok`, no query, no
      userinfo), and their endpoint digest; `preview_start` (the auth
      kind, the client id or the masked header template,
      `config::cmd_refs` extracted and not run). No secret, no request.
   4. `print_program` and the first typed confirmation.
   5. `list_live`: reload the connector and the account and compare both
      digests with the confirmed ones, then `http_target` again on the
      reloaded account and its endpoint digest compared with the
      confirmed one, all before any secret
      (`mcp_approve_definition_changed`, `mcp_approve_account_changed`,
      `mcp_server_changed`).
   6. `resolve_secrets`, then `open`: the token request (`oauth2_refresh`,
      through the no-proxy agent), `Session::connect` under the fixed
      30 s (a 401 while connecting takes the one refresh of "The engine"
      item 3.2), the raw listing within its limits; the close; the
      reflected-secret walk (the minted tokens included), the digests, the
      rows and the prepared `pins` map.
   7. `print_pins` and the second typed confirmation.
   8. The account approval, then `pin::record` of the prepared map.
2. A call of an mcp function inside a run:
   1. The arguments are a JSON object (`invalid_args` otherwise).
   2. The admission decision (`send_once::preview`) and the `max_calls`
      budget.
   3. The argument schema, then `check_pin`: the config dir rule, the pin
      store, the record, `http_target` (new: the endpoint and the token
      URL checked, their endpoint digest), `ServerPin::compare` of the
      run's tree and account digests and the endpoint digest
      (`ProgramFact::Digest`), and the tool's pin. Nothing has been sent
      and no secret is read.
   4. `resolve_secrets`, then `connect`: the token request (through the
      no-proxy agent), `server/discover` under the function's deadline (a
      401 there or on `tools/list` takes one refresh, the new tokens
      registered for redaction, the server value rebuilt with the new
      bearer, and one more connect), the raw listing within its limits,
      the reflected-secret walk, the tool digest compared.
   5. `send_once::gate` records the intent; the marker is stamped. When
      step 2 sent the call to the gate unconnected and the gate would now
      admit it, the gate records nothing (`SendGate::NeedsStart`), steps 3
      and 4 run, and the gate decides again.
   6. `tools/call` is handed over: `start_call` counts it before the POST
      opens its connection (from here every error of a write is
      `unknown`; `NotSent` only for a POST never started; a crash here
      leaves a pending intent read `unknown`).
   7. A success: the `connector_call` is journaled and the intent
      settled, then `close`. A failure: `close` first, then the
      `connector_call` and the settlement. `close` answers
      `Ended::NotStarted` with no warning; a POST still on its blocking
      thread ends at its own deadline.
3. The pin-check healthcheck (unchanged), and the live listing of
   `list_approved` (`apb connector tools`, the dashboard route), which is
   this order with rows:
   1. The read-only guard; a healthcheck without a `healthcheck` function
      is refused as on main (`list_approved` takes the first mcp function
      and needs no `healthcheck`).
   2. `LiveTarget::load` and `require_trust`.
   3. `check_tools` (healthcheck) or `check_server` (`list_approved`)
      with the installed tree digest and the merged account's digest,
      `http_target` (new) inside it before the comparison.
   4. `resolve_secrets`, then `open`: the token request (through the
      no-proxy agent), `Session::connect` under the deadline (a 401 while
      connecting takes the one refresh of "The engine" item 3.2), the raw
      listing within its limits; then the reflected-secret walk, and
      `ServerPin::compare` of every mcp function's tool digest (the
      healthcheck refuses on the first difference, `list_approved` builds
      the rows).
   5. `close` (its note in `warnings` for the healthcheck, in `cleanup`
      for the listing); no `tools/call`, no journal, no write.
4. A refused live call of an mcp function outside a run:
   1. The read-only guard (`answer_live`, `call/mod.rs`).
   2. `LiveTarget::load`, and `require_trust` unless it is a dry-run.
   3. `build_prepared`: the argument schema, eval-doubles' step 5b, the
      mock and inbox branches.
   4. Branch 7a: a dry-run answered by `mcp::dry_run`, which renders its
      `url` through `http_target` (new; a refused endpoint is the dry
      run's `config` error); a live call refused
      `mcp_live_call_unsupported`. No secret is resolved, no helper runs,
      nothing is sent.

### Wrong implementations (D7)

1. Digesting rmcp's typed answer of an HTTP `tools/list` (the reqwest
   client, or decoding before the tap). Caught by
   `connector_mcp_http.rs::a_changed_execution_member_refuses_the_next_http_call`
   (the scripted server lists a tool with an `execution` member, then
   changes only it: the next call is refused `mcp_tool_changed` and the
   scripted server saw zero `tools/call` requests after the changed
   listing).
2. Settling any post-hand-over failure of an HTTP write `not_sent`, or a
   second classifier for HTTP. Caught by
   `connector_mcp_http.rs::every_failure_of_an_http_write_after_the_hand_over_settles_unknown`
   (one marker write per row of "Settlement of an HTTP write": 401, 403,
   500 with a JSON-RPC error body of code -32602, 500 with a plain body,
   an event stream cut before the answer, the deadline; for each the
   journal's `send_intent_settled.outcome` is `"unknown"` and the
   `connector_call.outcome` is the table's), and
   `a_401_on_the_call_settles_unknown` (a `none` write: the intent settles
   `unknown` and a repeat of the same intent is refused `unknown_effect`,
   the server saw one `tools/call`).
3. Sending a token before the binding check, or to another server.
   Caught by `connector_mcp_http.rs::a_changed_account_url_sends_nothing`
   (the account's `server_host` edited after approval:
   `mcp_server_changed`, neither the token endpoint nor either MCP endpoint
   saw a request).
4. Honouring the caller's proxy or a redirect. Caught by
   `http_test.rs::the_callers_proxy_is_ignored` (`HTTPS_PROXY` and
   `HTTP_PROXY` pointed at a listener that records connections: it sees
   none) and `http_test.rs::a_redirect_is_not_followed` (a 307 to a
   second server: `Transport`, the second server saw nothing). The three
   tests that set a proxy variable (`the_callers_proxy_is_ignored`,
   `the_token_request_ignores_the_callers_proxy`,
   `a_no_proxy_source_sets_no_proxy`) never set it in the test process:
   each runs its body in a child process of its own test binary
   (`std::env::current_exe()` with the test's exact name and
   `--exact`, the variables set on that child only, and a marker
   variable the body reads to know it is the child), so the other tests
   of the same binary, which build ureq agents that honour the
   variables (`send_raw`, `TokenProxy::Caller`) without holding
   `common::env_lock` (`crates/apb-engine/tests/suite/common/mod.rs`
   line 35), never see them under `cargo test`'s threads.
5. A plaintext `http://` endpoint, the loopback and `localhost`
   included, or one with a query. Caught by
   `mcp::tests::a_plaintext_url_is_refused` (`http://crm.example/mcp`,
   `http://localhost:8080/mcp`, `http://127.0.0.1:8080/mcp` and
   `{{account.server_url}}`: `https_only` refuses all four, and
   `url_scheme_ok` refuses the first, the second and the fourth and
   admits the third only through its test-build arm),
   `server::tests::http_target_refuses_plaintext_and_a_query` (a rendered
   `http://localhost:<port>/mcp`, a query, and a plaintext token URL), and
   `connector_mcp_http.rs::a_url_with_a_query_is_refused`.
6. Keeping the stdio refusal of `transport: http`. Caught by
   `mcp::tests::an_http_connector_loads`.
7. Allowing `query`, `path` or `basic` auth, or any `auth` on stdio.
   Caught by `mcp::tests::http_takes_header_or_oauth2_refresh_only` and
   `stdio_still_refuses_auth`.
8. Reading an answer past the cap, or an endless event stream. Caught by
   `http_test.rs::a_large_answer_is_too_large` and
   `an_event_stream_past_the_message_bound_is_too_large`.
9. A token in the journal, the result or a refusal. Caught by
   `connector_mcp_http.rs::the_token_never_reaches_the_journal_or_the_result`.
10. A server echoing the minted access token in its tool list hashed or
    stored. Caught by
    `connector_mcp_http.rs::a_listing_that_reflects_the_access_token_is_refused`.
11. Wording an HTTP close as a stop. Caught by
    `http_test.rs::close_of_an_http_session_is_not_started` (`Ended::NotStarted`,
    `end_warning` is `None`).
12. Checking the endpoint or the token URL after `resolve_secrets`.
    Caught by
    `connector_mcp_http.rs::a_refused_endpoint_starts_no_secret_helper`
    and `a_refused_token_url_starts_no_secret_helper` (the account's secret
    is `{{cmd:...}}` of a helper that writes a marker file; a call and a
    `list_approved` each refused `config`, the marker file absent, the
    scripted servers saw nothing).
13. Reconnecting after a 401 with the old bearer, or before the refreshed
    tokens are registered for redaction. Caught by
    `connector_mcp_http.rs::a_401_at_connect_refreshes_once` (the token
    endpoint answers access token A, then B with a rotated refresh token:
    the first `server/discover` POST carries `Authorization: Bearer A`,
    the second carries `Bearer B`, and no request after the refresh
    carries A) and
    `a_listing_that_echoes_the_refreshed_token_is_refused` (the first
    `server/discover` answers 401, the refresh returns B and a rotated
    refresh token, and the `tools/list` after the reconnect echoes B in a
    description; a second case echoes the rotated refresh token: each
    refused `mcp_tools_reflect_a_secret`, nothing hashed or stored, no
    `tools/call`, and neither value in the journal or the result).
14. Sending an MCP connector's token request through the caller's proxy.
    Caught by `connector_mcp_http.rs::the_token_request_ignores_the_callers_proxy`
    (`HTTPS_PROXY` and `HTTP_PROXY` pointed at a listener that records
    connections, set as `http_test.rs::the_callers_proxy_is_ignored` sets
    them; an `oauth2_refresh` call mints, connects and calls: the token
    endpoint saw its request, the listener saw no connection) and
    `oauth::token::tests::a_no_proxy_source_sets_no_proxy` (the agent
    config of a `TokenProxy::None` source has no proxy with the variables
    set).

15. A second reader of HTTP bytes: handing rmcp a message the strict
    reader did not accept, decoding before `read_frame`, or reading the
    body through a plain `serde_json` parse that keeps the last of two
    members of one name (mcp-tool-connectors fix round 4). Caught by
    `http_test.rs::a_byte_order_mark_body_never_reaches_rmcp`,
    `a_duplicated_member_in_a_body_is_refused`,
    `garbage_after_the_json_body_is_refused` and
    `connector_mcp_http.rs::a_duplicated_tool_member_never_matches_a_pin`
    (a `tools/list` body whose tool names `execution` twice, the first
    changed: refused `mcp_message_invalid`, no `tools/call`, no intent).
16. Handing on any frame of an event stream before the stream was framed
    and accepted up to the answer, skipping an event the reader refused,
    refusing the empty priming event, or reading past the answer. Caught
    by `http_test.rs::a_bad_event_before_the_answer_hands_on_nothing` (an
    event with an unknown field, then the answer: `Transport`, rmcp
    received nothing), `an_unknown_event_field_is_refused`,
    `a_priming_event_without_data_is_skipped` (an event with only an `id`
    and an empty `data` first: the answer after it is read),
    `the_field_value_loses_one_leading_space` (`data:  {..}` with two
    spaces is refused by the strict reader as a frame with whitespace
    around it; `data: {..}` and `data:{..}` are read) and
    `the_stream_is_closed_after_the_answer` (the scripted server sends
    the answer, then keeps the connection open and later sends a bad
    event: the call is answered well before the deadline and nothing
    after the answer's blank line is read).
17. A `tools/call` POST counted after its connection opens, or a POST
    started after the seal. Caught by
    `http_test.rs::a_refused_connection_after_the_call_started_is_lost`
    (the listener closed before the call: `Lost`, never `NotSent`),
    `a_call_after_the_seal_is_not_sent_and_never_posted` and
    `connector_mcp_http.rs::a_connection_refused_on_the_call_settles_unknown`
    (the journal's `send_intent_settled.outcome` is `unknown`).
18. A fault that echoes server-chosen text (a body, a content type, a
    session id, a header value). Caught by
    `http_test.rs::no_fault_echoes_the_server` (each carries the marker
    `ECHO-7731`; no `Failure.message` holds it) and
    `connector_mcp_http.rs::the_token_never_reaches_the_journal_or_the_result`.
19. An http record without the endpoint digest, or a check that does not
    compare it. Caught by
    `connector_mcp_http.rs::an_endpoint_digest_that_differs_refuses_the_call`
    (the record's `program_digest` edited: `mcp_server_changed`, neither
    the token endpoint nor the MCP endpoint saw a request) and
    `digest::tests::the_endpoint_digest_covers_the_token_url`.
20. An http MCP connector approved without a terminal or inside a run
    step. Caught by
    `connector_mcp_cli_test.rs::approve_of_an_http_mcp_connector_without_a_terminal_is_refused`
    (exit as for stdio, `mcp_approve_needs_a_person`, the scripted token
    endpoint and MCP endpoint saw nothing, `mcp-tools.json` absent) and
    the stdio tests of the same path, unchanged.
21. Dropping an HTTP body or event that is not a frame, as a stdout log
    line is dropped. Caught by `http_test.rs::a_plain_text_body_ends_the_session`
    (a 200 `application/json` body `hello`: `mcp_message_invalid:`,
    `dropped_lines` 0).

## D8. The host matrix of issue 57 (procedure only)

mcp-tool-connectors moved the manual host matrix and the tier-0
re-measurement of issue 57 here (its out of scope 14). They need real host
CLIs and model calls on the owner's login, so the implementer writes the
procedure and runs nothing. `docs/HOST-INTEGRATION.md` gains a section
"Host matrix on the 2026-07-28 revision (issue 57)" after "Support for
server instructions" (line 5), and its paragraph at line 18 (rewritten by
mcp-tool-connectors D6 item 5) points to it. The section holds:

1. The hosts: Claude Code on its default revision and with
   `MCP_PROTOCOL_NEGOTIATION=legacy`, Codex, opencode, Hermes.
2. Per host, what to record: the negotiated revision (`server/discover`
   or `initialize`, read from the host's MCP debug log or from `apb mcp`'s
   own log line of the first request), whether the tier-0 text (1948 bytes as
   shipped) arrives whole or where a cut text ends (one size tested, not a
   host's limit), whether the offer-to-save rule fires on
   a first-time recurring task, and whether `supervisor_wait_event`
   long-polls past 60 s.
3. The command lines to start each host against `apb mcp` in a scratch
   project, written as templates with `<host>` and `<model>` placeholders;
   no personal path, account or model preference.
4. An empty results table with one row per host and a "measured on
   (date, host version)" column.
5. The rule that issue 57 closes when the owner has filled the table.

No code, no test, and no host CLI run belong to D8.

## Unchanged from main

1. The send ledger, the marker, the call's bounds and journal, the
   attempt binding: an HTTP send is recorded, refused, reconciled and
   settled by the same code as a stdio one.
2. The connector authorization: `AuthSpec` (no new kind), `TokenSource`
   (`access_token`, `refresh_now`, the in-memory cache keyed by
   `cache_key`, `token.rs` line 47, the rotation warning; `render` gains
   only the `proxy` parameter, which its two callers on main pass as
   `TokenProxy::Caller`, and the rotation and redaction code after a mint
   moves into `adopt_minted` without a change of behaviour), the consent
   (`consent::plan`, `start`, `finish`, `exchange`, `CallbackParams`,
   `authorize_url`), `apb connector oauth` (`crates/apb-cli/src/connector_oauth.rs`).
3. Trust: tree and account approvals, `acknowledge_untrusted` never
   bypassing them.
4. Redaction: `result::redact_message`, `response::redact_value`, and
   mcp-tool-connectors' `map::composed` and `redact::{server_text,
   server_name, redact_tree}`.
5. The process-wide guards and `fsutil`.
6. `call/target.rs`, `call/send_once.rs`, `connector/effect.rs`,
   `gate/`, `event.rs`, `manifest.rs`, `guarded.rs`: no change; an HTTP
   call reaches the mcp kind through mcp-tool-connectors'
   `PreparedCall::Mcp` path and its `send_once::gate` with `NeedsStart`.
   `crates/apb-engine/src/connector/call/mod.rs`, `call/http.rs` and
   `connector/imap.rs` change only by the `TokenProxy::Caller` argument
   and the call of `adopt_minted` (item 2).
7. `validate/` but for `validate/mcp.rs` (V303) and the dispatch
   comment in `validate/mod.rs`; `crates/apb-core/src/connector/resolve.rs`
   but for the one `ConnectorFacts.mcp_reserved_header` field V303 reads
   (the transport name is already generic); the generated
   TypeScript (`web/src/lib/api.gen.ts`; the connector types are
   hand-written), `crates/apb-mcp/src/server/read_only.rs`.

Unchanged from mcp-tool-connectors: the four orders, `ServerPin::compare`,
`PinStore` and its one binding per connector and account, the tool
digest, the listing limits, `approve_mcp` and its two confirmations and
its terminal rule, the healthcheck (`pin::healthcheck` and its private
`check_live`), the gate's `mcp_tools_unpinned`, `frame.rs` (its rules
reused as they are), the stdio paths of `tap.rs` and `session.rs`, the
drop rule and its count note over stdio, eval doubles (a doubled http
function is answered at eval-doubles' step 5b like any other), the run
surfaces, the fit rows, `testing::StubServer` (the HTTP tests script
their own servers).

## Overlap with the in-flight changes

mcp-tool-connectors (#293) and eval-doubles (#291) are merged; what they
own is on main and named in "What this change builds on". Still in
flight, each in its own worktree on the owner's machine: start-rules,
inbox-waits, incremental-lists-and-session-fork and one-point-oh-fixes.
Checked on their designs at this refresh: none reserves a code in V300 to
V319 (one-point-oh-fixes reserves V320 to V339), none adds a run guard this
change needs (their guard names: `rule_start`, `list_skip`), and none
names a symbol this change adds.

| Change | Shared file | Who owns what there |
|---|---|---|
| (merged) mcp-tool-connectors | every file under `connector/mcp/`, `crates/apb-mcp-client/`, `connector_mcp.rs`, `connector_mcp_cli_test.rs`, `connectors_api_test.rs` | its symbols are main's; this change adds `list_approved`, `check_server`, `CheckedPin::compare_server`, `listing_view`, `ListingView`, `ServerView`, `RowView`, `tools_cmd`, the `digests` parameter of `server_lines`, `HttpServer`, `UreqClient`, `HttpFault`, the event-stream reader, `LineTap::{classify_message, start_call, read_frame, read_body}`, `FailureKind::Auth`, `resolve_endpoint`, `http_target`, `HttpTarget`, `http_server`, `HttpPreview`, `http_program`, `endpoint_digest`, the http arms of `check_tools`, `open`, `connect`, `dry_run`, `preview_start`, `list_live`, `record`, `print_program` and `map`, and the rule changes of D7 "Manifest" with `url_scheme_ok` and `https_only` |
| start-rules | `crates/apb-server/src/lib.rs` | its `mod trigger_sweep;` and routes; this change one route line pair |
| start-rules, inbox-waits | `docs/CONNECTORS.md` | their sentences in "Receiving events (webhooks and the inbox)" (line 343 on); this change "MCP servers" (line 503), "OAuth accounts", "Contract tests" and "The `apb connector` CLI" |
| start-rules | `Cargo.lock`, `THIRD-PARTY-NOTICES.txt` | it adds no dependency (its design: "no new dependency"); this change adds rmcp's `client-side-sse` crates |
| inbox-waits | `crates/apb-core/src/connector/def.rs` | its webhook and inbox rules; separate functions; this change touches only `validate_mcp_connector`, `validate_templates` and the rule-15 doc list |
| incremental-lists-and-session-fork | none | |
| one-point-oh-fixes | `docs/CONNECTORS.md`; the validator code block | its section "### gitlab" (its D-gitlab item 6); this change "MCP servers" and the sections above. Codes: it reserves V320 to V339, this change takes V303; no code clash |

## Validator codes

One: V303 (error), an http MCP connector's `auth.header` naming a
reserved header (D7 "Manifest" rule 7), the first free code of the
family block V300 to V319 (the 1.0 plan's): V300 to V302 are
mcp-tool-connectors' (on main); V304 to V319 stay reserved. On main
besides: V240 to V247, V249, V250, V280, V281 (eval-doubles) and V340
(secret params, #292). In flight: V248 and V251 to V259 reserved by the
people-in-the-loop family; V260 to V267 start-rules (V268 to V279
reserved); V282 reserved by eval-doubles; V283
incremental-lists-and-session-fork; V284 to V286 inbox-waits (V287 to
V299 reserved); V320 one-point-oh-fixes, which also reserves V321 to
V339. The manifest rules 1 to 6 of D7 carry no code, as every connector
manifest rule on main; rule 7 is the validator's V303.

## Docs to update

1. `docs/CONNECTORS.md`:
   1. "MCP servers" (line 503, after "Receiving events (webhooks and the
      inbox)", line 343): the http part (`transport:
      http`, `url`, `protocol: modern`, the connector-level `auth` of kind
      `header` or `oauth2_refresh`, the https rule (plaintext refused on
      every host, the loopback included; the peer authenticated by web
      PKI on every call), the endpoint and token URL checked before any
      secret, no query,
      no proxy and no redirect (the token request included), one POST per
      message, the 401 refresh before the hand-over with the new token
      sent on the one reconnect, a 401 on the call is unknown for a
      write, every answer read by the same strict reader as a stdio line
      and never a line dropped, the endpoint digest in the approval), the
      table "How a pin binds an HTTP server" in plain words, `apb connector
      tools` with its exit codes and its run-step refusal, the doctor rows,
      the dashboard's MCP section (one row per project and account, lists
      on request in that project, pins nothing, shows no digest), and what is not supported yet (proposal, out of scope 1 to
      5).
   2. "OAuth accounts" (line 134): an MCP connector over HTTP uses the
      same block and `apb connector oauth`; it needs `healthcheck`; the
      authorization server is the one the definition and account name
      (no discovery); `resource` can go on the authorization request
      through `authorize_params`.
   3. "Contract tests" (line 1389, a `###` under the testing section):
      the expectation kind `mcp`.
   4. "The `apb connector` CLI" (line 1451): `tools`.
   5. In "OAuth accounts" and in "MCP servers": every listing of an
      `oauth2_refresh` account (`apb connector tools`, the dashboard's
      "List tools") mints an access token in a fresh process; with a
      provider that rotates refresh tokens the rotation is not stored
      (`ROTATION_WARNING`), so listing can invalidate the stored refresh
      token. Header annotations (`x-mcp-header`) are not supported over
      HTTP (a listing fails closed), and the listing and healthcheck
      routes of an MCP connector take only a signed-in dashboard
      session.
2. `docs/HOST-INTEGRATION.md`: D8.
3. `docs/HOWTO-authoring.md`: the V303 row of the validator code table,
   after V302.
4. `CLAUDE.md` and `AGENTS.md` (identical shared text), limited to what
   this change adds: the `apb-mcp-client` line names the Streamable HTTP
   client over ureq; in `apb-engine` `connector/mcp/` gains `list_approved`
   and the http arms; in the CLI `connector_mcp.rs` is also `apb connector
   tools`; in `apb-server` the connectors route serves the MCP listing;
   the concepts paragraph "MCP tool connectors" says stdio and Streamable
   HTTP (the 2026-07-28 revision, the connector-level `header` or
   `oauth2_refresh` auth, the endpoint bound by the tree and account
   digests and the endpoint digest, every answer through the same strict
   reader) and that the listing surfaces write no pin and show no
   digest. The `apb-mcp-client` line ("a blocking stdio client of the
   Model Context Protocol", `CLAUDE.md` line 345) and the concepts
   paragraph ("MCP tool connectors", line 1303) are the two places.
5. The public record `docs/superpowers/specs/<implementation
   date>-mcp-http-and-surfaces-design.md`: proposal and design as one
   document, with no mention of `openspec/` and no reference to any
   outside article, author or project.

## Release notes (for the release that ships this)

1. MCP connectors can reach a server over Streamable HTTP (`transport:
   http`, the 2026-07-28 revision) with a header token or the existing
   `oauth2_refresh` account; the endpoint is https only (plaintext is
   refused, on the loopback too), is approved with the account, and is
   checked before any secret is resolved; a person approves it at an
   interactive terminal, as a stdio server; every call checks the
   approval, every answer is read by the same strict reader as a stdio
   server's, and every failure of a write after the call was handed over
   settles unknown. Neither the transport nor its token request uses the
   calling process's proxy variables.
2. `apb connector tools <c> [--account <a>]` shows an approved MCP
   server's tools with their status and exits 3 when any changed.
3. `apb connector doctor`, MCP `connectors_list` and the dashboard's
   connector page show an MCP connector's approvals without starting its
   server; the page shows one row per project and account, lists the live
   tools on request in the row's project and approves nothing.
4. Connector contract cases (`tests.yaml`) take the expectation kind `mcp`.
5. New validator error V303: an http MCP connector's auth header may not
   name a header the transport sets. Over HTTP a server whose tools carry
   `x-mcp-header` annotations is refused. The dashboard's MCP tool listing
   and the healthcheck of an MCP connector take only a signed-in session
   (a keyless dashboard or a bearer key is refused).
6. `docs/HOST-INTEGRATION.md` holds the host matrix procedure of issue 57.

## Files

Every file this change touches, by crate, on top of mcp-tool-connectors.

Workspace:

1. `Cargo.lock`
2. `THIRD-PARTY-NOTICES.txt`

apb-mcp-client:

1. `crates/apb-mcp-client/Cargo.toml`
2. `crates/apb-mcp-client/src/lib.rs`
3. `crates/apb-mcp-client/src/session.rs`
4. `crates/apb-mcp-client/src/http.rs` (new)
5. `crates/apb-mcp-client/src/tap.rs` (`classify_message`, `start_call`,
   `read_frame`, `read_body` and their two counters)
6. `crates/apb-mcp-client/tests/main.rs`
7. `crates/apb-mcp-client/tests/suite/http_test.rs` (new)

apb-core:

1. `crates/apb-core/src/connector/mcp/mod.rs`
2. `crates/apb-core/src/connector/mcp/pins.rs` (doc comments)
3. `crates/apb-core/src/connector/mcp/digest.rs` (`endpoint_digest`)
4. `crates/apb-core/src/connector/def.rs`
5. `crates/apb-core/src/connector/contract.rs`
6. `crates/apb-core/src/connector/resolve.rs` (`mcp_reserved_header`)
7. `crates/apb-core/src/validate/mcp.rs` (V303)

apb-engine:

1. `crates/apb-engine/src/connector/mcp/mod.rs`
2. `crates/apb-engine/src/connector/mcp/server.rs`
3. `crates/apb-engine/src/connector/mcp/map.rs`
4. `crates/apb-engine/src/connector/mcp/pin.rs`
5. `crates/apb-engine/src/connector/contract_test.rs`
6. `crates/apb-engine/tests/main.rs`
7. `crates/apb-engine/tests/suite/connector_mcp_http.rs` (new)
8. `crates/apb-engine/tests/suite/connector_mcp.rs`
   (`a_changed_tool_refusal_names_the_listing`)
9. `crates/apb-engine/src/connector/oauth/token.rs` (`TokenProxy`,
   `adopt_minted`)
10. `crates/apb-engine/src/connector/call/http.rs` (`adopt_minted`)
11. `crates/apb-engine/src/connector/call/mod.rs` (`TokenProxy::Caller`,
    `adopt_minted`; fix round 1: `healthcheck` takes the caller's channel
    over one load, `prepare_live`)
12. `crates/apb-engine/src/connector/imap.rs` (`TokenProxy::Caller`)
13. `crates/apb-engine/src/connector/call/target.rs` (fix round 1:
    `LiveTarget::load_picking`, `name`, `doc`, `is_mcp`)
14. `crates/apb-engine/src/read_only.rs` and
    `crates/apb-engine/src/scenario/guard.rs` (fix round 1: the channel
    argument in their healthcheck tests)
15. `crates/apb-engine/tests/suite/connector_healthcheck.rs` and
    `crates/apb-engine/tests/suite/connector_e2e.rs` (fix round 1: the
    channel argument)

apb CLI:

1. `crates/apb-cli/src/connector.rs`
2. `crates/apb-cli/src/connector_mcp.rs`
3. `crates/apb-cli/tests/suite/connector_mcp_cli_test.rs`

apb-mcp:

1. `crates/apb-mcp/src/tools/meta.rs`
2. `crates/apb-mcp/tests/suite/read_tools_test.rs`

apb-server:

1. `crates/apb-server/src/routes/connectors/mod.rs`
2. `crates/apb-server/src/lib.rs`
3. `crates/apb-server/tests/suite/connectors_api_test.rs`

web:

1. `web/src/lib/api/connectors.ts`
2. `web/src/lib/mcpTools.ts` (new)
3. `web/src/lib/mcpTools.test.ts` (new)
4. `web/src/lib/components/McpToolsSection.svelte` (new)
5. `web/src/lib/components/McpToolsSection.test.ts` (new)
6. `web/src/lib/components/McpToolsSection.svelte.test.ts` (new, the
   mounted cases)
7. `web/src/pages/ConnectorView.svelte`

Docs:

1. `docs/CONNECTORS.md`
2. `docs/HOST-INTEGRATION.md`
3. `docs/HOWTO-authoring.md` (the V303 row)

Records:

1. `openspec/changes/mcp-http-and-surfaces/proposal.md`
2. `openspec/changes/mcp-http-and-surfaces/design.md`
3. `openspec/changes/mcp-http-and-surfaces/tasks.md`
4. `openspec/changes/mcp-http-and-surfaces/specs/mcp-connector-listing/spec.md`
5. `openspec/changes/mcp-http-and-surfaces/specs/mcp-http-transport/spec.md`
6. `docs/superpowers/specs/<implementation date>-mcp-http-and-surfaces-design.md`
7. `CLAUDE.md`
8. `AGENTS.md`

Count: 2 + 7 + 7 + 12 + 3 + 2 + 3 + 6 + 3 + 8 = 53 files (the design
review added `resolve.rs`, `validate/mcp.rs`, the engine's
`connector_mcp.rs` and `HOWTO-authoring.md`; `validate/mod.rs` changes
only in its dispatch comment if at all). The struct
literal sites of `McpServerSpec` that the draft counted (up to 3) do not
exist on main f0340685 (`rg -n 'McpServerSpec \{' crates` finds only the
definition), so they add nothing; up to 3 test helper files a test may
need (a shared scripted MCP endpoint, a fixture) are allowed, so at most
56. The target is at most about 80; nothing is split off.

Read and relied on, not changed (a change to one of them is a difference
to report): `call/render.rs`, `call/send_once.rs`, `call/target.rs`,
`connector/effect.rs` (mcp-tool-connectors' `tool_error` term is the
only classifier change), `oauth/consent.rs`,
`crates/apb-core/src/connector/oauth.rs`, `crates/apb-core/src/connector/config.rs`,
`crates/apb-cli/src/connector_oauth.rs`, `crates/apb-mcp-client/src/frame.rs`,
`crates/apb-mcp-client/src/stdio.rs`, `crates/apb-mcp-client/src/testing.rs`,
`crates/apb-engine/src/connector/mcp/redact.rs`,
`crates/apb-engine/tests/suite/common/mod.rs` (`spawn_script`, line 435),
`crates/apb-mcp/src/server/read_only.rs`, `web/src/lib/api.gen.ts`.

## Settled decisions

The refresh against main f0340685 found places where the merged code, or
the archived design of mcp-tool-connectors, contradicted a decision of
this design rather than a name. Each is decided; the design review of
2026-11-21 settled them as follows.

1. The healthcheck and the listing: a new `list_approved` (with
   `check_server`) beside the healthcheck, which stays untouched but for
   the `&mut` redaction list `open` takes. The name `check_live` stays
   the healthcheck's private body.
2. An http connector's authorization lives in the connector-level
   `auth:` block (decision 1); the doc comment on `McpServerSpec` that
   expected an `mcp.auth` field is corrected, and no `mcp.auth` exists.
3. The HTTP client's reader is the `StreamableHttpClient` over ureq that
   calls the tap's own `classify_line`, `observe` and write gate
   (`classify_message`, `read_frame`, `start_call`), with apb's strict
   event-stream reader framing a stream up to its answer before any frame
   is handed on. HTTP is not presented to rmcp as a byte pipe.
4. The consent's token exchange keeps main's agent and `check_token_url`
   ("Pre-existing on main" item 3); it runs only at a person's terminal,
   and decision 1 keeps the consent unchanged. A small follow-up change
   gives it the no-proxy agent and the https rule.
5. The endpoint in the pin binding: the record keeps `endpoint_digest`
   over the rendered endpoint and, for `oauth2_refresh`, the rendered
   token URL in `program_digest`, recomputed from the installed
   definition and account on every call, listing and approval, before
   any secret, and compared in the single pin comparison
   (`ServerPin::compare` through `ProgramFact::Digest`); a difference is
   `mcp_server_changed`. The tree and account digests alone do not bind
   the URL in a run: a run renders the endpoint from its snapshot
   (`runs/<id>/connectors/<c>.yaml` and the manifest's account fields),
   which no digest is recomputed over, so the endpoint digest is what
   ties the bytes the URL is rendered from to the approval. No program
   file is opened for http, and `ProgramFact::Gone` stays the stdio
   answer for a missing file (D7 "The engine" item 3.1).
6. A call counts as handed over before its POST opens a connection: only
   a POST apb never started is `NotSent`; a refused connection or a
   failed name lookup is `Lost` and settles `unknown` although nothing
   may have left.
7. `ListingView.cleanup` is shown: a warning that a process may still
   hold the account's secrets, carrying no digest.
8. Header annotations are not supported over HTTP in this change: a
   listing whose tools carry `x-mcp-header` fails closed
   (`mcp_header_annotation_unsupported`), the client sends only the
   rmcp headers `mcp-protocol-version`, `mcp-method` and `mcp-name` and
   refuses any other name rmcp hands over, and an MCP http `auth.header`
   naming a reserved header is the validator error V303. No argument
   value is ever promoted into a header.
9. The dashboard's MCP listing route and the merged healthcheck route of
   an MCP connector take only a signed-in session, as a publish consent
   does.

## Review record

### Closure pass, 2026-11-20

The draft was written on 316c8180, before #287 to #290 merged and before
mcp-tool-connectors went through its seven review passes. This pass
checked every claim against main efae5822 (`zg --rg -F`, then the line
read), against rmcp 3.9.0 and ureq 3.5.2 in the local registry, and
against mcp-tool-connectors (its seventh pass, rebased on eval-doubles)
and the four in-flight designs. What it changed:

1. Base and sequence: main efae5822; the in-flight changes by their
   current names (eval-doubles, start-rules, inbox-waits,
   incremental-lists-and-session-fork); mcp-tool-connectors' symbols by
   name only.
2. rmcp: the draft assumed rmcp 3.11.0 with the reqwest client,
   `max_json_response_size` and `auth_challenge()`. mcp-tool-connectors
   keeps rmcp at 3.9.0; 3.9.0's config has no `max_json_response_size`
   (lines 2105 to 2145) and `auth_challenge()` exists only behind the
   `auth` feature (line 305). More important, rmcp's reqwest client
   decodes every answer into typed messages, so an HTTP `tools/list`
   would be digested from rmcp's `Tool`, the exact defect
   mcp-tool-connectors' M1 fixed for stdio. The client is now apb's own
   `StreamableHttpClient` over ureq, which taps the raw result before
   decoding (D7 "The client").
3. Authorization: the draft specified its own OAuth flow (`mcp.auth`,
   Protected Resource Metadata and authorization server discovery, the
   `resource` parameter, the `iss` check, `plan_mcp`,
   `CallbackParams.iss`, the issuer field and line). Cut to reuse: an
   HTTP MCP connector uses the existing connector-level `auth:` block of
   kind `header` or `oauth2_refresh`, `TokenSource`, its cache and its
   refresh-once rule, and the unchanged `apb connector oauth`. The rest is
   future work (proposal, out of scope 1).
4. Pin binding over HTTP: stated for every rule of mcp-tool-connectors (the
   mapping table). The endpoint is a function of the tree and account
   digests (non-secret account fields are literal, `config.rs` lines 154
   to 211), so no extra fact is compared; TLS identity is what every apb
   HTTP call checks (ureq's default rustls roots), with no pin.
5. Listing surfaces: the draft built `apb connector tools` and the
   dashboard route on `list_live`, which since mcp-tool-connectors' fifth
   pass needs the person's program confirmation; they now run
   `check_live`, the healthcheck's order, which starts only the pinned
   program of an approved account. The draft's healthcheck test duplicated
   mcp-tool-connectors' `the_dashboard_healthcheck_of_an_mcp_connector_checks_the_pins`
   and is dropped.
6. Refusal and surface text: the draft's refusal named `apb connector
   approve` and the doctor row and the dashboard printed it with a copy
   button, and the listing printed digest prefixes; mcp-tool-connectors
   forbids both (its D2 wrong implementation 17 and seventh pass). The
   refusal now adds only `apb connector tools`; no surface prints a
   digest or the approve command (`listing_view` removes named digest
   members from the whole listing, lesson b).
7. Settlement: the draft settled a 401 or 403 on `tools/call` `not_sent`.
   Every error of a write after the hand-over settles `unknown`
   (`Lost(Auth)` row of `map::answer`).
8. Narrowing (lesson h): HTTP speaks the stateless 2026-07-28 revision
   only (no session id, no `DELETE`, no `GET` stream), with no proxy and
   no redirect; `basic`, `query` and `path` auth are refused.
9. The fixed environment over HTTP: ureq reads the caller's proxy
   variables by default (`src/config.rs` line 1073); the MCP transport
   sets `proxy(None)`.
10. Line drift: "The `apb connector` CLI" 832 to 875; `validate_templates`
    1255 to 1293; `eval_inbox` 753 to 772; the kind mismatch test 1273 to
    1312; `ConnectorAction` 26 to 27; `print_pins(listing, pinned)` is
    `print_pins(listing)`; the loopback rule names `127.0.0.1` and
    `localhost` (not `::1`).
11. Added: D8, the written procedure of issue 57's host matrix, which
    mcp-tool-connectors moved here and the draft had left out.
12. Closed items: the draft's pending owner decision (client
    registration: pre-registered, as every `oauth2_refresh` connector)
    and its optional section 6 (D7 is now required). No question is left
    for the implementer.
13. A style sentence that named a banned style framework was removed.

### Cut in the closure pass

1. `McpAuth`, `mcp.auth`, `connector/mcp/discovery.rs`,
   `connector/mcp/auth.rs`, `TokenSource::for_mcp`, the `resource` form
   pair and cache key, `plan_mcp`, the `ConsentPlan` fields, `CallbackParams.iss`,
   the issuer line of `apb connector oauth`, and their tests.
2. The reqwest and `auth` rmcp features and the rmcp 3.11.0 API table.
3. The 401 `not_sent` settlement on `tools/call` and its test
   `a_401_on_the_call_settles_not_sent`.
4. `list_live` as the listing path, the dashboard's copy button with the
   approve command, the doctor's approve command, digest columns on the
   listing surfaces.
5. The test `an_mcp_healthcheck_answers_from_the_dashboard` (a duplicate).
6. "Section 6 is separable" and the owner decision on client registration.

### Pre-existing on main, noted and not changed here

1. Every HTTP connector call and the OAuth token request honour the
   calling process's proxy variables (ureq's default, `src/config.rs` line
   1073; neither `http.rs` lines 326 to 334 nor `token.rs` lines 291 to
   296 sets a proxy). Recommended as its own small change. This change
   keeps that behaviour for them (`TokenProxy::Caller`) and gives only an
   MCP connector's token request the no-proxy agent (third pass, item 2).
2. `check_token_url` (`crates/apb-engine/src/connector/oauth/token.rs`
   lines 481 to 503) admits a plaintext token URL on `127.0.0.1` and on
   the name `localhost` in every build, so an HTTP connector's refresh
   token can go to a plaintext loopback listener. An MCP connector's
   token URL is held to `mcp::url_scheme_ok` first (D7 "The https rule");
   the HTTP connectors' rule is recommended for the same small change as
   item 1.
3. The consent of `apb connector oauth` (`consent::plan` checks the
   token URL with `check_token_url`, `crates/apb-engine/src/connector/oauth/consent.rs`
   line 107; `exchange`, line 335, builds its agent without `proxy(None)`)
   admits a plaintext loopback token URL and honours the calling
   process's proxy variables for every connector, an http MCP connector
   included. It runs only at a person's terminal and is refused inside a
   run step, so this change keeps it unchanged (its decision 1: no new
   consent) and records it for the same small change as items 1 and 2
   ("Settled decisions" item 4).

4. Stdio `args` and `env` are rendered, in a run, from the run's
   snapshot (`runs/<id>/connectors/<c>.yaml` and the manifest's account
   fields), which no digest is recomputed over: the program file digest
   is the only stdio fact a call recomputes (`call/target.rs`: "drift
   protection is the snapshot itself"). A process of the person's uid
   that edits the snapshot changes what the approved program is started
   with while the pin passes. Not fixed here (the http endpoint is bound
   by the endpoint digest, "Settled decisions" item 5); recorded as a
   follow-up for the change that freezes a run's snapshot.

### Second pass, 2026-11-20

A design review raised two blockers, one major and two minor findings.
Each was checked against main efae5822 and against mcp-tool-connectors'
final design (its D3 "Mapping" and its D4 "The four orders"). All five
are accepted with the review's fix.

1. Blocker, an approved plaintext loopback URL can reach another server.
   Confirmed: the pin binds the URL and the account digest and nothing
   about the peer, and the rule the draft reused, `check_token_url`
   (`token.rs` lines 481 to 503), admits `http://127.0.0.1` and the name
   `localhost` in every build without looking at the connected address.
   Fixed: an http MCP endpoint and its `oauth2_refresh` token URL are
   `https` only, refused by the manifest rule (the `url` starts with the
   literal `https://`, so no account field chooses the scheme) and again
   at call time by `server::http_target`, through one function,
   `mcp::url_scheme_ok` (D7 "The https rule"). Plaintext
   `http://127.0.0.1:` passes only in apb's own test builds, under the
   existing gate `#[cfg(all(debug_assertions, any(test, feature =
   "testing")))]` (`billing.rs` line 69, refused in release by `lib.rs`
   line 8); the connector HTTP tests already reach their servers that way
   (`common::spawn_http` line 198 and `spawn_script` line 435 bind
   `127.0.0.1:0`); no new feature or seam. The pin covers the URL and the
   account; web PKI authenticates the peer on every call with ureq's
   default certificate verification, and every call opens its own
   connection. The mapping table, D7 "Manifest" rule 4, "The engine" item
   1, the example URLs (`https://{{account.server_host}}/mcp`), the spec
   requirement "An HTTP MCP endpoint is https only" and tasks 7.1, 9.1 and
   10.2 follow. `token.rs` is no longer changed (the draft's shared
   `https_or_loopback` is cut), so the file count is 47. Main's plaintext
   loopback token URL for HTTP connectors is recorded under "Pre-existing
   on main", item 2.
2. Blocker, a write error can settle `not_sent` after the hand-over.
   Confirmed on main: `tool_error` is not in `effect::classify`'s
   `undetermined` (`effect.rs` lines 49 to 51), so it falls to `Failed`
   (line 63) and `send_once::finish` settles `NOT_SENT` (line 479).
   mcp-tool-connectors closes this for stdio with one change, `tool_error`
   joining `undetermined`, and maps every post-hand-over answer of a write
   (`Answered(Rejected)` of any code included) to `tool_error`, `network`
   or `timeout`. The draft did not say that HTTP relies on that change and
   left the path implicit. Fixed: D7 "Settlement of an HTTP write" states
   the one classifier change with its lines, maps every HTTP failure (401,
   403, any other status, a JSON-RPC error, a broken stream, a timeout,
   the caps) to an undetermined outcome through the same `map::answer`,
   keeps `http_status` `None` for MCP, and traces the path from the
   `CallError` through `call/mod.rs` lines 263 to 274, `settle_send`
   (lines 186 to 201), `send_once::finish` (line 461) and `classify` to
   the match at lines 475 to 479 and `ledger::UNKNOWN`. This change adds
   no second classifier. New tests assert the journaled
   `send_intent_settled.outcome`:
   `every_failure_of_an_http_write_after_the_hand_over_settles_unknown`,
   `a_401_on_the_call_settles_unknown` (with the `unknown_effect` repeat)
   and the unit test `no_http_failure_of_a_write_classifies_failed`.
3. Major, endpoint validation after secret resolution. Confirmed: the
   draft's `connect` called `resolve_secrets` before `resolve_endpoint`,
   and `resolve_reference` (`secrets.rs` line 163) runs a `{{cmd:...}}`
   helper. Fixed: `server::http_target` renders and checks the endpoint
   and the token URL from non-secret fields right after `check_pin` and
   before `resolve_secrets` in every order; "The order of one HTTP call"
   is replaced by "The four orders over HTTP", numbered lists that follow
   mcp-tool-connectors' four orders step by step with the one new step
   marked. Tests: `a_refused_endpoint_starts_no_secret_helper` and
   `a_refused_token_url_starts_no_secret_helper` (a marker-writing helper,
   a call and a `check_live`).
4. Minor, the changed-tool test required one `tools/call`. Confirmed (the
   refusal comes before dispatch). Fixed: zero `tools/call` requests after
   the changed listing, in D7 wrong implementation 1, the spec scenario
   and task 9.2.
5. Minor, `listing_view` passed future fields through. Confirmed: the view
   serialized `ToolListing` whole and removed named digests, and its test
   required unknown members to stay. Fixed: `ListingView`, `ServerView`
   and `RowView` are the explicit public shape, built field by field after
   an exhaustive destructuring (no `..`) of `ToolListing`, `ProgramFacts`
   and `ToolRow`, so a new field is a compile error until a change decides
   it; the CLI `--json`, the dashboard route and the web types use the
   shape, and no surface serializes `ToolListing`, `ServerPin` or
   `ToolPin` whole. Test: `listing_view_keeps_out_every_unlisted_field`;
   new spec requirement "Listing surfaces serialize an explicit public
   shape".

What the code contradicted in the draft: its reliance on
`check_token_url`'s loopback rule (plaintext in release builds, and
`localhost` accepted by name), its order (`resolve_secrets` before the
endpoint check, while `resolve_reference` runs helpers), and its silence
on `tool_error`, which `classify` on main does not count as undetermined
(the fix lives only in mcp-tool-connectors). The review's pre-existing
note on the OAuth proxy was already item 1 of "Pre-existing on main".

### Third pass, 2026-11-20

A confirmation review raised one blocker, one major and one minor
finding. Each was checked against main efae5822. All three are accepted
with the review's fix.

1. Blocker, the 401 retry left the replacement token's handling
   unspecified. Confirmed: the bearer lives in the `HttpServer` value, and
   the draft said only "connects once more", so a reconnect could send
   the rejected token A again, and the new token B and a rotated refresh
   token were not said to join the redaction set the reflected-secret walk
   reads. The existing HTTP connector path does both: `refresh_now`
   (`token.rs` line 233) and then, from `http.rs` line 176, the rotation
   warning, `register_redaction` of the access token and of a rotated
   refresh token, and the retry with the new token. Fixed: "The engine"
   item 3.2 orders the retry as refresh, `adopt_minted`, a rebuilt
   `HttpServer` with B, and one reconnect; `adopt_minted` (item 2.2) is
   the existing code of `http.rs` and `call/mod.rs` moved into one
   function that both keep calling, so the MCP path reuses that code path
   rather than copying it. The one retry exists only before the
   `tools/call` hand-over; a 401 on the call of a write stays `tool_error`,
   settled `unknown`, as decided in the second pass. Tests:
   `a_401_at_connect_refreshes_once` now requires `Bearer B` on the second
   discovery request and no A after the refresh, and the new
   `a_listing_that_echoes_the_refreshed_token_is_refused` (B, and in a
   second case the rotated refresh token). D7 wrong implementation 13.
2. Major, the MCP token request used the caller's proxy. Confirmed:
   `refresh_request` builds its agent without `proxy(None)` (`token.rs`
   lines 291 to 296), so with `HTTPS_PROXY` set the connection carrying
   the refresh token reaches the proxy, against this change's rule that
   nothing of the calling process reaches an MCP request. Fixed:
   `TokenSource::render` takes a `proxy: TokenProxy` parameter; the MCP
   `connect` passes `TokenProxy::None`, the two existing callers pass
   `TokenProxy::Caller`, so the HTTP and IMAP connectors keep their
   behaviour, which stays "Pre-existing on main" item 1. Tests:
   `the_token_request_ignores_the_callers_proxy` and the unit test
   `a_no_proxy_source_sets_no_proxy`. D7 wrong implementation 14.
3. Minor, machine-wide MCP approval rows lost project identity.
   Confirmed: `merged_accounts` (`routes/connectors/mod.rs` line 55) keeps
   the first project for each account name, and the machine-wide arm of
   `account_scope` (line 112) resolves through it, so a second project's
   `work` and its approvals were not shown and its server could not be
   listed. Fixed: the `mcp` block's account rows are built per project
   with `workspace` and `root_label` and their own pin comparison, and the
   listing route takes the row's `workspace` (required on a machine-wide
   dashboard) and resolves strictly in that project. Test:
   `two_projects_with_the_same_account_name_are_two_rows`, and
   `McpToolsSection.test.ts` `each account row names its project`. D5
   wrong implementation 9.

What this changes in the record: `oauth/token.rs`, `call/http.rs`,
`call/mod.rs` and `connector/imap.rs` move from "read, not changed" to
"Files" (48, at most 51), and "Unchanged from main" items 2 and 6 say
exactly what changes in them. The review's two pre-existing notes (the
HTTP connectors' token request and the plaintext loopback token URL) were
already items 1 and 2 of "Pre-existing on main".

### Refresh against main f0340685, 2026-11-21

mcp-tool-connectors merged (#293) with four fix rounds, a fifth round,
its pull request review and a rebase on #292; the draft was written
against its seventh pass. This pass rebased the spec onto main f0340685
(no conflict: the branch holds only this directory), read the merged
client crate (`session.rs`, `tap.rs`, `frame.rs`, `stdio.rs`), the engine
kind (`connector/mcp/`), the pin store, the approval in apb-cli, the
validator codes and `docs/CONNECTORS.md`, and the archived design's
"Wrong implementations", "Cut" sections and review record. It changed no
decision silently; the seven places where the code contradicted one
were listed for the owner, written the conservative way, and are now
"Settled decisions". Re-anchored:

1. Base and sequence: main efae5822 to f0340685; mcp-tool-connectors and
   eval-doubles merged; one-point-oh-fixes added to the in-flight list
   (V320); V340 (#292) on main.
2. Symbols that moved or changed shape: `check_live` is the
   healthcheck's private body, so the listing is `list_approved` with
   `check_server` and `CheckedPin::compare_server`; `pin` is already
   `pub mod pin` (the step is gone); `print_rows` is the existing
   `server_lines` with a new `digests` parameter; `ToolListing` has
   `cleanup`, now in `ListingView`; `FailureKind` has `ProgramChanged`,
   `CallFailure` has `NotSent`, `Ended::Reaped` carries `{ killed, group
   }`; the engine's start is `open` and `open_program` with `Start` and
   `OpenError`, and `connect` takes `redactions: Vec<Redaction>`;
   `Connected` holds the endpoint; `SessionHandle::close_noted` and
   `dropped_note` carry the end and the count notes; `dry_run` takes
   `connector, account_name, account_fields`; `resolve_program(command,
   path)` stays stdio and the http approval goes through the new
   `pin::http_program`; `StartPreview` gains `http`; tests pin through
   `PinStore::record` of a hand-built `ServerPin`, not `list_live`.
3. Lines on main, now (draft's line in brackets): `doctor_cmd` 574
   [556]; `guard_refusal` 152 [143]; `settle_send` 252 [186] and the
   outcome pair 356 to 362 [263 to 274]; `send_once::finish` 536 [461] and
   its match 549 to 559 [475 to 479]; the `undetermined` expression 49 to
   52 [49 to 51] and `Failed` 64 [63]; `TokenSource::render`'s caller in
   `call/mod.rs` 1314 [881] and the first mint 1333 to 1338 [899 to 909];
   `call/http.rs` refresh 178 [176], rotation 181 to 187 [182 to 187],
   failure 194 to 206 [190 to 206], `send_raw`'s agent 326 to 333 [326 to
   334]; `validate_templates` 1423 [1293]; `sole_secret` 1800 [1617]; rule
   15 of `from_yaml` 651 to 657 [624], the connector-level rule now
   `validate_mcp_connector`; `resolve_reference` 167 [163]; `refresh_now`
   234 [233]; the contract dispatch 88 [89]; `docs/CONNECTORS.md` "MCP
   servers" 503 [none], "Receiving events" 343 [339], "Contract tests"
   1389 [813], the CLI 1451 [875]; the `ConnectorView.svelte` guard 322 to
   324 [324]. Unchanged and confirmed: `ConnectorAction` 27, `Test` 109,
   `connector_cmd` 135, `approve_cmd` 345, `connectors_list` 22 and its
   read-only row 51, `get_connector_handler` 336,
   `healthcheck_connector_handler` 432, `merged_accounts` 55,
   `account_scope` 91 (arm 98 to 111), `resolve_root` 123,
   `enumerate_workspaces` 181, the healthcheck route 213 to 216,
   `ExpectKind` 263, `json_subset` 728, `eval_inbox` 772, the mismatch
   test 1312, `check_token_url` 481, `render` 125, `access_token` 212,
   `refresh_request` 279 and its agent 291 to 296, `cache_key` 47,
   `imap.rs` 1020, `register_redaction` 97, `auth_header` 495,
   `validate_url` 218, `AuthSpec` 26, `CallErrorCode::Auth` 21,
   `StepCaller::refusal` 37, `ExitCode::from(3)` 169, `classify` 36, the
   testing gate (`billing.rs` 69, `config.rs` 490, `lib.rs` 8),
   `spawn_http` 198, `spawn_script` 435, `sanitize_for_terminal` 85,
   `validate_accounts` 154, `account_digest` 231, `render_raw` 227,
   `RenderCtx` 32, `consent::plan` 51, `fetchConnector` 102,
   `runConnectorHealthcheck` 134, `HOST-INTEGRATION.md` lines 5 and 18,
   ureq 3.5.2 (`config.rs` 1073, `tls/mod.rs` 375, `proxy` 557), and the
   rmcp 3.9.0 table.
4. Lessons of the merged change made explicit requirements here (mapping
   table rows "One strict reader", "The dropped-line rule", "A call counts
   as handed over", "Server text redacted whole"; D7 "The client";
   D7 wrong implementations 15 to 21): one parser decides before anything
   is acted on, for a body and for an event stream; every outcome after
   the request leaves is unknown; secrets redacted by the one function
   before any cut and no server-chosen string in a fault; the pin binding
   names the endpoint and the token URL through the endpoint digest;
   approval only at an interactive terminal (unchanged path, a new http
   test); https only; no proxy from the caller environment for the
   transport and the token request; OAuth reused from the connector
   layer.
5. Files: 49 (at most 52): `digest.rs` added for `endpoint_digest`; the
   `McpServerSpec` literal sites the draft counted do not exist.

### Design review, 2026-11-21

An independent review raised one blocker, four major and eleven minor
findings, checked against main f0340685, rmcp 3.9.0 and ureq 3.5.2. The
owner decided each:

1. Blocker B1 (the endpoint unbound in a run under the alternative of
   decision 5): the endpoint digest stays as written, recomputed from the
   rendered endpoint and token URL on every call before any secret and
   compared in the single pin comparison ("Settled decisions" item 5).
   The same gap for stdio `args` and `env` is "Pre-existing on main"
   item 4, a follow-up for the freeze change; stdio is not changed here.
2. M1 (the http arm of `CheckedPin`): moot with B1 reversed; stated
   anyway in D7 "The engine" item 3.1 (no program file opened for http,
   `Gone` stays stdio's), test `an_http_check_opens_no_program_file`.
3. M2 (rmcp's header annotations and headers): narrowed. A listing with
   `x-mcp-header` annotations fails closed (D7 "The client" item 3.11),
   only `mcp-protocol-version`, `mcp-method` and `mcp-name` pass from
   rmcp (item 3.4), a reserved `auth.header` is V303 (D7 "Manifest" rule
   7); the mapping table row corrected; header annotations are future
   work (proposal, out of scope 11).
4. M3 (event-stream and content-type rules): an event with no or empty
   `data` is skipped with its bytes counted, the field parsing sentences
   and the media type rule (D7 "The client" items 3.8 and 3.9).
5. M4 (the listing route's caller): the route takes only a signed-in
   session, as a publish consent; the merged healthcheck route of an MCP
   connector gets the same rule as a fix (D5 "Dashboard" items 2 and 6).
6. Minors: 1 the seal refuses every POST (item 3.0); 2 whitespace and
   control characters in the rendered URLs (D7 "The engine" item 1.2); 3
   decoded bytes and `key_log` (items 3.6 and "The https rule"); 4
   `Accepted` only for a notification (item 3.7); 5 the stream is read
   until the answer's event is whole, then closed (item 3.9); 6 the
   address sentence ("The https rule"); 7 one-point-oh-fixes in the
   overlap table and codes; 8 the changed-tool refusal test moved to
   `connector_mcp.rs`; 9 `open_http` checks the token URL the source
   rendered; 10 the rotation note in the docs and the listing's output; 11
   the proxy tests run in a child process.

What this changes in the record: V303 is this change's one validator
code; `resolve.rs`, `validate/mcp.rs`, the engine's `connector_mcp.rs`
and `HOWTO-authoring.md` join "Files" (53, at most 56).

### Fix round 1, 2026-11-21

Rebased on main 3248c77f ("One point oh fixes" #294). Two reviews of the
implemented branch: one with one major and two minor findings (C1 to C3
below), one independent pass with seven minor findings (R1 to R7). Each
with its disposition:

1. The rebase. #294 refuses connector approval inside a run step on the
   CLI and over HTTP without a signed-in session, added V320 and
   `project_checks.junit`, a doctor line, and renamed `ClaudeAdapter` to
   `CliAdapter`. One conflict, in
   `connectors_api_test.rs`: the MCP tests' own keyed-server builder now
   sits over #294's `keyed` and `sign_in` helpers instead of a second
   copy. The session rule of the listing route and of the MCP
   healthcheck is #294's helper, `DecisionChannel::person_only_refusal`,
   the one the approval route asks (item 2). `AGENTS.md` and `CLAUDE.md`
   merged without a conflict and stay mirrored with both sides' text.
   V303 stays free: #294 added V320 only. `CliAdapter` is not named
   anywhere in this change. #294's run-step refusal of an approval now
   comes before this change's terminal refusal, so
   `approve_of_an_http_mcp_connector_without_a_terminal_is_refused`
   reads #294's words inside a run step, as its stdio twin does.
2. C1, major (the healthcheck's session check loaded the definition once
   and executed a separately loaded one, so a definition absent at the
   route's read and restored before the engine's would pass a keyless or
   bearer caller): fixed. The caller's `DecisionChannel` goes into the
   engine; `connector::call::healthcheck` loads the connector once and
   selects the branch on that load, the MCP branch's `check_live` asks
   `pin::refuse_caller` (over `person_only_refusal`) on the same loaded
   target before the trust read and any secret, and a load error fails
   closed. `list_approved` takes the same shape (one load through
   `LiveTarget::load_picking`, the refusal on that definition), and so
   does `list_live` for its single load. The routes no longer decide
   themselves; they map `pin::is_caller_refusal` (listing) and
   the healthcheck's caller refusal (typed since fix round 3) to 403 `{ "error":
   "mcp_needs_a_person", "message" }`. Test, seam-free:
   `connector_mcp.rs::the_healthcheck_and_the_listing_refuse_a_caller_that_is_not_a_person_before_any_secret`
   calls the engine entries with `DashboardKeyless`, `HttpBearer`, `Mcp`
   and `RunStep` over an approved, pinned MCP definition whose token
   comes from a `{{cmd:...}}` helper: each refused `mcp_needs_a_person`,
   the helper never run, nothing started; `DashboardSession` passes.
   `connectors_api_test.rs::the_mcp_tools_route_refuses_a_keyless_dashboard_and_a_bearer`
   now also covers the healthcheck route. Bite: with the two
   `refuse_caller` calls removed, both tests fail.
3. C2 and R1 (the rotation warning dropped on the healthcheck and on the
   approval listing): fixed. `check_live` puts `Opened.warnings` in the
   healthcheck's `warnings` with the close note, and `list_live` joins
   them into the listing's note through `listing_note`, the one helper
   `list_approved` uses too. Tests:
   `connector_mcp_http.rs::a_healthcheck_of_a_rotating_oauth_account_carries_the_rotation_warning`
   and
   `an_approval_listing_of_a_rotating_oauth_account_carries_the_rotation_warning`
   (neither token in the text). Bite: with the warnings dropped on both
   paths, both fail.
4. R2 (a 2xx JSON body handed to rmcp without being checked as the
   answer): fixed. The JSON arm runs the strict reader, then the same
   `answers` check as the other two arms, and a body that is not the
   answer is `mcp_message_invalid:` with nothing handed on (D7 "The
   client" item 3.8). No other path hands rmcp a server request: an
   event stream drops every frame but the answer and the `GET` stream is
   refused, so over HTTP a server request is never answered. Test:
   `http_test.rs::a_json_body_that_is_not_the_answer_is_never_handed_on`
   (a `ping` request and another id's response; the call is lost
   `mcp_message_invalid` at once, and after the session closed and the
   server stopped the methods are `server/discover` and `tools/call`
   only). Bite: without the check, the call waits to its deadline.
5. R3 (the proxy unit test could pass with its child running no test):
   fixed. The child appends one line to a marker file after its
   assertions and the parent asserts the file holds exactly that line.
   Bite: with the child's test path changed, the parent fails "the child
   ran exactly this test, once".
6. C3 and R6 (proofs of absence by sleeping, and a refused-connection
   test racing the accept thread): fixed. The scripted server's
   `stop()` sets the flag, joins the accept thread (which takes every
   connection already queued before it ends, dropping the listener) and
   waits until every accepted connection's request is read and
   recorded. `nothing_is_posted_after_the_seal` and
   `a_call_after_the_seal_is_not_sent_and_never_posted` close the
   session and stop the server before asserting;
   `a_refused_connection_after_the_call_started_is_lost` no longer
   sleeps. Bite: with the seal check before a POST removed, the first
   fails; with the seal checks of a POST and of `start_call` removed, the
   second fails.
7. R4, R5 and R7 (the web section): fixed (D5 "Dashboard" items 4 and
   5). The section moved to `web/src/lib/components/` and uses the
   project's `ui/button` and `ui/table`; each row's button is named
   "List tools of <account>[ in <project>]"; each row's result is a
   polite live region, busy while listing; a 403 is a refused state with
   the fixed words "Sign in to the dashboard to list tools, or run `apb
   connector tools` at a terminal.", and a page that knows it has no key
   offers no button and shows those words. Tests:
   `McpToolsSection.svelte.test.ts` (the labels and regions, busy while
   listing, the 403 state without the server text, another failure's own
   message, no button without keys, the button for a signed-in session)
   and `mcpTools.test.ts`. Bite: with the labels, the live region, the
   keyless condition and the 403 branch removed, five cases fail.

The spec deltas state the engine's decision on the executed definition,
the 403 answer, the refused state and the JSON arm's answer rule;
`docs/CONNECTORS.md` names the refused state and the healthcheck's
rotation warning.

### Fix round 2, 2026-11-21

A second code pass over the fixed branch: one major and two minor
findings (K1 to K3). Each with its disposition:

1. K1, major (a cached access token lost the redaction of the refresh
   token its mint rotated: `TokenSource::access_token` answered a cache
   hit with `new_refresh: None` and `rotated: false`, so a second
   listing in the same dashboard process showed the rotated refresh
   token unredacted and without the reflected-secret refusal, and a
   later call answer carried it the same way): fixed in the one place
   every user of the cache passes, `connector/oauth/token.rs`. The cache
   entry `Cached` keeps `rotated_refresh`, a sealed `Secret` (never
   serialized, logged or sent), and a hit answers the same `Minted` a
   fresh mint gave (`rotated` and `new_refresh` from the entry), so
   `adopt_minted` (the HTTP call, its 401 refresh and the MCP open) and
   the IMAP build register it for redaction and push the rotation
   warning on every use for as long as the entry lives; a 401 refresh
   that does not rotate keeps the entry's earlier rotation. Main had
   the same gap: on main 3248c77f the cache hit answered `new_refresh:
   None` for the HTTP and the IMAP connectors as well, so a second call
   of a rotating Google account in one process lost the redaction and
   the warning; the fix covers them by the same rule. Tests:
   `connector_oauth.rs::a_cached_token_keeps_the_rotated_refresh_redacted_and_warned`
   (the non-MCP HTTP path: two calls, one token request, an answer that
   echoes the rotated token redacted and warned both times),
   `connector_mcp_http.rs::a_cached_token_refuses_a_listing_that_reflects_the_rotated_refresh`
   (two `list_approved` in one process, one token request, both refused
   `mcp_tools_reflect_a_secret` with the warning) and
   `a_cached_token_redacts_the_rotated_refresh_in_a_call_answer` (two
   calls, the answer redacted and warned both times, the journal
   clean). Bite: with the hit answering `rotated: false` and
   `new_refresh: None` again, all three fail.
2. K2, minor (the rotation warning dropped on the failure paths after a
   mint): fixed structurally. `CallError` carries `warnings` (shown as
   the result's top-level `warnings`, as a success shows them, merged
   once with the call's own; never journaled), added through
   `CallError::carrying`. `open_http` owns the open's warnings and runs
   its steps (`open_http_steps`) through one wrapper that attaches them
   to every answer after the mint, success or error; the post-open
   refusals pass through `Opened::fail` (a verification refusal, a
   changed pinned tool) or `Opened::carrying` (the reflected-secret
   walk and the rest of `list_live` and `list_approved`), both
   attaching the warnings. The non-MCP HTTP build's scope check after
   the mint carries them too. The surfaces print them: the call and the
   healthcheck JSON through `to_json`, the listing route through its
   error body, `apb connector tools` and the approval after the error
   line. Tests:
   `connector_mcp_http.rs::a_failed_discovery_after_a_rotating_mint_carries_the_warning`,
   `a_changed_pinned_tool_after_a_rotating_mint_carries_the_warning` and
   `a_reflected_secret_after_a_rotating_mint_carries_the_warning` (no
   `tools/call`, neither token in the result or the journal), and the
   listing test of item 1. Bite: with the wrapper, `Opened::fail` and
   `Opened::carrying` not attaching, all four fail.
3. K3, minor (a body that stalls after its headers reported as a
   network failure: ureq hands its own timeout out of a body read
   wrapped in an io error of kind `Other`, and both readers recognized
   only `TimedOut`): fixed. One mapper, `http.rs` `body_fault`, used by
   the JSON and the event-stream reader, answers `HttpFault::Timeout`
   for an io `TimedOut` and for a wrapped `ureq::Error::Timeout` or
   `ureq::Error::Io(TimedOut)`; settlement stays `unknown` (the client's
   `Lost` call). Tests: `http::tests::a_stalled_body_is_a_timeout_for_both_readers`
   (the client's own exchange against a server that sends the head and
   holds the body: `Timeout` at the configured 500 ms deadline for
   both content types, deterministic, no outer timer),
   `the_body_fault_recognizes_every_timeout_shape`, and
   `http_test.rs::a_stalled_body_of_a_listing_is_a_timeout_for_both_readers`
   and `a_stalled_body_of_a_call_is_a_lost_timeout_for_both_readers`
   (the outcome class through the session). Bite: with the old
   `TimedOut`-only mapping, the two unit tests fail; the session tests
   race the outer timer and are not the bite.

The spec delta of mcp-http-transport states the cached token's rotation,
the warning on a refusal and the stalled body; `docs/CONNECTORS.md`
says the warning comes again on a reuse and after an error.

### Fix round 3, 2026-11-21

A third adversarial pass over the branch after fix round 2 found no
blocker and no major, and eight minor findings (M1 to M8). All eight are
fixed:

1. M1 (the rotation warning shown twice after a 401 refresh, which
   inherits the first mint's rotation, in the listing's `cleanup` and
   the healthcheck's `warnings`): `adopt_minted` pushes
   `ROTATION_WARNING` only when the call's warnings do not hold it yet.
   Test: `token::tests::adopting_two_rotated_mints_warns_once`.
2. M2 (the healthcheck route decided its 403 by the message text of any
   connector's JSON): `call::healthcheck_outcome` answers
   `HealthcheckOutcome::Answer(value, ok)` or
   `HealthcheckOutcome::CallerRefused(error)`, the latter only from
   `pin::refuse_caller` on the MCP branch; the route answers 403 on that
   variant alone, and `pin::caller_refusal_in` is gone.
   `call::healthcheck` keeps its `(value, ok)` shape for its other
   callers. No black-box test is possible: no connector of another kind
   produces a permission error with that prefix, which is the point of
   deciding on the type; the route tests of the refusal
   (`the_mcp_healthcheck_route_refuses_a_keyless_dashboard_and_a_bearer`
   and its siblings) pass through the typed path.
3. M3 (expired token cache entries kept for the life of the process,
   each with a sealed rotated refresh token): every insert drops the
   other entries whose access token has expired (`token::store`), and a
   refresh the endpoint refuses `invalid_grant` drops its entry
   (`TokenError.grant_refused`). Tests:
   `token::tests::an_insert_drops_expired_entries` and
   `an_invalid_grant_refresh_drops_the_entry`.
4. M4 (the IMAP `forget` dropped the carried rotation): `forget` ends
   the entry's access token (`expires_at` now) instead of removing it,
   so a later refresh of the same stale token that does not rotate
   still registers the earlier rotated token and warns, until an insert
   of another entry drops it (M3). Test:
   `token::tests::forget_keeps_the_carried_rotation`.
5. M5 (a `tools/call` that never left counted as handed over): `post`
   checks the deadline and builds the request in memory before
   `start_call`, so a passed deadline (`Timeout`) and a request that
   does not build (`REQUEST_UNBUILT`) fail with the count unmoved and
   the call settles `not_sent`; building a request does no I/O, so no
   byte can leave before the count. The deadline is checked again inside
   `exchange` after the hand-over, where it stays `unknown`. Test:
   `http::tests::a_call_that_never_left_is_not_handed_over`.
6. M6 (`last_fault` kept across messages): every `post` clears it
   first. Test: `http::tests::a_post_clears_the_fault_of_an_earlier_one`.
7. M7 (`mcp-session-id` not reserved): `RESERVED_HTTP_HEADERS`, the one
   list V303 and the client share, adds `mcp-session-id` and
   `last-event-id`. Test: `reserved_http_headers_are_case_insensitive`
   names both.
8. M8 (a test slept 50 ms for the scripted listener to close): the
   scripted server's `Reply::ThenClose` closes the listener before it
   gives the listing, so the call's connection is refused with no
   timing; `a_connection_refused_on_the_call_settles_unknown` uses it.

The spec delta of mcp-http-transport names the two added reserved
headers; `docs/CONNECTORS.md` and the V303 row of
`docs/HOWTO-authoring.md` list them.

### Pull request review, 2026-11-21

The bot review of the pull request: eleven review threads and fourteen
code-scanning alerts. Each with its disposition:

1. The tree confirmation took an http url's last path segment for a
   launcher (`https://tracker.example/node` warned of a `node` file):
   fixed, `print_tree` asks `launcher_name` of a stdio `command` only.
   Test: `connector_mcp::tests::the_tree_warns_of_a_launcher_command_only`.
2. `apb connector doctor` skipped the pin store when an MCP connector's
   accounts did not load: fixed, the transport row and the pin store are
   read right after the manifest, before the accounts. Test:
   `connector_mcp_cli_test.rs::doctor_reads_the_pins_when_the_accounts_do_not_load`.
3. The doctor did not name an MCP connector's transport, which the
   listing requirement asks: fixed, one `mcp transport` row per MCP
   connector. Test: `doctor_starts_no_server` asserts it.
4. The healthcheck of an MCP connector without a `healthcheck` answered
   its config error to a caller that is not a person: fixed, the caller
   is decided on the loaded definition before its healthcheck function
   and its accounts are read. Test:
   `connector_mcp.rs::the_caller_of_an_mcp_connector_without_a_healthcheck_is_refused_first`.
5. A rendered endpoint or token URL with an empty host (`https:///mcp`)
   passed `http_target`: fixed, both must name a host, before any
   secret. Test: `server::tests::http_target_refuses_an_empty_host`.
6. A token URL with a query: not changed. This design does not require
   a query-free token URL; the endpoint digest pins the token URL whole,
   the refresh token goes in the form body and never in the URL, and the
   HTTP and IMAP connectors' token URLs take a query alike.
7. `connectors_list` and the connector detail returned the pin store
   reader's diagnostic, which can quote a stored value and names the
   local path: fixed, both say the fixed words `PIN_STORE_UNREADABLE`;
   `apb connector doctor` at a terminal keeps the diagnostic. Tests:
   `read_tools_test.rs::connectors_list_reports_a_pin_store_error` and
   `connectors_api_test.rs::the_connector_detail_carries_the_mcp_block`.
8. The host matrix procedure could not measure a cut length with one
   payload size: fixed in `docs/HOST-INTEGRATION.md` and D8, which now
   record whether the 1948-byte text arrives whole, one size tested, not
   a limit.
9. The public record lists the change's records by role: not changed.
   It names no working file path and follows the merged records' form;
   the file count of "Files" counts them.
10. A failed dashboard listing dropped the rotation warning: fixed, the
    failure keeps `warnings` (`toMcpToolsResult`) and the section shows
    them under the error. Tests: `api/connectors.test.ts` and
    `McpToolsSection.svelte.test.ts`.
11. Code scanning, cleartext logging, all in test code: the assertion
    messages that printed a fixture token now name its position; the
    others print account names, refusal texts and test definitions
    only, and are dismissed as false positives as on the merged changes.

### Wrong implementations

Each item names the test or check that catches it; the per-decision lists
above hold the rest.

1. Listing tools for an account nobody approved, or through `list_live`
   without the program confirmation.
   `connector_mcp_cli_test.rs::tools_of_an_unapproved_account_starts_nothing`.
2. A digest or the approve command on a listing surface.
   `tools_json_carries_no_digest`, `tools_lists_each_row_and_exits_3_when_changed`,
   `the_connector_detail_carries_the_mcp_block`, `McpToolsSection.test.ts`.
3. A route or tool that writes a pin. `the_mcp_tools_route_writes_no_pin`.
4. Server text through `{@html}`. `a description is rendered as text`.
5. A doctor or `connectors_list` that starts a server.
   `doctor_starts_no_server`,
   `connectors_list_names_mcp_tools_without_starting_a_server`.
6. An HTTP tool digest over rmcp's typed `Tool`.
   `a_changed_execution_member_refuses_the_next_http_call`.
7. A post-hand-over failure of a write settled `not_sent`.
   `every_failure_of_an_http_write_after_the_hand_over_settles_unknown`,
   `a_401_on_the_call_settles_unknown`.
8. A token sent before the binding check or to an endpoint the approval
   does not name. `a_changed_account_url_sends_nothing`.
9. The caller's proxy, a redirect, a plaintext endpoint (the loopback
   included), or a query in the endpoint. `the_callers_proxy_is_ignored`,
   `a_redirect_is_not_followed`, `a_plaintext_url_is_refused`,
   `http_target_refuses_plaintext_and_a_query`,
   `a_url_with_a_query_is_refused`.
10. A second OAuth implementation: discovery, a new consent, an `mcp.auth`
    block. The final diff review: `oauth/consent.rs`,
    `crates/apb-core/src/connector/oauth.rs` and `connector_oauth.rs`
    unchanged, no `discovery.rs` and no `auth.rs` under `connector/mcp/`.
11. A token, a refresh token or a reflected access token stored, printed
    or journaled. `the_token_never_reaches_the_journal_or_the_result`,
    `a_listing_that_reflects_the_access_token_is_refused`.
12. A legacy HTTP session (session id, `DELETE`, `GET` stream).
    `http_test.rs::a_session_id_from_the_server_is_refused` and
    `mcp::tests::http_needs_protocol_modern`.
13. An HTTP close worded as a stop. `close_of_an_http_session_is_not_started`.
14. A new event, field, run guard or manifest mark, or a validator code
    other than V303. The final diff review: `event.rs`, `manifest.rs` and
    `guarded.rs` unchanged, `validate/` changed only in `validate/mcp.rs`
    (V303) and its dispatch comment; tasks.md 0.5 and 10.2.
15. Running a host CLI for D8. The final report lists no such run;
    `docs/HOST-INTEGRATION.md`'s table is empty.
16. A reconnect after a 401 with the old bearer, or before the refreshed
    tokens are registered. `a_401_at_connect_refreshes_once`,
    `a_listing_that_echoes_the_refreshed_token_is_refused`.
17. An MCP token request through the caller's proxy.
    `the_token_request_ignores_the_callers_proxy`,
    `a_no_proxy_source_sets_no_proxy`.
18. Machine-wide MCP account rows that drop a project's account of the
    same name, or a live listing in a project the row does not name.
    `two_projects_with_the_same_account_name_are_two_rows`.
19. A second reader, a second capture or a second hand-over count for
    HTTP: an answer handed to rmcp before the strict reader accepted it,
    a frame of an event stream handed on before the whole stream was
    framed, a POST counted after its connection opened, or an HTTP body
    dropped like a stdio log line. D7 wrong implementations 15 to 17 and
    21.
20. Server-chosen text in a fault. `no_fault_echoes_the_server`.
21. An http pin that does not bind the endpoint and the token URL.
    `an_endpoint_digest_that_differs_refuses_the_call`,
    `the_endpoint_digest_covers_the_token_url`.
22. An http MCP connector approved without a terminal.
    `approve_of_an_http_mcp_connector_without_a_terminal_is_refused`.
23. A tool's `x-mcp-header` annotation reaching rmcp over HTTP, an
    argument value promoted into a header, or a header name rmcp hands
    over sent unchecked.
    `a_tool_with_a_header_annotation_fails_the_http_listing_closed`,
    `an_unknown_rmcp_header_is_refused`.
24. An MCP http `auth.header` naming a header the transport sets.
    `a_reserved_auth_header_is_v303`, `a_reserved_server_header_is_refused`.
25. The empty priming event refused, a content type matched by
    substring or with its parameters, a stream read past its answer, or
    a 202 answering a request. `a_priming_event_without_data_is_skipped`,
    `a_media_type_with_parameters_is_read`,
    `the_stream_is_closed_after_the_answer`,
    `an_accepted_request_is_a_transport_fault`.
26. A cache hit that forgets the refresh token its mint rotated.
    `a_cached_token_keeps_the_rotated_refresh_redacted_and_warned`,
    `a_cached_token_refuses_a_listing_that_reflects_the_rotated_refresh`.
27. A refusal after a rotating mint without the rotation warning.
    `a_reflected_secret_after_a_rotating_mint_carries_the_warning` and
    its two siblings.
28. A stalled body reported as a network failure.
    `a_stalled_body_is_a_timeout_for_both_readers`.
26. A POST after the seal, a control message included.
    `nothing_is_posted_after_the_seal`.
27. The MCP listing or healthcheck route answering a keyless dashboard
    or a bearer key, or the rule decided on one load of the definition
    and another load executed.
    `the_healthcheck_and_the_listing_refuse_a_caller_that_is_not_a_person_before_any_secret`,
    `the_mcp_tools_route_refuses_a_keyless_dashboard_and_a_bearer`,
    `the_mcp_healthcheck_route_refuses_a_keyless_dashboard_and_a_bearer`.
28. An http pin check that opens the endpoint as a program file, or a
    missing stdio file no longer `Gone`.
    `an_http_check_opens_no_program_file`.
29. A rendered endpoint or token URL with whitespace or a control
    character reached. `http_target_refuses_whitespace_in_a_rendered_url`.
30. A 2xx JSON body handed to rmcp that does not answer the request, or a
    server request over HTTP answered.
    `a_json_body_that_is_not_the_answer_is_never_handed_on`.
31. A rotation warning dropped by the healthcheck or the approval
    listing. `a_healthcheck_of_a_rotating_oauth_account_carries_the_rotation_warning`,
    `an_approval_listing_of_a_rotating_oauth_account_carries_the_rotation_warning`.
32. The dashboard's listing button offered without keys, a 403 shown as
    the server's text, or rows a screen reader cannot tell apart.
    `McpToolsSection.svelte.test.ts`.
