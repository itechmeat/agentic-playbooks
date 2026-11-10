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
use apb_core::secret::{Redaction, Secret};
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{SmtpConnection, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use serde_json::{Value, json};

use crate::connector::result::{CallError, CallErrorCode, CallOk, redact_message};

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
    /// Sealed, exposed only at the AUTH call in `send`.
    password: Option<Secret>,
    timeout_sec: u64,
    /// `Some` for send, `None` for verify.
    message: Option<BuiltMessage>,
    /// The resolved secrets with their labels; every error message is
    /// scrubbed against these before it leaves the process.
    redactions: Vec<Redaction>,
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
    /// Lowers the timeout to at most `secs`.
    pub(crate) fn cap_timeout(&mut self, secs: u64) {
        self.timeout_sec = self.timeout_sec.min(secs);
    }

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
            let creds = Credentials::new(user.clone(), pass.expose_secret().to_string());
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
    /// `timeout`; a connect/DNS failure, or a send cut off without a reply,
    /// is `network`; an AUTH rejection is `auth`; any other protocol
    /// rejection is `service` with the SMTP reply carried in the message. Every message is scrubbed through the interim
    /// literal redaction so a resolved password can never leak.
    fn map_err(&self, stage: Stage, err: lettre::transport::smtp::Error) -> CallError {
        if err.is_timeout() {
            return self.redact(CallError::new(
                CallErrorCode::Timeout,
                "smtp operation timed out",
            ));
        }
        // At the send stage only the server's own reply (a 4xx or 5xx code)
        // says the message was refused; a connection lost meanwhile, after
        // DATA perhaps, leaves it unknown whether the server took it.
        let replied = err.is_transient() || err.is_permanent();
        let (code, prefix) = match stage {
            Stage::Connect => (CallErrorCode::Network, "smtp connection failed"),
            Stage::Auth => (CallErrorCode::Auth, "smtp authentication rejected"),
            Stage::Starttls => (CallErrorCode::Service, "smtp STARTTLS failed"),
            Stage::Send if replied => (CallErrorCode::Service, "smtp send rejected"),
            Stage::Send => (CallErrorCode::Network, "smtp send interrupted"),
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
    secrets: &BTreeMap<String, Secret>,
    redactions: Vec<Redaction>,
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
    password: Option<Secret>,
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
///
/// Renders and builds the message through the shared composer (D4). The
/// SMTP path keeps its exact shape: lettre strips the Bcc header (the
/// envelope still delivers to every bcc recipient), no forced Message-ID.
fn render_message(msg: &SmtpMessage, ctx: &RenderCtx) -> Result<BuiltMessage, CallError> {
    let composed = crate::connector::compose::compose(
        "smtp",
        msg,
        ctx,
        &crate::connector::compose::ComposeOpts::default(),
    )?;
    Ok(BuiltMessage {
        from: composed.from,
        subject: composed.subject,
        to: composed.to,
        cc: composed.cc,
        bcc: composed.bcc,
        recipients: composed.recipients,
        envelope: composed.envelope,
        formatted: composed.formatted,
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
    let password = render_conn_opt(c.password.as_deref(), ctx, "password")?.map(Secret::new);
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

    /// The derived `Debug` of a ready call masks its password and its
    /// redaction values: a `{:?}` in a log never carries the credential.
    #[test]
    fn debug_masks_the_password_and_the_redactions() {
        let call = SmtpCall {
            host: "smtp.example.test".into(),
            port: 587,
            use_tls: true,
            username: Some("bot".into()),
            password: Some(Secret::new("pw-canary-value")),
            timeout_sec: 5,
            message: None,
            redactions: vec![Redaction::new(Secret::new("pw-canary-value"), "SMTP_PASS")],
        };
        let shown = format!("{call:?}");
        assert!(
            !shown.contains("pw-canary-value"),
            "the password leaked through Debug"
        );
        assert!(shown.contains("smtp.example.test"));
    }

    fn spec_send() -> SmtpSpec {
        yaml_serde::from_str(
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
