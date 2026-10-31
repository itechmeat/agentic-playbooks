//! The connector-call result taxonomy shared by the HTTP path
//! (`connector_call`) and the SMTP path (`connector_smtp`): the structured
//! error code and error, the success value, and the interim literal message
//! redaction. It is the dependency sink both call kinds point at, so neither
//! call module has to depend on the other (keeps the file graph acyclic).

use apb_core::connector::inbox::InboxGap;
use apb_core::secret::Redaction;
use serde_json::{Value, json};

use crate::connector::completeness::Incompleteness;
pub use crate::connector::completeness::{BodyIncomplete, IncompleteReason};

/// The structured error code taxonomy (spec section 8). The wire form is the
/// snake_case string from [`CallErrorCode::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallErrorCode {
    Config,
    Permission,
    InvalidArgs,
    Auth,
    NotFound,
    RateLimited,
    Service,
    Network,
    Timeout,
    /// A 2xx on a `read_only` function whose response body did not read
    /// whole (cut at the read cap, or the read failed partway): the honest
    /// answer is an error, never a partial body passed as complete data.
    IncompleteResponse,
    /// A send of this intent may already have gone out, and apb cannot tell:
    /// a repeat is refused until a person decides (send-once gate).
    UnknownEffect,
    /// The same intent is being sent right now by another call.
    InFlight,
    /// The attempt this call claims to belong to has ended, so it may not
    /// send anything.
    AttemptClosed,
}

impl CallErrorCode {
    /// The wire string used in the result JSON and the event `outcome`.
    pub fn as_str(self) -> &'static str {
        match self {
            CallErrorCode::Config => "config",
            CallErrorCode::Permission => "permission",
            CallErrorCode::InvalidArgs => "invalid_args",
            CallErrorCode::Auth => "auth",
            CallErrorCode::NotFound => "not_found",
            CallErrorCode::RateLimited => "rate_limited",
            CallErrorCode::Service => "service",
            CallErrorCode::Network => "network",
            CallErrorCode::Timeout => "timeout",
            CallErrorCode::IncompleteResponse => "incomplete_response",
            CallErrorCode::UnknownEffect => "unknown_effect",
            CallErrorCode::InFlight => "in_flight",
            CallErrorCode::AttemptClosed => "attempt_closed",
        }
    }
}

/// A structured call error (spec section 8).
#[derive(Debug, Clone)]
pub struct CallError {
    pub code: CallErrorCode,
    pub message: String,
    pub http_status: Option<u16>,
    pub retry_after_sec: Option<u64>,
    /// Set only on an `IncompleteResponse` error: why the body read did not
    /// complete.
    pub incomplete: Option<BodyIncomplete>,
}

impl CallError {
    pub(crate) fn new(code: CallErrorCode, message: impl Into<String>) -> Self {
        CallError {
            code,
            message: message.into(),
            http_status: None,
            retry_after_sec: None,
            incomplete: None,
        }
    }

    /// Builds the `incomplete_response` error for a read-only call whose 2xx
    /// body did not read whole: it carries the real status, the reason, and
    /// the cap when relevant, and no partial body travels as data. The
    /// message tells the caller to narrow the request; nothing in apb
    /// retries such a call automatically.
    pub(crate) fn incomplete_response(status: u16, incomplete: BodyIncomplete) -> Self {
        let why = match incomplete.reason {
            IncompleteReason::BodyCap => format!(
                "the response body was cut at the {} byte read cap",
                incomplete.cap_bytes.unwrap_or_default()
            ),
            IncompleteReason::ReadError => "the response body read failed partway".to_string(),
        };
        let mut err = CallError::new(
            CallErrorCode::IncompleteResponse,
            format!(
                "{why}, so the result would be incomplete; narrow the request (page, filter, or use a smaller page size) and call again"
            ),
        );
        err.http_status = Some(status);
        err.incomplete = Some(incomplete);
        err
    }

    /// The `{ "ok": false, "error": { ... } }` result JSON.
    pub(crate) fn to_json(&self) -> Value {
        let mut err = serde_json::Map::new();
        err.insert("code".into(), json!(self.code.as_str()));
        err.insert("message".into(), json!(self.message));
        if let Some(s) = self.http_status {
            err.insert("http_status".into(), json!(s));
        }
        if let Some(r) = self.retry_after_sec {
            err.insert("retry_after_sec".into(), json!(r));
        }
        if let Some(b) = &self.incomplete {
            b.detail(&mut err);
        }
        json!({ "ok": false, "error": Value::Object(err) })
    }
}

/// A successful call result (spec section 8). HTTP and mock carry
/// `status` plus the body-read outcome (plus the `link`/`picked` HTTP
/// extras); smtp carries only a body (spec 4.2: `{ ok: true, body: {
/// accepted, from, subject } }` for send, `{ verified: true }` for verify);
/// inbox carries the fixed envelope of a local store read, with the same
/// `picked` flag HTTP uses (spec 2026-08-16-webhook-ingest-design).
#[derive(Debug)]
pub enum CallOk {
    Http {
        status: u16,
        body: Value,
        /// Set when the body read did not complete (cut at the read cap, or
        /// a read error after part of the body); `None` for a whole body.
        /// The incompleteness lives here, OUTSIDE `body`, so the
        /// `response_pick` projection cannot remove it, and surfaces at the
        /// top level of the result JSON.
        incomplete: Option<BodyIncomplete>,
        /// The raw `Link` response header, when the service sent one (spec 4.4).
        link: Option<String>,
        /// True when the function's `response_pick` projection was applied to
        /// `body` (spec 4.5), so the caller knows it holds a subset.
        picked: bool,
    },
    Smtp {
        body: Value,
    },
    Inbox {
        body: Value,
        /// True when `response_pick` was applied to the envelope.
        picked: bool,
        /// An unread-events gap the read detected (retention dropped events
        /// the consumer never acknowledged): the inclusive lost seq range.
        /// Emitted at the top level of the result JSON, outside `body`, so a
        /// `response_pick` projection cannot hide it. `None` keeps today's
        /// shape (no `gap` key).
        gap: Option<InboxGap>,
    },
    /// The intent was already sent and confirmed: nothing went out again.
    AlreadySent {
        intent: String,
        label: String,
        /// The attempt that sent it.
        sent_in_attempt: Option<u32>,
        /// The created item's id or link, when known.
        effect_ref: Option<String>,
    },
}

impl CallOk {
    /// The `{ "ok": true, ... }` success JSON, shaped per kind. HTTP keeps the
    /// full `status`/`body`/`truncated` shape and appends `link`/`picked`
    /// exactly as before (link only when present, picked only when true);
    /// smtp emits just `{ ok, body }`; inbox emits `{ ok, body }` plus the
    /// same `picked` marker HTTP uses, and never a status (no request left
    /// the machine). A complete result adds no keys beyond that shape; an
    /// incomplete one is annotated by the shared completeness rule
    /// ([`crate::connector::completeness`]).
    pub(crate) fn to_success_json(&self) -> Value {
        let mut value = match self {
            CallOk::Http {
                status,
                body,
                incomplete,
                link,
                picked,
            } => {
                let truncated = incomplete
                    .as_ref()
                    .is_some_and(|b| b.reason == IncompleteReason::BodyCap);
                let mut value = json!({
                    "ok": true,
                    "status": status,
                    "body": body,
                    "truncated": truncated,
                });
                if let Some(link) = link {
                    value["link"] = json!(link);
                }
                if *picked {
                    value["picked"] = json!(true);
                }
                value
            }
            CallOk::Smtp { body } => json!({ "ok": true, "body": body }),
            CallOk::Inbox { body, picked, .. } => {
                let mut value = json!({ "ok": true, "body": body });
                if *picked {
                    value["picked"] = json!(true);
                }
                value
            }
            CallOk::AlreadySent {
                intent,
                label,
                sent_in_attempt,
                effect_ref,
            } => json!({
                "ok": true,
                "already_sent": true,
                "intent": { "id": intent, "label": label },
                "sent_in_attempt": sent_in_attempt,
                "ref": effect_ref,
            }),
        };
        if let Some(loss) = self.incompleteness() {
            loss.annotate(&mut value);
        }
        value
    }

    /// What this success did not deliver whole, if anything.
    fn incompleteness(&self) -> Option<Incompleteness<'_>> {
        match self {
            CallOk::Http { incomplete, .. } => incomplete.as_ref().map(Incompleteness::Body),
            CallOk::Inbox { gap, .. } => gap.as_ref().map(Incompleteness::InboxGap),
            CallOk::Smtp { .. } | CallOk::AlreadySent { .. } => None,
        }
    }

    /// The response body regardless of shape (test/inspection accessor);
    /// `null` for an already-sent answer, which carries no response.
    pub fn body(&self) -> &Value {
        static NONE: Value = Value::Null;
        match self {
            CallOk::Http { body, .. } | CallOk::Smtp { body } | CallOk::Inbox { body, .. } => body,
            CallOk::AlreadySent { .. } => &NONE,
        }
    }

    /// The journal's `response_complete` value for this result: `Some` for an
    /// HTTP response (whole, or cut or failed partway), `None` where
    /// completeness does not apply (smtp, imap, inbox; a mock is decided by
    /// its call kind, not by the result).
    pub(crate) fn response_complete(&self) -> Option<bool> {
        match self {
            CallOk::Http { incomplete, .. } => Some(incomplete.is_none()),
            CallOk::Smtp { .. } | CallOk::Inbox { .. } | CallOk::AlreadySent { .. } => None,
        }
    }
}

/// Applies the interim literal redaction to a plain message string: every
/// occurrence of a resolved secret value is replaced with
/// `[redacted:<LABEL>]`. Used for error messages by both call kinds (the HTTP
/// body path uses `redact_value` over its string leaves).
pub(crate) fn redact_message(msg: String, redactions: &[Redaction]) -> String {
    if redactions.is_empty() {
        return msg;
    }
    Redaction::apply_all_owned(msg, redactions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_message_scrubs_secret_in_error_text() {
        let redactions = vec![Redaction::new(
            apb_core::secret::Secret::new("super-secret-xyz"),
            "MOCK_TOKEN",
        )];
        let msg = "Connection Failed: http://127.0.0.1:9/items?api_key=super-secret-xyz refused"
            .to_string();
        let out = redact_message(msg, &redactions);
        assert!(!out.contains("super-secret-xyz"), "secret leaked");
        assert!(out.contains("[redacted:MOCK_TOKEN]"), "not redacted");
    }
}
