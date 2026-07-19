//! The connector-call result taxonomy shared by the HTTP path
//! (`connector_call`) and the SMTP path (`connector_smtp`): the structured
//! error code and error, the success value, and the interim literal message
//! redaction. It is the dependency sink both call kinds point at, so neither
//! call module has to depend on the other (keeps the file graph acyclic).

use serde_json::{Value, json};

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
}

impl CallError {
    pub(crate) fn new(code: CallErrorCode, message: impl Into<String>) -> Self {
        CallError {
            code,
            message: message.into(),
            http_status: None,
            retry_after_sec: None,
        }
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
        json!({ "ok": false, "error": Value::Object(err) })
    }
}

/// A successful call result (spec section 8). HTTP and mock carry
/// `status`/`truncated` (plus the `link`/`picked` HTTP extras); smtp carries
/// only a body (spec 4.2: `{ ok: true, body: { accepted, from, subject } }`
/// for send, `{ verified: true }` for verify).
#[derive(Debug)]
pub enum CallOk {
    Http {
        status: u16,
        body: Value,
        truncated: bool,
        /// The raw `Link` response header, when the service sent one (spec 4.4).
        link: Option<String>,
        /// True when the function's `response_pick` projection was applied to
        /// `body` (spec 4.5), so the caller knows it holds a subset.
        picked: bool,
    },
    Smtp {
        body: Value,
    },
}

impl CallOk {
    /// The `{ "ok": true, ... }` success JSON, shaped per kind. HTTP keeps the
    /// full `status`/`body`/`truncated` shape and appends `link`/`picked`
    /// exactly as before (link only when present, picked only when true);
    /// smtp emits just `{ ok, body }`.
    pub(crate) fn to_success_json(&self) -> Value {
        match self {
            CallOk::Http {
                status,
                body,
                truncated,
                link,
                picked,
            } => {
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
        }
    }

    /// The response body regardless of shape (test/inspection accessor).
    pub fn body(&self) -> &Value {
        match self {
            CallOk::Http { body, .. } | CallOk::Smtp { body } => body,
        }
    }
}

/// Applies the interim literal redaction to a plain message string: every
/// occurrence of a resolved secret value is replaced with
/// `[redacted:<LABEL>]`. Used for error messages by both call kinds (the HTTP
/// body path uses `redact_value` over its string leaves).
pub(crate) fn redact_message(mut msg: String, redactions: &[(String, String)]) -> String {
    for (secret, var) in redactions {
        if msg.contains(secret.as_str()) {
            msg = msg.replace(secret.as_str(), &format!("[redacted:{var}]"));
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_message_scrubs_secret_in_error_text() {
        let redactions = vec![("super-secret-xyz".to_string(), "MOCK_TOKEN".to_string())];
        let msg = "Connection Failed: http://127.0.0.1:9/items?api_key=super-secret-xyz refused"
            .to_string();
        let out = redact_message(msg, &redactions);
        assert!(!out.contains("super-secret-xyz"), "secret leaked: {out}");
        assert!(out.contains("[redacted:MOCK_TOKEN]"), "not redacted: {out}");
    }
}
