//! The evaluation of one stored event against one rule: pure, so a pass
//! and `apb trigger test` run exactly the same checks in exactly the same
//! order.

use crate::connector::common::value_at_path;
use crate::connector::event_filter::ConditionResult;
use crate::connector::inbox::InboxEvent;
use crate::connector::marker;
use crate::start_key::StartKey;
use crate::trigger::model::InboxSource;
use crate::trigger::render::{self, Part, RenderError};

/// How one event came out of the rule's checks.
#[derive(Debug, Clone, PartialEq)]
pub enum EventVerdict {
    /// Every condition passed; the prefixed start key.
    Matched { start_key: StartKey },
    /// A code of the journal's closed set and the rule's own name it is
    /// about.
    Skipped {
        code: &'static str,
        name: Option<String>,
    },
}

/// Decides one event: the filter, the send marker at the paths the rule
/// reads, the key, then every param in name order, the first failure
/// deciding the reason. `own_run` tells whether a marker's run named a run
/// directory of this project that this installation created.
pub fn evaluate_event(
    rule: &crate::trigger::model::Rule,
    source: &InboxSource,
    event: &InboxEvent,
    incarnation: &str,
    own_run: &dyn Fn(&str) -> bool,
) -> (EventVerdict, Vec<ConditionResult>) {
    // 1. The filter: the type condition first, then each `where` entry in
    // the rule's order. Every result comes back either way.
    let conditions = source.filter().explain(event);
    if conditions.iter().any(|c| !c.passed) {
        let type_failed = !source.types.is_empty() && conditions.first().is_some_and(|c| !c.passed);
        let code = if type_failed { "type" } else { "where" };
        return (EventVerdict::Skipped { code, name: None }, conditions);
    }

    // 2. The send marker, exactly where the rule reads.
    if reads_own_send(source, rule, event, own_run) {
        return (
            EventVerdict::Skipped {
                code: "own_send",
                name: None,
            },
            conditions,
        );
    }

    // 3. The key, rendered and prefixed (`trigger:<incarnation>:<key>`),
    // then checked as every start key is. The reasons never repeat the
    // key.
    let key_parts = match render::parse(&source.key) {
        Ok(parts) => parts,
        // A saved rule's templates parse (V260 refuses the rest); a
        // malformed one cannot form a key.
        Err(_) => {
            return skip("key_invalid", None, conditions);
        }
    };
    let rendered = match render::render(&key_parts, event) {
        Ok(text) => text,
        Err(RenderError::Missing(path)) => return skip("key_missing", Some(path), conditions),
        Err(RenderError::NotScalar(path)) => {
            return skip("key_not_scalar", Some(path), conditions);
        }
        Err(RenderError::TooLong) => return skip("key_too_long", None, conditions),
    };
    let prefixed = format!("trigger:{incarnation}:{rendered}");
    let start_key = match StartKey::parse(&prefixed) {
        Ok(key) => key,
        Err(_) => return skip("key_invalid", None, conditions),
    };

    // 4. Every param in name order (`Rule.params` is a `BTreeMap`). The
    // rendered values are dropped: the fire renders them again from the
    // same event.
    for (name, template) in &rule.params {
        let parts = match render::parse(template) {
            Ok(parts) => parts,
            Err(_) => return skip("param_missing", Some(name.clone()), conditions),
        };
        let code = match render::render(&parts, event) {
            Ok(_) => continue,
            Err(RenderError::Missing(_)) => "param_missing",
            Err(RenderError::NotScalar(_)) => "param_not_scalar",
            Err(RenderError::TooLong) => "param_too_long",
        };
        return skip(code, Some(name.clone()), conditions);
    }

    // 5. Every check passed.
    (EventVerdict::Matched { start_key }, conditions)
}

/// A skip with the condition results the filter gave, whatever the reason.
fn skip(
    code: &'static str,
    name: Option<String>,
    conditions: Vec<ConditionResult>,
) -> (EventVerdict, Vec<ConditionResult>) {
    (EventVerdict::Skipped { code, name }, conditions)
}

/// Whether the string at a body path the rule's key and param templates
/// read ends with exactly one well-formed send marker of a run this
/// installation created (`own_run`). A marker in a field the rule does not
/// read, or naming a run another machine made, is not its own send.
fn reads_own_send(
    source: &InboxSource,
    rule: &crate::trigger::model::Rule,
    event: &InboxEvent,
    own_run: &dyn Fn(&str) -> bool,
) -> bool {
    let templates =
        std::iter::once(source.key.as_str()).chain(rule.params.values().map(String::as_str));
    for template in templates {
        let Ok(parts) = render::parse(template) else {
            continue;
        };
        for part in &parts {
            let Part::Body(path) = part else {
                continue;
            };
            let Some(value) = value_at_path(&event.body, path) else {
                continue;
            };
            let Some(text) = value.as_str() else {
                continue;
            };
            // The marker's run comes from the event: one safe path segment
            // before `own_run` turns it into a path.
            if let Some(fields) = marker::trailing_marker(text)
                && crate::registry::is_safe_segment(&fields.run)
                && own_run(&fields.run)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::event_filter::BodyCondition;
    use crate::connector::marker::{self, MarkerFields, MarkerFormat};
    use crate::trigger::model::{Rule, RuleLimits, RulePlaybook, RuleScope, RuleSource};
    use serde_json::json;
    use std::collections::BTreeMap;

    fn make_event(body: serde_json::Value) -> InboxEvent {
        InboxEvent {
            seq: 9,
            received_at: 0,
            provider_id: "p".to_string(),
            event_type: Some("issues".to_string()),
            body,
        }
    }

    /// A rule over the github `issues` example, with the key, params,
    /// types and `where` conditions of the case.
    fn rule(
        key: &str,
        params: &[(&str, &str)],
        types: &[&str],
        conditions: Vec<BodyCondition>,
    ) -> Rule {
        Rule {
            name: "triage-new-issues".to_string(),
            playbook: RulePlaybook {
                id: "issue-triage".to_string(),
                version: "1.2.0".to_string(),
                scope: RuleScope::Project,
            },
            source: RuleSource::Inbox(InboxSource {
                connector: "github".to_string(),
                account: "main".to_string(),
                types: types.iter().map(|t| (*t).to_string()).collect(),
                conditions,
                key: key.to_string(),
            }),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<BTreeMap<String, String>>(),
            limits: RuleLimits { max_per_hour: 5 },
            budget: None,
            external_data: true,
            require_ready: true,
            notify: None,
        }
    }

    fn source_of(rule: &Rule) -> InboxSource {
        match &rule.source {
            RuleSource::Inbox(source) => source.clone(),
        }
    }

    fn send_marker(run: &str) -> String {
        marker::render(
            MarkerFormat::HtmlComment,
            &MarkerFields {
                run: run.to_string(),
                node: "triage".to_string(),
                attempt: Some(1),
                nonce: "abc".to_string(),
            },
        )
    }

    #[test]
    fn conditions_decide_in_order() {
        // The type condition fails first: reason `type`.
        let r = rule("{{event.seq}}", &[], &["pull_request"], vec![]);
        let source = source_of(&r);
        let (verdict, conditions) =
            evaluate_event(&r, &source, &make_event(json!({})), "inc", &|_| false);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "type",
                name: None
            }
        );
        assert_eq!(conditions.len(), 1);
        assert!(!conditions[0].passed);

        // The type passes, a `where` condition fails: reason `where`.
        let r = rule(
            "{{event.seq}}",
            &[],
            &["issues"],
            vec![BodyCondition {
                path: "action".to_string(),
                equals: json!("opened"),
            }],
        );
        let source = source_of(&r);
        let (verdict, conditions) = evaluate_event(
            &r,
            &source,
            &make_event(json!({ "action": "closed" })),
            "inc",
            &|_| false,
        );
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "where",
                name: None
            }
        );
        assert_eq!(conditions.len(), 2);
        assert!(conditions[0].passed);
        assert!(!conditions[1].passed);

        // Everything passes: the conditions come back all passed.
        let (verdict, conditions) = evaluate_event(
            &r,
            &source,
            &make_event(json!({ "action": "opened" })),
            "inc",
            &|_| false,
        );
        assert!(matches!(verdict, EventVerdict::Matched { .. }));
        assert!(conditions.iter().all(|c| c.passed));
    }

    #[test]
    fn own_send_needs_a_marker_of_this_installation_where_the_rule_reads() {
        let marker_text = send_marker("r1");
        let commented = format!("a comment\n\n{marker_text}");
        // The rule reads exactly `body.comment.text`.
        let r = rule(
            "{{event.seq}}",
            &[("note", "{{body.comment.text}}")],
            &["issues"],
            vec![],
        );
        let source = source_of(&r);

        // A well-formed trailing marker of this installation at the path
        // the rule reads: skipped `own_send`.
        let event = make_event(json!({ "comment": { "text": commented } }));
        let (verdict, conditions) = evaluate_event(&r, &source, &event, "inc", &|run| run == "r1");
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "own_send",
                name: None
            }
        );
        assert!(conditions.iter().all(|c| c.passed));

        // The same marker naming a run another machine made: not a skip.
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| false);
        assert!(matches!(verdict, EventVerdict::Matched { .. }));

        // A marker at a field the rule does not read: not a skip. The
        // field the rule does read holds an ordinary text.
        let event = make_event(
            json!({ "comment": { "text": "an ordinary comment" }, "other": { "text": commented } }),
        );
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert!(matches!(verdict, EventVerdict::Matched { .. }));

        // The keyword quoted in the text, no trailing marker: not a skip.
        let event = make_event(
            json!({ "comment": { "text": format!("see apb-intent v1 run=x {marker_text} and more") } }),
        );
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert!(matches!(verdict, EventVerdict::Matched { .. }));
    }

    #[test]
    fn a_long_key_is_invalid() {
        let r = rule("{{body.long}}", &[], &["issues"], vec![]);
        let source = source_of(&r);
        let event = make_event(json!({ "long": "k".repeat(300) }));
        let (verdict, conditions) = evaluate_event(&r, &source, &event, "inc", &|_| false);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "key_invalid",
                name: None
            }
        );
        assert!(conditions.iter().all(|c| c.passed));
    }

    #[test]
    fn the_start_key_names_the_incarnation() {
        let r = rule("{{body.repo}}-{{event.seq}}", &[], &["issues"], vec![]);
        let source = source_of(&r);
        let event = make_event(json!({ "repo": "acme/app" }));
        let (verdict, _) = evaluate_event(&r, &source, &event, "cafe1234", &|_| true);
        match verdict {
            EventVerdict::Matched { start_key } => {
                assert_eq!(start_key.expose(), "trigger:cafe1234:acme/app-9");
            }
            other => panic!("expected a match, got {other:?}"),
        }
    }

    #[test]
    fn a_skip_names_the_rules_path_never_the_value() {
        // A missing key path: the name is the template's path, never a
        // value of the event.
        let r = rule("{{body.missing}}", &[], &["issues"], vec![]);
        let source = source_of(&r);
        let event = make_event(json!({ "repo": "acme/app" }));
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "key_missing",
                name: Some("missing".to_string())
            }
        );

        // A key path that holds an object: the name is the path, never the
        // value there.
        let r = rule("{{body.repo}}", &[], &["issues"], vec![]);
        let source = source_of(&r);
        let event = make_event(json!({ "repo": { "full_name": "acme/app" } }));
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "key_not_scalar",
                name: Some("repo".to_string())
            }
        );

        // A key that renders too long: no name at all.
        let r = rule("{{body.long}}", &[], &["issues"], vec![]);
        let source = source_of(&r);
        let event = make_event(json!({ "long": "x".repeat(5000) }));
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "key_too_long",
                name: None
            }
        );

        // A param that misses: the name is the param's name.
        let r = rule(
            "{{event.seq}}",
            &[("issue", "{{body.missing}}")],
            &["issues"],
            vec![],
        );
        let source = source_of(&r);
        let event = make_event(json!({}));
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "param_missing",
                name: Some("issue".to_string())
            }
        );

        // A param that renders too long: the name is the param's name.
        let r = rule(
            "{{event.seq}}",
            &[("big", "{{body.long}}")],
            &["issues"],
            vec![],
        );
        let source = source_of(&r);
        let event = make_event(json!({ "long": "x".repeat(5000) }));
        let (verdict, _) = evaluate_event(&r, &source, &event, "inc", &|_| true);
        assert_eq!(
            verdict,
            EventVerdict::Skipped {
                code: "param_too_long",
                name: Some("big".to_string())
            }
        );
    }
}
