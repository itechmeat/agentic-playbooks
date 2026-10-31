//! What makes two sends the same send: the node (and list item), the
//! connector function, the account, and a label. The label is the caller's
//! `--intent` name when it gives one. Without one it is, for a list item's
//! send, [`ITEM_LABEL`]: one send per item and function, so a rerun of the
//! item that writes the payload differently is still the same send (and
//! answers `already_sent` once it went out); an item that sends one
//! function more than once names each send with its own `--intent`.
//! Outside a list it is a short hash of the arguments, so a retry that
//! repeats a call word for word is recognised even without a name.

use serde_json::Value;

/// The identity of one send within a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentKey {
    pub node: String,
    pub connector: String,
    pub function: String,
    pub account: String,
    pub label: String,
    /// The list item the send belongs to: a send of one item is never the
    /// same send as another item's, whatever its label.
    pub item: Option<String>,
}

impl IntentKey {
    /// The execution the send belongs to.
    pub fn exec_key(&self) -> crate::exec_key::ExecKey<'_> {
        crate::exec_key::ExecKey::new(&self.node, self.item.as_deref())
    }
}

/// What every label of a send made without `--intent` starts with; a
/// caller's `--intent` never does (`:` is not allowed in one).
pub const ARGS_LABEL_PREFIX: &str = "args:";

/// The label of a list item's send made without `--intent`: the item
/// itself is the send (the key already names the node, the item, the
/// connector function and the account). A caller's `--intent` never
/// equals it (`:` is not allowed in one).
pub const ITEM_LABEL: &str = "item:send";

/// The label of a send made without `--intent`: [`ITEM_LABEL`] for a list
/// item's send, else [`args_label`].
pub fn default_label(item: Option<&str>, args: &Value) -> String {
    match item {
        Some(_) => ITEM_LABEL.to_string(),
        None => args_label(args),
    }
}

/// The label of a send outside a list made without `--intent`: `args:` plus the first 16
/// hex characters of the SHA-256 of the arguments' canonical JSON (object
/// keys sorted at every level), taken as the caller wrote them, before any
/// text mode or stamp.
pub fn args_label(args: &Value) -> String {
    let mut text = String::new();
    canonical(args, &mut text);
    let digest = apb_core::content::sha256_hex(text.as_bytes());
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    format!("{ARGS_LABEL_PREFIX}{}", &hex[..16.min(hex.len())])
}

/// Checks a caller's `--intent` label: `[a-z0-9][a-z0-9._-]{0,63}`.
pub fn validate_label(label: &str) -> Result<(), String> {
    let mut chars = label.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok =
        chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    if first_ok && rest_ok && label.len() <= 64 {
        Ok(())
    } else {
        Err(format!(
            "intent label `{label}` is invalid: use 1 to 64 characters of a-z, 0-9, `.`, `_` and `-`, starting with a letter or digit"
        ))
    }
}

fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_args_label_ignores_key_order_and_sees_values() {
        let a = args_label(&json!({ "b": 1, "a": { "y": [1, 2], "x": "s" } }));
        let b = args_label(&json!({ "a": { "x": "s", "y": [1, 2] }, "b": 1 }));
        assert_eq!(a, b);
        assert!(a.starts_with("args:") && a.len() == 21);
        assert_ne!(
            a,
            args_label(&json!({ "b": 2, "a": { "y": [1, 2], "x": "s" } }))
        );
    }

    #[test]
    fn an_item_send_without_a_name_is_one_send_whatever_its_args() {
        let one = default_label(Some("a"), &json!({ "text": "hi" }));
        assert_eq!(one, default_label(Some("a"), &json!({ "text": "hello" })));
        assert!(validate_label(&one).is_err(), "no --intent can take it");
        let outside = default_label(None, &json!({ "text": "hi" }));
        assert_eq!(outside, args_label(&json!({ "text": "hi" })));
    }

    #[test]
    fn labels_follow_the_slug_rule() {
        assert!(validate_label("announce-v1.4.0").is_ok());
        assert!(validate_label("a").is_ok());
        assert!(validate_label(&"a".repeat(64)).is_ok());
        assert!(validate_label(&"a".repeat(65)).is_err());
        assert!(validate_label("").is_err());
        assert!(validate_label("-x").is_err());
        assert!(validate_label("Upper").is_err());
        assert!(validate_label("a b").is_err());
    }
}
