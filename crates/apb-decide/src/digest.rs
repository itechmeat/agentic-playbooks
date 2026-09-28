//! Canonical JSON digests: the cache and replay keys.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `value` with every object's keys in sorted order, whatever map type
/// serde_json was built with, so equal values always serialize equally.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), canonical(&map[k]));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// `sha256:<hex>` of the canonical JSON of `value`.
pub fn digest(value: &Value) -> String {
    let text = serde_json::to_string(&canonical(value)).unwrap_or_default();
    let hash = Sha256::digest(text.as_bytes());
    let mut hex = String::with_capacity(7 + 64);
    hex.push_str("sha256:");
    for b in hash {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// The digest of a question map, as sent on the wire.
pub fn questions_digest(questions: &std::collections::BTreeMap<String, crate::Question>) -> String {
    digest(&serde_json::to_value(questions).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_order_does_not_change_the_digest() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":[{"k":1,"j":2}]}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"x":[{"j":2,"k":1}],"y":2},"b":1}"#).unwrap();
        assert_eq!(digest(&a), digest(&b));
        assert_ne!(digest(&a), digest(&serde_json::json!({"b": 2})));
        assert!(digest(&a).starts_with("sha256:"));
    }
}
