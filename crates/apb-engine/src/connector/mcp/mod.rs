//! The MCP tool kind of a connector call (design D4 "The engine side"):
//! the pin check that needs no secret and starts nothing (`check_pin`),
//! the start of the pinned server with its listing and tool check
//! (`connect`), and the one `tools/call` (`McpCall::send`). `server` builds
//! the server's fixed environment, `map` turns answers and failures into
//! call results, and `pin` holds a person's approval listing and the
//! healthcheck of the pins.
//!
//! A call never caches a listing: every call starts its own server, lists
//! its tools and compares the mapped tool with its pin before anything is
//! sent.

pub(crate) mod map;
pub mod pin;
pub(crate) mod redact;
pub(crate) mod server;

pub(crate) use redact::redact_tree;

pub use server::{HttpPreview, StartPreview, preview_start};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apb_core::config::AccountEntry;
use apb_core::connector::def::{AuthSpec, ConnectorDoc, FunctionSpec};
use apb_core::connector::mcp::pins::{BindingDiff, LiveBinding, PinStore, ProgramFact, ServerPin};
use apb_core::connector::mcp::{McpProtocol, McpServerSpec, McpTransportKind};
use apb_core::connector::template::{RenderCtx, render_raw};
use apb_core::secret::{Redaction, Secret};
use apb_core::step_env::StepCaller;
use apb_mcp_client::{
    CallFailure, Ended, EnvValue, Era, Failure, FailureKind, GroupEnd, Lifecycle, ProgramFile,
    StartFrom, ToolAnswer,
};
use serde_json::{Map, Value, json};

use crate::connector::intent::stamp::{Stamp, prepare_body_args};
use crate::connector::oauth::{TokenProxy, TokenSource, adopt_minted};
use crate::connector::result::{CallError, CallErrorCode, Dispatched, redact_message};
use server::HttpTarget;

/// The client name apb gives every server it starts.
const CLIENT_NAME: &str = "apb";

/// The fixed bound of an approval's listing: a function's `timeout_sec`
/// bounds calls and the healthcheck only.
pub const MCP_PIN_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// The most `tools/list` pages a listing reads.
pub const MCP_LIST_MAX_PAGES: u32 = 20;

/// The most tools a listing holds.
pub const MCP_LIST_MAX_TOOLS: usize = 500;

/// The most raw `tools/list` result bytes a listing reads, over all pages.
pub const MCP_LIST_MAX_BYTES: usize = 2 * 1024 * 1024;

/// The program file at `path`, opened once without following a link and
/// hashed from that descriptor (`apb_mcp_client::ProgramFile`), started
/// the way this platform starts it; `None` when the file is missing, a
/// link or not a regular file. The one way the engine reads a program
/// file: a call, the healthcheck, an approval's listing and its
/// resolution.
pub(crate) fn program_file(path: &Path) -> Option<ProgramFile> {
    ProgramFile::open(path, StartFrom::platform()).ok()
}

/// Whether a raw tool definition holds any of the account's resolved
/// secrets, in any member name or string value at any depth, or in the
/// JSON text of a number (design D2 "Reflected secrets").
pub(crate) fn reflects_secret(value: &Value, redactions: &[Redaction]) -> bool {
    redact_tree(value.clone(), redactions).1
}

/// The refusal of a listing that reflects a secret: it names no tool and
/// no member.
pub(crate) fn reflected_secret(connector: &str, account: &str) -> CallError {
    CallError::new(
        CallErrorCode::Permission,
        format!(
            "mcp_tools_reflect_a_secret: the server of `{connector}` lists for account `{account}` a tool definition that holds a secret of the account; nothing was hashed, approved or pinned"
        ),
    )
}

/// What the call path hands the mcp kind before the send intent exists.
/// It holds no secret: the binding is checked before any is resolved.
pub(crate) struct McpConnectInput<'a> {
    pub doc: &'a ConnectorDoc,
    /// The tree digest the call runs under, compared with the pin's
    /// binding: the run manifest's `ManifestConnector.digest` in a run, the
    /// installed connector's digest in the healthcheck.
    pub connector_digest: &'a str,
    /// The account digest the call runs under: the granted account's
    /// `ManifestAccount.digest` in a run, `config::account_digest` of the
    /// merged account in the healthcheck.
    pub account_digest: &'a str,
    pub function: &'a FunctionSpec,
    pub connector: &'a str,
    pub account_name: &'a str,
    pub account_fields: &'a BTreeMap<String, String>,
    pub full: bool,
    pub deadline: Instant,
}

/// Refuses arguments that are not a JSON object (a `tools/call` takes its
/// `arguments` as an object, whether or not the function declares an
/// `args_schema`), or that are nested deeper or longer than the client
/// takes (`apb_mcp_client::check_arguments`), before anything resolves
/// or starts: a call the edge admits always parses in the line tap.
pub(crate) fn require_call_arguments(
    connector: &str,
    function: &str,
    args: &Value,
) -> Result<(), CallError> {
    let kind = match args {
        Value::Object(map) => {
            return apb_mcp_client::check_arguments(map).map_err(|refusal| {
                CallError::new(
                    CallErrorCode::InvalidArgs,
                    format!("{} (`{connector}.{function}`)", refusal.message()),
                )
            });
        }
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
    };
    Err(CallError::new(
        CallErrorCode::InvalidArgs,
        format!("mcp: the arguments of `{connector}.{function}` must be a JSON object, not {kind}"),
    ))
}

/// The refusal of a live mcp call outside a run (the playground, a pinned
/// notice): nothing is resolved and nothing starts.
pub(crate) fn live_call_unsupported(connector: &str, function: &str) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!(
            "mcp_live_call_unsupported: `{connector}.{function}` calls an MCP tool, which this apb calls only from a run; outside a run the healthcheck checks the pins without a call"
        ),
    )
}

/// The dry-run answer: the pinned program (or the `command` when no record
/// exists), the rendered args, the env names (sorted), the tool and the
/// prepared arguments. Starts nothing and resolves no secret.
pub(crate) fn dry_run(
    doc: &ConnectorDoc,
    function: &FunctionSpec,
    connector: &str,
    account_name: &str,
    account_fields: &BTreeMap<String, String>,
    args: &Value,
    stamp: Option<&Stamp<'_>>,
) -> Result<Value, CallError> {
    require_call_arguments(connector, &function.name, args)?;
    let spec = spec_of(doc)?;
    let tool = tool_of(function)?;
    if spec.transport == McpTransportKind::Http {
        // The endpoint a call would reach, checked as a call checks it; no
        // secret, no token, no request.
        let target = server::http_target(doc, account_fields)?;
        let auth = match doc.auth.as_ref() {
            None => "none",
            Some(AuthSpec::Header { .. }) => "header",
            Some(AuthSpec::Oauth2Refresh { .. }) => "oauth2_refresh",
            Some(AuthSpec::Query { .. }) => "query",
            Some(AuthSpec::Basic { .. }) => "basic",
            Some(AuthSpec::Path { .. }) => "path",
        };
        let arguments = prepare_body_args(function, args, stamp).into_owned();
        return Ok(json!({
            "ok": true,
            "dry_run": true,
            "connector": connector,
            "account": account_name,
            "transport": "http",
            "url": target.endpoint,
            "auth": auth,
            "tool": tool,
            "arguments": arguments,
        }));
    }
    // Display only: the program a call would start, as recorded.
    let program = PinStore::load()
        .ok()
        .and_then(|s| s.server(connector, account_name).map(|p| p.program.clone()))
        .or_else(|| spec.command.clone());
    let rendered = server::render_args(spec, account_fields)?;
    let env: Vec<&String> = spec.env.keys().collect();
    let arguments = prepare_body_args(function, args, stamp).into_owned();
    Ok(json!({
        "ok": true,
        "dry_run": true,
        "connector": connector,
        "account": account_name,
        "transport": spec.transport.as_str(),
        "program": program,
        "args": rendered,
        "env": env,
        "tool": tool,
        "arguments": arguments,
    }))
}

/// The checks that need no secret and start nothing, in this order: the
/// config dir rule, `PinStore::load`, the record, `ServerPin::compare` of
/// the tree digest, the account digest and the program file (path and
/// streamed digest), and the presence of the tool's pin. Every `Err` comes
/// before `resolve_secrets`, so no `{{cmd:...}}` helper of the account runs
/// for a call the pins refuse.
pub(crate) fn check_pin(input: &McpConnectInput<'_>) -> Result<CheckedPin, CallError> {
    let tool = tool_of(input.function)?;
    check_tools(input, vec![(input.function.name.clone(), tool.to_string())])
}

/// [`check_pin`] for every `(function, tool)` of `tools`: the healthcheck
/// checks every mcp function of the connector. [`check_server`] followed
/// by the presence of each tool's pin.
pub(crate) fn check_tools(
    input: &McpConnectInput<'_>,
    tools: Vec<(String, String)>,
) -> Result<CheckedPin, CallError> {
    let checked = check_server(input, tools)?;
    for (_, tool) in &checked.tools {
        checked.compare(tool, None)?;
    }
    Ok(checked)
}

/// The server-level part of [`check_tools`], for the listing surfaces
/// (design D5 "`list_approved`"): the config dir rule, `PinStore::load`,
/// the record, and `ServerPin::compare` of the tree digest, the account
/// digest and the program file, with no tool required to be pinned. Every
/// `Err` comes before any secret is resolved or anything starts.
pub(crate) fn check_server(
    input: &McpConnectInput<'_>,
    tools: Vec<(String, String)>,
) -> Result<CheckedPin, CallError> {
    let spec = spec_of(input.doc)?.clone();
    config_dir_rule()?;
    let store =
        PinStore::load().map_err(|e| CallError::new(CallErrorCode::Config, e.to_string()))?;
    let record = store
        .server(input.connector, input.account_name)
        .cloned()
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Permission,
                format!(
                    "mcp_server_unpinned: the MCP server of `{}` is not approved for account `{}`; a person approves it at a terminal (docs/CONNECTORS.md, MCP servers)",
                    input.connector, input.account_name
                ),
            )
        })?;
    // The transport is the installed definition's, never the record's.
    // Over http no program file is opened (the record's `program` holds
    // the endpoint): the endpoint and the token URL are rendered and
    // checked before the comparison and before any secret, and their
    // endpoint digest is the program fact. Over stdio a missing file is
    // still `ProgramFact::Gone`.
    let (program, program_digest, http) = match spec.transport {
        McpTransportKind::Http => {
            let target = server::http_target(input.doc, input.account_fields)?;
            (None, Some(target.digest.clone()), Some(target))
        }
        McpTransportKind::Stdio => {
            let program = program_file(Path::new(&record.program));
            let digest = program.as_ref().map(|p| p.digest().to_string());
            (program, digest, None)
        }
    };
    let checked = CheckedPin {
        connector: input.connector.to_string(),
        account_name: input.account_name.to_string(),
        connector_digest: input.connector_digest.to_string(),
        account_digest: input.account_digest.to_string(),
        program_digest,
        program,
        http,
        auth: input.doc.auth.clone(),
        record,
        spec,
        tools,
        account_fields: input.account_fields.clone(),
        read_only: input.function.read_only,
        response_pick: if input.full {
            Vec::new()
        } else {
            input.function.response_pick.clone()
        },
        deadline: input.deadline,
        home: None,
    };
    checked.compare_server()?;
    let home = apb_core::config::account_entry().ok_or_else(no_home)?;
    Ok(CheckedPin {
        home: Some(home),
        ..checked
    })
}

/// A run step reads pins only from the default config dir: any redirect
/// could admit a store the step wrote itself.
fn config_dir_rule() -> Result<(), CallError> {
    if StepCaller::from_env().describe().is_none() || !apb_core::config::config_dir_redirected() {
        return Ok(());
    }
    let shown = |p: Option<PathBuf>| {
        p.map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".to_string())
    };
    Err(CallError::new(
        CallErrorCode::Permission,
        format!(
            "mcp_config_dir_redirected: the config dir is redirected to `{}` away from `{}`; a run step reads MCP pins only from the default config dir",
            shown(apb_core::config::config_dir()),
            shown(apb_core::config::default_config_dir()),
        ),
    ))
}

/// The record and the facts `check_pin` held, handed to `connect`.
pub(crate) struct CheckedPin {
    connector: String,
    account_name: String,
    connector_digest: String,
    account_digest: String,
    /// The program file's digest as `check_pin` read it from `program`'s
    /// descriptor; `None` when it was gone.
    program_digest: Option<String>,
    /// The program file `check_pin` hashed, its descriptor kept open
    /// across the secret resolution until the start takes it. Always
    /// `None` for an http server.
    program: Option<ProgramFile>,
    /// The checked target of an http server; `None` for stdio.
    http: Option<HttpTarget>,
    /// The connector-level authorization (an http server's).
    auth: Option<AuthSpec>,
    record: ServerPin,
    spec: McpServerSpec,
    /// `(function, tool)` pairs the check covers.
    tools: Vec<(String, String)>,
    account_fields: BTreeMap<String, String>,
    read_only: bool,
    response_pick: Vec<String>,
    deadline: Instant,
    home: Option<AccountEntry>,
}

impl CheckedPin {
    /// The one comparison of the binding, with the program fact held.
    fn compare(&self, tool: &str, tool_digest: Option<&str>) -> Result<(), CallError> {
        self.binding_diff(tool, tool_digest)
            .map_err(|diff| self.refusal(&diff, tool))
    }

    /// `ServerPin::compare` of the record with the facts held.
    fn binding_diff(&self, tool: &str, tool_digest: Option<&str>) -> Result<(), BindingDiff> {
        let program = match &self.program_digest {
            Some(d) => ProgramFact::Digest(d),
            None => ProgramFact::Gone,
        };
        self.record.compare(&LiveBinding {
            connector_digest: &self.connector_digest,
            account_digest: &self.account_digest,
            program,
            tool,
            tool_digest,
        })
    }

    /// The comparison of the server alone: `ServerPin::compare` with an
    /// empty tool name, `ToolUnpinned` read as passing. In `compare` the
    /// tree, account and program arms come before the tool arm, so every
    /// server-level difference still refuses.
    fn compare_server(&self) -> Result<(), CallError> {
        match self.binding_diff("", None) {
            Ok(()) | Err(BindingDiff::ToolUnpinned) => Ok(()),
            Err(diff) => Err(self.refusal(&diff, "")),
        }
    }

    /// The refusal of one difference: it names what differs and carries
    /// no digest.
    fn refusal(&self, diff: &BindingDiff, tool: &str) -> CallError {
        let (c, a) = (&self.connector, &self.account_name);
        let why = match diff {
            BindingDiff::Tree { .. } => "the connector definition changed",
            BindingDiff::Account { .. } => "the account changed",
            BindingDiff::ProgramGone => "the program file is gone, a link or not a regular file",
            BindingDiff::Program { .. } if self.http.is_some() => "the server endpoint changed",
            BindingDiff::Program { .. } => "the program file changed",
            BindingDiff::ToolUnpinned => {
                return CallError::new(
                    CallErrorCode::Permission,
                    format!(
                        "mcp_tool_unpinned: tool `{tool}` of `{c}` is not approved for account `{a}`; a person approves it at a terminal (docs/CONNECTORS.md, MCP servers)"
                    ),
                );
            }
            BindingDiff::Tool { .. } => {
                return CallError::new(
                    CallErrorCode::Permission,
                    format!(
                        "mcp_tool_changed: tool `{tool}` of `{c}` changed since it was approved for account `{a}`; `apb connector tools {c} --account {a}` lists what changed, and a person approves it again at a terminal, which shows the new definition (docs/CONNECTORS.md, MCP servers)"
                    ),
                );
            }
        };
        CallError::new(
            CallErrorCode::Permission,
            format!(
                "mcp_server_changed: the server of `{c}` ({}) is not the one approved for account `{a}` ({why}); a person approves it again at a terminal (docs/CONNECTORS.md, MCP servers)",
                self.record.program
            ),
        )
    }
}

/// A started server whose tools were listed, before any tool is checked.
pub(crate) struct Opened {
    session: SessionHandle,
    tools: Vec<Value>,
    era: String,
    /// Warnings of the open (an http server's rotated refresh token).
    warnings: Vec<String>,
}

/// Starts the pinned program, the file `check_pin` hashed, with the fixed
/// environment and lists its tools within the listing limits. A failure
/// closes what was started.
pub(crate) fn open(
    checked: &mut CheckedPin,
    secrets: &BTreeMap<String, Secret>,
    redactions: &mut Vec<Redaction>,
) -> Result<Opened, CallError> {
    if let Some(target) = checked.http.clone() {
        let changed = || {
            checked.refusal(
                &BindingDiff::Program {
                    pinned: String::new(),
                },
                "",
            )
        };
        return open_http(
            HttpStart {
                target: &target,
                auth: checked.auth.as_ref(),
                account_fields: &checked.account_fields,
                connector: &checked.connector,
                account: &checked.account_name,
                deadline: checked.deadline,
            },
            secrets,
            redactions,
        )
        .map_err(|e| match e {
            OpenError::Call(e) => e,
            OpenError::ProgramChanged => changed(),
        });
    }
    let home = checked.home.clone().ok_or_else(no_home)?;
    let program = checked
        .program
        .take()
        .ok_or_else(|| checked.refusal(&BindingDiff::ProgramGone, ""))?;
    open_program(
        Start {
            program,
            spec: &checked.spec,
            account_fields: &checked.account_fields,
            home: &home,
            deadline: checked.deadline,
        },
        secrets,
        redactions,
    )
    .map_err(|e| match e {
        OpenError::Call(e) => e,
        OpenError::ProgramChanged => checked.refusal(
            &BindingDiff::Program {
                pinned: String::new(),
            },
            "",
        ),
    })
}

/// What starting a server takes besides the secrets.
pub(crate) struct Start<'a> {
    /// The program file, opened and hashed once; the start runs this file.
    pub program: ProgramFile,
    pub spec: &'a McpServerSpec,
    pub account_fields: &'a BTreeMap<String, String>,
    pub home: &'a AccountEntry,
    pub deadline: Instant,
}

/// Why `open_program` did not answer a listed server.
pub(crate) enum OpenError {
    Call(CallError),
    /// The path no longer held the hashed bytes right before the spawn
    /// (`StartFrom::PathCheckedAgain`); nothing started. The caller words
    /// it as its own `mcp_server_changed`.
    ProgramChanged,
}

/// Starts `start.program` with the fixed environment and lists its tools
/// within the listing limits. A failure closes what was started.
pub(crate) fn open_program(
    start: Start<'_>,
    secrets: &BTreeMap<String, Secret>,
    redactions: &[Redaction],
) -> Result<Opened, OpenError> {
    let server = server::stdio_server(
        start.program,
        start.spec,
        start.account_fields,
        secrets,
        start.home,
    )
    .map_err(OpenError::Call)?;
    let lifecycle = match start.spec.protocol {
        McpProtocol::Auto => Lifecycle::Auto,
        McpProtocol::Modern => Lifecycle::Modern,
        McpProtocol::Legacy => Lifecycle::Legacy,
    };
    let session = apb_mcp_client::Session::connect(
        apb_mcp_client::Server::Stdio(server),
        lifecycle,
        CLIENT_NAME,
        env!("CARGO_PKG_VERSION"),
        start.deadline,
    )
    .map_err(|f| {
        if f.kind == FailureKind::ProgramChanged {
            OpenError::ProgramChanged
        } else {
            OpenError::Call(failed(f, redactions))
        }
    })?;
    let era = match session.era() {
        Era::Modern(v) => format!("modern {v}"),
        Era::Legacy(v) => format!("legacy {v}"),
    };
    let session = SessionHandle::new(session);
    match session.list_tools(start.deadline) {
        Ok(tools) => Ok(Opened {
            session,
            tools,
            era,
            warnings: Vec::new(),
        }),
        Err(f) => Err(OpenError::Call(with_warning(
            map::connect_error(f, redactions),
            session.close_noted(),
        ))),
    }
}

/// What reaching an http server takes besides the secrets: its checked
/// target and the connector-level authorization.
pub(crate) struct HttpStart<'a> {
    pub target: &'a HttpTarget,
    pub auth: Option<&'a AuthSpec>,
    pub account_fields: &'a BTreeMap<String, String>,
    pub connector: &'a str,
    pub account: &'a str,
    pub deadline: Instant,
}

/// The request headers of an http server's `header` auth, rendered with
/// the resolved secrets into one secret header; none for another kind.
fn auth_headers(
    auth: Option<&AuthSpec>,
    fields: &BTreeMap<String, String>,
    secrets: &BTreeMap<String, Secret>,
) -> Result<Vec<(String, EnvValue)>, CallError> {
    let Some(AuthSpec::Header {
        header,
        value_template,
    }) = auth
    else {
        return Ok(Vec::new());
    };
    let ctx = RenderCtx {
        account: fields,
        args: &Value::Null,
        secrets,
    };
    let value = render_raw(value_template, &ctx).map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("auth header render failed: {e}"),
        )
    })?;
    Ok(vec![(header.clone(), EnvValue::Secret(Secret::new(value)))])
}

/// Reaches an http server whose target was checked, after the secrets were
/// resolved: the authorization (a `header` value, or for `oauth2_refresh`
/// a token minted through an agent with no proxy, its tokens registered in
/// `redactions` before anything is sent with them), the connect in the
/// modern revision and the listing within its limits. A 401 while
/// connecting or listing with `oauth2_refresh` takes one refresh: the new
/// tokens registered first, the server value rebuilt with the new bearer
/// (the old one dropped), and one more connect within the same deadline.
/// Nothing here hands a `tools/call` over.
///
/// It owns the open's warnings: every return after a mint, success or
/// error, carries the ones accumulated so far (a rotated refresh token
/// warns even when the connect, the refresh or the listing then fails).
pub(crate) fn open_http(
    start: HttpStart<'_>,
    secrets: &BTreeMap<String, Secret>,
    redactions: &mut Vec<Redaction>,
) -> Result<Opened, OpenError> {
    let mut warnings = Vec::new();
    match open_http_steps(start, secrets, redactions, &mut warnings) {
        Ok(mut opened) => {
            opened.warnings = warnings;
            Ok(opened)
        }
        Err(OpenError::Call(e)) => Err(OpenError::Call(e.carrying(&warnings))),
        Err(OpenError::ProgramChanged) => Err(OpenError::ProgramChanged),
    }
}

/// The steps of [`open_http`], adding every warning to `warnings` as it
/// arises; the caller attaches them to whatever this answers.
fn open_http_steps(
    start: HttpStart<'_>,
    secrets: &BTreeMap<String, Secret>,
    redactions: &mut Vec<Redaction>,
    warnings: &mut Vec<String>,
) -> Result<Opened, OpenError> {
    let source = TokenSource::render(
        start.auth,
        start.account_fields,
        secrets,
        start.connector,
        start.account,
        TokenProxy::None,
    )
    .map_err(OpenError::Call)?;
    let mut bearer = None;
    if let Some(source) = &source {
        // The token URL the source renders is the one the binding check
        // passed, or nothing is requested.
        if Some(source.token_url()) != start.target.token_url.as_deref() {
            return Err(OpenError::ProgramChanged);
        }
        let minted = source
            .access_token(start.deadline, redactions)
            .map_err(|e| OpenError::Call(CallError::from(e)))?;
        bearer = Some(adopt_minted(
            &minted,
            &mut |value, label| redactions.push(Redaction::new(value, label)),
            warnings,
        ));
    }
    let mut refreshed = false;
    loop {
        let headers =
            auth_headers(start.auth, start.account_fields, secrets).map_err(OpenError::Call)?;
        let server = server::http_server(&start.target.endpoint, headers, bearer.take());
        let attempt = connect_and_list(server, start.deadline);
        let failure = match attempt {
            Ok((session, tools, era)) => {
                return Ok(Opened {
                    session,
                    tools,
                    era,
                    warnings: Vec::new(),
                });
            }
            Err(failure) => failure,
        };
        let (f, note) = failure;
        let retry = matches!(f.kind, FailureKind::Auth { status: 401 }) && !refreshed;
        let Some(source) = source.as_ref().filter(|_| retry) else {
            return Err(OpenError::Call(with_warning(
                map::connect_error(f, redactions),
                note,
            )));
        };
        refreshed = true;
        match source.refresh_now(start.deadline, redactions) {
            Ok(minted) => {
                bearer = Some(adopt_minted(
                    &minted,
                    &mut |value, label| redactions.push(Redaction::new(value, label)),
                    warnings,
                ));
            }
            Err(token_err) => {
                let mut err = map::connect_error(f, redactions);
                err.message = redact_message(
                    format!(
                        "{}; renewing the token failed: {}",
                        err.message, token_err.message
                    ),
                    redactions,
                );
                return Err(OpenError::Call(with_warning(err, note)));
            }
        }
    }
}

/// A connected session, its raw listing and its era.
type Listed = (SessionHandle, Vec<Value>, String);

/// One connect of an http server and its listing; a failure answers the
/// client's `Failure` and the close's note.
fn connect_and_list(
    server: apb_mcp_client::HttpServer,
    deadline: Instant,
) -> Result<Listed, (Failure, Option<String>)> {
    let session = apb_mcp_client::Session::connect(
        apb_mcp_client::Server::Http(server),
        Lifecycle::Modern,
        CLIENT_NAME,
        env!("CARGO_PKG_VERSION"),
        deadline,
    )
    .map_err(|f| (f, None))?;
    let era = match session.era() {
        Era::Modern(v) => format!("modern {v}"),
        Era::Legacy(v) => format!("legacy {v}"),
    };
    let session = SessionHandle::new(session);
    match session.list_tools(deadline) {
        Ok(tools) => Ok((session, tools, era)),
        Err(f) => {
            let note = session.close_noted();
            Err((f, note))
        }
    }
}

/// The refusal of a machine whose account database names no home.
pub(crate) fn no_home() -> CallError {
    CallError::new(
        CallErrorCode::Config,
        "mcp: the user's home could not be read from the account database",
    )
}

impl Opened {
    /// Finds `tool` in the listing, refuses a definition that reflects a
    /// secret of the account, and compares its digest with the pin.
    /// Answers the digest.
    fn verify(
        &self,
        checked: &CheckedPin,
        function: &str,
        tool: &str,
        redactions: &[Redaction],
    ) -> Result<String, CallError> {
        let found = self
            .tools
            .iter()
            .find(|t| t.get("name").and_then(Value::as_str) == Some(tool))
            .ok_or_else(|| {
                CallError::new(
                    CallErrorCode::Config,
                    format!(
                        "mcp_tool_missing: the server no longer lists tool `{tool}` that `{}.{function}` maps",
                        checked.connector
                    ),
                )
            })?;
        if reflects_secret(found, redactions) {
            return Err(reflected_secret(&checked.connector, &checked.account_name));
        }
        let digest = apb_core::connector::mcp::digest::tool_digest(found);
        checked.compare(tool, Some(&digest))?;
        Ok(digest)
    }

    /// Closes the session after a failure, the end's warning added and
    /// the open's warnings carried.
    fn fail(self, err: CallError) -> CallError {
        with_warning(err, self.session.close_noted()).carrying(&self.warnings)
    }

    /// Runs the steps that follow the open with its warnings carried on
    /// every error they answer: the one point every post-open refusal
    /// (a changed pinned tool, a reflected secret, a verification
    /// refusal) passes through.
    pub(crate) fn carrying<T>(
        warnings: &[String],
        steps: impl FnOnce() -> Result<T, CallError>,
    ) -> Result<T, CallError> {
        steps().map_err(|e| e.carrying(warnings))
    }
}

/// The start, the listing, the reflected-secret walk and the tool pin
/// check, with the secrets resolved after `check_pin`. Every `Err` is a
/// failure before the send intent is recorded. Answers a connected session
/// whose tool is verified, with no argument sent yet.
pub(crate) fn connect(
    mut checked: CheckedPin,
    secrets: &BTreeMap<String, Secret>,
    mut redactions: Vec<Redaction>,
) -> Result<Connected, CallError> {
    let (function, tool) = checked
        .tools
        .first()
        .cloned()
        .ok_or_else(|| CallError::new(CallErrorCode::Config, "mcp: the call names no tool"))?;
    let mut opened = open(&mut checked, secrets, &mut redactions)?;
    let tool_digest = match opened.verify(&checked, &function, &tool, &redactions) {
        Ok(d) => d,
        Err(e) => return Err(opened.fail(e)),
    };
    // An http endpoint has no query, so no account-specific parameter is
    // journaled.
    let endpoint = match &checked.http {
        Some(target) => format!("mcp+{}#{tool}", target.endpoint),
        None => format!("mcp+stdio://{}/{tool}", checked.connector),
    };
    Ok(Connected {
        session: opened.session,
        warnings: std::mem::take(&mut opened.warnings),
        endpoint,
        tool,
        tool_digest,
        read_only: checked.read_only,
        response_pick: checked.response_pick,
        redactions,
        deadline: checked.deadline,
    })
}

/// A connected session whose tool is verified, before any argument is
/// sent.
pub(crate) struct Connected {
    session: SessionHandle,
    tool: String,
    tool_digest: String,
    endpoint: String,
    read_only: bool,
    response_pick: Vec<String>,
    redactions: Vec<Redaction>,
    deadline: Instant,
    /// Warnings of the open, carried into the dispatch.
    warnings: Vec<String>,
}

impl Connected {
    /// Puts the prepared arguments in (after the send intent is recorded
    /// and the marker stamped) and gives the call to dispatch. The call
    /// path checked them an object (`require_call_arguments`), and preparing them
    /// keeps that shape; `send` hands over only an object.
    pub(crate) fn with_arguments(self, arguments: Value) -> McpCall {
        McpCall {
            connected: self,
            arguments,
        }
    }

    /// Closes the session of a call that will not be made (the gate
    /// answered or refused after the connect), with `end_warning`.
    pub(crate) fn abandon(self) -> Option<String> {
        self.session.close_noted()
    }
}

/// The one open session of a call, shared by the call and the call path
/// so the session outlives `dispatch` and is closed only after the result
/// is journaled and settled.
#[derive(Clone)]
pub(crate) struct SessionHandle(Arc<Mutex<Option<apb_mcp_client::Session>>>);

impl SessionHandle {
    fn new(session: apb_mcp_client::Session) -> Self {
        SessionHandle(Arc::new(Mutex::new(Some(session))))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<apb_mcp_client::Session>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every tool of the server within the listing limits.
    fn list_tools(&self, deadline: Instant) -> Result<Vec<Value>, Failure> {
        match self.lock().as_ref() {
            Some(s) => s.list_tools(
                deadline,
                MCP_LIST_MAX_PAGES,
                MCP_LIST_MAX_TOOLS,
                MCP_LIST_MAX_BYTES,
            ),
            None => Err(session_closed()),
        }
    }

    fn call_tool(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        deadline: Instant,
    ) -> Result<ToolAnswer, CallFailure> {
        match self.lock().as_ref() {
            Some(s) => s.call_tool(name, arguments, deadline),
            // Nothing was handed to a transport.
            None => Err(CallFailure::NotSent(session_closed())),
        }
    }

    /// Closes the session (`Session::close`) once; later calls answer
    /// `Ended::NotStarted`.
    pub(crate) fn close(&self) -> Ended {
        self.close_counted().0
    }

    /// Closes the session once, with the lines its server wrote that the
    /// client dropped unread (`Session::dropped_lines`), counted once the
    /// close returned so a line written right after the last answer is in.
    fn close_counted(&self) -> (Ended, usize) {
        let taken = self.lock().take();
        match taken {
            Some(mut session) => {
                let ended = session.close();
                (ended, session.dropped_lines())
            }
            None => (Ended::NotStarted, 0),
        }
    }

    /// Closes the session once and answers its note: the end's warning
    /// (`end_warning`) and the count of dropped lines (`dropped_note`),
    /// joined, or `None` when neither applies. The one close every call
    /// path and the healthcheck report through.
    pub(crate) fn close_noted(&self) -> Option<String> {
        let (ended, dropped) = self.close_counted();
        let notes: Vec<String> = [end_warning(&ended), dropped_note(dropped)]
            .into_iter()
            .flatten()
            .collect();
        (!notes.is_empty()).then(|| notes.join("; "))
    }
}

fn session_closed() -> Failure {
    Failure {
        kind: FailureKind::Transport,
        message: "the MCP session is closed".to_string(),
        ended: Ended::NotStarted,
    }
}

/// One verified call with its arguments, ready to be handed over.
pub(crate) struct McpCall {
    connected: Connected,
    arguments: Value,
}

impl McpCall {
    /// `mcp+stdio://<connector>/<tool>`, or `mcp+<endpoint>#<tool>` for an
    /// http server.
    pub(crate) fn endpoint(&self) -> String {
        self.connected.endpoint.clone()
    }

    /// The verified pin digest, for `connector_call.tool_digest`.
    pub(crate) fn tool_digest(&self) -> String {
        self.connected.tool_digest.clone()
    }

    pub(crate) fn session(&self) -> SessionHandle {
        self.connected.session.clone()
    }

    pub(crate) fn cap_deadline(&mut self, at: Instant) {
        self.connected.deadline = self.connected.deadline.min(at);
    }

    /// Sends the call and maps the answer. Does not close the session.
    pub(crate) fn send(self) -> Dispatched {
        let McpCall {
            connected,
            arguments,
        } = self;
        let answer = match arguments {
            Value::Object(arguments) => {
                connected
                    .session
                    .call_tool(&connected.tool, arguments, connected.deadline)
            }
            // Nothing is handed over: a recorded intent settles not sent.
            _ => Err(CallFailure::NotSent(Failure {
                kind: FailureKind::Transport,
                message: "the prepared arguments are not a JSON object".to_string(),
                ended: Ended::NotStarted,
            })),
        };
        Dispatched {
            result: map::answer(
                answer,
                connected.read_only,
                &connected.response_pick,
                &connected.redactions,
            ),
            http_status: None,
            pages: None,
            warnings: connected.warnings,
        }
    }
}

/// The warning an end adds: `Some` only for `Ended::Unconfirmed`. The one
/// place that turns an end into text, so no path can word an unconfirmed
/// end as a stop.
pub(crate) fn end_warning(ended: &Ended) -> Option<String> {
    match ended {
        Ended::Unconfirmed => {
            Some("the MCP server process could not be confirmed ended".to_string())
        }
        Ended::Reaped {
            group: GroupEnd::NotShownEmpty,
            ..
        } => Some(GROUP_NOT_SHOWN_EMPTY.to_string()),
        Ended::Reaped {
            group: GroupEnd::Empty,
            ..
        }
        | Ended::NotStarted => None,
    }
}

/// The note of a session whose server wrote lines that are not JSON-RPC
/// messages (log text, blank lines), dropped unread: a count only, never
/// their text.
pub(crate) fn dropped_note(dropped: usize) -> Option<String> {
    (dropped > 0).then(|| {
        format!(
            "the MCP server wrote {dropped} line(s) on its output that are not JSON-RPC messages (log text or blank lines); apb dropped them unread"
        )
    })
}

/// The note of a server whose process group was not seen empty after the
/// close: a process the server started may still run, with the account's
/// secrets, and may still act.
pub(crate) const GROUP_NOT_SHOWN_EMPTY: &str = "the MCP server was reaped, but its process group could not be shown empty: a process it started may still run and act";

/// `err` with the end's warning appended to its message, when there is one.
pub(crate) fn with_end(err: CallError, ended: &Ended) -> CallError {
    with_warning(err, end_warning(ended))
}

/// `err` with `warning` appended to its message, when there is one.
pub(crate) fn with_warning(mut err: CallError, warning: Option<String>) -> CallError {
    if let Some(w) = warning {
        err.message = format!("{}; {w}", err.message);
    }
    err
}

/// A failed start or handshake: the client already ran its bounded close.
fn failed(f: Failure, redactions: &[Redaction]) -> CallError {
    let ended = f.ended.clone();
    with_end(map::connect_error(f, redactions), &ended)
}

/// A redacted refusal: every message of the healthcheck passes the
/// account's redactions.
pub(crate) fn redacted(mut err: CallError, redactions: &[Redaction]) -> CallError {
    err.message = redact_message(err.message, redactions);
    err
}

fn spec_of(doc: &ConnectorDoc) -> Result<&McpServerSpec, CallError> {
    doc.mcp.as_ref().ok_or_else(|| {
        CallError::new(
            CallErrorCode::Config,
            format!("mcp: connector `{}` has no `mcp:` block", doc.name),
        )
    })
}

fn tool_of(function: &FunctionSpec) -> Result<&str, CallError> {
    function
        .mcp
        .as_ref()
        .map(|m| m.tool.as_str())
        .ok_or_else(|| {
            CallError::new(
                CallErrorCode::Config,
                format!("mcp: function `{}` maps no tool", function.name),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use apb_mcp_client::Ended;

    #[test]
    fn an_unconfirmed_end_is_a_warning_never_a_stop_claim() {
        let warning = end_warning(&Ended::Unconfirmed).expect("a warning");
        assert!(
            warning.contains("could not be confirmed ended"),
            "{warning}"
        );
        for claim in ["stopped", "was killed", "has ended", "exited"] {
            assert!(!warning.contains(claim), "{warning}");
        }
        let reaped = |killed, group| Ended::Reaped { killed, group };
        assert_eq!(end_warning(&reaped(false, GroupEnd::Empty)), None);
        assert_eq!(end_warning(&reaped(true, GroupEnd::Empty)), None);
        assert_eq!(end_warning(&Ended::NotStarted), None);
        // A group not seen empty is said as what was not shown, never as
        // a claim about the processes in it.
        let group = end_warning(&reaped(false, GroupEnd::NotShownEmpty)).expect("a warning");
        assert!(group.contains("could not be shown empty"), "{group}");
        for claim in ["were killed", "are gone", "stopped"] {
            assert!(!group.contains(claim), "{group}");
        }
    }

    #[test]
    fn the_walk_redacts_strings_member_names_and_numbers() {
        use apb_core::secret::Secret;
        let redactions = vec![
            Redaction::new(Secret::new("tok-secret-1"), "token"),
            Redaction::new(Secret::new("123456789"), "pin"),
        ];
        let (v, changed) = redact_tree(
            json!({ "tok-secret-1": [123456789u64, "a tok-secret-1"], "n": 5 }),
            &redactions,
        );
        assert!(changed);
        let text = v.to_string();
        assert!(
            !text.contains("tok-secret-1") && !text.contains("123456789"),
            "{text}"
        );
        assert_eq!(v["n"], json!(5));
        let (same, changed) = redact_tree(json!({ "a": [1, "b"] }), &redactions);
        assert!(!changed);
        assert_eq!(same, json!({ "a": [1, "b"] }));
    }

    #[test]
    fn max_line_bytes_covers_the_body_cap() {
        use crate::connector::call::wire::BODY_CAP;
        assert_eq!(apb_mcp_client::MAX_LINE_BYTES, 7 * BODY_CAP + 64 * 1024);
    }
}
