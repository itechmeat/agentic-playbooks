//! Native SMTP execution for the `smtp` connector function kind (spec 4.2).
//! Rendering, address validation, dry-run shaping, and the blocking lettre
//! transport (connect / EHLO / STARTTLS / AUTH / MAIL-RCPT-DATA / QUIT) live
//! here. This module is the SMTP twin of the HTTP path in `connector_call`,
//! kept separate so that file stays cohesive.
//!
//! Secrets (the SMTP password) are held in the resolved `SmtpCall` only, never
//! logged: the event log records host/port/subject/recipient-count, and every
//! error message is scrubbed through the interim literal redaction before it
//! can be printed.

use std::collections::BTreeMap;

use apb_core::connector::def::{SmtpMessage, SmtpSpec};
use apb_core::connector::template::{Namespace, RenderCtx, placeholders, render_raw};
use lettre::Address;
use lettre::message::{Mailbox, Message, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{SmtpConnection, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use serde_json::{Value, json};

use crate::connector_result::{CallError, CallErrorCode, CallOk, redact_message};

/// The outcome of preparing an smtp call: a terminal dry-run render, or a
/// ready-to-send call. Public so the offline envelope render (`build` with
/// `dry_run: true`) can serve slice 4's contract-test runner (spec obligation).
#[derive(Debug)]
pub enum SmtpBuild {
    DryRun(Value),
    Call(Box<SmtpCall>),
}

/// A rendered, ready-to-send smtp call. Holds resolved connection parameters
/// (including the secret password, never logged), the built envelope, and the
/// formatted message bytes. `verify` calls carry no message.
#[derive(Debug)]
pub struct SmtpCall {
    host: String,
    port: u16,
    use_tls: bool,
    username: Option<String>,
    password: Option<String>,
    timeout_sec: u64,
    /// `Some` for send, `None` for verify.
    message: Option<BuiltMessage>,
    /// (resolved secret value, redaction label) pairs; every error message is
    /// scrubbed against these before it leaves the process.
    redactions: Vec<(String, String)>,
}

/// The built email: the from address string, the subject, the per-list
/// recipient email strings, the lettre envelope, and the RFC 5322 bytes.
#[derive(Debug)]
struct BuiltMessage {
    from: String,
    subject: String,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    recipients: Vec<String>,
    envelope: lettre::address::Envelope,
    formatted: Vec<u8>,
}

/// Which handshake stage an error came from, for taxonomy mapping (spec 4.2).
#[derive(Clone, Copy)]
enum Stage {
    Connect,
    Starttls,
    Auth,
    Send,
}

impl SmtpCall {
    /// The event-log endpoint: `smtp://host:port`, no credentials.
    pub(crate) fn endpoint(&self) -> String {
        format!("smtp://{}:{}", self.host, self.port)
    }

    /// Event metadata: subject and recipient count (both `None` for verify).
    pub(crate) fn event_extra(&self) -> (Option<String>, Option<u32>) {
        match &self.message {
            Some(m) => (Some(m.subject.clone()), Some(m.recipients.len() as u32)),
            None => (None, None),
        }
    }

    /// Connects, optionally upgrades to TLS via STARTTLS, authenticates, and
    /// sends the message (or, for a verify function, only probes the
    /// connection). Refuses to proceed in plaintext when `use_tls` is set but
    /// the server does not advertise STARTTLS, and refuses to AUTH when
    /// credentials are present over a non-TLS connection (the password would go
    /// in cleartext); the sole plaintext opt-in is a credential-less relay. The
    /// `QUIT` is best-effort.
    pub fn send(self) -> Result<CallOk, CallError> {
        let timeout = std::time::Duration::from_secs(self.timeout_sec);
        let hello = ClientId::default();

        // Connect (TCP + greeting + EHLO). No implicit TLS; STARTTLS is applied
        // below when use_tls is set.
        let mut conn = SmtpConnection::connect(
            (self.host.as_str(), self.port),
            Some(timeout),
            &hello,
            None,
            None,
        )
        .map_err(|e| self.map_err(Stage::Connect, e))?;

        if self.use_tls {
            if !conn.can_starttls() {
                let _ = conn.quit();
                return Err(CallError::new(
                    CallErrorCode::Service,
                    "smtp server does not advertise STARTTLS but use_tls is set; refusing to send in plaintext",
                ));
            }
            let tls = TlsParameters::new(self.host.clone()).map_err(|e| {
                CallError::new(CallErrorCode::Config, format!("smtp TLS setup failed: {e}"))
            })?;
            conn.starttls(&tls, &hello)
                .map_err(|e| self.map_err(Stage::Starttls, e))?;
        }

        // AUTH transmits the password. Refuse to authenticate over a plaintext
        // connection: if credentials are present but the connection was not
        // upgraded to TLS, fail before any AUTH so the password never leaves in
        // cleartext. The one plaintext opt-in is a credential-less relay.
        if !self.use_tls && (self.username.is_some() || self.password.is_some()) {
            let _ = conn.quit();
            return Err(CallError::new(
                CallErrorCode::Config,
                "smtp credentials over a plaintext connection are refused; set use_tls to true, or drop username and password for an unauthenticated relay",
            ));
        }

        if let (Some(user), Some(pass)) = (self.username.as_ref(), self.password.as_ref()) {
            let creds = Credentials::new(user.clone(), pass.clone());
            conn.auth(&[Mechanism::Plain, Mechanism::Login], &creds)
                .map_err(|e| self.map_err(Stage::Auth, e))?;
        }

        let result = match &self.message {
            Some(m) => {
                conn.send(&m.envelope, &m.formatted)
                    .map_err(|e| self.map_err(Stage::Send, e))?;
                CallOk::Smtp {
                    body: json!({
                        "accepted": m.recipients,
                        "from": m.from,
                        "subject": m.subject,
                    }),
                }
            }
            None => CallOk::Smtp {
                body: json!({ "verified": true }),
            },
        };
        let _ = conn.quit();
        Ok(result)
    }

    /// Maps a lettre smtp error into the call taxonomy (spec 4.2): a timeout is
    /// `timeout`; a connect/DNS failure is `network`; an AUTH rejection is
    /// `auth`; any other protocol rejection is `service` with the SMTP reply
    /// carried in the message. Every message is scrubbed through the interim
    /// literal redaction so a resolved password can never leak.
    fn map_err(&self, stage: Stage, err: lettre::transport::smtp::Error) -> CallError {
        if err.is_timeout() {
            return self.redact(CallError::new(
                CallErrorCode::Timeout,
                "smtp operation timed out",
            ));
        }
        let (code, prefix) = match stage {
            Stage::Connect => (CallErrorCode::Network, "smtp connection failed"),
            Stage::Auth => (CallErrorCode::Auth, "smtp authentication rejected"),
            Stage::Starttls => (CallErrorCode::Service, "smtp STARTTLS failed"),
            Stage::Send => (CallErrorCode::Service, "smtp send rejected"),
        };
        self.redact(CallError::new(code, format!("{prefix}: {err}")))
    }

    fn redact(&self, mut err: CallError) -> CallError {
        err.message = redact_message(err.message, &self.redactions);
        err
    }
}

/// Renders and validates an smtp function. Dry-run shapes the envelope without
/// connecting or touching secrets; a real build additionally resolves the
/// connection parameters. Bad addresses are `invalid_args` and are caught
/// before any connection.
pub fn build(
    spec: &SmtpSpec,
    account: &BTreeMap<String, String>,
    args: &Value,
    secrets: &BTreeMap<String, String>,
    redactions: Vec<(String, String)>,
    dry_run: bool,
    timeout_sec: u64,
) -> Result<SmtpBuild, CallError> {
    let ctx = RenderCtx {
        account,
        args,
        secrets,
    };

    // Message rendering + address validation happens for both dry-run and real
    // sends (verify has no message).
    let built = match &spec.message {
        Some(m) => Some(render_message(m, &ctx)?),
        None => None,
    };

    if dry_run {
        return Ok(SmtpBuild::DryRun(dry_run_json(account, &built)));
    }

    let conn = render_connection(spec, &ctx)?;
    Ok(SmtpBuild::Call(Box::new(SmtpCall {
        host: conn.host,
        port: conn.port,
        use_tls: conn.use_tls,
        username: conn.username,
        password: conn.password,
        timeout_sec,
        message: built,
        redactions,
    })))
}

/// The resolved SMTP connection parameters (host/port/TLS plus optional
/// credentials). The password is never logged.
struct Connection {
    host: String,
    port: u16,
    use_tls: bool,
    username: Option<String>,
    password: Option<String>,
}

/// Builds a config-kind render error naming the smtp field.
fn render_err(field: &str, err: impl std::fmt::Display) -> CallError {
    CallError::new(
        CallErrorCode::Config,
        format!("smtp {field} render failed: {err}"),
    )
}

/// Renders one OPTIONAL smtp message field leniently (cross-slice obligation
/// 4). Slice-5 manifests template optional fields individually, e.g.
/// `cc: "{{args.cc}}"`, and callers legitimately omit the backing arg. The
/// generic renderer treats an unresolved `{{args.*}}`/`{{account.*}}`
/// placeholder as a hard error; for an OPTIONAL field that must instead mean
/// "field absent". This helper distinguishes the two:
///   - `Ok(None)` when the template references an arg or account value that is
///     absent from the call context, or renders to an empty string;
///   - `Ok(Some(value))` for a non-empty render;
///   - `Err(..)` for any structural render error (malformed placeholder or a
///     non-scalar arg value), which still fails loudly.
///
/// Required fields (`from_email`, `to`, `subject`) never use this path, so a
/// missing required arg still fails.
fn render_optional(
    field: &str,
    template: &str,
    ctx: &RenderCtx,
) -> Result<Option<String>, CallError> {
    for (ns, name) in placeholders(template).map_err(|e| render_err(field, e))? {
        let absent = match ns {
            Namespace::Args => !name.is_empty() && ctx.args.get(name.as_str()).is_none(),
            Namespace::Account => !ctx.account.contains_key(name.as_str()),
            _ => false,
        };
        if absent {
            return Ok(None);
        }
    }
    let value = render_raw(template, ctx).map_err(|e| render_err(field, e))?;
    Ok(if value.is_empty() { None } else { Some(value) })
}

/// Renders and parses a REQUIRED comma-separated address list into lettre
/// mailboxes. Empty tokens are skipped; a token that is not a valid address is
/// `invalid_args` naming the field.
fn render_addresses(
    field: &str,
    template: &str,
    ctx: &RenderCtx,
) -> Result<Vec<Mailbox>, CallError> {
    let rendered = render_raw(template, ctx).map_err(|e| render_err(field, e))?;
    parse_addresses(field, &rendered)
}

/// Renders one OPTIONAL address list (cc/bcc) leniently; an absent arg yields
/// an empty list.
fn render_optional_addresses(
    field: &str,
    template: Option<&str>,
    ctx: &RenderCtx,
) -> Result<Vec<Mailbox>, CallError> {
    match template {
        Some(t) => match render_optional(field, t, ctx)? {
            Some(rendered) => parse_addresses(field, &rendered),
            None => Ok(Vec::new()),
        },
        None => Ok(Vec::new()),
    }
}

/// Parses a comma-separated rendered address list into mailboxes.
fn parse_addresses(field: &str, rendered: &str) -> Result<Vec<Mailbox>, CallError> {
    let mut out = Vec::new();
    for token in rendered.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let mb = token.parse::<Mailbox>().map_err(|_| {
            CallError::new(
                CallErrorCode::InvalidArgs,
                format!("smtp {field} address is not valid: `{token}`"),
            )
        })?;
        out.push(mb);
    }
    Ok(out)
}

/// Renders and builds the message: from mailbox, recipient mailboxes, subject,
/// and the multipart/alternative-or-single body. At least one of `body_text`
/// or `body_html` must resolve (`invalid_args` otherwise); an empty `to` list
/// is `invalid_args`.
fn render_message(msg: &SmtpMessage, ctx: &RenderCtx) -> Result<BuiltMessage, CallError> {
    let from_email = render_raw(&msg.from_email, ctx).map_err(|e| render_err("from_email", e))?;
    let from_name = match &msg.from_name {
        Some(t) => render_optional("from_name", t, ctx)?,
        None => None,
    };
    let from_addr = from_email.parse::<Address>().map_err(|_| {
        CallError::new(
            CallErrorCode::InvalidArgs,
            format!("smtp from_email is not valid: `{from_email}`"),
        )
    })?;
    let from_mb = Mailbox::new(from_name, from_addr);

    let subject = render_raw(&msg.subject, ctx).map_err(|e| render_err("subject", e))?;
    let to = render_addresses("to", &msg.to, ctx)?;
    let cc = render_optional_addresses("cc", msg.cc.as_deref(), ctx)?;
    let bcc = render_optional_addresses("bcc", msg.bcc.as_deref(), ctx)?;
    if to.is_empty() {
        return Err(CallError::new(
            CallErrorCode::InvalidArgs,
            "smtp `to` has no recipients",
        ));
    }

    let body_text = match &msg.body_text {
        Some(t) => render_optional("body_text", t, ctx)?,
        None => None,
    };
    let body_html = match &msg.body_html {
        Some(t) => render_optional("body_html", t, ctx)?,
        None => None,
    };

    let mut builder = Message::builder().from(from_mb.clone()).subject(&subject);
    for mb in &to {
        builder = builder.to(mb.clone());
    }
    for mb in &cc {
        builder = builder.cc(mb.clone());
    }
    for mb in &bcc {
        builder = builder.bcc(mb.clone());
    }

    let email = match (body_text, body_html) {
        (Some(t), Some(h)) => builder.multipart(MultiPart::alternative_plain_html(t, h)),
        (Some(t), None) => builder.singlepart(SinglePart::plain(t)),
        (None, Some(h)) => builder.singlepart(SinglePart::html(h)),
        (None, None) => {
            return Err(CallError::new(
                CallErrorCode::InvalidArgs,
                "smtp message needs body_text or body_html",
            ));
        }
    }
    .map_err(|e| {
        CallError::new(
            CallErrorCode::Config,
            format!("smtp message build failed: {e}"),
        )
    })?;

    let to_strings: Vec<String> = to.iter().map(|mb| mb.email.to_string()).collect();
    let cc_strings: Vec<String> = cc.iter().map(|mb| mb.email.to_string()).collect();
    let bcc_strings: Vec<String> = bcc.iter().map(|mb| mb.email.to_string()).collect();
    let recipients: Vec<String> = to_strings
        .iter()
        .chain(cc_strings.iter())
        .chain(bcc_strings.iter())
        .cloned()
        .collect();
    let envelope = email.envelope().clone();
    Ok(BuiltMessage {
        from: from_mb.email.to_string(),
        subject,
        to: to_strings,
        cc: cc_strings,
        bcc: bcc_strings,
        recipients,
        envelope,
        formatted: email.formatted(),
    })
}

/// Renders the connection block into concrete parameters. `port` must parse to
/// a u16 and `use_tls` to a bool (config errors otherwise). The password is
/// resolved from secrets and never logged.
fn render_connection(spec: &SmtpSpec, ctx: &RenderCtx) -> Result<Connection, CallError> {
    let c = &spec.connection;
    let host = render_raw(&c.host, ctx).map_err(|e| render_err("host", e))?;
    let port_s = render_raw(&c.port, ctx).map_err(|e| render_err("port", e))?;
    let port: u16 = port_s.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::Config,
            format!("smtp port is not a valid port: `{port_s}`"),
        )
    })?;
    let use_tls_s = render_raw(&c.use_tls, ctx).map_err(|e| render_err("use_tls", e))?;
    let use_tls: bool = use_tls_s.trim().parse().map_err(|_| {
        CallError::new(
            CallErrorCode::Config,
            format!("smtp use_tls must be true or false: `{use_tls_s}`"),
        )
    })?;
    let username = render_conn_opt(c.username.as_deref(), ctx, "username")?;
    let password = render_conn_opt(c.password.as_deref(), ctx, "password")?;
    Ok(Connection {
        host,
        port,
        use_tls,
        username,
        password,
    })
}

/// Renders an optional connection field (username/password) presence-aware,
/// mirroring `render_optional` but over the account and secret namespaces.
/// Before rendering, the placeholders are scanned: if the template references
/// an `{{account.X}}` key absent from the account map, or a `{{secret.X}}` key
/// absent from the secrets map, the field is treated as absent (`Ok(None)`).
/// This lets the unauthenticated-relay setup that PUBLIC.md documents simply
/// omit the optional `username`/`password` account fields to mean "no
/// credentials", rather than forcing an empty placeholder.
///
/// A PRESENT reference whose value still fails to render is a hard config
/// error (a malformed placeholder stays loud); an empty render is treated as
/// absent. Note that a configured-but-unresolvable secret fails earlier, at
/// secret resolution, before this point is reached. Required connection fields
/// (host/port/use_tls) never use this path and stay strict.
fn render_conn_opt(
    template: Option<&str>,
    ctx: &RenderCtx,
    field: &str,
) -> Result<Option<String>, CallError> {
    match template {
        Some(t) => {
            for (ns, name) in placeholders(t).map_err(|e| render_err(field, e))? {
                let absent = match ns {
                    Namespace::Account => !ctx.account.contains_key(name.as_str()),
                    Namespace::Secret => !ctx.secrets.contains_key(name.as_str()),
                    _ => false,
                };
                if absent {
                    return Ok(None);
                }
            }
            let v = render_raw(t, ctx).map_err(|e| render_err(field, e))?;
            Ok(if v.is_empty() { None } else { Some(v) })
        }
        None => Ok(None),
    }
}

/// The dry-run JSON (spec 4.2): the rendered envelope (from/to/cc/bcc/subject),
/// no connection block, no secrets. A verify function (no message) reports the
/// endpoint only, from the non-secret account host/port.
fn dry_run_json(account: &BTreeMap<String, String>, built: &Option<BuiltMessage>) -> Value {
    match built {
        Some(m) => json!({
            "ok": true,
            "dry_run": true,
            "envelope": {
                "from": m.from,
                "to": m.to,
                "cc": m.cc,
                "bcc": m.bcc,
                "subject": m.subject,
            },
        }),
        None => {
            let host = account.get("host").cloned().unwrap_or_default();
            let port = account.get("port").cloned().unwrap_or_default();
            json!({
                "ok": true,
                "dry_run": true,
                "verify": true,
                "endpoint": format!("smtp://{host}:{port}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn spec_send() -> SmtpSpec {
        serde_yaml_ng::from_str(
            "connection:\n  host: \"{{account.host}}\"\n  port: \"{{account.port}}\"\n  use_tls: \"{{account.use_tls}}\"\n  username: \"{{account.username}}\"\n  password: \"{{secret.password}}\"\nmessage:\n  from_email: \"{{account.from_email}}\"\n  from_name: \"{{account.from_name}}\"\n  to: \"{{args.to}}\"\n  cc: \"{{args.cc}}\"\n  bcc: \"{{args.bcc}}\"\n  subject: \"{{args.subject}}\"\n  body_text: \"{{args.body_text}}\"\n  body_html: \"{{args.body_html}}\"\n",
        )
        .unwrap()
    }

    fn account() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("host".into(), "smtp.example.com".into()),
            ("port".into(), "587".into()),
            ("use_tls".into(), "true".into()),
            ("username".into(), "u".into()),
            ("from_email".into(), "a@b.c".into()),
            ("from_name".into(), "Alice".into()),
        ])
    }

    #[test]
    fn dry_run_renders_envelope_without_secrets() {
        let spec = spec_send();
        let args = json!({"to": "x@y.z, w@y.z", "cc": "c@y.z", "subject": "Hi", "body_text": "T"});
        let out = match build(
            &spec,
            &account(),
            &args,
            &BTreeMap::new(),
            Vec::new(),
            true,
            30,
        )
        .unwrap()
        {
            SmtpBuild::DryRun(v) => v,
            _ => panic!("expected dry-run"),
        };
        assert_eq!(out["dry_run"], json!(true));
        assert_eq!(out["envelope"]["from"], json!("a@b.c"));
        assert_eq!(out["envelope"]["to"], json!(["x@y.z", "w@y.z"]));
        assert_eq!(out["envelope"]["cc"], json!(["c@y.z"]));
        assert_eq!(out["envelope"]["subject"], json!("Hi"));
    }

    #[test]
    fn bad_address_is_invalid_args() {
        let spec = spec_send();
        let args = json!({"to": "not-an-email", "subject": "Hi", "body_text": "T"});
        let err = build(
            &spec,
            &account(),
            &args,
            &BTreeMap::new(),
            Vec::new(),
            true,
            30,
        )
        .unwrap_err();
        assert_eq!(err.code, CallErrorCode::InvalidArgs);
    }

    #[test]
    fn missing_body_is_invalid_args() {
        let spec = spec_send();
        let args = json!({"to": "x@y.z", "subject": "Hi"});
        let err = build(
            &spec,
            &account(),
            &args,
            &BTreeMap::new(),
            Vec::new(),
            true,
            30,
        )
        .unwrap_err();
        assert_eq!(err.code, CallErrorCode::InvalidArgs);
    }

    #[test]
    fn optional_fields_absent_when_args_missing() {
        // Cross-slice obligation 4: a send_email-shaped spec whose args lack
        // cc/bcc/body_html renders successfully with those fields absent, while
        // the required to/subject/from_email still resolve.
        let spec = spec_send();
        let args = json!({"to": "x@y.z", "subject": "Hi", "body_text": "T"});
        let out = match build(
            &spec,
            &account(),
            &args,
            &BTreeMap::new(),
            Vec::new(),
            true,
            30,
        )
        .unwrap()
        {
            SmtpBuild::DryRun(v) => v,
            _ => panic!("expected dry-run"),
        };
        assert_eq!(out["envelope"]["to"], json!(["x@y.z"]));
        assert_eq!(out["envelope"]["cc"], json!([]));
        assert_eq!(out["envelope"]["bcc"], json!([]));
        assert_eq!(out["envelope"]["subject"], json!("Hi"));
    }

    #[test]
    fn missing_required_arg_still_fails_loudly() {
        // The lenient rule must not mask a missing REQUIRED arg: `to` uses the
        // strict path, so omitting it is a render (config) error, not "absent".
        let spec = spec_send();
        let args = json!({"subject": "Hi", "body_text": "T"});
        let err = build(
            &spec,
            &account(),
            &args,
            &BTreeMap::new(),
            Vec::new(),
            true,
            30,
        )
        .unwrap_err();
        assert_eq!(err.code, CallErrorCode::Config);
    }
}
