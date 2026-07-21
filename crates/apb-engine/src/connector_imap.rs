//! Native IMAP execution for the `imap` connector function kind (spec 3.2,
//! wave 2). This is the read-only twin of the SMTP path in `connector_smtp`:
//! rendering, typed op-plan validation, dry-run shaping, and the blocking
//! `imap` client conversation live here, kept separate so each file stays
//! cohesive.
//!
//! Silent-read guarantee (spec 3.3): message content is fetched only through
//! `BODY.PEEK[]`, so reading a message never sets `\Seen`; the two read ops
//! (`search`, `fetch`) open the mailbox with `EXAMINE` (read-only), and only
//! `set_flags` opens it with `SELECT`. The string `BODY[` never appears in any
//! FETCH this module composes.
//!
//! Secrets (the IMAP password or OAuth token) are held in the resolved
//! `ImapCall` only, never logged: the event log records host/port and no
//! subjects (spec 3.4), and every error message is scrubbed through the interim
//! literal redaction before it can be printed.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use apb_core::connector::def::{ImapOp, ImapSpec};
use apb_core::connector::template::{
    Namespace, RenderCtx, placeholders, render_raw, resolve_optional_arg, single_args_placeholder,
};
use mail_parser::{MessageParser, MimeHeaders};
use serde_json::{Map, Value, json};

use crate::connector_result::{CallError, CallErrorCode, CallOk, redact_message};

/// The per-part cap for a fetched message's `text`/`html` body (spec 3.3): a
/// larger part is cut to this many bytes and the result marked `truncated`.
const BODY_PART_CAP: usize = 262144;

/// The outcome of preparing an imap call: a terminal dry-run render, or a
/// ready-to-run call. Public so the offline envelope render (`build` with
/// `dry_run: true`) can serve the dashboard playground and contract runners,
/// mirroring `SmtpBuild`.
#[derive(Debug)]
pub enum ImapBuild {
    DryRun(Value),
    Call(Box<ImapCall>),
}

/// How a function authenticates to the IMAP server (spec 3.2): `LOGIN` with a
/// password, or `AUTHENTICATE XOAUTH2` with a bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMethod {
    Password,
    XOauth2,
}

/// The typed, validated op plan (spec 3.3). Every field is already parsed from
/// the rendered params, so the execute path never re-parses a string.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ImapOpPlan {
    Verify,
    ListFolders,
    Search {
        folder: String,
        unread_only: bool,
        from_contains: Option<String>,
        subject_contains: Option<String>,
        since_days: Option<u32>,
        limit: u32,
    },
    Fetch {
        folder: String,
        uid: u32,
    },
    SetFlags {
        folder: String,
        uids: Vec<u32>,
        seen: bool,
    },
}

/// A rendered, ready-to-run imap call. Holds the resolved connection parameters
/// (including the secret password or token, never logged), the typed op plan,
/// the deadline budget, and the redaction pairs.
#[derive(Debug)]
pub struct ImapCall {
    host: String,
    port: u16,
    use_tls: bool,
    auth_method: AuthMethod,
    username: String,
    /// The secret: an account password (LOGIN) or a bearer token (XOAUTH2).
    /// Never logged; scrubbed out of every error via `redactions`.
    secret: String,
    plan: ImapOpPlan,
    timeout_sec: u64,
    /// (resolved secret value, redaction label) pairs; every error message is
    /// scrubbed against these before it leaves the process.
    redactions: Vec<(String, String)>,
}

impl ImapCall {
    /// The event-log endpoint: `imap://host:port`, no credentials.
    pub(crate) fn endpoint(&self) -> String {
        format!("imap://{}:{}", self.host, self.port)
    }

    /// Event metadata: none. Spec 3.4 keeps subjects and recipient counts out
    /// of the event log for imap, so this always returns `(None, None)`, in the
    /// same shape as the smtp path.
    pub(crate) fn event_extra(&self) -> (Option<String>, Option<u32>) {
        (None, None)
    }

    /// Connects (optionally over TLS), authenticates, runs the op, and logs
    /// out. Every returned error is scrubbed against the resolved secrets.
    pub fn send(self) -> Result<CallOk, CallError> {
        let deadline = Instant::now() + Duration::from_secs(self.timeout_sec);
        let stream = self.connect(deadline)?;
        let mut client = imap::Client::new(stream);
        client
            .read_greeting()
            .map_err(|e| self.map_err(Stage::Connect, e))?;

        let mut session = self.authenticate(client)?;
        let result = self.run_op(&mut session);
        // LOGOUT is best-effort: the op result is authoritative.
        let _ = session.logout();
        result
    }

    /// Opens the TCP (and, when `use_tls`, the rustls) stream with a connect
    /// timeout and read/write socket timeouts drawn from the remaining budget.
    fn connect(&self, deadline: Instant) -> Result<ImapStream, CallError> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| {
                self.redact(CallError::new(
                    CallErrorCode::Network,
                    format!(
                        "imap address `{}:{}` did not resolve: {e}",
                        self.host, self.port
                    ),
                ))
            })?
            .next()
            .ok_or_else(|| {
                self.redact(CallError::new(
                    CallErrorCode::Network,
                    format!("imap address `{}:{}` did not resolve", self.host, self.port),
                ))
            })?;

        let connect_budget = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        let tcp = TcpStream::connect_timeout(&addr, connect_budget).map_err(|e| {
            let code = if is_timeout(&e) {
                CallErrorCode::Timeout
            } else {
                CallErrorCode::Network
            };
            self.redact(CallError::new(
                code,
                format!("imap connection to {}:{} failed: {e}", self.host, self.port),
            ))
        })?;

        let budget = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        for r in [
            tcp.set_read_timeout(Some(budget)),
            tcp.set_write_timeout(Some(budget)),
        ] {
            r.map_err(|e| {
                self.redact(CallError::new(
                    CallErrorCode::Network,
                    format!("imap socket setup failed: {e}"),
                ))
            })?;
        }

        if self.use_tls {
            Ok(ImapStream::Tls(Box::new(self.wrap_tls(tcp)?)))
        } else {
            Ok(ImapStream::Plain(tcp))
        }
    }

    /// Wraps a connected TCP stream in a rustls client stream, verifying the
    /// server certificate with the platform trust store. Any TLS setup or
    /// handshake failure is a `network` error (spec 3.4).
    fn wrap_tls(&self, tcp: TcpStream) -> Result<RustlsStream, CallError> {
        use rustls_platform_verifier::BuilderVerifierExt;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| self.tls_err(format!("imap TLS provider setup failed: {e}")))?
            .with_platform_verifier()
            .map_err(|e| self.tls_err(format!("imap TLS platform verifier setup failed: {e}")))?
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|e| self.tls_err(format!("imap TLS server name is invalid: {e}")))?;
        let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| self.tls_err(format!("imap TLS handshake setup failed: {e}")))?;
        Ok(rustls::StreamOwned::new(conn, tcp))
    }

    fn tls_err(&self, message: String) -> CallError {
        self.redact(CallError::new(CallErrorCode::Network, message))
    }

    /// Runs LOGIN or AUTHENTICATE XOAUTH2. A rejected credential maps to
    /// `auth`; a transport failure maps to `network`/`timeout`.
    fn authenticate(&self, client: imap::Client<ImapStream>) -> Result<Session, CallError> {
        match self.auth_method {
            AuthMethod::Password => client
                .login(&self.username, &self.secret)
                .map_err(|(e, _)| self.map_err(Stage::Auth, e)),
            AuthMethod::XOauth2 => {
                let authenticator = XOauth2 {
                    user: self.username.clone(),
                    token: self.secret.clone(),
                };
                client
                    .authenticate("XOAUTH2", &authenticator)
                    .map_err(|(e, _)| self.map_err(Stage::Auth, e))
            }
        }
    }

    /// Dispatches the typed op against the authenticated session.
    fn run_op(&self, session: &mut Session) -> Result<CallOk, CallError> {
        match &self.plan {
            ImapOpPlan::Verify => Ok(CallOk::Smtp {
                body: json!({ "authenticated": true }),
            }),
            ImapOpPlan::ListFolders => self.op_list_folders(session),
            ImapOpPlan::Search {
                folder,
                unread_only,
                from_contains,
                subject_contains,
                since_days,
                limit,
            } => self.op_search(
                session,
                folder,
                *unread_only,
                from_contains.as_deref(),
                subject_contains.as_deref(),
                *since_days,
                *limit,
            ),
            ImapOpPlan::Fetch { folder, uid } => self.op_fetch(session, folder, *uid),
            ImapOpPlan::SetFlags { folder, uids, seen } => {
                self.op_set_flags(session, folder, uids, *seen)
            }
        }
    }

    fn op_list_folders(&self, session: &mut Session) -> Result<CallOk, CallError> {
        let names = session
            .list(Some(""), Some("*"))
            .map_err(|e| self.map_err(Stage::Op, e))?;
        let folders: Vec<Value> = names
            .iter()
            .map(|n| {
                let attrs: Vec<String> = n.attributes().iter().map(|a| format!("{a:?}")).collect();
                json!({ "name": n.name(), "attributes": attrs })
            })
            .collect();
        Ok(CallOk::Smtp {
            body: json!({ "folders": folders }),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn op_search(
        &self,
        session: &mut Session,
        folder: &str,
        unread_only: bool,
        from_contains: Option<&str>,
        subject_contains: Option<&str>,
        since_days: Option<u32>,
        limit: u32,
    ) -> Result<CallOk, CallError> {
        // Read op: open read-only with EXAMINE so no message loses \Recent and
        // nothing is marked \Seen (spec 3.3 silent-read guarantee).
        session
            .examine(folder)
            .map_err(|e| self.map_err(Stage::Op, e))?;
        let criteria = search_criteria(unread_only, from_contains, subject_contains, since_days);
        let hits = session
            .uid_search(&criteria)
            .map_err(|e| self.map_err(Stage::Op, e))?;
        let total_matched = hits.len();

        // Highest `limit` UIDs, newest first (highest UID first).
        let mut uids: Vec<u32> = hits.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(limit as usize);

        let messages = if uids.is_empty() {
            Vec::new()
        } else {
            let set = uids
                .iter()
                .map(|u| u.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let fetches = session
                .uid_fetch(&set, "(FLAGS ENVELOPE RFC822.SIZE INTERNALDATE)")
                .map_err(|e| self.map_err(Stage::Op, e))?;
            // Order the fetched rows to match the newest-first uid order.
            let mut by_uid: BTreeMap<u32, Value> = BTreeMap::new();
            for f in fetches.iter() {
                if let Some(uid) = f.uid {
                    by_uid.insert(uid, envelope_message_json(f));
                }
            }
            uids.iter().filter_map(|u| by_uid.remove(u)).collect()
        };

        Ok(CallOk::Smtp {
            body: json!({
                "folder": folder,
                "total_matched": total_matched,
                "messages": messages,
            }),
        })
    }

    fn op_fetch(&self, session: &mut Session, folder: &str, uid: u32) -> Result<CallOk, CallError> {
        // Read op: EXAMINE (read-only) plus BODY.PEEK[] so fetching content
        // never sets \Seen (spec 3.3 silent-read guarantee).
        session
            .examine(folder)
            .map_err(|e| self.map_err(Stage::Op, e))?;
        let fetches = session
            .uid_fetch(uid.to_string(), "(FLAGS BODY.PEEK[])")
            .map_err(|e| self.map_err(Stage::Op, e))?;
        let fetch = fetches.iter().find(|f| f.uid == Some(uid)).ok_or_else(|| {
            self.redact(CallError::new(
                CallErrorCode::Service,
                format!("imap message with uid {uid} was not found in `{folder}`"),
            ))
        })?;
        let seen = has_seen(fetch);
        let raw = fetch.body().unwrap_or(&[]);
        Ok(CallOk::Smtp {
            body: fetch_body_json(uid, raw, seen),
        })
    }

    fn op_set_flags(
        &self,
        session: &mut Session,
        folder: &str,
        uids: &[u32],
        seen: bool,
    ) -> Result<CallOk, CallError> {
        // The one write op: SELECT (read-write), then STORE the \Seen flag.
        session
            .select(folder)
            .map_err(|e| self.map_err(Stage::Op, e))?;
        let set = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let query = if seen {
            "+FLAGS (\\Seen)"
        } else {
            "-FLAGS (\\Seen)"
        };
        let updated = session
            .uid_store(&set, query)
            .map_err(|e| self.map_err(Stage::Op, e))?;
        Ok(CallOk::Smtp {
            body: json!({ "folder": folder, "updated": updated.iter().count() }),
        })
    }

    /// Maps an `imap::Error` into the call taxonomy (spec 3.4): a socket
    /// timeout is `timeout`; an I/O or lost-connection failure is `network`; a
    /// rejected LOGIN/AUTHENTICATE is `auth`; a server `NO`/`BAD` is `service`
    /// with the server text; a client-side string validation is `invalid_args`.
    /// Every message is scrubbed against the resolved secrets first.
    fn map_err(&self, stage: Stage, err: imap::Error) -> CallError {
        let call_err = match err {
            imap::Error::Io(e) if is_timeout(&e) => {
                CallError::new(CallErrorCode::Timeout, "imap operation timed out")
            }
            imap::Error::Io(e) => CallError::new(
                CallErrorCode::Network,
                format!("imap connection failed: {e}"),
            ),
            imap::Error::ConnectionLost => {
                CallError::new(CallErrorCode::Network, "imap connection was lost")
            }
            imap::Error::No(text) => self.protocol_err(stage, "NO", &text),
            imap::Error::Bad(text) => self.protocol_err(stage, "BAD", &text),
            imap::Error::Validate(e) => CallError::new(
                CallErrorCode::InvalidArgs,
                format!("imap command input is not valid: {e}"),
            ),
            other => CallError::new(CallErrorCode::Service, format!("imap error: {other}")),
        };
        self.redact(call_err)
    }

    /// A server rejection (`NO`/`BAD`). During the auth stage it is an `auth`
    /// error; otherwise it is a `service` error carrying the server text.
    fn protocol_err(&self, stage: Stage, kind: &str, text: &str) -> CallError {
        match stage {
            Stage::Auth => CallError::new(
                CallErrorCode::Auth,
                format!("imap authentication rejected ({kind}): {text}"),
            ),
            Stage::Connect | Stage::Op => CallError::new(
                CallErrorCode::Service,
                format!("imap server rejected the command ({kind}): {text}"),
            ),
        }
    }

    fn redact(&self, mut err: CallError) -> CallError {
        err.message = redact_message(err.message, &self.redactions);
        err
    }
}

/// Which stage an error came from, for the auth-vs-service split.
#[derive(Clone, Copy)]
enum Stage {
    Connect,
    Auth,
    Op,
}

/// The XOAUTH2 SASL authenticator (spec 3.2): the `imap` crate base64-encodes
/// the returned payload and sends it after the server's continuation request.
struct XOauth2 {
    user: String,
    token: String,
}

impl imap::Authenticator for XOauth2 {
    type Response = String;
    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
    }
}

/// A blocking IMAP transport: either a plaintext TCP stream (test listeners
/// only) or a rustls-wrapped one. Presenting a single `Read + Write` type lets
/// `imap::Client` be monomorphized over one stream type.
type RustlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

enum ImapStream {
    Plain(TcpStream),
    Tls(Box<RustlsStream>),
}

type Session = imap::Session<ImapStream>;

impl Read for ImapStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            ImapStream::Plain(s) => s.read(buf),
            ImapStream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for ImapStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            ImapStream::Plain(s) => s.write(buf),
            ImapStream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            ImapStream::Plain(s) => s.flush(),
            ImapStream::Tls(s) => s.flush(),
        }
    }
}

/// True when an I/O error is a socket read/write timeout (the read/write
/// timeout deadline elapsed), on either the connect or an operation.
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// True when the fetch carries the `\Seen` system flag.
fn has_seen(fetch: &imap::types::Fetch) -> bool {
    fetch
        .flags()
        .iter()
        .any(|f| matches!(f, imap::types::Flag::Seen))
}

/// Renders and validates an imap function. Dry-run shapes the endpoint and
/// typed params without connecting or touching secrets; a real build resolves
/// the full connection. Bad params are `invalid_args` and are caught before any
/// connection. Mirrors `connector_smtp::build`'s parameter shape exactly.
pub fn build(
    spec: &ImapSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, String>,
    redactions: Vec<(String, String)>,
    dry_run: bool,
    timeout_sec: u64,
) -> Result<ImapBuild, CallError> {
    let ctx = RenderCtx {
        account,
        args,
        secrets,
    };

    // Params + typed plan render for both dry-run and real runs (args only, no
    // secrets), so a bad param is rejected before anything connects.
    let plan = build_plan(spec.op, &spec.params, &ctx)?;

    if dry_run {
        // The endpoint uses only the host/port templates (account values), so
        // it renders without resolving any secret. The credentials are not
        // rendered at all in dry-run.
        let host = render_field("host", &spec.connection.host, &ctx)?;
        let port = render_port(&spec.connection.port, &ctx)?;
        return Ok(ImapBuild::DryRun(json!({
            "ok": true,
            "dry_run": true,
            "imap": {
                "endpoint": format!("imap://{host}:{port}"),
                "op": spec.op.as_str(),
                "params": plan_params_json(&plan),
            },
        })));
    }

    let conn = render_connection(spec, &ctx)?;
    Ok(ImapBuild::Call(Box::new(ImapCall {
        host: conn.host,
        port: conn.port,
        use_tls: conn.use_tls,
        auth_method: conn.auth_method,
        username: conn.username,
        secret: conn.secret,
        plan,
        timeout_sec,
        redactions,
    })))
}

/// The resolved IMAP connection parameters. The secret (password or token) is
/// never logged.
struct Connection {
    host: String,
    port: u16,
    use_tls: bool,
    auth_method: AuthMethod,
    username: String,
    secret: String,
}

/// Builds a config-kind render error naming the imap field.
fn render_err(field: &str, err: impl std::fmt::Display) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!("imap {field} render failed: {err}"),
    )
}

/// Renders one required connection field verbatim; a render failure is a
/// config error naming the field.
fn render_field(field: &str, template: &str, ctx: &RenderCtx) -> Result<String, CallError> {
    render_raw(template, ctx).map_err(|e| render_err(field, e))
}

/// Renders and parses the port to a u16 (config error otherwise).
fn render_port(template: &str, ctx: &RenderCtx) -> Result<u16, CallError> {
    let port_s = render_field("port", template, ctx)?;
    port_s.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::Config,
            format!("imap port is not a valid port: `{port_s}`"),
        )
    })
}

/// Renders the full connection block into concrete parameters (real runs
/// only). `use_tls` defaults to true when its template references an absent
/// account field (mirrors the smtp presence rule); `auth_method` must render to
/// `password` or `xoauth2`.
fn render_connection(spec: &ImapSpec, ctx: &RenderCtx) -> Result<Connection, CallError> {
    let c = &spec.connection;
    let host = render_field("host", &c.host, ctx)?;
    let port = render_port(&c.port, ctx)?;
    let use_tls = render_use_tls(&c.use_tls, ctx)?;
    let auth_method = render_auth_method(&c.auth_method, ctx)?;
    let username = render_field("username", &c.username, ctx)?;
    let secret = render_field("password", &c.password, ctx)?;
    Ok(Connection {
        host,
        port,
        use_tls,
        auth_method,
        username,
        secret,
    })
}

/// Renders `use_tls`: an absent account placeholder defaults to `true`; a
/// present value must parse to a bool (config error otherwise).
fn render_use_tls(template: &str, ctx: &RenderCtx) -> Result<bool, CallError> {
    for (ns, name) in placeholders(template).map_err(|e| render_err("use_tls", e))? {
        if ns == Namespace::Account && !ctx.account.contains_key(name.as_str()) {
            return Ok(true);
        }
    }
    let s = render_field("use_tls", template, ctx)?;
    s.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::Config,
            format!("imap use_tls must be true or false: `{s}`"),
        )
    })
}

/// Renders `auth_method`: it must be `password` or `xoauth2`; anything else is
/// a config error naming the field.
fn render_auth_method(template: &str, ctx: &RenderCtx) -> Result<AuthMethod, CallError> {
    let s = render_field("auth_method", template, ctx)?;
    match s.trim() {
        "password" => Ok(AuthMethod::Password),
        "xoauth2" => Ok(AuthMethod::XOauth2),
        other => Err(CallError::new(
            CallErrorCode::Config,
            format!("imap auth_method must be `password` or `xoauth2`: `{other}`"),
        )),
    }
}

/// Renders the op params and validates them into a typed `ImapOpPlan`. Every
/// failure is `invalid_args` naming the param. A params value that is a single
/// `{{args.x}}` placeholder with `x` absent (or explicitly null) is dropped
/// (Task 3 rule), which is how optional search params arrive absent.
fn build_plan(
    op: ImapOp,
    params: &BTreeMap<String, String>,
    ctx: &RenderCtx,
) -> Result<ImapOpPlan, CallError> {
    let rendered = render_params(params, ctx)?;
    match op {
        ImapOp::Verify => Ok(ImapOpPlan::Verify),
        ImapOp::ListFolders => Ok(ImapOpPlan::ListFolders),
        ImapOp::Search => {
            let folder = require_folder(&rendered)?;
            let unread_only = match rendered.get("unread_only") {
                Some(v) => parse_bool("unread_only", v)?,
                None => false,
            };
            let from_contains = optional_text(&rendered, "from_contains")?;
            let subject_contains = optional_text(&rendered, "subject_contains")?;
            let since_days = match rendered.get("since_days") {
                Some(v) => Some(parse_since_days(v)?),
                None => None,
            };
            let limit = parse_limit(&rendered)?;
            Ok(ImapOpPlan::Search {
                folder,
                unread_only,
                from_contains,
                subject_contains,
                since_days,
                limit,
            })
        }
        ImapOp::Fetch => {
            let folder = require_folder(&rendered)?;
            let uid = parse_uid(&rendered)?;
            Ok(ImapOpPlan::Fetch { folder, uid })
        }
        ImapOp::SetFlags => {
            let folder = require_folder(&rendered)?;
            let uids = parse_uids(&rendered)?;
            let seen = match rendered.get("seen") {
                Some(v) => parse_bool("seen", v)?,
                None => return Err(missing("seen")),
            };
            Ok(ImapOpPlan::SetFlags { folder, uids, seen })
        }
    }
}

/// Renders the params map, applying the single-placeholder drop rule: a value
/// that is exactly `{{args.x}}` with `x` absent or explicitly null is dropped;
/// any other value interpolates as a string (spec 3.3, Task 3 semantics).
fn render_params(
    params: &BTreeMap<String, String>,
    ctx: &RenderCtx,
) -> Result<BTreeMap<String, String>, CallError> {
    let mut out = BTreeMap::new();
    for (key, template) in params {
        if let Some(field) = single_args_placeholder(template)
            && resolve_optional_arg(ctx, field).is_none()
        {
            continue;
        }
        let value = render_raw(template, ctx).map_err(|e| {
            CallError::new(
                CallErrorCode::InvalidArgs,
                format!("imap param `{key}` render failed: {e}"),
            )
        })?;
        out.insert(key.clone(), value);
    }
    Ok(out)
}

fn missing(param: &str) -> CallError {
    CallError::new(
        CallErrorCode::InvalidArgs,
        format!("imap op is missing required param `{param}`"),
    )
}

/// Requires a non-empty `folder`, rejecting any control character (protocol
/// injection guard, spec section 2).
fn require_folder(rendered: &BTreeMap<String, String>) -> Result<String, CallError> {
    let folder = rendered.get("folder").ok_or_else(|| missing("folder"))?;
    reject_controls("folder", folder)?;
    if folder.is_empty() {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            "imap param `folder` must not be empty",
        ));
    }
    Ok(folder.clone())
}

/// An optional free-text search param, rejecting control characters.
fn optional_text(
    rendered: &BTreeMap<String, String>,
    param: &str,
) -> Result<Option<String>, CallError> {
    match rendered.get(param) {
        Some(v) => {
            reject_controls(param, v)?;
            Ok(Some(v.clone()))
        }
        None => Ok(None),
    }
}

/// Rejects any control character (including CR and LF) in a value that reaches
/// an IMAP command line, so a param cannot inject a second command (spec 2).
fn reject_controls(param: &str, value: &str) -> Result<(), CallError> {
    if value.chars().any(|c| c.is_control()) {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            format!("imap param `{param}` contains a control character"),
        ));
    }
    Ok(())
}

fn parse_bool(param: &str, value: &str) -> Result<bool, CallError> {
    value.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::InvalidArgs,
            format!("imap param `{param}` must be true or false: `{value}`"),
        )
    })
}

fn parse_limit(rendered: &BTreeMap<String, String>) -> Result<u32, CallError> {
    let raw = rendered.get("limit").ok_or_else(|| missing("limit"))?;
    let limit: u32 = raw.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::InvalidArgs,
            format!("imap param `limit` is not a number: `{raw}`"),
        )
    })?;
    if !(1..=100).contains(&limit) {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            format!("imap param `limit` must be between 1 and 100: `{limit}`"),
        ));
    }
    Ok(limit)
}

fn parse_since_days(value: &str) -> Result<u32, CallError> {
    let days: u32 = value.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::InvalidArgs,
            format!("imap param `since_days` is not a number: `{value}`"),
        )
    })?;
    if days < 1 {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            "imap param `since_days` must be at least 1",
        ));
    }
    Ok(days)
}

fn parse_uid(rendered: &BTreeMap<String, String>) -> Result<u32, CallError> {
    let raw = rendered.get("uid").ok_or_else(|| missing("uid"))?;
    raw.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::InvalidArgs,
            format!("imap param `uid` is not a number: `{raw}`"),
        )
    })
}

/// Parses `uids` as a non-empty comma-separated u32 list.
fn parse_uids(rendered: &BTreeMap<String, String>) -> Result<Vec<u32>, CallError> {
    let raw = rendered.get("uids").ok_or_else(|| missing("uids"))?;
    let mut out = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let uid: u32 = token.parse().map_err(|_| {
            CallError::new(
                CallErrorCode::InvalidArgs,
                format!("imap param `uids` has a non-numeric entry: `{token}`"),
            )
        })?;
        out.push(uid);
    }
    if out.is_empty() {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            "imap param `uids` must be a non-empty comma-separated list",
        ));
    }
    Ok(out)
}

/// Composes the IMAP SEARCH criteria from the typed plan. `UNSEEN` for
/// unread-only; `FROM`/`SUBJECT` with the value IMAP-quoted (backslash and
/// double-quote escaped); `SINCE dd-Mon-yyyy` in UTC from `since_days`.
fn search_criteria(
    unread_only: bool,
    from_contains: Option<&str>,
    subject_contains: Option<&str>,
    since_days: Option<u32>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if unread_only {
        parts.push("UNSEEN".to_string());
    }
    if let Some(v) = from_contains {
        parts.push(format!("FROM {}", quote_imap(v)));
    }
    if let Some(v) = subject_contains {
        parts.push(format!("SUBJECT {}", quote_imap(v)));
    }
    if let Some(days) = since_days {
        parts.push(format!("SINCE {}", since_date(days)));
    }
    if parts.is_empty() {
        // A bare search with no criteria matches every message in the mailbox.
        parts.push("ALL".to_string());
    }
    parts.join(" ")
}

/// IMAP-quotes a search value: wrap in double quotes with `\` and `"` escaped
/// (RFC 3501 quoted string). Control characters were already rejected upstream.
fn quote_imap(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Formats `now - since_days` in UTC as an IMAP `dd-Mon-yyyy` date.
fn since_date(days: u32) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let target = now - (days as i64) * 86_400;
    let (y, m, d) = civil_from_days(target.div_euclid(86_400));
    format!("{:02}-{}-{:04}", d, MONTHS[m - 1], y)
}

/// Converts a count of days since the Unix epoch to a `(year, month, day)`
/// civil date (Howard Hinnant's algorithm), so a UTC `SINCE` date needs no
/// date-library dependency.
fn civil_from_days(z: i64) -> (i64, usize, usize) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as usize;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as usize;
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Builds the search result JSON for one message from its FLAGS/ENVELOPE fetch.
/// Absent envelope parts render as absent fields (spec 3.3).
fn envelope_message_json(fetch: &imap::types::Fetch) -> Value {
    let mut m = Map::new();
    if let Some(uid) = fetch.uid {
        m.insert("uid".into(), json!(uid));
    }
    if let Some(env) = fetch.envelope() {
        // The address type here is the `imap` crate's own (older, pinned)
        // imap-proto dependency, which it does not re-export by name. A
        // shared closure bound once via `let` can't be type-checked ahead of
        // its use sites (E0282), so this stays a macro: each expansion site
        // infers the concrete (unnamable) address type independently from
        // its own `list.iter()` context, while the formatting logic itself
        // has one source of truth.
        macro_rules! address_to_string {
            ($a:expr) => {{
                let mailbox = $a.mailbox.map(bytes_lossy);
                let host = $a.host.map(bytes_lossy);
                match (mailbox, host) {
                    (Some(mb), Some(h)) => Some(format!("{mb}@{h}")),
                    (Some(mb), None) => Some(mb),
                    (None, Some(h)) => Some(h),
                    (None, None) => None,
                }
            }};
        }
        if let Some(from) = env
            .from
            .as_deref()
            .and_then(|list| list.iter().find_map(|a| address_to_string!(a)))
        {
            m.insert("from".into(), json!(from));
        }
        if let Some(to) = env
            .to
            .as_deref()
            .map(|list| {
                list.iter()
                    .filter_map(|a| address_to_string!(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|s| !s.is_empty())
        {
            m.insert("to".into(), json!(to));
        }
        if let Some(subject) = env.subject.map(bytes_lossy) {
            m.insert("subject".into(), json!(subject));
        }
        if let Some(date) = env.date.map(bytes_lossy) {
            m.insert("date".into(), json!(date));
        }
    }
    m.insert("seen".into(), json!(has_seen(fetch)));
    if let Some(size) = fetch.size {
        m.insert("size".into(), json!(size));
    }
    Value::Object(m)
}

fn bytes_lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Parses the raw RFC822 bytes of a fetched message into the fetch result body
/// (spec 3.3): from/to/cc/subject/date/seen plus the capped text and html
/// bodies, the attachment list, and the `truncated` flag.
fn fetch_body_json(uid: u32, raw: &[u8], seen: bool) -> Value {
    let mut m = Map::new();
    m.insert("uid".into(), json!(uid));
    let mut truncated = false;

    if let Some(msg) = MessageParser::default().parse(raw) {
        if let Some(from) = msg.from().and_then(mail_address_first) {
            m.insert("from".into(), json!(from));
        }
        if let Some(to) = msg.to().map(mail_address_join).filter(|s| !s.is_empty()) {
            m.insert("to".into(), json!(to));
        }
        if let Some(cc) = msg.cc().map(mail_address_join).filter(|s| !s.is_empty()) {
            m.insert("cc".into(), json!(cc));
        }
        if let Some(subject) = msg.subject() {
            m.insert("subject".into(), json!(subject));
        }
        if let Some(date) = msg.date() {
            m.insert("date".into(), json!(date.to_rfc3339()));
        }
        if let Some(text) = msg.body_text(0) {
            let (capped, cut) = cap_body(&text);
            truncated |= cut;
            m.insert("text".into(), json!(capped));
        }
        if let Some(html) = msg.body_html(0) {
            let (capped, cut) = cap_body(&html);
            truncated |= cut;
            m.insert("html".into(), json!(capped));
        }
        let attachments: Vec<Value> = msg
            .attachments()
            .map(|part| {
                json!({
                    "filename": part.attachment_name(),
                    "mime": mime_of(part),
                    "size": part.len(),
                })
            })
            .collect();
        m.insert("attachments".into(), json!(attachments));
    } else {
        m.insert("attachments".into(), json!([]));
    }

    m.insert("seen".into(), json!(seen));
    m.insert("truncated".into(), json!(truncated));
    Value::Object(m)
}

/// Caps a body part at `BODY_PART_CAP` bytes on a UTF-8 boundary, reporting
/// whether anything was cut (spec 3.3).
fn cap_body(s: &str) -> (String, bool) {
    if s.len() <= BODY_PART_CAP {
        return (s.to_string(), false);
    }
    let mut end = BODY_PART_CAP;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// The `type/subtype` MIME string of a message part, or `application/octet-stream`.
fn mime_of(part: &mail_parser::MessagePart) -> String {
    match part.content_type() {
        Some(ct) => match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_string(),
        },
        None => "application/octet-stream".to_string(),
    }
}

/// The first address of a mail-parser address header, as its email string.
fn mail_address_first(addr: &mail_parser::Address) -> Option<String> {
    addr.first()
        .and_then(|a| a.address.as_ref())
        .map(|c| c.to_string())
}

/// All addresses of a mail-parser address header joined by `, `.
fn mail_address_join(addr: &mail_parser::Address) -> String {
    addr.iter()
        .filter_map(|a| a.address.as_ref().map(|c| c.to_string()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The typed params JSON for the dry-run render (spec 3.3): the plan's fields,
/// with optional search params present only when set.
fn plan_params_json(plan: &ImapOpPlan) -> Value {
    match plan {
        ImapOpPlan::Verify | ImapOpPlan::ListFolders => json!({}),
        ImapOpPlan::Search {
            folder,
            unread_only,
            from_contains,
            subject_contains,
            since_days,
            limit,
        } => {
            let mut m = Map::new();
            m.insert("folder".into(), json!(folder));
            m.insert("unread_only".into(), json!(unread_only));
            if let Some(v) = from_contains {
                m.insert("from_contains".into(), json!(v));
            }
            if let Some(v) = subject_contains {
                m.insert("subject_contains".into(), json!(v));
            }
            if let Some(v) = since_days {
                m.insert("since_days".into(), json!(v));
            }
            m.insert("limit".into(), json!(limit));
            Value::Object(m)
        }
        ImapOpPlan::Fetch { folder, uid } => json!({ "folder": folder, "uid": uid }),
        ImapOpPlan::SetFlags { folder, uids, seen } => {
            json!({ "folder": folder, "uids": uids, "seen": seen })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_body_truncates_large_part() {
        // A part just over the cap is cut and reported truncated; the returned
        // text is exactly the cap length (on a UTF-8 boundary of ASCII input).
        let big = "a".repeat(BODY_PART_CAP + 10);
        let (capped, truncated) = cap_body(&big);
        assert!(truncated, "an over-cap part must report truncated");
        assert_eq!(capped.len(), BODY_PART_CAP);
    }

    #[test]
    fn cap_body_keeps_small_part() {
        let (capped, truncated) = cap_body("hello");
        assert!(!truncated);
        assert_eq!(capped, "hello");
    }

    #[test]
    fn fetch_body_json_caps_large_text() {
        // The fetch body helper parses a raw message with an over-cap text part
        // and marks the whole result truncated (the unit named in the brief).
        let big = "x".repeat(BODY_PART_CAP + 100);
        let raw = format!(
            "From: a@b.c\r\nTo: d@e.f\r\nSubject: Big\r\nContent-Type: text/plain\r\n\r\n{big}"
        );
        let body = fetch_body_json(7, raw.as_bytes(), true);
        assert_eq!(body["uid"], json!(7));
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(body["seen"], json!(true));
        assert_eq!(body["text"].as_str().unwrap().len(), BODY_PART_CAP);
    }

    #[test]
    fn quote_imap_escapes_backslash_and_quote() {
        assert_eq!(quote_imap("he said \"hi\""), "\"he said \\\"hi\\\"\"");
        assert_eq!(quote_imap("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn search_criteria_composes_all_clauses() {
        let c = search_criteria(true, Some("bob@x"), Some("re: hi"), Some(7));
        assert!(c.contains("UNSEEN"), "{c}");
        assert!(c.contains("FROM \"bob@x\""), "{c}");
        assert!(c.contains("SUBJECT \"re: hi\""), "{c}");
        assert!(c.contains("SINCE "), "{c}");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-01-01 is 10957 days after the epoch.
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }
}
