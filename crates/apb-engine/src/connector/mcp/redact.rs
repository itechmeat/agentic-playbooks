//! The account's redactions over server-provided text and JSON (design D3
//! "Mapping", D2 "Reflected secrets"): the one walk the listing check, the
//! listing's display and a tool's answer share, and the bound of every
//! server-provided string. A leaf: it imports nothing of the mcp kind.

use apb_core::secret::Redaction;
use serde_json::{Map, Value};

/// The longest server-provided text a result, the journal or the terminal
/// keeps: a tool's error text, a JSON-RPC error message, a client failure
/// that may carry server data. Cut after the redaction, never before.
pub(crate) const SERVER_TEXT_CHARS: usize = 500;

/// Server-provided `text` as apb keeps it: the account's redactions over
/// the whole text first, then cut to `SERVER_TEXT_CHARS` characters on a
/// character boundary, with a marker when cut.
pub(crate) fn server_text(text: &str, redactions: &[Redaction]) -> String {
    let redacted = Redaction::apply_all(text, redactions);
    let total = redacted.chars().count();
    if total <= SERVER_TEXT_CHARS {
        return redacted;
    }
    let kept: String = redacted.chars().take(SERVER_TEXT_CHARS).collect();
    format!(
        "{kept} [cut: {} more characters]",
        total - SERVER_TEXT_CHARS
    )
}

/// The longest server-provided name or short field a listing keeps: a
/// tool's title, an unmapped tool's name, a member name of a tool.
pub(crate) const SERVER_NAME_CHARS: usize = 200;

/// A server-provided name or short field as a listing keeps it: the
/// account's redactions over the whole text first, then cut to
/// `SERVER_NAME_CHARS` characters, with a marker when cut.
pub(crate) fn server_name(text: &str, redactions: &[Redaction]) -> String {
    let redacted = Redaction::apply_all(text, redactions);
    let total = redacted.chars().count();
    if total <= SERVER_NAME_CHARS {
        return redacted;
    }
    let kept: String = redacted.chars().take(SERVER_NAME_CHARS).collect();
    format!(
        "{kept} [cut: {} more characters]",
        total - SERVER_NAME_CHARS
    )
}

/// The account's redactions over every part of a server's JSON that can
/// hold text: every string, every object member name, and the JSON text of
/// every number (a number whose text holds a secret becomes the redacted
/// text, a string). Answers the redacted value and whether anything was
/// replaced. The one walk the listing check (`reflects_secret`), the
/// listing's display and a tool's answer share.
pub(crate) fn redact_tree(value: Value, redactions: &[Redaction]) -> (Value, bool) {
    let mut changed = false;
    let value = walk(value, redactions, &mut changed);
    (value, changed)
}

fn walk(value: Value, redactions: &[Redaction], changed: &mut bool) -> Value {
    let mut text = |s: String| {
        let r = Redaction::apply_all(&s, redactions);
        if r != s {
            *changed = true;
        }
        r
    };
    match value {
        Value::String(s) => Value::String(text(s)),
        Value::Number(n) => {
            let shown = n.to_string();
            let redacted = text(shown.clone());
            if redacted == shown {
                Value::Number(n)
            } else {
                Value::String(redacted)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| walk(v, redactions, changed))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let k = {
                    let r = Redaction::apply_all(&k, redactions);
                    if r != k {
                        *changed = true;
                    }
                    r
                };
                let v = walk(v, redactions, changed);
                out.insert(k, v);
            }
            Value::Object(out)
        }
        other => other,
    }
}
