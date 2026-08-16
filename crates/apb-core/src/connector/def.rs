//! Connector definition schema: the content of `connector.yaml` (spec
//! 2026-07-18-connectors-design, section 3.1).
//!
//! A connector links a playbook node to an external service through a
//! declarative manifest: an auth block, an optional inbound `webhook` block,
//! the account fields the connector needs, and a set of callable functions.
//! A function is exactly one of five kinds: HTTP, mock, smtp, imap, or
//! inbox. Besides the structural checks below, `from_yaml` also runs
//! `validate_templates`, which enforces the secret-placement policy over the
//! template placeholders parsed by `super::template` (`{{secret.*}}` only in
//! `auth`, the smtp password, the imap password, and the webhook block).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::common::{ConnectorError, validate_snake_name};

/// How the connector authenticates outgoing requests. The tag `kind`
/// selects the variant; each variant only accepts the fields it needs, so
/// (for example) a `header` auth block cannot also carry a `param`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthSpec {
    Header {
        header: String,
        value_template: String,
    },
    Query {
        param: String,
        value_template: String,
    },
    Basic {
        username_template: String,
        password_template: String,
    },
    Path {
        value_template: String,
    },
}

/// One field of the schema of an account for this connector (e.g.
/// `base-url`, `token`). `secret` fields may only ever hold an
/// `{{env.VAR}}` reference in account config (enforced by the config
/// loader, not here).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountField {
    pub name: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
}

/// A canned response for a `mock` function: no network call is made, the
/// function simply returns this status and body. Mocks are a permanent part
/// of the format (used by the fake test connector and by connector authors).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MockSpec {
    pub status: u16,
    pub body: serde_json::Value,
}

/// One authored example call for a function: `args` renders into the agent
/// instruction block after the description, and the validator checks it
/// against the function's `args_schema` (spec 4.4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExampleSpec {
    pub args: serde_json::Value,
    pub note: String,
}

fn default_timeout() -> u64 {
    30
}

/// The SMTP connection block: host/port/TLS plus optional credentials. Every
/// value is a template string. `password` is the one non-`auth` location the
/// secret-placement policy allows `{{secret.*}}` (spec 4.2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpConnection {
    pub host: String,
    pub port: String,
    pub use_tls: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
}

/// The SMTP message block. `to`/`cc`/`bcc` are comma-separated address lists.
/// Message templates follow function-body rules: only `account.*` and
/// `args.*`, never `secret.*`. `cc`/`bcc`/`from_name`/`body_text`/`body_html`
/// are optional and rendered leniently at call time (a missing optional arg
/// means "field absent", not an error - see `connector_smtp::render_optional`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpMessage {
    pub from_email: String,
    #[serde(default)]
    pub from_name: Option<String>,
    pub to: String,
    #[serde(default)]
    pub cc: Option<String>,
    #[serde(default)]
    pub bcc: Option<String>,
    pub subject: String,
    #[serde(default)]
    pub body_text: Option<String>,
    #[serde(default)]
    pub body_html: Option<String>,
}

/// The `smtp` function kind (spec 4.2). Exactly one of a connector's function
/// kinds. `verify: true` probes the connection and carries no `message`;
/// `verify: false` (the default) sends the `message`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SmtpSpec {
    pub connection: SmtpConnection,
    #[serde(default)]
    pub message: Option<SmtpMessage>,
    #[serde(default)]
    pub verify: bool,
}

/// The IMAP connection block: host/port/TLS/auth method plus credentials.
/// Every value is a template string. `password` is the one connection field
/// the secret-placement policy allows `{{secret.*}}` in (spec 3.2, wave 2).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImapConnection {
    pub host: String,
    pub port: String,
    pub use_tls: String,
    pub auth_method: String,
    pub username: String,
    pub password: String,
}

/// One IMAP operation a function can perform (spec 3.3, wave 2). Each op
/// accepts its own set of `params`, enforced by `validate_imap_params`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImapOp {
    Verify,
    ListFolders,
    Search,
    Fetch,
    SetFlags,
}

impl ImapOp {
    /// The snake_case name of the op, as it appears on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            ImapOp::Verify => "verify",
            ImapOp::ListFolders => "list_folders",
            ImapOp::Search => "search",
            ImapOp::Fetch => "fetch",
            ImapOp::SetFlags => "set_flags",
        }
    }
}

/// The `imap` function kind (spec 3.2, wave 2). Exactly one of a connector's
/// function kinds. `op` selects the IMAP operation; `params` carries the
/// op-specific arguments (spec 3.3), validated by `validate_imap_params`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImapSpec {
    pub connection: ImapConnection,
    pub op: ImapOp,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

/// The challenge dialect a provider performs before it will deliver
/// anything (spec 2026-08-16-webhook-ingest-design). One variant in v1:
/// `meta_hub`, the `hub.mode`/`hub.verify_token`/`hub.challenge` echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeDialect {
    MetaHub,
}

/// The signature scheme a provider signs deliveries with. One variant in v1:
/// HMAC-SHA256 over the raw body, hex encoded. An enum rather than a free
/// description, because a fully generic scheme language would be
/// speculation; a second provider adds a second variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureScheme {
    HmacSha256Hex,
}

/// How a delivery is authenticated. There is no unsigned mode: this block is
/// mandatory inside `webhook`, and `secret` is the one place besides `auth`,
/// the smtp password and the imap password where `{{secret.*}}` is allowed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureSpec {
    pub scheme: SignatureScheme,
    /// Header the provider carries the signature in, matched
    /// case-insensitively at ingest time. A literal, never a template.
    pub header: String,
    /// Literal prefix the header value carries before the hex digest, e.g.
    /// `sha256=`. Empty means the header is the bare digest.
    #[serde(default)]
    pub prefix: String,
    /// The shared secret, as a `{{secret.<field>}}` reference to a
    /// `secret: true` account field. Resolved at ingest time, never cached.
    pub secret: String,
}

/// The document-level `webhook:` block: everything the ingest listener needs
/// to accept a delivery for this connector (spec
/// 2026-08-16-webhook-ingest-design, "Connector schema: the webhook block").
///
/// A connector carrying this block declares inbox functions, and a connector
/// declaring inbox functions carries this block; `from_yaml` enforces both
/// directions, since a manifest violating either is broken for every
/// consumer and not only for a playbook that happens to reference it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookSpec {
    /// Absent when the provider performs no verification handshake, in which
    /// case a GET to the hook path is a flat 404.
    #[serde(default)]
    pub challenge: Option<ChallengeDialect>,
    /// Required exactly when `challenge` is set; a `{{secret.<field>}}`
    /// reference like `signature.secret`.
    #[serde(default)]
    pub verify_token: Option<String>,
    pub signature: SignatureSpec,
    /// Dot path into the delivery body yielding the provider's own id for
    /// this event, used for dedupe. Absent means dedupe falls back to the
    /// SHA-256 of the raw body.
    #[serde(default)]
    pub dedupe_path: Option<String>,
}

/// One inbox operation a function can perform (spec
/// 2026-08-16-webhook-ingest-design). All three are local reads or cursor
/// moves against `apb_core::connector::inbox`; none touches the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxOp {
    /// Return pending events without moving the cursor.
    Read,
    /// Advance a consumer cursor after processing.
    Ack,
    /// Return the pending count only. The natural probe for an
    /// ingest-only connector, which has no outbound function to healthcheck.
    PeekDepth,
}

impl InboxOp {
    /// The snake_case name of the op, as it appears on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            InboxOp::Read => "read",
            InboxOp::Ack => "ack",
            InboxOp::PeekDepth => "peek_depth",
        }
    }
}

/// The `inbox` function kind (spec 2026-08-16-webhook-ingest-design). The
/// fifth and last kind a function may be. It carries no connection block:
/// the store it reads is derived from the connector name and the selected
/// account, and the arguments come from `args_schema` like any other call.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InboxSpec {
    pub op: InboxOp,
}

/// One callable function of the connector: exactly one of an HTTP call
/// (`method` + `url`, optionally `query`/`headers`/`body`/`body_form`), a
/// `mock` (canned response, no network), an `smtp` block, an `imap` block, or
/// an `inbox` block (a read of the local inbound-event store). `from_yaml`
/// enforces the exactly-one rule, never more and never none.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub deprecated: Option<String>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    /// Form-encoded request body (spec 2026-07-22-official-connectors-wave-3,
    /// section 4.1): a string-to-template map rendered to an
    /// `application/x-www-form-urlencoded` body. Mutually exclusive with
    /// `body`, and only valid on body-carrying methods (POST, PUT, PATCH,
    /// DELETE). Values follow function-body template rules (`account.*` and
    /// `args.*` only, never `secret.*`).
    #[serde(default)]
    pub body_form: BTreeMap<String, String>,
    #[serde(default)]
    pub args_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub examples: Vec<ExampleSpec>,
    #[serde(default)]
    pub response_pick: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
    #[serde(default)]
    pub mock: Option<MockSpec>,
    #[serde(default)]
    pub smtp: Option<SmtpSpec>,
    #[serde(default)]
    pub imap: Option<ImapSpec>,
    #[serde(default)]
    pub inbox: Option<InboxSpec>,
}

impl FunctionSpec {
    /// A mock function returns a canned response instead of making an HTTP
    /// call.
    pub fn is_mock(&self) -> bool {
        self.mock.is_some()
    }

    /// An smtp function sends email or probes an SMTP server natively (spec
    /// 4.2), instead of making an HTTP call or returning a mock.
    pub fn is_smtp(&self) -> bool {
        self.smtp.is_some()
    }

    /// An imap function reads mailboxes natively (spec 3.2, wave 2), instead
    /// of making an HTTP call, returning a mock, or sending mail over smtp.
    pub fn is_imap(&self) -> bool {
        self.imap.is_some()
    }

    /// An inbox function reads the local inbound-event store (spec
    /// 2026-08-16-webhook-ingest-design) instead of making an HTTP call,
    /// returning a mock, sending mail, or opening a mailbox.
    pub fn is_inbox(&self) -> bool {
        self.inbox.is_some()
    }
}

/// Connector-level false-success predicate (spec
/// 2026-07-22-official-connectors-wave-3, section 4.2): after a 2xx response
/// on any HTTP function of the connector, the engine evaluates `path` against
/// the parsed JSON body; when the value there equals `equals` (JSON
/// equality), the call maps to a `service` error whose message is the string
/// at `message_path` (a fixed fallback when absent). `path` and
/// `message_path` are dot-separated field chains over objects, like
/// `response_pick` paths without array semantics. Mock, smtp, and imap
/// functions ignore the block; non-2xx statuses keep the status-table
/// mapping.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorWhen {
    pub path: String,
    pub equals: serde_json::Value,
    #[serde(default)]
    pub message_path: Option<String>,
}

/// The content of `connector.yaml`: the whole connector manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorDoc {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub healthcheck: Option<String>,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
    /// Declares when a 2xx response is actually an error (spec
    /// 2026-07-22-official-connectors-wave-3, section 4.2). Connector-level:
    /// applies to every HTTP function.
    #[serde(default)]
    pub error_when: Option<ErrorWhen>,
    /// Inbound delivery contract (spec 2026-08-16-webhook-ingest-design).
    /// Present exactly when the connector declares `inbox` functions.
    #[serde(default)]
    pub webhook: Option<WebhookSpec>,
    #[serde(default)]
    pub account_fields: Vec<AccountField>,
    #[serde(default)]
    pub functions: Vec<FunctionSpec>,
}

impl ConnectorDoc {
    /// Parses and structurally validates a `connector.yaml` document.
    /// `expected_name` is the connector folder name; the document's `name`
    /// field must equal it (a mismatch, like a mismatched profile
    /// directory, is a validation error rather than a silent rename).
    ///
    /// Validation rules, each failing as `ConnectorError::Invalid` naming
    /// the offending item:
    /// 1. `name` passes `validate_profile_name` and equals `expected_name`.
    /// 2. `version` is non-empty.
    /// 3. Function names pass `validate_snake_name` and are unique.
    /// 4. Each function is HTTP xor mock.
    /// 5. `healthcheck`, when present, names an existing function.
    /// 6. Account field names pass `validate_snake_name` and are unique.
    /// 7. `error_when`, when present, has a non-empty `path` and the
    ///    connector has at least one HTTP function (the block applies only
    ///    to HTTP responses; the spec's "warn" on the no-HTTP case is a hard
    ///    error here because the validator has no warning channel and the
    ///    combination has no legitimate use).
    pub fn from_yaml(yaml: &str, expected_name: &str) -> Result<Self, ConnectorError> {
        let doc: ConnectorDoc =
            serde_yaml_ng::from_str(yaml).map_err(|e| ConnectorError::Yaml(e.to_string()))?;

        crate::profile::validate_profile_name(&doc.name).map_err(ConnectorError::Invalid)?;
        if doc.name != expected_name {
            return Err(ConnectorError::Invalid(format!(
                "connector name `{}` does not match expected `{expected_name}`",
                doc.name
            )));
        }
        if doc.version.trim().is_empty() {
            return Err(ConnectorError::Invalid(format!(
                "connector `{}` has an empty version",
                doc.name
            )));
        }

        let mut seen_functions = std::collections::HashSet::new();
        for f in &doc.functions {
            validate_snake_name(&f.name).map_err(ConnectorError::Invalid)?;
            if !seen_functions.insert(f.name.as_str()) {
                return Err(ConnectorError::Invalid(format!(
                    "duplicate function name `{}`",
                    f.name
                )));
            }

            let is_http = f.method.is_some() || f.url.is_some();
            let is_mock = f.mock.is_some();
            let is_smtp = f.smtp.is_some();
            let is_imap = f.imap.is_some();
            let is_inbox = f.inbox.is_some();
            match [is_http, is_mock, is_smtp, is_imap, is_inbox]
                .iter()
                .filter(|set| **set)
                .count()
            {
                0 => {
                    return Err(ConnectorError::Invalid(format!(
                        "function `{}` is not an HTTP call (method + url), a mock, an smtp block, an imap block, or an inbox block",
                        f.name
                    )));
                }
                1 => {
                    if is_http {
                        if f.method.is_none() || f.url.is_none() {
                            return Err(ConnectorError::Invalid(format!(
                                "function `{}` must set both `method` and `url`",
                                f.name
                            )));
                        }
                        validate_body_form_shape(f)?;
                    } else if is_mock {
                        if !f.query.is_empty() || f.body.is_some() || !f.body_form.is_empty() {
                            return Err(ConnectorError::Invalid(format!(
                                "mock function `{}` must not set `query`, `body`, or `body_form`",
                                f.name
                            )));
                        }
                    } else if is_smtp {
                        validate_smtp_shape(f)?;
                    } else if is_imap {
                        validate_imap_shape(f)?;
                    } else {
                        validate_inbox_shape(f)?;
                    }
                }
                _ => {
                    return Err(ConnectorError::Invalid(format!(
                        "function `{}` must be exactly one of: an HTTP call, a mock, an smtp block, an imap block, or an inbox block",
                        f.name
                    )));
                }
            }

            // response_pick projects a JSON document (spec 4.5). An HTTP
            // function projects the response body and an inbox function
            // projects the fixed envelope it returns; a mock returns an
            // authored payload, an smtp function a send/verify receipt, and
            // an imap function its own op result, so a projection on any of
            // those three is meaningless. The inbox case is not merely
            // tolerated: the official-connector gate requires every
            // read_only function that is not smtp or imap to carry one.
            if !f.response_pick.is_empty() && (is_mock || is_smtp || is_imap) {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` sets response_pick but is neither an HTTP nor an inbox function; response_pick is only valid on those two kinds",
                    f.name
                )));
            }
        }

        if let Some(hc) = &doc.healthcheck
            && doc.function(hc).is_none()
        {
            return Err(ConnectorError::Invalid(format!(
                "healthcheck names unknown function `{hc}`"
            )));
        }

        if let Some(ew) = &doc.error_when {
            if ew.path.trim().is_empty() {
                return Err(ConnectorError::Invalid(
                    "error_when requires a non-empty `path`".to_string(),
                ));
            }
            // The predicate only ever runs against HTTP responses; a
            // connector with the block and zero HTTP functions is an
            // authoring error (spec wave-3 4.2 says "warn", hardened to an
            // error here since the validator has no warning channel).
            let has_http = doc.functions.iter().any(|f| f.method.is_some());
            if !has_http {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` declares error_when but has no HTTP functions",
                    doc.name
                )));
            }
        }

        if let Some(hook) = &doc.webhook {
            validate_webhook_shape(&doc.name, hook)?;
        }

        // The webhook block and the inbox functions are two halves of one
        // contract: the block says how a delivery is accepted, the functions
        // say how it is read back. A manifest with one and not the other is
        // broken for every consumer (install, listing, run snapshot), so it
        // is refused here rather than deferred to the playbook validator,
        // which never opens a connector manifest. The playbook-facing half
        // (a node granting inbox functions of a connector that lost its
        // block) is validator rule V42.
        let inbox_names = doc.inbox_functions();
        match (doc.webhook.is_some(), inbox_names.is_empty()) {
            (true, true) => {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` declares a webhook block but no inbox function to read the delivered events",
                    doc.name
                )));
            }
            (false, false) => {
                return Err(ConnectorError::Invalid(format!(
                    "connector `{}` declares inbox function(s) {} but no webhook block, so nothing can ever be delivered",
                    doc.name,
                    inbox_names.join(", ")
                )));
            }
            _ => {}
        }

        let mut seen_fields = std::collections::HashSet::new();
        for field in &doc.account_fields {
            validate_snake_name(&field.name).map_err(ConnectorError::Invalid)?;
            if !seen_fields.insert(field.name.as_str()) {
                return Err(ConnectorError::Invalid(format!(
                    "duplicate account field name `{}`",
                    field.name
                )));
            }
        }

        validate_templates(&doc)?;
        validate_examples(&doc)?;

        Ok(doc)
    }

    /// Looks up a function by name.
    pub fn function(&self, name: &str) -> Option<&FunctionSpec> {
        self.functions.iter().find(|f| f.name == name)
    }

    /// Names of functions marked `read_only: true`, in manifest order. Used
    /// to resolve the `functions: read_only` grant shorthand (spec section
    /// 5).
    pub fn read_only_functions(&self) -> Vec<String> {
        self.functions
            .iter()
            .filter(|f| f.read_only)
            .map(|f| f.name.clone())
            .collect()
    }

    /// Names of `inbox` functions, in manifest order. Used by the
    /// mutual-requirement rule here, by the playbook validator (V42, V43),
    /// and by the doctor and dashboard to decide whether a connector is
    /// ingest-capable.
    pub fn inbox_functions(&self) -> Vec<String> {
        self.functions
            .iter()
            .filter(|f| f.is_inbox())
            .map(|f| f.name.clone())
            .collect()
    }

    /// Names of account fields marked `secret: true`, in manifest order.
    pub fn secret_fields(&self) -> Vec<String> {
        self.account_fields
            .iter()
            .filter(|f| f.secret)
            .map(|f| f.name.clone())
            .collect()
    }
}

/// `body_form` shape rules (spec 2026-07-22-official-connectors-wave-3,
/// section 4.1): mutually exclusive with `body`, and allowed only on
/// body-carrying methods (POST, PUT, PATCH, DELETE, case-insensitively).
/// `body` itself keeps no method restriction (out of scope for wave 3). Only
/// called for HTTP functions; the mock/smtp/imap shape checks reject
/// `body_form` themselves.
fn validate_body_form_shape(f: &FunctionSpec) -> Result<(), ConnectorError> {
    if f.body_form.is_empty() {
        return Ok(());
    }
    if f.body.is_some() {
        return Err(ConnectorError::Invalid(format!(
            "function `{}` must not set both `body` and `body_form`",
            f.name
        )));
    }
    let method = f.method.as_deref().unwrap_or("");
    let carries_body = matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH" | "DELETE"
    );
    if !carries_body {
        return Err(ConnectorError::Invalid(format!(
            "function `{}` sets `body_form` on method `{method}`; body_form is only valid on POST, PUT, PATCH, or DELETE",
            f.name
        )));
    }
    Ok(())
}

/// SMTP-internal shape rules (spec 4.2): a `verify: true` function carries no
/// `message`; a `verify: false` function must carry one. An smtp function must
/// not also carry `query` or `body` (those are HTTP-only).
fn validate_smtp_shape(f: &FunctionSpec) -> Result<(), ConnectorError> {
    let smtp = f.smtp.as_ref().expect("caller checked is_smtp");
    if !f.query.is_empty() || f.body.is_some() || !f.body_form.is_empty() {
        return Err(ConnectorError::Invalid(format!(
            "smtp function `{}` must not set `query`, `body`, or `body_form`",
            f.name
        )));
    }
    match (smtp.verify, smtp.message.is_some()) {
        (true, true) => Err(ConnectorError::Invalid(format!(
            "smtp function `{}` sets `verify: true` and must not carry a `message`",
            f.name
        ))),
        (false, false) => Err(ConnectorError::Invalid(format!(
            "smtp function `{}` must carry a `message` (or set `verify: true`)",
            f.name
        ))),
        _ => Ok(()),
    }
}

/// IMAP-internal shape rules (spec 3.2, wave 2): an imap function must not
/// also carry `query`, `body`, or `headers` (those are HTTP-only), and its
/// `params` must match what its `op` accepts (spec 3.3).
fn validate_imap_shape(f: &FunctionSpec) -> Result<(), ConnectorError> {
    let imap = f.imap.as_ref().expect("caller checked is_imap");
    if !f.query.is_empty() {
        return Err(ConnectorError::Invalid(format!(
            "imap function `{}` must not set `query`",
            f.name
        )));
    }
    if f.body.is_some() {
        return Err(ConnectorError::Invalid(format!(
            "imap function `{}` must not set `body`",
            f.name
        )));
    }
    if !f.body_form.is_empty() {
        return Err(ConnectorError::Invalid(format!(
            "imap function `{}` must not set `body_form`",
            f.name
        )));
    }
    if !f.headers.is_empty() {
        return Err(ConnectorError::Invalid(format!(
            "imap function `{}` must not set `headers`",
            f.name
        )));
    }
    validate_imap_params(&f.name, imap.op, &imap.params)
}

/// Inbox-internal shape rules (spec 2026-08-16-webhook-ingest-design): an
/// inbox function must not carry `query`, `body`, `body_form`, or `headers`
/// (those are HTTP-only), and it has no connection block to check. Its
/// arguments are validated at call time against `args_schema` like every
/// other kind, so nothing about `op` constrains them here.
fn validate_inbox_shape(f: &FunctionSpec) -> Result<(), ConnectorError> {
    for (present, field) in [
        (!f.query.is_empty(), "query"),
        (f.body.is_some(), "body"),
        (!f.body_form.is_empty(), "body_form"),
        (!f.headers.is_empty(), "headers"),
    ] {
        if present {
            return Err(ConnectorError::Invalid(format!(
                "inbox function `{}` must not set `{field}`",
                f.name
            )));
        }
    }
    Ok(())
}

/// Validates `params` against the allowed and required keys of `op` (spec
/// 3.3): `verify` and `list_folders` take no params; `search` requires
/// `folder` and `limit` with optional `unread_only`, `from_contains`,
/// `subject_contains`, `since_days`; `fetch` requires `folder` and `uid`;
/// `set_flags` requires `folder`, `uids`, and `seen`. Any other key is an
/// error, and any missing required key is an error.
fn validate_imap_params(
    function_name: &str,
    op: ImapOp,
    params: &BTreeMap<String, String>,
) -> Result<(), ConnectorError> {
    let (required, optional): (&[&str], &[&str]) = match op {
        ImapOp::Verify | ImapOp::ListFolders => (&[], &[]),
        ImapOp::Search => (
            &["folder", "limit"],
            &[
                "unread_only",
                "from_contains",
                "subject_contains",
                "since_days",
            ],
        ),
        ImapOp::Fetch => (&["folder", "uid"], &[]),
        ImapOp::SetFlags => (&["folder", "uids", "seen"], &[]),
    };

    for key in params.keys() {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            return Err(ConnectorError::Invalid(format!(
                "imap function `{function_name}` op `{}` does not accept param `{key}`",
                op.as_str()
            )));
        }
    }
    for key in required {
        if !params.contains_key(*key) {
            return Err(ConnectorError::Invalid(format!(
                "imap function `{function_name}` op `{}` is missing required param `{key}`",
                op.as_str()
            )));
        }
    }
    Ok(())
}

/// Webhook-block shape rules (spec 2026-08-16-webhook-ingest-design). The
/// signature block is mandatory by the struct itself; what is checked here
/// is that the optional parts are coherent: a challenge dialect needs a
/// token and a token needs a dialect, the header names something, and a
/// declared dedupe path is a real path.
fn validate_webhook_shape(connector: &str, hook: &WebhookSpec) -> Result<(), ConnectorError> {
    if hook.signature.header.trim().is_empty() {
        return Err(ConnectorError::Invalid(format!(
            "connector `{connector}` webhook signature needs a non-empty `header`"
        )));
    }
    match (hook.challenge.is_some(), hook.verify_token.is_some()) {
        (true, false) => {
            return Err(ConnectorError::Invalid(format!(
                "connector `{connector}` webhook declares a challenge dialect and must carry a `verify_token`"
            )));
        }
        (false, true) => {
            return Err(ConnectorError::Invalid(format!(
                "connector `{connector}` webhook carries a `verify_token` but declares no `challenge` dialect to use it in"
            )));
        }
        _ => {}
    }
    if let Some(path) = &hook.dedupe_path
        && (path.trim().is_empty() || path.split('.').any(|s| s.trim().is_empty()))
    {
        return Err(ConnectorError::Invalid(format!(
            "connector `{connector}` webhook `dedupe_path` must be a dot path with non-empty segments"
        )));
    }
    Ok(())
}

/// The two `AccountField` names, split by secrecy: non-secret names that
/// `{{account.*}}` placeholders may reference, and secret names that
/// `{{secret.*}}` placeholders may reference.
struct FieldNames<'a> {
    account: std::collections::HashSet<&'a str>,
    secret: std::collections::HashSet<&'a str>,
}

impl<'a> FieldNames<'a> {
    fn from_fields(fields: &'a [AccountField]) -> Self {
        let mut account = std::collections::HashSet::new();
        let mut secret = std::collections::HashSet::new();
        for field in fields {
            if field.secret {
                secret.insert(field.name.as_str());
            } else {
                account.insert(field.name.as_str());
            }
        }
        FieldNames { account, secret }
    }

    /// Checks that an `Account` or `Secret` placeholder names a declared
    /// account field of the matching secrecy. `Args` is always allowed here;
    /// callers reject `Args` themselves where it is out of place (auth).
    fn check(
        &self,
        ns: crate::connector::template::Namespace,
        name: &str,
    ) -> Result<(), ConnectorError> {
        use crate::connector::template::Namespace;
        match ns {
            Namespace::Account => {
                if !self.account.contains(name) {
                    return Err(ConnectorError::Invalid(format!(
                        "placeholder `{{{{account.{name}}}}}` names an unknown or secret account field"
                    )));
                }
            }
            Namespace::Secret => {
                if !self.secret.contains(name) {
                    return Err(ConnectorError::Invalid(format!(
                        "placeholder `{{{{secret.{name}}}}}` names an unknown or non-secret account field"
                    )));
                }
            }
            Namespace::Args | Namespace::Auth => {}
        }
        Ok(())
    }
}

/// Validates every template placeholder in the connector against the
/// secret-placement policy: `{{secret.*}}` is allowed only inside `auth`
/// (any occurrence in a function's `url`, `query`, or `body` is rejected);
/// `{{args.*}}` and bare `{{args}}` are not allowed inside `auth`; every
/// `{{account.*}}` / `{{secret.*}}` placeholder must name a declared account
/// field of the matching secrecy. Called at the end of `ConnectorDoc::from_yaml`.
pub fn validate_templates(doc: &ConnectorDoc) -> Result<(), ConnectorError> {
    use crate::connector::template::{Namespace, placeholders};

    let fields = FieldNames::from_fields(&doc.account_fields);
    let is_path_auth = matches!(doc.auth, Some(AuthSpec::Path { .. }));

    for f in &doc.functions {
        if let Some(url) = &f.url {
            let mut auth_markers = 0;
            for (ns, name) in placeholders(url)? {
                if ns == Namespace::Auth {
                    auth_markers += 1;
                    continue;
                }
                reject_secret(ns, &format!("function `{}` url", f.name))?;
                fields.check(ns, &name)?;
            }
            if is_path_auth && auth_markers != 1 {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` url must contain the `{{{{auth}}}}` placeholder exactly once for path auth (found {auth_markers})",
                    f.name
                )));
            }
            if !is_path_auth && auth_markers > 0 {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` url uses `{{{{auth}}}}` but the connector does not use path auth",
                    f.name
                )));
            }
        }
        for value in f.query.values() {
            for (ns, name) in placeholders(value)? {
                reject_auth(ns, &format!("function `{}` query", f.name))?;
                reject_secret(ns, &format!("function `{}` query", f.name))?;
                fields.check(ns, &name)?;
            }
        }
        for value in f.headers.values() {
            for (ns, name) in placeholders(value)? {
                reject_auth(ns, &format!("function `{}` headers", f.name))?;
                reject_secret(ns, &format!("function `{}` headers", f.name))?;
                fields.check(ns, &name)?;
            }
        }
        // body_form values follow function-body rules (spec
        // 2026-07-22-official-connectors-wave-3, section 4.1), walked the same
        // way `query` values are.
        for value in f.body_form.values() {
            for (ns, name) in placeholders(value)? {
                reject_auth(ns, &format!("function `{}` body_form", f.name))?;
                reject_secret(ns, &format!("function `{}` body_form", f.name))?;
                fields.check(ns, &name)?;
            }
        }
        if let Some(body) = &f.body {
            validate_body_templates(body, &f.name, &fields)?;
        }
        if let Some(smtp) = &f.smtp {
            validate_smtp_templates(smtp, &f.name, &fields)?;
        }
        if let Some(imap) = &f.imap {
            validate_imap_templates(imap, &f.name, &fields)?;
        }
    }

    if let Some(hook) = &doc.webhook {
        validate_webhook_templates(hook, &doc.name, &fields)?;
    }

    if let Some(auth) = &doc.auth {
        for template in auth_templates(auth) {
            for (ns, name) in placeholders(template)? {
                reject_auth(ns, "auth template")?;
                if ns == Namespace::Args {
                    return Err(ConnectorError::Invalid(
                        "args placeholders are not allowed in auth templates".to_string(),
                    ));
                }
                fields.check(ns, &name)?;
            }
        }
    }

    Ok(())
}

/// Validates the placeholders in an `smtp` function (spec 4.2). The
/// `connection` block follows auth-adjacent rules: `account.*` is allowed and
/// `secret.*` only via the `password` field; `args.*` is rejected. The
/// `message` block follows function-body rules: `account.*` and `args.*` only,
/// `secret.*` rejected. The reserved `{{auth}}` marker is a function-url-only
/// construct and is rejected in both blocks.
fn validate_smtp_templates(
    smtp: &SmtpSpec,
    function_name: &str,
    fields: &FieldNames,
) -> Result<(), ConnectorError> {
    use crate::connector::template::{Namespace, placeholders};

    // Connection: account allowed, args rejected, secret only via `password`.
    let conn = &smtp.connection;
    let non_password: [&str; 3] = [
        conn.host.as_str(),
        conn.port.as_str(),
        conn.use_tls.as_str(),
    ];
    for template in non_password.iter().copied().chain(conn.username.as_deref()) {
        for (ns, name) in placeholders(template)? {
            reject_auth(ns, &format!("function `{function_name}` smtp connection"))?;
            if ns == Namespace::Args {
                return Err(ConnectorError::Invalid(format!(
                    "args placeholders are not allowed in smtp connection of function `{function_name}`"
                )));
            }
            reject_secret(ns, &format!("function `{function_name}` smtp connection"))?;
            fields.check(ns, &name)?;
        }
    }
    if let Some(password) = &conn.password {
        for (ns, name) in placeholders(password)? {
            reject_auth(
                ns,
                &format!("function `{function_name}` smtp connection password"),
            )?;
            if ns == Namespace::Args {
                return Err(ConnectorError::Invalid(format!(
                    "args placeholders are not allowed in smtp connection password of function `{function_name}`"
                )));
            }
            fields.check(ns, &name)?;
        }
    }

    // Message: account/args only, secret rejected.
    if let Some(msg) = &smtp.message {
        let strings: [Option<&str>; 8] = [
            Some(msg.from_email.as_str()),
            msg.from_name.as_deref(),
            Some(msg.to.as_str()),
            msg.cc.as_deref(),
            msg.bcc.as_deref(),
            Some(msg.subject.as_str()),
            msg.body_text.as_deref(),
            msg.body_html.as_deref(),
        ];
        for template in strings.into_iter().flatten() {
            for (ns, name) in placeholders(template)? {
                reject_auth(ns, &format!("function `{function_name}` smtp message"))?;
                reject_secret(ns, &format!("function `{function_name}` smtp message"))?;
                fields.check(ns, &name)?;
            }
        }
    }
    Ok(())
}

/// Validates the placeholders in an `imap` function (spec 3.2, wave 2). Every
/// connection field other than `password`, and every `params` value, follows
/// function-body rules: `account.*` and `args.*` are allowed, `secret.*` is
/// rejected. `password` mirrors `SmtpConnection.password` exactly: `args.*`
/// is rejected, `account.*` and `secret.*` are allowed (it is the one field
/// the secret-placement policy allows `{{secret.*}}` in). The reserved
/// `{{auth}}` marker is a function-url-only construct and is rejected
/// everywhere in the imap block.
fn validate_imap_templates(
    imap: &ImapSpec,
    function_name: &str,
    fields: &FieldNames,
) -> Result<(), ConnectorError> {
    use crate::connector::template::{Namespace, placeholders};

    let conn = &imap.connection;
    let non_password: [&str; 5] = [
        conn.host.as_str(),
        conn.port.as_str(),
        conn.use_tls.as_str(),
        conn.auth_method.as_str(),
        conn.username.as_str(),
    ];
    for template in non_password {
        for (ns, name) in placeholders(template)? {
            reject_auth(ns, &format!("function `{function_name}` imap connection"))?;
            reject_secret(ns, &format!("function `{function_name}` imap connection"))?;
            fields.check(ns, &name)?;
        }
    }
    for (ns, name) in placeholders(conn.password.as_str())? {
        reject_auth(
            ns,
            &format!("function `{function_name}` imap connection password"),
        )?;
        if ns == Namespace::Args {
            return Err(ConnectorError::Invalid(format!(
                "args placeholders are not allowed in imap connection password of function `{function_name}`"
            )));
        }
        fields.check(ns, &name)?;
    }

    for value in imap.params.values() {
        for (ns, name) in placeholders(value)? {
            reject_auth(ns, &format!("function `{function_name}` imap params"))?;
            reject_secret(ns, &format!("function `{function_name}` imap params"))?;
            fields.check(ns, &name)?;
        }
    }
    Ok(())
}

/// Validates the placeholders in the `webhook` block (spec
/// 2026-08-16-webhook-ingest-design). `verify_token` and `signature.secret`
/// are the third deliberate exemption to the auth-only secret rule, after
/// `SmtpConnection.password` and `ImapConnection.password`: `account.*` and
/// `secret.*` are allowed there, `args.*` is not (no call args exist at
/// ingest time), and the reserved `{{auth}}` marker is a function-url-only
/// construct rejected everywhere here. Every other field of the block is a
/// literal the ingest listener compares byte for byte, so any placeholder in
/// it is an authoring error rather than a value that would be rendered.
fn validate_webhook_templates(
    hook: &WebhookSpec,
    connector: &str,
    fields: &FieldNames,
) -> Result<(), ConnectorError> {
    use crate::connector::template::{Namespace, placeholders};

    let secret_bearing = [
        Some(hook.signature.secret.as_str()),
        hook.verify_token.as_deref(),
    ];
    for template in secret_bearing.into_iter().flatten() {
        for (ns, name) in placeholders(template)? {
            reject_auth(ns, &format!("connector `{connector}` webhook"))?;
            if ns == Namespace::Args {
                return Err(ConnectorError::Invalid(format!(
                    "args placeholders are not allowed in the webhook block of connector `{connector}`"
                )));
            }
            fields.check(ns, &name)?;
        }
    }

    let literals = [
        Some(hook.signature.header.as_str()),
        Some(hook.signature.prefix.as_str()),
        hook.dedupe_path.as_deref(),
    ];
    for template in literals.into_iter().flatten() {
        if !placeholders(template)?.is_empty() {
            return Err(ConnectorError::Invalid(format!(
                "connector `{connector}` webhook `header`, `prefix` and `dedupe_path` are literals and must not contain placeholders"
            )));
        }
    }
    Ok(())
}

/// Validates that every function example's `args` conform to that function's
/// `args_schema` (spec 4.4), so an example cannot drift from the schema
/// silently. Functions without an `args_schema` are skipped (nothing to
/// check against).
fn validate_examples(doc: &ConnectorDoc) -> Result<(), ConnectorError> {
    for f in &doc.functions {
        if f.examples.is_empty() {
            continue;
        }
        let Some(schema) = &f.args_schema else {
            continue;
        };
        let validator = jsonschema::validator_for(schema).map_err(|e| {
            ConnectorError::Invalid(format!(
                "function `{}` args_schema is not a valid JSON schema: {e}",
                f.name
            ))
        })?;
        for (i, ex) in f.examples.iter().enumerate() {
            if let Some(err) = validator.iter_errors(&ex.args).next() {
                return Err(ConnectorError::Invalid(format!(
                    "function `{}` example {} args fail its args_schema: {err}",
                    f.name,
                    i + 1
                )));
            }
        }
    }
    Ok(())
}

/// Errors if `ns` is `Auth`: the `{{auth}}` marker is confined to a
/// function `url`, so any occurrence found while walking a query, body, or
/// auth template is a hard error naming where it was found (spec 4.3).
fn reject_auth(
    ns: crate::connector::template::Namespace,
    where_: &str,
) -> Result<(), ConnectorError> {
    if ns == crate::connector::template::Namespace::Auth {
        return Err(ConnectorError::Invalid(format!(
            "the `{{{{auth}}}}` placeholder is allowed only in a function url ({where_})"
        )));
    }
    Ok(())
}

/// Errors if `ns` is `Secret`: secret placeholders are confined to `auth`, so
/// any occurrence found while walking a function's `url`/`query`/`body` is a
/// hard error naming where it was found.
fn reject_secret(
    ns: crate::connector::template::Namespace,
    where_: &str,
) -> Result<(), ConnectorError> {
    if ns == crate::connector::template::Namespace::Secret {
        return Err(ConnectorError::Invalid(format!(
            "secret placeholders are allowed only in auth ({where_})"
        )));
    }
    Ok(())
}

/// Walks a `body` JSON value, validating the placeholders in every string
/// leaf (arrays and objects recurse; non-string scalars carry no
/// placeholders).
fn validate_body_templates(
    value: &serde_json::Value,
    function_name: &str,
    fields: &FieldNames,
) -> Result<(), ConnectorError> {
    use crate::connector::template::placeholders;

    match value {
        serde_json::Value::String(s) => {
            for (ns, name) in placeholders(s)? {
                reject_auth(ns, &format!("function `{function_name}` body"))?;
                reject_secret(ns, &format!("function `{function_name}` body"))?;
                fields.check(ns, &name)?;
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for item in items {
                validate_body_templates(item, function_name, fields)?;
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            for v in map.values() {
                validate_body_templates(v, function_name, fields)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The template strings carried by an `AuthSpec`, in a uniform list
/// regardless of variant.
fn auth_templates(auth: &AuthSpec) -> Vec<&str> {
    match auth {
        AuthSpec::Header { value_template, .. } => vec![value_template.as_str()],
        AuthSpec::Query { value_template, .. } => vec![value_template.as_str()],
        AuthSpec::Basic {
            username_template,
            password_template,
        } => vec![username_template.as_str(), password_template.as_str()],
        AuthSpec::Path { value_template } => vec![value_template.as_str()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JIRA_YAML: &str = r#"
name: jira
version: 0.1.0
healthcheck: ping
auth:
  kind: header
  header: Authorization
  value_template: "Bearer {{secret.token}}"
account_fields:
  - name: base_url
    required: true
  - name: token
    required: true
    secret: true
functions:
  - name: list_issues
    description: Search issues by JQL
    read_only: true
    method: GET
    url: "{{account.base_url}}/rest/api/3/search"
    query: { jql: "{{args.jql}}" }
    args_schema: { type: object, properties: { jql: { type: string } }, required: [jql] }
  - name: create_issue
    description: Create an issue
    method: POST
    url: "{{account.base_url}}/rest/api/3/issue"
    body: "{{args}}"
    args_schema: { type: object }
  - name: ping
    description: Fake reachability check
    mock: { status: 200, body: { ok: true } }
"#;

    #[test]
    fn parses_valid_doc_with_defaults() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        assert_eq!(doc.functions.len(), 3);
        let f = doc.function("list_issues").unwrap();
        assert!(f.read_only);
        assert_eq!(f.timeout_sec, 30);
        assert!(doc.function("ping").unwrap().is_mock());
        assert_eq!(doc.read_only_functions(), vec!["list_issues".to_string()]);
    }

    #[test]
    fn defaults_apply_to_functions_without_explicit_values() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        let create = doc.function("create_issue").unwrap();
        assert!(!create.read_only);
        assert_eq!(create.timeout_sec, 30);
        assert!(create.query.is_empty());
        assert!(!doc.function("ping").unwrap().read_only);
    }

    #[test]
    fn secret_fields_lists_secret_account_fields() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        assert_eq!(doc.secret_fields(), vec!["token".to_string()]);
    }

    #[test]
    fn name_mismatch_is_rejected() {
        let err = ConnectorDoc::from_yaml(JIRA_YAML, "not-jira").unwrap_err();
        assert!(matches!(err, ConnectorError::Invalid(_)));
    }

    #[test]
    fn unknown_field_is_rejected() {
        let bad = format!("{JIRA_YAML}bogus: 1\n");
        assert!(ConnectorDoc::from_yaml(&bad, "jira").is_err());
    }

    #[test]
    fn rejects_function_that_is_both_http_and_mock() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    mock: { status: 200, body: {} }\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn rejects_function_that_is_neither_http_nor_mock() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn duplicate_function_name_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n  - name: f\n    description: b\n    method: GET\n    url: http://b\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn healthcheck_naming_missing_function_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nhealthcheck: missing\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn empty_version_is_rejected() {
        let y = "name: x\nversion: \"\"\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn duplicate_account_field_name_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\n  - name: base_url\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn invalid_function_name_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: Bad_Name\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn function_name_with_hyphen_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list-issues\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn function_name_with_uppercase_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: ListIssues\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn function_name_with_underscore_is_accepted() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: list_issues\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_ok());
    }

    #[test]
    fn account_field_name_with_hyphen_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base-url\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn account_field_name_with_uppercase_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: BaseUrl\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn account_field_name_with_underscore_is_accepted() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_ok());
    }

    #[test]
    fn response_pick_on_mock_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    response_pick: [a, b]\n    mock: { status: 200, body: { a: 1 } }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(
            err.contains("f") && err.contains("response_pick"),
            "was: {err}"
        );
    }

    #[test]
    fn response_pick_on_http_is_accepted() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    response_pick: [number, user.login]\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_ok());
    }

    #[test]
    fn examples_validate_against_args_schema() {
        let ok = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }\n    examples:\n      - args: { title: hi }\n        note: minimal create\n";
        assert!(ConnectorDoc::from_yaml(ok, "x").is_ok());

        let bad = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    args_schema: { type: object, properties: { title: { type: string } }, required: [title] }\n    examples:\n      - args: { nope: 1 }\n        note: missing required title\n";
        let err = ConnectorDoc::from_yaml(bad, "x").unwrap_err().to_string();
        assert!(err.contains("f") && err.contains("example"), "was: {err}");
    }

    #[test]
    fn headers_forbid_secret_and_auth_allow_account_and_args() {
        let ok = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: base_url\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/x\"\n    headers:\n      X-Api-Version: \"2024-01\"\n      X-From: \"{{account.base_url}}\"\n      X-Q: \"{{args.q}}\"\n";
        assert!(ConnectorDoc::from_yaml(ok, "x").is_ok());

        let sec = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    headers:\n      X-Leak: \"{{secret.token}}\"\n";
        let err = ConnectorDoc::from_yaml(sec, "x").unwrap_err().to_string();
        assert!(err.contains("secret") && err.contains("auth"), "was: {err}");
    }

    #[test]
    fn path_auth_requires_auth_placeholder_exactly_once() {
        let ok = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    read_only: true\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/getMe\"\n";
        assert!(ConnectorDoc::from_yaml(ok, "t").is_ok());

        let missing = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    method: GET\n    url: \"{{account.base_url}}/getMe\"\n";
        let err = ConnectorDoc::from_yaml(missing, "t")
            .unwrap_err()
            .to_string();
        assert!(err.contains("get_me") && err.contains("auth"), "was: {err}");

        let twice = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: get_me\n    description: probe\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/{{auth}}/getMe\"\n";
        assert!(ConnectorDoc::from_yaml(twice, "t").is_err());
    }

    #[test]
    fn auth_placeholder_without_path_auth_is_rejected() {
        let y = "name: t\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: \"Bearer {{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/x\"\n";
        let err = ConnectorDoc::from_yaml(y, "t").unwrap_err().to_string();
        assert!(err.contains("auth") && err.contains("path"), "was: {err}");
    }

    #[test]
    fn auth_placeholder_in_query_or_body_is_rejected() {
        let y = "name: t\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: \"{{account.base_url}}/{{auth}}/x\"\n    query: { k: \"{{auth}}\" }\n";
        assert!(ConnectorDoc::from_yaml(y, "t").is_err());
    }

    #[test]
    fn auth_variants_reject_foreign_fields() {
        let y = "name: x\nversion: 0.1.0\nauth:\n  kind: header\n  header: Authorization\n  value_template: t\n  param: extra\nfunctions:\n  - name: f\n    description: a\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    // -- smtp function kind (slice 3) --

    const SMTP_YAML: &str = r#"
name: smtp
version: 0.1.0
healthcheck: verify
account_fields:
  - name: host
    required: true
  - name: port
    required: true
  - name: use_tls
  - name: username
  - name: from_email
    required: true
  - name: from_name
  - name: password
    required: true
    secret: true
functions:
  - name: send_email
    description: Send an email over SMTP
    smtp:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
      message:
        from_email: "{{account.from_email}}"
        from_name: "{{account.from_name}}"
        to: "{{args.to}}"
        cc: "{{args.cc}}"
        bcc: "{{args.bcc}}"
        subject: "{{args.subject}}"
        body_text: "{{args.body_text}}"
        body_html: "{{args.body_html}}"
    args_schema: { type: object, properties: { to: { type: string }, subject: { type: string } }, required: [to, subject] }
  - name: verify
    description: Probe the SMTP connection without sending
    read_only: true
    smtp:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
      verify: true
"#;

    #[test]
    fn parses_smtp_send_and_verify() {
        let doc = ConnectorDoc::from_yaml(SMTP_YAML, "smtp").unwrap();
        let send = doc.function("send_email").unwrap();
        assert!(send.is_smtp());
        assert!(!send.is_mock());
        let smtp = send.smtp.as_ref().unwrap();
        assert!(!smtp.verify);
        assert!(smtp.message.is_some());
        let verify = doc.function("verify").unwrap();
        assert!(verify.is_smtp());
        assert!(verify.read_only);
        assert!(verify.smtp.as_ref().unwrap().verify);
        assert!(verify.smtp.as_ref().unwrap().message.is_none());
    }

    #[test]
    fn rejects_function_that_is_both_smtp_and_http() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn rejects_verify_with_message_and_send_without_message() {
        let with_msg = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n      message: { from_email: a@b.c, to: \"{{args.to}}\", subject: \"{{args.subject}}\" }\n";
        assert!(ConnectorDoc::from_yaml(with_msg, "x").is_err());
        let no_msg = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n";
        assert!(ConnectorDoc::from_yaml(no_msg, "x").is_err());
    }

    #[test]
    fn response_pick_on_smtp_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    response_pick: [a, b]\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err();
        assert!(
            err.to_string().contains("response_pick"),
            "message was: {err}"
        );
    }

    #[test]
    fn smtp_password_allows_secret_placeholder() {
        assert!(ConnectorDoc::from_yaml(SMTP_YAML, "smtp").is_ok());
    }

    #[test]
    fn secret_in_smtp_message_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: password\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      message: { from_email: a@b.c, to: \"{{secret.password}}\", subject: s }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err();
        assert!(err.to_string().contains("secret"), "message was: {err}");
    }

    #[test]
    fn secret_in_smtp_host_is_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: \"{{secret.token}}\", port: \"25\", use_tls: \"false\" }\n      verify: true\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn args_in_smtp_connection_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: \"{{args.host}}\", port: \"25\", use_tls: \"false\" }\n      verify: true\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn unknown_account_field_in_smtp_message_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      message: { from_email: \"{{account.nope}}\", to: \"{{args.to}}\", subject: s }\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn auth_placeholder_in_smtp_connection_or_message_is_rejected() {
        let conn = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: \"{{auth}}\", port: \"25\", use_tls: \"false\" }\n      verify: true\n";
        let err = ConnectorDoc::from_yaml(conn, "x").unwrap_err().to_string();
        assert!(err.contains("auth"), "message was: {err}");
        let msg = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      message: { from_email: a@b.c, to: \"{{auth}}\", subject: s }\n";
        assert!(ConnectorDoc::from_yaml(msg, "x").is_err());
    }

    // -- imap function kind (wave 2) --

    const IMAP_YAML: &str = r#"
name: imap
version: 0.1.0
account_fields:
  - name: host
    required: true
  - name: port
    required: true
  - name: use_tls
  - name: auth_method
  - name: username
  - name: password
    required: true
    secret: true
functions:
  - name: verify
    description: Probe the IMAP connection without listing anything
    read_only: true
    imap:
      connection:
        host: "{{account.host}}"
        port: "{{account.port}}"
        use_tls: "{{account.use_tls}}"
        auth_method: "{{account.auth_method}}"
        username: "{{account.username}}"
        password: "{{secret.password}}"
      op: verify
"#;

    #[test]
    fn imap_function_parses_minimal() {
        let doc = ConnectorDoc::from_yaml(IMAP_YAML, "imap").unwrap();
        let f = doc.function("verify").unwrap();
        assert!(f.is_imap());
        let imap = f.imap.as_ref().unwrap();
        assert_eq!(imap.op, ImapOp::Verify);
        assert!(imap.params.is_empty());
    }

    #[test]
    fn imap_and_mock_together_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: p }\n      op: verify\n    mock: { status: 200, body: {} }\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn imap_with_http_fields_rejected() {
        let base = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n";
        let imap_block = "    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: p }\n      op: verify\n";

        let with_query = format!("{base}    query: {{ k: v }}\n{imap_block}");
        let err = ConnectorDoc::from_yaml(&with_query, "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("query"), "message was: {err}");

        let with_body = format!("{base}    body: {{ a: 1 }}\n{imap_block}");
        let err = ConnectorDoc::from_yaml(&with_body, "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("body"), "message was: {err}");

        let with_headers = format!("{base}    headers: {{ X-A: b }}\n{imap_block}");
        let err = ConnectorDoc::from_yaml(&with_headers, "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("headers"), "message was: {err}");

        let with_response_pick = format!("{base}    response_pick: [a]\n{imap_block}");
        let err = ConnectorDoc::from_yaml(&with_response_pick, "x")
            .unwrap_err()
            .to_string();
        assert!(err.contains("response_pick"), "message was: {err}");
    }

    #[test]
    fn imap_op_unknown_param_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: p }\n      op: verify\n      params: { folder: X }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("folder"), "message was: {err}");
    }

    #[test]
    fn imap_op_missing_required_param_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: p }\n      op: search\n      params: { folder: INBOX }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("limit"), "message was: {err}");
    }

    #[test]
    fn imap_secret_in_params_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: password\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: \"{{secret.password}}\" }\n      op: search\n      params: { folder: \"{{secret.password}}\", limit: \"10\" }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("secret"), "message was: {err}");
    }

    // -- body_form (spec 2026-07-22-official-connectors-wave-3, section 4.1) --

    const FORM_YAML: &str = r#"
name: chat
version: 0.1.0
account_fields:
  - name: base_url
    required: true
functions:
  - name: send_message
    description: Send a message with a form-encoded body
    method: POST
    url: "{{account.base_url}}/messages"
    body_form:
      type: stream
      to: "{{args.to}}"
      content: "{{args.content}}"
    args_schema: { type: object, properties: { to: { type: string }, content: { type: string } }, required: [to, content] }
"#;

    #[test]
    fn parses_body_form_function() {
        let doc = ConnectorDoc::from_yaml(FORM_YAML, "chat").unwrap();
        let f = doc.function("send_message").unwrap();
        assert_eq!(f.body_form.len(), 3);
        assert_eq!(f.body_form.get("type").map(String::as_str), Some("stream"));
        assert!(f.body.is_none());
    }

    #[test]
    fn body_and_body_form_together_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body: \"{{args}}\"\n    body_form: { k: v }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("f") && err.contains("body_form"), "was: {err}");
    }

    #[test]
    fn body_form_on_get_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n    body_form: { k: v }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("f") && err.contains("body_form"), "was: {err}");
    }

    #[test]
    fn body_form_on_body_carrying_methods_accepted() {
        // The method check is case-insensitive; `body` itself keeps no such
        // restriction (out of scope for wave 3).
        for method in ["POST", "PUT", "PATCH", "DELETE", "post"] {
            let y = format!(
                "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: {method}\n    url: http://a\n    body_form: {{ k: v }}\n"
            );
            assert!(
                ConnectorDoc::from_yaml(&y, "x").is_ok(),
                "method {method} must accept body_form"
            );
        }
    }

    #[test]
    fn body_form_on_mock_smtp_or_imap_rejected() {
        let mock = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    body_form: { k: v }\n    mock: { status: 200, body: {} }\n";
        let err = ConnectorDoc::from_yaml(mock, "x").unwrap_err().to_string();
        assert!(err.contains("body_form"), "was: {err}");

        let smtp = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    body_form: { k: v }\n    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n";
        let err = ConnectorDoc::from_yaml(smtp, "x").unwrap_err().to_string();
        assert!(err.contains("body_form"), "was: {err}");

        let imap = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    body_form: { k: v }\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: p }\n      op: verify\n";
        let err = ConnectorDoc::from_yaml(imap, "x").unwrap_err().to_string();
        assert!(err.contains("body_form"), "was: {err}");
    }

    #[test]
    fn secret_in_body_form_value_rejected() {
        let y = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body_form: { k: \"{{secret.token}}\" }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("secret") && err.contains("auth"), "was: {err}");
    }

    #[test]
    fn auth_placeholder_in_body_form_value_rejected() {
        let y = "name: x\nversion: 0.1.0\nauth:\n  kind: path\n  value_template: \"bot{{secret.token}}\"\naccount_fields:\n  - name: base_url\n  - name: token\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: \"{{account.base_url}}/{{auth}}/x\"\n    body_form: { k: \"{{auth}}\" }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("auth"), "was: {err}");
    }

    #[test]
    fn unknown_account_field_in_body_form_rejected() {
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    method: POST\n    url: http://a\n    body_form: { k: \"{{account.nope}}\" }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("nope"), "was: {err}");
    }

    #[test]
    fn imap_args_in_connection_password_rejected() {
        let args = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: \"{{args.password}}\" }\n      op: verify\n";
        let err = ConnectorDoc::from_yaml(args, "x").unwrap_err().to_string();
        assert!(
            err.contains("args") && err.contains("password"),
            "message was: {err}"
        );

        let account = "name: x\nversion: 0.1.0\naccount_fields:\n  - name: password\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: \"{{account.password}}\" }\n      op: verify\n";
        assert!(ConnectorDoc::from_yaml(account, "x").is_err());
    }

    // -- error_when (spec 2026-07-22-official-connectors-wave-3, section 4.2) --

    const ERROR_WHEN_YAML: &str = r#"
name: chat
version: 0.1.0
error_when:
  path: ok
  equals: false
  message_path: error
account_fields:
  - name: base_url
    required: true
functions:
  - name: list_items
    description: List items
    read_only: true
    method: GET
    url: "{{account.base_url}}/items"
"#;

    #[test]
    fn parses_error_when_block() {
        let doc = ConnectorDoc::from_yaml(ERROR_WHEN_YAML, "chat").unwrap();
        let ew = doc.error_when.as_ref().unwrap();
        assert_eq!(ew.path, "ok");
        assert_eq!(ew.equals, serde_json::json!(false));
        assert_eq!(ew.message_path.as_deref(), Some("error"));
    }

    #[test]
    fn error_when_defaults_to_none() {
        let doc = ConnectorDoc::from_yaml(JIRA_YAML, "jira").unwrap();
        assert!(doc.error_when.is_none());
    }

    #[test]
    fn error_when_message_path_is_optional_and_equals_is_any_scalar() {
        let y = "name: x\nversion: 0.1.0\nerror_when:\n  path: status\n  equals: error\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n";
        let doc = ConnectorDoc::from_yaml(y, "x").unwrap();
        let ew = doc.error_when.as_ref().unwrap();
        assert_eq!(ew.equals, serde_json::json!("error"));
        assert!(ew.message_path.is_none());
    }

    #[test]
    fn error_when_unknown_field_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nerror_when:\n  path: ok\n  equals: false\n  bogus: 1\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n";
        assert!(ConnectorDoc::from_yaml(y, "x").is_err());
    }

    #[test]
    fn error_when_empty_path_is_rejected() {
        let y = "name: x\nversion: 0.1.0\nerror_when:\n  path: \"\"\n  equals: false\nfunctions:\n  - name: f\n    description: d\n    method: GET\n    url: http://a\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(
            err.contains("error_when") && err.contains("path"),
            "was: {err}"
        );
    }

    #[test]
    fn error_when_without_http_functions_is_rejected() {
        // The spec says validation "warns" here; the connector validator has
        // no warning channel, so the case is a hard error (deliberate
        // deviation: a manifest with the block and zero HTTP functions is an
        // authoring error with no legitimate use).
        let y = "name: x\nversion: 0.1.0\nerror_when:\n  path: ok\n  equals: false\nfunctions:\n  - name: f\n    description: d\n    mock: { status: 200, body: { ok: false } }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(
            err.contains("error_when") && err.contains("HTTP"),
            "was: {err}"
        );
    }

    // -- webhook block (spec 2026-08-16-webhook-ingest-design) --

    const WEBHOOK_YAML: &str = r#"
name: echo-hooks
version: 0.1.0
webhook:
  challenge: meta_hub
  verify_token: "{{secret.verify_token}}"
  signature:
    scheme: hmac_sha256_hex
    header: X-Hub-Signature-256
    prefix: "sha256="
    secret: "{{secret.app_secret}}"
  dedupe_path: entry.0.id
account_fields:
  - name: verify_token
    required: true
    secret: true
  - name: app_secret
    required: true
    secret: true
functions:
  - name: inbox_read
    description: Read pending inbound events without consuming them
    read_only: true
    response_pick: [events, cursor]
    args_schema: { type: object, properties: { consumer: { type: string } } }
    inbox:
      op: read
"#;

    #[test]
    fn parses_the_webhook_block() {
        let doc = ConnectorDoc::from_yaml(WEBHOOK_YAML, "echo-hooks").unwrap();
        let hook = doc.webhook.as_ref().expect("webhook block parses");
        assert_eq!(hook.challenge, Some(ChallengeDialect::MetaHub));
        assert_eq!(
            hook.verify_token.as_deref(),
            Some("{{secret.verify_token}}")
        );
        assert_eq!(hook.signature.scheme, SignatureScheme::HmacSha256Hex);
        assert_eq!(hook.signature.header, "X-Hub-Signature-256");
        assert_eq!(hook.signature.prefix, "sha256=");
        assert_eq!(hook.signature.secret, "{{secret.app_secret}}");
        assert_eq!(hook.dedupe_path.as_deref(), Some("entry.0.id"));
    }

    #[test]
    fn webhook_block_defaults_to_absent_and_rejects_unknown_keys() {
        assert!(
            ConnectorDoc::from_yaml(JIRA_YAML, "jira")
                .unwrap()
                .webhook
                .is_none()
        );
        let bad = WEBHOOK_YAML.replace("  dedupe_path: entry.0.id", "  bogus: 1");
        assert!(ConnectorDoc::from_yaml(&bad, "echo-hooks").is_err());
        let bad_sig = WEBHOOK_YAML.replace("    prefix: \"sha256=\"", "    bogus: 1");
        assert!(ConnectorDoc::from_yaml(&bad_sig, "echo-hooks").is_err());
    }

    #[test]
    fn webhook_secret_placeholders_are_the_third_exemption() {
        // The two secret-carrying fields accept `{{secret.*}}`.
        assert!(ConnectorDoc::from_yaml(WEBHOOK_YAML, "echo-hooks").is_ok());

        // A secret placeholder naming a non-secret field is rejected, exactly
        // like the smtp and imap password fields.
        let non_secret = WEBHOOK_YAML.replace(
            "  - name: app_secret\n    required: true\n    secret: true",
            "  - name: app_secret\n    required: true",
        );
        let err = ConnectorDoc::from_yaml(&non_secret, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("app_secret"), "was: {err}");

        // An undeclared field is rejected.
        let unknown = WEBHOOK_YAML.replace("{{secret.app_secret}}", "{{secret.nope}}");
        let err = ConnectorDoc::from_yaml(&unknown, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "was: {err}");

        // `{{account.*}}` is allowed (a non-secret token is a bad idea but a
        // legal one); `{{args.*}}` and `{{auth}}` are not.
        let with_args = WEBHOOK_YAML.replace("{{secret.app_secret}}", "{{args.app_secret}}");
        let err = ConnectorDoc::from_yaml(&with_args, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("args"), "was: {err}");
        let with_auth = WEBHOOK_YAML.replace("{{secret.verify_token}}", "{{auth}}");
        let err = ConnectorDoc::from_yaml(&with_auth, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("auth"), "was: {err}");
    }

    #[test]
    fn webhook_literal_fields_reject_placeholders() {
        for (field, replacement) in [
            (
                "    header: X-Hub-Signature-256",
                "    header: \"{{account.h}}\"",
            ),
            ("    prefix: \"sha256=\"", "    prefix: \"{{args.p}}\""),
            (
                "  dedupe_path: entry.0.id",
                "  dedupe_path: \"{{secret.app_secret}}\"",
            ),
        ] {
            let y = WEBHOOK_YAML.replace(field, replacement);
            let err = ConnectorDoc::from_yaml(&y, "echo-hooks")
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("webhook"),
                "a placeholder in a literal webhook field must be named: {err}"
            );
        }
    }

    #[test]
    fn webhook_shape_rules() {
        // A challenge dialect requires a verify token.
        let no_token = WEBHOOK_YAML.replace("  verify_token: \"{{secret.verify_token}}\"\n", "");
        let err = ConnectorDoc::from_yaml(&no_token, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("verify_token"), "was: {err}");

        // A verify token without a challenge dialect has nothing to verify.
        let no_challenge = WEBHOOK_YAML.replace("  challenge: meta_hub\n", "");
        let err = ConnectorDoc::from_yaml(&no_challenge, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("challenge"), "was: {err}");

        // An empty header name would match nothing.
        let empty_header =
            WEBHOOK_YAML.replace("    header: X-Hub-Signature-256", "    header: \"\"");
        let err = ConnectorDoc::from_yaml(&empty_header, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("header"), "was: {err}");

        // An empty dedupe path is an authoring mistake, not "no path".
        let empty_path = WEBHOOK_YAML.replace("  dedupe_path: entry.0.id", "  dedupe_path: \"\"");
        let err = ConnectorDoc::from_yaml(&empty_path, "echo-hooks")
            .unwrap_err()
            .to_string();
        assert!(err.contains("dedupe_path"), "was: {err}");

        // The signature block itself is mandatory: no unsigned mode exists.
        let unsigned = "name: echo-hooks\nversion: 0.1.0\nwebhook:\n  dedupe_path: id\nfunctions:\n  - name: f\n    description: d\n    inbox: { op: read }\n";
        assert!(ConnectorDoc::from_yaml(unsigned, "echo-hooks").is_err());
    }

    // -- inbox function kind (spec 2026-08-16-webhook-ingest-design) --

    #[test]
    fn parses_the_three_inbox_ops() {
        let y = WEBHOOK_YAML.replace(
            "  - name: inbox_read\n    description: Read pending inbound events without consuming them\n    read_only: true\n    response_pick: [events, cursor]\n    args_schema: { type: object, properties: { consumer: { type: string } } }\n    inbox:\n      op: read\n",
            "  - name: inbox_read\n    description: Read pending inbound events without consuming them\n    read_only: true\n    response_pick: [events, cursor]\n    inbox:\n      op: read\n  - name: inbox_ack\n    description: Advance the consumer cursor after processing\n    inbox:\n      op: ack\n  - name: inbox_depth\n    description: How many events are pending\n    read_only: true\n    response_pick: [pending]\n    inbox:\n      op: peek_depth\n",
        );
        let doc = ConnectorDoc::from_yaml(&y, "echo-hooks").unwrap();
        assert_eq!(doc.functions.len(), 3);
        assert!(doc.function("inbox_read").unwrap().is_inbox());
        assert_eq!(
            doc.function("inbox_read")
                .unwrap()
                .inbox
                .as_ref()
                .unwrap()
                .op,
            InboxOp::Read
        );
        assert_eq!(
            doc.function("inbox_ack")
                .unwrap()
                .inbox
                .as_ref()
                .unwrap()
                .op,
            InboxOp::Ack
        );
        assert_eq!(
            doc.function("inbox_depth")
                .unwrap()
                .inbox
                .as_ref()
                .unwrap()
                .op,
            InboxOp::PeekDepth
        );
        assert_eq!(InboxOp::PeekDepth.as_str(), "peek_depth");
        assert_eq!(
            doc.inbox_functions(),
            vec![
                "inbox_read".to_string(),
                "inbox_ack".to_string(),
                "inbox_depth".to_string()
            ]
        );
        // An inbox function is not any of the other four kinds.
        let f = doc.function("inbox_ack").unwrap();
        assert!(!f.is_mock() && !f.is_smtp() && !f.is_imap());
    }

    #[test]
    fn an_inbox_function_is_exactly_one_kind() {
        let hook = "webhook:\n  signature: { scheme: hmac_sha256_hex, header: X-Sig, secret: \"{{secret.s}}\" }\naccount_fields:\n  - name: s\n    secret: true\n";
        for other in [
            "    method: GET\n    url: http://a\n",
            "    mock: { status: 200, body: {} }\n",
            "    smtp:\n      connection: { host: h, port: \"25\", use_tls: \"false\" }\n      verify: true\n",
            "    imap:\n      connection: { host: h, port: \"993\", use_tls: \"true\", auth_method: password, username: u, password: p }\n      op: verify\n",
        ] {
            let y = format!(
                "name: x\nversion: 0.1.0\n{hook}functions:\n  - name: f\n    description: d\n{other}    inbox:\n      op: read\n"
            );
            let err = ConnectorDoc::from_yaml(&y, "x").unwrap_err().to_string();
            assert!(err.contains("exactly one"), "was: {err}");
        }
        // And the zero-kind message names the new kind too.
        let none = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n";
        let err = ConnectorDoc::from_yaml(none, "x").unwrap_err().to_string();
        assert!(err.contains("inbox"), "was: {err}");
    }

    #[test]
    fn inbox_rejects_http_only_fields() {
        let hook = "webhook:\n  signature: { scheme: hmac_sha256_hex, header: X-Sig, secret: \"{{secret.s}}\" }\naccount_fields:\n  - name: s\n    secret: true\n";
        for (field, needle) in [
            ("    query: { k: v }\n", "query"),
            ("    body: { a: 1 }\n", "body"),
            ("    body_form: { k: v }\n", "body_form"),
            ("    headers: { X-A: b }\n", "headers"),
        ] {
            let y = format!(
                "name: x\nversion: 0.1.0\n{hook}functions:\n  - name: f\n    description: d\n{field}    inbox:\n      op: read\n"
            );
            let err = ConnectorDoc::from_yaml(&y, "x").unwrap_err().to_string();
            assert!(err.contains(needle), "expected `{needle}` in: {err}");
        }
    }

    #[test]
    fn response_pick_is_allowed_on_inbox_and_still_refused_on_the_other_three() {
        // Allowed: an inbox read projects the fixed envelope, and the
        // official-connector gate requires a read_only function to carry one.
        assert!(ConnectorDoc::from_yaml(WEBHOOK_YAML, "echo-hooks").is_ok());

        // The rejection message now names the two kinds that may carry it.
        let y = "name: x\nversion: 0.1.0\nfunctions:\n  - name: f\n    description: d\n    response_pick: [a]\n    mock: { status: 200, body: { a: 1 } }\n";
        let err = ConnectorDoc::from_yaml(y, "x").unwrap_err().to_string();
        assert!(err.contains("response_pick"), "was: {err}");
        assert!(
            err.contains("inbox"),
            "the message must say where it is legal: {err}"
        );
    }

    #[test]
    fn inbox_functions_and_the_webhook_block_require_each_other() {
        let no_hook = "name: x\nversion: 0.1.0\nfunctions:\n  - name: inbox_read\n    description: d\n    read_only: true\n    response_pick: [events]\n    inbox:\n      op: read\n";
        let err = ConnectorDoc::from_yaml(no_hook, "x")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("inbox") && err.contains("webhook"),
            "an inbox function without a webhook block must be refused: {err}"
        );

        let no_inbox = "name: x\nversion: 0.1.0\nwebhook:\n  signature: { scheme: hmac_sha256_hex, header: X-Sig, secret: \"{{secret.s}}\" }\naccount_fields:\n  - name: s\n    secret: true\nfunctions:\n  - name: f\n    description: d\n    mock: { status: 200, body: {} }\n";
        let err = ConnectorDoc::from_yaml(no_inbox, "x")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("webhook") && err.contains("inbox"),
            "a webhook block with no inbox function must be refused: {err}"
        );
    }
}
