//! The path reader of an observation: a dotted path into a JSON document,
//! where a segment ending in `[]` maps over an array (`data.items[].id`).
//!
//! Whatever does not fit the observation's type reads as unknown, never as
//! zero, empty or a truncated list: a missing key, a type mismatch, a
//! number that is not finite, a list longer than [`MAX_LIST_ITEMS`], an
//! item or a field text longer than [`MAX_ITEM_CHARS`], or text with a
//! control or invisible format character ([`crate::text::has_hidden`]).
//!
//! A list is a set: an item that appears twice is kept once, in the order
//! it first appeared, so the counts a comparison shows agree with its
//! verdict. An integer item keeps its exact digits.

use serde_json::Value;

use super::{MAX_ITEM_CHARS, MAX_LIST_ITEMS, Observation, ObservationType, Observe, ObservedValue};

/// One parsed path segment: a key (empty only for a leading `[]`) and
/// whether the segment maps over an array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub key: String,
    pub map: bool,
}

/// Parses a path, or says why it is not one (validator V64).
pub fn parse_path(path: &str) -> Result<Vec<Segment>, String> {
    if path.trim().is_empty() {
        return Err("the path is empty".into());
    }
    let mut out = Vec::new();
    for (i, raw) in path.split('.').enumerate() {
        let (key, map) = match raw.strip_suffix("[]") {
            Some(k) => (k, true),
            None => (raw, false),
        };
        let key_ok = key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if !key_ok || (key.is_empty() && !(map && i == 0)) {
            return Err(format!("`{path}` is not a dotted path (segment `{raw}`)"));
        }
        out.push(Segment {
            key: key.to_string(),
            map,
        });
    }
    Ok(out)
}

/// The values a path reaches: one value, or with a `[]` segment the mapped
/// list. `Err` names why the path does not reach anything.
fn walk<'a>(doc: &'a Value, segments: &[Segment]) -> Result<(Vec<&'a Value>, bool), String> {
    let mut current: Vec<&Value> = vec![doc];
    let mut mapped = false;
    for seg in segments {
        let mut next = Vec::with_capacity(current.len());
        for v in current {
            let v = if seg.key.is_empty() {
                v
            } else {
                v.get(&seg.key)
                    .ok_or_else(|| format!("`{}` is not in the response", seg.key))?
            };
            if seg.map {
                let items = v
                    .as_array()
                    .ok_or_else(|| format!("`{}[]` is not an array", seg.key))?;
                next.extend(items.iter());
            } else {
                next.push(v);
            }
        }
        if seg.map {
            mapped = true;
        }
        current = next;
    }
    Ok((current, mapped))
}

fn finite(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

/// `s` as observed text: at most [`MAX_ITEM_CHARS`] characters, no
/// control or invisible format character. `what` names it in the reason.
fn checked_text(s: &str, what: &str) -> Result<String, String> {
    if s.chars().count() > MAX_ITEM_CHARS {
        return Err(format!("{what} is longer than {MAX_ITEM_CHARS} characters"));
    }
    if crate::text::has_hidden(s) {
        return Err(format!("{what} contains a control or format character"));
    }
    Ok(s.to_string())
}

fn list_item(v: &Value) -> Result<String, String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        // An integer keeps its exact digits: two ids past 2^53 stay apart.
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        Value::Number(_) => match finite(v) {
            Some(n) => super::format_number(n),
            None => return Err("a list item is not a finite number".into()),
        },
        _ => return Err("a list item is not a string, a number or a bool".into()),
    };
    checked_text(&s, "a list item")
}

/// The items of a list, each once, in the order they first appear.
fn distinct(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    items
        .into_iter()
        .filter(|i| seen.insert(i.clone()))
        .collect()
}

/// Reads `observe` out of `doc`.
pub fn extract(doc: &Value, observe: &Observe) -> Observation {
    match read(doc, observe) {
        Ok(v) => Observation::value(v),
        Err(reason) => Observation::unknown(reason),
    }
}

fn read(doc: &Value, observe: &Observe) -> Result<ObservedValue, String> {
    let segments = parse_path(&observe.path)?;
    let (values, mapped) = walk(doc, &segments)?;
    let single = || -> Result<&Value, String> {
        match (mapped, values.as_slice()) {
            (false, [v]) => Ok(v),
            _ => Err(format!("`{}` maps over an array", observe.path)),
        }
    };
    match observe.kind {
        ObservationType::Number if observe.count => {
            let len = if mapped {
                values.len()
            } else {
                let arr = single()?
                    .as_array()
                    .ok_or_else(|| format!("`{}` is not an array to count", observe.path))?;
                arr.len()
            };
            Ok(ObservedValue::Number(len as f64))
        }
        ObservationType::Number => finite(single()?)
            .map(ObservedValue::Number)
            .ok_or_else(|| format!("`{}` is not a finite number", observe.path)),
        ObservationType::List => {
            let items: Vec<&Value> = if mapped {
                values
            } else {
                single()?
                    .as_array()
                    .ok_or_else(|| format!("`{}` is not a list", observe.path))?
                    .iter()
                    .collect()
            };
            if items.len() > MAX_LIST_ITEMS {
                return Err(format!(
                    "`{}` has {} items, more than the {MAX_LIST_ITEMS} an observation keeps",
                    observe.path,
                    items.len()
                ));
            }
            items
                .into_iter()
                .map(list_item)
                .collect::<Result<Vec<_>, _>>()
                .map(|l| ObservedValue::List(distinct(l)))
        }
        ObservationType::Field => {
            let v = single()?;
            match v {
                Value::String(s) => {
                    checked_text(s, &format!("`{}`", observe.path)).map(ObservedValue::Text)
                }
                Value::Bool(b) => Ok(ObservedValue::Bool(*b)),
                Value::Number(_) => finite(v)
                    .map(ObservedValue::Number)
                    .ok_or_else(|| format!("`{}` is not a finite number", observe.path)),
                _ => Err(format!("`{}` is not a scalar", observe.path)),
            }
        }
    }
}

/// Whether `path` in `doc` holds `equals` (a connector's `complete_when`).
/// A path that does not reach one value is not complete. Two integers
/// compare by their exact digits; a number with a fraction compares by
/// value (`1` holds `1.0`). A text `equals` (what a `{{params.*}}`
/// template renders to) holds a number or a bool written the same way
/// (`"false"` holds `false`, `"3"` holds `3`), and an integer against an
/// integer text again compares exact digits.
pub fn holds(doc: &Value, path: &str, equals: &Value) -> bool {
    let Ok(segments) = parse_path(path) else {
        return false;
    };
    match walk(doc, &segments) {
        Ok((v, false)) if v.len() == 1 => same_scalar(v[0], equals),
        _ => false,
    }
}

/// The exact value of an integer number, `None` for one with a fraction.
fn integer(n: &serde_json::Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}

fn same_number(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    match (integer(a), integer(b)) {
        (Some(x), Some(y)) => x == y,
        _ => matches!((a.as_f64(), b.as_f64()), (Some(x), Some(y)) if x == y),
    }
}

fn same_scalar(found: &Value, equals: &Value) -> bool {
    match (found, equals) {
        (Value::Number(a), Value::Number(b)) => same_number(a, b),
        (Value::Number(n), Value::String(s)) => {
            let s = s.trim();
            match (integer(n), s.parse::<i128>()) {
                (Some(x), Ok(y)) => x == y,
                _ => n.to_string() == s || s.parse::<f64>().ok() == n.as_f64(),
            }
        }
        (Value::Bool(b), Value::String(s)) => b.to_string() == s.trim(),
        _ => found == equals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseline::ExpectRule;
    use serde_json::json;

    fn obs(kind: ObservationType, path: &str, count: bool) -> Observe {
        Observe {
            id: "o".into(),
            kind,
            path: path.into(),
            count,
            expect: ExpectRule::default(),
        }
    }

    fn unknown(o: &Observation) -> bool {
        matches!(o, Observation::Unknown { .. })
    }

    #[test]
    fn reads_numbers_counts_lists_and_fields() {
        let doc =
            json!({"totalCount": 37, "data": {"items": [{"id": "a"}, {"id": 2}]}, "ok": true});
        assert_eq!(
            extract(&doc, &obs(ObservationType::Number, "totalCount", false)),
            Observation::value(ObservedValue::Number(37.0))
        );
        assert_eq!(
            extract(&doc, &obs(ObservationType::Number, "data.items", true)),
            Observation::value(ObservedValue::Number(2.0))
        );
        assert_eq!(
            extract(&doc, &obs(ObservationType::List, "data.items[].id", false)),
            Observation::value(ObservedValue::List(vec!["a".into(), "2".into()]))
        );
        assert_eq!(
            extract(&doc, &obs(ObservationType::Field, "ok", false)),
            Observation::value(ObservedValue::Bool(true))
        );
    }

    #[test]
    fn an_over_cap_list_is_unknown_not_truncated() {
        let items: Vec<u32> = (0..=MAX_LIST_ITEMS as u32).collect();
        let doc = json!({ "ids": items });
        assert!(unknown(&extract(
            &doc,
            &obs(ObservationType::List, "ids", false)
        )));
        let long = "x".repeat(MAX_ITEM_CHARS + 1);
        let doc = json!({ "ids": [long] });
        assert!(unknown(&extract(
            &doc,
            &obs(ObservationType::List, "ids", false)
        )));
    }

    #[test]
    fn integer_items_keep_their_exact_digits() {
        let doc: Value =
            serde_json::from_str(r#"{"ids": [9007199254740992, 9007199254740993, -7, 1.5]}"#)
                .unwrap();
        assert_eq!(
            extract(&doc, &obs(ObservationType::List, "ids", false)),
            Observation::value(ObservedValue::List(vec![
                "9007199254740992".into(),
                "9007199254740993".into(),
                "-7".into(),
                "1.5".into()
            ]))
        );
    }

    #[test]
    fn a_list_keeps_each_item_once() {
        let doc = json!({"ids": ["a", "b", "a"]});
        assert_eq!(
            extract(&doc, &obs(ObservationType::List, "ids", false)),
            Observation::value(ObservedValue::List(vec!["a".into(), "b".into()]))
        );
    }

    #[test]
    fn hidden_characters_or_an_over_cap_field_read_unknown() {
        let long = "x".repeat(MAX_ITEM_CHARS + 1);
        for doc in [
            json!({"v": long}),
            json!({"v": "ok\u{202E}evil"}),
            json!({"v": "two\nlines"}),
        ] {
            assert!(
                unknown(&extract(&doc, &obs(ObservationType::Field, "v", false))),
                "{doc}"
            );
        }
        let doc = json!({"ids": ["fine", "zero\u{200B}width"]});
        assert!(unknown(&extract(
            &doc,
            &obs(ObservationType::List, "ids", false)
        )));
    }

    #[test]
    fn counting_a_mapped_path_has_no_list_cap() {
        let items: Vec<Value> = (0..=MAX_LIST_ITEMS).map(|i| json!({ "id": i })).collect();
        let doc = json!({ "items": items });
        assert_eq!(
            extract(&doc, &obs(ObservationType::Number, "items[].id", true)),
            Observation::value(ObservedValue::Number((MAX_LIST_ITEMS + 1) as f64))
        );
        assert!(unknown(&extract(
            &doc,
            &obs(ObservationType::List, "items[].id", false)
        )));
    }

    #[test]
    fn a_type_mismatch_is_unknown_never_zero() {
        let doc = json!({"totalCount": "37", "missing": null});
        for path in ["totalCount", "missing", "absent"] {
            let o = extract(&doc, &obs(ObservationType::Number, path, false));
            assert!(unknown(&o), "{path}: {o:?}");
        }
        assert!(unknown(&extract(
            &doc,
            &obs(ObservationType::Number, "totalCount", true)
        )));
    }

    #[test]
    fn path_syntax() {
        assert!(parse_path("a.b[].c").is_ok());
        assert!(parse_path("[]").is_ok());
        for bad in ["", "a..b", "a.b c", "a.[]", "a[0]"] {
            assert!(parse_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn complete_when_holds_only_on_the_exact_value() {
        let doc = json!({"pageInfo": {"hasNextPage": false}});
        assert!(holds(&doc, "pageInfo.hasNextPage", &json!(false)));
        assert!(!holds(&doc, "pageInfo.hasNextPage", &json!(true)));
        assert!(!holds(&doc, "pageInfo.missing", &json!(false)));
    }

    #[test]
    fn complete_when_compares_numbers_as_numbers_and_rendered_text_by_its_words() {
        let doc: Value = serde_json::from_str(r#"{"n": 1.0, "more": false, "page": 3}"#).unwrap();
        assert!(holds(&doc, "n", &json!(1)));
        assert!(holds(&doc, "more", &json!("false")));
        assert!(holds(&doc, "page", &json!("3")));
        assert!(!holds(&doc, "more", &json!("true")));
        assert!(!holds(&doc, "page", &json!("4")));
    }

    #[test]
    fn complete_when_compares_integers_beyond_double_precision_by_their_digits() {
        let doc: Value = serde_json::from_str(r#"{"id": 9007199254740993, "half": 0.5}"#).unwrap();
        assert!(holds(&doc, "id", &json!(9007199254740993u64)));
        assert!(!holds(&doc, "id", &json!(9007199254740992u64)));
        assert!(holds(&doc, "id", &json!("9007199254740993")));
        assert!(!holds(&doc, "id", &json!("9007199254740992")));
        assert!(holds(&doc, "half", &json!("0.5")));
        let doc: Value = serde_json::from_str(r#"{"id": 9007199254740992}"#).unwrap();
        assert!(!holds(&doc, "id", &json!("9007199254740993")));
    }
}
