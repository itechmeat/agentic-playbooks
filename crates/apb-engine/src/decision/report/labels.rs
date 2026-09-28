//! Labellers: what a run's later events say about a journaled decision
//! (issue #165 Part 13). One labeller per use; each reads only facts the
//! journal already records, never a model.
//!
//! A decision is framed as "act or not": the completion check acts by
//! flagging a reply, retry advice by stopping or switching, triage by
//! retrying on its own, the review triage by deciding, routing by moving an
//! attempt to another tier. A label says whether acting would have been
//! right ([`Label::Act`]) or wrong ([`Label::Keep`]); a decision whose run
//! has not shown it yet stays [`Label::Unlabelled`] and is left out of every
//! accuracy figure (it still counts in the coverage line).
//!
//! Only the completion check is labelled today. The other uses are stubs
//! ([`PendingLabeller`]) until their events journal the join keys named in
//! [`JoinKey`]; each stub says which later event will label it.

use std::collections::BTreeMap;

use crate::event::{DecisionAnswer, DecisionBaseline, Event, EventPayload, supervisor_action};

/// One journaled `decision_made`, with the run it belongs to.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionRecord {
    pub run_id: String,
    pub playbook: String,
    pub seq: u64,
    pub ts: u128,
    pub use_site: String,
    pub node: Option<String>,
    pub attempt: Option<u32>,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The provider's kind from the run manifest (`systemone`, `fake`, an
    /// emulation), when the manifest names it.
    pub provider_kind: Option<String>,
    pub calibrated: bool,
    pub mode: String,
    pub answers: BTreeMap<String, DecisionAnswer>,
    pub applied: bool,
    pub would_change: Option<bool>,
    pub baseline: Option<DecisionBaseline>,
    pub latency_ms: u64,
    pub cost_usd: Option<f64>,
    pub cached: bool,
    pub error: Option<String>,
    pub output_chars: Option<u64>,
}

impl DecisionRecord {
    /// The record of a `decision_made` event, `None` for any other event.
    pub fn from_event(run_id: &str, playbook: &str, e: &Event) -> Option<Self> {
        let EventPayload::DecisionMade {
            use_site,
            node,
            attempt,
            provider,
            model,
            calibrated,
            mode,
            output_chars,
            answers,
            applied,
            would_change,
            baseline,
            latency_ms,
            cost_usd,
            cached,
            error,
            ..
        } = &e.payload
        else {
            return None;
        };
        Some(DecisionRecord {
            run_id: run_id.to_string(),
            playbook: playbook.to_string(),
            seq: e.seq,
            ts: e.ts,
            use_site: use_site.clone(),
            node: node.clone(),
            attempt: *attempt,
            provider: provider.clone(),
            model: model.clone(),
            provider_kind: None,
            calibrated: *calibrated,
            mode: mode.clone(),
            answers: answers.clone(),
            applied: *applied,
            would_change: *would_change,
            baseline: baseline.clone(),
            latency_ms: *latency_ms,
            cost_usd: *cost_usd,
            cached: *cached,
            error: error.clone(),
            output_chars: *output_chars,
        })
    }

    /// Answered: a provider replied (a cache hit included).
    pub fn answered(&self) -> bool {
        self.error.is_none() && self.provider.is_some()
    }

    /// The `p` of one answer.
    pub fn p(&self, question: &str) -> Option<f64> {
        self.answers.get(question).and_then(|a| a.p)
    }
}

/// What ties a decision to the later events that label it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinKey {
    /// One attempt of one node: the completion check, retry advice, routing.
    Attempt { node: String, attempt: u32 },
    /// The n-th visit (1-based) of a `human_review` gate: the review triage.
    /// Needs a gate-visit field on the event (not journaled yet).
    GateVisit { node: String, visit: u32 },
    /// The `wake_raised` a supervisor triage answered, by its seq. Needs a
    /// wake-seq field on the event (not journaled yet).
    Wake { seq: u64 },
}

impl JoinKey {
    /// The key the record carries today: node and attempt, when present.
    pub fn of(r: &DecisionRecord) -> Option<Self> {
        Some(JoinKey::Attempt {
            node: r.node.clone()?,
            attempt: r.attempt?,
        })
    }
}

/// What a run's later events say about one decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    /// Acting would have been right (the completion check: the reply was
    /// not a finished result).
    Act,
    /// Acting would have been wrong: today's behaviour was right.
    Keep,
    /// Not known (yet), with the reason.
    Unlabelled(&'static str),
}

/// The labeller of one use.
pub trait Labeller: Sync {
    fn use_site(&self) -> &'static str;
    /// Where the labels come from, printed with the report.
    fn label_source(&self) -> &'static str;
    /// Today's behaviour without the use, as a label: the completion check
    /// never flags (`Keep`, "always complete").
    fn today(&self) -> (Label, &'static str);
    /// The config default of the use's threshold (the scale of
    /// `apb decisions thresholds set --threshold`).
    fn default_threshold(&self) -> f64;
    /// The probability that acting is right, for Brier and ECE.
    fn act_probability(&self, r: &DecisionRecord) -> Option<f64>;
    /// Whether the use acts on this answer at `threshold`.
    fn acts_at(&self, r: &DecisionRecord, threshold: f64) -> Option<bool>;
    /// For a `choice` use, the option that labels the decision (the
    /// eligibility rule wants 20 per option). `None` for a yes/no use.
    fn label_option(&self, _r: &DecisionRecord, _label: &Label) -> Option<String> {
        None
    }
    /// The label from the events of the decision's run.
    fn label(&self, r: &DecisionRecord, events: &[Event]) -> Label;
    /// A stub that labels nothing yet.
    fn pending(&self) -> bool {
        false
    }
}

/// The labeller of a use name. Every known use has one.
pub fn labeller_for(use_site: &str) -> Box<dyn Labeller> {
    match use_site {
        "completion_check" => Box::new(CompletionLabeller),
        other => Box::new(PendingLabeller::for_use(other)),
    }
}

// --- the completion check ---------------------------------------------------

/// Labels a completion check from what happened after the attempt:
///
/// - `Act` (the reply was not finished) when, before the node starts again,
///   a supervisor retried it (`node_retry`), the run was moved back to it or
///   to a node that ran before it (`run_continue_from`, a patch or migration
///   `continue_from`), or the next node to start after it finished `failed`;
/// - `Keep` when the next node finished `succeeded` with none of those, or
///   the node was the last one and the run finished `succeeded`;
/// - unlabelled while the run has not shown either, when another attempt of
///   the same visit followed (its outcome is not this attempt's), and above
///   shadow when the check's own anomaly wake came first (the outcome may be
///   the decision's doing).
pub struct CompletionLabeller;

/// The node a supervisor or patch moved the run back to, if this event does.
fn moved_back_to(e: &Event) -> Option<&str> {
    match &e.payload {
        EventPayload::SupervisorAction {
            action,
            node: Some(n),
            ..
        } if action == supervisor_action::NODE_RETRY
            || action == supervisor_action::RUN_CONTINUE_FROM =>
        {
            Some(n)
        }
        EventPayload::PatchApplied { continue_from, .. }
        | EventPayload::RunMigrated { continue_from, .. } => Some(continue_from),
        _ => None,
    }
}

impl Labeller for CompletionLabeller {
    fn use_site(&self) -> &'static str {
        "completion_check"
    }

    fn label_source(&self) -> &'static str {
        "downstream outcome: the next node failing, a supervisor retry, or a continue_from back"
    }

    fn today(&self) -> (Label, &'static str) {
        (Label::Keep, "always complete")
    }

    fn default_threshold(&self) -> f64 {
        apb_core::decisions::COMPLETION_FINAL_RESULT_CUT
    }

    fn act_probability(&self, r: &DecisionRecord) -> Option<f64> {
        r.p("final_result").map(|p| 1.0 - p)
    }

    fn acts_at(&self, r: &DecisionRecord, threshold: f64) -> Option<bool> {
        r.p("final_result").map(|p| p < threshold)
    }

    fn label(&self, r: &DecisionRecord, events: &[Event]) -> Label {
        let Some(node) = r.node.as_deref() else {
            return Label::Unlabelled("no node on the decision");
        };
        let Some(at) = events.iter().position(|e| e.seq == r.seq) else {
            return Label::Unlabelled("decision not in its journal");
        };
        // Nodes that ran before the decision: moving back to one of them
        // redoes the checked node's work.
        let before: Vec<&str> = events[..at]
            .iter()
            .filter_map(|e| match &e.payload {
                EventPayload::NodeStarted { node, .. } => Some(node.as_str()),
                _ => None,
            })
            .collect();
        // The events up to the node's next start: a retry of it or a move
        // back ends up there, and a later start is a new decision's business.
        let window: Vec<&Event> = events[at + 1..]
            .iter()
            .take_while(
                |e| !matches!(&e.payload, EventPayload::NodeStarted { node: n, .. } if n == node),
            )
            .collect();
        let restarted = window.len() < events.len() - at - 1;
        // Another attempt of the same visit (the checked one was rejected
        // or failed after it): what follows is that attempt's outcome, not
        // this one's, and an enforce rejection would label itself.
        if window.iter().any(|e| {
            matches!(&e.payload, EventPayload::AttemptStarted { node: n, attempt, .. }
                if n == node && r.attempt.is_some_and(|a| *attempt > a))
        }) {
            return Label::Unlabelled("a later attempt of the node followed");
        }
        // Above shadow the check raised its own anomaly wake: a supervisor
        // retry that answers it would confirm the decision by its own doing.
        if r.mode != "shadow"
            && window.iter().any(|e| {
                matches!(&e.payload, EventPayload::WakeRaised { trigger: crate::event::WakeTrigger::Anomaly, node: n, .. } if n == node)
            })
        {
            return Label::Unlabelled("the decision's own anomaly wake preceded the outcome");
        }
        if window.iter().any(|e| {
            moved_back_to(e).is_some_and(|target| target == node || before.contains(&target))
        }) {
            return Label::Act;
        }
        let mut own_finished = false;
        let mut next: Option<&str> = None;
        for e in &window {
            match &e.payload {
                EventPayload::NodeFinished { node: n, .. } if n == node => own_finished = true,
                EventPayload::NodeStarted { node: n, .. } if own_finished && next.is_none() => {
                    next = Some(n.as_str());
                }
                EventPayload::NodeFinished {
                    node: n, status, ..
                } if Some(n.as_str()) == next => {
                    return match status.as_str() {
                        "failed" => Label::Act,
                        "succeeded" => Label::Keep,
                        _ => Label::Unlabelled("the next node neither failed nor succeeded"),
                    };
                }
                EventPayload::RunFinished { outcome } if next.is_none() && own_finished => {
                    return if outcome == "succeeded" {
                        Label::Keep
                    } else {
                        Label::Unlabelled("the run ended without a next node")
                    };
                }
                _ => {}
            }
        }
        if restarted {
            return Label::Unlabelled("the node ran again before any outcome");
        }
        Label::Unlabelled("no downstream outcome yet")
    }
}

// --- the uses whose events do not carry join keys yet -----------------------

/// A use without a labeller yet: every decision stays unlabelled, with the
/// event that will label it once the use journals its join key.
pub struct PendingLabeller {
    use_site: &'static str,
    source: &'static str,
}

impl PendingLabeller {
    pub fn for_use(use_site: &str) -> Self {
        let (use_site, source) = match use_site {
            "retry_advice" => (
                "retry_advice",
                "pending: the next attempt's outcome and fallback of the same node (Attempt key)",
            ),
            "supervisor_triage" => (
                "supervisor_triage",
                "pending: the supervisor directive that answered the wake (Wake key)",
            ),
            "review_triage" => (
                "review_triage",
                "pending: the review_decided of the same gate visit (GateVisit key)",
            ),
            "routing" => (
                "routing",
                "pending: the attempt outcome on the chosen tier and any fallback (Attempt key)",
            ),
            "judge_node" => (
                "judge_node",
                "pending: later human corrections (weakest labels)",
            ),
            "judge_edge" => (
                "judge_edge",
                "pending: later human corrections (weakest labels)",
            ),
            "catalog_rank" => ("catalog_rank", "pending: the playbook the agent ran"),
            _ => ("unknown", "no labeller for this use"),
        };
        PendingLabeller { use_site, source }
    }
}

impl Labeller for PendingLabeller {
    fn use_site(&self) -> &'static str {
        self.use_site
    }
    fn label_source(&self) -> &'static str {
        self.source
    }
    fn today(&self) -> (Label, &'static str) {
        (Label::Keep, "today's behaviour")
    }
    fn default_threshold(&self) -> f64 {
        0.5
    }
    fn act_probability(&self, _r: &DecisionRecord) -> Option<f64> {
        None
    }
    fn acts_at(&self, r: &DecisionRecord, _threshold: f64) -> Option<bool> {
        r.would_change
    }
    fn label(&self, _r: &DecisionRecord, _events: &[Event]) -> Label {
        Label::Unlabelled("no labeller for this use yet")
    }
    fn pending(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            seq,
            ts: seq as u128,
            payload,
        }
    }

    fn started(seq: u64, node: &str) -> Event {
        ev(
            seq,
            EventPayload::NodeStarted {
                node: node.into(),
                attempt: 1,
            },
        )
    }

    fn finished(seq: u64, node: &str, status: &str) -> Event {
        ev(
            seq,
            EventPayload::NodeFinished {
                node: node.into(),
                status: status.into(),
                attempt: 1,
                output: String::new(),
                artifacts: Vec::new(),
            },
        )
    }

    fn decision(seq: u64, node: &str) -> DecisionRecord {
        DecisionRecord {
            run_id: "r".into(),
            playbook: "p".into(),
            seq,
            ts: 0,
            use_site: "completion_check".into(),
            node: Some(node.into()),
            attempt: Some(1),
            provider: Some("main".into()),
            model: Some("m".into()),
            provider_kind: None,
            calibrated: true,
            mode: "shadow".into(),
            answers: BTreeMap::new(),
            applied: false,
            would_change: None,
            baseline: None,
            latency_ms: 1,
            cost_usd: None,
            cached: false,
            error: None,
            output_chars: None,
        }
    }

    #[test]
    fn a_later_attempt_or_the_decisions_own_wake_leaves_it_unlabelled() {
        let attempt = |seq: u64, n: u32| {
            ev(
                seq,
                serde_json::from_value(serde_json::json!({
                    "type": "attempt_started", "node": "a", "attempt": n, "agent": "x"
                }))
                .unwrap(),
            )
        };
        let events = vec![
            started(1, "a"),
            decision_event(2),
            attempt(3, 2),
            finished(4, "a", "succeeded"),
            started(5, "b"),
            finished(6, "b", "succeeded"),
        ];
        assert!(matches!(
            CompletionLabeller.label(&decision(2, "a"), &events),
            Label::Unlabelled(_)
        ));
        let wake = ev(
            3,
            EventPayload::WakeRaised {
                trigger: crate::event::WakeTrigger::Anomaly,
                node: "a".into(),
                detail: String::new(),
                triage: None,
            },
        );
        let retry = ev(
            4,
            EventPayload::SupervisorAction {
                action: supervisor_action::NODE_RETRY.into(),
                node: Some("a".into()),
                detail: String::new(),
            },
        );
        let events = vec![started(1, "a"), decision_event(2), wake, retry];
        let mut advised = decision(2, "a");
        assert_eq!(CompletionLabeller.label(&advised, &events), Label::Act);
        advised.mode = "advise".into();
        assert!(matches!(
            CompletionLabeller.label(&advised, &events),
            Label::Unlabelled(_)
        ));
    }

    fn decision_event(seq: u64) -> Event {
        ev(
            seq,
            EventPayload::RunResumed {
                from_node: "placeholder".into(),
            },
        )
    }

    fn label(events: &[Event]) -> Label {
        CompletionLabeller.label(&decision(2, "a"), events)
    }

    #[test]
    fn the_next_node_decides_the_completion_label() {
        let base = || vec![started(0, "start"), started(1, "a"), decision_event(2)];
        let mut ok = base();
        ok.extend([finished(3, "a", "succeeded"), started(4, "b")]);
        let mut pending = ok.clone();
        ok.push(finished(5, "b", "succeeded"));
        assert_eq!(label(&ok), Label::Keep);
        let mut failed = pending.clone();
        failed.push(finished(5, "b", "failed"));
        assert_eq!(label(&failed), Label::Act);
        pending.push(ev(5, EventPayload::RunPaused { reason: "x".into() }));
        assert!(matches!(label(&pending), Label::Unlabelled(_)));
        // The last node: the run's outcome labels it.
        let mut last = base();
        last.extend([
            finished(3, "a", "succeeded"),
            ev(
                4,
                EventPayload::RunFinished {
                    outcome: "succeeded".into(),
                },
            ),
        ]);
        assert_eq!(label(&last), Label::Keep);
    }

    #[test]
    fn a_retry_or_a_move_back_labels_act() {
        let mut retry = vec![
            started(0, "start"),
            started(1, "a"),
            decision_event(2),
            finished(3, "a", "succeeded"),
            started(4, "b"),
            finished(5, "b", "succeeded"),
        ];
        let mut back = retry.clone();
        retry.push(ev(
            6,
            EventPayload::SupervisorAction {
                action: supervisor_action::NODE_RETRY.into(),
                node: Some("a".into()),
                detail: String::new(),
            },
        ));
        // A retry of the node wins even after the next node succeeded (a
        // gate that let a false success through).
        assert_eq!(label(&retry), Label::Act);
        back.truncate(5);
        back.push(ev(
            5,
            EventPayload::SupervisorAction {
                action: supervisor_action::RUN_CONTINUE_FROM.into(),
                node: Some("start".into()),
                detail: String::new(),
            },
        ));
        assert_eq!(label(&back), Label::Act);
        // A move forward to a node that never ran before is not a move back.
        let mut forward = back.clone();
        forward.pop();
        forward.push(ev(
            5,
            EventPayload::SupervisorAction {
                action: supervisor_action::RUN_CONTINUE_FROM.into(),
                node: Some("c".into()),
                detail: String::new(),
            },
        ));
        assert!(matches!(label(&forward), Label::Unlabelled(_)));
    }
}
