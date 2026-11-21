//! The `wait_for: { type: inbox }` block (inbox-waits design D4 "Schema"):
//! the types a playbook writes, the wait's binding site and binding, and
//! the pure matching and value rules the drive and the park waker share.
//!
//! A wait makes no connector call. Its site [`inbox_site`] grants no
//! function; it exists so the run gate trusts the wait's connector and
//! its exact account. The wait's output is the matching event's `seq`,
//! `received_at` and `type` plus the values of the fields the playbook
//! declares ([`Take`]); no event body leaves the inbox store.

use serde_json::Value;

// The block's types (`InboxWait`, `Condition`, `Take`) are defined in
// `crate::schema` beside the `WaitFor::Inbox` variant that carries them,
// so the definition graph stays one-directional; every path this module
// documents names them through here.
pub use crate::schema::{Condition, InboxWait, Take};

use crate::schema::{ConnectorBinding, FunctionsAllow};

/// The most `take` entries one wait declares.
pub const MAX_TAKE: usize = 16;
/// Output keys apb sets itself; a `take` name may not be one of them.
pub const RESERVED_NAMES: [&str; 3] = ["seq", "received_at", "type"];
/// The least time between two reads of the account's log by a live
/// visit, and only when the log changed (D4 "The visit" step 3).
pub const MIN_READ_INTERVAL_MS: u64 = 1_000;

/// The binding site of node `node`'s inbox wait: `<node>#inbox`.
pub fn inbox_site(node: &str) -> String {
    format!("{node}#inbox")
}

/// The binding the site carries: the connector, exactly the one
/// account, no function, no call budget, untrusted.
pub fn binding(w: &InboxWait) -> ConnectorBinding {
    ConnectorBinding {
        name: w.connector.clone(),
        accounts: Some(vec![w.account.clone()]),
        functions: FunctionsAllow::List(Vec::new()),
        max_calls: None,
        on_unknown: None,
        untrusted: true,
    }
}

/// Whether an event matches (`types`, then every condition).
pub fn matches(w: &InboxWait, body: &Value, rendered: &[String], event_type: Option<&str>) -> bool {
    if !crate::connector::event_filter::type_passes(&w.types, event_type) {
        return false;
    }
    // `rendered` is one text per condition, in order; a caller that hands
    // anything else is wrong, so nothing matches.
    debug_assert_eq!(
        w.conditions.len(),
        rendered.len(),
        "one rendered text per condition"
    );
    if w.conditions.len() != rendered.len() {
        return false;
    }
    w.conditions
        .iter()
        .zip(rendered.iter())
        .all(|(c, text)| value_text(body, &c.path).is_some_and(|v| v == *text))
}

/// The text of the body value at `path` (`connector::common::value_at_path`):
/// a string as it is, anything else as compact JSON; `None` when the path
/// does not resolve.
pub fn value_text(body: &Value, path: &str) -> Option<String> {
    match crate::connector::common::value_at_path(body, path)? {
        Value::String(s) => Some(s.clone()),
        v => Some(v.to_string()),
    }
}

/// The output object of a matching event: `seq`, `received_at`, `type`
/// (absent when the event has none) and one key per `take` entry.
pub fn output(
    w: &InboxWait,
    seq: u64,
    received_at: u64,
    event_type: Option<&str>,
    body: &Value,
) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    out.insert("seq".into(), Value::from(seq));
    out.insert("received_at".into(), Value::from(received_at));
    if let Some(t) = event_type {
        out.insert("type".into(), Value::from(t));
    }
    for take in &w.take {
        // The baseline observation reader with its limits unchanged: a
        // text over MAX_ITEM_CHARS, one with a hidden character, a
        // missing key, a non-scalar or a number that is not finite reads
        // as unknown, stored as `null` without its reason.
        let observation = crate::baseline::extract::extract(
            body,
            &crate::baseline::Observe {
                id: take.name.clone(),
                kind: take.kind.unwrap_or(crate::baseline::ObservationType::Field),
                path: take.path.clone(),
                count: false,
                expect: crate::baseline::ExpectRule::default(),
            },
        );
        let value = match observation.as_value() {
            Some(crate::baseline::ObservedValue::Text(s)) => Value::from(s.as_str()),
            Some(crate::baseline::ObservedValue::Bool(b)) => Value::from(*b),
            Some(crate::baseline::ObservedValue::Number(n)) => Value::from(*n),
            _ => Value::Null,
        };
        out.insert(take.name.clone(), value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain_wait() -> InboxWait {
        InboxWait {
            connector: "echo-hooks".into(),
            account: "main".into(),
            conditions: Vec::new(),
            types: Vec::new(),
            take: Vec::new(),
        }
    }

    /// A wait with no conditions and no types matches every stored event,
    /// typed or not.
    #[test]
    fn a_plain_wait_matches_every_event() {
        let w = plain_wait();
        assert!(matches(&w, &json!({ "a": 1 }), &[], None));
        assert!(matches(&w, &json!({ "a": 1 }), &[], Some("push")));
        assert!(matches(&w, &json!(null), &[], Some("issues.opened")));
    }

    /// Every condition must meet: the body value's text at the path
    /// equals the rendered text, a string as it is and anything else as
    /// compact JSON. A condition over a path that does not resolve never
    /// matches.
    #[test]
    fn conditions_compare_rendered_text() {
        let w = InboxWait {
            conditions: vec![
                Condition {
                    path: "from".into(),
                    equals: "{{params.customer}}".into(),
                },
                Condition {
                    path: "count".into(),
                    equals: "42".into(),
                },
            ],
            ..plain_wait()
        };
        // Both conditions met: the string as it is, the number as compact
        // JSON text.
        assert!(matches(
            &w,
            &json!({ "from": "acme", "count": 42 }),
            &["acme".to_string(), "42".to_string()],
            None
        ));
        // One condition short: no match.
        assert!(!matches(
            &w,
            &json!({ "from": "acme", "count": 41 }),
            &["acme".to_string(), "42".to_string()],
            None
        ));
        // A path that does not resolve does not match, whatever the render.
        let missing = InboxWait {
            conditions: vec![Condition {
                path: "nothere".into(),
                equals: "x".into(),
            }],
            ..plain_wait()
        };
        assert!(!matches(&missing, &json!({}), &["x".to_string()], None));
        // Objects and bools compare as compact JSON.
        let object = InboxWait {
            conditions: vec![Condition {
                path: "pr".into(),
                equals: "{\"n\":7}".into(),
            }],
            ..plain_wait()
        };
        assert!(matches(
            &object,
            &json!({ "pr": { "n": 7 } }),
            &["{\"n\":7}".to_string()],
            None
        ));
        assert_eq!(
            value_text(&json!({ "pr": { "n": 7 } }), "pr").as_deref(),
            Some("{\"n\":7}")
        );
        assert_eq!(
            value_text(&json!({ "on": true }), "on").as_deref(),
            Some("true")
        );
        assert_eq!(value_text(&json!({ "nope": 1 }), "missing"), None);
    }

    /// A `types` list needs the event's type: an untyped event passes
    /// only an empty list; an empty list passes every event.
    #[test]
    fn type_passes_needs_a_type_when_listed() {
        let types = vec!["push".to_string(), "issues".to_string()];
        assert!(crate::connector::event_filter::type_passes(
            &types,
            Some("push")
        ));
        assert!(crate::connector::event_filter::type_passes(
            &types,
            Some("issues")
        ));
        assert!(!crate::connector::event_filter::type_passes(
            &types,
            Some("release")
        ));
        assert!(
            !crate::connector::event_filter::type_passes(&types, None),
            "an untyped event never passes a list"
        );
        assert!(crate::connector::event_filter::type_passes(
            &[],
            Some("release")
        ));
        assert!(
            crate::connector::event_filter::type_passes(&[], None),
            "an empty list passes an untyped event too"
        );
        // A listed wait matches only the listed types.
        let w = InboxWait {
            types: types.clone(),
            ..plain_wait()
        };
        assert!(matches(&w, &json!({}), &[], Some("push")));
        assert!(!matches(&w, &json!({}), &[], Some("release")));
        assert!(!matches(&w, &json!({}), &[], None));
    }

    /// The output holds the fixed keys and one key per take entry, the
    /// values read by the baseline observation reader: text as a string,
    /// a bool as a bool, a number as a number.
    #[test]
    fn output_keeps_only_declared_values() {
        let w = InboxWait {
            take: vec![
                Take {
                    name: "action".into(),
                    path: "action".into(),
                    kind: None,
                },
                Take {
                    name: "n".into(),
                    path: "count".into(),
                    kind: Some(crate::baseline::ObservationType::Number),
                },
                Take {
                    name: "ok".into(),
                    path: "approved".into(),
                    kind: Some(crate::baseline::ObservationType::Field),
                },
            ],
            ..plain_wait()
        };
        let body = json!({ "action": "close", "count": 7, "approved": true, "secret": "hide me" });
        let out = output(&w, 9, 1_000, Some("push"), &body);
        assert_eq!(out.get("seq"), Some(&json!(9)));
        assert_eq!(out.get("received_at"), Some(&json!(1_000)));
        assert_eq!(out.get("type"), Some(&json!("push")));
        assert_eq!(out.get("action"), Some(&json!("close")));
        // A number comes out of the observation reader as an f64: 7 reads
        // as 7.0.
        assert_eq!(out.get("n").and_then(|v| v.as_f64()), Some(7.0));
        assert_eq!(out.get("ok"), Some(&json!(true)));
        // A field no take entry names never reaches the output.
        assert!(out.get("secret").is_none());
        assert_eq!(out.len(), 6);
        // No type: the key is absent, not null.
        let out = output(&w, 9, 1_000, None, &body);
        assert!(!out.contains_key("type"));
        assert_eq!(out.len(), 5);
        // An empty take holds the fixed keys only.
        let empty = output(&plain_wait(), 3, 500, Some("push"), &body);
        assert_eq!(empty.len(), 3);
    }

    /// A value the reader cannot read comes out as `null`: a missing key,
    /// a text over the observation limit, a text with a hidden character.
    #[test]
    fn an_unreadable_value_is_null() {
        let long = "x".repeat(crate::baseline::MAX_ITEM_CHARS + 1);
        let w = InboxWait {
            take: vec![
                Take {
                    name: "gone".into(),
                    path: "nope".into(),
                    kind: None,
                },
                Take {
                    name: "long".into(),
                    path: "text".into(),
                    kind: None,
                },
                Take {
                    name: "hidden".into(),
                    path: "marked".into(),
                    kind: None,
                },
            ],
            ..plain_wait()
        };
        let body = json!({
            "text": long,
            "marked": format!("a{}b", '\u{200b}'),
        });
        let out = output(&w, 1, 0, None, &body);
        assert_eq!(out.get("gone"), Some(&Value::Null));
        assert_eq!(out.get("long"), Some(&Value::Null));
        assert_eq!(out.get("hidden"), Some(&Value::Null));
        // A text of exactly the limit still reads.
        let w = InboxWait {
            take: vec![Take {
                name: "fit".into(),
                path: "text".into(),
                kind: None,
            }],
            ..plain_wait()
        };
        let body = json!({ "text": "x".repeat(crate::baseline::MAX_ITEM_CHARS) });
        let out = output(&w, 1, 0, None, &body);
        assert_eq!(
            out.get("fit").and_then(|v| v.as_str()).map(str::len),
            Some(crate::baseline::MAX_ITEM_CHARS)
        );
    }

    /// The site of a node names the wait: `<node>#inbox`.
    #[test]
    fn inbox_site_round_trips() {
        assert_eq!(inbox_site("w"), "w#inbox");
        assert_eq!(inbox_site("a#check"), "a#check#inbox");
    }

    /// The wait and an inbox read's filter agree on the type test: the one
    /// predicate both use.
    #[test]
    fn the_wait_and_the_filter_share_the_type_test() {
        let types = vec!["push".to_string()];
        let filter = crate::connector::event_filter::EventFilter {
            types: types.clone(),
            conditions: Vec::new(),
        };
        let wait = InboxWait {
            types: types.clone(),
            ..plain_wait()
        };
        for event_type in [None, Some("push"), Some("release")] {
            let event = crate::connector::inbox::InboxEvent {
                seq: 1,
                received_at: 0,
                provider_id: "p".to_string(),
                event_type: event_type.map(str::to_string),
                body: json!({}),
            };
            assert_eq!(
                filter.matches(&event),
                matches(&wait, &json!({}), &[], event_type),
                "{event_type:?}"
            );
        }
    }
}
