//! The publish nodes of a run for the outcome views: what each one pushed,
//! skipped or refused, and a push whose result nobody knows (which also
//! counts as an unknown send: see [`super::intents`]).

pub use crate::publish::view::{PlanView, PublishView as RunPublish, PushState, PushView};

use apb_core::publish::{output, short_sha as short};

use crate::event::Event;

/// Every publish node that pinned a target or planned, in journal order,
/// each push's state read at `now_ms`.
pub fn publish_from(events: &[Event], now_ms: u64) -> Vec<RunPublish> {
    crate::publish::view::fold(events, now_ms)
}

/// The done lines of a publish node: what a confirmed push sent (the facts
/// of the plan it carried, never a later plan's), and a person's skip of
/// the node's last visit.
pub fn done_lines(p: &RunPublish) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(push) = p.push.as_ref().filter(|x| x.state == PushState::Confirmed) {
        out.push(format!("{}: published {}", p.node, pushed(push)));
    }
    if p.result.as_deref() == Some(output::SKIPPED) {
        out.push(format!("{}: publishing was skipped by a person", p.node));
    }
    out
}

/// What a push sent, `<after> to <url> branch <branch>`, or its intent id
/// when the plan it carried is no longer in the journal.
fn pushed(push: &PushView) -> String {
    if push.after.is_empty() {
        format!("(push {}; its plan is not in the journal)", push.intent)
    } else {
        format!(
            "{} to {} branch {}",
            short(&push.after),
            push.url,
            push.branch
        )
    }
}

/// "N commit(s), M made by this run": the push's whole count.
pub fn commit_count(plan: &PlanView) -> String {
    format!(
        "{} commit(s), {} made by this run",
        plan.commits.len() + plan.omitted,
        plan.this_run
    )
}

/// One plain line per fact of a publish node, for the report and the CLI.
pub fn lines(p: &RunPublish) -> Vec<String> {
    let mut out = Vec::new();
    match (&p.url, &p.pin_refusal) {
        (_, Some(code)) => out.push(format!("{}: target not pinned ({code})", p.node)),
        (Some(url), None) => out.push(format!("{}: target {} ({url})", p.node, p.remote)),
        (None, None) => {}
    }
    if let Some(plan) = &p.plan {
        let over = plan
            .remote_before
            .as_deref()
            .map_or("a new branch".to_string(), |b| format!("over {}", short(b)));
        out.push(format!(
            "{}: plan {} to branch {} as {over}, {}{}",
            p.node,
            short(&plan.after),
            plan.branch,
            commit_count(plan),
            if plan.outside_push_suspected {
                "; commits of this run were already on the remote"
            } else {
                ""
            }
        ));
    }
    if p.awaiting_consent {
        out.push(format!("{}: waiting for a person's consent", p.node));
    }
    if let Some(push) = &p.push {
        let detail = push
            .detail
            .as_deref()
            .map(|d| format!(": {d}"))
            .unwrap_or_default();
        out.push(format!(
            "{}: push {} {}{detail}",
            p.node,
            push.intent,
            push.state.as_str()
        ));
        if !push.after.is_empty() {
            out.push(format!("{}: that push carried {}", p.node, pushed(push)));
        }
    }
    if let Some(r) = &p.result {
        out.push(format!("{}: {r}", p.node));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn confirmed(after: &str) -> RunPublish {
        RunPublish {
            node: "ship".into(),
            remote: "origin".into(),
            url: None,
            pin_refusal: None,
            plan: None,
            awaiting_consent: false,
            push: Some(PushView {
                intent: "si-1".into(),
                state: PushState::Confirmed,
                after: after.into(),
                branch: if after.is_empty() {
                    String::new()
                } else {
                    "main".into()
                },
                url: if after.is_empty() {
                    String::new()
                } else {
                    "file:///r.git".into()
                },
                remote_before: None,
                plan_digest: String::new(),
                detail: None,
                effect_ref: None,
            }),
            result: Some("published".into()),
        }
    }

    #[test]
    fn a_push_without_its_plan_names_its_intent_instead_of_empty_facts() {
        assert_eq!(
            done_lines(&confirmed(&"a".repeat(40))),
            ["ship: published aaaaaaaaaaaa to file:///r.git branch main"]
        );
        assert_eq!(
            done_lines(&confirmed("")),
            ["ship: published (push si-1; its plan is not in the journal)"]
        );
        assert!(lines(&confirmed("")).iter().all(|l| !l.contains("  ")));
    }
}
