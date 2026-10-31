//! Loading a list's items: from an earlier node's JSON output, from a list
//! node's own output, or inline; checking their ids; and the `where` filter.
//! Pure, so the validator checks inline items with the same code the engine
//! loads them with.

use serde_json::Value;

use super::{ITEM_ID_MAX_CHARS, ItemFilter};
use crate::content::sha256_hex;

/// One item of a list.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: String,
    pub value: Value,
    /// The digest of the item's canonical JSON.
    pub digest: String,
}

/// An item as a list source offers it: from a list node's output it carries
/// the state it ended in there.
#[derive(Debug, Clone, PartialEq)]
pub struct SourcedItem {
    pub item: Item,
    pub state: Option<String>,
}

/// Why a list's items cannot be loaded. Indexes count from 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemsError {
    NotJson,
    /// The output has no array under the field.
    NotArray,
    NotObject(usize),
    MissingKey(usize),
    /// The key is not a string or an integer, empty, or too long.
    BadKey(usize),
    /// The key holds a control character, a backtick or another character
    /// that does not print.
    UnsafeKey(usize),
    Duplicate(Vec<String>),
    /// Two ids share the digest prefix that names their files.
    DigestCollision(String, String),
    TooMany {
        count: usize,
        max: u32,
    },
}

impl std::fmt::Display for ItemsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ItemsError::NotJson => f.write_str("the source output is not a JSON object"),
            ItemsError::NotArray => f.write_str("the source field is not a JSON array"),
            ItemsError::NotObject(i) => write!(f, "item {} is not a JSON object", i + 1),
            ItemsError::MissingKey(i) => write!(f, "item {} has no key", i + 1),
            ItemsError::BadKey(i) => write!(
                f,
                "item {} has a key that is not a non-empty string or integer of at most {ITEM_ID_MAX_CHARS} characters",
                i + 1
            ),
            ItemsError::UnsafeKey(i) => write!(
                f,
                "item {} has a key with a control character, a backtick or another character that does not print",
                i + 1
            ),
            ItemsError::Duplicate(ids) => {
                let noun = if ids.len() == 1 { "id" } else { "ids" };
                write!(f, "{} duplicate {noun}: {}", ids.len(), ids.join(", "))
            }
            ItemsError::DigestCollision(a, b) => write!(
                f,
                "ids `{a}` and `{b}` share a digest prefix; rename one of them"
            ),
            ItemsError::TooMany { count, max } => {
                write!(f, "{count} items exceed max_items {max}")
            }
        }
    }
}

/// The items under `field` of a node's JSON object `output`, named by
/// `key`, at most `max` of them.
pub fn items_from_output(
    output: &str,
    field: &str,
    key: &str,
    max: u32,
) -> Result<Vec<Item>, ItemsError> {
    let doc: Value = serde_json::from_str(output).map_err(|_| ItemsError::NotJson)?;
    let obj = doc.as_object().ok_or(ItemsError::NotJson)?;
    let values = obj
        .get(field)
        .and_then(Value::as_array)
        .ok_or(ItemsError::NotArray)?;
    items_from_values(values, key, max)
}

/// Items from a list of JSON objects named by `key`, at most `max`.
pub fn items_from_values(values: &[Value], key: &str, max: u32) -> Result<Vec<Item>, ItemsError> {
    if values.len() > max as usize {
        return Err(ItemsError::TooMany {
            count: values.len(),
            max,
        });
    }
    let mut items = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        let obj = v.as_object().ok_or(ItemsError::NotObject(i))?;
        let id = id_of(obj.get(key).ok_or(ItemsError::MissingKey(i))?, i)?;
        items.push(item(id, v.clone()));
    }
    no_duplicates(&items)?;
    Ok(items)
}

/// The items of a list node's own output (`items[]` entries of `id`,
/// `state`, `item` and `output`), at most `max`. Each item is the entry's
/// `item` object with the earlier list's `output` added as `output`.
pub fn items_from_list_output(output: &str, max: u32) -> Result<Vec<SourcedItem>, ItemsError> {
    let doc: Value = serde_json::from_str(output).map_err(|_| ItemsError::NotJson)?;
    let entries = doc
        .get("items")
        .and_then(Value::as_array)
        .ok_or(ItemsError::NotArray)?;
    if entries.len() > max as usize {
        return Err(ItemsError::TooMany {
            count: entries.len(),
            max,
        });
    }
    let mut out = Vec::with_capacity(entries.len());
    for (i, e) in entries.iter().enumerate() {
        let obj = e.as_object().ok_or(ItemsError::NotObject(i))?;
        let id = id_of(obj.get("id").ok_or(ItemsError::MissingKey(i))?, i)?;
        let mut value = obj
            .get("item")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        // Always this list's output (null when it has none), so a chain of
        // lists never hands an item an output from further upstream. The key
        // is always there, so it is part of every list-sourced item's
        // digest; `item.output` reads null, never missing.
        let prev = obj
            .get("output")
            .filter(|o| o.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        value.insert("output".into(), prev);
        out.push(SourcedItem {
            item: item(id, Value::Object(value)),
            state: obj.get("state").and_then(Value::as_str).map(str::to_string),
        });
    }
    let plain: Vec<Item> = out.iter().map(|s| s.item.clone()).collect();
    no_duplicates(&plain)?;
    Ok(out)
}

/// Applies `filter`: returns the kept items and how many were filtered out.
/// From a list source (`state` known), `field` reads the earlier list's
/// output, and `state` the state the item ended in there; an item is kept
/// only when every condition set holds. `equals: null`
/// keeps an item whose field is present and null (a missing field is not
/// equal to it); any other `equals` keeps an item whose field is the same
/// string, number or bool. An object or array never matches.
pub fn apply_filter(items: Vec<SourcedItem>, filter: Option<&ItemFilter>) -> (Vec<Item>, u32) {
    let Some(f) = filter else {
        return (items.into_iter().map(|s| s.item).collect(), 0);
    };
    let mut out = Vec::new();
    let mut dropped = 0u32;
    for s in items {
        let from_list = s.state.is_some();
        let state_ok = f
            .state
            .as_deref()
            .is_none_or(|want| s.state.as_deref() == Some(want));
        let field_ok = match (&f.field, &f.equals) {
            (Some(field), Some(equals)) => {
                let doc = if from_list {
                    s.item.value.get("output").unwrap_or(&Value::Null)
                } else {
                    &s.item.value
                };
                field_equals(doc.get(field), equals)
            }
            _ => true,
        };
        if state_ok && field_ok {
            out.push(s.item);
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

/// One digest over every item's id and content, in order: two loads with
/// the same digest are the same list.
pub fn items_digest(items: &[Item]) -> String {
    let mut text = String::new();
    for i in items {
        text.push_str(&i.id);
        text.push('\n');
        text.push_str(&i.digest);
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

/// The bytes an item's JSON takes.
pub fn item_bytes(item: &Item) -> usize {
    serde_json::to_string(&item.value).map_or(0, |s| s.len())
}

fn item(id: String, value: Value) -> Item {
    let digest = sha256_hex(canonical(&value).as_bytes());
    Item { id, value, digest }
}

/// A scalar as the `output_field` edge compares it: a string as is, a number
/// or a bool in its JSON form; anything else matches nothing.
fn scalar(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether a field's value matches `equals` (see [`apply_filter`]).
fn field_equals(got: Option<&Value>, equals: &Value) -> bool {
    match equals {
        Value::Null => matches!(got, Some(Value::Null)),
        _ => scalar(got).is_some_and(|g| scalar(Some(equals)) == Some(g)),
    }
}

/// The id at index `i`: a non-empty string or integer of at most
/// [`ITEM_ID_MAX_CHARS`] characters that prints (it reaches prompts, the
/// environment and journal lines).
fn id_of(v: &Value, i: usize) -> Result<String, ItemsError> {
    let id = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return Err(ItemsError::BadKey(i)),
    };
    if id.trim().is_empty() || id.chars().count() > ITEM_ID_MAX_CHARS {
        return Err(ItemsError::BadKey(i));
    }
    if id.chars().any(unsafe_id_char) {
        return Err(ItemsError::UnsafeKey(i));
    }
    Ok(id)
}

/// A character an id may not hold: a backtick, a separator other than a
/// plain space, or a hidden character ([`crate::text::is_hidden_char`]:
/// controls, invisible formatting marks, bidi embeddings, overrides and
/// isolates).
fn unsafe_id_char(c: char) -> bool {
    c == '`' || (c.is_whitespace() && c != ' ') || crate::text::is_hidden_char(c)
}

/// The first 32 hex characters of an item id's SHA-256: what an item's
/// files and attempt names carry instead of its id.
pub fn item_hash(id: &str) -> String {
    let digest = sha256_hex(id.as_bytes());
    let hex = digest.strip_prefix("sha256:").unwrap_or(&digest);
    hex[..ITEM_HASH_HEX.min(hex.len())].to_string()
}

/// How many hex characters of an id's digest [`item_hash`] keeps.
pub const ITEM_HASH_HEX: usize = 32;

/// Refuses duplicate ids, and two ids whose [`item_hash`] is the same (they
/// would share files).
fn no_duplicates(items: &[Item]) -> Result<(), ItemsError> {
    let mut seen = std::collections::BTreeSet::new();
    let mut dups: Vec<String> = Vec::new();
    for i in items {
        if !seen.insert(i.id.as_str()) && !dups.contains(&i.id) {
            dups.push(i.id.clone());
        }
    }
    if !dups.is_empty() {
        return Err(ItemsError::Duplicate(dups));
    }
    let mut hashes: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    for i in items {
        if let Some(other) = hashes.insert(item_hash(&i.id), &i.id) {
            return Err(ItemsError::DigestCollision(other.to_string(), i.id.clone()));
        }
    }
    Ok(())
}

/// Canonical JSON: object keys sorted at every level.
fn canonical(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical(&map[k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        s => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(items: &[Item]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn items_load_by_key_and_refuse_every_bad_shape() {
        let ok = items_from_output(r#"{"m":[{"id":"a"},{"id":7}]}"#, "m", "id", 10).unwrap();
        assert_eq!(ids(&ok), ["a", "7"]);
        assert_eq!(
            items_from_output("nope", "m", "id", 10),
            Err(ItemsError::NotJson)
        );
        assert_eq!(
            items_from_output(r#"{"m":3}"#, "m", "id", 10),
            Err(ItemsError::NotArray)
        );
        assert_eq!(
            items_from_output(r#"{"m":[1]}"#, "m", "id", 10),
            Err(ItemsError::NotObject(0))
        );
        assert_eq!(
            items_from_output(r#"{"m":[{"x":1}]}"#, "m", "id", 10),
            Err(ItemsError::MissingKey(0))
        );
        for bad in [r#"{"id":""}"#, r#"{"id":1.5}"#, r#"{"id":[1]}"#] {
            let doc = format!(r#"{{"m":[{bad}]}}"#);
            assert_eq!(
                items_from_output(&doc, "m", "id", 10),
                Err(ItemsError::BadKey(0))
            );
        }
        let dup = items_from_output(
            r#"{"m":[{"id":"a"},{"id":"b"},{"id":"a"},{"id":"b"},{"id":"a"}]}"#,
            "m",
            "id",
            10,
        );
        assert_eq!(
            dup,
            Err(ItemsError::Duplicate(vec!["a".into(), "b".into()]))
        );
        assert_eq!(
            dup.unwrap_err().to_string(),
            "2 duplicate ids: a, b",
            "the refusal names the ids"
        );
        let many = items_from_output(r#"{"m":[{"id":1},{"id":2},{"id":3}]}"#, "m", "id", 2);
        assert_eq!(many, Err(ItemsError::TooMany { count: 3, max: 2 }));
    }

    /// An id reaches prompts, the environment and journal lines: one that
    /// does not print is refused with a reason of its own.
    #[test]
    fn ids_that_do_not_print_are_refused() {
        for bad in [
            "a\u{0}b",
            "a\nb",
            "a`b",
            "a\tb",
            "a\u{200B}b",
            "a\u{7F}",
            "a\u{2066}b\u{2069}",
            "a\u{061C}b",
        ] {
            let v = [json!({ "id": bad })];
            assert_eq!(
                items_from_values(&v, "id", 5),
                Err(ItemsError::UnsafeKey(0)),
                "{bad:?}"
            );
        }
        assert_eq!(
            ItemsError::UnsafeKey(0).to_string(),
            "item 1 has a key with a control character, a backtick or another character that does not print"
        );
        let ok = items_from_values(&[json!({"id": "a b-é"})], "id", 5).unwrap();
        assert_eq!(ids(&ok), ["a b-é"]);
        assert_eq!(
            ItemsError::Duplicate(vec!["a".into()]).to_string(),
            "1 duplicate id: a"
        );
    }

    #[test]
    fn an_item_hash_keeps_32_hex_characters() {
        let h = item_hash("a");
        assert_eq!(h.len(), 32);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(h, item_hash("b"));
    }

    /// `equals: null` keeps a field that is present and null; a missing
    /// field is not equal to it, and no other `equals` matches a missing or
    /// non-scalar field.
    #[test]
    fn equals_null_keeps_only_a_present_null_field() {
        let load = || {
            items_from_values(
                &[
                    json!({"id": "null", "x": null}),
                    json!({"id": "missing"}),
                    json!({"id": "obj", "x": {"a": 1}}),
                    json!({"id": "str", "x": "null"}),
                ],
                "id",
                5,
            )
            .unwrap()
            .into_iter()
            .map(|item| SourcedItem { item, state: None })
            .collect::<Vec<_>>()
        };
        let filter = |equals: Value| ItemFilter {
            field: Some("x".into()),
            equals: Some(equals),
            state: None,
        };
        let (kept, dropped) = apply_filter(load(), Some(&filter(Value::Null)));
        assert_eq!((ids(&kept), dropped), (vec!["null"], 3));
        let (kept, _) = apply_filter(load(), Some(&filter(json!("null"))));
        assert_eq!(ids(&kept), ["str"]);
        let (kept, _) = apply_filter(load(), Some(&filter(json!({"a": 1}))));
        assert!(kept.is_empty(), "an object never matches");
    }

    /// A list's own output always replaces an upstream `output`, even when
    /// it has none.
    #[test]
    fn a_chained_list_never_hands_on_an_upstream_output() {
        let out = json!({"items": [
            {"id": "a", "state": "needs_look", "item": {"output": {"from": "A"}}, "output": null},
        ]})
        .to_string();
        let items = items_from_list_output(&out, 5).unwrap();
        assert_eq!(items[0].item.value["output"], Value::Null);
    }

    #[test]
    fn the_digest_ignores_key_order_but_sees_values() {
        let a = items_from_values(&[json!({"id": "x", "a": 1, "b": 2})], "id", 5).unwrap();
        let b = items_from_values(&[json!({"b": 2, "a": 1, "id": "x"})], "id", 5).unwrap();
        let c = items_from_values(&[json!({"b": 3, "a": 1, "id": "x"})], "id", 5).unwrap();
        assert_eq!(items_digest(&a), items_digest(&b));
        assert_ne!(items_digest(&a), items_digest(&c));
    }

    #[test]
    fn the_filter_reads_fields_of_a_plain_source() {
        let items = items_from_values(
            &[
                json!({"id": "a", "paid": false}),
                json!({"id": "b", "paid": true}),
            ],
            "id",
            5,
        )
        .unwrap()
        .into_iter()
        .map(|item| SourcedItem { item, state: None })
        .collect();
        let f = ItemFilter {
            field: Some("paid".into()),
            equals: Some(json!("false")),
            state: None,
        };
        let (kept, dropped) = apply_filter(items, Some(&f));
        assert_eq!(ids(&kept), ["a"]);
        assert_eq!(dropped, 1);
    }

    #[test]
    fn the_filter_reads_state_and_output_of_a_list_source() {
        let out = json!({"items": [
            {"id": "a", "state": "passed", "item": {"s": 1}, "output": {"actionable": "true"}},
            {"id": "b", "state": "needs_look", "item": {"s": 2}, "output": {"actionable": "true"}},
            {"id": "c", "state": "passed", "item": {"s": 3}, "output": {"actionable": "false"}},
        ]})
        .to_string();
        let load = || items_from_list_output(&out, 10).unwrap();
        let passed = ItemFilter {
            field: None,
            equals: None,
            state: Some("passed".into()),
        };
        assert_eq!(ids(&apply_filter(load(), Some(&passed)).0), ["a", "c"]);
        let actionable = ItemFilter {
            field: Some("actionable".into()),
            equals: Some(json!("true")),
            state: None,
        };
        let (kept, dropped) = apply_filter(load(), Some(&actionable));
        assert_eq!(ids(&kept), ["a", "b"]);
        assert_eq!(dropped, 1);
        assert_eq!(kept[0].value["output"]["actionable"], "true");
        assert_eq!(kept[0].value["s"], 1);
    }

    /// A state and a field together: an item is kept only when both hold,
    /// so an item whose check did not pass is dropped although its output
    /// reads actionable.
    #[test]
    fn a_state_and_a_field_together_keep_only_items_that_pass_both() {
        let out = json!({"items": [
            {"id": "a", "state": "passed", "item": {}, "output": {"actionable": "true"}},
            {"id": "b", "state": "needs_look", "item": {}, "output": {"actionable": "true"}},
            {"id": "c", "state": "passed", "item": {}, "output": {"actionable": "false"}},
        ]})
        .to_string();
        let both = ItemFilter {
            field: Some("actionable".into()),
            equals: Some(json!("true")),
            state: Some("passed".into()),
        };
        let (kept, dropped) = apply_filter(items_from_list_output(&out, 10).unwrap(), Some(&both));
        assert_eq!(ids(&kept), ["a"]);
        assert_eq!(dropped, 2);
    }
}
