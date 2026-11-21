//! The filter of an inbox read (spec 2026-11-18-typed-inbox-reads,
//! "The filter"): which events of one account a reader wants. The webhook
//! block names an event's type (`connector::def::WebhookSpec`), the ingest
//! listener stores it on the event, and a read may ask for types and body
//! conditions. One condition compares the value at a dot path of the body
//! exactly as a list's `where` compares an item field
//! (`list::items::field_equals`), so `equals: null` needs the path present
//! and null, a missing path never equals anything, and an object or array
//! never matches.
//!
//! [`EventFilter::explain`] names every condition and how it came out on one
//! event, and never carries body text: the only value it reports is the
//! event's type, which the webhook block's cleaning already bounds.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::inbox::InboxEvent;

/// At most this many types and this many conditions in one filter.
pub const MAX_TYPES: usize = 20;
/// At most this many conditions in one filter.
pub const MAX_CONDITIONS: usize = 10;

/// Whether an event of type `event_type` passes a `types` list: an empty
/// list passes every event, an untyped event passes only an empty list, and
/// a typed event passes a list that names its type.
///
/// The one type test of a run: an inbox read's filter and an inbox wait's
/// `types` list mean the same thing by it.
pub fn type_passes(types: &[String], event_type: Option<&str>) -> bool {
    match event_type {
        None => types.is_empty(),
        Some(t) => types.is_empty() || types.iter().any(|want| want == t),
    }
}

/// Whether `ty` is an event type a stored event can carry: it cleans to
/// itself ([`super::webhook::clean_event_type`]).
pub fn type_is_storable(ty: &str) -> bool {
    super::webhook::clean_event_type(ty).as_deref() == Some(ty)
}

/// The `types` list of one filter or one inbox wait: at most [`MAX_TYPES`]
/// entries, and every one of them an event type a stored event can carry.
/// The error names the first problem.
pub fn check_types(types: &[String]) -> Result<(), String> {
    if types.len() > MAX_TYPES {
        return Err(format!(
            "at most {MAX_TYPES} types in one filter, got {}",
            types.len()
        ));
    }
    for ty in types {
        if !type_is_storable(ty) {
            return Err(format!(
                "event type `{ty}` must be 1 to {} characters of [A-Za-z0-9._:/-]",
                super::webhook::EVENT_TYPE_MAX
            ));
        }
    }
    Ok(())
}

/// The paths of one filter's or one wait's conditions: every one of them a
/// dot path with non-empty segments. The error names the first problem.
pub fn check_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Result<(), String> {
    for path in paths {
        if path.trim().is_empty() || path.split('.').any(|s| s.trim().is_empty()) {
            return Err(format!(
                "condition path `{path}` must be a dot path with non-empty segments"
            ));
        }
    }
    Ok(())
}

/// Which inbox events a reader wants: an event passes when its type is in
/// `types` (when `types` is not empty) and every condition holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventFilter {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    #[serde(default, rename = "where", skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<BodyCondition>,
}

/// One condition over the event body: the value at `path` equals
/// `equals` (a string, number, bool or null; never an object or array).
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyCondition {
    pub path: String,
    #[cfg_attr(feature = "ts", ts(type = "string | number | boolean | null"))]
    pub equals: Value,
}

/// How one condition came out on one event. Never carries body text:
/// `got` is the event type for the type condition, `None` otherwise.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionResult {
    /// `type in [issues, pull_request]` or `action = "opened"`.
    pub condition: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub got: Option<String>,
}

impl EventFilter {
    /// Whether the filter names nothing: every event passes it.
    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.conditions.is_empty()
    }

    /// The filter's own shape: the caps above, every type cleans to itself
    /// ([`super::webhook::clean_event_type`]), every path is a dot path with
    /// non-empty segments, every `equals` is a scalar or null. The error
    /// names the first problem.
    pub fn check(&self) -> Result<(), String> {
        check_types(&self.types)?;
        if self.conditions.len() > MAX_CONDITIONS {
            return Err(format!(
                "at most {MAX_CONDITIONS} conditions in one filter, got {}",
                self.conditions.len()
            ));
        }
        check_paths(self.conditions.iter().map(|c| c.path.as_str()))?;
        for condition in &self.conditions {
            let path = &condition.path;
            if !matches!(
                condition.equals,
                Value::Null | Value::String(_) | Value::Number(_) | Value::Bool(_)
            ) {
                return Err(format!(
                    "condition `{} = {}` must equal a string, number, bool or null",
                    path, condition.equals
                ));
            }
        }
        Ok(())
    }

    /// One result per condition, the type condition first (when `types` is
    /// not empty), in declaration order.
    pub fn explain(&self, event: &InboxEvent) -> Vec<ConditionResult> {
        let mut out = Vec::with_capacity(self.conditions.len() + 1);
        if !self.types.is_empty() {
            let got = event.event_type.clone();
            out.push(ConditionResult {
                condition: format!("type in [{}]", self.types.join(", ")),
                passed: type_passes(&self.types, got.as_deref()),
                got,
            });
        }
        for condition in &self.conditions {
            let got = super::common::value_at_path(&event.body, &condition.path);
            out.push(ConditionResult {
                condition: format!("{} = {}", condition.path, condition.equals),
                passed: crate::list::items::field_equals(got, &condition.equals),
                got: None,
            });
        }
        out
    }

    /// Every result of [`explain`](Self::explain) passed, decided without
    /// building them: the type first, then each condition, stopping at the
    /// first that fails.
    pub fn matches(&self, event: &InboxEvent) -> bool {
        type_passes(&self.types, event.event_type.as_deref())
            && self.conditions.iter().all(|c| {
                crate::list::items::field_equals(
                    super::common::value_at_path(&event.body, &c.path),
                    &c.equals,
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn untyped_event(body: Value) -> InboxEvent {
        InboxEvent {
            seq: 1,
            received_at: 0,
            provider_id: "p".to_string(),
            event_type: None,
            body,
        }
    }

    fn typed_event(ty: &str, body: Value) -> InboxEvent {
        InboxEvent {
            seq: 1,
            received_at: 0,
            provider_id: "p".to_string(),
            event_type: Some(ty.to_string()),
            body,
        }
    }

    /// `matches` decides exactly what every `explain` result passing says,
    /// over typed and untyped events and every filter shape.
    #[test]
    fn matches_agrees_with_explain() {
        let cond = |path: &str, equals: Value| BodyCondition {
            path: path.to_string(),
            equals,
        };
        let filters = [
            EventFilter::default(),
            EventFilter {
                types: vec!["issues".to_string()],
                conditions: Vec::new(),
            },
            EventFilter {
                types: Vec::new(),
                conditions: vec![cond("action", json!("opened"))],
            },
            EventFilter {
                types: vec!["issues".to_string(), "push".to_string()],
                conditions: vec![cond("action", json!("opened")), cond("n", json!(1))],
            },
        ];
        let events = [
            untyped_event(json!({ "action": "opened", "n": 1 })),
            typed_event("issues", json!({ "action": "opened", "n": 1 })),
            typed_event("issues", json!({ "action": "closed", "n": 1 })),
            typed_event("push", json!({ "action": "opened", "n": 2 })),
            typed_event("other", json!({})),
        ];
        for f in &filters {
            for e in &events {
                assert_eq!(
                    f.matches(e),
                    f.explain(e).iter().all(|r| r.passed),
                    "{f:?} on {e:?}"
                );
            }
        }
    }

    #[test]
    fn an_untyped_event_never_passes_a_type_condition() {
        let filter = EventFilter {
            types: vec!["issues".to_string()],
            conditions: Vec::new(),
        };
        assert!(!filter.matches(&untyped_event(json!({ "action": "opened" }))));
        let results = filter.explain(&untyped_event(json!({})));
        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
        assert_eq!(results[0].got, None);
        assert_eq!(results[0].condition, "type in [issues]");
        // A typed event of another type passes nothing either.
        assert!(!filter.matches(&typed_event("push", json!({}))));
        // The right type passes.
        assert!(filter.matches(&typed_event("issues", json!({}))));
    }

    #[test]
    fn a_condition_reads_a_dot_path() {
        let filter = EventFilter {
            types: Vec::new(),
            conditions: vec![BodyCondition {
                path: "issue.0.state".to_string(),
                equals: json!("open"),
            }],
        };
        assert!(filter.check().is_ok());
        let hit = json!({ "issue": [{ "state": "open", "note": "SECRET-BODY" }] });
        assert!(filter.matches(&untyped_event(hit.clone())));
        let results = filter.explain(&untyped_event(hit));
        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
        assert_eq!(results[0].got, None);
        assert_eq!(results[0].condition, r#"issue.0.state = "open""#);
        // A path that does not resolve never matches.
        let miss = json!({ "issue": [{ "state": "closed" }] });
        assert!(!filter.matches(&untyped_event(miss)));
    }

    #[test]
    fn equals_null_needs_a_present_null() {
        let filter = EventFilter {
            types: Vec::new(),
            conditions: vec![BodyCondition {
                path: "merged_at".to_string(),
                equals: Value::Null,
            }],
        };
        assert!(filter.check().is_ok());
        assert!(filter.matches(&untyped_event(json!({ "merged_at": null }))));
        // A missing path never equals anything.
        assert!(!filter.matches(&untyped_event(json!({ "other": null }))));
    }

    #[test]
    fn an_object_never_equals() {
        let filter = EventFilter {
            types: Vec::new(),
            conditions: vec![BodyCondition {
                path: "a".to_string(),
                equals: json!({ "x": 1 }),
            }],
        };
        // A body object at the path never matches a scalar `equals` either.
        let against_scalar = EventFilter {
            types: Vec::new(),
            conditions: vec![BodyCondition {
                path: "a".to_string(),
                equals: json!("nomatch"),
            }],
        };
        assert!(!against_scalar.matches(&untyped_event(json!({ "a": { "b": 1 } }))));
        assert!(!against_scalar.matches(&untyped_event(json!({ "a": [1, 2] }))));
        assert!(filter.check().is_err(), "an object equals is refused");
    }

    #[test]
    fn check_refuses_an_object_equals() {
        let filter = EventFilter {
            types: Vec::new(),
            conditions: vec![BodyCondition {
                path: "a".to_string(),
                equals: json!([1]),
            }],
        };
        assert!(filter.check().is_err());
        // A bad path is refused too.
        let bad_path = EventFilter {
            types: Vec::new(),
            conditions: vec![BodyCondition {
                path: "a..b".to_string(),
                equals: json!(1),
            }],
        };
        assert!(bad_path.check().is_err());
        // A type that does not clean to itself is refused.
        let bad_type = EventFilter {
            types: vec!["issues open".to_string()],
            conditions: Vec::new(),
        };
        assert!(bad_type.check().is_err());
    }

    #[test]
    fn explain_never_carries_body_text() {
        let filter = EventFilter {
            types: vec!["issues".to_string()],
            conditions: vec![BodyCondition {
                path: "action".to_string(),
                equals: json!("opened"),
            }],
        };
        let event = typed_event(
            "issues",
            json!({ "action": "opened", "text": "SECRET-BODY" }),
        );
        let results = filter.explain(&event);
        let rendered = format!("{results:?}");
        assert!(
            !rendered.contains("SECRET-BODY"),
            "an explanation never carries body text: {rendered}"
        );
    }
}
