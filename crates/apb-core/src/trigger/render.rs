//! The templates of a rule: the key and the params a rule renders against
//! one stored event. Body paths go through `connector::common::
//! value_at_path`, so a missing path and a value that is not a scalar are
//! told apart.

use crate::connector::common::value_at_path;
use crate::connector::inbox::InboxEvent;

/// A rendered param or key longer than this is refused. Lives here: this
/// module enforces it, and a back-reference to `rule` from here would
/// close a module cycle.
pub const RENDERED_MAX_CHARS: usize = 4096;

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Text(String),
    Body(String),
    EventType,
    EventSeq,
    ReceivedAt,
}

/// Parses `text {{body.a.b}} {{event.type}} {{event.seq}}
/// {{event.received_at}}`. Any other placeholder, an empty path or an
/// unclosed `{{` is an error.
pub fn parse(template: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(at) = rest.find("{{") {
        if at > 0 {
            parts.push(Part::Text(rest[..at].to_string()));
        }
        let after = &rest[at + "{{".len()..];
        let end = after
            .find("}}")
            .ok_or_else(|| format!("unclosed `{{{{` in `{template}`"))?;
        parts.push(placeholder(&after[..end])?);
        rest = &after[end + "}}".len()..];
    }
    if !rest.is_empty() {
        parts.push(Part::Text(rest.to_string()));
    }
    Ok(parts)
}

/// One `{{ ... }}` placeholder, by its inner text.
fn placeholder(name: &str) -> Result<Part, String> {
    if let Some(path) = name.strip_prefix("body.") {
        if path.is_empty() {
            return Err("an empty path in `{{body.}}`; name a dot path after `body.`".into());
        }
        return Ok(Part::Body(path.to_string()));
    }
    match name {
        "event.type" => Ok(Part::EventType),
        "event.seq" => Ok(Part::EventSeq),
        "event.received_at" => Ok(Part::ReceivedAt),
        _ => Err(format!(
            "unknown placeholder `{{{{{name}}}}}`; supported are `{{{{body.<dot path>}}}}`, `{{{{event.type}}}}`, `{{{{event.seq}}}}` and `{{{{event.received_at}}}}`"
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// The path holds nothing; the name is the template's path exactly as
    /// written after `body.`, or `event.type`.
    Missing(String),
    /// The path holds an empty string, null, an object or an array.
    NotScalar(String),
    /// The rendered text is longer than `RENDERED_MAX_CHARS`.
    TooLong,
}

/// Renders against one stored event: a body path through
/// `connector::common::value_at_path`, which tells the two reasons apart:
/// `None` is `Missing`; `Some` of a non-empty string, a number or a bool
/// renders as `scalar_at_path` would; `Some` of an empty string, null, an
/// object or an array is `NotScalar`. `scalar_at_path` alone is not used:
/// it answers `None` for a missing path and a non-scalar alike. `event.type`
/// is the stored type (`Missing` named `event.type` when untyped),
/// `event.seq` the seq, `event.received_at` RFC 3339 UTC
/// (`clock::iso_utc`). Longer than `RENDERED_MAX_CHARS` is `TooLong`.
pub fn render(parts: &[Part], event: &InboxEvent) -> Result<String, RenderError> {
    let mut out = String::new();
    for part in parts {
        match part {
            Part::Text(text) => out.push_str(text),
            Part::Body(path) => out.push_str(&body_text(path, event)?),
            Part::EventType => match &event.event_type {
                Some(ty) => out.push_str(ty),
                None => return Err(RenderError::Missing("event.type".to_string())),
            },
            Part::EventSeq => out.push_str(&event.seq.to_string()),
            Part::ReceivedAt => out.push_str(&crate::clock::iso_utc(event.received_at)),
        }
        if out.len() > RENDERED_MAX_CHARS {
            return Err(RenderError::TooLong);
        }
    }
    Ok(out)
}

/// The text at one body path, or why there is none.
fn body_text(path: &str, event: &InboxEvent) -> Result<String, RenderError> {
    let Some(value) = value_at_path(&event.body, path) else {
        return Err(RenderError::Missing(path.to_string()));
    };
    match value {
        serde_json::Value::String(s) if !s.is_empty() => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        serde_json::Value::Bool(b) => Ok(b.to_string()),
        _ => Err(RenderError::NotScalar(path.to_string())),
    }
}

/// Whether the template reads event text (a body path or the type).
pub fn reads_event_text(parts: &[Part]) -> bool {
    parts
        .iter()
        .any(|p| matches!(p, Part::Body(_) | Part::EventType))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_event(body: serde_json::Value) -> InboxEvent {
        InboxEvent {
            seq: 41,
            received_at: 0,
            provider_id: "p".to_string(),
            event_type: Some("issues".to_string()),
            body,
        }
    }

    #[test]
    fn a_template_renders_body_paths_and_the_event() {
        let parts = parse(
            "{{body.repository.full_name}}#{{body.issue.number}} ({{event.type}}, seq {{event.seq}} at {{event.received_at}})",
        )
        .expect("the template parses");
        let event = make_event(json!({
            "repository": { "full_name": "acme/app" },
            "issue": { "number": 7 },
        }));
        let rendered = render(&parts, &event).expect("it renders");
        assert_eq!(
            rendered,
            "acme/app#7 (issues, seq 41 at 1970-01-01T00:00:00Z)"
        );
    }

    #[test]
    fn an_unknown_placeholder_does_not_parse() {
        // An unknown name, an empty body path and an unclosed `{{` are each
        // an error naming the problem.
        assert!(parse("{{event.body}}").is_err());
        assert!(parse("{{body.}}").is_err());
        assert!(parse("{{}}").is_err());
        assert!(parse("text {{body.a and more").is_err());
        let why = parse("{{event.body}}").expect_err("an unknown placeholder is refused");
        assert!(why.contains("event.body"), "names the placeholder: {why}");
        assert!(
            parse("{{body.}}")
                .expect_err("an empty path is refused")
                .contains("empty"),
            "names the empty path"
        );
        assert!(
            parse("text {{body.a and more")
                .expect_err("an unclosed brace is refused")
                .contains("unclosed"),
            "names the unclosed brace"
        );
    }

    #[test]
    fn an_object_is_not_scalar() {
        let parts = parse("{{body.issue}}").expect("the template parses");
        let event = make_event(json!({ "issue": { "number": 7 } }));
        assert_eq!(
            render(&parts, &event),
            Err(RenderError::NotScalar("issue".to_string()))
        );
        let event = make_event(json!({ "issue": ["a"] }));
        assert_eq!(
            render(&parts, &event),
            Err(RenderError::NotScalar("issue".to_string()))
        );
    }

    #[test]
    fn a_missing_path_and_an_empty_string_differ() {
        // A missing path is `Missing`; an empty string at a present path is
        // `NotScalar`. Rendering through `scalar_at_path` alone would give
        // `None` for both.
        let missing = parse("{{body.a}}").expect("the template parses");
        let empty = parse("{{body.e}}").expect("the template parses");
        let event = make_event(json!({ "e": "" }));
        assert_eq!(
            render(&missing, &event),
            Err(RenderError::Missing("a".to_string()))
        );
        assert_eq!(
            render(&empty, &event),
            Err(RenderError::NotScalar("e".to_string()))
        );
    }

    #[test]
    fn a_render_longer_than_the_cap_is_too_long() {
        let parts = parse("{{body.long}}").expect("the template parses");
        let event = make_event(json!({ "long": "x".repeat(RENDERED_MAX_CHARS + 1) }));
        assert_eq!(render(&parts, &event), Err(RenderError::TooLong));
        // Exactly at the cap still renders.
        let event = make_event(json!({ "long": "x".repeat(RENDERED_MAX_CHARS) }));
        assert!(render(&parts, &event).is_ok());
    }

    #[test]
    fn reads_event_text_sees_the_type() {
        assert!(reads_event_text(&[Part::EventType]));
        assert!(reads_event_text(&[Part::Body("a".into())]));
        assert!(!reads_event_text(&[Part::Text("x".into())]));
        assert!(!reads_event_text(&[Part::EventSeq]));
        assert!(!reads_event_text(&[Part::ReceivedAt]));
        let parts = parse("{{event.seq}} and {{event.received_at}}").expect("the template parses");
        assert!(!reads_event_text(&parts));
        let parts = parse("{{event.type}}").expect("the template parses");
        assert!(reads_event_text(&parts));
    }
}
