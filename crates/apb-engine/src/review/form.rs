//! The engine-side views of a review gate's form (people-in-the-loop
//! D3): what a request shows beside its options (the rendered `show:`
//! entries and field defaults), and the consume-time check of the values
//! a decision carries. The playbook-side types live in
//! `apb_core::review_form`.

use std::path::Path;

pub use super::views::{FieldView, ShownEntry};

/// What a request shows beside its options, before the scope is taken.
#[derive(Debug, Clone, Default)]
pub struct GateDetail {
    pub show: Vec<ShownEntry>,
    pub fields: Vec<FieldView>,
    pub max_scope: Option<apb_core::review_form::MaxScope>,
    /// The options the request offers, for the withheld check.
    pub options: Vec<String>,
    /// The seq the scope window may not start before (0 for every gate of
    /// this change; see D2 `window_start`).
    pub floor_seq: u64,
}

/// The `review_detail` of one open request: what the request shows beside
/// its options, read from the journal.
#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub scope: Option<crate::review::scope::ReviewScope>,
    pub show: Vec<ShownEntry>,
    pub fields: Vec<FieldView>,
    pub withheld: Vec<String>,
    pub withheld_reason: Option<String>,
}

/// The latest `ReviewDetail` of `node` whose `request` is the open
/// request's seq (`crate::review::open_request`); `None` when no detail
/// answers the open request (an older journal, a request not yet
/// declared).
pub fn detail_of(events: &[crate::event::Event], node: &str) -> Option<Detail> {
    let (request, _) = crate::review::open_request(events, node)?;
    events.iter().rev().find_map(|e| match &e.payload {
        crate::event::EventPayload::ReviewDetail {
            node: n,
            request: r,
            scope,
            show,
            fields,
            withheld,
            withheld_reason,
        } if n == node && *r == request => Some(Detail {
            scope: scope.clone(),
            show: show.clone(),
            fields: fields.clone(),
            withheld: withheld.clone(),
            withheld_reason: withheld_reason.clone(),
        }),
        _ => None,
    })
}

/// The rendered field defaults of a request's detail, by field name; none
/// without a detail.
pub(crate) fn defaults_of(
    detail: Option<&Detail>,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    detail
        .map(|d| {
            d.fields
                .iter()
                .filter_map(|f| f.default.clone().map(|v| (f.name.clone(), v)))
                .collect()
        })
        .unwrap_or_default()
}

/// The reason a run ends on when the drive refuses a decision's field
/// values it consumed (`review_fields_refused`).
pub const FIELDS_REFUSED: &str = "review_fields_refused";

/// The consume-time check of the values a decision carries
/// (people-in-the-loop D3), over the request's `review_detail`: the
/// withheld options are refused again, `check_values` runs against the
/// gate's `fields` and the request's rendered defaults, and the values
/// are scrubbed of the run's secret values. `Ok(None)` for a gate without
/// fields; `Err(reason)` ends the run.
pub(crate) fn settle(
    root: &Path,
    run_dir: &Path,
    playbook: &apb_core::schema::Playbook,
    node: &str,
    decision: &str,
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
    events: &[crate::event::Event],
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, String> {
    settle_unscrubbed(root, run_dir, playbook, node, decision, fields, events).map_err(|reason| {
        refusal_text(
            &crate::run_redact::RunRedactor::for_run(root, run_dir),
            &reason,
        )
    })
}

/// The longest refusal reason the run journals, in bytes.
const REFUSAL_MAX_BYTES: usize = 300;

/// A refusal reason as the run journals it: scrubbed of the run's secret
/// values (withheld whole when they do not resolve), then cut at
/// [`REFUSAL_MAX_BYTES`] on a char boundary. A hand-written channel line
/// is free text: a decision word or a field name may carry anything. The
/// channel's refusals of the same checks go through it too
/// (`review::check_form`).
pub(crate) fn refusal_text(redactor: &crate::run_redact::RunRedactor, reason: &str) -> String {
    let mut text = redactor
        .secret_text(reason)
        .unwrap_or_else(|_| crate::run_redact::WITHHELD.to_string());
    if text.len() > REFUSAL_MAX_BYTES {
        let mut cut = REFUSAL_MAX_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

fn settle_unscrubbed(
    root: &Path,
    run_dir: &Path,
    playbook: &apb_core::schema::Playbook,
    node: &str,
    decision: &str,
    fields: &std::collections::BTreeMap<String, serde_json::Value>,
    events: &[crate::event::Event],
) -> Result<Option<std::collections::BTreeMap<String, serde_json::Value>>, String> {
    let Some(gate) = playbook.node(node) else {
        return Ok(None);
    };
    let apb_core::schema::NodeKind::HumanReview {
        fields: spec,
        max_scope,
        options,
        ..
    } = &gate.kind
    else {
        return Ok(None);
    };
    // Only a gate that uses a form feature journals a detail
    // (`NodeKind::needs_review_detail`); for such a gate a missing
    // detail fails closed, it is never read as a gate without the feature.
    if !gate.kind.needs_review_detail() {
        return Ok(None);
    }
    let Some(detail) = detail_of(events, node) else {
        return Err(format!(
            "the detail of the request of gate `{node}` is not recorded; its form cannot be checked"
        ));
    };
    let detail = Some(detail);
    // The withheld check again: a hand-written channel line may name an
    // option the gate refuses above its scope. Without a recorded scope it
    // fails closed: every option `max_scope` may withhold is refused.
    if let Some(max) = max_scope {
        let offered = apb_core::schema::effective_review_options(options);
        let (withheld, reason) = match detail.as_ref().and_then(|d| d.scope.as_ref()) {
            Some(scope) => crate::review::scope::withheld(max, scope, &offered),
            None => (
                max.withhold_for(&offered),
                Some("the scope of the request is not recorded".to_string()),
            ),
        };
        if withheld.iter().any(|o| o == decision) {
            return Err(format!(
                "option `{decision}` of gate `{node}` is withheld: {}",
                reason.unwrap_or_default()
            ));
        }
    }
    if spec.is_empty() {
        return Ok(None);
    }
    // The defaults the request rendered.
    let defaults = defaults_of(detail.as_ref());
    let mut values = apb_core::review_form::check_values(spec, decision, fields, &defaults)?;
    // Scrubbed of the run's secret values before they are journaled.
    let mut json =
        serde_json::Value::Object(values.iter().map(|(k, v)| (k.clone(), v.clone())).collect());
    if crate::run_redact::RunRedactor::for_run(root, run_dir)
        .secret_json(&mut json)
        .is_err()
    {
        return Err(
            "the run's secret values are unresolved; the field values are withheld".to_string(),
        );
    }
    values = serde_json::from_value(json).unwrap_or_default();
    Ok(Some(values))
}

#[cfg(test)]
mod tests {
    use super::settle;
    use crate::event::{Event, EventPayload};

    fn playbook(gate: &str) -> apb_core::schema::Playbook {
        apb_core::schema::Playbook::from_yaml(&format!(
            "schema: 2\nid: p\nname: p\nversion: 1.0.0\nnodes:\n  - {{ id: start, type: start }}\n  - {gate}\n  - {{ id: done, type: finish, outcome: success }}\nedges:\n  - {{ from: start, to: gate }}\n  - {{ from: gate, to: done }}\n"
        ))
        .unwrap()
    }

    /// The journal of a gate waiting with its request alone.
    fn request_only() -> Vec<Event> {
        vec![Event {
            seq: 1,
            ts: 1,
            payload: serde_json::from_value(serde_json::json!({
                "type": "review_requested", "node": "gate", "options": ["approve", "reject"]
            }))
            .unwrap(),
        }]
    }

    /// The consume-time check reads a missing detail of a gate that uses
    /// a form feature as a refusal, never as a gate without the feature;
    /// a plain gate has no detail to read and passes.
    #[test]
    fn a_missing_detail_fails_closed_only_where_the_gate_needs_one() {
        let dir = tempfile::tempdir().unwrap();
        let run = |gate: &str| {
            settle(
                dir.path(),
                dir.path(),
                &playbook(gate),
                "gate",
                "approve",
                &Default::default(),
                &request_only(),
            )
        };
        assert_eq!(
            run("{ id: gate, type: human_review, options: [approve, reject] }"),
            Ok(None)
        );
        for gate in [
            "{ id: gate, type: human_review, options: [approve, reject], max_scope: { files: 3 } }",
            "{ id: gate, type: human_review, options: [approve, reject], fields: [{ name: note, type: text }] }",
            "{ id: gate, type: human_review, options: [approve, reject], show: [{ label: L, text: t }] }",
        ] {
            let refused = run(gate).unwrap_err();
            assert!(refused.contains("is not recorded"), "{gate}");
        }
        // With its detail, the same field gate is checked as usual.
        let mut events = request_only();
        events.push(Event {
            seq: 2,
            ts: 1,
            payload: EventPayload::ReviewDetail {
                node: "gate".into(),
                request: 1,
                scope: None,
                show: Vec::new(),
                fields: Vec::new(),
                withheld: Vec::new(),
                withheld_reason: None,
            },
        });
        let gate = playbook(
            "{ id: gate, type: human_review, options: [approve, reject], fields: [{ name: note, type: text }] }",
        );
        assert!(
            settle(
                dir.path(),
                dir.path(),
                &gate,
                "gate",
                "approve",
                &Default::default(),
                &events,
            )
            .is_ok()
        );
    }
}
